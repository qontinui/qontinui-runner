//! On-disk enrollment state for the machine-side capture agent.
//!
//! The capture agent runs on a developer's machine, captures that machine's
//! real dev-environment configuration (secret-free), and POSTs it to the
//! qontinui-web backend so the server can compute drift vs a canonical machine.
//!
//! Authentication is a per-machine API key (`mk_<token>`) — NOT a user JWT.
//! The key is minted ONCE by the enroll endpoint and stored in the encrypted
//! `SecureStorage` (see `secure_storage::store_agent_machine_key`). This file
//! holds only the NON-secret enrollment state: the backend URL, the
//! machine_id, the environment_id (returned by enroll), and when we enrolled.
//!
//! `~/.qontinui/env-agent.json` is written atomically (tmp + rename), mirroring
//! `fleet::write_last_budget_cache` / `pair::backfill_paired_tenant_id`.
//!
//! **`environment_id` is ALWAYS sourced from the enroll response — never
//! hardcoded.** Until the agent is enrolled (`is_enrolled()` false) the periodic
//! capture loop no-ops.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted enrollment state for the env-capture agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvAgentConfig {
    /// qontinui-web backend base URL (e.g. `http://localhost:8000` or
    /// `https://qontinui.io`). The capture PUT targets
    /// `{backend_url}/api/v1/devenv/agent/environments/{environment_id}/config`.
    pub backend_url: String,
    /// Stable per-machine identity returned by the enroll endpoint. Matches the
    /// `machine_id` carried in the enroll request (or the server-assigned one
    /// when the request sent `null`).
    pub machine_id: String,
    /// Environment this machine is enrolled into. Returned by the enroll
    /// response — NEVER hardcoded. Capture pushes target this id.
    pub environment_id: String,
    /// RFC3339 timestamp of when enrollment succeeded. Informational; `None`
    /// for a hand-written config that omitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<String>,
}

impl EnvAgentConfig {
    /// Path of `~/.qontinui/env-agent.json` for the current user.
    pub fn path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".qontinui").join("env-agent.json"))
    }

    /// Load the persisted config, or `None` when the file is absent /
    /// unreadable / unparseable. A missing file is the common "never enrolled"
    /// case, so a `None` return is not an error.
    pub fn load() -> Option<Self> {
        let path = Self::path()?;
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist this config atomically (tmp + rename), creating
    /// `~/.qontinui/` if needed. Mirrors the atomic-write idiom used across the
    /// runner (`fleet::write_last_budget_cache`, `pair::persist_pairing`).
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or_else(|| "could not resolve home directory".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let pretty =
            serde_json::to_vec_pretty(self).map_err(|e| format!("serialize env-agent.json: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &pretty).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        Ok(())
    }

    /// True when a machine_id + environment_id are both present and non-empty.
    /// The presence of an actual `mk_` key is a separate concern owned by
    /// `secure_storage::get_agent_machine_key`; callers that POST also check
    /// that.
    pub fn is_enrolled(&self) -> bool {
        !self.machine_id.trim().is_empty() && !self.environment_id.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("env-agent.json");
        let cfg = EnvAgentConfig {
            backend_url: "http://localhost:8000".to_string(),
            machine_id: "machine-abc".to_string(),
            environment_id: "env-123".to_string(),
            enrolled_at: Some("2026-06-22T00:00:00Z".to_string()),
        };
        let pretty = serde_json::to_vec_pretty(&cfg).unwrap();
        std::fs::write(&path, pretty).unwrap();
        let loaded: EnvAgentConfig =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.machine_id, "machine-abc");
        assert_eq!(loaded.environment_id, "env-123");
        assert!(loaded.is_enrolled());
    }

    #[test]
    fn is_enrolled_false_when_ids_blank() {
        let cfg = EnvAgentConfig {
            backend_url: "http://localhost:8000".to_string(),
            machine_id: "".to_string(),
            environment_id: "".to_string(),
            enrolled_at: None,
        };
        assert!(!cfg.is_enrolled());
    }

    #[test]
    fn legacy_config_without_enrolled_at_deserializes() {
        let raw = r#"{"backend_url":"http://h:8000","machine_id":"m","environment_id":"e"}"#;
        let parsed: EnvAgentConfig = serde_json::from_str(raw).expect("must decode");
        assert!(parsed.enrolled_at.is_none());
        assert!(parsed.is_enrolled());
    }
}
