//! Per-pane FINISHED state for the Terminal page's window borders.
//!
//! A session is "marked finished" through either of TWO doors, and a pane that
//! reflects the mark has to hear both of them:
//!
//! - the **runner-local** marker, `TerminalSessionRecord::finished_at`, written
//!   by [`super::terminal::terminal_session_set_finished`] and
//!   `POST /sessions/{id}/finish` — `/finish-session`'s first rung, which also
//!   queues the coord write;
//! - **coord's work axis**, `coord.sessions.session_status = finished`, which
//!   `/finish-session` writes directly through `coord_report_status` when the
//!   runner rung does not carry the call. Nothing copies that write back into
//!   the local store, so a reader of `finished_at` alone would miss it.
//!
//! Both are the WORK axis — "is there anything left to do" — and neither says
//! anything about liveness. A finished session keeps running until it exits.
//!
//! ## Which door wins
//!
//! coord is authoritative for the FACT once it has seen the local mark: the
//! store's own contract is that a local mark coord contradicts may be cleared
//! "only when it is synced" (`TerminalSessionRecord::finish_synced`). So:
//!
//! - an UNSYNCED local mark is finished whatever coord says — coord reads
//!   `working` until the outbox delivers it;
//! - a SYNCED local mark that coord now contradicts with an explicit status
//!   (the session reported `working` again after being finished) is not
//!   finished. An UNSET axis does not contradict it: coord answers from the
//!   newest of a session's rows, and a re-registration creates a row whose
//!   axis is empty while the mark it already ACKed stays on the older one;
//! - with no local mark, coord's answer stands.
//!
//! ## Tri-state, because an unread coord is not "unfinished"
//!
//! [`FinishedVerdict::Unknown`] is returned when nothing local decides it AND
//! coord could not say — it was unreachable, or it could not resolve the id at
//! all. Its silence there is not evidence that nobody marked the session.

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
    /// The session's work is marked finished.
    Finished,
    /// It is not marked finished (or coord has since contradicted the mark).
    NotFinished,
    /// Nothing local decides it, and coord could not say.
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

/// The runner-local finished marker of one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMark {
    pub finished_at: i64,
    /// coord has ACKed this mark (`TerminalSessionRecord::finish_synced`).
    pub synced: bool,
}

/// What coord said about one session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordObservation {
    /// coord's row reads `finished` (with its `since`, when coord serves one).
    Finished(Option<i64>),
    /// coord's row reads an explicit status other than `finished`.
    NotFinished,
    /// coord has a row for this id with no status on its work axis. Not
    /// finished on coord, but no contradiction of a mark coord ACKed earlier
    /// (see the module docs).
    Unset,
    /// coord was not asked, could not answer, or could not resolve the id.
    Unobserved,
}

/// Merge the two doors into one verdict. See the module docs for the rules.
pub fn merge(local: Option<LocalMark>, coord: CoordObservation) -> SessionFinishedState {
    let finished = |source, finished_at| SessionFinishedState {
        verdict: FinishedVerdict::Finished,
        source: Some(source),
        finished_at,
    };
    let not = |verdict| SessionFinishedState {
        verdict,
        source: None,
        finished_at: None,
    };
    match (local, coord) {
        (Some(local), CoordObservation::Finished(coord_at)) => finished(
            FinishedSource::Both,
            Some(coord_at.map_or(local.finished_at, |c| c.min(local.finished_at))),
        ),
        // coord saw this mark and has explicitly moved off it since.
        (Some(LocalMark { synced: true, .. }), CoordObservation::NotFinished) => {
            not(FinishedVerdict::NotFinished)
        }
        (Some(local), _) => finished(FinishedSource::Local, Some(local.finished_at)),
        (None, CoordObservation::Finished(coord_at)) => finished(FinishedSource::Coord, coord_at),
        (None, CoordObservation::NotFinished | CoordObservation::Unset) => {
            not(FinishedVerdict::NotFinished)
        }
        (None, CoordObservation::Unobserved) => not(FinishedVerdict::Unknown),
    }
}

/// Read coord's answer for each of `ids` out of one bulk [`StatusFetch`].
///
/// `ids` must be the list the fetch was made with: [`session_work_status::fetch`]
/// sends only the first [`session_work_status::MAX_IDS`], so an id past that cap
/// was never asked about and is [`CoordObservation::Unobserved`], as is an id
/// coord named in its `unknown` / `invalid` lists.
pub fn coord_observations(
    ids: &[String],
    fetch: &StatusFetch,
) -> HashMap<String, CoordObservation> {
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let obs = if fetch.degraded
                || i >= session_work_status::MAX_IDS
                || fetch.unresolved_ids.contains(id)
            {
                CoordObservation::Unobserved
            } else {
                match fetch.by_session_id.get(id) {
                    Some(SessionWorkStatus::Finished) => {
                        CoordObservation::Finished(fetch.finished_at_by_session_id.get(id).copied())
                    }
                    Some(_) => CoordObservation::NotFinished,
                    // Answered, resolved, and no status: a row whose axis is
                    // empty (`map_from_body` keeps only non-blank statuses).
                    None => CoordObservation::Unset,
                }
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
/// [`session_work_status`]); a coord failure degrades each id nothing local
/// decides to `unknown` rather than failing the call.
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
            let local = store.get(id).and_then(|r| {
                r.finished_at.map(|finished_at| LocalMark {
                    finished_at,
                    synced: r.finish_synced,
                })
            });
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

/// The event the page re-reads on, emitted whenever a session's runner-local
/// finished marker changes, so a local mark reaches its pane at once instead of
/// on the next poll. Payload: `{ claudeSessionId }`.
pub const FINISHED_CHANGED_EVENT: &str = "terminal-session-finished-changed";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
            unresolved_ids: HashSet::new(),
            source: "coord",
            degraded: false,
            note: String::new(),
            requested: 0,
            resolved: 0,
        }
    }

    fn mark(finished_at: i64, synced: bool) -> Option<LocalMark> {
        Some(LocalMark {
            finished_at,
            synced,
        })
    }

    #[test]
    fn coord_finished_alone_is_finished_from_coord() {
        let s = merge(None, CoordObservation::Finished(Some(50)));
        assert_eq!(s.verdict, FinishedVerdict::Finished);
        assert_eq!(s.source, Some(FinishedSource::Coord));
        assert_eq!(s.finished_at, Some(50));
    }

    #[test]
    fn an_unsynced_local_mark_wins_over_a_coord_that_has_not_caught_up() {
        for coord in [CoordObservation::NotFinished, CoordObservation::Unobserved] {
            let s = merge(mark(10, false), coord);
            assert_eq!(s.verdict, FinishedVerdict::Finished, "{coord:?}");
            assert_eq!(s.source, Some(FinishedSource::Local));
            assert_eq!(s.finished_at, Some(10));
        }
    }

    #[test]
    fn a_synced_local_mark_yields_to_coord_moving_off_it() {
        // Finished, ACKed, then the session reported `working` again.
        let s = merge(mark(10, true), CoordObservation::NotFinished);
        assert_eq!(s.verdict, FinishedVerdict::NotFinished);
        assert_eq!(s.source, None);
    }

    #[test]
    fn a_synced_local_mark_stands_when_coord_is_unread_or_unset() {
        // Unset: a re-registration's fresh row, whose axis is empty.
        for coord in [CoordObservation::Unobserved, CoordObservation::Unset] {
            let s = merge(mark(10, true), coord);
            assert_eq!(s.verdict, FinishedVerdict::Finished, "{coord:?}");
            assert_eq!(s.source, Some(FinishedSource::Local));
        }
    }

    #[test]
    fn both_doors_report_the_earliest_mark() {
        let s = merge(mark(80, true), CoordObservation::Finished(Some(30)));
        assert_eq!(s.source, Some(FinishedSource::Both));
        assert_eq!(s.finished_at, Some(30));
        let s = merge(mark(80, false), CoordObservation::Finished(None));
        assert_eq!(s.finished_at, Some(80));
    }

    #[test]
    fn an_unread_coord_is_unknown_never_not_finished() {
        let s = merge(None, CoordObservation::Unobserved);
        assert_eq!(s.verdict, FinishedVerdict::Unknown);
        assert_eq!(s.source, None);
        for coord in [CoordObservation::NotFinished, CoordObservation::Unset] {
            assert_eq!(merge(None, coord).verdict, FinishedVerdict::NotFinished);
        }
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
        // Answered and resolved with no status: a row whose axis is empty.
        assert_eq!(obs["c"], CoordObservation::Unset);
    }

    #[test]
    fn ids_coord_could_not_resolve_are_unobserved() {
        let ids: Vec<String> = ["gone", "bad"].iter().map(|s| s.to_string()).collect();
        let mut fetch = fetch_with(&[], &[]);
        fetch.unresolved_ids = ids.iter().cloned().collect();
        let obs = coord_observations(&ids, &fetch);
        assert_eq!(obs["gone"], CoordObservation::Unobserved);
        assert_eq!(obs["bad"], CoordObservation::Unobserved);
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
        assert_eq!(obs["id-0"], CoordObservation::Unset);
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
