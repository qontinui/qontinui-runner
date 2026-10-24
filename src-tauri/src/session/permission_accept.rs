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
//! The stream-json path ([`crate::claude_session::dispatcher`]) reads the same
//! dial through [`stream_json_denies`], with a HIGHER bar: only an answered
//! `off` from an EXPLICIT row (`resolved_scope` present and not `none`) denies
//! a headless control request (D5). An unanswered dial, an `off` that is
//! coord's no-row default, and an answer with no scope all keep today's allow.
//! The no-row arm is the deploy-order case: until coord's Phase 1 ships, coord
//! answers this unknown domain `off` with `resolved_scope: "none"`, and a
//! runner shipped first must not read that as "deny every headless tool
//! request" — nor may a coord outage start failing headless sessions.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::mcp::fleet_policy_poller::{
    permission_auto_accept_dial, permission_auto_accept_scope, PERMISSION_AUTO_ACCEPT_OFF,
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

/// The pad's decision for one raw hook body, against the dial as it stands
/// now. Lenient: an empty or non-JSON body is [`AcceptVerdict::None`].
pub fn decide(body: &[u8]) -> AcceptVerdict {
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let (answered, level) = permission_auto_accept_dial();
    permission_accept_verdict(answered, &level, &payload)
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
/// today's allow (D5; see the module docs for the deploy-order reason).
pub fn stream_json_denies_from(answered: bool, level: &str, resolved_scope: Option<&str>) -> bool {
    let explicit_row = resolved_scope
        .map(str::trim)
        .is_some_and(|s| !s.is_empty() && s != NO_ROW_SCOPE);
    answered && level == PERMISSION_AUTO_ACCEPT_OFF && explicit_row
}

/// [`stream_json_denies_from`] against the dial as it stands now.
pub fn stream_json_denies() -> bool {
    let (answered, level) = permission_auto_accept_dial();
    stream_json_denies_from(answered, &level, permission_auto_accept_scope().as_deref())
}

/// The `/health` `permissionAutoAccept` block, PURE over its inputs:
/// `{"answered": bool, "level": "on"|"off"|null, "acceptCount": u64}`.
/// `level` is `null` before coord answers and for an unrecognised answer —
/// UNKNOWN is not rendered as a level.
pub fn health_json_from(answered: bool, level: &str, accept_count: u64) -> Value {
    let level = (answered
        && (level == PERMISSION_AUTO_ACCEPT_ON || level == PERMISSION_AUTO_ACCEPT_OFF))
        .then_some(level);
    serde_json::json!({
        "answered": answered,
        "level": level,
        "acceptCount": accept_count,
    })
}

/// The `/health` `permissionAutoAccept` block, live.
pub fn health_json() -> Value {
    let (answered, level) = permission_auto_accept_dial();
    health_json_from(answered, &level, accept_count())
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

    #[test]
    fn decide_on_garbage_bytes_is_none_even_when_on() {
        let pin = pin_permission_auto_accept_for_test();
        pin.set("on");
        let garbage: [&[u8]; 5] = [b"", b"not json", b"{", b"[]", b"null"];
        for body in garbage {
            assert_eq!(decide(body), AcceptVerdict::None, "{body:?}");
        }
        let good = payload("Bash", Some("default")).to_string();
        assert_eq!(decide(good.as_bytes()), AcceptVerdict::Allow);
    }

    /// The live sequence through the SHIPPING apply path: a session spawned
    /// before coord answered gets `allow` on its NEXT prompt once coord answers
    /// `on`, and an on→off flip is `none` on the next prompt after it.
    #[test]
    fn unanswered_then_answered_on_then_flipped_off() {
        let pin = pin_permission_auto_accept_for_test();
        let body = payload("Bash", Some("default")).to_string();

        assert_eq!(decide(body.as_bytes()), AcceptVerdict::None, "cold start");
        // A failed poll answers nothing.
        pin.apply(None);
        assert_eq!(
            decide(body.as_bytes()),
            AcceptVerdict::None,
            "still unanswered"
        );

        pin.apply(Some("on"));
        assert_eq!(decide(body.as_bytes()), AcceptVerdict::Allow, "answered on");
        // A later failure keeps last-good.
        pin.apply(None);
        assert_eq!(decide(body.as_bytes()), AcceptVerdict::Allow, "kept");

        pin.apply(Some("off"));
        assert_eq!(decide(body.as_bytes()), AcceptVerdict::None, "flipped off");
        pin.apply(Some(" ON "));
        assert_eq!(
            decide(body.as_bytes()),
            AcceptVerdict::Allow,
            "the poller normalizes case and whitespace"
        );
        pin.apply(Some("banana"));
        assert_eq!(
            decide(body.as_bytes()),
            AcceptVerdict::None,
            "an unrecognised answer replaces on"
        );
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
        let body = payload("Bash", Some("default")).to_string();
        for scope in [Some("none"), Some("tenant"), None] {
            pin.set_with_scope("on", scope);
            assert_eq!(decide(body.as_bytes()), AcceptVerdict::Allow, "{scope:?}");
            pin.set_with_scope("off", scope);
            assert_eq!(decide(body.as_bytes()), AcceptVerdict::None, "{scope:?}");
        }
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
        let v = health_json_from(false, "", 0);
        assert_eq!(
            v,
            json!({"answered": false, "level": null, "acceptCount": 0})
        );
        let v = health_json_from(true, "on", 7);
        assert_eq!(
            v,
            json!({"answered": true, "level": "on", "acceptCount": 7})
        );
        let v = health_json_from(true, "off", 7);
        assert_eq!(v["level"], "off");
        assert_eq!(
            health_json_from(true, "", 1)["level"],
            Value::Null,
            "an unrecognised answer is not rendered as a level"
        );
        assert_eq!(
            health_json_from(false, "on", 1)["level"],
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
