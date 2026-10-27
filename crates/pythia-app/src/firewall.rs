//! This computer's firewall and the debugger: whether it lets the database
//! connect to Pythia, and the command that allows it, run with
//! administrator rights after asking.
//!
//! - Windows: Windows Defender Firewall, read and changed with PowerShell
//!   (`Get-NetFirewall*`, `New-NetFirewallRule`). Answering *Cancel* to its
//!   prompt when the debugger first listened leaves rules that block
//!   Pythia.
//! - Linux: ufw or firewalld. Under WSL, Windows networking decides what
//!   reaches Linux at all.
//! - macOS: the application firewall (`socketfilterfw`).

use std::path::Path;

use pythia_debugger::setup::{Ports, ports_text};

use crate::desktop;

/// The name of the rule Pythia adds.
pub const RULE_NAME: &str = "Pythia PL/SQL debugger";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firewall {
    Windows,
    Ufw,
    Firewalld,
    MacOs,
}

impl Firewall {
    pub fn name(self) -> &'static str {
        match self {
            Firewall::Windows => "Windows Firewall",
            Firewall::Ufw => "ufw",
            Firewall::Firewalld => "firewalld",
            Firewall::MacOs => "the macOS firewall",
        }
    }

    /// Its rules are about ports, so the debugger needs fixed ones.
    pub fn needs_ports(self) -> bool {
        matches!(self, Firewall::Ufw | Firewall::Firewalld)
    }
}

/// What Pythia found out about this computer's firewall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// None: no firewall Pythia knows how to read.
    pub firewall: Option<Firewall>,
    /// Whether it filters incoming connections (None: unknown).
    pub enabled: Option<bool>,
    /// Its rules about Pythia's incoming connections: name and whether it
    /// allows them.
    pub rules: Vec<(String, bool)>,
    /// The organization manages the firewall: rules added here don't
    /// count.
    pub managed: bool,
    /// Other things to know, e.g. about WSL's networking.
    pub notes: Vec<String>,
    /// Pythia can run a rule command with administrator rights (else it is
    /// for copying).
    pub elevate: bool,
}

impl State {
    /// Whether it lets the database connect: `Some(false)` when it blocks
    /// Pythia, `None` when Pythia can't tell.
    pub fn allows(&self) -> Option<bool> {
        if self.enabled == Some(false) {
            return Some(true);
        }
        // On Windows a blocking rule wins over allowing ones.
        if self.rules.iter().any(|(_, allows)| !allows) {
            return Some(false);
        }
        if self.rules.iter().any(|(_, allows)| *allows) {
            return Some(true);
        }
        match (self.firewall, self.enabled) {
            // Both block what no rule allows.
            (Some(Firewall::Windows | Firewall::MacOs), Some(true)) => Some(false),
            _ => None,
        }
    }

    /// One line about it.
    pub fn summary(&self) -> String {
        let Some(firewall) = self.firewall else {
            return "No firewall found that Pythia can read (ufw, firewalld, Windows or macOS \
                    firewall)."
                .to_owned();
        };
        let name = firewall.name();
        let blocking: Vec<&str> = self
            .rules
            .iter()
            .filter(|(_, allows)| !allows)
            .map(|(n, _)| n.as_str())
            .collect();
        if self.enabled == Some(false) {
            format!("{name} is off.")
        } else if !blocking.is_empty() {
            format!(
                "{name} blocks Pythia ({}). Such rules come from answering Cancel when \
                 Windows asked whether Pythia may accept connections.",
                blocking.join(", ")
            )
        } else if let Some((rule, _)) = self.rules.first() {
            format!("{name} lets the database connect to Pythia (rule {rule}).")
        } else if self.allows() == Some(false) {
            format!("{name} is on and no rule lets the database connect to Pythia.")
        } else if self.enabled == Some(true) {
            format!("{name} is on; Pythia can't read its rules without administrator rights.")
        } else {
            format!("{name} is installed; Pythia can't tell whether it is on.")
        }
    }
}

/// Finds out about the firewall, for the program `exe`. Runs commands, so
/// call it on a thread of its own.
pub fn detect(exe: &Path) -> State {
    let mut state = if cfg!(windows) {
        detect_windows(exe)
    } else if cfg!(target_os = "macos") {
        detect_macos(exe)
    } else {
        detect_linux()
    };
    state.elevate = state.firewall.is_some_and(can_elevate);
    state
}

/// The command that lets the database connect to `exe` on `ports` (any
/// port without), only from `remote` if given; none if the firewall
/// needs ports and there are none.
pub fn rule_command(
    state: &State,
    exe: &Path,
    ports: Option<&Ports>,
    remote: Option<&str>,
) -> Option<String> {
    let firewall = state.firewall?;
    if firewall.needs_ports() && ports.is_none() {
        return None;
    }
    let exe = exe.to_string_lossy();
    Some(match firewall {
        Firewall::Windows => {
            let exe = ps_quote(&exe);
            let mut lines = Vec::new();
            if state.rules.iter().any(|(_, allows)| !allows) {
                lines.push(format!(
                    "Get-NetFirewallApplicationFilter -Program {exe} | Get-NetFirewallRule | \
                     Where-Object {{ $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }} | \
                     Remove-NetFirewallRule"
                ));
            }
            lines.push(format!(
                "Remove-NetFirewallRule -DisplayName '{RULE_NAME}' -ErrorAction SilentlyContinue"
            ));
            let mut add = format!(
                "New-NetFirewallRule -DisplayName '{RULE_NAME}' -Direction Inbound -Action Allow \
                 -Protocol TCP -Program {exe} -Profile Any"
            );
            if let Some(ports) = ports {
                add.push_str(&format!(" -LocalPort {}", ports_text(ports)));
            }
            if let Some(remote) = remote {
                add.push_str(&format!(" -RemoteAddress {}", ps_quote(remote)));
            }
            lines.push(add);
            lines.join("\n")
        }
        Firewall::Ufw => {
            let ports = ports?;
            let port = if ports.start() == ports.end() {
                ports.start().to_string()
            } else {
                format!("{}:{}", ports.start(), ports.end())
            };
            format!(
                "ufw allow proto tcp from {} to any port {port} comment '{RULE_NAME}'",
                remote.unwrap_or("any")
            )
        }
        Firewall::Firewalld => {
            let ports = ports_text(ports?);
            let rule = match remote {
                Some(remote) => format!(
                    "--add-rich-rule='rule family=\"ipv4\" source address=\"{remote}\" \
                     port port=\"{ports}\" protocol=\"tcp\" accept'"
                ),
                None => format!("--add-port={ports}/tcp"),
            };
            format!("firewall-cmd {rule} && firewall-cmd --permanent {rule}")
        }
        Firewall::MacOs => {
            let exe = sh_quote(&exe);
            format!("{SOCKETFILTERFW} --add {exe} && {SOCKETFILTERFW} --unblockapp {exe}")
        }
    })
}

/// Whether Pythia can ask for administrator rights to change `firewall`.
fn can_elevate(firewall: Firewall) -> bool {
    match firewall {
        Firewall::Windows | Firewall::MacOs => true,
        Firewall::Ufw | Firewall::Firewalld => {
            which("pkexec")
                && (std::env::var_os("DISPLAY").is_some()
                    || std::env::var_os("WAYLAND_DISPLAY").is_some())
        }
    }
}

/// Runs `command` with administrator rights: Windows asks with its UAC
/// prompt, Linux with polkit (`pkexec`), macOS for the password. Blocks
/// until it's done.
pub fn run_elevated(firewall: Firewall, command: &str) -> Result<(), String> {
    match firewall {
        Firewall::Windows => run_windows_elevated(command),
        Firewall::Ufw | Firewall::Firewalld => {
            let output = desktop::command("pkexec")
                .args(["sh", "-c", command])
                .output()
                .map_err(|e| format!("pkexec: {e}"))?;
            match output.status.code() {
                Some(0) => Ok(()),
                // Dismissed, or not authorized.
                Some(126 | 127) => Err("Not authorized (or the prompt was dismissed).".into()),
                _ => Err(error_text(&output)),
            }
        }
        Firewall::MacOs => {
            let script = format!(
                "do shell script \"{}\" with administrator privileges",
                command.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let output = desktop::command("osascript")
                .args(["-e", &script])
                .output()
                .map_err(|e| format!("osascript: {e}"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(error_text(&output))
            }
        }
    }
}

fn error_text(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("The command failed ({}).", output.status)
    } else {
        stderr
    }
}

// ----- Windows ---------------------------------------------------------------

/// Reads the profiles, the network categories in use and the inbound rules
/// for the program, as JSON.
const WINDOWS_DETECT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore | ForEach-Object {
  [pscustomobject]@{ Name = "$($_.Name)"; Enabled = "$($_.Enabled)"; Inbound = "$($_.DefaultInboundAction)"; LocalRules = "$($_.AllowLocalFirewallRules)" } })
$categories = @(Get-NetConnectionProfile | ForEach-Object { "$($_.NetworkCategory)" })
$rules = @(Get-NetFirewallApplicationFilter -Program $exe | Get-NetFirewallRule | Where-Object { "$($_.Direction)" -eq 'Inbound' -and "$($_.Enabled)" -eq 'True' } | ForEach-Object {
  [pscustomobject]@{ Name = "$($_.DisplayName)"; Action = "$($_.Action)"; Profile = "$($_.Profile)" } })
[pscustomobject]@{ Profiles = $profiles; Categories = $categories; Rules = $rules } | ConvertTo-Json -Compress -Depth 4"#;

fn detect_windows(exe: &Path) -> State {
    let script = format!(
        "$exe = {}\n{WINDOWS_DETECT}",
        ps_quote(&exe.to_string_lossy())
    );
    let output = desktop::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(encode_command(&script))
        .output();
    let mut state = match output {
        Ok(output) if output.status.success() => {
            parse_windows(&String::from_utf8_lossy(&output.stdout))
        }
        _ => None,
    }
    .unwrap_or_else(|| State {
        firewall: Some(Firewall::Windows),
        ..State::default()
    });
    if state.enabled.is_none() {
        state
            .notes
            .push("Pythia couldn't read Windows Firewall's settings.".into());
    }
    state
}

/// [`WINDOWS_DETECT`]'s output.
fn parse_windows(json: &str) -> Option<State> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let list = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    // Profiles of the networks in use (all of them if that's unknown).
    let categories: Vec<String> = list("Categories")
        .iter()
        .filter_map(|c| c.as_str())
        .map(|c| {
            if c == "DomainAuthenticated" {
                "Domain"
            } else {
                c
            }
            .to_owned()
        })
        .collect();
    let in_use = |profile: &str| {
        categories.is_empty()
            || profile == "Any"
            || categories
                .iter()
                .any(|c| profile.split(", ").any(|p| p == c))
    };
    let profiles: Vec<serde_json::Value> = list("Profiles")
        .into_iter()
        .filter(|p| in_use(&text(p, "Name")))
        .collect();
    if profiles.is_empty() {
        return None;
    }
    let filtering =
        |p: &serde_json::Value| text(p, "Enabled") == "True" && text(p, "Inbound") != "Allow";
    let rules = list("Rules")
        .iter()
        .filter(|r| in_use(&text(r, "Profile")))
        .map(|r| (text(r, "Name"), text(r, "Action") == "Allow"))
        .collect();
    Some(State {
        firewall: Some(Firewall::Windows),
        enabled: Some(profiles.iter().any(filtering)),
        rules,
        managed: profiles
            .iter()
            .any(|p| filtering(p) && text(p, "LocalRules") == "False"),
        ..State::default()
    })
}

fn run_windows_elevated(command: &str) -> Result<(), String> {
    // The elevated PowerShell has no console of ours: its error goes to a
    // file.
    let errors = std::env::temp_dir().join(format!("pythia-firewall-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&errors);
    let inner = format!(
        "$ErrorActionPreference = 'Stop'\ntry {{\n{command}\n}} catch {{\n  \
         $_ | Out-String | Set-Content -LiteralPath {} -Encoding UTF8\n  exit 1\n}}",
        ps_quote(&errors.to_string_lossy())
    );
    let outer = format!(
        "try {{ $p = Start-Process -FilePath 'powershell.exe' -Verb RunAs -Wait -PassThru \
         -WindowStyle Hidden -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{}'; \
         exit $p.ExitCode }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1223 }}",
        encode_command(&inner)
    );
    let output = desktop::command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(encode_command(&outer))
        .output()
        .map_err(|e| format!("PowerShell: {e}"))?;
    let failed = std::fs::read_to_string(&errors).ok();
    let _ = std::fs::remove_file(&errors);
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1223) => Err(format!(
            "Not run: Windows didn't get administrator rights ({}).",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        _ => Err(failed
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| error_text(&output))),
    }
}

/// A PowerShell string literal.
fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A script for `powershell -EncodedCommand`: base64 of its UTF-16LE text
/// (no script file, so no execution policy applies).
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ----- Linux -----------------------------------------------------------------

fn detect_linux() -> State {
    let mut state = State::default();
    if is_wsl() {
        state.notes.push(
            "Pythia runs in WSL. Databases in containers on this computer reach it directly; \
             one on another computer only with WSL's mirrored networking (networkingMode=mirrored \
             in .wslconfig, then Windows Firewall applies), or use Pythia for Windows."
                .into(),
        );
    }
    if let Ok(output) = desktop::command("firewall-cmd").arg("--state").output() {
        state.firewall = Some(Firewall::Firewalld);
        state.enabled = Some(output.status.success());
    } else if which("ufw") || Path::new("/usr/sbin/ufw").exists() {
        state.firewall = Some(Firewall::Ufw);
        state.enabled = std::fs::read_to_string("/etc/ufw/ufw.conf")
            .ok()
            .map(|conf| ufw_enabled(&conf));
    }
    state
}

fn ufw_enabled(conf: &str) -> bool {
    conf.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// A POSIX shell word.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

// ----- macOS -----------------------------------------------------------------

const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

fn detect_macos(exe: &Path) -> State {
    let run = |args: &[&str]| {
        desktop::command(SOCKETFILTERFW)
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
    };
    let mut state = State {
        firewall: Some(Firewall::MacOs),
        ..State::default()
    };
    // "Firewall is enabled. (State = 1)"
    state.enabled = run(&["--getglobalstate"]).map(|s| s.contains("enabled"));
    let exe_text = exe.to_string_lossy();
    if let Some(app) = run(&["--getappblocked", &exe_text]) {
        if app.contains("permitted") || app.contains("not blocked") {
            state.rules.push(("Pythia".into(), true));
        } else if app.contains("blocked") {
            state.rules.push(("Pythia".into(), false));
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_commands() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // UTF-16LE, as `powershell -EncodedCommand` reads it.
        assert_eq!(encode_command("dir"), "ZABpAHIA");
        assert_eq!(ps_quote("C:\\it's\\pythia.exe"), "'C:\\it''s\\pythia.exe'");
        assert_eq!(sh_quote("/Apps/it's"), "'/Apps/it'\\''s'");
    }

    fn windows(rules: Vec<(&str, bool)>) -> State {
        State {
            firewall: Some(Firewall::Windows),
            enabled: Some(true),
            rules: rules.into_iter().map(|(n, a)| (n.to_owned(), a)).collect(),
            ..State::default()
        }
    }

    #[test]
    fn windows_state() {
        let json = r#"{"Profiles":[{"Name":"Domain","Enabled":"True","Inbound":"NotConfigured","LocalRules":"NotConfigured"},{"Name":"Private","Enabled":"True","Inbound":"Block","LocalRules":"NotConfigured"},{"Name":"Public","Enabled":"True","Inbound":"Block","LocalRules":"False"}],"Categories":["Private"],"Rules":[{"Name":"pythia.exe","Action":"Block","Profile":"Public"},{"Name":"pythia.exe","Action":"Block","Profile":"Private"}]}"#;
        let state = parse_windows(json).unwrap();
        assert_eq!(state.enabled, Some(true));
        // Only the rule for the private network in use counts; the public
        // profile's group policy doesn't either.
        assert_eq!(state.rules, [("pythia.exe".to_owned(), false)]);
        assert!(!state.managed);
        assert_eq!(state.allows(), Some(false));
        assert!(
            state.summary().contains("blocks Pythia (pythia.exe)"),
            "{}",
            state.summary()
        );

        let json = r#"{"Profiles":[{"Name":"Domain","Enabled":"True","Inbound":"Block","LocalRules":"False"}],"Categories":["DomainAuthenticated"],"Rules":[]}"#;
        let state = parse_windows(json).unwrap();
        assert!(state.managed);
        assert_eq!(state.allows(), Some(false));
        let json = r#"{"Profiles":[{"Name":"Private","Enabled":"False","Inbound":"Block","LocalRules":"True"}],"Categories":["Private"],"Rules":[]}"#;
        assert_eq!(parse_windows(json).unwrap().allows(), Some(true));
        assert_eq!(parse_windows("not json"), None);

        let allowed = windows(vec![(RULE_NAME, true)]);
        assert_eq!(allowed.allows(), Some(true));
        assert!(allowed.summary().contains("lets the database connect"));
    }

    #[test]
    fn commands() {
        let exe = Path::new("C:\\Program Files\\Pythia\\pythia.exe");
        let blocked = windows(vec![("pythia.exe", false)]);
        let cmd = rule_command(&blocked, exe, Some(&(4000..=4009)), Some("10.1.2.3")).unwrap();
        let lines: Vec<&str> = cmd.lines().collect();
        assert_eq!(lines.len(), 3, "{cmd}");
        assert!(lines[0].contains("Remove-NetFirewallRule") && lines[0].contains("'Block'"));
        assert_eq!(
            lines[2],
            "New-NetFirewallRule -DisplayName 'Pythia PL/SQL debugger' -Direction Inbound \
             -Action Allow -Protocol TCP -Program 'C:\\Program Files\\Pythia\\pythia.exe' \
             -Profile Any -LocalPort 4000-4009 -RemoteAddress '10.1.2.3'"
        );
        // Without blocking rules, ports or a remote address.
        let cmd = rule_command(&windows(vec![]), exe, None, None).unwrap();
        assert_eq!(cmd.lines().count(), 2);
        assert!(!cmd.contains("-LocalPort") && !cmd.contains("-RemoteAddress"));

        let exe = Path::new("/opt/pythia/pythia");
        let ufw = State {
            firewall: Some(Firewall::Ufw),
            ..State::default()
        };
        assert_eq!(rule_command(&ufw, exe, None, None), None);
        assert_eq!(
            rule_command(&ufw, exe, Some(&(4000..=4009)), None).unwrap(),
            "ufw allow proto tcp from any to any port 4000:4009 comment 'Pythia PL/SQL debugger'"
        );
        assert_eq!(
            rule_command(&ufw, exe, Some(&(4000..=4000)), Some("10.1.2.3")).unwrap(),
            "ufw allow proto tcp from 10.1.2.3 to any port 4000 comment 'Pythia PL/SQL debugger'"
        );
        let firewalld = State {
            firewall: Some(Firewall::Firewalld),
            ..State::default()
        };
        assert_eq!(
            rule_command(&firewalld, exe, Some(&(4000..=4009)), None).unwrap(),
            "firewall-cmd --add-port=4000-4009/tcp && firewall-cmd --permanent --add-port=4000-4009/tcp"
        );
        assert!(
            rule_command(&firewalld, exe, Some(&(4000..=4009)), Some("10.1.2.3"))
                .unwrap()
                .contains("source address=\"10.1.2.3\" port port=\"4000-4009\"")
        );
        let mac = State {
            firewall: Some(Firewall::MacOs),
            ..State::default()
        };
        assert_eq!(
            rule_command(&mac, exe, None, None).unwrap(),
            "/usr/libexec/ApplicationFirewall/socketfilterfw --add '/opt/pythia/pythia' && \
             /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp '/opt/pythia/pythia'"
        );
        assert_eq!(rule_command(&State::default(), exe, None, None), None);
    }

    #[test]
    fn linux_states() {
        assert!(ufw_enabled("# comment\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(!ufw_enabled("ENABLED=no\n"));
        let off = State {
            firewall: Some(Firewall::Ufw),
            enabled: Some(false),
            ..State::default()
        };
        assert_eq!(off.allows(), Some(true));
        assert_eq!(off.summary(), "ufw is off.");
        let on = State {
            enabled: Some(true),
            ..off
        };
        assert_eq!(on.allows(), None);
        assert!(on.summary().contains("can't read its rules"));
        assert!(State::default().summary().starts_with("No firewall found"));
    }
}
