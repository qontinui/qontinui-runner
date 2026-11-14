//! coord's per-computer services bound, and how a computer over it says so
//! (plan `2026-11-13-computer-services-cap-and-sample-outcome`, Phase 1).
//!
//! coord refuses a report WHOLE when one computer lists more than
//! [`MAX_SERVICES_PER_COMPUTER`] services, so `presanitize` cuts the list to
//! the bound from its largest kind and withdraws every kind that lost rows from
//! `services_complete_kinds`. That keeps the report deliverable, but on its own
//! it is silent: coord would serve 256 rows as if they were the whole
//! inventory. [`OverBound`] is the transition machine that puts the cut on
//! coord's read surface — a `telemetry_gap` with `reason:
//! "services_over_bound"` on entering the state, again whenever the truncated
//! kinds change, every [`REEMIT_EVERY_SECS`] while it persists, and one
//! CLOSING event (`cleared: true`, no truncated kinds) on leaving it.
//!
//! **Reading it:** take the newest `services_over_bound` event. `cleared:
//! true` reads "within the bound". Otherwise, younger than its
//! `reemit_after_secs` reads "over" — that window is the re-emit period plus a
//! margin ([`REEMIT_MARGIN_SECS`]) for a jittered tick and delivery back-off,
//! so a standing condition never reads stale between re-emits — the clock
//! runs even while scans produce no service information, and such a re-emit
//! says `scan_answered: false` with every kind held at its last vouched
//! count. An older one,
//! or none, reads "no longer over" only when coord served fewer events than
//! its `EVENTS_LIMIT`; at the limit, or when the reporter stopped reporting,
//! it is UNKNOWN.
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
//! already over stays over (HELD) until a scan sees that kind completely —
//! but only a kind THIS instrument can ever report complete
//! (`ServiceScan::completable_kinds`). The event reports a held kind's last
//! count a scan could vouch for, apart from this scan's counts. Any change of
//! the over-bound kind set — leaving included — is adopted only after
//! [`SETTLE_SCANS`] consecutive scans propose the same new set, so a computer
//! at the edge (256 ↔ 257, or two kinds trading the "largest" slot) is one
//! entry, not one per crossing.

use std::collections::{BTreeMap, BTreeSet};

use super::events::{self, ComputerEvent};
use super::services::{self, ServiceRow};
use super::{MAX_SERVICES_PER_COMPUTER, MAX_UNIT_LEN};

/// The `telemetry_gap` reason this module emits.
pub(crate) const OVER_BOUND_REASON: &str = "services_over_bound";

/// While a computer stays over the bound, the event is re-sent this often.
pub(crate) const REEMIT_EVERY_SECS: i64 = 86_400;

/// How much later than [`REEMIT_EVERY_SECS`] a successor may land: the
/// re-emit is decided on the first ±20%-jittered 30 s tick past the period,
/// and the report carrying it can sit behind a 404/503 or 5xx back-off of up
/// to 360 s per attempt and a 300 s strip window. One hour covers several of
/// those in a row.
pub(crate) const REEMIT_MARGIN_SECS: i64 = 3_600;

/// The freshness window the event advertises as `reemit_after_secs`.
pub(crate) const REEMIT_AFTER_SECS: i64 = REEMIT_EVERY_SECS + REEMIT_MARGIN_SECS;

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
    /// Queue a `services_over_bound` `telemetry_gap` event for the Over state.
    Emit,
    /// WARN: the computer entered the state, or its truncated kinds changed.
    Warn,
    /// Queue the CLOSING `services_over_bound` event: the computer left the
    /// state, so the newest event says "within".
    Clear,
    /// INFO: the computer is back within the bound.
    Info,
}

/// A change of the over-bound kind set — leaving the state included — is
/// adopted only after this many CONSECUTIVE scans propose the same new set.
/// A computer at the edge (a runner unit being re-registered moves the count
/// 256 ↔ 257; two kinds of near-equal size trade the "largest" slot) would
/// otherwise emit an event and a WARN on every tick. Three scans is 90 s at
/// the default 30 s cadence: long enough to ride out a unit re-registering,
/// short beside the 24 h re-emit, and the only cost is a change reported 60 s
/// later than the first scan that showed it. ENTERING is not delayed: the
/// first truncated report must carry its event.
pub(crate) const SETTLE_SCANS: u32 = 3;

/// One tick's observed scan, as the machine needs it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scan<'a> {
    /// [`services_cut`] of the observed scan.
    pub(crate) cut: &'a BTreeSet<&'static str>,
    /// [`kind_counts`] of the observed scan.
    pub(crate) counts: &'a BTreeMap<&'static str, usize>,
    /// The kinds the scan saw completely.
    pub(crate) complete_kinds: &'a [&'static str],
    /// The kinds the instrument that took the scan can EVER report complete.
    pub(crate) completable_kinds: &'a [&'static str],
}

/// The over-bound state of one computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Over {
    /// The kinds over the bound, as last adopted (and last emitted).
    pub(crate) kinds: BTreeSet<&'static str>,
    /// Kinds this scan kept over because it did not see them completely.
    pub(crate) held: BTreeSet<&'static str>,
    /// Per kind, the last count a scan could vouch for: one that saw the kind
    /// completely, or — for a kind no scan has yet seen completely — the scan
    /// that put it over (a lower bound). A held kind keeps its entry, so a
    /// partial scan's smaller count is never reported as the kind's size.
    pub(crate) last_counts: BTreeMap<&'static str, usize>,
    /// When the last event was queued (in-memory: a reporter rebuild re-emits
    /// on entering, which can add an event but never lose one).
    pub(crate) emitted_at: chrono::DateTime<chrono::Utc>,
    /// A different kind set (empty = leaving) proposed by the last
    /// consecutive scans, and how many (see [`SETTLE_SCANS`]).
    pub(crate) proposed: Option<(BTreeSet<&'static str>, u32)>,
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
    /// only when the scan's instrument could ever report it complete; a kind
    /// it never can would otherwise be held for ever, so its own count
    /// decides. Any change of the kind set is adopted after [`SETTLE_SCANS`]
    /// consecutive scans propose it. PURE.
    pub(crate) fn step(
        &self,
        scan: Option<Scan<'_>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Action>) {
        let Some(scan) = scan else {
            // No service information: the state holds — but the re-emit
            // clock still runs, so a long scan outage does not age the over
            // event out. Every adopted kind is then HELD (nothing vouched for
            // it this tick), reported with its last vouched count.
            return match self {
                OverBound::Over(prev)
                    if (now - prev.emitted_at).num_seconds() >= REEMIT_EVERY_SECS =>
                {
                    let mut next = prev.clone();
                    next.held = prev.kinds.clone();
                    next.emitted_at = now;
                    (OverBound::Over(next), vec![Action::Emit])
                }
                _ => (self.clone(), Vec::new()),
            };
        };
        let OverBound::Over(prev) = self else {
            if scan.cut.is_empty() {
                return (OverBound::Within, Vec::new());
            }
            let mut last_counts = scan.counts.clone();
            vouch_zero_for_empty_complete_kinds(&mut last_counts, &scan);
            let entered = Over {
                kinds: scan.cut.clone(),
                held: BTreeSet::new(),
                last_counts,
                emitted_at: now,
                proposed: None,
            };
            return (OverBound::Over(entered), vec![Action::Emit, Action::Warn]);
        };
        let held: BTreeSet<&'static str> = prev
            .kinds
            .iter()
            .copied()
            .filter(|k| {
                scan.completable_kinds.contains(k)
                    && !scan.complete_kinds.contains(k)
                    && !scan.cut.contains(k)
            })
            .collect();
        let observed: BTreeSet<&'static str> = scan.cut.union(&held).copied().collect();
        let mut last_counts = prev.last_counts.clone();
        for (k, n) in scan.counts {
            if !held.contains(k) {
                last_counts.insert(k, *n);
            }
        }
        vouch_zero_for_empty_complete_kinds(&mut last_counts, &scan);
        let mut next = Over {
            kinds: prev.kinds.clone(),
            held,
            last_counts,
            emitted_at: prev.emitted_at,
            proposed: None,
        };
        if observed != prev.kinds {
            let seen = match &prev.proposed {
                Some((set, n)) if *set == observed => n.saturating_add(1),
                _ => 1,
            };
            if seen >= SETTLE_SCANS {
                if observed.is_empty() {
                    return (OverBound::Within, vec![Action::Clear, Action::Info]);
                }
                next.kinds = observed;
                next.emitted_at = now;
                return (OverBound::Over(next), vec![Action::Emit, Action::Warn]);
            }
            // Not settled yet: the adopted set stands (and keeps its re-emit).
            next.proposed = Some((observed, seen));
        }
        if (now - prev.emitted_at).num_seconds() >= REEMIT_EVERY_SECS {
            next.emitted_at = now;
            return (OverBound::Over(next), vec![Action::Emit]);
        }
        (OverBound::Over(next), Vec::new())
    }
}

/// A kind the scan saw completely with no row at all is vouched for as zero,
/// rather than leaving an older count standing. PURE.
fn vouch_zero_for_empty_complete_kinds(
    last_counts: &mut BTreeMap<&'static str, usize>,
    scan: &Scan<'_>,
) {
    for k in scan.complete_kinds {
        if !scan.counts.contains_key(k) {
            last_counts.insert(k, 0);
        }
    }
}

/// The `telemetry_gap` saying this computer's service list is capped at
/// coord's bound. `observed` is THIS scan's sendable count per kind, for the
/// kinds it saw as they are; a HELD kind (over, but not seen completely by
/// this scan) is left out of `observed` and listed in `held_kinds`, with its
/// last count a scan could vouch for in `held_last_counts`. `scan_counts`
/// is `None` when this tick's scan produced no service information
/// (`scan_answered: false`): every kind is then held and `observed` is
/// empty. PURE.
pub(crate) fn over_bound_event(
    identity: &str,
    scan_counts: Option<&BTreeMap<&'static str, usize>>,
    over: &Over,
    now: chrono::DateTime<chrono::Utc>,
) -> ComputerEvent {
    let observed: BTreeMap<&'static str, usize> = scan_counts
        .into_iter()
        .flatten()
        .filter(|(k, _)| !over.held.contains(*k))
        .map(|(k, n)| (*k, *n))
        .collect();
    let held_last_counts: BTreeMap<&'static str, Option<usize>> = over
        .held
        .iter()
        .map(|k| (*k, over.last_counts.get(k).copied()))
        .collect();
    gap_event(
        identity,
        now,
        serde_json::json!({
            "reason": OVER_BOUND_REASON,
            "cleared": false,
            "scan_answered": scan_counts.is_some(),
            "bound": MAX_SERVICES_PER_COMPUTER,
            "observed": observed,
            "held_kinds": over.held,
            "held_last_counts": held_last_counts,
            "truncated_kinds": over.kinds,
            "reemit_after_secs": REEMIT_AFTER_SECS,
        }),
    )
}

/// The CLOSING `services_over_bound` event: the computer is back within the
/// bound, so no kind is truncated any more. Same schema as the over event;
/// `reemit_after_secs` is `null` because a cleared state promises no
/// successor — it stays true until a later event says otherwise. PURE.
pub(crate) fn cleared_event(
    identity: &str,
    scan_counts: &BTreeMap<&'static str, usize>,
    now: chrono::DateTime<chrono::Utc>,
) -> ComputerEvent {
    gap_event(
        identity,
        now,
        serde_json::json!({
            "reason": OVER_BOUND_REASON,
            "cleared": true,
            "scan_answered": true,
            "bound": MAX_SERVICES_PER_COMPUTER,
            "observed": scan_counts,
            "held_kinds": [],
            "held_last_counts": {},
            "truncated_kinds": [],
            "reemit_after_secs": null,
        }),
    )
}

/// Rate limit on the over-bound WARN for one computer. A computer whose unit
/// count drifts slowly across the bound (ephemeral runners registering near
/// 256) can enter and leave every few ticks even with [`SETTLE_SCANS`]; each
/// entry still queues its event, so coord's read surface stays truthful, but
/// the WARN — a request for a human's attention — fires once per kind set per
/// [`REEMIT_EVERY_SECS`], the cadence the event itself restates a standing
/// condition at. A repeat inside the window is logged at debug, and so is
/// the INFO that closes an entry whose WARN was suppressed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WarnLimit {
    last: Option<(BTreeSet<&'static str>, chrono::DateTime<chrono::Utc>)>,
    /// Whether the latest [`WarnLimit::admit`] was admitted.
    entry_warned: bool,
}

impl WarnLimit {
    /// `true` = WARN for `kinds` at `now` (and remember it). PURE.
    pub(crate) fn admit(
        &mut self,
        kinds: &BTreeSet<&'static str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let due = match &self.last {
            Some((last_kinds, at)) => {
                last_kinds != kinds || (now - *at).num_seconds() >= REEMIT_EVERY_SECS
            }
            None => true,
        };
        if due {
            self.last = Some((kinds.clone(), now));
        }
        self.entry_warned = due;
        due
    }

    /// Whether the INFO closing the current entry is logged at info: only
    /// when that entry's WARN was. PURE.
    pub(crate) fn info_due(&self) -> bool {
        self.entry_warned
    }
}

fn gap_event(
    identity: &str,
    now: chrono::DateTime<chrono::Utc>,
    detail: serde_json::Value,
) -> ComputerEvent {
    let at = events::rfc3339(now);
    let cleared = if detail["cleared"] == true {
        "cleared"
    } else {
        "over"
    };
    ComputerEvent {
        client_event_id: events::event_id(
            "telemetry_gap",
            &[OVER_BOUND_REASON, cleared, identity, &at],
        ),
        kind: "telemetry_gap".into(),
        observed_at: at,
        detail,
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
    /// What the Linux buses can report complete.
    const LINUX: &[&str] = &["gh_actions_runner", "qontinui_runner"];

    fn counts(c: &[(&'static str, usize)]) -> BTreeMap<&'static str, usize> {
        c.iter().copied().collect()
    }

    /// Step `s` with a scan of these counts, seen completely for `complete`,
    /// by an instrument that can complete `completable`; the cut is what
    /// [`services_cut`] makes of those counts.
    fn step_by(
        s: &OverBound,
        c: &[(&'static str, usize)],
        complete: &[&'static str],
        completable: &[&'static str],
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
                completable_kinds: completable,
            }),
            now,
        )
    }

    /// [`step_by`] with the Linux instrument.
    fn step_with(
        s: &OverBound,
        c: &[(&'static str, usize)],
        complete: &[&'static str],
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Action>) {
        step_by(s, c, complete, LINUX, now)
    }

    /// Step `n` times with the same scan, returning the last state and every
    /// step's actions.
    fn step_n(
        s: &OverBound,
        n: usize,
        c: &[(&'static str, usize)],
        complete: &[&'static str],
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Vec<Action>>) {
        let mut s = s.clone();
        let mut all = Vec::new();
        for _ in 0..n {
            let (next, a) = step_with(&s, c, complete, now);
            s = next;
            all.push(a);
        }
        (s, all)
    }

    fn over(s: &OverBound) -> &Over {
        match s {
            OverBound::Over(o) => o,
            OverBound::Within => panic!("expected Over, got Within"),
        }
    }

    #[test]
    fn the_over_bound_machine_emits_on_enter_change_and_daily_and_clears_on_return() {
        let t0 = at("2026-11-14T00:00:00Z");
        let t1 = at("2026-11-14T00:00:30Z");

        // Within, staying within: nothing.
        let (s, a) = step_with(&OverBound::Within, &[(GH, 256)], ALL, t0);
        assert_eq!((s, a), (OverBound::Within, vec![]));

        // Enter: event + WARN at once.
        let (s1, a) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        assert_eq!(over(&s1).kinds, set(&[GH]));
        assert_eq!(over(&s1).emitted_at, t0);

        // Same kinds on the next tick: nothing, and the emit clock holds.
        let (s2, a) = step_with(&s1, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert_eq!(s2, s1);

        // Changed kinds: adopted on the third consecutive scan.
        let both = [(GH, 300), (QR, 300)];
        let (s3, a) = step_n(&s2, 3, &both, ALL, t1);
        assert_eq!(a, vec![vec![], vec![], vec![Action::Emit, Action::Warn]]);
        assert_eq!(over(&s3).kinds, set(&[GH, QR]));

        // One second short of 24 h: nothing; at 24 h: event, no WARN.
        let (s4, a) = step_with(&s3, &both, ALL, at("2026-11-14T23:59:59Z"));
        assert_eq!(a, vec![]);
        let day = at("2026-11-15T00:00:30Z");
        let (s5, a) = step_with(&s4, &both, ALL, day);
        assert_eq!(a, vec![Action::Emit]);
        assert_eq!(over(&s5).emitted_at, day);

        // Back within: the closing event and INFO after three in-bound scans.
        let (s6, a) = step_n(&s5, 3, &[(GH, 10)], ALL, day);
        assert_eq!(a, vec![vec![], vec![], vec![Action::Clear, Action::Info]]);
        assert_eq!(s6, OverBound::Within);

        // Over → no scan → over: the gap holds the state.
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        let (gap, a) = o.step(None, t1);
        assert_eq!(a, vec![]);
        assert_eq!(gap, o);
        let (again, a) = step_with(&gap, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert_eq!(again, o);
    }

    /// Review round 3, A1: the advertised freshness window outlasts the
    /// re-emit period by a margin, so the newest event of a standing
    /// condition never reads stale while its successor is on the way.
    #[test]
    fn the_advertised_freshness_outlasts_the_reemit_period_by_an_hour() {
        assert_eq!(REEMIT_EVERY_SECS, 86_400);
        assert_eq!(REEMIT_AFTER_SECS, 90_000);
        assert_eq!(REEMIT_AFTER_SECS - REEMIT_EVERY_SECS, 3_600, "the margin");
        let (s, _) = step_with(
            &OverBound::Within,
            &[(GH, 300)],
            ALL,
            at("2026-11-14T00:00:00Z"),
        );
        let e = over_bound_event(
            "h",
            Some(&counts(&[(GH, 300)])),
            over(&s),
            at("2026-11-14T00:00:00Z"),
        );
        assert_eq!(e.detail["reemit_after_secs"], 90_000);
    }

    /// A PARTIAL scan (a bus that did not answer, a truncated `sc query`)
    /// that happens to list fewer than 256 units must not read as "back
    /// within" — the next complete scan would re-enter, and the state would
    /// flap.
    #[test]
    fn a_partial_scan_under_the_bound_does_not_read_as_back_within() {
        let t0 = at("2026-11-14T00:00:00Z");
        let t1 = at("2026-11-14T00:00:30Z");
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);

        let (held, a) = step_with(&o, &[(GH, 12)], &[QR], t1);
        assert_eq!(a, vec![], "a partial scan cannot say the kind shrank");
        assert_eq!(over(&held).kinds, set(&[GH]));
        assert_eq!(over(&held).held, set(&[GH]));
        assert_eq!(over(&held).proposed, None, "a held scan proposes nothing");
        let (again, a) = step_with(&held, &[(GH, 300)], ALL, t1);
        assert_eq!(a, vec![]);
        assert!(over(&again).held.is_empty());

        // A partial scan that drops one of two over kinds is no change either.
        let (two, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 300)], ALL, t0);
        let (kept, a) = step_n(&two, 5, &[(GH, 300), (QR, 5)], &[GH], t1);
        assert!(a.iter().all(Vec::is_empty));
        assert_eq!(over(&kept).kinds, set(&[GH, QR]));
    }

    /// Review round 3, A8: a held scan in the middle of the leave dwell
    /// restarts it — the dwell counts CONSECUTIVE complete in-bound scans.
    #[test]
    fn a_held_scan_during_the_leave_dwell_restarts_it() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        let (two_in, a) = step_n(&o, 2, &[(GH, 10)], ALL, t0);
        assert!(a.iter().all(Vec::is_empty));
        assert_eq!(over(&two_in).proposed, Some((BTreeSet::new(), 2)));

        let (held, a) = step_with(&two_in, &[(GH, 12)], &[QR], t0);
        assert_eq!(a, vec![]);
        assert_eq!(
            over(&held).proposed,
            None,
            "the held scan restarts the dwell"
        );

        let (s, a) = step_n(&held, 2, &[(GH, 10)], ALL, t0);
        assert!(
            a.iter().all(Vec::is_empty),
            "two scans are not three: {a:?}"
        );
        let (s, a) = step_with(&s, &[(GH, 10)], ALL, t0);
        assert_eq!(
            (s, a),
            (OverBound::Within, vec![Action::Clear, Action::Info])
        );
    }

    /// Review round 3, A8: a kind-set change proposed while a kind is held
    /// is adopted only when the same set is seen three scans in a row, and
    /// the adopted state still knows which kind is held.
    #[test]
    fn a_kind_set_change_while_a_kind_is_held_settles_like_any_other() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (two, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 300)], ALL, t0);
        let partial = [(GH, 12), (QR, 200)];

        let (s, a) = step_n(&two, 2, &partial, &[QR], t0);
        assert!(a.iter().all(Vec::is_empty));
        assert_eq!(over(&s).proposed, Some((set(&[GH]), 2)));
        // The old set comes back once: the proposal is dropped.
        let (s, a) = step_with(&s, &[(GH, 12), (QR, 300)], &[QR], t0);
        assert_eq!(a, vec![]);
        assert_eq!(over(&s).proposed, None);

        let (s, a) = step_n(&s, 3, &partial, &[QR], t0);
        assert_eq!(a, vec![vec![], vec![], vec![Action::Emit, Action::Warn]]);
        assert_eq!(over(&s).kinds, set(&[GH]));
        assert_eq!(over(&s).held, set(&[GH]));
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
        let (s, a) = step_n(&two, 3, &[(GH, 12), (QR, 200)], &[QR], t1);
        assert_eq!(a.last(), Some(&vec![Action::Emit, Action::Warn]));
        let o = over(&s);
        assert_eq!(o.kinds, set(&[GH]));
        assert_eq!(o.held, set(&[GH]));
        assert_eq!(o.last_counts.get(GH), Some(&300));
        let e = over_bound_event("h", Some(&counts(&[(GH, 12), (QR, 200)])), o, t1);
        assert_eq!(
            e.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "cleared": false,
                "scan_answered": true,
                "bound": 256,
                "observed": {"qontinui_runner": 200},
                "held_kinds": ["gh_actions_runner"],
                "held_last_counts": {"gh_actions_runner": 300},
                "truncated_kinds": ["gh_actions_runner"],
                "reemit_after_secs": 90000,
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
        let (s, a) = step_n(&s, 3, &[(GH, 256)], ALL, t0);
        assert_eq!(a, vec![vec![], vec![], vec![Action::Clear, Action::Info]]);
        assert_eq!(s, OverBound::Within);
    }

    /// Review round 3, A4: two kinds of near-equal size trading the
    /// "largest" slot every tick flip the cut kind every tick; the adopted
    /// kind set does not follow, so there is no event or WARN per tick.
    #[test]
    fn two_kinds_trading_the_largest_slot_do_not_flap() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (mut s, a) = step_with(&OverBound::Within, &[(GH, 200), (QR, 200)], ALL, t0);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        let first = over(&s).kinds.clone();
        for i in 1..=20 {
            let c = if i % 2 == 1 {
                [(GH, 201), (QR, 200)]
            } else {
                [(GH, 200), (QR, 200)]
            };
            let (next, a) = step_with(&s, &c, ALL, t0);
            assert_eq!(a, vec![], "tick {i}");
            assert_eq!(over(&next).kinds, first, "tick {i}");
            s = next;
        }
    }

    /// Review round 2, A3: a kind no instrument can report complete is never
    /// held by the partial-scan rule, or it could never leave.
    #[test]
    fn a_kind_no_scan_can_complete_is_not_held() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (s, _) = step_with(&OverBound::Within, &[(OTHER, 300)], ALL, t0);
        assert_eq!(over(&s).kinds, set(&[OTHER]));
        let (s, a) = step_n(&s, 3, &[(OTHER, 10)], ALL, t0);
        assert_eq!(a, vec![vec![], vec![], vec![Action::Clear, Action::Info]]);
        assert_eq!(s, OverBound::Within);
    }

    /// Review round 3, A5: completability is per instrument — the Windows
    /// `sc` listing never reports the runner kind complete, so a runner kind
    /// over the bound there is decided by its count, not held for ever.
    #[test]
    fn only_kinds_this_instrument_can_complete_are_held() {
        let t0 = at("2026-11-14T00:00:00Z");
        let sc: &[&str] = &["gh_actions_runner"];
        let (mut s, _) = step_by(&OverBound::Within, &[(QR, 300)], sc, sc, t0);
        let mut acts = Vec::new();
        for _ in 0..3 {
            let (next, a) = step_by(&s, &[(QR, 10)], sc, sc, t0);
            assert!(!matches!(&next, OverBound::Over(o) if !o.held.is_empty()));
            acts.push(a);
            s = next;
        }
        assert_eq!(
            acts,
            vec![vec![], vec![], vec![Action::Clear, Action::Info]]
        );
        // The Linux instrument CAN complete it, so there it is held.
        let (o, _) = step_with(&OverBound::Within, &[(QR, 300)], ALL, t0);
        let (held, _) = step_with(&o, &[(QR, 10)], &[GH], t0);
        assert_eq!(over(&held).held, set(&[QR]));
    }

    #[test]
    fn the_events_carry_the_bound_the_counts_and_the_truncated_kinds() {
        let now = at("2026-11-14T00:00:00Z");
        let (s, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 3)], ALL, now);
        let e = over_bound_event("h", Some(&counts(&[(GH, 300), (QR, 3)])), over(&s), now);
        assert_eq!(e.kind, "telemetry_gap");
        assert_eq!(e.observed_at, "2026-11-14T00:00:00Z");
        assert_eq!(
            e.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "cleared": false,
                "scan_answered": true,
                "bound": 256,
                "observed": {"gh_actions_runner": 300, "qontinui_runner": 3},
                "held_kinds": [],
                "held_last_counts": {},
                "truncated_kinds": ["gh_actions_runner"],
                "reemit_after_secs": 90000,
            })
        );

        let c = cleared_event("h", &counts(&[(GH, 10)]), now);
        assert_eq!(c.kind, "telemetry_gap");
        assert_ne!(
            c.client_event_id, e.client_event_id,
            "same instant, distinct ids"
        );
        assert_eq!(
            c.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "cleared": true,
                "scan_answered": true,
                "bound": 256,
                "observed": {"gh_actions_runner": 10},
                "held_kinds": [],
                "held_last_counts": {},
                "truncated_kinds": [],
                "reemit_after_secs": null,
            })
        );
    }

    /// Review round 4, A1: a slow drift across the bound (three scans over,
    /// three under, again and again) emits its event on every entry, but
    /// WARNs once per kind set per day.
    #[test]
    fn slow_oscillation_warns_once_per_day_and_still_emits_every_entry() {
        let t0 = at("2026-11-14T00:00:00Z");
        let mut s = OverBound::Within;
        let mut limit = WarnLimit::default();
        let (mut emits, mut warns, mut infos) = (0, 0, 0);
        let mut cycle = |s: &mut OverBound, now: chrono::DateTime<chrono::Utc>| {
            for c in [[(GH, 300)]; 3].into_iter().chain([[(GH, 10)]; 3]) {
                let (next, actions) = step_with(s, &c, ALL, now);
                for a in actions {
                    match (a, &next) {
                        (Action::Emit, _) => emits += 1,
                        (Action::Warn, OverBound::Over(o)) if limit.admit(&o.kinds, now) => {
                            warns += 1
                        }
                        (Action::Info, _) if limit.info_due() => infos += 1,
                        _ => {}
                    }
                }
                *s = next;
            }
        };
        for i in 0..20 {
            cycle(&mut s, t0 + chrono::Duration::seconds(i * 180));
        }
        assert_eq!(emits, 20, "every entry is on coord's read surface");
        assert_eq!(warns, 1, "one WARN for the day");
        assert_eq!(infos, 1, "and one INFO closing that entry");

        // A day later the same kind set WARNs again.
        let mut again = WarnLimit::default();
        let kinds = set(&[GH]);
        assert!(again.admit(&kinds, t0));
        assert!(!again.admit(&kinds, at("2026-11-14T23:59:59Z")));
        assert!(again.admit(&kinds, at("2026-11-15T00:00:00Z")));
        // A different kind set is not limited by the first.
        assert!(again.admit(&set(&[GH, QR]), at("2026-11-15T00:00:01Z")));
    }

    /// Review round 4, A2: a scan outage longer than the re-emit period still
    /// re-emits, with every kind held at its last vouched count, so the
    /// event never ages out while the computer may still be over.
    #[test]
    fn a_scan_outage_still_reemits_with_every_kind_held() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (o, _) = step_with(&OverBound::Within, &[(GH, 300)], ALL, t0);
        let (quiet, a) = o.step(None, at("2026-11-14T23:59:59Z"));
        assert_eq!((&quiet, a), (&o, vec![]));
        let day = at("2026-11-15T00:00:00Z");
        let (s, a) = quiet.step(None, day);
        assert_eq!(a, vec![Action::Emit]);
        assert_eq!(over(&s).held, set(&[GH]));
        assert_eq!(over(&s).emitted_at, day);
        let e = over_bound_event("h", None, over(&s), day);
        assert_eq!(e.detail["scan_answered"], false);
        assert_eq!(e.detail["observed"], serde_json::json!({}));
        assert_eq!(
            e.detail["held_last_counts"],
            serde_json::json!({"gh_actions_runner": 300})
        );
        assert_eq!(
            e.detail["truncated_kinds"],
            serde_json::json!(["gh_actions_runner"])
        );
    }

    /// Review round 4, A3: a complete scan that sees a kind at zero rows
    /// vouches for zero, rather than leaving the old count standing.
    #[test]
    fn a_complete_scan_with_no_rows_of_a_kind_vouches_for_zero() {
        let t0 = at("2026-11-14T00:00:00Z");
        let (two, _) = step_with(&OverBound::Within, &[(GH, 300), (QR, 300)], ALL, t0);
        assert_eq!(over(&two).last_counts.get(QR), Some(&300));
        let (s, _) = step_with(&two, &[(GH, 300)], ALL, t0);
        assert_eq!(over(&s).last_counts.get(QR), Some(&0));
        assert_eq!(over(&s).last_counts.get(GH), Some(&300));
    }
}
