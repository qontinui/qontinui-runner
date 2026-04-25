//! Per-wrapper credential storage backed by the OS keyring.
//!
//! Each wrapper has its own namespace; entries are keyed by the env var
//! name. The keyring service name is `com.qontinui.runner.wrapper.<id>`
//! and the account name is the env var literal — that pair gives every
//! `(wrapper, envvar)` a unique `(service, account)` slot in the OS
//! credential store.
//!
//! ## Security invariants
//!
//! - Decrypted values are returned only by [`CredentialStore::get`], which
//!   is invoked exclusively by [`super::manager::WrapperManager`] when
//!   spawning a wrapper subprocess (env-injection point).
//! - The HTTP route surface (`list`, `set`, `delete`) NEVER returns the
//!   plaintext value. `list` only reports presence (`hasValue: bool`).
//! - Errors and trace messages must not include the value. The helpers
//!   in this file deliberately omit the secret from their `Display` impls.

use keyring::Entry;
use tracing::warn;

/// Service-name prefix for every wrapper credential entry. Combined with
/// the wrapper id and env-var name to produce a unique slot per
/// `(wrapper, envvar)` pair.
pub const KEYRING_SERVICE_PREFIX: &str = "com.qontinui.runner.wrapper";

/// Compute the service name used in the OS keyring for a given wrapper id.
///
/// Pattern: `com.qontinui.runner.wrapper.<id>`. The wrapper-id itself is
/// validated upstream (`manifest::validate_manifest`) to be `[a-z0-9-]+`,
/// which is safe to embed in keyring service names on Windows / macOS /
/// Linux backends.
pub fn keyring_service(wrapper_id: &str) -> String {
    format!("{}.{}", KEYRING_SERVICE_PREFIX, wrapper_id)
}

/// Errors raised by credential operations.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("invalid env var name '{0}'")]
    InvalidName(String),
    #[error("keyring error: {0}")]
    Keyring(String),
}

impl From<keyring::Error> for CredentialError {
    fn from(e: keyring::Error) -> Self {
        // Crucially, keyring::Error's Display formats only metadata —
        // never the value.
        CredentialError::Keyring(e.to_string())
    }
}

/// Validate an env var name. Allows the standard unix env var charset
/// (`A-Z`, `0-9`, `_`) and forbids the empty string. Helps surface
/// frontend bugs before they corrupt the keyring with weird keys.
fn validate_name(name: &str) -> Result<(), CredentialError> {
    if name.is_empty() {
        return Err(CredentialError::InvalidName(name.to_string()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(CredentialError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Per-wrapper credential namespace.
///
/// All operations are scoped to one wrapper id; cross-wrapper access is
/// only possible by constructing a new `CredentialStore` with a different
/// id, which mirrors the keyring's own scoping model.
pub struct CredentialStore;

impl CredentialStore {
    /// Set a credential value for `(wrapper_id, name)`. Overwrites any
    /// existing value.
    pub fn set(wrapper_id: &str, name: &str, value: &str) -> Result<(), CredentialError> {
        validate_name(name)?;
        let entry = Entry::new(&keyring_service(wrapper_id), name)?;
        entry.set_password(value)?;
        Ok(())
    }

    /// Read a credential value. Returns `Ok(None)` if no entry exists.
    /// The result is plaintext — only `WrapperManager::spawn` should call
    /// this.
    pub fn get(wrapper_id: &str, name: &str) -> Result<Option<String>, CredentialError> {
        validate_name(name)?;
        let entry = Entry::new(&keyring_service(wrapper_id), name)?;
        match entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => {
                // Don't leak the entry name into a generic warn — it's
                // safe (env var name is not the secret), but stay tight.
                warn!(
                    "wrappers.credentials: keyring read failed for service '{}': {}",
                    keyring_service(wrapper_id),
                    e
                );
                Err(e.into())
            }
        }
    }

    /// Quick existence probe. Defined separately so HTTP `list` callers
    /// don't have to interpret a `Some(...)` they aren't allowed to read.
    pub fn has(wrapper_id: &str, name: &str) -> bool {
        match Self::get(wrapper_id, name) {
            Ok(Some(_)) => true,
            _ => false,
        }
    }

    /// Remove a credential. No-op if it doesn't exist.
    pub fn delete(wrapper_id: &str, name: &str) -> Result<(), CredentialError> {
        validate_name(name)?;
        let entry = Entry::new(&keyring_service(wrapper_id), name)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Clear all credentials for a wrapper. Called on uninstall.
    ///
    /// Iterates the manifest's declared `envVars` rather than the keyring
    /// itself because the OS keyring backends don't expose a "list entries
    /// for service" API portably. If the user previously stored a value
    /// for a now-removed env var, it will linger — that's an expected
    /// keyring backend limitation, called out in the install docs.
    pub fn clear_for_wrapper(wrapper_id: &str, env_var_names: &[String]) {
        for name in env_var_names {
            if let Err(e) = Self::delete(wrapper_id, name) {
                warn!(
                    "wrappers.credentials: delete '{}' for wrapper '{}' failed: {}",
                    name, wrapper_id, e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyring_service_uses_documented_pattern() {
        assert_eq!(keyring_service("v0"), "com.qontinui.runner.wrapper.v0");
    }

    #[test]
    fn rejects_invalid_env_var_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("lowercase").is_err());
        assert!(validate_name("HAS-DASH").is_err());
        assert!(validate_name("V0_TOKEN").is_ok());
    }
}
