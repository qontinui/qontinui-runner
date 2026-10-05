//! Per-pane FINISHED state for the Terminal page's window borders.
//!
//! A session is "marked finished" through either of TWO independent doors, and
//! a pane that reflects the mark has to hear both of them:
//!
//! - the **runner-local** marker, `TerminalSessionRecord::finished_at`, written
//!   by [`super::terminal::terminal_session_set_finished`] and
//!   `POST /sessions/{id}/finish`;
//! - **coord's work axis**, `coord.sessions.session_status = finished`, which is
//!   what `/finish-session` (and therefore `/unattended`'s closeout) writes,
//!   through `coord_report_status`. Nothing copies that write back into the
//!   local store, so a reader of `finished_at` alone would miss the most common
//!   way a session gets marked.
//!
//! Both are the WORK axis — "is there anything left to do" — and neither says
//! anything about liveness. A finished session keeps running until it exits.
//!
//! ## Tri-state, because an unread coord is not "unfinished"
//!
//! [`FinishedVerdict::Unknown`] is returned when the local marker is absent AND
//! coord could not be read: the coord door is where `/finish-session` writes, so
//! its silence cannot be read as "nobody marked this". The Terminal page draws
//! the finished border only on [`FinishedVerdict::Finished`], so an unknown
//! renders as the ordinary border — no claim either way — and is exposed as
//! such on the pane's `data-session-finished` attribute.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::commands::CommandResponse;
use crate::mcp::session_work_status::{self, StatusFetch};
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::session::tracking_health::SessionWorkStatus;

/// Whether a session is marked finished, as far as this runner can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishedVerdict {
    /// At least one door says the session's work is finished.
    Finished,
    /// The local marker is absent and coord answered without `finished`.
    NotFinished,
    /// The local marker is absent and coord could not be read.
    Unknown,
}

/// Which door(s) carried a [`FinishedVerdict::Finished`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishedSource {
    Local,
    Coord,
    Both,
}

/// One session's merged finished state, as the frontend reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFinishedState {
    pub verdict: FinishedVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<FinishedSource>,
    /// Unix millis of the EARLIEST mark among the doors that report one. Absent
    /// when the only door saying `finished` is a coord that serves no `since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

/// What coord said about one session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordObservation {
    /// coord answered and the work axis reads `finished` (with its `since`, when
    /// coord serves one).
    Finished(Option<i64>),
    /// coord answered, and the axis is something else, unset, or there is no
    /// row for this id. Either way, `/finish-session` has not marked it.
    NotFinished,
    /// coord was not asked about this id, or could not answer.
    Unobserved,
}

/// Merge the two doors into one verdict.
///
/// The local marker wins when the two disagree. It is the one the runner
/// already treats as authoritative for resume (`restorable_records`), and a
/// fresh local mark legitimately reads `working` on coord until the outbox
/// delivers it.
pub fn merge(local_finished_at: Option<i64>, coord: CoordObservation) -> SessionFinishedState {
    let finished = |source, finished_at| SessionFinishedState {
        verdict: FinishedVerdict::Finished,
        source: Some(source),
        finished_at,
    };
    match (local_finished_at, coord) {
        (Some(local), CoordObservation::Finished(coord_at)) => finished(
            FinishedSource::Both,
            Some(coord_at.map_or(local, |c| c.min(local))),
        ),
        (Some(local), _) => finished(FinishedSource::Local, Some(local)),
        (None, CoordObservation::Finished(coord_at)) => finished(FinishedSource::Coord, coord_at),
        (None, CoordObservation::NotFinished) => SessionFinishedState {
            verdict: FinishedVerdict::NotFinished,
            source: None,
            finished_at: None,
        },
        (None, CoordObservation::Unobserved) => SessionFinishedState {
            verdict: FinishedVerdict::Unknown,
            source: None,
            finished_at: None,
        },
    }
}

/// Read coord's answer for each of `ids` out of one bulk [`StatusFetch`].
///
/// `ids` must be the list the fetch was made with: [`session_work_status::fetch`]
/// sends only the first [`session_work_status::MAX_IDS`], so an id past that cap
/// was never asked about and is [`CoordObservation::Unobserved`], not
/// "not finished".
pub fn coord_observations(
    ids: &[String],
    fetch: &StatusFetch,
) -> HashMap<String, CoordObservation> {
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let obs = if fetch.degraded || i >= session_work_status::MAX_IDS {
                CoordObservation::Unobserved
            } else if fetch.by_session_id.get(id) == Some(&SessionWorkStatus::Finished) {
                CoordObservation::Finished(fetch.finished_at_by_session_id.get(id).copied())
            } else {
                CoordObservation::NotFinished
            };
            (id.clone(), obs)
        })
        .collect()
}

/// The finished state of each given Claude session, for the Terminal page's
/// pane borders.
///
/// `data` is `{ sessions: { <claudeSessionId>: SessionFinishedState }, coord:
/// { degraded, note } }`. One bulk coord read per call (≈4 s worst case, see
/// [`session_work_status`]); a coord failure degrades each unmarked id to
/// `unknown` rather than failing the call.
#[tauri::command]
pub async fn terminal_session_finished_states(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_ids: Vec<String>,
) -> Result<CommandResponse, String> {
    let mut ids: Vec<String> = Vec::with_capacity(claude_session_ids.len());
    for id in claude_session_ids {
        let id = id.trim().to_string();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }

    let fetch = session_work_status::fetch(&ids).await;
    let coord = coord_observations(&ids, &fetch);

    let sessions: serde_json::Map<String, serde_json::Value> = ids
        .iter()
        .map(|id| {
            let local = store.get(id).and_then(|r| r.finished_at);
            let state = merge(
                local,
                coord
                    .get(id)
                    .copied()
                    .unwrap_or(CoordObservation::Unobserved),
            );
            (
                id.clone(),
                serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "sessions": sessions,
            "coord": {
                "degraded": fetch.degraded,
                "note": fetch.note,
            },
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch_with(
        statuses: &[(&str, SessionWorkStatus)],
        finished_at: &[(&str, i64)],
    ) -> StatusFetch {
        StatusFetch {
            by_session_id: statuses
                .iter()
                .map(|(id, s)| (id.to_string(), s.clone()))
                .collect(),
            finished_at_by_session_id: finished_at
                .iter()
                .map(|(id, at)| (id.to_string(), *at))
                .collect(),
            source: "coord",
            degraded: false,
            note: String::new(),
            requested: 0,
            resolved: 0,
        }
    }

    #[test]
    fn coord_finished_alone_is_finished_from_coord() {
        let s = merge(None, CoordObservation::Finished(Some(50)));
        assert_eq!(s.verdict, FinishedVerdict::Finished);
        assert_eq!(s.source, Some(FinishedSource::Coord));
        assert_eq!(s.finished_at, Some(50));
    }

    #[test]
    fn local_mark_wins_over_a_coord_that_has_not_caught_up() {
        // A fresh local mark reads `working` on coord until the outbox drains.
        for coord in [CoordObservation::NotFinished, CoordObservation::Unobserved] {
            let s = merge(Some(10), coord);
            assert_eq!(s.verdict, FinishedVerdict::Finished, "{coord:?}");
            assert_eq!(s.source, Some(FinishedSource::Local));
            assert_eq!(s.finished_at, Some(10));
        }
    }

    #[test]
    fn both_doors_report_the_earliest_mark() {
        let s = merge(Some(80), CoordObservation::Finished(Some(30)));
        assert_eq!(s.source, Some(FinishedSource::Both));
        assert_eq!(s.finished_at, Some(30));
        let s = merge(Some(80), CoordObservation::Finished(None));
        assert_eq!(s.finished_at, Some(80));
    }

    #[test]
    fn an_unread_coord_is_unknown_never_not_finished() {
        let s = merge(None, CoordObservation::Unobserved);
        assert_eq!(s.verdict, FinishedVerdict::Unknown);
        assert_eq!(s.source, None);
        let s = merge(None, CoordObservation::NotFinished);
        assert_eq!(s.verdict, FinishedVerdict::NotFinished);
    }

    #[test]
    fn observations_read_only_finished_as_finished() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let fetch = fetch_with(
            &[
                ("a", SessionWorkStatus::Finished),
                ("b", SessionWorkStatus::Working),
            ],
            &[("a", 7)],
        );
        let obs = coord_observations(&ids, &fetch);
        assert_eq!(obs["a"], CoordObservation::Finished(Some(7)));
        assert_eq!(obs["b"], CoordObservation::NotFinished);
        // No row / unset axis: coord answered, and nobody finished it there.
        assert_eq!(obs["c"], CoordObservation::NotFinished);
    }

    #[test]
    fn a_degraded_fetch_observes_nothing() {
        let ids = vec!["a".to_string()];
        let mut fetch = fetch_with(&[("a", SessionWorkStatus::Finished)], &[]);
        fetch.degraded = true;
        assert_eq!(
            coord_observations(&ids, &fetch)["a"],
            CoordObservation::Unobserved
        );
    }

    #[test]
    fn ids_past_the_coord_cap_were_never_asked() {
        let ids: Vec<String> = (0..=session_work_status::MAX_IDS)
            .map(|i| format!("id-{i}"))
            .collect();
        let obs = coord_observations(&ids, &fetch_with(&[], &[]));
        assert_eq!(obs["id-0"], CoordObservation::NotFinished);
        assert_eq!(
            obs[&format!("id-{}", session_work_status::MAX_IDS)],
            CoordObservation::Unobserved
        );
    }

    #[test]
    fn wire_shape_is_camel_case_and_omits_absent_fields() {
        let v = serde_json::to_value(merge(None, CoordObservation::Finished(Some(5)))).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"verdict": "finished", "source": "coord", "finishedAt": 5})
        );
        let v = serde_json::to_value(merge(None, CoordObservation::Unobserved)).unwrap();
        assert_eq!(v, serde_json::json!({"verdict": "unknown"}));
    }
}
