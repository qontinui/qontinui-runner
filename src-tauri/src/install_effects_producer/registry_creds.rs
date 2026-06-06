//! Private-registry credential storage + subprocess env-injection for the
//! install-effects producer (Phase 4).
//!
//! A private package registry (a corp npm registry, a cargo alt-registry, a
//! private PyPI) needs an auth token to resolve/download. The producer must be
//! able to run its dry-run / install / audit subprocesses with that token in
//! the environment — but coord must NEVER see it, it must NEVER be logged, and
//! the HTTP surface must NEVER return it (presence only).
//!
//! ## Design (mirrors `wrappers::credentials`)
//!
//! - Backed by the OS keyring (`keyring::Entry`), service-scoped to
//!   [`KEYRING_SERVICE`] (`com.qontinui.runner.registry`), account = the env
//!   var name the token is injected as. Same `(service, account)` slot model
//!   as the wrapper store, but a single flat registry namespace rather than one
//!   per wrapper.
//! - Account names are validated `[A-Z0-9_]+` (the unix env-var charset) — the
//!   exact same rule as `wrappers::credentials::validate_name`. The supported
//!   names are the registry-auth env vars the package managers read:
//!   `NPM_TOKEN`, `CARGO_REGISTRIES_<NAME>_TOKEN`, `PIP_INDEX_URL`,
//!   `PIP_EXTRA_INDEX_URL`, `UV_INDEX_URL` — all of which match `[A-Z0-9_]+`.
//!
//! ## Security invariants (identical posture to `wrappers::credentials`)
//!
//! - Plaintext is returned ONLY by [`RegistryCredentialStore::get`], which is
//!   called exclusively by [`resolve_registry_env`] when building the
//!   subprocess env map. It is never returned by any route.
//! - The route surface reports presence only (`hasValue: bool`).
//! - Values are never logged. The injected env map is never folded into any
//!   coord payload (declare / predict-and-check / fs-observations / verify all
//!   build their bodies from typed fields, never from the process env).

use std::collections::BTreeMap;

use keyring::Entry;
use tracing::warn;

/// Service name used in the OS keyring for every registry credential entry.
/// Flat (single namespace) — unlike the per-wrapper service of
/// `wrappers::credentials`. The account name (the env var) makes each slot
/// unique.
pub const KEYRING_SERVICE: &str = "com.qontinui.runner.registry";

/// The fixed set of registry-auth env var names the producer knows how to
/// inject. A stored value for any of these is threaded into the dry-run /
/// install / audit subprocesses. This set bounds the keyring "list" walk (the
/// OS backends have no portable "enumerate entries for a service" API, so we
/// probe a known set — the same approach as
/// `wrappers::credentials::clear_for_wrapper`).
///
/// `CARGO_REGISTRIES_<NAME>_TOKEN` is variable (the `<NAME>` is the alt-registry
/// id), so we can't enumerate every possible cargo name; the route still
/// accepts any valid `[A-Z0-9_]+` name (so an operator can store
/// `CARGO_REGISTRIES_CORP_TOKEN`), and [`resolve_registry_env`] additionally
/// injects any of these well-known names that have a value. To also inject a
/// custom cargo name, it must be listed in a per-run `extra_names` set — see
/// [`resolve_registry_env`].
pub const WELL_KNOWN_NAMES: &[&str] = &[
    "NPM_TOKEN",
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "UV_INDEX_URL",
];

/// Errors raised by registry-credential operations. Mirrors
/// `wrappers::credentials::CredentialError`; `Display` never includes a value.
#[derive(Debug, thiserror::Error)]
pub enum RegistryCredentialError {
    #[error("invalid registry env var name '{0}' (must be [A-Z0-9_]+)")]
    InvalidName(String),
    #[error("keyring error: {0}")]
    Keyring(String),
}

impl From<keyring::Error> for RegistryCredentialError {
    fn from(e: keyring::Error) -> Self {
        // keyring::Error's Display formats only metadata — never the value.
        RegistryCredentialError::Keyring(e.to_string())
    }
}

/// Validate a registry env-var name: the standard unix env-var charset
/// (`A-Z`, `0-9`, `_`), non-empty. Identical rule to
/// `wrappers::credentials::validate_name` — kept local so the two stores stay
/// independent but the contract is the same.
pub fn validate_name(name: &str) -> Result<(), RegistryCredentialError> {
    if name.is_empty() {
        return Err(RegistryCredentialError::InvalidName(name.to_string()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(RegistryCredentialError::InvalidName(name.to_string()));
    }
    Ok(())
}

// ===========================================================================
// Store abstraction (trait so tests feed a fake — no real keyring needed)
// ===========================================================================

/// Read-only lookup of a registry credential value by env-var name. Abstracted
/// behind a trait so the env-injection path ([`resolve_registry_env`]) is
/// unit-testable with a fake map — unit tests never touch a real OS keyring.
///
/// Implementors MUST validate the name (reject non-`[A-Z0-9_]+`) and return
/// `Ok(None)` for an absent entry.
pub trait RegistryTokenLookup {
    /// Return the stored plaintext for `name`, or `None` if absent. Plaintext —
    /// callers must only use it for subprocess env injection.
    fn lookup(&self, name: &str) -> Option<String>;
}

/// The OS-keyring-backed registry credential store. The production
/// [`RegistryTokenLookup`].
pub struct RegistryCredentialStore;

impl RegistryCredentialStore {
    /// Store `value` under `name`. Overwrites any existing value.
    pub fn set(name: &str, value: &str) -> Result<(), RegistryCredentialError> {
        validate_name(name)?;
        let entry = Entry::new(KEYRING_SERVICE, name)?;
        entry.set_password(value)?;
        Ok(())
    }

    /// Read the plaintext for `name`. `Ok(None)` if absent. Plaintext — only
    /// [`resolve_registry_env`] should call this.
    pub fn get(name: &str) -> Result<Option<String>, RegistryCredentialError> {
        validate_name(name)?;
        let entry = Entry::new(KEYRING_SERVICE, name)?;
        match entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => {
                // The env-var name is not the secret, but stay tight: log the
                // service only, never the value.
                warn!(
                    "install-effects.registry-creds: keyring read failed for service '{}': {}",
                    KEYRING_SERVICE, e
                );
                Err(e.into())
            }
        }
    }

    /// Presence probe — defined separately so the HTTP route never has to
    /// interpret a `Some(...)` plaintext it isn't allowed to read.
    pub fn has(name: &str) -> bool {
        matches!(Self::get(name), Ok(Some(_)))
    }

    /// Remove the credential under `name`. No-op if absent.
    pub fn delete(name: &str) -> Result<(), RegistryCredentialError> {
        validate_name(name)?;
        let entry = Entry::new(KEYRING_SERVICE, name)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl RegistryTokenLookup for RegistryCredentialStore {
    fn lookup(&self, name: &str) -> Option<String> {
        // A keyring read error is treated as "absent" for injection purposes —
        // a private-registry token we can't read just degrades to "no auth",
        // never blocks the install.
        Self::get(name).ok().flatten()
    }
}

// ===========================================================================
// Env resolution (the injection seam)
// ===========================================================================

/// The resolved registry-auth env map injected into the producer's dry-run /
/// install / audit subprocesses. A plain `(name → value)` map; constructed once
/// per run by [`resolve_registry_env`] and threaded through the command
/// builders (each calls [`RegistryEnv::apply`] on the `Command`).
///
/// The values are plaintext tokens — this type is deliberately NOT `Serialize`
/// and is never folded into a coord payload or a log line.
#[derive(Debug, Clone, Default)]
pub struct RegistryEnv {
    vars: BTreeMap<String, String>,
}

impl RegistryEnv {
    /// An empty env (no private-registry credentials configured). Subprocess
    /// inherits the runner's environment unchanged.
    pub fn empty() -> Self {
        RegistryEnv {
            vars: BTreeMap::new(),
        }
    }

    /// True iff no credentials resolved (the common case — no private registry).
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The names injected (for tests / diagnostics). NEVER exposes values.
    pub fn names(&self) -> Vec<&str> {
        self.vars.keys().map(String::as_str).collect()
    }

    /// Apply the resolved vars onto a subprocess [`Command`] just before spawn.
    /// Called by every producer command builder so injection lives in ONE place
    /// per builder rather than at each spawn site.
    pub fn apply(&self, cmd: &mut std::process::Command) {
        for (k, v) in &self.vars {
            cmd.env(k, v);
        }
    }

    /// Build directly from a name→value map. Test seam.
    #[cfg(test)]
    pub fn from_map(vars: BTreeMap<String, String>) -> Self {
        RegistryEnv { vars }
    }
}

/// Resolve the registry-auth env map from `store` for one producer run.
///
/// Walks [`WELL_KNOWN_NAMES`] plus any `extra_names` (e.g. a custom
/// `CARGO_REGISTRIES_<NAME>_TOKEN` the caller knows about), validating each and
/// injecting the ones that have a stored value. Invalid names in `extra_names`
/// are skipped (never panics). A keyring read failure for a name ⇒ that name is
/// simply absent (honest degradation — never blocks the install).
pub fn resolve_registry_env<L: RegistryTokenLookup + ?Sized>(
    store: &L,
    extra_names: &[String],
) -> RegistryEnv {
    let mut vars = BTreeMap::new();
    let candidates = WELL_KNOWN_NAMES
        .iter()
        .map(|s| s.to_string())
        .chain(extra_names.iter().cloned());
    for name in candidates {
        if validate_name(&name).is_err() {
            continue;
        }
        if let Some(val) = store.lookup(&name) {
            vars.insert(name, val);
        }
    }
    RegistryEnv { vars }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory fake lookup — feeds env resolution without a real keyring.
    struct FakeStore(BTreeMap<String, String>);
    impl RegistryTokenLookup for FakeStore {
        fn lookup(&self, name: &str) -> Option<String> {
            // Mirror the real store: reject invalid names, return absent.
            if validate_name(name).is_err() {
                return None;
            }
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn keyring_service_is_registry_scoped() {
        assert_eq!(KEYRING_SERVICE, "com.qontinui.runner.registry");
    }

    #[test]
    fn rejects_invalid_env_var_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("lowercase").is_err());
        assert!(validate_name("HAS-DASH").is_err());
        assert!(validate_name("NPM_TOKEN").is_ok());
        assert!(validate_name("CARGO_REGISTRIES_CORP_TOKEN").is_ok());
        assert!(validate_name("PIP_INDEX_URL").is_ok());
    }

    #[test]
    fn resolve_injects_only_present_well_known_names() {
        let mut m = BTreeMap::new();
        m.insert("NPM_TOKEN".to_string(), "secret-npm".to_string());
        let env = resolve_registry_env(&FakeStore(m), &[]);
        assert_eq!(env.names(), vec!["NPM_TOKEN"]);
        assert!(!env.is_empty());
    }

    #[test]
    fn resolve_empty_when_nothing_stored() {
        let env = resolve_registry_env(&FakeStore(BTreeMap::new()), &[]);
        assert!(env.is_empty());
        assert!(env.names().is_empty());
    }

    #[test]
    fn resolve_includes_valid_extra_cargo_name() {
        let mut m = BTreeMap::new();
        m.insert(
            "CARGO_REGISTRIES_CORP_TOKEN".to_string(),
            "cargo-secret".to_string(),
        );
        let env = resolve_registry_env(&FakeStore(m), &["CARGO_REGISTRIES_CORP_TOKEN".to_string()]);
        assert_eq!(env.names(), vec!["CARGO_REGISTRIES_CORP_TOKEN"]);
    }

    #[test]
    fn resolve_skips_invalid_extra_names() {
        let mut m = BTreeMap::new();
        m.insert("bad-name".to_string(), "x".to_string());
        let env = resolve_registry_env(&FakeStore(m), &["bad-name".to_string()]);
        assert!(env.is_empty(), "invalid extra name must not be injected");
    }

    #[test]
    fn apply_sets_env_on_command() {
        let mut m = BTreeMap::new();
        m.insert("NPM_TOKEN".to_string(), "tok123".to_string());
        m.insert(
            "PIP_INDEX_URL".to_string(),
            "https://corp/simple".to_string(),
        );
        let env = RegistryEnv::from_map(m);

        let mut cmd = std::process::Command::new("cargo");
        env.apply(&mut cmd);

        // Assert on the built Command's env iterator (no spawn needed).
        let got: BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().to_string(),
                    v?.to_string_lossy().to_string(),
                ))
            })
            .collect();
        assert_eq!(got.get("NPM_TOKEN").map(String::as_str), Some("tok123"));
        assert_eq!(
            got.get("PIP_INDEX_URL").map(String::as_str),
            Some("https://corp/simple")
        );
    }
}
