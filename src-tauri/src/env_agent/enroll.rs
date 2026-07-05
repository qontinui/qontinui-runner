//! Shared enroll code path — binds this machine to a web devenv environment via
//! a one-time enrollment code.
//!
//! This is the SINGLE implementation of the enroll POST used by every consumer:
//! - the `qontinui_profile env enroll --code <code>` CLI (`bin/qontinui_profile.rs`),
//! - the in-app Tauri command (`commands::devenv_enroll`), and
//! - (Phase 3) the dispatched `events.devenv.enroll_requested` directive consumer.
//!
//! The enroll flow mirrors the backend contract in
//! `qontinui-web` `app/api/v1/endpoints/devenv_agent.py`: POST
//! `{enrollment_code, machine_id?, hostname?}` to
//! `{backend}/api/v1/devenv/agent/enroll`; on success the response carries the
//! machine key ONCE (stored immediately in `SecureStorage`) plus the
//! `environment_id` (ALWAYS sourced from the response, never hardcoded). Nothing
//! is written on failure.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::config::EnvAgentConfig;

/// Wire shape of the enroll request body. Conforms EXACTLY to the backend
/// contract: `{ enrollment_code, machine_id?, hostname?, coord_device_id? }`.
#[derive(Debug, Serialize)]
struct EnrollRequest {
    enrollment_code: String,
    machine_id: Option<String>,
    hostname: Option<String>,
    /// This machine's coord device identity (UUID). Omitted from the wire
    /// entirely when absent so the request stays compatible with backends that
    /// predate the contract extension (qontinui-web#697).
    #[serde(skip_serializing_if = "Option::is_none")]
    coord_device_id: Option<uuid::Uuid>,
}

/// Wire shape of the enroll response. The backend returns the machine key ONCE
/// — we store it immediately. `environment_id` is sourced from HERE.
#[derive(Debug, Deserialize)]
struct EnrollResponse {
    machine_id: String,
    machine_key: String,
    #[serde(default)]
    environment_id: Option<String>,
}

/// Fully-resolved enroll inputs. The CALLER resolves backend + identity so this
/// core is transport-agnostic and free of CLI-arg / profile coupling. Use
/// [`resolve_backend_base`] + [`local_machine_identity`] to populate the
/// non-`code` fields from the ambient environment.
#[derive(Debug, Clone)]
pub struct EnrollParams {
    /// The one-time enrollment code minted by the web dashboard.
    pub code: String,
    /// Web backend base URL (no trailing slash needed — trimmed here).
    pub backend: String,
    /// Stable machine identity from `~/.qontinui/machine.json`, or `None` to let
    /// the backend assign one.
    pub machine_id: Option<String>,
    /// This machine's hostname, or `None`.
    pub hostname: Option<String>,
    /// This machine's coord device identity (UUID) from
    /// `~/.qontinui/machine.json::device_id`. Sent to the backend so it can
    /// persist the devenv↔coord twin bridge linkage. `None` when the local
    /// identity is absent/unparseable — enroll still works, the field is just
    /// omitted from the wire.
    pub coord_device_id: Option<uuid::Uuid>,
    /// Diagnostic override for `environment_id`; the response value wins when
    /// present. Normally `None`.
    pub environment_override: Option<String>,
}

/// Successful enroll outcome — what got persisted.
#[derive(Debug, Clone, Serialize)]
pub struct EnrollOutcome {
    pub machine_id: String,
    pub environment_id: String,
    pub backend_url: String,
}

/// Perform the enroll POST, store the minted machine key in secure storage, and
/// write `~/.qontinui/env-agent.json`. Blocking (`reqwest::blocking`) — call
/// directly from the sync CLI, or via `spawn_blocking` from an async Tauri
/// command. Writes NOTHING on any failure (returns `Err`).
pub fn run_enroll(params: EnrollParams) -> Result<EnrollOutcome, String> {
    let code = params.code.trim();
    if code.is_empty() {
        return Err("enrollment code is empty".to_string());
    }
    let backend = params.backend.trim().trim_end_matches('/').to_string();
    if backend.is_empty() {
        return Err("backend base URL is empty".to_string());
    }

    let body = EnrollRequest {
        enrollment_code: code.to_string(),
        machine_id: params.machine_id.clone(),
        hostname: params.hostname.clone(),
        coord_device_id: params.coord_device_id,
    };
    let url = format!("{backend}/api/v1/devenv/agent/enroll");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {url} failed (backend unreachable?): {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!(
            "enroll failed — POST {url} -> HTTP {status}: {body_text}"
        ));
    }

    let parsed: EnrollResponse = resp
        .json()
        .map_err(|e| format!("enroll succeeded but decoding the response failed: {e}"))?;

    // environment_id ALWAYS comes from the response when present; the override is
    // a diagnostic fallback only.
    let environment_id = parsed
        .environment_id
        .clone()
        .or(params.environment_override)
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| {
            "enroll response did not include an environment_id and none was supplied; \
             refusing to write a half-enrolled config"
                .to_string()
        })?;

    // Store the machine key FIRST — it is the credential.
    let storage = crate::secure_storage::SecureStorage::new()
        .map_err(|e| format!("could not open secure storage: {e}"))?;
    storage
        .store_agent_machine_key(&parsed.machine_key)
        .map_err(|e| format!("failed to store machine key: {e}"))?;

    // Then write env-agent.json with the RESPONSE environment_id.
    let cfg = EnvAgentConfig {
        backend_url: backend.clone(),
        machine_id: parsed.machine_id.clone(),
        environment_id: environment_id.clone(),
        enrolled_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    cfg.save()
        .map_err(|e| format!("enrolled + key stored, but writing env-agent.json failed: {e}"))?;

    Ok(EnrollOutcome {
        machine_id: parsed.machine_id,
        environment_id,
        backend_url: backend,
    })
}

/// Resolve the web backend base URL for the enroll POST. Order:
/// explicit arg → `QONTINUI_WEB_BASE` → web base derived from the active
/// profile's `coord_url` (`pair::derive_web_base_from_coord(pair::coord_http_base())`).
/// Shared by the CLI (`resolve_env_backend_base`) and the Tauri command.
pub fn resolve_backend_base(explicit: Option<&str>) -> Result<String, String> {
    if let Some(b) = explicit {
        let t = b.trim();
        if !t.is_empty() {
            return Ok(t.trim_end_matches('/').to_string());
        }
    }
    if let Ok(v) = std::env::var("QONTINUI_WEB_BASE") {
        let t = v.trim();
        if !t.is_empty() {
            return Ok(t.trim_end_matches('/').to_string());
        }
    }
    let coord_base = crate::pair::coord_http_base().map_err(|e| {
        format!(
            "could not resolve a backend URL (no explicit backend, no QONTINUI_WEB_BASE, \
             and coord_url unavailable: {e})"
        )
    })?;
    Ok(crate::pair::derive_web_base_from_coord(&coord_base))
}

/// Read `~/.qontinui/machine.json` → `(machine_id, hostname, coord_device_id)`,
/// all optional. A missing/unreadable/unparseable file yields `(None, None, None)`
/// — enroll tolerates null identity (the backend may assign a machine_id).
/// Accepts the legacy `machine_id` key as an alias for `device_id`, matching the
/// CLI reader. The third element is the same `device_id` value parsed as a UUID
/// (coord's device identity), or `None` when it is absent/unparseable.
pub fn local_machine_identity() -> (Option<String>, Option<String>, Option<uuid::Uuid>) {
    #[derive(Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
        #[serde(default)]
        hostname: String,
    }
    let Some(home) = dirs::home_dir() else {
        return (None, None, None);
    };
    let path = home.join(".qontinui").join("machine.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return (None, None, None);
    };
    match serde_json::from_slice::<DeviceFile>(&bytes) {
        Ok(f) => {
            let hostname = if f.hostname.trim().is_empty() {
                None
            } else {
                Some(f.hostname)
            };
            let coord_device_id = uuid::Uuid::parse_str(f.device_id.trim()).ok();
            (Some(f.device_id), hostname, coord_device_id)
        }
        Err(_) => (None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_backend_prefers_explicit_and_trims_slash() {
        assert_eq!(
            resolve_backend_base(Some("https://qontinui.io/")).unwrap(),
            "https://qontinui.io"
        );
    }

    #[test]
    fn run_enroll_rejects_empty_code() {
        let err = run_enroll(EnrollParams {
            code: "   ".to_string(),
            backend: "https://qontinui.io".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
            environment_override: None,
        })
        .unwrap_err();
        assert!(err.contains("enrollment code is empty"), "got: {err}");
    }

    #[test]
    fn run_enroll_rejects_empty_backend() {
        let err = run_enroll(EnrollParams {
            code: "ENR-ABC".to_string(),
            backend: "  ".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
            environment_override: None,
        })
        .unwrap_err();
        assert!(err.contains("backend base URL is empty"), "got: {err}");
    }

    #[test]
    fn enroll_request_serializes_coord_device_id_only_when_present() {
        // Present → the `coord_device_id` key appears with the UUID string.
        let id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let with = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: Some("dev-1".to_string()),
            hostname: Some("host-1".to_string()),
            coord_device_id: Some(id),
        };
        let v = serde_json::to_value(&with).unwrap();
        assert_eq!(
            v.get("coord_device_id").and_then(|c| c.as_str()),
            Some("11111111-2222-3333-4444-555555555555"),
            "coord_device_id should serialize as the UUID string when Some"
        );

        // Absent → the key is OMITTED entirely (skip_serializing_if), keeping the
        // request compatible with pre-#697 backends.
        let without = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: None,
            hostname: None,
            coord_device_id: None,
        };
        let v = serde_json::to_value(&without).unwrap();
        assert!(
            v.get("coord_device_id").is_none(),
            "coord_device_id key must be absent when None, got: {v}"
        );
    }
}
