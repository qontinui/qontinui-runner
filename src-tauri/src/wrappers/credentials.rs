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
    /// Carries only the rejected name's LENGTH: a manifest's name is
    /// wrapper-supplied, and this error's Display reaches logs and HTTP bodies.
    #[error("invalid env var name (len={0})")]
    InvalidName(usize),
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

/// Validate an env var name for storing, reading or injecting it: the
/// manifest's rule (`manifest::env_var_name_ok`, `^[A-Z_][A-Z0-9_]{0,63}$`).
/// Helps surface frontend bugs before they corrupt the keyring with weird
/// keys.
fn validate_name(name: &str) -> Result<(), CredentialError> {
    if super::manifest::env_var_name_ok(name) {
        Ok(())
    } else {
        Err(CredentialError::InvalidName(name.len()))
    }
}

/// The rule earlier builds stored names under: non-empty `[A-Z0-9_]`, any
/// length, any first character. A value stored under such a name (a leading
/// digit, more than 64 characters) must stay REMOVABLE, so `delete` and the
/// uninstall cleanup accept it; `set`, `get` and injection keep the strict
/// rule.
fn legacy_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `validate_name`, widened to the legacy rule: for removal only.
fn validate_name_for_removal(name: &str) -> Result<(), CredentialError> {
    if super::manifest::env_var_name_ok(name) || legacy_name_ok(name) {
        Ok(())
    } else {
        Err(CredentialError::InvalidName(name.len()))
    }
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
        // The keychain gate (`instance_env::os_keychain_allowed`).
        if !qontinui_runner_lib::instance_env::os_keychain_allowed() {
            return Err(qontinui_runner_lib::instance_env::keychain_disabled_error().into());
        }
        let entry = Entry::new(&keyring_service(wrapper_id), name)?;
        entry.set_password(value)?;
        Ok(())
    }

    /// Read a credential value. Returns `Ok(None)` if no entry exists.
    /// The result is plaintext — only `WrapperManager::spawn` should call
    /// this.
    pub fn get(wrapper_id: &str, name: &str) -> Result<Option<String>, CredentialError> {
        validate_name(name)?;
        if !qontinui_runner_lib::instance_env::os_keychain_allowed() {
            return Ok(None);
        }
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
        matches!(Self::get(wrapper_id, name), Ok(Some(_)))
    }

    /// Remove a credential. No-op if it doesn't exist. Accepts a name in the
    /// legacy charset too (`legacy_name_ok`), so a value an earlier build
    /// stored is never orphaned in the keyring. Such a legacy-named value is
    /// listed as `<rejected, len=N>` with `hasValue: false`, so a DELETE
    /// removes it only when the caller already knows the exact name;
    /// otherwise it is removed on uninstall (`clear_for_wrapper`).
    pub fn delete(wrapper_id: &str, name: &str) -> Result<(), CredentialError> {
        validate_name_for_removal(name)?;
        if !qontinui_runner_lib::instance_env::os_keychain_allowed() {
            return Ok(());
        }
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
            // A name valid under neither rule can hold no stored value — no
            // build's `set` accepted it — so it is skipped. A legacy name is
            // still removed. Either way only a name in the CURRENT shape is
            // ever logged; any other by its length.
            let label = super::manifest::label_if(name, super::manifest::env_var_name_ok(name));
            if validate_name_for_removal(name).is_err() {
                warn!(
                    "wrappers.credentials: skipped an invalid env var name ({}) for wrapper '{}'",
                    label, wrapper_id
                );
                continue;
            }
            if let Err(e) = Self::delete(wrapper_id, name) {
                warn!(
                    "wrappers.credentials: delete {} for wrapper '{}' failed: {}",
                    label, wrapper_id, e
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
        // The error carries the length, never the name.
        let secret = concat!("sk-", "live-0123456789");
        let msg = validate_name(secret).unwrap_err().to_string();
        assert!(
            !msg.contains(secret) && msg.contains(&format!("len={}", secret.len())),
            "{msg}"
        );
    }

    /// A name an earlier build could store (leading digit, longer than 64)
    /// stays removable, never storable or readable; a name valid under
    /// neither rule is refused for removal too, by length only.
    #[test]
    fn a_legacy_name_is_removable_but_not_storable() {
        for legacy in ["1LEAD".to_string(), "A".repeat(65)] {
            assert!(validate_name_for_removal(&legacy).is_ok(), "{legacy}");
            assert!(validate_name(&legacy).is_err(), "{legacy}");
            assert!(
                matches!(
                    CredentialStore::set("w", &legacy, "v"),
                    Err(CredentialError::InvalidName(_))
                ),
                "{legacy}"
            );
            assert!(
                matches!(
                    CredentialStore::get("w", &legacy),
                    Err(CredentialError::InvalidName(_))
                ),
                "{legacy}"
            );
        }
        let bad = concat!("sk-", "live-0123456789");
        let msg = validate_name_for_removal(bad).unwrap_err().to_string();
        assert!(
            !msg.contains(bad) && msg.contains(&format!("len={}", bad.len())),
            "{msg}"
        );
    }

    /// Uninstall over a persisted index whose env var name is invalid: the
    /// name is skipped (no keyring call) and never logged.
    #[test]
    fn uninstall_never_logs_an_invalid_env_var_name() {
        let secret = concat!("sk-", "live-0123456789");
        let logs = super::super::capture_logs(|| {
            CredentialStore::clear_for_wrapper("w", &[secret.to_string()]);
        });
        assert!(!logs.contains(secret), "{logs}");
        assert!(logs.contains("skipped an invalid env var name"), "{logs}");
    }
}
