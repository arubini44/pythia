//! Net service names from `tnsnames.ora`.

use std::path::{Path, PathBuf};

/// Directories searched for `tnsnames.ora`, in Oracle's order:
/// `TNS_ADMIN`, `ORACLE_HOME/network/admin`, then the Instant Client
/// directory (found on the library path).
pub fn admin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("TNS_ADMIN") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("ORACLE_HOME") {
        dirs.push(PathBuf::from(home).join("network").join("admin"));
    }
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "PATH"] {
        if let Some(paths) = std::env::var_os(var) {
            for dir in std::env::split_paths(&paths) {
                let is_client = ["libclntsh.so", "libclntsh.dylib", "oci.dll"]
                    .iter()
                    .any(|f| dir.join(f).exists());
                if is_client {
                    dirs.push(dir.join("network").join("admin"));
                }
            }
        }
    }
    dirs
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
