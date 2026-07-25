//! Fleet session-handle acquisition — session-identity fabric Phase 1, runner
//! slice (plan `2026-07-05-session-identity-messaging-restore-fabric.md` §4
//! handle + registry, §6.2 restore rebind, §8 Phase 1).
//!
//! After a session registers with coord
//! ([`super::coord_register::AiCoordRegistrar::register_session`]), this
//! module asks coord's `coord.session_handles` registry to mint-or-rebind the
//! session's stable `fsh_…` handle via
//! `POST /coord/session-handles/register`, keyed SERVER-side on the durable
//! `claude_session_id` (the lifecycle store's primary key, which survives
//! restart via `claude --resume <id>`), and persists the returned handle into
//! `terminal-sessions.json` next to the `claude_session_id`
//! ([`SessionLifecycleStore::set_handle`]).
//!
//! Restore is covered for free: a restarted runner re-registers every resumed
//! session (fresh in-memory dedup index), so this call runs again and the
//! server rebinds the EXISTING handle row (its `current_agent_session_id` is
//! refreshed) rather than minting a new one. If the server returns a handle
//! that differs from the locally persisted one, the local file diverged —
//! server wins ([`SessionLifecycleStore::set_handle`] warns and overwrites).
//!
//! Best-effort BY CONSTRUCTION: coord may not serve the route yet (it deploys
//! separately), the runner may be unpaired, the network may be down — none of
//! that may ever fail registration or session startup. Every failure collapses
//! to a once-per-session debug/info log, and the HTTP call runs on a detached
//! thread so no registration path or async runtime ever blocks on it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// HTTP timeout for the register call (mirrors this module family's other
/// best-effort coord calls, e.g. the agent-log flush in `coord_register`).
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);

/// Max bytes of the human session label forwarded as the handle's `name`
/// alias. UTF-8-safe truncation via [`crate::str_utils::truncate_str`].
const MAX_NAME_BYTES: usize = 120;

/// Wire body for `POST /coord/session-handles/register`.
#[derive(Debug, Clone, Serialize)]
pub struct HandleRegisterRequest {
    /// The durable anchor the registry mints/rebinds on (lifecycle-store PK).
    pub claude_session_id: String,
    /// The volatile coord `agent_sessions` id this boot registered under —
    /// the server stamps it as the handle's `current_agent_session_id`.
    pub agent_session_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// Human alias (the session label), if one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<Uuid>,
}

/// Extract the `handle` field from a 2xx register-response body. Pure so it
/// is unit-testable; accepts only a non-empty string `handle`.
pub(crate) fn extract_handle(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let h = v.get("handle")?.as_str()?.trim();
    if h.is_empty() {
        None
    } else {
        Some(h.to_string())
    }
}

/// True the FIRST time this `claude_session_id` records a failure this
/// process — the once-per-session gate on failure logging (coord may not
/// serve the route for weeks; one line per session, not one per retry).
fn first_failure_for(claude_session_id: &str) -> bool {
    static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = LOGGED.get_or_init(|| Mutex::new(HashSet::new()));
    match set.lock() {
        Ok(mut g) => g.insert(claude_session_id.to_string()),
        Err(_) => true, // poisoned — err on the side of logging
    }
}

/// Fire the handle mint/rebind on a DETACHED thread and, on success, persist
/// the returned handle into the lifecycle store (only-write-when-changed;
/// server wins on divergence — both inside
/// [`SessionLifecycleStore::set_handle`]).
///
/// Callers gate on an ATTACHED lifecycle store (production attaches at boot;
/// unit-test registrars never do), which doubles as the guard keeping unit
/// tests network-silent — `register_session` under test must not POST to a
/// live coord.
///
/// Never blocks, never fails the caller: a thread-spawn failure is a debug
/// log and nothing else.
pub fn spawn_register(store: Arc<SessionLifecycleStore>, req: HandleRegisterRequest) {
    let sid_for_log = req.claude_session_id.clone();
    let spawned = std::thread::Builder::new()
        .name("session-handle-register".to_string())
        .spawn(move || {
            if let Some(handle) = register_blocking(&req) {
                store.set_handle(&req.claude_session_id, &handle);
            }
        });
    if let Err(e) = spawned {
        debug!(
            claude_session_id = %sid_for_log,
            error = %e,
            "session_handle: failed to spawn register thread (best-effort)"
        );
    }
}

/// Blocking mint/rebind call. Device-authed via the same best-effort bearer
/// pattern as this module family's other coord calls (`AuthManager`
/// access-token slot = the device-JWT; a missing token sends anonymously and
/// lets coord 401 — a normal best-effort failure, never fatal). Returns the
/// minted/rebound handle on 2xx, `None` on any failure.
fn register_blocking(req: &HandleRegisterRequest) -> Option<String> {
    let Some(base) = super::coord_register::coord_http_base() else {
        // Coord not configured (dev box without a profile) — silent no-op.
        return None;
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(REGISTER_TIMEOUT)
        .build()
        .ok()?;
    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .filter(|t| !t.is_empty());

    let url = format!("{base}/coord/session-handles/register");
    let mut r = client.post(&url).json(req);
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }

    match r.send() {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().unwrap_or_default();
            match extract_handle(&body) {
                Some(handle) => {
                    info!(
                        claude_session_id = %req.claude_session_id,
                        agent_session_id = %req.agent_session_id,
                        handle = %handle,
                        "session_handle: acquired/rebound fleet session handle"
                    );
                    Some(handle)
                }
                None => {
                    if first_failure_for(&req.claude_session_id) {
                        warn!(
                            claude_session_id = %req.claude_session_id,
                            "session_handle: 2xx register response carried no handle — ignoring"
                        );
                    }
                    None
                }
            }
        }
        Ok(resp) => {
            // 404/503 are EXPECTED until coord deploys the registry route —
            // info once per session, then silent.
            if first_failure_for(&req.claude_session_id) {
                info!(
                    claude_session_id = %req.claude_session_id,
                    status = %resp.status(),
                    "session_handle: coord register route unavailable (best-effort — retried on next registration)"
                );
            }
            None
        }
        Err(e) => {
            if first_failure_for(&req.claude_session_id) {
                debug!(
                    claude_session_id = %req.claude_session_id,
                    error = %e,
                    "session_handle: coord register call failed (best-effort — retried on next registration)"
                );
            }
            None
        }
    }
}

/// Normalize a session label into the handle `name` alias: trimmed,
/// UTF-8-safely truncated to [`MAX_NAME_BYTES`], empty → `None`. Pure so it
/// is unit-testable.
pub(crate) fn name_alias(label: &str) -> Option<String> {
    let t = crate::str_utils::truncate_str(label.trim(), MAX_NAME_BYTES).trim_end();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_handle_accepts_only_nonempty_string_handle() {
        assert_eq!(
            extract_handle(r#"{"handle":"fsh_abc123","status":"live"}"#).as_deref(),
            Some("fsh_abc123")
        );
        assert_eq!(
            extract_handle(r#"{"handle":"  fsh_ws  "}"#).as_deref(),
            Some("fsh_ws"),
            "surrounding whitespace is trimmed"
        );
        assert!(extract_handle(r#"{"handle":""}"#).is_none());
        assert!(extract_handle(r#"{"handle":"   "}"#).is_none());
        assert!(extract_handle(r#"{"handle":42}"#).is_none());
        assert!(extract_handle(r#"{"other":"x"}"#).is_none());
        assert!(extract_handle("not json").is_none());
        assert!(extract_handle("").is_none());
    }

    #[test]
    fn name_alias_trims_truncates_utf8_safely_and_drops_empty() {
        assert_eq!(
            name_alias("  fix the thing  ").as_deref(),
            Some("fix the thing")
        );
        assert!(name_alias("").is_none());
        assert!(name_alias("   ").is_none());
        // Multi-byte truncation never splits a char (é is 2 bytes; a byte
        // limit landing mid-char must back off, not panic).
        let long = "é".repeat(MAX_NAME_BYTES); // 2×MAX bytes
        let out = name_alias(&long).expect("non-empty");
        assert!(out.len() <= MAX_NAME_BYTES);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn request_serializes_omitting_absent_optionals() {
        let req = HandleRegisterRequest {
            claude_session_id: "cs-1".to_string(),
            agent_session_id: Uuid::nil(),
            task_run_id: None,
            terminal_id: None,
            name: None,
            machine_id: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["claude_session_id"], "cs-1");
        assert!(v.get("task_run_id").is_none());
        assert!(v.get("terminal_id").is_none());
        assert!(v.get("name").is_none());
        assert!(v.get("machine_id").is_none());
    }
}
