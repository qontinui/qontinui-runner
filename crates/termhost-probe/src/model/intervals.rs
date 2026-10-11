//! Fixed observation intervals (500 ms, the catalog's `hangs` sample unit) and
//! which of them are VALID.
//!
//! An interval is valid only when every window-state sample in it positively
//! showed the measured window foreground, visible, uncloaked, not minimized
//! and unoccluded, AND the real-input monitor was running and saw no input
//! that was not the probe's own. Anything it could not establish (no sample, a
//! query that failed, a monitor that is down or absent) makes it invalid —
//! policy verification-and-evidence/unknown-must-not-render-as-a-default
//! applied to a validity flag. Plan Phase 2: "In every arm, per sample
//! interval, record whether the measured window is foreground, uncloaked and
//! unoccluded ... failing intervals are excluded".
//!
//! An excluded interval counts toward one catalog reason, in this precedence:
//! `operator_activity` (real or foreign input — usually WHY focus moved),
//! then `occluded` (the window was shown not foreground, hidden, cloaked,
//! minimized or covered), then `harness_error` (the probe could not establish
//! the window's state or the operator's absence: no sample, a failed query, a
//! listener that is down or absent). A failure of the probe is never reported
//! as something the operator or another window did.

use std::collections::{BTreeMap, BTreeSet};

use super::{InputObs, InputOrigin, WindowSample};
use crate::records::{InputCounts, IntervalRecord, Ns, OccluderInfo};

/// The catalog reason an excluded interval counts toward.
pub const CLASS_OCCLUDED: &str = "occluded";
pub const CLASS_OPERATOR: &str = "operator_activity";
pub const CLASS_HARNESS: &str = "harness_error";

/// Whether a real-input monitor is configured for the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorPresence {
    Configured,
    Absent,
}

#[derive(Clone, Debug, Default)]
struct Acc {
    samples: u32,
    not_foreground: u32,
    not_visible: u32,
    cloaked: u32,
    minimized: u32,
    occluded: u32,
    state_unknown: u32,
    max_occluded_fraction: Option<f64>,
    occluders: BTreeSet<OccluderInfo>,
    hung: u32,
    real_input: InputCounts,
    foreign_injected: u32,
    probe_input: u32,
    frames: u32,
}

impl Acc {
    fn add_sample(&mut self, s: &WindowSample) {
        self.samples += 1;
        let mut unknown = false;
        let mut tally = |state: Option<bool>, bad_when: bool, counter: &mut u32| match state {
            Some(v) if v == bad_when => *counter += 1,
            Some(_) => {}
            None => unknown = true,
        };
        tally(s.foreground, false, &mut self.not_foreground);
        tally(s.visible, false, &mut self.not_visible);
        tally(s.cloaked, true, &mut self.cloaked);
        tally(s.minimized, true, &mut self.minimized);
        match s.occluded_fraction {
            Some(f) if f > 0.0 => {
                self.occluded += 1;
                self.max_occluded_fraction =
                    Some(self.max_occluded_fraction.map_or(f, |m| m.max(f)));
            }
            Some(_) => {
                self.max_occluded_fraction.get_or_insert(0.0);
            }
            None => unknown = true,
        }
        if unknown {
            self.state_unknown += 1;
        }
        for o in s.occluders.iter().take(8) {
            if self.occluders.len() < 5 {
                self.occluders.insert(o.clone());
            }
        }
        if s.hung == Some(true) {
            self.hung += 1;
        }
    }

    fn add_input(&mut self, e: &InputObs) {
        match e.origin {
            InputOrigin::Probe => self.probe_input += 1,
            InputOrigin::ForeignInjected => self.foreign_injected += 1,
            InputOrigin::Real => match e.device {
                super::InputDevice::Keyboard => self.real_input.keyboard += 1,
                super::InputDevice::Pointer => self.real_input.pointer += 1,
                super::InputDevice::Other => self.real_input.other += 1,
            },
        }
    }
}

/// The validity verdict over the full intervals of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub valid: u64,
    pub occluded: u64,
    pub operator: u64,
    pub harness: u64,
}

impl Tally {
    pub fn total(&self) -> u64 {
        self.valid + self.excluded()
    }

    pub fn excluded(&self) -> u64 {
        self.occluded + self.operator + self.harness
    }

    /// The reason the most excluded intervals count toward (ties go to the
    /// earlier class in the module's precedence); `None` when none was
    /// excluded.
    pub fn dominant_class(&self) -> Option<&'static str> {
        [
            (self.operator, CLASS_OPERATOR),
            (self.occluded, CLASS_OCCLUDED),
            (self.harness, CLASS_HARNESS),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .fold(
            None,
            |best: Option<(u64, &'static str)>, (n, c)| match best {
                Some((m, _)) if m >= n => best,
                _ => Some((n, c)),
            },
        )
        .map(|(_, c)| c)
    }
}

/// The interval accountant.
#[derive(Clone, Debug)]
pub struct Intervals {
    start_ns: u64,
    len_ns: u64,
    monitor: MonitorPresence,
    open: BTreeMap<u64, Acc>,
    next_to_close: u64,
    closed: Vec<IntervalRecord>,
    /// Periods the monitor was down: (from, until).
    monitor_down: Vec<(u64, Option<u64>)>,
}

impl Intervals {
    pub fn new(start_ns: u64, len_ns: u64, monitor: MonitorPresence) -> Self {
        assert!(len_ns > 0);
        Intervals {
            start_ns,
            len_ns,
            monitor,
            open: BTreeMap::new(),
            next_to_close: 0,
            closed: Vec::new(),
            monitor_down: Vec::new(),
        }
    }

    pub fn len_ns(&self) -> u64 {
        self.len_ns
    }

    /// The interval index holding `t`, or `None` before the run started or in
    /// an interval already closed.
    fn index_of(&self, t_ns: u64) -> Option<u64> {
        if t_ns < self.start_ns {
            return None;
        }
        let i = (t_ns - self.start_ns) / self.len_ns;
        (i >= self.next_to_close).then_some(i)
    }

    /// `[start, end)` of interval `i`.
    pub fn bounds(&self, i: u64) -> (u64, u64) {
        let a = self.start_ns + i * self.len_ns;
        (a, a + self.len_ns)
    }

    pub fn add_sample(&mut self, s: &WindowSample) {
        if let Some(i) = self.index_of(s.t_ns) {
            self.open.entry(i).or_default().add_sample(s);
        }
    }

    /// Count an input event. Non-probe input that arrives after its interval
    /// closed still taints it: the closed record is changed, and returned
    /// (marked `amended`) when its verdict changed, for the caller to re-emit.
    pub fn add_input(&mut self, e: &InputObs) -> Option<IntervalRecord> {
        if let Some(i) = self.index_of(e.t_ns) {
            self.open.entry(i).or_default().add_input(e);
            return None;
        }
        if e.origin == InputOrigin::Probe {
            return None;
        }
        let rec = self.closed_at_mut(e.t_ns)?;
        let before = (rec.valid, rec.exclusion_class);
        match e.origin {
            InputOrigin::ForeignInjected => {
                rec.foreign_injected += 1;
                if !rec.excluded.contains(&"foreign_injection") {
                    rec.excluded.push("foreign_injection");
                }
            }
            _ => {
                match e.device {
                    super::InputDevice::Keyboard => rec.real_input.keyboard += 1,
                    super::InputDevice::Pointer => rec.real_input.pointer += 1,
                    super::InputDevice::Other => rec.real_input.other += 1,
                }
                if !rec.excluded.contains(&"operator_input") {
                    rec.excluded.push("operator_input");
                }
            }
        }
        rec.valid = false;
        rec.exclusion_class = Some(CLASS_OPERATOR);
        if (rec.valid, rec.exclusion_class) == before {
            return None;
        }
        rec.amended = true;
        Some(rec.clone())
    }

    fn closed_at_mut(&mut self, t_ns: u64) -> Option<&mut IntervalRecord> {
        if t_ns < self.start_ns {
            return None;
        }
        let i = (t_ns - self.start_ns) / self.len_ns;
        self.closed.iter_mut().find(|r| r.index == i)
    }

    pub fn note_frame(&mut self, t_ns: u64) {
        if let Some(i) = self.index_of(t_ns) {
            self.open.entry(i).or_default().frames += 1;
        }
    }

    /// The monitor went down (or came back) at `t`.
    pub fn note_monitor(&mut self, t_ns: u64, healthy: bool) {
        match (healthy, self.monitor_down.last_mut()) {
            (false, Some((_, None))) => {}
            (false, _) => self.monitor_down.push((t_ns, None)),
            (true, Some((_, until @ None))) => *until = Some(t_ns),
            (true, _) => {}
        }
    }

    fn monitor_state(&self, a: u64, b: u64) -> &'static str {
        if self.monitor == MonitorPresence::Absent {
            return "absent";
        }
        let down = self
            .monitor_down
            .iter()
            .any(|&(from, until)| from < b && until.is_none_or(|u| u > a));
        if down {
            "down"
        } else {
            "ok"
        }
    }

    fn record(&self, i: u64, acc: &Acc, partial_end: Option<u64>) -> IntervalRecord {
        let (a, full_b) = self.bounds(i);
        let b = partial_end.unwrap_or(full_b);
        let monitor = self.monitor_state(a, b);
        let mut excluded = Vec::new();
        let mut harness_failed = false;
        if acc.samples == 0 {
            excluded.push("no_samples");
            harness_failed = true;
        }
        let mut visibility_failed = false;
        for (n, why) in [
            (acc.not_foreground, "not_foreground"),
            (acc.not_visible, "not_visible"),
            (acc.cloaked, "cloaked"),
            (acc.minimized, "minimized"),
            (acc.occluded, "occluded"),
        ] {
            if n > 0 {
                excluded.push(why);
                visibility_failed = true;
            }
        }
        if acc.state_unknown > 0 {
            excluded.push("state_unknown");
            harness_failed = true;
        }
        let mut operator_failed = false;
        if acc.real_input.total() > 0 {
            excluded.push("operator_input");
            operator_failed = true;
        }
        if acc.foreign_injected > 0 {
            excluded.push("foreign_injection");
            operator_failed = true;
        }
        match monitor {
            "down" => {
                excluded.push("monitor_down");
                harness_failed = true;
            }
            "absent" => {
                excluded.push("monitor_absent");
                harness_failed = true;
            }
            _ => {}
        }
        let exclusion_class = if operator_failed {
            Some(CLASS_OPERATOR)
        } else if visibility_failed {
            Some(CLASS_OCCLUDED)
        } else if harness_failed {
            Some(CLASS_HARNESS)
        } else {
            None
        };
        IntervalRecord {
            index: i,
            t_start_ns: Ns(a),
            t_end_ns: Ns(b),
            partial: partial_end.is_some(),
            samples: acc.samples,
            not_foreground: acc.not_foreground,
            not_visible: acc.not_visible,
            cloaked: acc.cloaked,
            minimized: acc.minimized,
            occluded: acc.occluded,
            state_unknown: acc.state_unknown,
            max_occluded_fraction: acc.max_occluded_fraction,
            occluders: acc.occluders.iter().cloned().collect(),
            hung: acc.hung,
            real_input: acc.real_input,
            foreign_injected: acc.foreign_injected,
            probe_input: acc.probe_input,
            monitor,
            frames: acc.frames,
            valid: exclusion_class.is_none(),
            excluded,
            exclusion_class,
            amended: false,
        }
    }

    /// Close every interval that ends at or before `t_close`, returning their
    /// records in order.
    pub fn close_until(&mut self, t_close: u64) -> Vec<IntervalRecord> {
        let mut out = Vec::new();
        loop {
            let (_, b) = self.bounds(self.next_to_close);
            if b > t_close {
                break;
            }
            let i = self.next_to_close;
            let acc = self.open.remove(&i).unwrap_or_default();
            let rec = self.record(i, &acc, None);
            self.closed.push(rec.clone());
            out.push(rec);
            self.next_to_close += 1;
        }
        out
    }

    /// Close everything up to `t_end`; a trailing partial interval is recorded
    /// with `partial: true` and never counted.
    pub fn finish(&mut self, t_end: u64) -> Vec<IntervalRecord> {
        let mut out = self.close_until(t_end);
        let (a, _) = self.bounds(self.next_to_close);
        if t_end > a {
            let i = self.next_to_close;
            let acc = self.open.remove(&i).unwrap_or_default();
            let rec = self.record(i, &acc, Some(t_end));
            self.closed.push(rec.clone());
            out.push(rec);
            self.next_to_close += 1;
        }
        out
    }

    pub fn closed(&self) -> &[IntervalRecord] {
        &self.closed
    }

    /// Full (non-partial) closed intervals.
    pub fn full(&self) -> impl Iterator<Item = &IntervalRecord> {
        self.closed.iter().filter(|r| !r.partial)
    }

    /// The end of the last closed full interval, if any.
    pub fn last_full_end(&self) -> Option<u64> {
        self.full().map(|r| r.t_end_ns.0).max()
    }

    /// True when every closed interval overlapping `[a, b]` is valid and not
    /// partial; false when any is invalid or the span is not fully closed.
    pub fn span_valid(&self, a: u64, b: u64) -> bool {
        if b < self.start_ns {
            return false;
        }
        let first = a.saturating_sub(self.start_ns) / self.len_ns;
        let last = (b - self.start_ns) / self.len_ns;
        (first..=last).all(|i| {
            self.closed
                .iter()
                .find(|r| r.index == i)
                .is_some_and(|r| r.valid && !r.partial)
        })
    }

    /// The validity verdict over the run's full intervals.
    pub fn tally(&self) -> Tally {
        let mut t = Tally::default();
        for r in self.full() {
            match r.exclusion_class {
                None => t.valid += 1,
                Some(CLASS_OCCLUDED) => t.occluded += 1,
                Some(CLASS_OPERATOR) => t.operator += 1,
                Some(_) => t.harness += 1,
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InputDevice;

    const MS: u64 = 1_000_000;

    fn good(t: u64) -> WindowSample {
        WindowSample {
            t_ns: t,
            foreground: Some(true),
            visible: Some(true),
            cloaked: Some(false),
            minimized: Some(false),
            occluded_fraction: Some(0.0),
            occluders: vec![],
            hung: Some(false),
        }
    }

    fn feed(iv: &mut Intervals, from: u64, to: u64, f: impl Fn(u64) -> WindowSample) {
        let mut t = from;
        while t < to {
            iv.add_sample(&f(t));
            t += 100 * MS;
        }
    }

    #[test]
    fn positively_good_intervals_are_valid() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 2000 * MS, good);
        let recs = iv.close_until(2000 * MS);
        assert_eq!(recs.len(), 4);
        assert!(recs.iter().all(|r| r.valid), "{recs:?}");
        assert_eq!(iv.tally().valid, 4);
        assert_eq!(iv.tally().excluded(), 0);
    }

    #[test]
    fn occlusion_and_focus_loss_exclude_as_occluded() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 1000 * MS, |t| {
            let mut s = good(t);
            if t >= 500 * MS {
                s.occluded_fraction = Some(0.4);
                s.occluders = vec![OccluderInfo {
                    class: "Notepad".into(),
                    pid: 7,
                }];
            }
            s
        });
        let recs = iv.close_until(1000 * MS);
        assert!(recs[0].valid);
        assert!(!recs[1].valid);
        assert_eq!(recs[1].exclusion_class, Some(CLASS_OCCLUDED));
        assert_eq!(recs[1].max_occluded_fraction, Some(0.4));
        assert_eq!(recs[1].occluders.len(), 1);
    }

    #[test]
    fn unknown_state_is_never_valid_and_blames_the_harness() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 500 * MS, |t| WindowSample {
            occluded_fraction: None,
            ..good(t)
        });
        let recs = iv.close_until(1000 * MS);
        assert!(!recs[0].valid);
        assert!(recs[0].excluded.contains(&"state_unknown"));
        assert_eq!(recs[0].exclusion_class, Some(CLASS_HARNESS));
        // No samples at all is invalid too — the probe could not look.
        assert!(!recs[1].valid);
        assert!(recs[1].excluded.contains(&"no_samples"));
        assert_eq!(recs[1].exclusion_class, Some(CLASS_HARNESS));
    }

    #[test]
    fn an_unestablished_cloak_or_minimize_state_counts_against_the_interval() {
        for sample in [
            |t| WindowSample {
                cloaked: None,
                ..good(t)
            },
            |t| WindowSample {
                minimized: None,
                ..good(t)
            },
        ] as [fn(u64) -> WindowSample; 2]
        {
            let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
            feed(&mut iv, 0, 500 * MS, sample);
            let recs = iv.close_until(500 * MS);
            assert!(!recs[0].valid, "{:?}", recs[0]);
            assert!(recs[0].excluded.contains(&"state_unknown"));
        }
    }

    #[test]
    fn real_input_excludes_as_operator_activity_and_probe_input_does_not() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 1000 * MS, good);
        iv.add_input(&InputObs {
            t_ns: 100 * MS,
            device: InputDevice::Keyboard,
            origin: InputOrigin::Probe,
        });
        iv.add_input(&InputObs {
            t_ns: 700 * MS,
            device: InputDevice::Pointer,
            origin: InputOrigin::Real,
        });
        let recs = iv.close_until(1000 * MS);
        assert!(recs[0].valid);
        assert_eq!(recs[0].probe_input, 1);
        assert!(!recs[1].valid);
        assert_eq!(recs[1].exclusion_class, Some(CLASS_OPERATOR));
    }

    #[test]
    fn late_real_input_invalidates_a_closed_interval_and_says_so() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 500 * MS, good);
        assert!(iv.close_until(500 * MS)[0].valid);
        let amended = iv
            .add_input(&InputObs {
                t_ns: 200 * MS,
                device: InputDevice::Keyboard,
                origin: InputOrigin::Real,
            })
            .expect("the verdict changed, so an amended record is returned");
        assert!(amended.amended && !amended.valid);
        assert_eq!(amended.exclusion_class, Some(CLASS_OPERATOR));
        assert!(!iv.closed()[0].valid);
        assert_eq!(iv.tally().operator, 1);
        // More late input of the same kind changes nothing further.
        assert!(iv
            .add_input(&InputObs {
                t_ns: 300 * MS,
                device: InputDevice::Keyboard,
                origin: InputOrigin::Real,
            })
            .is_none());
    }

    #[test]
    fn an_absent_or_failed_monitor_is_a_harness_failure_not_the_operator() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Absent);
        feed(&mut iv, 0, 500 * MS, good);
        let recs = iv.close_until(500 * MS);
        assert_eq!(recs[0].monitor, "absent");
        assert_eq!(recs[0].exclusion_class, Some(CLASS_HARNESS));

        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 1500 * MS, good);
        iv.note_monitor(600 * MS, false);
        iv.note_monitor(900 * MS, true);
        let recs = iv.close_until(1500 * MS);
        assert!(recs[0].valid);
        assert_eq!(recs[1].monitor, "down");
        assert_eq!(recs[1].exclusion_class, Some(CLASS_HARNESS));
        assert!(recs[2].valid);
    }

    #[test]
    fn a_trailing_partial_interval_is_recorded_not_counted() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 700 * MS, good);
        let recs = iv.finish(700 * MS);
        assert_eq!(recs.len(), 2);
        assert!(recs[1].partial);
        assert_eq!(iv.tally().valid, 1);
        assert_eq!(iv.last_full_end(), Some(500 * MS));
    }

    #[test]
    fn span_validity() {
        let mut iv = Intervals::new(0, 500 * MS, MonitorPresence::Configured);
        feed(&mut iv, 0, 1500 * MS, |t| {
            let mut s = good(t);
            if (500 * MS..1000 * MS).contains(&t) {
                s.foreground = Some(false);
            }
            s
        });
        iv.close_until(1500 * MS);
        assert!(iv.span_valid(0, 400 * MS));
        assert!(!iv.span_valid(400 * MS, 1100 * MS));
        assert!(iv.span_valid(1000 * MS, 1400 * MS));
        assert!(!iv.span_valid(1000 * MS, 1600 * MS), "not yet closed");
    }

    #[test]
    fn the_dominant_exclusion_class_follows_counts_then_precedence() {
        let t = Tally {
            valid: 1,
            occluded: 2,
            operator: 2,
            harness: 9,
        };
        assert_eq!(t.dominant_class(), Some(CLASS_HARNESS));
        let t = Tally {
            valid: 1,
            occluded: 2,
            operator: 2,
            harness: 0,
        };
        assert_eq!(t.dominant_class(), Some(CLASS_OPERATOR));
        assert_eq!(Tally::default().dominant_class(), None);
    }
}
