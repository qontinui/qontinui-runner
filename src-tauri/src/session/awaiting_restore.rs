//! Is a lifecycle row still OWED a restore by THIS process?
//!
//! One predicate, [`restore_still_owed`], read by two consumers so they can
//! never disagree about which rows a restore may still bring back (plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`, Phase 4,
//! D3):
//!
//! - the automatic page placement's record-aware loads
//!   ([`crate::terminal::page_placement`]), which counts an owed row on the page
//!   it was recorded on;
//! - the since-restart ledger's `restoreOwed` ([`OwedRestores`], served by
//!   [`crate::session::session_ledger::report`]), which tells a session still
//!   waiting for its restore from one that will not be restored by this boot.
//!
//! A row is owed iff it is
//!
//! - in this process's restore set: [`SessionLifecycleStore::restorable_records`]
//!   under the boot classification `terminal_session_list_open` reads it under
//!   ([`crate::session::shutdown_marker::restore_admission_inputs`]);
//! - unfinished. The restore set already leaves a finished row out; the
//!   predicate says so again rather than rely on its caller;
//! - not closed by this process. An exit since boot is never brought back by a
//!   restore in this process, although the restore set keeps such a row for a
//!   while for the NEXT boot.
//!
//! The answer is tri-state. `None` means it cannot be told: before the boot is
//! classified, which side of the boot a close is on is unknown, and the restore
//! set itself is read under stand-in inputs. Each consumer decides what `None`
//! means for it: placement keeps its old reading (nothing is known to have been
//! closed by this process, so the row still counts), and the ledger serves
//! `null`.
//!
//! This is not [`crate::session::restore_census::BootCensus::awaits_restore`].
//! That is the boot census's hold, which the liveness poll reads to keep a
//! census row open, and it answers a different question.

use std::collections::HashSet;

use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

/// The boot facts the restore set is read under: its two admission inputs, and
/// the instant this process booted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreView {
    pub prior_marker_at: Option<i64>,
    pub boot_was_clean: bool,
    /// `None` before the boot is classified: which side of the boot a close is
    /// on is then unknown.
    pub booted_at_ms: Option<i64>,
}

impl RestoreView {
    /// This process's view: the same classification
    /// `terminal_session_list_open` reads the restore set under.
    pub fn current() -> Self {
        let (prior_marker_at, boot_was_clean) =
            crate::session::shutdown_marker::restore_admission_inputs();
        Self {
            prior_marker_at,
            boot_was_clean,
            booted_at_ms: crate::session::shutdown_marker::boot_classification()
                .map(|c| c.booted_at_ms),
        }
    }

    /// THIS process's restore set, read under this view.
    pub fn restore_set(&self, store: &SessionLifecycleStore) -> Vec<TerminalSessionRecord> {
        store.restorable_records(self.prior_marker_at, self.boot_was_clean)
    }

    /// Was `row` closed by this process, at or after its boot? `None` before
    /// the boot is classified.
    fn closed_since_boot(&self, row: &TerminalSessionRecord) -> Option<bool> {
        let boot = self.booted_at_ms?;
        Some(row.closed_at.is_some_and(|closed| closed >= boot))
    }
}

/// D3 for one row of THIS process's restore set ([`RestoreView::restore_set`]):
///
/// - `Some(true)` while a restore in this process may still bring it back;
/// - `Some(false)` once it is finished, or once this process has closed it;
/// - `None` when that cannot be told, because the boot is not classified yet.
///
/// Membership is the caller's: a row outside the restore set is not owed,
/// whatever this says. A finished row reads `Some(false)` even before the boot
/// is classified, because finished is final for restore.
pub fn restore_still_owed(row: &TerminalSessionRecord, view: &RestoreView) -> Option<bool> {
    if row.finished_at.is_some() {
        return Some(false);
    }
    view.closed_since_boot(row).map(|closed| !closed)
}

/// D3 over the live store, by session: the sessions a restore in THIS process
/// still owes. This is the ledger's `restoreOwed`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwedRestores {
    /// The lowercased ids still owed. `None` when that cannot be read: there
    /// is no store, the boot is not classified yet, or a row of the restore
    /// set could not be judged. Then an id missing from the set could be that
    /// row's, so no id can be answered.
    owed: Option<HashSet<String>>,
}

impl OwedRestores {
    /// Read from `store` under `view`. With no store, every answer is `None`.
    pub fn read(store: Option<&SessionLifecycleStore>, view: &RestoreView) -> Self {
        let Some(store) = store else {
            return Self::default();
        };
        // Before the boot is classified the restore set itself is read under
        // stand-in inputs, so even an id no row names cannot be answered.
        if view.booted_at_ms.is_none() {
            return Self::default();
        }
        let owed = view
            .restore_set(store)
            .iter()
            .try_fold(HashSet::new(), |mut owed, row| {
                if restore_still_owed(row, view)? {
                    owed.insert(row.claude_session_id.to_ascii_lowercase());
                }
                Some(owed)
            });
        Self { owed }
    }

    /// Is `session_id` still owed a restore by this process? `None` when that
    /// cannot be read. The id is matched case-insensitively, as the ledger
    /// keys sessions.
    pub fn of(&self, session_id: &str) -> Option<bool> {
        self.owed
            .as_ref()
            .map(|owed| owed.contains(&session_id.to_ascii_lowercase()))
    }
}

#[cfg(test)]
mod tests;
