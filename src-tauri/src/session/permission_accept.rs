//! Permission auto-accept — the ONE decision on whether the runner answers a
//! Claude Code permission dialog itself (plan
//! `2026-10-23-runner-sessions-stop-at-permission-dialogs-so-accept-them-by-default-with-a-tenant-switch`,
//! Phase 2, D1–D5).
//!
//! The carrier's `PermissionRequest` hook (`claude_permission_request_hook.sh`)
//! POSTs each payload to the runner's pad `POST /sessions/{id}/permission-request`
//! ([`crate::mcp::sessions`]). The pad records the capture FIRST
//! ([`crate::session::permission_prompt_capture`]), then asks [`decide`] and
//! answers `{"decision": "allow" | "none"}`. The hook prints Claude Code's allow
//! envelope only on `allow`; on anything else the dialog shows as before.
//!
//! [`permission_accept_verdict`] is the pure core. It answers
//! [`AcceptVerdict::Allow`] ONLY when every one of these holds — each pinned by
//! a test:
//!
//! - the tenant dial `permission_auto_accept` has been ANSWERED by coord in
//!   this process, and its level is exactly `on` (D1);
//! - the payload is a JSON object naming a non-empty `tool_name`;
//! - `hook_event_name`, when present, is `PermissionRequest`;
//! - the tool is not one of [`NEVER_AUTO_ANSWERED_TOOLS`] (`AskUserQuestion`,
//!   `ExitPlanMode` — plan approval; Phase 0 measured that an `allow` answers
//!   neither, so this keeps them out of the accept count too) (D3);
//! - `permission_mode` is present and is not one of
//!   [`RESTRICTIVE_PERMISSION_MODES`] (`plan`, `dontAsk` — an explicit
//!   restrictive posture is never widened silently; the runner's
//!   `PermissionMode::AllowList` renders `dontAsk`) (D3).
//!
//! Everything else is [`AcceptVerdict::None`]: the dialog is left alone. Deny
//! rules and blocking `PreToolUse` guards refuse BEFORE `PermissionRequest` is
//! reached (Phase 0, D4), so nothing here can widen them.
//!
//! The PAD adds three bars on top of that pure verdict ([`decide_with`]):
//!
//! - **Freshness.** An answer older than [`ANSWER_FRESHNESS_SECS`] (no 2xx
//!   since — the device credential went dark and every poll 401s) reads as
//!   UNANSWERED: fail safe to the dialog.
//! - **This runner spawned the terminal.** Only a terminal holding a
//!   [`TerminalGrant`] — minted at spawn by `TerminalSession::spawn`, whose
//!   nonce rides the child env as [`NONCE_ENV`] — can be answered. The `allow`
//!   answer ECHOES that nonce and the hook requires the exact echo, so a
//!   process squatting the loopback port while the runner is down (which never
//!   sees the nonce: the hook does not send it) cannot answer a prompt.
//! - **Same tenant.** The dial is polled under the runner's DEFAULT device
//!   credential, so it is that tenant's dial. A terminal spawned for a
//!   DIFFERENT tenant (an explicit spawn tenant) is answered `none`; a terminal
//!   with no recorded spawn tenant keeps the default-tenant behaviour.
//!
//! The stream-json path ([`crate::claude_session::dispatcher`]) reads the same
//! dial through [`stream_json_denies`], with a HIGHER bar: only an answered
//! `off` from an EXPLICIT row (`resolved_scope` present and not `none`) denies
//! a headless control request (D5). An unanswered dial, an `off` that is
//! coord's no-row default, and an answer with no scope all keep today's allow.
//! The no-row arm is the deploy-order case: until coord's Phase 1 ships, coord
//! answers this unknown domain `off` with `resolved_scope: "none"`, and a
//! runner shipped first must not read that as "deny every headless tool
//! request" — nor may a coord outage start failing headless sessions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::mcp::fleet_policy_poller::{
    permission_auto_accept_dial, PermissionAutoAcceptDial, PERMISSION_AUTO_ACCEPT_OFF,
    PERMISSION_AUTO_ACCEPT_ON,
};

/// The `resolved_scope` coord answers when NO row exists and its domain
/// default applied — the one scope that is not a tenant's choice.
const NO_ROW_SCOPE: &str = "none";

/// Tools whose `PermissionRequest` is never auto-answered (D3).
pub const NEVER_AUTO_ANSWERED_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// `permission_mode` values that are never auto-answered (D3).
pub const RESTRICTIVE_PERMISSION_MODES: [&str; 2] = ["plan", "dontAsk"];

/// The only hook event this decision may answer.
const PERMISSION_REQUEST_EVENT: &str = "PermissionRequest";

/// The deny message a headless (stream-json) control request gets when the
/// tenant has turned auto-accept off (D5).
pub const STREAM_JSON_DENY_MESSAGE: &str = "permission auto-accept is off for this tenant";

/// How old a dial answer may be before the INTERACTIVE verdict reads it as
/// unanswered: 10 minutes, about 13 poll intervals. A 401/404/network failure
/// keeps last-good but does not refresh the answer time, so a credential that
/// goes dark turns an `on` back into the dialog within this bound.
pub const ANSWER_FRESHNESS_SECS: i64 = 600;

/// The child-env variable carrying a terminal's per-spawn nonce. Read by
/// `claude_permission_request_hook.sh`, which NEVER sends it — it only checks
/// the pad's echo.
pub const NONCE_ENV: &str = "QONTINUI_PERMREQ_NONCE";

/// How many terminal grants the registry holds; at the bound the oldest is
/// evicted (that terminal's prompts then fail closed to the dialog).
pub const MAX_TERMINAL_GRANTS: usize = 4096;

/// The verdict for one `PermissionRequest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptVerdict {
    /// The runner answers the dialog: the hook prints Claude Code's allow JSON.
    Allow,
    /// No decision: the hook prints nothing and the dialog shows as before.
    None,
}

impl AcceptVerdict {
    /// The pad's wire word: `"allow"` | `"none"`.
    pub fn as_str(self) -> &'static str {
        match self {
            AcceptVerdict::Allow => "allow",
            AcceptVerdict::None => "none",
        }
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn is_listed(value: &str, list: &[&str]) -> bool {
    list.iter().any(|item| value.eq_ignore_ascii_case(item))
}

/// The decision, PURE. See the module docs for the rule; every arm that is not
/// listed there is [`AcceptVerdict::None`].
///
/// The exclusion lists compare trimmed and ASCII-case-insensitively, so a
/// casing drift in a later CLI widens the exclusion rather than slipping past
/// it.
pub fn permission_accept_verdict(answered: bool, level: &str, payload: &Value) -> AcceptVerdict {
    if !answered || level != PERMISSION_AUTO_ACCEPT_ON {
        return AcceptVerdict::None;
    }
    if !payload.is_object() {
        return AcceptVerdict::None;
    }
    if let Some(event) = payload.get("hook_event_name") {
        if event.as_str() != Some(PERMISSION_REQUEST_EVENT) {
            return AcceptVerdict::None;
        }
    }
    let Some(tool) = str_field(payload, "tool_name")
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return AcceptVerdict::None;
    };
    if is_listed(tool, &NEVER_AUTO_ANSWERED_TOOLS) {
        return AcceptVerdict::None;
    }
    let Some(mode) = str_field(payload, "permission_mode")
        .map(str::trim)
        .filter(|m| !m.is_empty())
    else {
        return AcceptVerdict::None;
    };
    if is_listed(mode, &RESTRICTIVE_PERMISSION_MODES) {
        return AcceptVerdict::None;
    }
    AcceptVerdict::Allow
}

/// Whether the dial holds an answer the INTERACTIVE verdict may act on: coord
/// answered, and the last 2xx is no older than [`ANSWER_FRESHNESS_SECS`].
/// PURE.
pub fn answer_is_fresh(
    dial: &PermissionAutoAcceptDial,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    dial.answered
        && dial.answered_at.is_some_and(|at| {
            now.signed_duration_since(at) <= chrono::Duration::seconds(ANSWER_FRESHNESS_SECS)
        })
}

// ---------------------------------------------------------------------------
// Terminal grants — which terminals this runner process spawned
// ---------------------------------------------------------------------------

/// What the runner recorded about one terminal it spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalGrant {
    /// The per-spawn secret injected as [`NONCE_ENV`] and echoed in an allow.
    pub nonce: String,
    /// The tenant the caller spawned the terminal for, or `None` when none was
    /// named (the default credential's tenant governs, as before).
    pub spawn_tenant: Option<uuid::Uuid>,
}

type GrantMap = HashMap<String, (u64, TerminalGrant)>;

fn grants() -> &'static Mutex<GrantMap> {
    static GRANTS: OnceLock<Mutex<GrantMap>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

static GRANT_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh nonce: 32 lowercase hex characters of a v4 UUID (122 random bits).
/// Hex only, so the hook can match it inside a regex without escaping.
pub fn new_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Record that this process spawned `terminal_id` with `nonce` in its env.
/// Called once the child is up; a later grant for the same id replaces it.
pub fn register_terminal_grant(terminal_id: &str, nonce: String, spawn_tenant: Option<uuid::Uuid>) {
    let seq = GRANT_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut map = grants().lock().unwrap_or_else(|p| p.into_inner());
    if map.len() >= MAX_TERMINAL_GRANTS && !map.contains_key(terminal_id) {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, (s, _))| *s)
            .map(|(id, _)| id.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(
        terminal_id.to_string(),
        (
            seq,
            TerminalGrant {
                nonce,
                spawn_tenant,
            },
        ),
    );
}

/// Forget `terminal_id`'s grant — only when it is still the one minted with
/// `nonce`, so a stale drop can never remove a newer terminal's grant.
pub fn forget_terminal_grant(terminal_id: &str, nonce: &str) {
    let mut map = grants().lock().unwrap_or_else(|p| p.into_inner());
    if map.get(terminal_id).is_some_and(|(_, g)| g.nonce == nonce) {
        map.remove(terminal_id);
    }
}

fn terminal_grant(terminal_id: &str) -> Option<TerminalGrant> {
    grants()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(terminal_id)
        .map(|(_, g)| g.clone())
}

// ---------------------------------------------------------------------------
// The pad's decision
// ---------------------------------------------------------------------------

/// What the pad answers for one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadDecision {
    pub verdict: AcceptVerdict,
    /// The terminal's nonce, echoed ONLY on [`AcceptVerdict::Allow`].
    pub nonce: Option<String>,
    /// Why — a fixed label, never payload content.
    pub reason: &'static str,
}

impl PadDecision {
    fn none(reason: &'static str) -> Self {
        Self {
            verdict: AcceptVerdict::None,
            nonce: None,
            reason,
        }
    }
}

/// The pad's decision, PURE over the dial snapshot, the clock, the terminal's
/// grant and the payload. See the module docs for the three pad bars.
pub fn decide_with(
    dial: &PermissionAutoAcceptDial,
    now: chrono::DateTime<chrono::Utc>,
    grant: Option<&TerminalGrant>,
    payload: &Value,
) -> PadDecision {
    let fresh = answer_is_fresh(dial, now);
    if permission_accept_verdict(fresh, &dial.level, payload) != AcceptVerdict::Allow {
        return PadDecision::none(if dial.answered && !fresh {
            "dial answer is stale"
        } else {
            "verdict none"
        });
    }
    let Some(grant) = grant else {
        return PadDecision::none("terminal was not spawned by this runner process");
    };
    if let Some(spawn_tenant) = grant.spawn_tenant {
        if dial.tenant != Some(spawn_tenant) {
            return PadDecision::none("terminal's spawn tenant is not the dial's tenant");
        }
    }
    PadDecision {
        verdict: AcceptVerdict::Allow,
        nonce: Some(grant.nonce.clone()),
        reason: "allow",
    }
}

/// The pad's decision for one raw hook body on `terminal_id`, against the dial
/// and the grants as they stand now. Lenient: an empty or non-JSON body is
/// [`AcceptVerdict::None`].
pub fn decide(terminal_id: &str, body: &[u8]) -> PadDecision {
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    decide_with(
        &permission_auto_accept_dial(),
        chrono::Utc::now(),
        terminal_grant(terminal_id).as_ref(),
        &payload,
    )
}

/// How many `PermissionRequest`s the pad has answered `allow` since this
/// process started.
static ACCEPT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Count one `allow` answer.
pub fn record_accept() {
    ACCEPT_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// See [`ACCEPT_COUNT`].
pub fn accept_count() -> u64 {
    ACCEPT_COUNT.load(Ordering::Relaxed)
}

/// Whether a headless `can_use_tool` control request is DENIED. PURE.
///
/// Only an answered `off` from an EXPLICIT row denies: `resolved_scope` must be
/// present and not [`NO_ROW_SCOPE`]. Unanswered, `on`, unrecognised, an `off`
/// that is coord's no-row default, and an answer carrying no scope all keep
/// today's allow (D5; see the module docs for the deploy-order reason). Not
/// freshness-gated: an explicit `off` the tenant chose stays in force on
/// last-good, and the unanswered arm already keeps today's allow.
pub fn stream_json_denies_from(answered: bool, level: &str, resolved_scope: Option<&str>) -> bool {
    let explicit_row = resolved_scope
        .map(str::trim)
        .is_some_and(|s| !s.is_empty() && s != NO_ROW_SCOPE);
    answered && level == PERMISSION_AUTO_ACCEPT_OFF && explicit_row
}

/// [`stream_json_denies_from`] against the dial as it stands now.
pub fn stream_json_denies() -> bool {
    let dial = permission_auto_accept_dial();
    stream_json_denies_from(dial.answered, &dial.level, dial.resolved_scope.as_deref())
}

/// The `/health` `permissionAutoAccept` block, PURE over its inputs:
/// `{"answered": bool, "level": "on"|"off"|null, "acceptCount": u64,
///   "answeredAt": RFC 3339 | null, "tenantId": uuid | null}`.
///
/// `answered` is what the PAD acts on — a FRESH answer ([`answer_is_fresh`]) —
/// so a reader deciding "will a prompt here be accepted?" never trusts a stale
/// `on`. `level` is `null` whenever `answered` is false and for an
/// unrecognised answer — UNKNOWN is not rendered as a level. `answeredAt` is
/// the last 2xx (present even once stale, which is how a stale dial reads),
/// `tenantId` the tenant whose credential that answer was asked under.
pub fn health_json_from(
    dial: &PermissionAutoAcceptDial,
    now: chrono::DateTime<chrono::Utc>,
    accept_count: u64,
) -> Value {
    let fresh = answer_is_fresh(dial, now);
    let level = (fresh
        && (dial.level == PERMISSION_AUTO_ACCEPT_ON || dial.level == PERMISSION_AUTO_ACCEPT_OFF))
        .then_some(dial.level.as_str());
    serde_json::json!({
        "answered": fresh,
        "level": level,
        "acceptCount": accept_count,
        "answeredAt": dial.answered_at.map(|t| t.to_rfc3339()),
        "tenantId": dial.tenant.map(|t| t.to_string()),
    })
}

/// The `/health` `permissionAutoAccept` block, live.
pub fn health_json() -> Value {
    health_json_from(
        &permission_auto_accept_dial(),
        chrono::Utc::now(),
        accept_count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::fleet_policy_poller::pin_permission_auto_accept_for_test;
    use serde_json::json;

    /// The Phase 0 (CLI 2.1.309, T1a) payload shape, in manual mode.
    fn payload(tool: &str, mode: Option<&str>) -> Value {
        let mut p = json!({
            "session_id": "11111111-2222-3333-4444-555555555555",
            "transcript_path": "/t/x.jsonl",
            "cwd": "/w/repo",
            "prompt_id": "p1",
            "effort": {"level": "medium"},
            "hook_event_name": "PermissionRequest",
            "tool_name": tool,
            "tool_input": {"command": "rmdir $PROBE_EMPTY_VAR/", "description": "probe"},
            "permission_suggestions": [],
        });
        if let Some(m) = mode {
            p["permission_mode"] = json!(m);
        }
        p
    }

    #[test]
    fn answered_on_allows() {
        for mode in ["default", "bypassPermissions", "acceptEdits"] {
            assert_eq!(
                permission_accept_verdict(true, "on", &payload("Bash", Some(mode))),
                AcceptVerdict::Allow,
                "{mode}"
            );
        }
    }

    #[test]
    fn answered_off_is_none() {
        assert_eq!(
            permission_accept_verdict(true, "off", &payload("Bash", Some("default"))),
            AcceptVerdict::None
        );
    }

    #[test]
    fn unanswered_is_none_whatever_the_level() {
        for level in ["on", "off", ""] {
            assert_eq!(
                permission_accept_verdict(false, level, &payload("Bash", Some("default"))),
                AcceptVerdict::None,
                "{level:?}"
            );
        }
    }

    #[test]
    fn an_unrecognised_level_is_none() {
        for level in [
            "", "ON ", "On", "record", "gate", "observe", "true", "1", "banana",
        ] {
            assert_eq!(
                permission_accept_verdict(true, level, &payload("Bash", Some("default"))),
                AcceptVerdict::None,
                "{level:?} — the poller normalizes; the verdict takes only the exact word"
            );
        }
    }

    #[test]
    fn ask_user_question_and_exit_plan_mode_are_never_answered() {
        for tool in [
            "AskUserQuestion",
            "ExitPlanMode",
            " askuserquestion ",
            "EXITPLANMODE",
        ] {
            assert_eq!(
                permission_accept_verdict(true, "on", &payload(tool, Some("default"))),
                AcceptVerdict::None,
                "{tool:?}"
            );
        }
    }

    #[test]
    fn plan_and_dont_ask_modes_are_never_answered() {
        for mode in ["plan", "dontAsk", "DONTASK", " plan "] {
            assert_eq!(
                permission_accept_verdict(true, "on", &payload("Bash", Some(mode))),
                AcceptVerdict::None,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn an_absent_or_empty_permission_mode_is_none() {
        assert_eq!(
            permission_accept_verdict(true, "on", &payload("Bash", None)),
            AcceptVerdict::None
        );
        assert_eq!(
            permission_accept_verdict(true, "on", &payload("Bash", Some("  "))),
            AcceptVerdict::None
        );
        let mut non_string = payload("Bash", None);
        non_string["permission_mode"] = json!(3);
        assert_eq!(
            permission_accept_verdict(true, "on", &non_string),
            AcceptVerdict::None
        );
    }

    #[test]
    fn a_payload_without_a_tool_name_or_not_an_object_is_none() {
        for p in [
            Value::Null,
            json!([]),
            json!("PermissionRequest"),
            json!({}),
            json!({"permission_mode": "default"}),
            json!({"tool_name": "", "permission_mode": "default"}),
            json!({"tool_name": "  ", "permission_mode": "default"}),
            json!({"tool_name": 7, "permission_mode": "default"}),
        ] {
            assert_eq!(
                permission_accept_verdict(true, "on", &p),
                AcceptVerdict::None,
                "{p}"
            );
        }
    }

    #[test]
    fn only_a_permission_request_event_may_be_answered() {
        let mut p = payload("Bash", Some("default"));
        p["hook_event_name"] = json!("Notification");
        assert_eq!(
            permission_accept_verdict(true, "on", &p),
            AcceptVerdict::None
        );
        p["hook_event_name"] = json!(null);
        assert_eq!(
            permission_accept_verdict(true, "on", &p),
            AcceptVerdict::None,
            "a non-string event is not PermissionRequest"
        );
        // Absent is tolerated: the event name is not the decision's evidence.
        let mut absent = payload("Bash", Some("default"));
        absent.as_object_mut().unwrap().remove("hook_event_name");
        assert_eq!(
            permission_accept_verdict(true, "on", &absent),
            AcceptVerdict::Allow
        );
    }

    /// A fresh, answered dial at `level`, asked under `tenant`.
    fn dial(level: &str, tenant: Option<uuid::Uuid>) -> PermissionAutoAcceptDial {
        PermissionAutoAcceptDial {
            answered: true,
            level: level.to_string(),
            resolved_scope: Some("tenant".to_string()),
            answered_at: Some(chrono::Utc::now()),
            tenant,
        }
    }

    fn grant(nonce: &str, spawn_tenant: Option<uuid::Uuid>) -> TerminalGrant {
        TerminalGrant {
            nonce: nonce.to_string(),
            spawn_tenant,
        }
    }

    #[test]
    fn decide_on_garbage_bytes_is_none_even_when_on() {
        let pin = pin_permission_auto_accept_for_test();
        pin.set("on");
        let term = "t-pa-garbage";
        register_terminal_grant(term, "a1".repeat(16), None);
        let garbage: [&[u8]; 5] = [b"", b"not json", b"{", b"[]", b"null"];
        for body in garbage {
            assert_eq!(decide(term, body).verdict, AcceptVerdict::None, "{body:?}");
        }
        let good = payload("Bash", Some("default")).to_string();
        assert_eq!(decide(term, good.as_bytes()).verdict, AcceptVerdict::Allow);
        forget_terminal_grant(term, &"a1".repeat(16));
    }

    /// The live sequence through the SHIPPING apply path: a session spawned
    /// before coord answered gets `allow` on its NEXT prompt once coord answers
    /// `on`, and an on→off flip is `none` on the next prompt after it.
    #[test]
    fn unanswered_then_answered_on_then_flipped_off() {
        let pin = pin_permission_auto_accept_for_test();
        let term = "t-pa-sequence";
        let nonce = new_nonce();
        register_terminal_grant(term, nonce.clone(), None);
        let body = payload("Bash", Some("default")).to_string();
        let verdict = || decide(term, body.as_bytes()).verdict;

        assert_eq!(verdict(), AcceptVerdict::None, "cold start");
        // A failed poll answers nothing.
        pin.apply(None);
        assert_eq!(verdict(), AcceptVerdict::None, "still unanswered");

        pin.apply(Some("on"));
        assert_eq!(verdict(), AcceptVerdict::Allow, "answered on");
        // A later failure keeps last-good.
        pin.apply(None);
        assert_eq!(verdict(), AcceptVerdict::Allow, "kept");

        pin.apply(Some("off"));
        assert_eq!(verdict(), AcceptVerdict::None, "flipped off");
        pin.apply(Some(" ON "));
        assert_eq!(
            verdict(),
            AcceptVerdict::Allow,
            "the poller normalizes case and whitespace"
        );
        pin.apply(Some("banana"));
        assert_eq!(
            verdict(),
            AcceptVerdict::None,
            "an unrecognised answer replaces on"
        );
        forget_terminal_grant(term, &nonce);
    }

    /// RISK 4: an answer that has not been refreshed within the bound (every
    /// poll since has failed — e.g. the credential went dark) is UNANSWERED
    /// for the interactive verdict.
    #[test]
    fn a_stale_answer_is_unanswered_for_the_interactive_verdict() {
        let now = chrono::Utc::now();
        let p = payload("Bash", Some("default"));
        let g = grant("n1", None);
        let at = |ago_secs: i64| PermissionAutoAcceptDial {
            answered_at: Some(now - chrono::Duration::seconds(ago_secs)),
            ..dial("on", None)
        };
        let within = decide_with(&at(ANSWER_FRESHNESS_SECS - 30), now, Some(&g), &p);
        assert_eq!(within.verdict, AcceptVerdict::Allow);
        let edge = decide_with(&at(ANSWER_FRESHNESS_SECS), now, Some(&g), &p);
        assert_eq!(edge.verdict, AcceptVerdict::Allow, "the bound is inclusive");
        let stale = decide_with(&at(ANSWER_FRESHNESS_SECS + 1), now, Some(&g), &p);
        assert_eq!(stale.verdict, AcceptVerdict::None);
        assert_eq!(stale.reason, "dial answer is stale");
        assert_eq!(stale.nonce, None);
        // An answered dial with no answer time at all is not fresh either.
        let timeless = PermissionAutoAcceptDial {
            answered_at: None,
            ..dial("on", None)
        };
        assert!(!answer_is_fresh(&timeless, now));
        // /health says so: a stale dial reads unanswered, its time still shown.
        let h = health_json_from(&at(ANSWER_FRESHNESS_SECS + 1), now, 0);
        assert_eq!(h["answered"], false);
        assert_eq!(h["level"], Value::Null);
        assert!(h["answeredAt"].is_string(), "{h}");
    }

    /// RISK 3: the dial is the DEFAULT credential's tenant's dial. A terminal
    /// spawned for another tenant is answered `none`; one with no recorded
    /// spawn tenant keeps the default-tenant behaviour.
    #[test]
    fn a_terminal_spawned_for_another_tenant_gets_none() {
        let now = chrono::Utc::now();
        let p = payload("Bash", Some("default"));
        let (a, b) = (uuid::Uuid::from_u128(0xA1), uuid::Uuid::from_u128(0xB2));

        let same = decide_with(&dial("on", Some(a)), now, Some(&grant("n", Some(a))), &p);
        assert_eq!(same.verdict, AcceptVerdict::Allow, "same tenant");
        let other = decide_with(&dial("on", Some(a)), now, Some(&grant("n", Some(b))), &p);
        assert_eq!(other.verdict, AcceptVerdict::None, "another tenant");
        assert_eq!(
            other.reason,
            "terminal's spawn tenant is not the dial's tenant"
        );
        let unknown = decide_with(&dial("on", None), now, Some(&grant("n", Some(b))), &p);
        assert_eq!(
            unknown.verdict,
            AcceptVerdict::None,
            "a dial whose tenant is unknown cannot be shown to be B's"
        );
        for dial_tenant in [Some(a), None] {
            let none_recorded =
                decide_with(&dial("on", dial_tenant), now, Some(&grant("n", None)), &p);
            assert_eq!(
                none_recorded.verdict,
                AcceptVerdict::Allow,
                "no recorded spawn tenant keeps today's behaviour ({dial_tenant:?})"
            );
        }
    }

    /// RISK 5: only a terminal this process spawned can be answered, and the
    /// allow carries exactly that terminal's nonce.
    #[test]
    fn an_allow_echoes_the_terminals_nonce_and_an_unknown_terminal_gets_none() {
        let now = chrono::Utc::now();
        let p = payload("Bash", Some("default"));
        let known = decide_with(&dial("on", None), now, Some(&grant("feedface", None)), &p);
        assert_eq!(known.verdict, AcceptVerdict::Allow);
        assert_eq!(known.nonce.as_deref(), Some("feedface"));
        let unknown = decide_with(&dial("on", None), now, None, &p);
        assert_eq!(unknown.verdict, AcceptVerdict::None);
        assert_eq!(unknown.nonce, None);
        let off = decide_with(&dial("off", None), now, Some(&grant("feedface", None)), &p);
        assert_eq!(off.nonce, None, "a none never carries the nonce");
    }

    #[test]
    fn the_grant_registry_forgets_only_the_matching_nonce_and_is_bounded() {
        let term = "t-pa-registry";
        register_terminal_grant(term, "old".to_string(), None);
        register_terminal_grant(term, "new".to_string(), None);
        forget_terminal_grant(term, "old");
        assert_eq!(
            terminal_grant(term).map(|g| g.nonce).as_deref(),
            Some("new"),
            "a stale forget cannot remove a newer grant"
        );
        forget_terminal_grant(term, "new");
        assert_eq!(terminal_grant(term), None);
        let n = new_nonce();
        assert_eq!(n.len(), 32);
        assert!(n
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(new_nonce(), n);
    }

    #[test]
    fn stream_json_denies_only_on_an_answered_off_from_an_explicit_row() {
        for scope in ["tenant", "repo", "fleet", " tenant "] {
            assert!(
                stream_json_denies_from(true, "off", Some(scope)),
                "{scope:?}"
            );
        }
        // TODAY's coord answers this domain `off` from no row: still allowed.
        assert!(
            !stream_json_denies_from(true, "off", Some("none")),
            "an off from the no-row default keeps allow"
        );
        assert!(
            !stream_json_denies_from(true, "off", None),
            "an off with no scope cannot be shown to be a tenant's choice"
        );
        assert!(!stream_json_denies_from(true, "off", Some("")));
        assert!(
            !stream_json_denies_from(false, "off", Some("tenant")),
            "unanswered keeps allow"
        );
        assert!(!stream_json_denies_from(true, "on", Some("tenant")));
        assert!(
            !stream_json_denies_from(true, "", Some("tenant")),
            "unrecognised keeps allow"
        );
    }

    /// The interactive verdict is NOT scope-gated: an answered `on` allows
    /// from the no-row default too (after coord's Phase 1 that IS the default).
    #[test]
    fn the_interactive_verdict_allows_an_on_from_any_scope() {
        let pin = pin_permission_auto_accept_for_test();
        let term = "t-pa-scope";
        let nonce = new_nonce();
        register_terminal_grant(term, nonce.clone(), None);
        let body = payload("Bash", Some("default")).to_string();
        for scope in [Some("none"), Some("tenant"), None] {
            pin.set_with_scope("on", scope);
            assert_eq!(
                decide(term, body.as_bytes()).verdict,
                AcceptVerdict::Allow,
                "{scope:?}"
            );
            pin.set_with_scope("off", scope);
            assert_eq!(
                decide(term, body.as_bytes()).verdict,
                AcceptVerdict::None,
                "{scope:?}"
            );
        }
        forget_terminal_grant(term, &nonce);
    }

    /// The fixed contract: `/health` serves this block as
    /// `data.permissionAutoAccept` — inside the `data` object the handler
    /// wraps, which is what `qontinui-claude-config` reads.
    #[test]
    fn health_serves_the_block_inside_data() {
        let src = include_str!("../mcp_api.rs");
        let (_, from_data) = src
            .split_once("let mut data = serde_json::json!({")
            .expect("the /health data object");
        let (data_block, _) = from_data
            .split_once("\n    });\n")
            .expect("the /health data object closes");
        assert!(
            data_block.contains(
                "\"permissionAutoAccept\": crate::session::permission_accept::health_json(),"
            ),
            "permissionAutoAccept must be a key of /health's `data` object"
        );
        let wrapped = src
            .split_once("let mut data = serde_json::json!({")
            .unwrap()
            .1;
        assert!(
            wrapped.contains("\"data\": data,"),
            "the data object is served as `data`"
        );
    }

    #[test]
    fn the_health_block_has_the_fixed_shape() {
        let now = chrono::Utc::now();
        let cold = PermissionAutoAcceptDial::default();
        assert_eq!(
            health_json_from(&cold, now, 0),
            json!({
                "answered": false,
                "level": null,
                "acceptCount": 0,
                "answeredAt": null,
                "tenantId": null,
            })
        );
        let a = uuid::Uuid::from_u128(0xA1);
        let on = PermissionAutoAcceptDial {
            answered_at: Some(now),
            ..dial("on", Some(a))
        };
        assert_eq!(
            health_json_from(&on, now, 7),
            json!({
                "answered": true,
                "level": "on",
                "acceptCount": 7,
                "answeredAt": now.to_rfc3339(),
                "tenantId": a.to_string(),
            })
        );
        assert_eq!(health_json_from(&dial("off", None), now, 7)["level"], "off");
        assert_eq!(
            health_json_from(&dial("", None), now, 1)["level"],
            Value::Null,
            "an unrecognised answer is not rendered as a level"
        );
        let unanswered = PermissionAutoAcceptDial {
            answered: false,
            ..dial("on", None)
        };
        assert_eq!(
            health_json_from(&unanswered, now, 1)["level"],
            Value::Null,
            "an unanswered cache is not rendered as a level"
        );
    }

    #[test]
    fn the_accept_counter_only_climbs() {
        // Held so a pad test asserting an exact count under an `off` dial
        // cannot see this increment.
        let _pin = pin_permission_auto_accept_for_test();
        let before = accept_count();
        record_accept();
        assert!(accept_count() > before);
    }

    #[test]
    fn the_wire_words_are_the_hook_contract() {
        assert_eq!(AcceptVerdict::Allow.as_str(), "allow");
        assert_eq!(AcceptVerdict::None.as_str(), "none");
        assert_eq!(
            crate::mcp::fleet_policy_poller::PERMISSION_AUTO_ACCEPT_DOMAIN,
            "permission_auto_accept"
        );
    }
}
