//! Session-message doorbell — the coord → runner wake for the in-session
//! delivery poller (plan
//! `2026-10-02-a-sent-coord-message-does-not-wake-its-recipient-session`,
//! Phase 4).
//!
//! When `coord_send_message` (or a parked `claim:`/`resource:` delivery)
//! commits a row addressed to a session on this device, coord publishes a
//! `message_enqueued` directive on
//! `qontinui.sessions.<tenant>.<device>.message_enqueued` — the same subject
//! family [`super::handoff`] already receives through the `?subscribe=sessions`
//! lane. This module adds no second socket:
//! [`super::handoff::connect_and_pump`] forwards every frame here and
//! [`parse_message_push`] claims only the `.message_enqueued` suffix for this
//! device — the same disambiguation-by-trailing-segment the handoff, respawn,
//! attach and create arms use.
//!
//! The payload is ids only — `{message_id, to_session, priority}` — so the
//! push is a DOORBELL, never a second delivery channel: the body stays behind
//! the authorized `GET /coord/session-messages/pending` read the poller
//! already makes. Ringing it only wakes
//! [`crate::mcp::session_message_poller`] out of its 10 s sleep; that poll
//! stays the catch-up, so a dropped push degrades to today's latency, never
//! to a lost message.
//!
//! The wake is a process-global [`tokio::sync::Notify`]. `notify_one` stores a
//! single permit when the poller is mid-tick, so a push that lands while a
//! tick is running still causes exactly one immediate re-tick, and a burst of
//! pushes coalesces into one.

use std::sync::OnceLock;

use serde::Deserialize;
use tokio::sync::Notify;
use uuid::Uuid;

/// The channel suffix this arm claims (after `.<device>`).
const SUFFIX: &str = "message_enqueued";

/// The ids-only payload coord publishes with a `message_enqueued` directive.
/// `to_session` and `priority` default so a coord that trims a sibling field
/// still rings the bell; `message_id` is required — a frame without one is
/// not coord's directive.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MessageEnqueued {
    pub message_id: String,
    #[serde(default)]
    pub to_session: Option<String>,
    /// `fyi` | `normal` | `blocking`. Kept as the wire string: the poller
    /// decides the cooldown exception from the PENDING row's own priority
    /// (the authorized read), so an unknown value here only affects logs.
    #[serde(default)]
    pub priority: String,
}

impl MessageEnqueued {
    /// Did the sender declare this message urgent?
    pub fn is_blocking(&self) -> bool {
        self.priority.eq_ignore_ascii_case("blocking")
    }
}

/// The process-global doorbell the poller waits on beside its poll timer.
pub fn wake() -> &'static Notify {
    static WAKE: OnceLock<Notify> = OnceLock::new();
    WAKE.get_or_init(Notify::new)
}

/// Pure parse+filter of a coord `/ws` envelope into a [`MessageEnqueued`]
/// addressed to `device_id`. `None` when the frame is not a `message_enqueued`
/// directive for this device — the handoff, respawn, attach and create arms
/// see the same text and filter on their own suffixes.
pub(super) fn parse_message_push(text: &str, device_id: Uuid) -> Option<MessageEnqueued> {
    let envelope: serde_json::Value = serde_json::from_str(text).ok()?;
    let channel = envelope.get("channel").and_then(|c| c.as_str())?;

    // `machine_subject_raw` is exactly
    // `qontinui.sessions.<tenant>.<machine>.<kind>` — five segments, as
    // `create::parse_create_push` requires. Matching every segment (not only
    // the two ends) means a channel with an extra segment spliced in, or an
    // empty tenant, does not pass on its endpoints alone.
    let device = device_id.to_string();
    let segments: Vec<&str> = channel.split('.').collect();
    let [root, family, tenant, machine, kind] = segments.as_slice() else {
        return None;
    };
    if *root != "qontinui"
        || *family != "sessions"
        || tenant.is_empty()
        || *machine != device
        || *kind != SUFFIX
    {
        return None;
    }

    // Payload may be a JSON string (the Redis arm) or an inlined object.
    let payload_val = match envelope.get("payload") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s).ok()?,
        Some(other) => other.clone(),
        None => return None,
    };
    serde_json::from_value(payload_val).ok()
}

/// Message ids this process rang for, newest last — test-only evidence that a
/// frame reached the ring through whichever dispatcher carried it. Keyed by
/// id so concurrent tests sharing the global doorbell never read each
/// other's rings.
#[cfg(test)]
pub(crate) fn rung_ids() -> &'static std::sync::Mutex<Vec<String>> {
    static RUNG: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();
    RUNG.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Ring `wake` for one parsed directive.
fn ring(wake: &Notify, enqueued: &MessageEnqueued) {
    tracing::debug!(
        message_id = %enqueued.message_id,
        to_session = ?enqueued.to_session,
        priority = %enqueued.priority,
        blocking = enqueued.is_blocking(),
        "session messages: message_enqueued push received; waking the poller"
    );
    #[cfg(test)]
    rung_ids()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(enqueued.message_id.clone());
    wake.notify_one();
}

/// Handle one inbound `/ws` frame on the message-wake arm. Not a
/// `message_enqueued` directive for this device → ignored silently.
pub(super) fn handle_push_frame(device_id: Uuid, text: &str) {
    handle_push_frame_on(wake(), device_id, text);
}

/// [`handle_push_frame`] against an explicit doorbell, so the ring is
/// observable in a test without racing other users of the global one.
fn handle_push_frame_on(wake: &Notify, device_id: Uuid, text: &str) {
    if let Some(enqueued) = parse_message_push(text, device_id) {
        ring(wake, &enqueued);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn body() -> serde_json::Value {
        json!({
            "message_id": "0192a1b2-0000-7000-8000-0000000000aa",
            "to_session": Uuid::from_u128(20).to_string(),
            "priority": "blocking",
        })
    }

    /// A string-payload envelope on this device's `.message_enqueued` subject
    /// parses; the same body on another device's subject, on a sibling arm's
    /// suffix, or outside the sessions family is ignored.
    #[test]
    fn parses_only_this_devices_message_enqueued_suffix() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let tenant = Uuid::from_u128(30);

        let ok = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.message_enqueued"),
            "payload": body().to_string(),
        });
        let parsed = parse_message_push(&ok.to_string(), device).expect("parsed");
        assert_eq!(parsed.message_id, "0192a1b2-0000-7000-8000-0000000000aa");
        assert_eq!(
            parsed.to_session.as_deref(),
            Some(Uuid::from_u128(20).to_string().as_str())
        );
        assert!(parsed.is_blocking());

        for wrong in [
            format!("qontinui.sessions.{tenant}.{other}.message_enqueued"),
            format!("qontinui.sessions.{tenant}.{device}.attach_request"),
            format!("qontinui.sessions.{tenant}.{device}.create_request"),
            format!("qontinui.sessions.{tenant}.{device}.respawn_request"),
            format!("qontinui.sessions.{tenant}.{device}.handoff_request"),
            format!("qontinui.sessions.{tenant}.{device}.message_enqueued_v2"),
            format!("qontinui.other.{tenant}.{device}.message_enqueued"),
            format!("qontinui.sessions.a.b.{device}.message_enqueued"),
            format!("qontinui.sessions..{device}.message_enqueued"),
            format!("xqontinui.sessions.{tenant}.{device}.message_enqueued"),
            format!("qontinui.sessions.{tenant}.x{device}.message_enqueued"),
        ] {
            let env = json!({"channel": wrong, "payload": body().to_string()});
            assert!(
                parse_message_push(&env.to_string(), device).is_none(),
                "{wrong} must not parse"
            );
        }
    }

    /// An inlined-object payload parses; a payload without `message_id`, a
    /// missing payload, and non-JSON text do not. Absent `to_session` /
    /// `priority` default rather than refusing the doorbell.
    #[test]
    fn payload_forms_and_defaults() {
        let device = Uuid::from_u128(10);
        let tenant = Uuid::from_u128(30);
        let channel = format!("qontinui.sessions.{tenant}.{device}.message_enqueued");

        let inlined = json!({"channel": channel, "payload": body()});
        assert!(parse_message_push(&inlined.to_string(), device).is_some());

        let minimal = json!({"channel": channel, "payload": {"message_id": "m1"}});
        let parsed = parse_message_push(&minimal.to_string(), device).expect("minimal");
        assert_eq!(parsed.to_session, None);
        assert!(!parsed.is_blocking());

        let no_id = json!({"channel": channel, "payload": {"priority": "normal"}});
        assert!(parse_message_push(&no_id.to_string(), device).is_none());

        let no_payload = json!({"channel": channel});
        assert!(parse_message_push(&no_payload.to_string(), device).is_none());

        assert!(parse_message_push("not json", device).is_none());
    }

    /// Only `blocking` (any case) is urgent; `fyi`, `normal` and unknown
    /// values are not.
    #[test]
    fn only_blocking_is_blocking() {
        let mk = |p: &str| MessageEnqueued {
            message_id: "m".into(),
            to_session: None,
            priority: p.into(),
        };
        assert!(mk("blocking").is_blocking());
        assert!(mk("BLOCKING").is_blocking());
        assert!(!mk("normal").is_blocking());
        assert!(!mk("fyi").is_blocking());
        assert!(!mk("").is_blocking());
    }

    /// A frame for this device leaves a permit on the doorbell, so a waiter
    /// that arrives AFTER the push still wakes at once; a frame for another
    /// device rings nothing.
    #[tokio::test]
    async fn handle_push_frame_rings_the_doorbell() {
        let device = Uuid::from_u128(10);
        let other = Uuid::from_u128(11);
        let tenant = Uuid::from_u128(30);
        let wake = Notify::new();

        let foreign = json!({
            "channel": format!("qontinui.sessions.{tenant}.{other}.message_enqueued"),
            "payload": body().to_string(),
        });
        handle_push_frame_on(&wake, device, &foreign.to_string());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), wake.notified())
                .await
                .is_err(),
            "another device's frame must not ring"
        );

        let mine = json!({
            "channel": format!("qontinui.sessions.{tenant}.{device}.message_enqueued"),
            "payload": body().to_string(),
        });
        handle_push_frame_on(&wake, device, &mine.to_string());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), wake.notified())
                .await
                .is_ok(),
            "this device's frame must ring"
        );
    }
}
