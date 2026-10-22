use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privilege {
    #[default]
    Normal,
    Sysdba,
    Sysoper,
    Sysbackup,
}

impl Privilege {
    pub const ALL: [Privilege; 4] = [
        Privilege::Normal,
        Privilege::Sysdba,
        Privilege::Sysoper,
        Privilege::Sysbackup,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Privilege::Normal => "Normal",
            Privilege::Sysdba => "SYSDBA",
            Privilege::Sysoper => "SYSOPER",
            Privilege::Sysbackup => "SYSBACKUP",
        }
    }
}

/// A saved connection. Never contains the password.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionProfile {
    pub name: String,
    /// Slash separated folder path, e.g. `customer/prod`.
    pub folder: String,
    pub username: String,
    /// TNS alias, EZConnect (`host:port/service`) or a full descriptor.
    pub connect_string: String,
    pub privilege: Privilege,
    /// Production connections get a red marker and extra confirmations.
    pub production: bool,
    pub read_only: bool,
    /// Optional RGB accent shown in tabs and the status bar.
    pub color: Option<[u8; 3]>,
    /// The password is stored in the OS keychain.
    pub save_password: bool,
    /// Schema to work in after connecting (`ALTER SESSION SET CURRENT_SCHEMA`).
    pub default_schema: String,
    /// SQL run on every new session, e.g. NLS settings.
    pub login_script: String,
}

impl ConnectionProfile {
    /// `user@connect_string`, the way PL/SQL Developer shows connections.
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            format!("{}@{}", self.username, self.connect_string)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Connections {
    #[serde(rename = "connection")]
    pub profiles: Vec<ConnectionProfile>,
}

impl Connections {
    /// Adds the profile, replacing an existing one with the same display name.
    pub fn upsert(&mut self, profile: ConnectionProfile) {
        let name = profile.display_name();
        match self.profiles.iter_mut().find(|p| p.display_name() == name) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
    }
}
