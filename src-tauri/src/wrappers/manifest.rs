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
    /// Transport this wrapper uses. Phase 1 only enables `"api"`.
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

/// Validate a freshly-parsed `WrapperManifest`.
///
/// Currently only enforces the `id` charset constraint. Returns the manifest
/// unchanged on success; an error string ready to embed in an HTTP response
/// on failure.
pub fn validate_manifest(m: &WrapperManifest) -> Result<(), String> {
    if m.id.is_empty() {
        return Err("wrapper manifest: id must not be empty".to_string());
    }
    if !m
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "wrapper manifest: id '{}' must match [a-z0-9-]+ (lowercase letters, digits, hyphens)",
            m.id
        ));
    }
    if !m
        .id
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .unwrap_or(false)
    {
        return Err(format!(
            "wrapper manifest: id '{}' must start with a lowercase letter or digit",
            m.id
        ));
    }
    Ok(())
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
}
