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
//! The cut and the over-bound decision are ONE computation
//! ([`cut_to_bound`]), so the event can never name a kind the report did not
//! truncate. The decision is taken from the OBSERVED scan, never from a
//! delta's own (smaller) list: a delta of 44 changed rows says nothing about
//! whether the computer still watches 300 units.

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

/// One computer's over-bound state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum OverBound {
    #[default]
    Within,
    Over {
        /// The kinds the last emitted event named.
        kinds: BTreeSet<&'static str>,
        /// When that event was queued (in-memory: a reporter rebuild re-emits
        /// on entering, which can add an event but never lose one).
        emitted_at: chrono::DateTime<chrono::Utc>,
    },
}

impl OverBound {
    /// Advance on one tick's observation. `cut` is [`services_cut`] of the
    /// observed scan, or `None` when the scan produced no service information
    /// — which keeps the previous state and does nothing. PURE.
    pub(crate) fn step(
        &self,
        cut: Option<&BTreeSet<&'static str>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (OverBound, Vec<Action>) {
        let Some(cut) = cut else {
            return (self.clone(), Vec::new());
        };
        let entered = || OverBound::Over {
            kinds: cut.clone(),
            emitted_at: now,
        };
        match self {
            OverBound::Within if cut.is_empty() => (OverBound::Within, Vec::new()),
            OverBound::Within => (entered(), vec![Action::Emit, Action::Warn]),
            OverBound::Over { .. } if cut.is_empty() => (OverBound::Within, vec![Action::Info]),
            OverBound::Over { kinds, .. } if kinds != cut => {
                (entered(), vec![Action::Emit, Action::Warn])
            }
            OverBound::Over { emitted_at, .. }
                if (now - *emitted_at).num_seconds() >= REEMIT_AFTER_SECS =>
            {
                (entered(), vec![Action::Emit])
            }
            OverBound::Over { .. } => (self.clone(), Vec::new()),
        }
    }
}

/// The `telemetry_gap` saying this computer's service list is capped at
/// coord's bound: the bound, the observed sendable count per kind, and the
/// kinds the report truncates (whose removals coord does not apply while they
/// stay over). PURE.
pub(crate) fn over_bound_event(
    identity: &str,
    observed: &BTreeMap<&'static str, usize>,
    truncated: &BTreeSet<&'static str>,
    now: chrono::DateTime<chrono::Utc>,
) -> ComputerEvent {
    let at = events::rfc3339(now);
    ComputerEvent {
        client_event_id: events::event_id("telemetry_gap", &[OVER_BOUND_REASON, identity, &at]),
        kind: "telemetry_gap".into(),
        observed_at: at,
        detail: serde_json::json!({
            "reason": OVER_BOUND_REASON,
            "bound": MAX_SERVICES_PER_COMPUTER,
            "observed": observed,
            "truncated_kinds": truncated,
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

    #[test]
    fn the_over_bound_machine_emits_on_enter_change_and_daily_and_informs_on_return() {
        let t0 = at("2026-11-14T00:00:00Z");
        let gh_only = set(&["gh_actions_runner"]);
        let both = set(&["gh_actions_runner", "qontinui_runner"]);
        let none = BTreeSet::new();

        // Within, staying within: nothing.
        let (s, a) = OverBound::Within.step(Some(&none), t0);
        assert_eq!((s, a), (OverBound::Within, vec![]));

        // Enter: event + WARN.
        let (s1, a) = OverBound::Within.step(Some(&gh_only), t0);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);
        assert_eq!(
            s1,
            OverBound::Over {
                kinds: gh_only.clone(),
                emitted_at: t0
            }
        );

        // Same kinds on the next tick: nothing.
        let t1 = at("2026-11-14T00:00:30Z");
        let (s2, a) = s1.step(Some(&gh_only), t1);
        assert_eq!(a, vec![]);
        assert_eq!(s2, s1, "the emit clock does not move without an emit");

        // Changed kinds: event + WARN.
        let (s3, a) = s2.step(Some(&both), t1);
        assert_eq!(a, vec![Action::Emit, Action::Warn]);

        // Just under 24 h later: nothing; at 24 h: event, no WARN.
        let (s4, a) = s3.step(Some(&both), at("2026-11-14T23:59:59Z"));
        assert_eq!(a, vec![]);
        let day = at("2026-11-15T00:00:30Z");
        let (s5, a) = s4.step(Some(&both), day);
        assert_eq!(a, vec![Action::Emit]);
        assert_eq!(
            s5,
            OverBound::Over {
                kinds: both.clone(),
                emitted_at: day
            }
        );

        // Back within: INFO, no event.
        let (s6, a) = s5.step(Some(&none), day);
        assert_eq!((s6, a), (OverBound::Within, vec![Action::Info]));

        // Over → no scan → over: the gap holds the state, so no INFO and no
        // second event.
        let (o, _) = OverBound::Within.step(Some(&gh_only), t0);
        let (held, a) = o.step(None, t1);
        assert_eq!(a, vec![]);
        assert_eq!(held, o);
        let (again, a) = held.step(Some(&gh_only), t1);
        assert_eq!(a, vec![]);
        assert_eq!(again, o);
    }

    #[test]
    fn the_event_carries_the_bound_the_counts_and_the_truncated_kinds() {
        let now = at("2026-11-14T00:00:00Z");
        let e = over_bound_event(
            "h",
            &BTreeMap::from([("gh_actions_runner", 300), ("qontinui_runner", 3)]),
            &set(&["gh_actions_runner"]),
            now,
        );
        assert_eq!(e.kind, "telemetry_gap");
        assert_eq!(e.observed_at, "2026-11-14T00:00:00Z");
        assert_eq!(
            e.detail,
            serde_json::json!({
                "reason": "services_over_bound",
                "bound": 256,
                "observed": {"gh_actions_runner": 300, "qontinui_runner": 3},
                "truncated_kinds": ["gh_actions_runner"],
                "reemit_after_secs": 86400,
            })
        );
    }
}
