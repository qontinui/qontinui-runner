//! What a coord answer to the runner's `finished` PATCH proves — the verdict
//! the outbox drain reads before it lets `finish_synced` be stamped (plan
//! `2026-11-05-finish-synced-is-stamped-on-a-2xx-that-did-not-move-session-status`,
//! Phase 4).
//!
//! ## Why a 2xx is not enough
//!
//! The drain used to classify ANY 2xx to `PATCH /sessions/:id
//! {progress:{session_status:"finished"}}` as `Acked`, and the `Acked` arm
//! stamps `finish_synced`. On coord `89909b161` a status-carrying progress write
//! whose statement failed to prepare was swallowed at `debug!` and the route
//! still answered 2xx — with the row's OLD `session_status` in the body. The
//! runner then reported `finishSynced: true` for a session coord still read as
//! unfinished (dossier `finish-synced-true-while-coord-unfinished`).
//!
//! coord's PATCH answer is the session row re-hydrated AFTER the write
//! (`SessionRow.session_status`), so the runner can read, in the same exchange,
//! whether the row actually moved. That is what [`finished_ack_verdict`] does:
//! the signal now observes the state it names, on every coord build, whatever
//! the next swallowed write's cause turns out to be.
//!
//! ## What happens to an unconfirmed finish
//!
//! It is NOT dropped (that would lose the finish for good) and NOT retried
//! forever (a coord that never moves the row would cost a request a minute per
//! finished session, for good). The drain keeps the row on the session's
//! ordinary backoff (1 s doubling to one minute) and, only once it has had at
//! least [`FINISHED_UNCONFIRMED_MAX_ATTEMPTS`] unconfirmed answers AND the
//! first of them is at least [`FINISHED_UNCONFIRMED_MIN_AGE`] old, ACK-drops it
//! WITHOUT stamping. The count alone would be spent in ~30 s, which a coord
//! deploy or a transient swallow easily outlasts.
//!
//! The bound is NOT guaranteed to fire before the whole-session quarantine.
//! An unconfirmed answer never adds to `serving_failures`, but the session can
//! still be quarantined by its OTHER failures (an earlier row's serving
//! failures carried in the same retry entry, or a 5xx on the finish itself
//! while coord serves other rows). Entering quarantine drops the session's
//! retry entry, which restarts the count and the age. A quarantined session's
//! lifecycle rows — the finish among them — stay held and are retried once a
//! minute, so an unconfirmed finish there still reaches this bound and is
//! dropped the same way.
//!
//! A dropped finish is ACK-dropped WITHOUT stamping and counts it in
//! [`FINISHED_UNCONFIRMED_DROPPED`] (`GET /health`
//! `sessionOutbox.finishedUnconfirmed`). The mark stays unsynced, which is the
//! honest state. Exactly three things re-offer it, none of them automatic
//! within this process: a reason-less `POST /sessions/<id>/finish` (the
//! read-back's retry door in `SessionLifecycleStore::set_finished`); the
//! session's next coord registration or terminal bind (the registrar's late
//! delivery, in-process only); and the NEXT runner start's boot sweep
//! (`session::finish_heal`), which re-offers an unsynced mark only when the
//! mark's own coord row then reads `session_status` NULL. A row reading
//! `working` or anything else is never re-offered by the sweep.

use std::sync::atomic::AtomicU64;

use serde_json::Value as JsonValue;

use super::tracking_health::SessionWorkStatus;

/// The failure kind a finished row that coord answered 2xx WITHOUT moving
/// carries in the drain's classifier and in `/health`'s `lastFailure.kind`.
/// Distinct from `network` / `server_error`: coord answered, and answered
/// with a row that says the write did not land.
pub(crate) const FINISHED_UNCONFIRMED_KIND: &str = "finished_unconfirmed";

/// Unconfirmed answers an unconfirmed finish must have had before it may be
/// ACK-dropped unstamped (together with [`FINISHED_UNCONFIRMED_MIN_AGE`]).
pub(crate) const FINISHED_UNCONFIRMED_MAX_ATTEMPTS: u32 = 5;

/// How long after its FIRST unconfirmed answer an unconfirmed finish is kept
/// before it may be ACK-dropped unstamped (together with
/// [`FINISHED_UNCONFIRMED_MAX_ATTEMPTS`]).
///
/// Ten minutes, because a dropped finish is re-offered by nothing automatic in
/// this process, and the next runner start's heal sweep never re-offers a row
/// reading `working` — so dropping early loses the finish for as long as the
/// runner lives. Ten minutes outlasts a coord rolling deploy (the cause of the
/// swallowed writes this module exists for was one coord build) and a burst of
/// transient prepare failures, while costing at most ~15 PATCHes per finish on
/// the one-minute backoff cap: cheap, and still bounded.
pub(crate) const FINISHED_UNCONFIRMED_MIN_AGE: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Finished rows ACK-dropped unstamped after
/// [`FINISHED_UNCONFIRMED_MAX_ATTEMPTS`] unconfirmed answers spread over at
/// least [`FINISHED_UNCONFIRMED_MIN_AGE`], since this
/// process booted. Bumped only once the outbox ACK has landed.
pub(crate) static FINISHED_UNCONFIRMED_DROPPED: AtomicU64 = AtomicU64::new(0);

/// What one coord answer to a `finished` PATCH proves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinishedAck {
    /// coord's row reads `finished` in its own answer: the finish landed.
    /// The only verdict that may stamp `finish_synced`.
    Observed,
    /// coord answered, but its answer does not show the row finished — an
    /// other status, an absent one (UNKNOWN, never `finished`), an unreadable
    /// body, `progress_outcome.updated: false`, or a non-2xx that is not a
    /// refusal. Retried, bounded, never stamped.
    Unconfirmed(String),
    /// coord refused the row (a 4xx — e.g. `422 stale_finish_refused`).
    /// Never stamped. The drain's existing 4xx ladder owns this case; the
    /// verdict names it only so the function is total over a status.
    Refused,
}

/// Classify coord's answer to the runner's `finished` PATCH. Pure.
///
/// - not 2xx: a 4xx is [`FinishedAck::Refused`], anything else
///   [`FinishedAck::Unconfirmed`] (the drain only calls this for a 2xx);
/// - a body carrying `progress_outcome.updated` that is not `true` is
///   unconfirmed — coord said its progress half did not move the row;
/// - otherwise the body's own `session_status` decides: `finished` (or the
///   legacy alias `done`) is [`FinishedAck::Observed`]; any other word, a
///   `null`, an absent field (a coord that skips serializing an unset axis),
///   or a non-JSON body is unconfirmed.
pub(crate) fn finished_ack_verdict(status: u16, body: &str) -> FinishedAck {
    if !(200..300).contains(&status) {
        return if (400..500).contains(&status) {
            FinishedAck::Refused
        } else {
            FinishedAck::Unconfirmed(format!("HTTP {status} is not a 2xx"))
        };
    }
    let Ok(answer) = serde_json::from_str::<JsonValue>(body) else {
        return FinishedAck::Unconfirmed(
            "the 2xx body is not JSON, so coord's row was not observed".to_string(),
        );
    };
    if let Some(outcome) = answer.get("progress_outcome") {
        match outcome.get("updated") {
            Some(JsonValue::Bool(true)) => {}
            Some(JsonValue::Bool(false)) => {
                return FinishedAck::Unconfirmed(
                    "progress_outcome.updated is false — coord's progress write did not \
                     move the row"
                        .to_string(),
                )
            }
            Some(other) => {
                return FinishedAck::Unconfirmed(format!(
                    "progress_outcome.updated is {other}, not a boolean"
                ))
            }
            // A `progress_outcome` without `updated` says nothing either way:
            // the row's own status below decides.
            None => {}
        }
    }
    match answer.get("session_status") {
        Some(JsonValue::String(raw)) => match SessionWorkStatus::parse(raw) {
            SessionWorkStatus::Finished => FinishedAck::Observed,
            other => FinishedAck::Unconfirmed(format!(
                "coord's row reads session_status {:?}, not finished",
                other.as_wire()
            )),
        },
        Some(JsonValue::Null) | None => FinishedAck::Unconfirmed(
            "coord's answer carries no session_status — UNKNOWN, never finished".to_string(),
        ),
        Some(other) => FinishedAck::Unconfirmed(format!("session_status is {other}, not a string")),
    }
}

/// The `PushOutcome::Transport` message for an unconfirmed finish:
/// status-prefixed like every other non-ACK the drain carries, with
/// [`FINISHED_UNCONFIRMED_KIND`] as the second token so the classifier can
/// name it.
pub(crate) fn unconfirmed_message(status: u16, reason: &str) -> String {
    format!("{status}: {FINISHED_UNCONFIRMED_KIND} — {reason}")
}

/// Whether a drain failure message is an unconfirmed finish
/// ([`unconfirmed_message`]'s shape).
pub(crate) fn is_unconfirmed_message(msg: &str) -> bool {
    let mut tokens = msg.split_whitespace();
    let status_led = tokens
        .next()
        .map(|t| t.trim_end_matches(':'))
        .and_then(|t| t.parse::<u16>().ok())
        .is_some_and(|s| (100..=599).contains(&s));
    status_led && tokens.next() == Some(FINISHED_UNCONFIRMED_KIND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_2xx_whose_row_reads_finished_is_observed() {
        assert_eq!(
            finished_ack_verdict(
                200,
                r#"{"id":"01a1ab84-1c55-7000-8000-000000000001","session_status":"finished"}"#
            ),
            FinishedAck::Observed
        );
        // Phase 3's coord adds the outcome; `updated: true` keeps it observed.
        assert_eq!(
            finished_ack_verdict(
                200,
                r#"{"session_status":"finished","progress_outcome":{"updated":true,"transitioned":false}}"#
            ),
            FinishedAck::Observed
        );
        // The legacy alias coord's own parser accepts.
        assert_eq!(
            finished_ack_verdict(204, r#"{"session_status":"done"}"#),
            FinishedAck::Observed
        );
    }

    #[test]
    fn a_2xx_whose_row_reads_working_is_unconfirmed() {
        match finished_ack_verdict(200, r#"{"session_status":"working"}"#) {
            FinishedAck::Unconfirmed(reason) => assert!(reason.contains("\"working\""), "{reason}"),
            other => panic!("a working row is not a finish: {other:?}"),
        }
    }

    #[test]
    fn a_2xx_with_no_session_status_is_unconfirmed_never_finished() {
        for body in [
            r#"{"id":"01a1ab84-1c55-7000-8000-000000000001"}"#,
            r#"{"session_status":null}"#,
            "",
            "<html>bad gateway</html>",
            r#"{"session_status":42}"#,
        ] {
            assert!(
                matches!(finished_ack_verdict(200, body), FinishedAck::Unconfirmed(_)),
                "{body:?} must not read as finished"
            );
        }
    }

    #[test]
    fn progress_outcome_updated_false_overrides_a_finished_status() {
        assert!(matches!(
            finished_ack_verdict(
                200,
                r#"{"session_status":"finished","progress_outcome":{"updated":false,"transitioned":false}}"#
            ),
            FinishedAck::Unconfirmed(_)
        ));
        assert!(matches!(
            finished_ack_verdict(
                200,
                r#"{"session_status":"finished","progress_outcome":{"updated":"yes"}}"#
            ),
            FinishedAck::Unconfirmed(_)
        ));
    }

    #[test]
    fn non_2xx_answers_are_refused_or_unconfirmed() {
        assert_eq!(
            finished_ack_verdict(422, r#"{"error":"stale_finish_refused"}"#),
            FinishedAck::Refused
        );
        assert!(matches!(
            finished_ack_verdict(503, r#"{"error":"progress_write_not_applied"}"#),
            FinishedAck::Unconfirmed(_)
        ));
    }

    #[test]
    fn the_unconfirmed_message_is_status_prefixed_and_recognisable() {
        let m = unconfirmed_message(200, "coord's row reads session_status \"working\"");
        assert!(m.starts_with("200: finished_unconfirmed"), "{m}");
        assert!(is_unconfirmed_message(&m));
        assert!(!is_unconfirmed_message(
            "503 Service Unavailable: {\"error\":\"progress_write_not_applied\"}"
        ));
        assert!(!is_unconfirmed_message("[timeout] finished_unconfirmed"));
    }
}
