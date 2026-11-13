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
//!   The ledger asks one thing more than placement: has the row's PAGE been
//!   restored yet ([`PagesRestored`])? A page's restore that ran to its end and
//!   left the row out (a deselected or failed restore, see
//!   [`crate::terminal::page_placement`]) will not bring it back in this
//!   process, so the ledger reads such a row as not owed, while placement keeps
//!   counting it.
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use crate::session::session_lifecycle_store::{
    boot_restore_phase, BootRestorePhase, SessionLifecycleStore, TerminalSessionRecord,
};

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

/// One window's report of where its pages' boot restores stand: the webview's
/// own restore progress (`useTerminalInitialization.ts`, `publishRestorePages`),
/// the same sets the since-restart strip reads (`sinceRestart.ts`,
/// `pageRestoreSettled`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRestoreProgress {
    /// Pages whose restore has run to its end.
    pub complete: HashSet<String>,
    /// Pages whose restore the coord device drain deferred; each re-runs when
    /// the drain lifts.
    pub deferred: HashSet<String>,
    /// Every page in the persisted layout.
    pub known: HashSet<String>,
}

impl PageRestoreProgress {
    /// Has `page_id`'s boot restore run in this process? A deferred page has
    /// not, whatever else says so. A page in the layout has once its own restore
    /// completed. A page in NO layout holds orphans, which the first page to
    /// restore adopts, so it has once any page has. A blank page id is
    /// `default`, as the webview reads it.
    fn has_run(&self, page_id: &str) -> bool {
        let page = if page_id.is_empty() {
            "default"
        } else {
            page_id
        };
        if self.deferred.contains(page) {
            return false;
        }
        self.complete.contains(page) || (!self.known.contains(page) && !self.complete.is_empty())
    }

    fn merge(mut self, other: &PageRestoreProgress) -> Self {
        self.complete.extend(other.complete.iter().cloned());
        self.deferred.extend(other.deferred.iter().cloned());
        self.known.extend(other.known.iter().cloned());
        self
    }
}

/// Each window's latest [`PageRestoreProgress`], by window label. A page
/// restores in the window that shows it, so no one window's report covers
/// every page; the union does.
static PAGE_RESTORE_PROGRESS: Mutex<BTreeMap<String, PageRestoreProgress>> =
    Mutex::new(BTreeMap::new());

/// Record `window`'s latest restore progress, replacing its previous report.
pub fn record_page_restore_progress(window: &str, progress: PageRestoreProgress) {
    if let Ok(mut m) = PAGE_RESTORE_PROGRESS.lock() {
        m.insert(window.to_string(), progress);
    }
}

/// Which pages' boot restore has run in THIS process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagesRestored {
    /// No page's restore has run: the restore path has not handed out a
    /// restore set yet ([`BootRestorePhase`] is not `Ran`).
    NoneYet,
    /// The windows' reports, merged.
    Reported(PageRestoreProgress),
    /// A restore set was handed out but no window has reported where its pages
    /// stand, so which pages have run cannot be told.
    Unknown,
}

impl PagesRestored {
    /// This process's state: the windows' reports when any has reported, else
    /// what the boot-restore latch says.
    pub fn current() -> Self {
        let reports = PAGE_RESTORE_PROGRESS
            .lock()
            .ok()
            .filter(|m| !m.is_empty())
            .map(|m| {
                m.values()
                    .fold(PageRestoreProgress::default(), |acc, p| acc.merge(p))
            });
        match (reports, boot_restore_phase()) {
            (Some(progress), _) => Self::Reported(progress),
            (None, BootRestorePhase::NotYetRun | BootRestorePhase::Withheld) => Self::NoneYet,
            (None, BootRestorePhase::Ran) => Self::Unknown,
        }
    }

    /// Has `page_id`'s restore run? `None` when that cannot be told.
    fn has_run(&self, page_id: &str) -> Option<bool> {
        match self {
            Self::NoneYet => Some(false),
            Self::Reported(progress) => Some(progress.has_run(page_id)),
            Self::Unknown => None,
        }
    }
}

/// D3 over the live store, by session: the sessions a restore in THIS process
/// still owes. This is the ledger's `restoreOwed`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwedRestores {
    /// The lowercased ids [`restore_still_owed`] keeps, each with whether its
    /// page's restore is still to run (`None` when that cannot be told). An id
    /// absent from the map is not owed. `None` when that cannot be read: there
    /// is no store, the boot is not classified yet, or a row of the restore set
    /// could not be judged. Then an id missing from the map could be that
    /// row's, so no id can be answered.
    owed: Option<HashMap<String, Option<bool>>>,
}

impl OwedRestores {
    /// Read from `store` under `view`, with `pages` saying which pages' restore
    /// has run. With no store, every answer is `None`. A row is owed only while
    /// its page's restore is still to run: one that ran and did not bring the
    /// row back will not in this process.
    pub fn read(
        store: Option<&SessionLifecycleStore>,
        view: &RestoreView,
        pages: &PagesRestored,
    ) -> Self {
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
            .try_fold(HashMap::new(), |mut owed, row| {
                if restore_still_owed(row, view)? {
                    owed.insert(
                        row.claude_session_id.to_ascii_lowercase(),
                        pages.has_run(&row.page_id).map(|ran| !ran),
                    );
                }
                Some(owed)
            });
        Self { owed }
    }

    /// Is `session_id` still owed a restore by this process? `None` when that
    /// cannot be read. The id is matched case-insensitively, as the ledger
    /// keys sessions.
    pub fn of(&self, session_id: &str) -> Option<bool> {
        self.owed.as_ref().and_then(|owed| {
            owed.get(&session_id.to_ascii_lowercase())
                .copied()
                .unwrap_or(Some(false))
        })
    }
}

#[cfg(test)]
mod tests;
