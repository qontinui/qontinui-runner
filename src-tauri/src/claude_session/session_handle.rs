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
//!
//! ## Which id goes in `agent_session_id` (the 2026-07-29 correction)
//!
//! coord gates the presented `agent_session_id` through
//! `agent_sessions::session_on_device` (fail-closed) before it may enter a
//! handle's current pointer or lineage, so the value MUST be a
//! `coord.agent_sessions.id` bound to this device. That is the durable
//! `claude_session_id` — NOT the `coord.sessions.id` that
//! [`super::coord_register::AiCoordRegistrar::register_inner`] mints per boot
//! as `crate::session::uuid_v7()`. The two tables are the plan's §2.6 trap.
//!
//! The proof is on both sides of the wire: the runner publishes the anchor as
//! `claude_code_session_id` on the `Started` create-body, and coord's
//! `sessions.rs::create_session` upserts
//! `coord.agent_sessions(id = req.claude_code_session_id, device_id)`. Two
//! more signals agree — coord's mailbox scopes `to_session IN (SELECT id FROM
//! coord.agent_sessions WHERE device_id = $1)` while the shipped
//! `mcp::session_message_poller::resolve_target` resolves that same
//! `to_session` against this store by `claude_session_id`; and
//! `agent_runtime`'s `LaunchPayload::agent_session_id` is documented as the
//! Claude session uuid.
//!
//! Until this correction the caller passed the `coord.sessions` uuid, so
//! EVERY register was refused `403 agent_session_id is not bound to your
//! device` and the registry stayed empty — silently, because the outcome was
//! collapsed into one best-effort log. That is why [`HandleRegisterOutcome`]
//! now counts each terminal state and `GET /health` exposes it: coord cannot
//! see a refusal it never recorded a row for, so the runner is the only place
//! this break point is knowable.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// The coord `agent_sessions` id this session is addressable by — the
    /// server stamps it as the handle's `current_agent_session_id` and gates
    /// it fail-closed through `session_on_device`. See the module docs: this
    /// is the anchor's uuid, NOT the per-boot `coord.sessions` id.
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
    /// Whether a live runner terminal backs this handle — the §7 prompt
    /// resolver's precondition. ALWAYS sent (never elided): coord stores the
    /// column with `DEFAULT false`, so an omitted field on a REBIND would
    /// leave a stale `true` standing for a session whose PTY has since died,
    /// and an omitted field on a MINT would silently pin every handle
    /// non-promptable — which is how Phase 5 would end up with no resolver at
    /// all.
    pub promptable: bool,
}

/// The terminal state of one register attempt. Counted per-process and
/// exposed on `GET /health` so a systematically-refused register can never
/// again look identical to "coord has not deployed the route yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandleRegisterOutcome {
    /// 2xx, `minted: true` — a fresh `fsh_` handle was created.
    Minted,
    /// 2xx, `minted: false` — an existing handle row was re-bound.
    Rebound,
    /// 403 — coord refused the binding (`session_on_device` fail-closed, or a
    /// device/tenant mismatch on an existing handle). THE signal that the
    /// presented `agent_session_id` is in the wrong id space.
    Denied,
    /// 503 `registry_unavailable` — the qontinui-web alembic migration has
    /// not applied yet. Expected, transient, non-fatal.
    RegistryUnavailable,
    /// 404 — coord is deployed without the route (pre-Phase-1 build).
    RouteAbsent,
    /// Any other non-2xx status.
    OtherStatus,
    /// The request never completed (network, timeout, TLS).
    Transport,
    /// No coord base URL configured — a dev box without a profile.
    NoCoordBase,
    /// `claude_session_id` is not a UUID, so it cannot name a
    /// `coord.agent_sessions` row. Never sent; counted so a store holding a
    /// non-uuid anchor is visible instead of silently skipped.
    AnchorNotUuid,
    /// 2xx whose body carried no usable `handle`.
    MalformedResponse,
}

impl HandleRegisterOutcome {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Minted => "minted",
            Self::Rebound => "rebound",
            Self::Denied => "denied",
            Self::RegistryUnavailable => "registry_unavailable",
            Self::RouteAbsent => "route_absent",
            Self::OtherStatus => "other_status",
            Self::Transport => "transport",
            Self::NoCoordBase => "no_coord_base",
            Self::AnchorNotUuid => "anchor_not_uuid",
            Self::MalformedResponse => "malformed_response",
        }
    }

    pub(crate) const ALL: [Self; 10] = [
        Self::Minted,
        Self::Rebound,
        Self::Denied,
        Self::RegistryUnavailable,
        Self::RouteAbsent,
        Self::OtherStatus,
        Self::Transport,
        Self::NoCoordBase,
        Self::AnchorNotUuid,
        Self::MalformedResponse,
    ];

    /// This outcome's counter slot. An EXHAUSTIVE match, deliberately not a
    /// lookup in [`Self::ALL`]: a search would need a fallback for the
    /// not-found case, and any fallback silently mis-attributes a new variant
    /// to another variant's series — the precise class of quiet wrongness
    /// these counters exist to prevent. This way adding a variant fails to
    /// compile until it is given its own slot.
    const fn idx(self) -> usize {
        match self {
            Self::Minted => 0,
            Self::Rebound => 1,
            Self::Denied => 2,
            Self::RegistryUnavailable => 3,
            Self::RouteAbsent => 4,
            Self::OtherStatus => 5,
            Self::Transport => 6,
            Self::NoCoordBase => 7,
            Self::AnchorNotUuid => 8,
            Self::MalformedResponse => 9,
        }
    }
}

/// Per-outcome counters, in declaration order of
/// [`HandleRegisterOutcome::ALL`]. Mirrors the `selfId` counter idiom in
/// `mcp_api` (fixed cardinality, one `AtomicU64` per series, pure-sync read).
fn outcome_counters() -> &'static [AtomicU64; 10] {
    static COUNTERS: OnceLock<[AtomicU64; 10]> = OnceLock::new();
    COUNTERS.get_or_init(Default::default)
}

fn record_outcome(outcome: HandleRegisterOutcome) {
    outcome_counters()[outcome.idx()].fetch_add(1, Ordering::Relaxed);
}

/// Snapshot of the register-outcome counters for `GET /health`.
pub fn health_snapshot() -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for outcome in HandleRegisterOutcome::ALL {
        obj.insert(
            outcome.label().to_string(),
            serde_json::json!(outcome_counters()[outcome.idx()].load(Ordering::Relaxed)),
        );
    }
    serde_json::Value::Object(obj)
}

/// The `agent_session_id` to present for a session anchored on
/// `claude_session_id` — the single place the id-space rule lives (see the
/// module docs). `None` when the anchor is not a UUID and therefore cannot
/// name a `coord.agent_sessions` row; the skip is COUNTED, never silent.
pub fn agent_session_id_for_anchor(claude_session_id: &str) -> Option<Uuid> {
    match Uuid::parse_str(claude_session_id.trim()) {
        Ok(id) => Some(id),
        Err(_) => {
            record_outcome(HandleRegisterOutcome::AnchorNotUuid);
            debug!(
                claude_session_id,
                "session_handle: anchor is not a UUID — cannot name a coord.agent_sessions row, skipping handle register"
            );
            None
        }
    }
}

/// Classify a register response status into its terminal outcome. Pure so the
/// mapping is unit-testable without a live coord.
pub(crate) fn classify_status(status: u16) -> HandleRegisterOutcome {
    match status {
        403 => HandleRegisterOutcome::Denied,
        503 => HandleRegisterOutcome::RegistryUnavailable,
        404 => HandleRegisterOutcome::RouteAbsent,
        _ => HandleRegisterOutcome::OtherStatus,
    }
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

/// Whether a 2xx register-response body reports a freshly MINTED handle (as
/// opposed to a rebind of an existing row). Absent/non-bool ⇒ `false`, which
/// only ever mis-labels a counter — never a behavioral decision.
pub(crate) fn extract_minted(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("minted").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
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
    let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
        // Coord not configured (dev box without a profile) — silent no-op.
        record_outcome(HandleRegisterOutcome::NoCoordBase);
        return None;
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(REGISTER_TIMEOUT)
        .build()
        .ok()?;
    let url = format!("{base}/coord/session-handles/register");
    // Blocking sibling of the async helper. This function's own doc records the
    // posture — "a missing token sends anonymously and lets coord 401" — which
    // is fail-SOFT, exactly what `attach_device_auth_blocking` provides, off a
    // tokio runtime, from the same default slot this used to read by hand. The
    // gain is that the call finally reaches the coverage counters.
    //
    // `Device`: coord resolves this row's tenant from the registered handle,
    // never from the bearer, so no slot choice can move the row — the default
    // is correct by construction, which is exactly what that variant asserts.
    // Not `Unresolved`: nothing here failed to resolve.
    let r = crate::auth::attach_device_auth_blocking(
        client.post(&url).json(req),
        crate::auth::TenantScope::Device,
    );

    match r.send() {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().unwrap_or_default();
            match extract_handle(&body) {
                Some(handle) => {
                    let minted = extract_minted(&body);
                    record_outcome(if minted {
                        HandleRegisterOutcome::Minted
                    } else {
                        HandleRegisterOutcome::Rebound
                    });
                    info!(
                        claude_session_id = %req.claude_session_id,
                        agent_session_id = %req.agent_session_id,
                        handle = %handle,
                        minted,
                        promptable = req.promptable,
                        "session_handle: acquired/rebound fleet session handle"
                    );
                    Some(handle)
                }
                None => {
                    record_outcome(HandleRegisterOutcome::MalformedResponse);
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
            let status = resp.status();
            let outcome = classify_status(status.as_u16());
            record_outcome(outcome);
            // A 403 is NOT the benign not-deployed-yet case: coord actively
            // refused the binding, so every handle for this device is being
            // dropped. Warn on it (once per session) rather than filing it
            // under the same info line as 404/503, and say what to read.
            if first_failure_for(&req.claude_session_id) {
                if outcome == HandleRegisterOutcome::Denied {
                    warn!(
                        claude_session_id = %req.claude_session_id,
                        agent_session_id = %req.agent_session_id,
                        status = %status,
                        "session_handle: coord REFUSED the handle binding (fail-closed). The presented agent_session_id must be a coord.agent_sessions id bound to this device; see GET /health sessionHandles.denied"
                    );
                } else {
                    info!(
                        claude_session_id = %req.claude_session_id,
                        status = %status,
                        outcome = outcome.label(),
                        "session_handle: coord register route unavailable (best-effort — retried on next FRESH registration, typically next runner restart)"
                    );
                }
            }
            None
        }
        Err(e) => {
            record_outcome(HandleRegisterOutcome::Transport);
            if first_failure_for(&req.claude_session_id) {
                debug!(
                    claude_session_id = %req.claude_session_id,
                    error = %e,
                    "session_handle: coord register call failed (best-effort — retried on next FRESH registration, typically next runner restart)"
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
            promptable: false,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["claude_session_id"], "cs-1");
        assert!(v.get("task_run_id").is_none());
        assert!(v.get("terminal_id").is_none());
        assert!(v.get("name").is_none());
        assert!(v.get("machine_id").is_none());
    }

    #[test]
    fn promptable_is_always_sent_even_when_false() {
        // The §7 resolver's precondition must be re-asserted on EVERY bind:
        // coord's column is `DEFAULT false`, so eliding the field on a rebind
        // would leave a stale `true` standing for a session whose PTY died.
        for promptable in [true, false] {
            let req = HandleRegisterRequest {
                claude_session_id: "cs-1".to_string(),
                agent_session_id: Uuid::nil(),
                task_run_id: None,
                terminal_id: None,
                name: None,
                machine_id: None,
                promptable,
            };
            let v = serde_json::to_value(&req).unwrap();
            assert_eq!(
                v.get("promptable").and_then(serde_json::Value::as_bool),
                Some(promptable),
                "promptable={promptable} must serialize explicitly, never be elided"
            );
        }
    }

    #[test]
    fn agent_session_id_is_the_anchor_not_a_fresh_uuid() {
        // The id-space rule: what we present as `agent_session_id` IS the
        // durable anchor (a `coord.agent_sessions` id), never a per-boot
        // `coord.sessions` uuid. Regression guard for the 403-on-every-bind
        // defect this module's docs describe.
        let anchor = "1f7d9b6e-2c4a-4f19-9d33-8ab5c0e71a42";
        assert_eq!(
            agent_session_id_for_anchor(anchor),
            Some(Uuid::parse_str(anchor).unwrap())
        );
        assert_eq!(
            agent_session_id_for_anchor(&format!("  {anchor}  ")),
            Some(Uuid::parse_str(anchor).unwrap()),
            "surrounding whitespace is tolerated"
        );
    }

    #[test]
    fn non_uuid_anchor_is_skipped_and_counted_not_silent() {
        let before = counter_for(HandleRegisterOutcome::AnchorNotUuid);
        assert!(agent_session_id_for_anchor("not-a-uuid").is_none());
        // STRICTLY-GREATER, not `before + 1`: the counters are process-global
        // statics and `coord_register`'s own non-uuid-anchor test bumps this
        // same series from a parallel test thread, so an exact-delta assert
        // is flaky by construction.
        assert!(
            counter_for(HandleRegisterOutcome::AnchorNotUuid) > before,
            "a skipped register must move a counter — silent skips are what hid the 403s"
        );
    }

    #[test]
    fn status_classification_separates_denied_from_not_deployed() {
        // The distinction the original code collapsed: 403 means coord
        // actively REFUSED the binding (a defect on our side), while 404/503
        // mean the route/table is not there yet (benign, transient).
        assert_eq!(classify_status(403), HandleRegisterOutcome::Denied);
        assert_eq!(
            classify_status(503),
            HandleRegisterOutcome::RegistryUnavailable
        );
        assert_eq!(classify_status(404), HandleRegisterOutcome::RouteAbsent);
        assert_eq!(classify_status(500), HandleRegisterOutcome::OtherStatus);
        assert_eq!(classify_status(401), HandleRegisterOutcome::OtherStatus);
    }

    #[test]
    fn extract_minted_distinguishes_mint_from_rebind() {
        assert!(extract_minted(
            r#"{"handle":"fsh_a","minted":true,"rebound":false}"#
        ));
        assert!(!extract_minted(
            r#"{"handle":"fsh_a","minted":false,"rebound":true}"#
        ));
        // Absent / wrong-typed / unparseable ⇒ false (counter label only).
        assert!(!extract_minted(r#"{"handle":"fsh_a"}"#));
        assert!(!extract_minted(r#"{"minted":"yes"}"#));
        assert!(!extract_minted("not json"));
    }

    #[test]
    fn health_snapshot_exposes_every_outcome_series() {
        let snap = health_snapshot();
        let obj = snap
            .as_object()
            .expect("sessionHandles snapshot must be an object");
        for outcome in HandleRegisterOutcome::ALL {
            assert!(
                obj.contains_key(outcome.label()),
                "GET /health sessionHandles is missing the `{}` series",
                outcome.label()
            );
        }
        assert_eq!(
            obj.len(),
            HandleRegisterOutcome::ALL.len(),
            "snapshot must carry exactly the declared series"
        );
    }

    #[test]
    fn outcome_labels_are_unique() {
        let mut seen = HashSet::new();
        for outcome in HandleRegisterOutcome::ALL {
            assert!(
                seen.insert(outcome.label()),
                "duplicate outcome label `{}` would collapse two series into one",
                outcome.label()
            );
        }
    }

    #[test]
    fn every_outcome_owns_a_distinct_counter_slot() {
        // The `idx()` exhaustive match is what keeps a new variant from
        // silently sharing another's series. Assert the mapping is a
        // bijection onto the counter array.
        let mut seen = HashSet::new();
        for outcome in HandleRegisterOutcome::ALL {
            assert!(
                outcome.idx() < HandleRegisterOutcome::ALL.len(),
                "`{}` indexes outside the counter array",
                outcome.label()
            );
            assert!(
                seen.insert(outcome.idx()),
                "`{}` shares a counter slot with another outcome",
                outcome.label()
            );
        }
    }

    /// Read one outcome counter — test-local so the counters stay private.
    fn counter_for(outcome: HandleRegisterOutcome) -> u64 {
        outcome_counters()[outcome.idx()].load(Ordering::Relaxed)
    }
}
