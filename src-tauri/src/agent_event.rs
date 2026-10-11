//! Agent events — the pure half of the Claude Code hook ingest (plan
//! `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
//! Phases 3 and 4).
//!
//! The runner's `--settings` carrier registers `type: "http"` hooks that POST
//! the CLI's hook payload, UNPROJECTED, to `POST /terminals/agent-event` on the
//! runner's loopback API (D2 re-decided after Phase 1: no relay binary). That
//! payload carries free text the runner must never keep — `prompt`,
//! `tool_input`, `message`, `last_assistant_message` — so the route calls
//! [`project`] the moment the body parses and drops the parsed value. Nothing
//! outside [`PROJECTED_FIELDS`] survives the call, and every survivor is
//! re-validated as a bounded identifier.
//!
//! This module is in the lib crate (no IO, no clock) so the golden fixtures in
//! `tests/claude_event_fixtures.rs` can drive the SAME projection the route
//! runs, and so the [`hook_delivery`] decision is table-testable.
//!
//! ## Untrusted input
//!
//! Any local process can POST to loopback. Posture, mirroring
//! `terminal::agent_status_sideband`:
//! - the body is capped at [`MAX_BODY_BYTES`] and dropped WHOLE when larger;
//! - only allowlisted keys are read; each is trimmed, charset-checked and
//!   capped at [`MAX_FIELD_CHARS`] characters, and a field that fails is
//!   dropped without dropping its siblings;
//! - an unknown `hook_event_name` drops the whole event;
//! - nothing here panics, for any input.

use serde::Serialize;
use serde_json::Value;

use crate::agent_truth::{
    EndReason, FailureKind, HookEvent, NotificationType, PermissionTool, SessionStartSource,
};

/// Largest request body the ingest route will parse. Large enough for a real
/// `PermissionRequest` whose `tool_input` carries a whole file write; a larger
/// body is dropped whole and counted — it loses one event, never corrupts state.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Cap, in characters, on every projected string field.
pub const MAX_FIELD_CHARS: usize = 128;

/// The ONLY keys the projection reads. `error_type` is the documented name of
/// the `StopFailure` field; CLI 2.1.285 sends it as `error` (PROBE.md Q4), so
/// `error` is read first and `error_type` is the fallback. `agent_id` is read
/// for PRESENCE only (a subagent event), never its value.
pub const PROJECTED_FIELDS: [&str; 10] = [
    "hook_event_name",
    "session_id",
    "notification_type",
    "error",
    "error_type",
    "tool_name",
    "permission_mode",
    "reason",
    "source",
    "agent_id",
];

/// Every hook event the carrier registers as an http hook — and therefore the
/// only event names the ingest accepts.
pub const INGESTED_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// CLI versions a Phase 1 probe recorded fixtures for
/// (`tests/fixtures/claude-events/<version>/`), oldest first. A CLI version
/// not listed is [`HookDelivery::VersionMismatch`]: its payloads were never
/// measured, so the projection is trusted less. Pinned against the fixture
/// directory by `tests/claude_event_fixtures.rs`, so recording a new version
/// without listing it here fails a test.
pub const KNOWN_FIXTURE_CLI_VERSIONS: &[&str] = &["2.1.285"];

/// The newest recorded fixture version.
pub fn newest_fixture_version() -> &'static str {
    KNOWN_FIXTURE_CLI_VERSIONS.last().copied().unwrap_or("none")
}

/// A hook payload reduced to the allowlist. Every string is a validated,
/// bounded identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEventProjection {
    /// One of [`INGESTED_EVENTS`].
    pub hook_event_name: &'static str,
    pub session_id: Option<String>,
    pub notification_type: Option<String>,
    /// `error`, else `error_type`.
    pub error: Option<String>,
    pub tool_name: Option<String>,
    pub permission_mode: Option<String>,
    pub reason: Option<String>,
    pub source: Option<String>,
    /// The payload carried a non-null `agent_id` (a subagent's event).
    pub is_subagent: bool,
}

/// Why a body did not project. Each is a counter on the ingest route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionError {
    /// Larger than [`MAX_BODY_BYTES`].
    Oversize,
    /// Not JSON.
    NotJson,
    /// JSON, but not an object.
    NotObject,
    /// No usable `hook_event_name`.
    NoEventName,
    /// A `hook_event_name` outside [`INGESTED_EVENTS`].
    UnknownEvent,
}

impl ProjectionError {
    pub const fn as_str(self) -> &'static str {
        match self {
            ProjectionError::Oversize => "oversize",
            ProjectionError::NotJson => "not_json",
            ProjectionError::NotObject => "not_object",
            ProjectionError::NoEventName => "no_event_name",
            ProjectionError::UnknownEvent => "unknown_event",
        }
    }
}

/// Is `c` allowed in a projected identifier? Covers every value the CLI sends
/// in these fields (`session_id` uuids, `permission_prompt`, `bypassPermissions`,
/// `model_not_found`, `mcp__server__tool`, …) and nothing that could smuggle
/// prose: no whitespace, no quotes, no path separators.
fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

/// Read `key` as a bounded identifier: trimmed, non-empty, at most
/// [`MAX_FIELD_CHARS`] characters, identifier charset only. An over-long or
/// ill-formed value is DROPPED, never truncated — a truncated identifier is a
/// different identifier.
fn bounded_ident(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let raw = obj.get(key)?.as_str()?.trim();
    if raw.is_empty() || raw.chars().count() > MAX_FIELD_CHARS || !raw.chars().all(ident_char) {
        return None;
    }
    Some(raw.to_string())
}

/// Project an already-parsed payload onto the allowlist.
pub fn project(value: &Value) -> Result<AgentEventProjection, ProjectionError> {
    let obj = value.as_object().ok_or(ProjectionError::NotObject)?;
    let name = bounded_ident(obj, "hook_event_name").ok_or(ProjectionError::NoEventName)?;
    let hook_event_name = INGESTED_EVENTS
        .iter()
        .copied()
        .find(|known| *known == name)
        .ok_or(ProjectionError::UnknownEvent)?;
    Ok(AgentEventProjection {
        hook_event_name,
        session_id: bounded_ident(obj, "session_id"),
        notification_type: bounded_ident(obj, "notification_type"),
        error: bounded_ident(obj, "error").or_else(|| bounded_ident(obj, "error_type")),
        tool_name: bounded_ident(obj, "tool_name"),
        permission_mode: bounded_ident(obj, "permission_mode"),
        reason: bounded_ident(obj, "reason"),
        source: bounded_ident(obj, "source"),
        is_subagent: obj.get("agent_id").is_some_and(|v| !v.is_null()),
    })
}

/// Size-check, parse and project a raw request body. The parsed `Value` —
/// which holds the prompt, tool input and messages — is dropped before this
/// returns.
pub fn project_bytes(body: &[u8]) -> Result<AgentEventProjection, ProjectionError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(ProjectionError::Oversize);
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| ProjectionError::NotJson)?;
    project(&value)
}

impl AgentEventProjection {
    /// The typed reducer event. Total over [`INGESTED_EVENTS`].
    pub fn to_hook_event(&self) -> HookEvent {
        match self.hook_event_name {
            "UserPromptSubmit" => HookEvent::UserPromptSubmit,
            "PermissionRequest" => HookEvent::PermissionRequest {
                tool: PermissionTool::from_tool_name(self.tool_name.as_deref()),
            },
            "Notification" => HookEvent::Notification {
                notification_type: NotificationType::from_wire(
                    self.notification_type.as_deref().unwrap_or(""),
                ),
            },
            "Stop" => HookEvent::Stop,
            "StopFailure" => HookEvent::StopFailure {
                kind: FailureKind::from_error_type(self.error.as_deref().unwrap_or("unknown")),
            },
            "SessionStart" => HookEvent::SessionStart {
                source: SessionStartSource::from_wire(self.source.as_deref().unwrap_or("")),
            },
            // "SessionEnd" — the only remaining ingested name.
            _ => HookEvent::SessionEnd {
                reason: EndReason::from_wire(self.reason.as_deref().unwrap_or("")),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 4 — hook delivery
// ---------------------------------------------------------------------------

/// A runner submit followed by this much silence from `UserPromptSubmit` —
/// in a terminal whose hooks have never been heard from — reads as
/// [`HookDelivery::Absent`].
pub const ABSENT_AFTER_SUBMIT_MS: u64 = 10_000;

/// What the carrier on disk says about the agent-event hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarrierEvidence {
    /// The carrier was materialized WITH the http agent-event entries.
    WithEvents,
    /// The carrier was materialized, but the runner API port did not resolve,
    /// so the entries were omitted (the `-noevents` carrier name).
    WithoutEvents,
    /// No carrier was materialized in this process.
    NotMaterialized,
}

/// Everything [`hook_delivery`] weighs. Times are unix millis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryEvidence {
    pub carrier: Option<CarrierEvidence>,
    /// The identity shim's beacon for this terminal said whether it appended
    /// `--settings`.
    pub beacon_settings_delivered: Option<bool>,
    /// The first agent event that arrived for this terminal.
    pub first_event_at_ms: Option<u64>,
    /// The latest agent event of any kind.
    pub last_event_at_ms: Option<u64>,
    /// The latest `UserPromptSubmit`.
    pub last_user_prompt_submit_at_ms: Option<u64>,
    /// The latest prompt a runner producer SUBMITTED into the pane
    /// (`PtyInputSlots.last_submit`).
    pub last_runner_submit_at_ms: Option<u64>,
    /// `Some("disableAllHooks in user settings")`-style finding from a
    /// read-only inspection of the user / project / managed settings.
    pub shadowed_by: Option<String>,
    /// The installed CLI version, when known.
    pub cli_version: Option<String>,
}

/// Is this hook delivery trustworthy, and if not, why? Serialized as
/// `{ "status": "installed" | "shadowed" | "version_mismatch" | "absent" |
/// "unknown", "detail"?: string }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookDelivery {
    /// An agent event actually arrived from this terminal's CLI.
    Installed,
    /// A settings file disables the runner's hooks.
    Shadowed { by: String },
    /// The CLI version has no recorded fixture: hooks may work, but their
    /// payloads were never measured.
    VersionMismatch { cli: String, newest_fixture: String },
    /// The hooks are known not to be reaching the runner.
    Absent { reason: String },
    /// Not proven either way; `evidence` names the weaker evidence held.
    Unknown { evidence: Option<String> },
}

impl HookDelivery {
    pub const fn status(&self) -> &'static str {
        match self {
            HookDelivery::Installed => "installed",
            HookDelivery::Shadowed { .. } => "shadowed",
            HookDelivery::VersionMismatch { .. } => "version_mismatch",
            HookDelivery::Absent { .. } => "absent",
            HookDelivery::Unknown { .. } => "unknown",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            HookDelivery::Installed => None,
            HookDelivery::Shadowed { by } => Some(by.clone()),
            HookDelivery::VersionMismatch {
                cli,
                newest_fixture,
            } => Some(format!(
                "CLI {cli} has no recorded event fixture (newest fixture: {newest_fixture})"
            )),
            HookDelivery::Absent { reason } => Some(reason.clone()),
            HookDelivery::Unknown { evidence } => evidence.clone(),
        }
    }
}

impl Serialize for HookDelivery {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire {
            status: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            detail: Option<String>,
        }
        Wire {
            status: self.status(),
            detail: self.detail(),
        }
        .serialize(serializer)
    }
}

/// Is `cli` a version the fixtures cover? A leading `v` and anything after the
/// first whitespace (`2.1.285 (Claude Code)`) are ignored.
pub fn cli_version_has_fixture(cli: &str) -> bool {
    let bare = cli
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('v');
    KNOWN_FIXTURE_CLI_VERSIONS.contains(&bare)
}

/// Decide [`HookDelivery`] from the evidence, strongest first:
///
/// 1. an event ARRIVED ⇒ `Installed` (or `VersionMismatch` when the CLI is
///    known and unrecorded — the events flow, but their shape is unmeasured);
/// 2. a settings file disables hooks ⇒ `Shadowed`;
/// 3. a runner submit met [`ABSENT_AFTER_SUBMIT_MS`] of silence ⇒
///    `Absent{observed_silent}` (only while no event has EVER arrived: a
///    submit into a busy agent is queued, and a terminal whose hooks were
///    already heard from is not proven silent by one quiet submit);
/// 4. the carrier omitted the entries, or none was materialized ⇒ `Absent`;
/// 5. the CLI is known and unrecorded ⇒ `VersionMismatch`;
/// 6. otherwise `Unknown`, naming the weaker evidence (carrier, beacon).
pub fn hook_delivery(e: &DeliveryEvidence, now_ms: u64) -> HookDelivery {
    let mismatch = e
        .cli_version
        .as_deref()
        .filter(|cli| !cli_version_has_fixture(cli))
        .map(|cli| HookDelivery::VersionMismatch {
            cli: cli.to_string(),
            newest_fixture: newest_fixture_version().to_string(),
        });

    if e.first_event_at_ms.is_some() {
        return mismatch.unwrap_or(HookDelivery::Installed);
    }
    if let Some(by) = &e.shadowed_by {
        return HookDelivery::Shadowed { by: by.clone() };
    }
    if let Some(submit) = e.last_runner_submit_at_ms {
        let answered = e.last_user_prompt_submit_at_ms.is_some_and(|t| t >= submit);
        if !answered && now_ms.saturating_sub(submit) >= ABSENT_AFTER_SUBMIT_MS {
            return HookDelivery::Absent {
                reason: format!(
                    "observed_silent: a prompt was submitted and no UserPromptSubmit arrived \
                     within {}s",
                    ABSENT_AFTER_SUBMIT_MS / 1000
                ),
            };
        }
    }
    match e.carrier {
        Some(CarrierEvidence::WithoutEvents) => {
            return HookDelivery::Absent {
                reason: "carrier_without_agent_events: the runner API port did not resolve \
                         when the carrier was written"
                    .to_string(),
            }
        }
        Some(CarrierEvidence::NotMaterialized) => {
            return HookDelivery::Absent {
                reason: "carrier_not_materialized: no --settings carrier was written".to_string(),
            }
        }
        Some(CarrierEvidence::WithEvents) | None => {}
    }
    if let Some(m) = mismatch {
        return m;
    }
    if e.beacon_settings_delivered == Some(false) {
        return HookDelivery::Unknown {
            evidence: Some(
                "the identity shim ran without appending --settings; no agent event yet"
                    .to_string(),
            ),
        };
    }
    let mut held = Vec::new();
    if e.carrier == Some(CarrierEvidence::WithEvents) {
        held.push("carrier materialized with agent-event hooks");
    }
    if e.beacon_settings_delivered == Some(true) {
        held.push("shim beacon reported --settings delivered");
    }
    HookDelivery::Unknown {
        evidence: (!held.is_empty()).then(|| format!("{}; no agent event yet", held.join("; "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn projected(v: Value) -> AgentEventProjection {
        project(&v).expect("projects")
    }

    #[test]
    fn agent_event_projection_keeps_only_the_allowlist() {
        let p = projected(json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "3f1c2d4e-0000-4000-8000-000000000001",
            "tool_name": "Bash",
            "permission_mode": "default",
            "tool_input": { "command": "rm -rf / # secret" },
            "prompt": "secret prompt",
            "message": "secret",
            "last_assistant_message": "secret",
            "cwd": "/home/x",
        }));
        assert_eq!(p.hook_event_name, "PermissionRequest");
        assert_eq!(p.tool_name.as_deref(), Some("Bash"));
        assert_eq!(p.permission_mode.as_deref(), Some("default"));
        assert!(!p.is_subagent);
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("secret"), "no free text survives: {dbg}");
        assert!(!dbg.contains("/home/x"));
    }

    #[test]
    fn agent_event_forbidden_or_malformed_field_values_are_dropped() {
        let p = projected(json!({
            "hook_event_name": "Notification",
            "notification_type": "permission prompt with spaces",
            "session_id": "x".repeat(MAX_FIELD_CHARS + 1),
            "tool_name": 42,
            "reason": "../../etc/passwd",
        }));
        assert_eq!(p.notification_type, None);
        assert_eq!(p.session_id, None);
        assert_eq!(p.tool_name, None);
        assert_eq!(p.reason, None);
    }

    #[test]
    fn agent_event_stop_failure_reads_error_then_error_type() {
        let a = projected(
            json!({"hook_event_name": "StopFailure", "error": "rate_limit", "error_type": "overloaded"}),
        );
        assert_eq!(a.error.as_deref(), Some("rate_limit"));
        let b = projected(json!({"hook_event_name": "StopFailure", "error_type": "overloaded"}));
        assert_eq!(b.error.as_deref(), Some("overloaded"));
        assert_eq!(
            b.to_hook_event(),
            HookEvent::StopFailure {
                kind: FailureKind::Overloaded
            }
        );
        let c = projected(json!({"hook_event_name": "StopFailure"}));
        assert_eq!(
            c.to_hook_event(),
            HookEvent::StopFailure {
                kind: FailureKind::Unknown
            }
        );
    }

    #[test]
    fn agent_event_unknown_and_malformed_bodies_are_typed_drops() {
        assert_eq!(
            project(&json!({"hook_event_name": "PreToolUse"})),
            Err(ProjectionError::UnknownEvent)
        );
        assert_eq!(
            project(&json!({"hook_event_name": "SubagentStop"})),
            Err(ProjectionError::UnknownEvent)
        );
        assert_eq!(project(&json!({})), Err(ProjectionError::NoEventName));
        assert_eq!(project(&json!([1])), Err(ProjectionError::NotObject));
        assert_eq!(project_bytes(b"{nope"), Err(ProjectionError::NotJson));
        let big = vec![b' '; MAX_BODY_BYTES + 1];
        assert_eq!(project_bytes(&big), Err(ProjectionError::Oversize));
    }

    #[test]
    fn agent_event_subagent_is_presence_of_agent_id() {
        let p = projected(json!({"hook_event_name": "Stop", "agent_id": "a1"}));
        assert!(p.is_subagent);
        let q = projected(json!({"hook_event_name": "Stop", "agent_id": null}));
        assert!(!q.is_subagent);
    }

    #[test]
    fn agent_event_every_ingested_event_maps_to_its_hook_event() {
        for name in INGESTED_EVENTS {
            let p = projected(json!({"hook_event_name": name}));
            let ev = p.to_hook_event();
            let ok = match name {
                "UserPromptSubmit" => ev == HookEvent::UserPromptSubmit,
                "PermissionRequest" => matches!(ev, HookEvent::PermissionRequest { .. }),
                "Notification" => matches!(ev, HookEvent::Notification { .. }),
                "Stop" => ev == HookEvent::Stop,
                "StopFailure" => matches!(ev, HookEvent::StopFailure { .. }),
                "SessionStart" => matches!(ev, HookEvent::SessionStart { .. }),
                "SessionEnd" => matches!(ev, HookEvent::SessionEnd { .. }),
                _ => false,
            };
            assert!(ok, "{name} -> {ev:?}");
        }
    }

    // ---- hook delivery ----------------------------------------------------

    const NOW: u64 = 10_000_000;

    #[test]
    fn hook_delivery_an_arrived_event_is_installed() {
        let e = DeliveryEvidence {
            first_event_at_ms: Some(NOW - 5),
            shadowed_by: Some("disableAllHooks in user settings".into()),
            cli_version: Some("2.1.285 (Claude Code)".into()),
            ..Default::default()
        };
        assert_eq!(hook_delivery(&e, NOW), HookDelivery::Installed);
    }

    #[test]
    fn hook_delivery_unrecorded_cli_is_version_mismatch() {
        let e = DeliveryEvidence {
            first_event_at_ms: Some(NOW - 5),
            cli_version: Some("2.9.0".into()),
            ..Default::default()
        };
        assert_eq!(
            hook_delivery(&e, NOW),
            HookDelivery::VersionMismatch {
                cli: "2.9.0".into(),
                newest_fixture: newest_fixture_version().into()
            }
        );
        let wire = serde_json::to_value(hook_delivery(&e, NOW)).unwrap();
        assert_eq!(wire["status"], "version_mismatch");
        assert!(wire["detail"].as_str().unwrap().contains("2.9.0"));
    }

    #[test]
    fn hook_delivery_shadowed_before_silence() {
        let e = DeliveryEvidence {
            shadowed_by: Some("allowManagedHooksOnly in managed settings".into()),
            last_runner_submit_at_ms: Some(NOW - 60_000),
            ..Default::default()
        };
        let d = hook_delivery(&e, NOW);
        assert_eq!(d.status(), "shadowed");
        assert_eq!(
            d.detail().as_deref(),
            Some("allowManagedHooksOnly in managed settings")
        );
    }

    #[test]
    fn hook_delivery_silent_submit_is_absent_after_ten_seconds() {
        let mut e = DeliveryEvidence {
            carrier: Some(CarrierEvidence::WithEvents),
            last_runner_submit_at_ms: Some(NOW - ABSENT_AFTER_SUBMIT_MS + 1),
            ..Default::default()
        };
        assert_eq!(hook_delivery(&e, NOW).status(), "unknown");
        e.last_runner_submit_at_ms = Some(NOW - ABSENT_AFTER_SUBMIT_MS);
        let d = hook_delivery(&e, NOW);
        assert_eq!(d.status(), "absent");
        assert!(d.detail().unwrap().starts_with("observed_silent"));
        // An answering UserPromptSubmit clears it (and in practice also makes
        // the event count non-zero).
        e.last_user_prompt_submit_at_ms = Some(NOW - ABSENT_AFTER_SUBMIT_MS + 50);
        assert_eq!(hook_delivery(&e, NOW).status(), "unknown");
    }

    #[test]
    fn hook_delivery_carrier_without_events_is_absent() {
        let e = DeliveryEvidence {
            carrier: Some(CarrierEvidence::WithoutEvents),
            ..Default::default()
        };
        assert_eq!(hook_delivery(&e, NOW).status(), "absent");
    }

    #[test]
    fn hook_delivery_nothing_known_is_unknown_never_installed() {
        let d = hook_delivery(&DeliveryEvidence::default(), NOW);
        assert_eq!(d, HookDelivery::Unknown { evidence: None });
        let wire = serde_json::to_value(&d).unwrap();
        assert_eq!(wire, json!({"status": "unknown"}));

        let e = DeliveryEvidence {
            carrier: Some(CarrierEvidence::WithEvents),
            beacon_settings_delivered: Some(true),
            ..Default::default()
        };
        let d = hook_delivery(&e, NOW);
        assert_eq!(d.status(), "unknown");
        assert!(d.detail().unwrap().contains("beacon"));
    }

    #[test]
    fn hook_delivery_fixture_version_matching_ignores_suffix() {
        assert!(cli_version_has_fixture("2.1.285"));
        assert!(cli_version_has_fixture("2.1.285 (Claude Code)"));
        assert!(cli_version_has_fixture("v2.1.285"));
        assert!(!cli_version_has_fixture("2.1.28"));
        assert!(!cli_version_has_fixture(""));
    }
}
