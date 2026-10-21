//! Connection passwords in the OS keychain (never in config files).

use tracing::warn;

const SERVICE: &str = "orui";

fn entry(connection: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, connection)
        .inspect_err(|e| warn!("keychain unavailable: {e}"))
        .ok()
}

pub fn load(connection: &str) -> Option<String> {
    entry(connection)?.get_password().ok()
}

pub fn store(connection: &str, password: &str) {
    if let Some(entry) = entry(connection)
        && let Err(e) = entry.set_password(password)
    {
        warn!("storing password in keychain: {e}");
    }
}

pub fn forget(connection: &str) {
    if let Some(entry) = entry(connection) {
        let _ = entry.delete_credential();
    }
}
