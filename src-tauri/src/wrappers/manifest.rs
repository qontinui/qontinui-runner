//! Wrapper manifest types and parsing.
//!
//! Each wrapper declares itself via the `qontinui.wrapper` field of its
//! `package.json`, and exposes a `--manifest-only` mode on its Node
//! entrypoint that prints the parsed manifest plus action descriptors as
//! one line of JSON to stdout.
//!
//! The expected stdout shape is:
//!
//! ```json
//! {
//!   "manifestVersion": 1,
//!   "manifest": { ... `qontinui.wrapper` payload ... },
//!   "actions": [
//!     { "id": "...", "description": "...", "paramSchema": {...}, "exclusive": false }
//!   ]
//! }
//! ```
//!
//! Phase 1.1 of the wrapper-runner integration plan introduces this contract;
//! see `C:/tmp/wrapper-runner-integration-plan.md` for the design doc.

use serde::{Deserialize, Serialize};

/// Currently supported manifest version. Manifests with a higher
/// `manifestVersion` are rejected by the runner so a newer wrapper format
/// can be added without silently misinterpreting old runners.
pub const SUPPORTED_MANIFEST_VERSION: u32 = 1;

/// Top-level shape of `--manifest-only` stdout (Phase 1.1 contract).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestOnlyOutput {
    /// Schema version. The runner refuses to load anything newer than
    /// [`SUPPORTED_MANIFEST_VERSION`].
    #[serde(rename = "manifestVersion", default = "default_manifest_version")]
    pub manifest_version: u32,
    /// The `qontinui.wrapper` payload from the wrapper's `package.json`.
    pub manifest: WrapperManifest,
    /// Action descriptors (id + paramSchema + optional `exclusive` flag).
    #[serde(default)]
    pub actions: Vec<ActionDescriptor>,
}

fn default_manifest_version() -> u32 {
    1
}

/// Parsed `qontinui.wrapper` field.
///
/// Field names mirror the JSON exactly (camelCase) via serde rename so we can
/// pass the struct through the HTTP layer unmodified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrapperManifest {
    /// The wrapper's stable ID (used as install dir name and credential
    /// namespace prefix). Must match `^[a-z0-9][a-z0-9-]*$` to be safe to
    /// embed in filesystem paths and keyring service names.
    pub id: String,
    /// Display name shown in the runner UI.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Transport this wrapper uses: one of [`KNOWN_TRANSPORTS`] (checked at
    /// scan time); which of them the runner spawns is
    /// `manager::SPAWNABLE_TRANSPORTS`.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Free-form category tags (used for filtering in the marketplace).
    #[serde(default)]
    pub categories: Vec<String>,
    /// Environment variables the wrapper needs.
    #[serde(rename = "envVars", default)]
    pub env_vars: Vec<EnvVarSpec>,
}

fn default_transport() -> String {
    "api".to_string()
}

/// Declares an environment variable the wrapper expects at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVarSpec {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub description: String,
}

/// Action descriptor returned alongside the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Inferred param schema (from `paramSchemaOf` in the wrapper framework).
    /// Stored as opaque JSON; the runner does not introspect it beyond
    /// passing it to UI / clients.
    #[serde(rename = "paramSchema", default)]
    pub param_schema: serde_json::Value,
    /// If `true`, dispatches of this action are serialized via a per-(wrapper,
    /// action) mutex. Defaults to `false` (parallel dispatch).
    #[serde(default)]
    pub exclusive: bool,
}

/// The longest wrapper id accepted.
pub const MAX_WRAPPER_ID_LEN: usize = 64;

/// Every transport kind `@qontinui/ui-bridge-wrapper` defines. A manifest
/// declaring anything else is refused at scan time.
pub const KNOWN_TRANSPORTS: &[&str] = &["api", "headless", "headed", "injected", "live"];

/// Validate a freshly-parsed `WrapperManifest`.
///
/// Enforces the shapes of the wrapper-supplied strings the runner later puts
/// in errors and logs:
/// - `id`: 1..=`MAX_WRAPPER_ID_LEN` of `[a-z0-9-]`, starting with a letter or
///   digit;
/// - `transport`: one of [`KNOWN_TRANSPORTS`];
/// - every `envVars[].name`: `^[A-Z_][A-Z0-9_]{0,63}$`.
///
/// A rejected value is NEVER echoed into the error (which reaches an HTTP
/// body): only its length, and for an env var its index, are. Returns an
/// error string ready to embed in an HTTP response on failure.
pub fn validate_manifest(m: &WrapperManifest) -> Result<(), String> {
    if !id_shape_ok(&m.id) {
        return Err(format!(
            "wrapper manifest: id (len={}) must be 1..={MAX_WRAPPER_ID_LEN} of [a-z0-9-] \
             (lowercase letters, digits, hyphens), starting with a letter or digit",
            m.id.len()
        ));
    }
    if !KNOWN_TRANSPORTS.contains(&m.transport.as_str()) {
        return Err(format!(
            "wrapper manifest: transport (len={}) must be one of {KNOWN_TRANSPORTS:?}",
            m.transport.len()
        ));
    }
    if let Some((i, spec)) = m
        .env_vars
        .iter()
        .enumerate()
        .find(|(_, spec)| !env_var_name_ok(&spec.name))
    {
        return Err(format!(
            "wrapper manifest: envVars[{i}].name (len={}) must match ^[A-Z_][A-Z0-9_]{{0,63}}$",
            spec.name.len()
        ));
    }
    Ok(())
}

/// `^[A-Z_][A-Z0-9_]{0,63}$`: an env var name safe to put in a log line.
pub fn env_var_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A wrapper-supplied string for a log line or an error: itself when `ok`
/// says its shape is safe, else only its length. For a record that may
/// predate scan-time validation (the persisted index).
pub fn label_if(value: &str, ok: bool) -> String {
    if ok {
        value.to_string()
    } else {
        format!("<rejected, len={}>", value.len())
    }
}

fn id_shape_ok(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= MAX_WRAPPER_ID_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest_only_output() {
        let json = r#"{
            "manifestVersion": 1,
            "manifest": {
                "id": "v0",
                "displayName": "Vercel v0",
                "description": "",
                "transport": "api",
                "categories": [],
                "envVars": []
            },
            "actions": [
                { "id": "list-recent", "paramSchema": {} }
            ]
        }"#;
        let parsed: ManifestOnlyOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.manifest.id, "v0");
        assert_eq!(parsed.actions.len(), 1);
        assert!(!parsed.actions[0].exclusive);
    }

    #[test]
    fn rejects_invalid_id() {
        let m = WrapperManifest {
            id: "Invalid_ID".to_string(),
            display_name: "x".to_string(),
            description: String::new(),
            transport: "api".to_string(),
            categories: vec![],
            env_vars: vec![],
        };
        assert!(validate_manifest(&m).is_err());
    }

    /// A rejected id is wrapper-supplied text: the error names its length,
    /// never the id itself.
    #[test]
    fn a_rejected_id_is_never_echoed() {
        let mut m = WrapperManifest {
            id: String::new(),
            display_name: "x".to_string(),
            description: String::new(),
            transport: "api".to_string(),
            categories: vec![],
            env_vars: vec![],
        };
        let secret = concat!("Bearer ", "abcDEF123");
        for id in [
            secret.to_string(),
            "-leading-hyphen".to_string(),
            String::new(),
            "a".repeat(MAX_WRAPPER_ID_LEN + 1),
        ] {
            m.id = id.clone();
            let err = validate_manifest(&m).unwrap_err();
            assert!(id.is_empty() || !err.contains(&id), "{err}");
            assert!(err.contains(&format!("len={}", id.len())), "{err}");
        }
        for ok in ["gemini-web", "v0", "a".repeat(MAX_WRAPPER_ID_LEN).as_str()] {
            m.id = ok.to_string();
            assert!(validate_manifest(&m).is_ok(), "{ok}");
        }
    }

    /// The transport and every env var name are checked at scan time, and a
    /// rejected one is reported by length (and index), never echoed.
    #[test]
    fn a_rejected_transport_or_env_var_name_is_never_echoed() {
        let base = || WrapperManifest {
            id: "w".to_string(),
            display_name: "x".to_string(),
            description: String::new(),
            transport: "api".to_string(),
            categories: vec![],
            env_vars: vec![],
        };
        let spec = |name: &str| EnvVarSpec {
            name: name.to_string(),
            required: false,
            secret: false,
            description: String::new(),
        };
        let secret = concat!("sk-", "live-0123456789");
        let mut m = base();
        m.transport = secret.to_string();
        let err = validate_manifest(&m).unwrap_err();
        assert!(
            !err.contains(secret) && err.contains(&format!("len={}", secret.len())),
            "{err}"
        );
        for t in KNOWN_TRANSPORTS {
            m.transport = t.to_string();
            assert!(validate_manifest(&m).is_ok(), "{t}");
        }
        let mut m = base();
        m.env_vars = vec![spec("GEMINI_WEB_KEY"), spec(secret)];
        let err = validate_manifest(&m).unwrap_err();
        assert!(!err.contains(secret), "{err}");
        assert!(err.contains("envVars[1]"), "{err}");
        for bad in ["lower", "1LEAD", "", &"A".repeat(65), "A-B"] {
            assert!(!env_var_name_ok(bad), "{bad}");
        }
        for good in ["_X", "A", "GEMINI_WEB_STORAGE_STATE_PATH", &"A".repeat(64)] {
            assert!(env_var_name_ok(good), "{good}");
        }
        assert_eq!(
            label_if(secret, false),
            format!("<rejected, len={}>", secret.len())
        );
        assert_eq!(label_if("API_KEY", true), "API_KEY");
    }
}
