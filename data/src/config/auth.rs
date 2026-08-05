use exchange::proxy::{Proxy, ProxyAuth};

// ── Proxy auth (keychain) ───────────────────────────────────────────────

const PROXY_KEYCHAIN_SERVICE: &str = "flowsurface.proxy";

fn proxy_entry_for(proxy: &Proxy) -> Result<keyring::Entry, keyring::Error> {
    let key = proxy.to_url_string_no_auth();
    keyring::Entry::new(PROXY_KEYCHAIN_SERVICE, &key)
}

/// Load proxy credentials from the system keychain.
///
/// Returns `None` if no credentials are stored, the keychain is unavailable,
/// or the stored value is not valid JSON.
pub fn load_proxy_auth(proxy: &Proxy) -> Option<ProxyAuth> {
    let key = proxy.to_url_string_no_auth();

    let entry = match proxy_entry_for(proxy) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            return None;
        }
    };

    let secret = match entry.get_password() {
        Ok(s) => s,
        Err(err) => {
            log::info!(
                "No proxy auth in keychain for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            return None;
        }
    };

    match serde_json::from_str::<ProxyAuth>(&secret) {
        Ok(auth) => Some(auth),
        Err(err) => {
            log::warn!(
                "Proxy auth in keychain is invalid JSON for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            None
        }
    }
}

/// Save proxy credentials to the system keychain.
///
/// The auth is JSON-serialized before storage.
/// Does nothing if the proxy has no auth configured.
pub fn save_proxy_auth(proxy: &Proxy) {
    let key = proxy.to_url_string_no_auth();

    let Some(auth) = proxy.auth() else {
        log::info!(
            "Not saving proxy auth: auth is None (service={PROXY_KEYCHAIN_SERVICE} key={key})"
        );
        return;
    };

    let entry = match proxy_entry_for(proxy) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            return;
        }
    };

    let secret = match serde_json::to_string(auth) {
        Ok(s) => s,
        Err(err) => {
            log::warn!(
                "Failed to serialize proxy auth for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            return;
        }
    };

    match entry.set_password(&secret) {
        Ok(()) => {
            log::info!("Stored proxy auth in keychain (service={PROXY_KEYCHAIN_SERVICE} key={key})")
        }
        Err(err) => {
            log::warn!(
                "Failed to store proxy auth in keychain (service={PROXY_KEYCHAIN_SERVICE} key={key}): {err}"
            );
        }
    }
}

/// Delete proxy credentials from the system keychain.
///
/// Uses the proxy's URL (without auth) to look up the stored credential.
/// Silently succeeds if no credential exists.
pub fn delete_proxy_auth(proxy: &Proxy) {
    let key = proxy.to_url_string_no_auth();
    let entry = match proxy_entry_for(proxy) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={PROXY_KEYCHAIN_SERVICE} key={key}: {err}"
            );
            return;
        }
    };
    match entry.delete_credential() {
        Ok(()) => log::info!(
            "Deleted proxy auth from keychain (service={PROXY_KEYCHAIN_SERVICE} key={key})"
        ),
        Err(keyring::Error::NoEntry) => { /* nothing to delete */ }
        Err(err) => log::warn!(
            "Failed to delete proxy auth from keychain (service={PROXY_KEYCHAIN_SERVICE} key={key}): {err}"
        ),
    }
}

// ── Server auth (keychain) ──────────────────────────────────────────────

const SERVER_KEYCHAIN_SERVICE: &str = "flowsurface.server";
const RITHMIC_KEYCHAIN_SERVICE: &str = "flowsurface.rithmic";

fn rithmic_entry_for(url: &str) -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(RITHMIC_KEYCHAIN_SERVICE, url)
}

pub fn load_rithmic_credentials(url: &str) -> Option<(String, String)> {
    let entry = rithmic_entry_for(url).ok()?;
    let stored = entry.get_password().ok()?;
    let (user, password) = stored.split_once('\n')?;
    if user.is_empty() || password.is_empty() {
        return None;
    }
    Some((user.to_string(), password.to_string()))
}

pub fn save_rithmic_credentials(url: &str, user: &str, password: &str) {
    if user.is_empty() || password.is_empty() || user.contains('\n') || password.contains('\n') {
        log::warn!("Refusing invalid Rithmic credentials for keychain storage");
        return;
    }
    match rithmic_entry_for(url)
        .and_then(|entry| entry.set_password(&format!("{user}\n{password}")))
    {
        Ok(()) => log::info!("Stored Rithmic credentials in keychain"),
        Err(error) => log::warn!("Failed to store Rithmic credentials in keychain: {error}"),
    }
}

pub fn delete_rithmic_credentials(url: &str) {
    match rithmic_entry_for(url).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => log::warn!("Failed to delete Rithmic credentials from keychain: {error}"),
    }
}

fn server_entry_for(url: &str) -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(SERVER_KEYCHAIN_SERVICE, url)
}

/// Load the server auth token from the system keychain.
///
/// Returns `None` if no token is stored, the keychain is unavailable,
/// or the stored value is not valid UTF-8.
pub fn load_server_token(url: &str) -> Option<String> {
    let entry = match server_entry_for(url) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={SERVER_KEYCHAIN_SERVICE} key={url}: {err}"
            );
            return None;
        }
    };

    match entry.get_password() {
        Ok(token) => {
            log::info!(
                "Loaded server auth token from keychain (service={SERVER_KEYCHAIN_SERVICE} key={url})"
            );
            Some(token)
        }
        Err(keyring::Error::NoEntry) => {
            log::info!(
                "No server auth token in keychain for service={SERVER_KEYCHAIN_SERVICE} key={url}"
            );
            None
        }
        Err(err) => {
            log::warn!(
                "Failed to read server auth token from keychain (service={SERVER_KEYCHAIN_SERVICE} key={url}): {err}"
            );
            None
        }
    }
}

/// Save the server auth token to the system keychain.
///
/// The token is stored as a plain UTF-8 string (not JSON-wrapped).
pub fn save_server_token(url: &str, token: &str) {
    let entry = match server_entry_for(url) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={SERVER_KEYCHAIN_SERVICE} key={url}: {err}"
            );
            return;
        }
    };

    match entry.set_password(token) {
        Ok(()) => log::info!(
            "Stored server auth token in keychain (service={SERVER_KEYCHAIN_SERVICE} key={url})"
        ),
        Err(err) => log::warn!(
            "Failed to store server auth token in keychain (service={SERVER_KEYCHAIN_SERVICE} key={url}): {err}"
        ),
    }
}

/// Delete the server auth token from the system keychain.
///
/// Silently succeeds if no credential exists.
pub fn delete_server_token(url: &str) {
    let entry = match server_entry_for(url) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "Keychain entry init failed for service={SERVER_KEYCHAIN_SERVICE} key={url}: {err}"
            );
            return;
        }
    };
    match entry.delete_credential() {
        Ok(()) => log::info!(
            "Deleted server auth token from keychain (service={SERVER_KEYCHAIN_SERVICE} key={url})"
        ),
        Err(keyring::Error::NoEntry) => { /* nothing to delete */ }
        Err(err) => log::warn!(
            "Failed to delete server auth token from keychain (service={SERVER_KEYCHAIN_SERVICE} key={url}): {err}"
        ),
    }
}
