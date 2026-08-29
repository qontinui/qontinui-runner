//! Graceful degradation for a column the live database may not have yet.
//!
//! `project.*` schema is authored by qontinui-web's alembic chain, not by the
//! runner, so a runner build can reach production before the migration that
//! adds a column it references has DEPLOYED. A statement naming a missing
//! column fails with SQLSTATE `42703` (`undefined_column`), which for the
//! workflow journals would turn "we cannot record a fingerprint yet" into
//! "the step was never journalled at all" — strictly worse than the defect
//! being fixed.
//!
//! [`OptionalColumn`] makes that failure a *degradation* instead: the first
//! `42703` is remembered, subsequent statements are issued in their
//! column-free form, and the memory EXPIRES so a runner that was started
//! before the migration deployed picks the column up again without a restart.
//!
//! Two properties are deliberate:
//!
//! * **Only a definitive `42703` marks the column absent.** A pool error, a
//!   connection reset or any other SQLSTATE leaves the belief untouched, so a
//!   transient blip cannot permanently disable the column for the process.
//! * **Absence expires.** Caching "absent" forever would mean a long-lived
//!   runner keeps writing NULL fingerprints for days after the migration
//!   landed. Every NULL is a cache miss (correct but costly), so the belief is
//!   re-tested periodically.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::warn;

/// How long a `42703` suppresses use of the column before it is retried.
const ABSENCE_TTL_MS: i64 = 5 * 60 * 1000;

/// A column that may or may not exist in the live schema.
///
/// Construct one `static` per (table, column) pair — beliefs are per column,
/// because two columns added by the same revision can still be deployed apart
/// if the revision is ever split.
pub struct OptionalColumn {
    label: &'static str,
    /// Unix epoch millis before which the column is believed ABSENT.
    /// `0` means "believed present" (the optimistic default).
    absent_until_ms: AtomicI64,
}

impl OptionalColumn {
    pub const fn new(label: &'static str) -> Self {
        Self {
            label,
            absent_until_ms: AtomicI64::new(0),
        }
    }

    /// Should the next statement name this column?
    ///
    /// Optimistic by default: the healthy case pays no probe query.
    pub fn is_believed_present(&self) -> bool {
        let until = self.absent_until_ms.load(Ordering::Relaxed);
        until == 0 || now_ms() >= until
    }

    /// Classify a statement failure. Returns `true` — and records the absence —
    /// only when Postgres definitively said the column does not exist, in which
    /// case the caller should retry with the column-free statement.
    pub fn note_error(&self, err: &tokio_postgres::Error) -> bool {
        let is_undefined_column = err
            .code()
            .map(|c| *c == tokio_postgres::error::SqlState::UNDEFINED_COLUMN)
            .unwrap_or(false);

        if is_undefined_column && self.record_absence(now_ms()) {
            // Loud once per TTL, not on every statement: this is a real
            // capability loss (replay degrades to "always re-execute"), not a
            // nuisance.
            warn!(
                column = %self.label,
                "Column is absent from the live schema — falling back to the \
                 column-free statement. Replay validation degrades to a MISS \
                 (steps re-execute and re-bill) until the migration deploys."
            );
        }

        is_undefined_column
    }

    /// Record an absence observed at `now`, returning whether this observation
    /// is a fresh transition from "believed present" and therefore worth a log
    /// line.
    ///
    /// Warn on every RE-detection, not only the first one ever. A previous
    /// value of `0` is the never-seen-before case; a non-zero value that has
    /// already elapsed means the TTL expired, the column was optimistically
    /// retried, and Postgres said `42703` again. Both are transitions from
    /// "believed present" to "believed absent", and both deserve a line.
    ///
    /// Gating on `== 0` alone (the original form) made the warning fire exactly
    /// ONCE per process: after the first TTL the counter is never back at `0`,
    /// so a migration that stayed undeployed for days went silent after five
    /// minutes. This keeps it visible at roughly one line per
    /// [`ABSENCE_TTL_MS`] — a natural backoff, not one line per statement.
    ///
    /// The absence/TTL semantics themselves are unchanged: every observation
    /// still pushes the expiry out to `now + ABSENCE_TTL_MS`.
    fn record_absence(&self, now: i64) -> bool {
        let previous_absent_until = self
            .absent_until_ms
            .swap(now.saturating_add(ABSENCE_TTL_MS), Ordering::Relaxed);
        previous_absent_until == 0 || now >= previous_absent_until
    }

    /// Test-only: forget any recorded absence.
    #[cfg(test)]
    pub fn reset(&self) {
        self.absent_until_ms.store(0, Ordering::Relaxed);
    }

    /// Test-only: pretend Postgres reported the column missing.
    #[cfg(test)]
    pub fn mark_absent_for_test(&self) {
        self.absent_until_ms
            .store(now_ms().saturating_add(ABSENCE_TTL_MS), Ordering::Relaxed);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    static COL: OptionalColumn = OptionalColumn::new("test.column");

    #[test]
    fn believed_present_by_default_and_after_reset() {
        COL.reset();
        assert!(
            COL.is_believed_present(),
            "the healthy case must not pay a probe"
        );
    }

    #[test]
    fn recorded_absence_suppresses_the_column_then_expires() {
        let col = OptionalColumn::new("test.expiring");
        assert!(col.is_believed_present());
        col.mark_absent_for_test();
        assert!(!col.is_believed_present());

        // The absence is time-boxed, not permanent: a runner started before the
        // migration deployed must pick the column up without a restart.
        col.absent_until_ms
            .store(now_ms() - 1, std::sync::atomic::Ordering::Relaxed);
        assert!(col.is_believed_present());
    }

    #[test]
    fn absence_warns_once_per_ttl_not_once_per_process() {
        let col = OptionalColumn::new("test.rewarn");
        // Each observation pushes the expiry to `now + TTL` (pre-existing
        // semantics, untouched here), so the timeline is measured from the LAST
        // observation, not from `t0`.
        let mut last = 1_000_000_000_000_i64;

        // First detection: nothing was believed absent yet.
        assert!(col.record_absence(last), "first detection must warn");

        // Inside the TTL, repeated 42703s are the same known outage — silent.
        last += 1;
        assert!(!col.record_absence(last));
        last += ABSENCE_TTL_MS - 1;
        assert!(!col.record_absence(last));

        // Once the TTL has elapsed the column was optimistically retried and
        // Postgres said 42703 again. That is a fresh "believed present ->
        // believed absent" transition and MUST warn again, otherwise a
        // still-undeployed migration goes silent after five minutes.
        last += ABSENCE_TTL_MS;
        assert!(
            col.record_absence(last),
            "re-detection after a TTL expiry must warn again"
        );

        last += 1;
        assert!(!col.record_absence(last));

        // ...and again on the next expiry: this is a per-TTL backoff, not a
        // once-per-process latch.
        last += ABSENCE_TTL_MS;
        assert!(
            col.record_absence(last),
            "the warning must keep recurring while the column stays absent"
        );
    }

    #[test]
    fn record_absence_leaves_ttl_semantics_unchanged() {
        let col = OptionalColumn::new("test.ttl-semantics");
        let t0 = 2_000_000_000_000;

        // Every observation — warning or not — pushes the expiry out to
        // now + TTL. The log fix must not shorten or extend the suppression.
        col.record_absence(t0);
        assert_eq!(
            col.absent_until_ms.load(Ordering::Relaxed),
            t0 + ABSENCE_TTL_MS
        );

        col.record_absence(t0 + 10);
        assert_eq!(
            col.absent_until_ms.load(Ordering::Relaxed),
            t0 + 10 + ABSENCE_TTL_MS
        );
    }
}
