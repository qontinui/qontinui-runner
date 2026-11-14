//! coord's per-computer services bound, and how a computer over it says so
//! (plan `2026-11-13-computer-services-cap-and-sample-outcome`, Phase 1).
//!
//! coord refuses a report WHOLE when one computer lists more than
//! [`MAX_SERVICES_PER_COMPUTER`] services, so `presanitize` cuts the list to
//! the bound from its largest kind and withdraws every kind that lost rows from
//! `services_complete_kinds`. That keeps the report deliverable, but on its own
//! it is silent: coord would serve 256 rows as if they were the whole
//! inventory. [`OverBound`] is the transition machine that puts the cut on
//! coord's read surface — one `telemetry_gap` with `reason:
//! "services_over_bound"` on entering the state, again whenever the truncated
//! kinds change, and every [`REEMIT_AFTER_SECS`] while it persists.
//!
//! **Reading it:** a `services_over_bound` event younger than its
//! `reemit_after_secs` reads "over". An older one, or none, reads "no longer
//! over" only when coord served fewer events than its `EVENTS_LIMIT`; at the
//! limit it is UNKNOWN.
//!
//! What the event says: the named kinds exceed coord's bound on this
//! computer, so every FULL report truncates them and coord cannot apply
//! removals of them. It does not say the report it rides was truncated — a
//! delta carrying it may list only a few changed rows. The decision uses the
//! same cut ([`cut_to_bound`]) as `presanitize`, but over the OBSERVED scan,
//! never a delta's own (smaller) list: a delta of 44 changed rows says nothing
//! about whether the computer still watches 300 units.
//!
//! A PARTIAL scan (a bus that did not answer, a truncated `sc query`) cannot
//! say a kind it did not see completely is back within the bound, so a kind
//! already over stays over (HELD) until a scan sees that kind completely, and
//! the event reports a held kind's last count a scan could vouch for, apart
//! from this scan's counts. Leaving takes [`LEAVE_AFTER_WITHIN_SCANS`]
//! consecutive complete in-bound scans, so a computer at the edge is one entry,
//! not one per crossing.

use std::collections::{BTreeMap, BTreeSet};

use super::events::{self, ComputerEvent};
use super::services::{self, ServiceRow};
use super::{MAX_SERVICES_PER_COMPUTER, MAX_UNIT_LEN};

/// The `telemetry_gap` reason this module emits.
pub(crate) const OVER_BOUND_REASON: &str = "services_over_bound";

/// While a computer stays over the bound, the event is re-sent this often, so
/// a reader can tell a standing condition from a stale one.
pub(crate) const REEMIT_AFTER_SECS: i64 = 86_400;

/// A row coord can accept at all: its unit name is within `MAX_UNIT_LEN`.
/// `presanitize` drops the others before they take a slot.
pub(crate) fn sendable(r: &ServiceRow) -> bool {
    r.unit.chars().count() <= MAX_UNIT_LEN
}

/// Cut `rows` to [`MAX_SERVICES_PER_COMPUTER`]: the LARGEST kind (ties: the
/// later name) is moved to the tail, preserving order otherwise, and the list
/// is truncated. Returns the kinds that lost rows — empty when the list fits.
/// The caller must have dropped unsendable rows first. PURE.
pub(crate) fn cut_to_bound(rows: &mut Vec<ServiceRow>) -> BTreeSet<&'static str> {
    if rows.len() <= MAX_SERVICES_PER_COMPUTER {
        return BTreeSet::new();
    }
    let largest = kind_counts(rows)
        .into_iter()
        .max_by_key(|(k, n)| (*n, *k))
        .map(|(k, _)| k);
    rows.sort_by_key(|r| Some(services::classify_unit(&r.unit)) == largest);
    let cut = rows[MAX_SERVICES_PER_COMPUTER..]
        .iter()
        .map(|r| services::classify_unit(&r.unit))
        .collect();
    rows.truncate(MAX_SERVICES_PER_COMPUTER);
    cut
}

/// The kinds [`cut_to_bound`] would truncate for these rows, counting only the
/// sendable ones. Empty = within coord's bound. PURE.
pub(crate) fn services_cut(rows: &[ServiceRow]) -> BTreeSet<&'static str> {
    let mut sendable_rows: Vec<ServiceRow> = rows.iter().filter(|r| sendable(r)).cloned().collect();
    cut_to_bound(&mut sendable_rows)
}

/// Sendable rows per kind. PURE.
pub(crate) fn kind_counts(rows: &[ServiceRow]) -> BTreeMap<&'static str, usize> {
    let mut per_kind = BTreeMap::new();
    for r in rows.iter().filter(|r| sendable(r)) {
        *per_kind
            .entry(services::classify_unit(&r.unit))
            .or_default() += 1;
    }
    per_kind
}

/// What one [`OverBound::step`] asks the reporter to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Queue a `services_over_bound` `telemetry_gap` event.
    Emit,
    /// WARN: the computer entered the state, or its truncated kinds changed.
    Warn,
    /// INFO: the computer is back within the bound.
    Info,
}

/// Leaving the over-bound state takes this many CONSECUTIVE complete scans
/// within the bound. A computer sitting at the edge (a runner unit being
/// re-registered moves the count 256 ↔ 257) would otherwise emit an event and
/// a WARN on every crossing back over, every tick. Three scans is 90 s at the
/// default 30 s cadence: long enough to ride out a unit re-registering, short
/// beside the 24 h re-emit, and the only cost of the dwell is an INFO that
/// arrives 60 s later than the first in-bound scan.
pub(crate) const LEAVE_AFTER_WITHIN_SCANS: u32 = 3;

/// One tick's observed scan, as the machine needs it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scan<'a> {
    /// [`services_cut`] of the observed scan.
    pub(crate) cut: &'a BTreeSet<&'static str>,
    /// [`kind_counts`] of the observed scan.
    pub(crate) counts: &'a BTreeMap<&'static str, usize>,
    /// The kinds the scan saw completely.
    pub(crate) complete_kinds: &'a [&'static str],
}

/// The over-bound state of one computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Over {
    /// The kinds over the bound: cut by this scan, or HELD.
    pub(crate) kinds: BTreeSet<&'static str>,
    /// Kinds kept over because this scan did not see them completely.
    pub(crate) held: BTreeSet<&'static str>,
    /// Per kind, the last count a scan could vouch for: one that saw the kind
    /// completely, or — for a kind no scan has yet seen completely — the scan
    /// that put it over (a lower bound). A held kind keeps its entry, so a
    /// partial scan's smaller count is never reported as the kind's size.
    pub(crate) last_counts: BTreeMap<&'static str, usize>,
    /// When the last event was queued (in-memory: a reporter rebuild re-emits
    /// on entering, which can add an event but never lose one).
    pub(crate) emitted_at: chrono::DateTime<chrono::Utc>,
    /// Consecutive complete scans within the bound (see
    /// [`LEAVE_AFTER_WITHIN_SCANS`]).
    pub(crate) within_scans: u32,
}

/// One computer's over-bound state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum OverBound {
    #[default]
    Within,
    Over(Over),
}

impl OverBound {
    /// Advance on one tick's observation; `None` = the scan produced no
    /// service information, which keeps the previous state and does nothing.
    ///
    /// A kind already over that this scan did NOT see completely is HELD
    /// over — a partial scan's smaller count is not evidence it shrank — but
    /// only a kind some instrument can report complete
    /// ([`services::COMPLETABLE_KINDS`]): a kind no scan can ever vouch for
    /// would otherwise be held for ever, so its own count decides. Leaving
    /// takes [`LEAVE_AFTER_WITHIN_SCANS`] consecutive complete scans within
    /// the bound, and crossing back over during that dwell is not a new
    /// entry. PURE.
    pub(crate) fn step(
        &self,
        scan: Option<Scan<'_>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Action>) {
        let Some(scan) = scan else {
            return (self.clone(), Vec::new());
        };
        let OverBound::Over(prev) = self else {
            if scan.cut.is_empty() {
                return (OverBound::Within, Vec::new());
            }
            let entered = Over {
                kinds: scan.cut.clone(),
                held: BTreeSet::new(),
                last_counts: scan.counts.clone(),
                emitted_at: now,
                within_scans: 0,
            };
            return (OverBound::Over(entered), vec![Action::Emit, Action::Warn]);
        };
        let held: BTreeSet<&'static str> = prev
            .kinds
            .iter()
            .copied()
            .filter(|k| {
                services::COMPLETABLE_KINDS.contains(k)
                    && !scan.complete_kinds.contains(k)
                    && !scan.cut.contains(k)
            })
            .collect();
        let kinds: BTreeSet<&'static str> = scan.cut.union(&held).copied().collect();
        let mut last_counts = prev.last_counts.clone();
        for (k, n) in scan.counts {
            if !held.contains(k) {
                last_counts.insert(k, *n);
            }
        }
        let mut next = Over {
            kinds,
            held,
            last_counts,
            emitted_at: prev.emitted_at,
            within_scans: 0,
        };
        if next.kinds.is_empty() {
            let within_scans = prev.within_scans.saturating_add(1);
            if within_scans >= LEAVE_AFTER_WITHIN_SCANS {
                return (OverBound::Within, vec![Action::Info]);
            }
            // The dwell: still over, naming the kinds it last named.
            next.kinds = prev.kinds.clone();
            next.within_scans = within_scans;
            return (OverBound::Over(next), Vec::new());
        }
        if next.kinds != prev.kinds {
            next.emitted_at = now;
            return (OverBound::Over(next), vec![Action::Emit, Action::Warn]);
        }
        if (now - prev.emitted_at).num_seconds() >= REEMIT_AFTER_SECS {
            next.emitted_at = now;
            return (OverBound::Over(next), vec![Action::Emit]);
        }
        (OverBound::Over(next), Vec::new())
    }
}

/// The `telemetry_gap` saying this computer's service list is capped at
/// coord's bound. `observed` is THIS scan's sendable count per kind, for the
/// kinds it saw as they are; a HELD kind (over, but not seen completely by
/// this scan) is left out of `observed` and listed in `held_kinds`, with its
/// last count a scan could vouch for in `held_last_counts`. PURE.
pub(crate) fn over_bound_event(
    identity: &str,
    scan_counts: &BTreeMap<&'static str, usize>,
    over: &Over,
    now: chrono::DateTime<chrono::Utc>,
) -> ComputerEvent {
    let at = events::rfc3339(now);
    let observed: BTreeMap<&'static str, usize> = scan_counts
        .iter()
        .filter(|(k, _)| !over.held.contains(*k))
        .map(|(k, n)| (*k, *n))
        .collect();
    let held_last_counts: BTreeMap<&'static str, Option<usize>> = over
        .held
        .iter()
        .map(|k| (*k, over.last_counts.get(k).copied()))
        .collect();
    ComputerEvent {
        client_event_id: events::event_id("telemetry_gap", &[OVER_BOUND_REASON, identity, &at]),
        kind: "telemetry_gap".into(),
        observed_at: at,
        detail: serde_json::json!({
            "reason": OVER_BOUND_REASON,
            "bound": MAX_SERVICES_PER_COMPUTER,
            "observed": observed,
            "held_kinds": over.held,
            "held_last_counts": held_last_counts,
            "truncated_kinds": over.kinds,
            "reemit_after_secs": REEMIT_AFTER_SECS,
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn row(unit: String) -> ServiceRow {
        ServiceRow {
            kind: services::classify_unit(&unit).to_string(),
            unit,
            active_state: Some("active".into()),
            sub_state: Some("running".into()),
            result: None,
            restart_policy: None,
            oom_policy: None,
            memory_max: None,
            memory_peak: None,
            n_restarts: None,
            state_changed_at: None,
            runner_name: None,
            repo: None,
        }
    }

    fn gh(n: usize) -> Vec<ServiceRow> {
        (0..n)
            .map(|i| row(format!("actions.runner.r{i}.service")))
            .collect()
    }

    fn qr(n: usize) -> Vec<ServiceRow> {
        (0..n)
            .map(|i| row(format!("qontinui-runner-{i}.service")))
            .collect()
    }

    fn set(kinds: &[&'static str]) -> BTreeSet<&'static str> {
        kinds.iter().copied().collect()
    }

    /// A scan that saw both kinds completely.
    const ALL: &[&str] = &["gh_actions_runner", "qontinui_runner"];

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn the_cut_is_empty_up_to_256_rows_and_names_the_largest_kind_past_it() {
        for n in [0, 1, 255, 256] {
            assert!(services_cut(&gh(n)).is_empty(), "{n} rows fit");
        }
        assert_eq!(services_cut(&gh(257)), set(&["gh_actions_runner"]));

        let mut mixed = qr(3);
        mixed.extend(gh(300));
        assert_eq!(
            services_cut(&mixed),
            set(&["gh_actions_runner"]),
            "the small kind is sent whole"
        );

        let mut two = gh(300);
        two.extend(qr(300));
        assert_eq!(
            services_cut(&two),
            set(&["gh_actions_runner", "qontinui_runner"]),
            "600 rows cannot fit by cutting one kind of 300"
        );

        // An unsendable (over-long) unit takes no slot: 256 sendable + 1 that
        // presanitize drops is within the bound.
        let mut long = gh(256);
        long.push(row(format!("actions.runner.{}.service", "u".repeat(300))));
        assert!(services_cut(&long).is_empty());
    }

    #[test]
    fn cutting_keeps_256_rows_and_counts_only_sendable_ones() {
        let mut rows = qr(3);
        rows.extend(gh(300));
        let cut = cut_to_bound(&mut rows);
        assert_eq!(rows.len(), 256);
        assert_eq!(cut, set(&["gh_actions_runner"]));
        assert_eq!(
            rows.iter().filter(|r| r.kind == "qontinui_runner").count(),
            3
        );

        let mut counted = gh(2);
        counted.push(row(format!("actions.runner.{}.service", "u".repeat(300))));
        counted.extend(qr(1));
        assert_eq!(
            kind_counts(&counted),
            BTreeMap::from([("gh_actions_runner", 2), ("qontinui_runner", 1)])
        );
    }

    const GH: &str = "gh_actions_runner";
    const QR: &str = "qontinui_runner";
    const OTHER: &str = "other_watched";

    fn counts(c: &[(&'static str, usize)]) -> BTreeMap<&'static str, usize> {
        c.iter().copied().collect()
    }

    /// Step `s` with a scan of these counts, seen completely for `complete`;
    /// the cut is what [`services_cut`] would make of those counts.
    fn step_with(
        s: &OverBound,
        c: &[(&'static str, usize)],
        complete: &[&'static str],
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Action>) {
        let counts = counts(c);
        let mut rows = Vec::new();
        for (kind, n) in &counts {
            let prefix = match *kind {
                GH => "actions.runner.r",
                QR => "qontinui-runner-",
                _ => "other-",
            };
            rows.extend((0..*n).map(|i| row(format!("{prefix}{i}.service"))));
        }
        let cut = services_cut(&rows);
        s.step(
            Some(Scan {
                cut: &cut,
                counts: &counts,
                complete_kinds: complete,
            }),
            now,
        )
    }

    fn over(s: &OverBound) -> &Over {
        match s {
            OverBound::Over(o) => o,
            OverBound::Within => panic!("expected Over, got Within"),
        }
    }

    #[test]
    fn the_over_bound_machine_emits_on_enter_change_and_daily_and_informs_on_return() {
        let t0 = at("2026-11-14T00:00:00Z");
        let t1 = at("2026-11-14T00:00:30Z");

        // Within, staying within: nothing.
        let (s, a) = step_with(&OverBound::Within, &[(GH, 256)], ALL, t0);
        assert_eq!((s, a), (OverBound::Within, vec![]));

        // Enter: event + WARN.
        let (s1, a) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        assert_eq!(over(&s1).kinds, set(&[GH]));
        assert_eq!(over(&s1).emitted_at, t0);

        // Same kinds on the next tick: nothing, and the emit clock holds.
        let (s2, a) = step_with(&s1, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert_eq!(s2, s1);

        // Changed kinds: event + WARN.
        let (s3, a) = step_with(&s2, &[(GH, 300), (QR, 300)], ALL, t1);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        assert_eq!(over(&s3).kinds, set(&[GH, QR]));

        // Just under 24 h later: nothing; at 24 h: event, no WARN.
        let both = [(GH, 300), (QR, 300)];
        let (s4, a) = step_with(&s3, &both, ALL, at("2026-11-14T23:59:59Z"));
        assert_eq!(a, vec![]);
        let day = at("2026-11-15T00:00:30Z");
        let (s5, a) = step_with(&s4, &both, ALL, day);
        assert_eq!(a, vec![Action::Emit]);
        assert_eq!(over(&s5).emitted_at, day);

        // Back within: INFO only after three complete in-bound scans.
        let (s6, a) = step_with(&s5, &[(GH, 10)], ALL, day);
        assert_eq!(a, vec![]);
        let (s7, a) = step_with(&s6, &[(GH, 10)], ALL, day);
        assert_eq!(a, vec![]);
        assert_eq!(over(&s7).within_scans, 2);
        let (s8, a) = step_with(&s7, &[(GH, 10)], ALL, day);
        assert_eq!((s8, a), (OverBound::Within, vec![Action::Info]));

        // Over → no scan → over: the gap holds the state, so no INFO and no
        // second event.
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        let (gap, a) = o.step(None, t1);
        assert_eq!(a, vec![]);
        assert_eq!(gap, o);
        let (again, a) = step_with(&gap, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert_eq!(again, o);
    }

    /// A PARTIAL scan (a bus that did not answer, a truncated `sc query`)
    /// that happens to list fewer than 256 units must not read as "back
    /// within" — the next complete scan would re-enter, and the state would
    /// flap with an INFO and a fresh event every time.
    #[test]
    fn a_partial_scan_under_the_bound_does_not_read_as_back_within() {
        let t0 = at("2026-11-14T00:00:00Z");
        let t1 = at("2026-11-14T00:00:30Z");
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);

        // Over → partial scan under 256 → over: no INFO, no second event.
        let (held, a) = step_with(&o, &[(GH, 12)], &[QR], t1);
        assert_eq!(a, vec![], "a partial scan cannot say the kind shrank");
        assert_eq!(over(&held).kinds, set(&[GH]));
        assert_eq!(over(&held).held, set(&[GH]));
        assert_eq!(over(&held).within_scans, 0, "a held scan is not in-bound");
        let (again, a) = step_with(&held, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert!(over(&again).held.is_empty());

        // A partial scan that drops one of two over kinds is no change either.
        let (two, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 300)], ALL, t0);
        let (kept, a) = step_with(&two, &[(GH, 300), (QR, 5)], &[GH], t1);
        assert_eq!(a, vec![]);
        assert_eq!(over(&kept).kinds, set(&[GH, QR]));
    }

    /// Review round 2, A1: an event decided while a kind is HELD reports the
    /// held kind's last count a scan could vouch for, apart from this scan's
    /// counts — never the partial scan's smaller number.
    #[test]
    fn a_held_kind_is_reported_with_its_last_complete_count() {
        let t0 = at("2026-11-14T00:00:00Z");
        let t1 = at("2026-11-14T00:00:30Z");
        let (two, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 300)], ALL, t0);
        // Only the runner kind seen completely, and it is within; the GitHub
        // runner kind answered partially with 12 rows.
        let (s, a) = step_with(&two, &[(GH, 12), (QR, 200)], &[QR], t1);
        assert_eq!(a, vec![Action::Emit, Action::Warn], "the kinds changed");
        let o = over(&s);
        assert_eq!(o.kinds, set(&[GH]));
        assert_eq!(o.held, set(&[GH]));
        assert_eq!(o.last_counts.get(GH), Some(&300));
        let e = over_bound_event("h", &counts(&[(GH, 12), (QR, 200)]), o, t1);
        assert_eq!(
            e.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "bound": 256,
                "observed": {"qontinui_runner": 200},
                "held_kinds": ["gh_actions_runner"],
                "held_last_counts": {"gh_actions_runner": 300},
                "truncated_kinds": ["gh_actions_runner"],
                "reemit_after_secs": 86400,
            })
        );
    }

    /// Review round 2, A2: a computer oscillating 256 ↔ 257 is one entry —
    /// crossing back over during the dwell is not a new enter.
    #[test]
    fn oscillating_at_the_bound_is_one_entry_not_one_per_crossing() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (mut s, a) = step_with(&OverBound::Within, &[(GH, 257)], ALL, t0);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        for i in 1..=20 {
            let n = if i % 2 == 0 { 257 } else { 256 };
            let (next, a) = step_with(&s, &[(GH, n)], ALL, t0);
            assert_eq!(a, vec![], "tick {i} at {n}");
            assert_eq!(over(&next).kinds, set(&[GH]), "tick {i}");
            s = next;
        }
        for _ in 0..2 {
            let (next, a) = step_with(&s, &[(GH, 256)], ALL, t0);
            assert_eq!(a, vec![]);
            s = next;
        }
        let (s, a) = step_with(&s, &[(GH, 256)], ALL, t0);
        assert_eq!((s, a), (OverBound::Within, vec![Action::Info]));
    }

    /// Review round 2, A3: a kind no instrument can report complete is never
    /// held by the partial-scan rule, or it could never leave.
    #[test]
    fn a_kind_no_scan_can_complete_is_not_held() {
        assert!(!services::COMPLETABLE_KINDS.contains(&OTHER));
        let t0 = at("2026-11-14T00:00:00Z");
        let (mut s, _) = step_with(&OverBound::Within, &[(OTHER, 300)], ALL, t0);
        assert_eq!(over(&s).kinds, set(&[OTHER]));
        for _ in 0..2 {
            let (next, a) = step_with(&s, &[(OTHER, 10)], ALL, t0);
            assert_eq!(a, vec![]);
            assert!(over(&next).held.is_empty());
            s = next;
        }
        let (s, a) = step_with(&s, &[(OTHER, 10)], ALL, t0);
        assert_eq!((s, a), (OverBound::Within, vec![Action::Info]));
    }

    #[test]
    fn the_event_carries_the_bound_the_counts_and_the_truncated_kinds() {
        let now = at("2026-11-14T00:00:00Z");
        let (s, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 3)], ALL, now);
        let e = over_bound_event("h", &counts(&[(GH, 300), (QR, 3)]), over(&s), now);
        assert_eq!(e.kind, "telemetry_gap");
        assert_eq!(e.observed_at, "2026-11-14T00:00:00Z");
        assert_eq!(
            e.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "bound": 256,
                "observed": {"gh_actions_runner": 300, "qontinui_runner": 3},
                "held_kinds": [],
                "held_last_counts": {},
                "truncated_kinds": ["gh_actions_runner"],
                "reemit_after_secs": 86400,
            })
        );
    }
}
