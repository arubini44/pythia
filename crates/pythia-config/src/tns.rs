//! Net service names from `tnsnames.ora`.

use std::path::{Path, PathBuf};

/// Directories searched for `tnsnames.ora`, in Oracle's order:
/// `TNS_ADMIN`, `ORACLE_HOME/network/admin`, on Windows the same from the
/// registry, then the `network/admin` of each Oracle client on the library
/// path.
pub fn admin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = env("TNS_ADMIN") {
        dirs.push(dir);
    }
    if let Some(home) = env("ORACLE_HOME") {
        dirs.push(admin_dir(&home));
    }
    let clients = client_dirs();
    #[cfg(windows)]
    dirs.extend(registry::admin_dirs(&clients));
    for dir in &clients {
        dirs.extend(client_admin_dirs(dir));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// An environment variable that is set and not empty.
fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn admin_dir(oracle_home: &Path) -> PathBuf {
    oracle_home.join("network").join("admin")
}

/// Directories on the library path with an Oracle client library in them.
fn client_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "PATH"] {
        if let Some(paths) = std::env::var_os(var) {
            for dir in std::env::split_paths(&paths) {
                let is_client = ["libclntsh.so", "libclntsh.dylib", "oci.dll"]
                    .iter()
                    .any(|f| dir.join(f).exists());
                if is_client {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

/// Where a client whose library is in `dir` keeps its configuration: the
/// Instant Client in `dir/network/admin`, a full client (library in
/// `ORACLE_HOME/bin` on Windows, `ORACLE_HOME/lib` elsewhere) in
/// `ORACLE_HOME/network/admin`.
fn client_admin_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![admin_dir(dir)];
    let in_home = dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin") || name.eq_ignore_ascii_case("lib"));
    if in_home && let Some(home) = dir.parent() {
        dirs.push(admin_dir(home));
    }
    dirs
}

/// Oracle's installer on Windows sets no environment variables: the homes
/// are in the registry, `TNS_ADMIN` too where an admin set it.
#[cfg(windows)]
mod registry {
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, RRF_RT_REG_EXPAND_SZ,
        RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };

    const ERROR_MORE_DATA: u32 = 234;

    /// From `HKLM\SOFTWARE\ORACLE` (64- and 32-bit clients): its
    /// `TNS_ADMIN`, then each home's (`KEY_<name>`) `TNS_ADMIN` and
    /// `ORACLE_HOME\network\admin`. Homes whose `bin` is on the library path
    /// come first, in its order, as the one Oracle would load.
    pub(super) fn admin_dirs(clients: &[PathBuf]) -> Vec<PathBuf> {
        oracle_dirs(HKEY_LOCAL_MACHINE, r"SOFTWARE\ORACLE", clients)
    }

    fn oracle_dirs(root: HKEY, path: &str, clients: &[PathBuf]) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        let mut homes = Vec::new();
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let Some(oracle) = Key::open(root, path, view) else {
                continue;
            };
            dirs.extend(oracle.string(None, "TNS_ADMIN").map(PathBuf::from));
            for name in oracle.subkeys() {
                if name
                    .get(..4)
                    .is_some_and(|p| p.eq_ignore_ascii_case("KEY_"))
                {
                    let home = oracle.string(Some(&name), "ORACLE_HOME").map(PathBuf::from);
                    let tns_admin = oracle.string(Some(&name), "TNS_ADMIN").map(PathBuf::from);
                    homes.push((home, tns_admin));
                }
            }
        }
        homes.sort_by_key(|(home, _)| {
            home.as_ref()
                .and_then(|h| clients.iter().position(|c| same_dir(c, &h.join("bin"))))
                .unwrap_or(usize::MAX)
        });
        for (home, tns_admin) in homes {
            dirs.extend(tns_admin);
            dirs.extend(home.map(|h| super::admin_dir(&h)));
        }
        dirs
    }

    fn same_dir(a: &Path, b: &Path) -> bool {
        let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
        norm(a) == norm(b)
    }

    struct Key(HKEY);

    impl Key {
        fn open(root: HKEY, path: &str, view: u32) -> Option<Key> {
            let mut key = null_mut();
            let path = wide(path);
            let status =
                unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ | view, &mut key) };
            (status == 0).then_some(Key(key))
        }

        fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            // Key names have at most 255 characters.
            let mut buf = [0u16; 256];
            for index in 0.. {
                let mut len = buf.len() as u32;
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        buf.as_mut_ptr(),
                        &mut len,
                        null(),
                        null_mut(),
                        null_mut(),
                        null_mut(),
                    )
                };
                if status != 0 {
                    break;
                }
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
            out
        }

        /// A string value of this key or of its `subkey`, with environment
        /// variables expanded; None when missing or blank.
        fn string(&self, subkey: Option<&str>, name: &str) -> Option<String> {
            let subkey = subkey.map(wide);
            let name = wide(name);
            let mut buf = vec![0u16; 512];
            loop {
                let mut size = (buf.len() * 2) as u32;
                let status = unsafe {
                    RegGetValueW(
                        self.0,
                        subkey.as_ref().map_or(null(), |s| s.as_ptr()),
                        name.as_ptr(),
                        RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                        null_mut(),
                        buf.as_mut_ptr().cast(),
                        &mut size,
                    )
                };
                match status {
                    0 => break,
                    ERROR_MORE_DATA => buf.resize(size as usize / 2 + 1, 0),
                    _ => return None,
                }
            }
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let value = String::from_utf16_lossy(&buf[..len]);
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

        #[test]
        fn reads_the_registry() {
            let nt = Key::open(
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows NT",
                KEY_WOW64_64KEY,
            )
            .unwrap();
            assert!(nt.subkeys().iter().any(|k| k == "CurrentVersion"));
            let product = nt.string(Some("CurrentVersion"), "ProductName").unwrap();
            assert!(product.contains("Windows"), "{product}");
            // REG_EXPAND_SZ, "%SystemDrive%\Users".
            let profiles = nt
                .string(Some(r"CurrentVersion\ProfileList"), "ProfilesDirectory")
                .unwrap();
            assert!(!profiles.contains('%'), "{profiles}");
            assert_eq!(nt.string(Some("CurrentVersion"), "NoSuchValue"), None);
            // Whatever is installed: no panic, and only absolute paths.
            assert!(admin_dirs(&[]).iter().all(|d| d.is_absolute()));
        }

        /// Oracle's layout in a scratch key of HKCU (HKLM needs admin).
        #[test]
        fn oracle_homes() {
            let base = format!(r"Software\pythia-test-{}", std::process::id());
            let reg = |args: &[&str]| {
                let ok = std::process::Command::new("reg")
                    .args(args)
                    .output()
                    .unwrap();
                assert!(ok.status.success(), "reg {args:?}");
            };
            let set = |key: &str, name: &str, value: &str| {
                let key = format!(r"HKCU\{base}\ORACLE{key}");
                reg(&["add", &key, "/v", name, "/d", value, "/f"]);
            };
            set("", "TNS_ADMIN", r"C:\tns");
            set(
                r"\KEY_OraClient12Home1",
                "ORACLE_HOME",
                r"C:\oracle\client12",
            );
            set(
                r"\KEY_OraClient19Home1",
                "ORACLE_HOME",
                r"C:\oracle\client19",
            );
            set(r"\KEY_OraClient19Home1", "TNS_ADMIN", r"\\fs\oracle\tns");
            set(r"\VirtualBox", "InstallDir", r"C:\VirtualBox");
            let path = format!(r"{base}\ORACLE");
            let clients = [PathBuf::from(r"C:\Oracle\Client19\BIN\")];
            let mut dirs = oracle_dirs(HKEY_CURRENT_USER, &path, &clients);
            reg(&["delete", &format!(r"HKCU\{base}"), "/f"]);
            // HKCU\Software has one view for 64- and 32-bit programs.
            let mut seen = std::collections::HashSet::new();
            dirs.retain(|d| seen.insert(d.clone()));
            let dirs: Vec<_> = dirs
                .iter()
                .map(|d| d.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                dirs,
                [
                    r"C:\tns",
                    r"\\fs\oracle\tns",
                    r"C:\oracle\client19\network\admin",
                    r"C:\oracle\client12\network\admin",
                ]
            );
        }
    }
}

/// Aliases defined in the first `tnsnames.ora` found, sorted.
pub fn aliases() -> Vec<String> {
    for dir in admin_dirs() {
        let file = dir.join("tnsnames.ora");
        if file.exists() {
            let mut out = Vec::new();
            read_file(&file, &mut out, 0);
            out.sort_by_key(|a| a.to_lowercase());
            out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
            return out;
        }
    }
    Vec::new()
}

fn read_file(path: &Path, out: &mut Vec<String>, depth: usize) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for entry in parse(&text) {
        match entry {
            Entry::Alias(a) => out.push(a),
            // IFILE=path includes another file (bounded to avoid cycles).
            Entry::Include(file) if depth < 4 => read_file(Path::new(&file), out, depth + 1),
            Entry::Include(_) => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Alias(String),
    Include(String),
}

/// Top-level `name[, name] = (...)` entries and `IFILE = path` lines.
fn parse(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut pending = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        for (i, c) in line.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                '=' if depth == 0 => {
                    let names = std::mem::take(&mut pending);
                    let names = names.trim();
                    if names.eq_ignore_ascii_case("IFILE") {
                        // The rest of the line is the included file.
                        out.push(Entry::Include(
                            line[i + 1..].trim().trim_matches('"').to_owned(),
                        ));
                        break;
                    }
                    out.extend(
                        names
                            .split(',')
                            .map(str::trim)
                            .filter(|n| !n.is_empty())
                            .map(|n| Entry::Alias(n.to_owned())),
                    );
                }
                _ if depth == 0 => pending.push(c),
                _ => {}
            }
        }
        if depth == 0 {
            pending.push(' ');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_client_homes() {
        let ic = Path::new("/opt/oracle/instantclient_23_26");
        assert_eq!(client_admin_dirs(ic), [ic.join("network/admin")]);
        // Windows: ORACLE_HOME\bin; Linux: $ORACLE_HOME/lib, also the
        // Instant Client RPM's /usr/lib/oracle/<ver>/client64/lib.
        for home in ["/u01/app/client_1", "/usr/lib/oracle/21/client64"] {
            for lib in ["bin", "lib", "BIN"] {
                let dir = Path::new(home).join(lib);
                assert_eq!(
                    client_admin_dirs(&dir),
                    [
                        dir.join("network/admin"),
                        Path::new(home).join("network/admin")
                    ]
                );
            }
        }
    }

    #[test]
    fn parses_aliases() {
        let text = "# comment\nPROD =\n  (DESCRIPTION =\n    (ADDRESS = (PROTOCOL = TCP)(HOST = db1)(PORT = 1521))\n    (CONNECT_DATA = (SERVICE_NAME = prod)))\n\ntest, test.world = (DESCRIPTION=(ADDRESS=(HOST=x)))\nIFILE = /etc/more.ora\n";
        assert_eq!(
            parse(text),
            [
                Entry::Alias("PROD".into()),
                Entry::Alias("test".into()),
                Entry::Alias("test.world".into()),
                Entry::Include("/etc/more.ora".into()),
            ]
        );
    }
}
