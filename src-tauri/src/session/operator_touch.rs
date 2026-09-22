//! Operator-touch emission — the shared idempotency-key + payload builder for
//! every Phase B2 trigger (plan
//! `2026-08-27-operator-touch-observation-runner-emitter`).
//!
//! Three producers call [`emit`]: the waiter-thread `on_exit` hooks
//! (`kind = "session_exit"`, [`KIND_SESSION_EXIT`]),
//! [`crate::terminal::operator_touch_watch`] (`kind = "idle_at_prompt"`,
//! [`KIND_IDLE_AT_PROMPT`]) and the `Notification`-hook landing pad
//! (`kind = "permission_prompt"`, [`KIND_PERMISSION_PROMPT`]). All three ride
//! the SAME durable outbox as every other coord-bound session event
//! ([`crate::session::coord_sync`]), via
//! [`crate::session::SessionRegistry::coord_sync`]`().outbox().record(...)`.
//!
//! ## Why this module exists rather than three copies of the same logic
//!
//! coord's write route (`POST /coord/sessions/operator-touch`, plan Phase B1,
//! qontinui-coord#2288) requires a caller-derived `idempotency_key` and
//! REJECTS a malformed one with a typed 422 (no `:`, blank, or over-length).
//! The grammar for a runner-observed touch (no natural coord-minted id) is
//! fixed by the `coordtouch_01` migration's own docstring:
//! `<session_id>:<kind>:<epoch-bucket>`. Deriving that in three places risks
//! three subtly different bucket widths or separators, which would either
//! (a) fail to dedup a genuine repeat (re-creating the ≥1,309-row inflation
//! the plan's §2d exists to collapse) or (b) collide two DIFFERENT touches
//! into one row (under-counting). One function, one grammar.
//!
//! ## The bucket width is 60 seconds — a named constant, not a magic number
//!
//! Per the plan's §2e resolution: both observers of one runner-local event
//! are on ONE device (sub-second clock skew, so 60s clears it by two orders
//! of magnitude), and 60s is short enough that two genuinely distinct touches
//! rarely collapse while long enough that a wedged session re-observing the
//! SAME stall every tick collapses to one row per bucket instead of flooding.
//! It is also the natural pairing for the idle trigger: Claude Code's own
//! `Notification` idle event fires at 60s.
use uuid::Uuid;

use crate::session::{SessionEventKind, SessionRegistry};

/// The runner's three observable `kind`s — the coord-native `question`/`gate`
/// kinds are never emitted here (module header; plan §2e's derivability
/// table — coord mints those ids, the runner cannot).
pub const KIND_PERMISSION_PROMPT: &str = "permission_prompt";
pub const KIND_IDLE_AT_PROMPT: &str = "idle_at_prompt";
pub const KIND_SESSION_EXIT: &str = "session_exit";

/// `source` for every row this runner writes — one of coord's
/// `ACCEPTED_SOURCES`, also DB CHECK-constrained coord-side.
pub const SOURCE_RUNNER_HOOK: &str = "runner_hook";

/// The dedup window (module header). Shared by the emitter and any later
/// reader — do not re-derive this number elsewhere.
pub const BUCKET_WIDTH_SECS: i64 = 60;

/// Floor a moment to its 60-second bucket. Pure so the boundary is
/// unit-testable without a clock.
pub fn epoch_bucket(now_unix_secs: i64) -> i64 {
    now_unix_secs.div_euclid(BUCKET_WIDTH_SECS) * BUCKET_WIDTH_SECS
}

/// Build the caller's idempotency key: `<coord_session_id>:<kind>:<bucket>`.
/// `coord_session_id` is `coord.sessions.id` — the one identifier both a
/// runner-side re-observation (a busy grid-scan tick) and, structurally,
/// any future observer of the SAME session would derive identically, per the
/// migration's "derivable from the event alone" contract.
pub fn idempotency_key(coord_session_id: Uuid, kind: &str, bucket: i64) -> String {
    format!("{coord_session_id}:{kind}:{bucket}")
}

/// Build the `POST /coord/sessions/operator-touch` body. Every field beyond
/// the three required ones (`kind`, `idempotency_key`, `source`) is left
/// OMITTED rather than guessed — coord defaults `reason_code` to
/// `"unclassified"` and `policy_authorized` to `"unknown"` itself, which is
/// this plan's "no agent participation required" constraint stated as code:
/// if nothing downstream ever classifies this touch, it is still recorded
/// honestly as unclassified/unknown rather than the emitter fabricating a
/// guess.
///
/// `claude_code_session_id` is the CLAUDE HARNESS session id
/// (`TerminalSession::pinned_session_id`) — coord resolves it through the
/// same device+tenant-scoped `resolve_target` the sibling session-tool-
/// activity routes use. This is deliberately NOT `coord_session_id`: coord's
/// resolver matches on `coord.sessions.claude_code_session_id`, not on the
/// row's own primary key.
pub fn touch_payload(
    kind: &str,
    coord_session_id: Uuid,
    claude_code_session_id: Option<&str>,
    bucket: i64,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "kind": kind,
        "idempotency_key": idempotency_key(coord_session_id, kind, bucket),
        "source": SOURCE_RUNNER_HOOK,
    });
    if let Some(id) = claude_code_session_id.filter(|s| !s.trim().is_empty()) {
        body["claude_code_session_id"] = serde_json::Value::String(id.to_string());
    }
    body
}

/// Per-machine kill switch (plan §2a4's resolution: ship the whole feature
/// ARMED by default, with a disable rather than an opt-in — the precedent it
/// argues against, `agent-status-report.sh` / `QONTINUI_CONTEXT_HANDOFF`, is
/// two shipped observability surfaces that both default OFF and are
/// therefore dark). Exactly `"0"` disables; absent or any other value is
/// armed. Checked in [`emit`] — the one choke point every producer (the exit
/// hook, the idle watcher, the `Notification` hook landing pad) already goes
/// through — so a future producer cannot forget to check it.
pub fn armed() -> bool {
    std::env::var("QONTINUI_OPERATOR_TOUCH_HOOK")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

/// Enqueue one operator touch into the durable session outbox. Best-effort by
/// construction: an `Err` here means the LOCAL append failed (disk full,
/// poisoned lock) — the caller logs it and carries on, exactly like every
/// other best-effort producer in this crate. A record that DOES get appended
/// is retried by `coord_sync` under the same best-effort posture as
/// `HelperTaskCreated` (bounded attempts, then Ack-dropped) and coord's own
/// route is separately fail-open on every DB error — a failed emit must never
/// block or slow the session that triggered it (plan §2c's design
/// constraints).
///
/// A no-op `Ok(())` when [`armed`] is false — the disabled state is silent by
/// design, matching every other flag-gated producer in this crate.
pub fn emit(
    registry: &SessionRegistry,
    coord_session_id: Uuid,
    kind: &str,
    claude_code_session_id: Option<&str>,
) -> Result<(), String> {
    if !armed() {
        return Ok(());
    }
    let bucket = epoch_bucket(chrono::Utc::now().timestamp());
    let payload = touch_payload(kind, coord_session_id, claude_code_session_id, bucket);
    registry
        .coord_sync()
        .outbox()
        .record(
            registry.machine_id(),
            coord_session_id,
            SessionEventKind::OperatorTouch,
            payload,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Pure predicate: is this a PROVEN non-zero exit? `None` — the waiter thread
/// itself failed to observe a status — is NOT proof of a non-zero exit, the
/// same `unknown`-is-honest discipline `policy_authorized` uses elsewhere in
/// this store. Split out from [`emit_session_exit_if_nonzero`] so the
/// boundary is unit-testable with no registry.
fn is_genuine_nonzero_exit(exit_code: Option<i32>) -> bool {
    matches!(exit_code, Some(code) if code != 0)
}

/// Trigger 4 — non-zero exit (plan §2b/§2c), the shared call every `on_exit`
/// hook site makes. Emits a `session_exit` touch iff the PTY's real exit code
/// (recovered by the §2b `pane_io` fix) is a GENUINE non-zero (see
/// [`is_genuine_nonzero_exit`]). A failure to enqueue is logged and
/// swallowed, never propagated — an on-exit hook must never make session
/// teardown fail.
pub fn emit_session_exit_if_nonzero(
    registry: &SessionRegistry,
    coord_session_id: Uuid,
    claude_code_session_id: Option<&str>,
    exit_code: Option<i32>,
) {
    if !is_genuine_nonzero_exit(exit_code) {
        return;
    }
    let Some(code) = exit_code else {
        return; // Unreachable given the check above; keeps `code` unwrapped below.
    };
    if let Err(e) = emit(
        registry,
        coord_session_id,
        KIND_SESSION_EXIT,
        claude_code_session_id,
    ) {
        tracing::warn!(
            coord_session = %coord_session_id,
            exit_code = code,
            error = %e,
            "operator_touch: session_exit enqueue failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_bucket_floors_to_the_60s_window() {
        assert_eq!(epoch_bucket(0), 0);
        assert_eq!(epoch_bucket(59), 0);
        assert_eq!(epoch_bucket(60), 60);
        assert_eq!(epoch_bucket(119), 60);
        assert_eq!(epoch_bucket(120), 120);
        // A real-ish unix timestamp, hand-verified: 1_726_000_037 / 60 = 28766667r17
        assert_eq!(epoch_bucket(1_726_000_037), 1_726_000_020);
    }

    #[test]
    fn epoch_bucket_never_goes_negative_for_a_nonnegative_input() {
        for t in [0_i64, 1, 60, 3600, 86_400, 1_726_000_037] {
            assert!(epoch_bucket(t) >= 0);
            assert!(epoch_bucket(t) <= t);
        }
    }

    #[test]
    fn idempotency_key_matches_coords_published_grammar() {
        let sid = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let key = idempotency_key(sid, KIND_PERMISSION_PROMPT, 120);
        assert_eq!(
            key,
            "11111111-1111-4111-8111-111111111111:permission_prompt:120"
        );
        // Structural-minimal check coord's own 422 enforces: at least one ':'.
        assert!(key.contains(':'));
    }

    #[test]
    fn idempotency_key_is_stable_within_one_bucket_and_changes_across_buckets() {
        let sid = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
        let a = idempotency_key(sid, KIND_IDLE_AT_PROMPT, epoch_bucket(960));
        let b = idempotency_key(sid, KIND_IDLE_AT_PROMPT, epoch_bucket(1015));
        let c = idempotency_key(sid, KIND_IDLE_AT_PROMPT, epoch_bucket(1020));
        assert_eq!(a, b, "same 60s bucket must collapse to one key");
        assert_ne!(c, a, "the next bucket must be a different key");
    }

    #[test]
    fn genuine_nonzero_exit_predicate_is_honest_about_unknown() {
        assert!(
            !is_genuine_nonzero_exit(None),
            "unknown is not proof of non-zero"
        );
        assert!(
            !is_genuine_nonzero_exit(Some(0)),
            "a clean exit is not a touch"
        );
        assert!(is_genuine_nonzero_exit(Some(1)));
        assert!(is_genuine_nonzero_exit(Some(127)));
        assert!(
            is_genuine_nonzero_exit(Some(-1)),
            "a negative/unusual code is still non-zero"
        );
    }

    #[test]
    fn touch_payload_carries_only_the_required_fields_plus_the_harness_id() {
        let sid = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        let body = touch_payload(KIND_SESSION_EXIT, sid, Some("harness-abc"), 60);
        assert_eq!(body["kind"], KIND_SESSION_EXIT);
        assert_eq!(body["source"], SOURCE_RUNNER_HOOK);
        assert_eq!(
            body["idempotency_key"],
            "33333333-3333-4333-8333-333333333333:session_exit:60"
        );
        assert_eq!(body["claude_code_session_id"], "harness-abc");
        // No reason_code / policy_authorized / resolution — coord defaults
        // them, and the emitter must not fabricate a classification.
        assert!(body.get("reason_code").is_none());
        assert!(body.get("policy_authorized").is_none());
        assert!(body.get("resolution").is_none());
    }

    /// Best-effort rather than exhaustive: mutating `QONTINUI_OPERATOR_TOUCH_HOOK`
    /// itself would race every other test in this process (env is
    /// process-global), so this only pins the default when nothing in this
    /// process has set it — which is the state a fresh `cargo test` process
    /// starts in.
    #[test]
    fn armed_defaults_true_when_the_kill_switch_is_unset() {
        if std::env::var("QONTINUI_OPERATOR_TOUCH_HOOK").is_err() {
            assert!(armed(), "absent kill switch must mean ARMED (plan §2a4)");
        }
    }

    #[test]
    fn touch_payload_omits_a_blank_harness_session_id_rather_than_sending_empty_string() {
        let sid = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();
        let body = touch_payload(KIND_IDLE_AT_PROMPT, sid, Some("   "), 0);
        assert!(body.get("claude_code_session_id").is_none());
        let body_none = touch_payload(KIND_IDLE_AT_PROMPT, sid, None, 0);
        assert!(body_none.get("claude_code_session_id").is_none());
    }
}
