//! What the ledger's classification reads from the aftermath for ONE roster
//! (plan `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
//! Phase 3).
//!
//! A roster is read against the records that cover it, and no others:
//!
//! - this boot's own prior roster (the generation this boot retained, and the
//!   report's top-level diff) against THIS boot's record alone. An earlier boot
//!   that named the same file (an unchanged roster `rotate_in` declined to
//!   duplicate) observed a different restart, so it says nothing about this
//!   one;
//! - an older generation against the earlier records that name its file.
//!
//! The newest poll tick's process evidence applies to every roster: it says who
//! holds a session NOW.

use super::{AftermathInputs, AftermathRecord, CameBackBy, Close, ProcessEvidence};
use crate::session::close_reason::CloseReason;

impl Close {
    /// Was this close deliberate, by the typed close-reason set? A stored
    /// string outside the set is not.
    pub fn is_deliberate(&self) -> bool {
        CloseReason::is_deliberate_wire(&self.reason)
    }
}

/// One session as the records covering a roster have it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSession {
    /// The EARLIEST instant any covering record saw it come back.
    pub came_back_at: Option<i64>,
    pub came_back_by: Option<CameBackBy>,
    /// Its close, as the NEWEST covering record holding it last saw the
    /// registry (`None` while that record saw it open).
    pub close: Option<Close>,
    /// Its finish, as that same record last saw the registry.
    pub finished_at: Option<i64>,
}

/// The aftermath as one roster's classification reads it.
#[derive(Debug, Clone)]
pub struct AftermathView<'a> {
    /// The records covering the roster, newest boot first.
    records: Vec<&'a AftermathRecord>,
    process: &'a ProcessEvidence,
}

impl AftermathInputs {
    /// The view of this boot's own prior roster: this boot's record alone.
    pub fn this_boot_view(&self) -> AftermathView<'_> {
        AftermathView {
            records: self.this_boot.iter().collect(),
            process: &self.process,
        }
    }

    /// The view of the retained generation stored as `file`: this boot's
    /// record when it is the generation THIS boot retained, otherwise the
    /// earlier records that name `file`.
    pub fn generation_view(&self, file: &str, this_boot: bool) -> AftermathView<'_> {
        let records = if this_boot {
            self.this_boot.iter().collect()
        } else {
            self.earlier
                .iter()
                .filter(|r| r.generation_file.as_deref() == Some(file))
                .collect()
        };
        AftermathView {
            records,
            process: &self.process,
        }
    }
}

impl AftermathView<'_> {
    /// `id` (lowercased) as the covering records have it, or `None` when no
    /// covering record has it.
    pub fn session(&self, id: &str) -> Option<RecordedSession> {
        let mut holding = self.records.iter().filter_map(|r| r.sessions.get(id));
        let newest = holding.next()?;
        let mut out = RecordedSession {
            came_back_at: newest.came_back_at,
            came_back_by: newest.came_back_by,
            close: newest.close(),
            finished_at: newest.finished_at,
        };
        for older in holding {
            if let Some(at) = older.came_back_at {
                if out.came_back_at.is_none_or(|cur| at < cur) {
                    out.came_back_at = Some(at);
                    out.came_back_by = older.came_back_by;
                }
            }
        }
        Some(out)
    }

    /// A live `claude` held `id` (lowercased) at the newest poll tick, at most
    /// two tick intervals ago.
    pub fn held_now(&self, id: &str) -> bool {
        matches!(self.process, ProcessEvidence::Held(ids) if ids.contains(id))
    }

    /// The newest tick's process read is UNKNOWN (failed, not run yet this
    /// boot, or stale): a session it might have found cannot be ruled out.
    pub fn process_unknown(&self) -> bool {
        matches!(self.process, ProcessEvidence::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    use super::super::{SessionAftermath, AFTERMATH_VERSION};
    use super::*;

    fn record(boot_ms: i64, file: &str, sessions: &[(&str, SessionAftermath)]) -> AftermathRecord {
        AftermathRecord {
            aftermath_version: AFTERMATH_VERSION,
            boot_ms,
            generation_file: Some(file.to_string()),
            sessions: sessions
                .iter()
                .map(|(id, s)| (id.to_string(), s.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn back_at(at: i64, by: CameBackBy) -> SessionAftermath {
        SessionAftermath {
            came_back_at: Some(at),
            came_back_by: Some(by),
            ..SessionAftermath::default()
        }
    }

    /// An older generation reads only the records naming its file; this boot's
    /// generation reads only this boot's record, even when an earlier boot
    /// named the same file. Mutation: `generation_view` ignoring `this_boot`
    /// (always the earlier records naming the file).
    #[test]
    fn each_roster_reads_only_the_records_that_cover_it() {
        let inputs = AftermathInputs {
            this_boot: Some(record(
                300,
                "g-now",
                &[("a", back_at(310, CameBackBy::Bound))],
            )),
            earlier: Arc::new(vec![
                record(200, "g-now", &[("b", back_at(210, CameBackBy::Stamp))]),
                record(100, "g-old", &[("c", back_at(110, CameBackBy::Process))]),
            ]),
            process: ProcessEvidence::NotNeeded,
        };

        let mine = inputs.generation_view("g-now", true);
        assert!(mine.session("a").is_some());
        assert!(
            mine.session("b").is_none(),
            "an earlier boot that named this boot's file observed a different restart"
        );
        assert_eq!(
            inputs.this_boot_view().session("a"),
            mine.session("a"),
            "the top-level diff reads the same record"
        );

        let old = inputs.generation_view("g-old", false);
        assert_eq!(old.session("c").and_then(|s| s.came_back_at), Some(110));
        assert!(old.session("a").is_none());
        let same_file_older = inputs.generation_view("g-now", false);
        assert!(same_file_older.session("b").is_some());
        assert!(same_file_older.session("a").is_none());
    }

    /// Across several records covering one roster: came back at the EARLIEST
    /// instant any saw, closed and finished as the NEWEST holding it last saw.
    /// Mutation: `session` taking the close from the oldest record.
    #[test]
    fn several_records_merge_to_the_earliest_return_and_the_newest_close() {
        let newer = SessionAftermath {
            came_back_at: Some(250),
            came_back_by: Some(CameBackBy::Bound),
            closed_at: Some(260),
            close_reason: Some("explicit".to_string()),
            finished_at: None,
        };
        let older = SessionAftermath {
            came_back_at: Some(150),
            came_back_by: Some(CameBackBy::Stamp),
            closed_at: Some(160),
            close_reason: Some("pty-exit".to_string()),
            finished_at: Some(170),
        };
        let inputs = AftermathInputs {
            this_boot: None,
            earlier: Arc::new(vec![
                record(200, "g", &[("s", newer)]),
                record(100, "g", &[("s", older)]),
            ]),
            process: ProcessEvidence::NotNeeded,
        };

        let s = inputs.generation_view("g", false).session("s").unwrap();
        assert_eq!(s.came_back_at, Some(150));
        assert_eq!(s.came_back_by, Some(CameBackBy::Stamp));
        assert_eq!(
            s.close,
            Some(Close {
                at_ms: Some(260),
                reason: "explicit".to_string()
            })
        );
        assert_eq!(s.finished_at, None, "the newest record saw it unfinished");
    }

    /// Mutation: `process_unknown` never true (an UNKNOWN read would then let
    /// a miss read as a confident one).
    #[test]
    fn held_now_reads_only_a_fresh_read_and_only_an_unknown_read_is_unknown() {
        let held: HashSet<String> = ["s".to_string()].into_iter().collect();
        let view = |process: ProcessEvidence| {
            let inputs = AftermathInputs {
                process,
                ..AftermathInputs::default()
            };
            let v = inputs.this_boot_view();
            (v.held_now("s"), v.held_now("t"), v.process_unknown())
        };
        assert_eq!(view(ProcessEvidence::Held(held)), (true, false, false));
        assert_eq!(view(ProcessEvidence::NotNeeded), (false, false, false));
        assert_eq!(view(ProcessEvidence::Unknown), (false, false, true));
    }
}
