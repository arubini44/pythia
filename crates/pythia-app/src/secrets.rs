//! Connection passwords and AI API keys in the OS keychain (never in
//! config files).

use tracing::warn;

const SERVICE: &str = "pythia";
/// The service passwords were stored under before the rename to Pythia.
const LEGACY_SERVICE: &str = "orui";
/// API keys of AI backends, by backend id.
const AI_SERVICE: &str = "pythia-ai";

fn entry(service: &str, connection: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(service, connection)
        .inspect_err(|e| warn!("keychain unavailable: {e}"))
        .ok()
}

pub fn load(connection: &str) -> Option<String> {
    if let Ok(password) = entry(SERVICE, connection)?.get_password() {
        return Some(password);
    }
    let legacy = entry(LEGACY_SERVICE, connection)?;
    let password = legacy.get_password().ok()?;
    if entry(SERVICE, connection)?.set_password(&password).is_ok() {
        let _ = legacy.delete_credential();
    }
    Some(password)
}

pub fn store(connection: &str, password: &str) {
    if let Some(entry) = entry(SERVICE, connection)
        && let Err(e) = entry.set_password(password)
    {
        warn!("storing password in keychain: {e}");
    }
}

pub fn forget(connection: &str) {
    for service in [SERVICE, LEGACY_SERVICE] {
        if let Some(entry) = entry(service, connection) {
            let _ = entry.delete_credential();
        }
    }
}

pub fn load_api_key(backend: &str) -> Option<String> {
    entry(AI_SERVICE, backend)?.get_password().ok()
}

pub fn store_api_key(backend: &str, key: &str) -> Result<(), String> {
    keyring::Entry::new(AI_SERVICE, backend)
        .and_then(|entry| entry.set_password(key))
        .map_err(|e| format!("Could not store the key in the keychain: {e}"))
}

pub fn forget_api_key(backend: &str) {
    if let Some(entry) = entry(AI_SERVICE, backend) {
        let _ = entry.delete_credential();
    }
}
