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
    /// The **devenv** machine UUID. `None` on every locally-initiated enroll —
    /// see [`EnrollParams::machine_id`]. The backend only sanity-checks it when
    /// non-null (`payload.machine_id is not None and != machine.id`), so a null
    /// here is the correct "don't assert an identity I can't know" signal.
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
    /// The **devenv** machine UUID (`devenv_machines.id`), asserted only when we
    /// already know it — i.e. a server-issued enroll directive supplied it. The
    /// agent CANNOT derive this locally: enroll is what RETURNS it, and the
    /// enrollment code already identifies the machine server-side. Leave `None`
    /// in every locally-initiated enroll.
    ///
    /// This is a DIFFERENT identity space from coord's `device_id` in
    /// `~/.qontinui/machine.json` — sending the latter here made the backend's
    /// `payload.machine_id != machine.id` sanity check fail unconditionally with
    /// `409 machine_id_mismatch` on every re-enroll. Coord's identity travels in
    /// [`EnrollParams::coord_device_id`] instead.
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
    // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/devenv/agent/enroll`.
    // The enrollment CODE is the credential; this runs before any device JWT
    // exists.
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
        scope_root: carried_forward_scope_root(EnvAgentConfig::load()),
    };
    cfg.save()
        .map_err(|e| format!("enrolled + key stored, but writing env-agent.json failed: {e}"))?;

    Ok(EnrollOutcome {
        machine_id: parsed.machine_id,
        environment_id,
        backend_url: backend,
    })
}

/// The `scope_root` an enroll must write, given whatever config was already on
/// disk: the PRIOR value, carried forward unchanged.
///
/// Enroll rewrites `env-agent.json` **wholesale**, so returning `None` here
/// would silently erase an operator's declared capture scope on every
/// re-enroll. The symptom would not be an error — it would be a box quietly
/// re-measuring a different toolchain and reporting drift nobody could explain.
///
/// Split out from [`run_enroll`] purely so this is reachable by a test: the
/// carry-forward sits AFTER the enroll POST, so no test that stops at
/// validation can observe it.
fn carried_forward_scope_root(prior: Option<EnvAgentConfig>) -> Option<String> {
    prior.and_then(|p| p.scope_root)
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

/// This machine's local identity as read from `~/.qontinui/machine.json`.
///
/// Deliberately does NOT carry a devenv `machine_id`: that file holds **coord's**
/// device identity, which lives in a different identity space than the devenv
/// `devenv_machines.id`. See [`EnrollParams::machine_id`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalMachineIdentity {
    /// `true` when `~/.qontinui/machine.json` was found, readable, AND parseable.
    /// This is the explicit presence signal callers should use to decide whether
    /// to warn the operator — never infer it from a wire field.
    pub machine_json_present: bool,
    /// This machine's hostname, or `None` when absent/blank.
    pub hostname: Option<String>,
    /// Coord's device identity (`device_id`) parsed as a UUID, or `None` when the
    /// file is absent or the value is not a UUID.
    pub coord_device_id: Option<uuid::Uuid>,
}

/// Parse the bytes of a `~/.qontinui/machine.json` into a [`LocalMachineIdentity`].
/// Accepts the legacy `machine_id` key as an alias for `device_id`, matching the
/// CLI reader. Unparseable input yields the default (absent) identity. Split out
/// from [`local_machine_identity`] so it is unit-testable without a real HOME.
fn parse_machine_json(bytes: &[u8]) -> LocalMachineIdentity {
    #[derive(Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
        #[serde(default)]
        hostname: String,
    }
    match serde_json::from_slice::<DeviceFile>(bytes) {
        Ok(f) => LocalMachineIdentity {
            machine_json_present: true,
            hostname: if f.hostname.trim().is_empty() {
                None
            } else {
                Some(f.hostname)
            },
            coord_device_id: uuid::Uuid::parse_str(f.device_id.trim()).ok(),
        },
        Err(_) => LocalMachineIdentity::default(),
    }
}

/// Read `~/.qontinui/machine.json` into a [`LocalMachineIdentity`]. A
/// missing/unreadable/unparseable file yields the default (all-absent) identity
/// — enroll tolerates a null identity, the backend identifies the machine from
/// the enrollment code.
pub fn local_machine_identity() -> LocalMachineIdentity {
    let Some(home) = dirs::home_dir() else {
        return LocalMachineIdentity::default();
    };
    let path = home.join(".qontinui").join("machine.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return LocalMachineIdentity::default();
    };
    parse_machine_json(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-enroll must PRESERVE a declared capture scope. Enroll replaces the
    /// whole config file, so a regression here erases the operator's
    /// `scope_root` — and because the erased state is a perfectly valid config,
    /// the only visible symptom is the box silently re-measuring a different
    /// toolchain. That is worth a test even though the helper is one line.
    #[test]
    fn enroll_carries_a_declared_scope_root_forward() {
        let prior = EnvAgentConfig {
            backend_url: "http://h:8000".to_string(),
            machine_id: "m".to_string(),
            environment_id: "e".to_string(),
            enrolled_at: Some("2026-06-22T00:00:00Z".to_string()),
            scope_root: Some("D:/qontinui-root".to_string()),
        };
        assert_eq!(
            carried_forward_scope_root(Some(prior)).as_deref(),
            Some("D:/qontinui-root"),
        );
    }

    /// A first enroll (no config on disk) and a re-enroll of a box that never
    /// declared a scope both yield `None` — the field stays absent rather than
    /// being invented.
    #[test]
    fn enroll_leaves_scope_root_unset_when_there_was_none() {
        assert!(carried_forward_scope_root(None).is_none());
        assert!(carried_forward_scope_root(Some(EnvAgentConfig {
            backend_url: "http://h:8000".to_string(),
            machine_id: "m".to_string(),
            environment_id: "e".to_string(),
            enrolled_at: None,
            scope_root: None,
        }))
        .is_none());
    }

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

    /// A PRESENT, readable `machine.json` must NOT put coord's `device_id` into
    /// the devenv `machine_id` wire field — that conflation is what made the
    /// backend's `payload.machine_id != machine.id` check fail unconditionally
    /// (`409 machine_id_mismatch`) on every re-enroll of a bound machine.
    #[test]
    fn present_machine_json_yields_no_devenv_machine_id_but_keeps_coord_device_id() {
        let identity = parse_machine_json(
            br#"{"device_id":"11111111-2222-3333-4444-555555555555","hostname":"box-1"}"#,
        );
        assert!(identity.machine_json_present);
        assert_eq!(identity.hostname.as_deref(), Some("box-1"));
        assert_eq!(
            identity.coord_device_id,
            Some(uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap())
        );

        // Build the wire body exactly as every locally-initiated enroll does.
        let body = EnrollRequest {
            enrollment_code: "ENR-ABC".to_string(),
            machine_id: None,
            hostname: identity.hostname.clone(),
            coord_device_id: identity.coord_device_id,
        };
        let v = serde_json::to_value(&body).unwrap();
        let wire_machine_id = v.get("machine_id");
        assert!(
            match wire_machine_id {
                None => true,
                Some(m) => m.is_null(),
            },
            "devenv machine_id must never be asserted from local identity, got: {v}"
        );
        assert_eq!(
            v.get("coord_device_id").and_then(|c| c.as_str()),
            Some("11111111-2222-3333-4444-555555555555"),
            "coord identity must still travel in coord_device_id, got: {v}"
        );
    }

    /// The legacy `machine_id` alias still parses, and still does NOT become the
    /// devenv `machine_id` — it is coord's identity under an old key name.
    #[test]
    fn legacy_machine_id_key_parses_as_coord_device_id() {
        let identity =
            parse_machine_json(br#"{"machine_id":"11111111-2222-3333-4444-555555555555"}"#);
        assert!(identity.machine_json_present);
        assert!(identity.hostname.is_none(), "blank hostname → None");
        assert!(identity.coord_device_id.is_some());
    }

    /// A non-UUID `device_id` is still a PRESENT file: the presence signal must
    /// not collapse into "is the coord UUID parseable", or the CLI would warn
    /// about a file that exists and is readable.
    #[test]
    fn non_uuid_device_id_is_still_present() {
        let identity = parse_machine_json(br#"{"device_id":"not-a-uuid","hostname":"box-1"}"#);
        assert!(identity.machine_json_present);
        assert!(identity.coord_device_id.is_none());
    }

    /// Unreadable/unparseable content ⇒ the all-absent identity, which is what
    /// drives the CLI's "no readable machine.json" note.
    #[test]
    fn unparseable_machine_json_yields_absent_identity() {
        for bad in [&b"{"[..], &b""[..], &b"{\"hostname\":\"box-1\"}"[..]] {
            let identity = parse_machine_json(bad);
            assert_eq!(
                identity,
                LocalMachineIdentity::default(),
                "unparseable input must yield the absent identity"
            );
            assert!(!identity.machine_json_present);
        }
    }
}
