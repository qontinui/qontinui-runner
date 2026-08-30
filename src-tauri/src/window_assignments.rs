//! Window → terminal-session ownership + the runner's own window registry.
//!
//! Phase 1 of the pop-out terminal-windows plan
//! (`plans/2026-06-03-runner-popout-terminal-windows.md`). A single runner
//! process can host multiple OS windows (`"main"` + `"term-N"` pop-outs),
//! each rendering its own subset of terminal tabs. This is the authoritative,
//! persisted source of truth for **which window owns which session** and the
//! set of known windows.
//!
//! ## Design
//!
//! Mirrors the atomic-write persistence of [`crate::session::pane_store`]: the
//! whole [`WindowAssignmentsState`] is serialized to a single JSON file
//! (`<.qontinui>/runner/window-assignments.json`), rewritten atomically via
//! temp-file + rename on every mutation. The map is tiny (one entry per live
//! window / assigned session), so read+rewrite-whole is fine.
//!
//! **Decoupled from Tauri on purpose.** Mutators persist and RETURN the change
//! (the new record, the prior owner, the list of reassigned sessions); the
//! command layer (`commands::window_manager`) is responsible for emitting the
//! `window-opened` / `window-closed` / `session-assignment-changed` events. This
//! keeps the module unit-testable without an `AppHandle`.
//!
//! ## Invariants
//! - **Exactly one owner per session.** [`WindowAssignments::assign_session`] is
//!   the only mutator of `session_owner`; a move is unmount-in-source /
//!   mount-in-target driven by one `session-assignment-changed` event.
//! - **No orphaned PTYs.** Closing a window reassigns its sessions to `"main"`
//!   (never drops them); PTYs live in the process-global `TerminalManager`,
//!   independent of window lifetime.
//! - **Default to `"main"`.** [`WindowAssignments::owner_of`] returns `"main"`
//!   for any unknown session id, so unassigned tabs render in the main window —
//!   backward-compatible with the single-window world.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// A window label: `"main"` for the primary window, `"term-N"` for pop-outs.
pub type WindowLabel = String;

/// The primary window's label (matches `get_main_window_label()` and the
/// UI Bridge `MAIN_WINDOW_LABEL`).
pub const MAIN_WINDOW_LABEL: &str = "main";

/// Prefix for allocated pop-out window labels (`term-1`, `term-2`, …).
const POPOUT_LABEL_PREFIX: &str = "term-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Main,
    PopOut,
}

/// Window geometry, persisted for Phase-2 restore. Physical pixels (global
/// desktop coordinates), matching what `set_position`/`set_size` consume on
/// restore — see `commands::terminal_windows::restore_pop_out_windows`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowRecord {
    pub label: WindowLabel,
    pub kind: WindowKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,
    /// Last-known geometry, for Phase-2 restore. Optional today.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub geometry: Option<WindowGeometry>,
    /// The terminal PAGE this window is bound to, when it hosts a whole
    /// detached page (the "pop out page" feature) rather than an ad-hoc set of
    /// individually-moved terminals. Page ids are STABLE across process
    /// restarts (unlike terminal ids, which are regenerated each launch), so a
    /// page-bound pop-out is the only kind of pop-out that can survive a reboot:
    /// its terminals are re-bound by `page_id`, not by the stale `session_owner`
    /// map. A page-bound window is restored on boot (and skipped by the empty-
    /// pop-out prune) even though it owns no `session_owner` entries.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bound_page: Option<String>,
    /// Unix-millis creation time.
    pub created_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WindowAssignmentsState {
    /// `session_id`/terminalId → owning window. Exactly one owner per session.
    /// Sessions absent from this map are owned by `"main"` by default.
    #[serde(default)]
    pub session_owner: HashMap<String, WindowLabel>,
    /// Known windows, including `"main"`.
    #[serde(default)]
    pub windows: HashMap<WindowLabel, WindowRecord>,
}

/// Result of closing a window: the sessions that were reassigned to `"main"`
/// (each with its prior owner — always the closed label), so the command layer
/// can emit one `session-assignment-changed` per moved session.
#[derive(Debug, Clone, Default)]
pub struct WindowClose {
    /// `(session_id, from_label)` for each session moved to `"main"`.
    pub reassigned: Vec<(String, WindowLabel)>,
    /// Whether a window record was actually removed.
    pub removed: bool,
}

/// Tauri managed state. Cheap to share via the managed-state `State<'_, _>`.
#[derive(Debug)]
pub struct WindowAssignments {
    path: PathBuf,
    inner: Mutex<WindowAssignmentsState>,
    /// Pop-out labels handed out by [`WindowAssignments::reserve_popout_label`]
    /// that have **no persisted record** — a window whose webview is mid-build,
    /// or one whose build failed.
    ///
    /// In-memory only and deliberately never persisted: a label burned by a
    /// failed build must be reusable after a restart.
    ///
    /// Why it exists: `term-N` is derived as `max(N) + 1` over the *records*
    /// (see [`next_popout_label`]), so today it is allocated by the very act of
    /// inserting the record. Plan `2026-08-10-popout-webview2-creation-failure`
    /// Phase 3 moves record insertion to **after** a successful build, which
    /// would otherwise let two concurrent `open_terminal_window` calls derive
    /// the same label while neither has committed a record. The reservation set
    /// closes that window: allocation stays atomic, while nothing is persisted
    /// until the webview is proven.
    ///
    /// A reservation is **never handed back** on failure, and that is
    /// deliberate. Tauri inserts a window into its own registry the moment
    /// `build()` returns `Ok` (`tauri` 2.11.1 `src/manager/webview.rs`,
    /// `attach_webview`) and never removes a *webview-less* one — no wry-side
    /// window exists, so no `Destroyed` event can ever fire for it. Rebuilding
    /// the same label would therefore fail with
    /// `Error::WebviewLabelAlreadyExists` (`src/manager/webview.rs:437`) for the
    /// rest of the process's life. Burning the label costs a monotonically
    /// higher `N` on the next attempt — exactly what the pre-Phase-3 code did,
    /// minus the persisted record that survived reboots.
    reserved_labels: Mutex<HashSet<WindowLabel>>,
    /// Sequence for the poisoned-`inner`-lock fallback label only. See
    /// [`WindowAssignments::fallback_label`].
    fallback_seq: AtomicU64,
}

impl WindowAssignments {
    /// Open (or initialize) the store at `path`, loading any existing state.
    /// A missing/corrupt file is treated as empty (the safe default).
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let state = load_state(&path);
        Ok(Self {
            path,
            inner: Mutex::new(state),
            reserved_labels: Mutex::new(HashSet::new()),
            fallback_seq: AtomicU64::new(0),
        })
    }

    /// Register the `"main"` window record on boot if absent. Returns `true`
    /// if it was created. Persists on change.
    pub fn ensure_main(&self, now_ms: i64) -> bool {
        let snapshot = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on ensure_main");
                    return false;
                }
            };
            if s.windows.contains_key(MAIN_WINDOW_LABEL) {
                return false;
            }
            s.windows.insert(
                MAIN_WINDOW_LABEL.to_string(),
                WindowRecord {
                    label: MAIN_WINDOW_LABEL.to_string(),
                    kind: WindowKind::Main,
                    title: None,
                    geometry: None,
                    bound_page: None,
                    created_at: now_ms,
                },
            );
            s.clone()
        };
        self.persist(&snapshot);
        true
    }

    /// Reserve the next monotonic `term-N` label **without persisting
    /// anything**.
    ///
    /// The returned label collides with neither an existing record nor any
    /// other outstanding reservation. It stays reserved until
    /// [`Self::create_reserved_window`] commits it as a record — and if the
    /// window is never built, it stays reserved (burned) for the rest of the
    /// process's life, which is required, not sloppy: see
    /// [`WindowAssignments::reserved_labels`].
    ///
    /// Plan `2026-08-10-popout-webview2-creation-failure` Phase 3: a pop-out
    /// record must not be persisted until its webview is *proven*, so the label
    /// has to be allocatable independently of the record. A plain read-only
    /// "peek" would not do — two concurrent opens would peek the same `N`.
    pub fn reserve_popout_label(&self) -> WindowLabel {
        self.reserve_popout_label_where(|_| false)
    }

    /// [`Self::reserve_popout_label`], but additionally skipping any label the
    /// caller reports is **still taken by a live webview**.
    ///
    /// `label_is_taken` answers "does the windowing runtime still know this
    /// label?" — in production, `app.get_webview_window(label).is_some()`.
    ///
    /// # Why this store cannot infer that itself
    ///
    /// This module owns *records*, and a record is not the window. The two
    /// disagree in one direction that matters, and it is not hypothetical
    /// (manual-test-loop iteration 2): the X-button on a pop-out removes the
    /// record while leaving the webview alive, because a pop-out's React tree
    /// registers `onCloseRequested` and `tauri`'s own window handler turns any
    /// such JS listener into an automatic `api.prevent_close()`
    /// (`tauri-2.11.1/src/manager/window.rs`). The OS window is never dropped,
    /// but [`next_popout_label`] — which derives `max(N) + 1` over records and
    /// reservations — has just been told `term-1` is free. Handing it back
    /// makes the next `WebviewWindowBuilder::build()` fail with
    /// `Error::WebviewLabelAlreadyExists`.
    ///
    /// The primary fix for that is destroying the window (see
    /// `commands::terminal_windows::handle_window_close`), which frees the
    /// label in `tauri`'s registry. This predicate is the **second** line:
    /// `destroy()` is an asynchronous message to the event loop and can fail
    /// outright, and the `reserved_labels` doc records a separate way a label
    /// can be permanently taken with no record — a `build()` that returned
    /// `Ok` for a webview-less window. In both cases the store's own view is
    /// correct and still not sufficient, so the liveness question is asked of
    /// the runtime that can answer it.
    pub fn reserve_popout_label_where(
        &self,
        label_is_taken: impl Fn(&str) -> bool,
    ) -> WindowLabel {
        // Lock order in this module: `reserved_labels` BEFORE `inner`. Every
        // method that takes both must use this order.
        let mut reserved = match self.reserved_labels.lock() {
            Ok(r) => r,
            Err(poisoned) => poisoned.into_inner(),
        };
        let label = match self.inner.lock() {
            Ok(s) => next_free_popout_label(&s.windows, &reserved, label_is_taken),
            Err(e) => {
                // Poisoned lock: still hand back a usable, unique label so the
                // caller can proceed, just not a monotonic `term-N` one.
                warn!(error = %e, "window_assignments: lock poisoned on reserve_popout_label");
                self.fallback_label()
            }
        };
        reserved.insert(label.clone());
        label
    }

    /// The label handed out when the `inner` lock is poisoned and the normal
    /// `max(N) + 1` derivation is unavailable.
    ///
    /// A process-lifetime [`AtomicU64`], not a wall clock. The wall-clock
    /// spelling this replaced (`term-<unix_millis>`) collides outright for two
    /// reservations inside the same millisecond, and the collision is
    /// **silent**: `HashSet::insert` returns `false` and both callers walk away
    /// believing they hold the label, after which the second `build()` fails
    /// with `WebviewLabelAlreadyExists` for no visible reason. A counter cannot
    /// do that.
    ///
    /// The `fallback-` infix keeps these out of the monotonic namespace on
    /// purpose: [`next_popout_label`] parses the suffix as `u32`, so a
    /// `fallback-` label is skipped rather than becoming a `max(N)` that all
    /// later labels have to climb past.
    fn fallback_label(&self) -> WindowLabel {
        let n = self.fallback_seq.fetch_add(1, AtomicOrdering::Relaxed);
        format!("{POPOUT_LABEL_PREFIX}fallback-{n}")
    }

    /// Commit a label reserved by [`Self::reserve_popout_label`] as a real
    /// pop-out record. Persists and returns the record (the command layer emits
    /// `window-opened`).
    pub fn create_reserved_window(
        &self,
        label: WindowLabel,
        title: Option<String>,
        geometry: Option<WindowGeometry>,
        bound_page: Option<String>,
        now_ms: i64,
    ) -> WindowRecord {
        let record = WindowRecord {
            label: label.clone(),
            kind: WindowKind::PopOut,
            title,
            geometry,
            bound_page,
            created_at: now_ms,
        };
        let snapshot = {
            // Same lock order as `reserve_popout_label`.
            let mut reserved = match self.reserved_labels.lock() {
                Ok(r) => r,
                Err(poisoned) => poisoned.into_inner(),
            };
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    // The reservation is deliberately NOT released here. The
                    // window this label names WAS built — releasing it would
                    // hand the same label to the next open, whose `build()`
                    // then fails with `WebviewLabelAlreadyExists`. Persistence
                    // is what is lost on this path, not the allocation.
                    warn!(error = %e, "window_assignments: lock poisoned on create_reserved_window");
                    return record;
                }
            };
            s.windows.insert(label.clone(), record.clone());
            reserved.remove(&label);
            s.clone()
        };
        self.persist(&snapshot);
        record
    }

    /// The atomic move primitive: set `session_id`'s owner to `to`. Returns the
    /// PRIOR owner (`from`), which is `"main"` (the default) when the session
    /// wasn't explicitly owned before. Returns `None` only when the assignment
    /// is a no-op (already owned by `to`). Persists on change.
    pub fn assign_session(&self, session_id: &str, to: &str) -> Option<WindowLabel> {
        let (from, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on assign_session");
                    return None;
                }
            };
            let prior = s
                .session_owner
                .get(session_id)
                .cloned()
                .unwrap_or_else(|| MAIN_WINDOW_LABEL.to_string());
            if prior == to {
                return None; // no-op
            }
            // Assigning back to "main" is represented by REMOVING the explicit
            // entry, so the default-to-main rule keeps the map minimal.
            if to == MAIN_WINDOW_LABEL {
                s.session_owner.remove(session_id);
            } else {
                s.session_owner
                    .insert(session_id.to_string(), to.to_string());
            }
            (prior, s.clone())
        };
        self.persist(&snapshot);
        Some(from)
    }

    /// Close a window: reassign every session it owned to `"main"` and remove
    /// its record. Never orphans a PTY. Returns the moves so the caller can
    /// emit `session-assignment-changed` per session then `window-closed`.
    /// Closing `"main"` is a no-op (the main window is never removed here).
    pub fn close_window(&self, label: &str) -> WindowClose {
        if label == MAIN_WINDOW_LABEL {
            return WindowClose::default();
        }
        let (result, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on close_window");
                    return WindowClose::default();
                }
            };
            let reassigned: Vec<(String, WindowLabel)> = s
                .session_owner
                .iter()
                .filter(|(_, owner)| owner.as_str() == label)
                .map(|(sid, owner)| (sid.clone(), owner.clone()))
                .collect();
            for (sid, _) in &reassigned {
                s.session_owner.remove(sid); // back to default "main"
            }
            let removed = s.windows.remove(label).is_some();
            (
                WindowClose {
                    reassigned,
                    removed,
                },
                s.clone(),
            )
        };
        self.persist(&snapshot);
        result
    }

    /// The owning window of a session, defaulting to `"main"` for unknown ids.
    pub fn owner_of(&self, session_id: &str) -> WindowLabel {
        self.inner
            .lock()
            .ok()
            .and_then(|s| s.session_owner.get(session_id).cloned())
            .unwrap_or_else(|| MAIN_WINDOW_LABEL.to_string())
    }

    /// Sessions a window should render. For `"main"` this is only the sessions
    /// EXPLICITLY mapped to main (the default-to-main rule means the frontend
    /// derives main's full set as "all tabs minus those owned elsewhere").
    #[allow(dead_code)]
    pub fn sessions_for(&self, label: &str) -> Vec<String> {
        self.inner
            .lock()
            .ok()
            .map(|s| {
                s.session_owner
                    .iter()
                    .filter(|(_, owner)| owner.as_str() == label)
                    .map(|(sid, _)| sid.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A full snapshot for hydrating a window on load.
    pub fn snapshot(&self) -> WindowAssignmentsState {
        self.inner.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// All known window records (for `list_runner_windows`).
    pub fn window_records(&self) -> Vec<WindowRecord> {
        self.inner
            .lock()
            .map(|s| s.windows.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Pop-out (`term-N`) records only — the windows to recreate on boot.
    /// `"main"` is created by the normal startup path, never restored here.
    pub fn pop_out_records(&self) -> Vec<WindowRecord> {
        self.inner
            .lock()
            .map(|s| {
                s.windows
                    .values()
                    .filter(|r| r.kind == WindowKind::PopOut)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a window currently owns at least one assigned session. `"main"`
    /// is the default owner for every unmapped session, so it is reported as
    /// non-empty whenever ANY session exists (it always "could" host one) — but
    /// callers only ever ask this about `term-N` pop-outs, where an empty answer
    /// means "no tab renders here". Unknown labels are empty.
    pub fn has_assigned_sessions(&self, label: &str) -> bool {
        self.inner
            .lock()
            .map(|s| s.session_owner.values().any(|owner| owner == label))
            .unwrap_or(false)
    }

    /// Whether `label` is a PAGE-BOUND pop-out — a window that hosts an entire
    /// terminal page and claims its tabs by `page_id`, not through the
    /// `session_owner` map. Such a window is never "empty" in the sense the
    /// auto-close / sweep paths mean: its tabs simply do not appear in
    /// `session_owner`. Unknown labels are not page-bound.
    pub fn is_page_bound(&self, label: &str) -> bool {
        self.inner
            .lock()
            .map(|s| s.windows.get(label).is_some_and(|r| r.bound_page.is_some()))
            .unwrap_or(false)
    }

    /// Clear the entire `session_owner` map (reverting every session to the
    /// default `"main"`), persisting on change. Returns the number of entries
    /// dropped.
    ///
    /// **Boot-only.** Terminal ids are a fresh `uuid_v4` per launch and do NOT
    /// survive a process restart (`terminal::manager::create`), so EVERY
    /// persisted owner entry references a terminal that will never reappear —
    /// the map is entirely stale on boot. Clearing it before
    /// [`Self::prune_empty_pop_outs`] makes every persisted pop-out correctly
    /// register as empty (no live tab can ever claim it) so the boot orphan
    /// sweep prunes them all, instead of a stale entry wrongly pinning a dead
    /// pop-out open forever. Mid-session callers must NOT use this (it would
    /// strand live tabs back on `main`).
    pub fn clear_session_owners(&self) -> usize {
        let (count, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on clear_session_owners");
                    return 0;
                }
            };
            if s.session_owner.is_empty() {
                return 0;
            }
            let count = s.session_owner.len();
            s.session_owner.clear();
            (count, s.clone())
        };
        self.persist(&snapshot);
        count
    }

    /// Prune every pop-out (`term-N`) record that currently owns NO assigned
    /// session, removing it from the registry so the boot-restore loop stops
    /// resurrecting it and the monotonic `term-N` counter stops climbing on
    /// dead records. Returns the labels pruned. Never touches `"main"` or a
    /// pop-out that still owns a session. Persists on change.
    ///
    /// **Why this is the boot-orphan fix:** PTYs do not survive a process
    /// restart, so a persisted pop-out's sessions are always gone on the next
    /// boot — `restore_pop_out_windows` would otherwise recreate an empty,
    /// purposeless window every time. Pruning the empties on boot breaks that
    /// loop. (A pop-out whose tabs were reassigned to `main` before shutdown is
    /// likewise empty here and correctly pruned — its tabs render on `main`.)
    pub fn prune_empty_pop_outs(&self) -> Vec<WindowLabel> {
        let (pruned, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on prune_empty_pop_outs");
                    return Vec::new();
                }
            };
            let owned: std::collections::HashSet<&str> =
                s.session_owner.values().map(|v| v.as_str()).collect();
            // A PAGE-BOUND pop-out is intentionally retained even with no
            // `session_owner` entry: its terminals are claimed by `page_id`
            // (stable across restarts), not by the per-terminal owner map. The
            // boot orphan sweep clears `session_owner` (all stale), which would
            // otherwise make every page-bound window look empty and prune it —
            // defeating page restore. Per-id pop-outs (no `bound_page`) still
            // prune when empty, exactly as before.
            let empties: Vec<WindowLabel> = s
                .windows
                .values()
                .filter(|r| {
                    r.kind == WindowKind::PopOut
                        && r.bound_page.is_none()
                        && !owned.contains(r.label.as_str())
                })
                .map(|r| r.label.clone())
                .collect();
            if empties.is_empty() {
                return Vec::new();
            }
            for label in &empties {
                s.windows.remove(label);
            }
            (empties, s.clone())
        };
        self.persist(&snapshot);
        pruned
    }

    /// Record a window's last-known geometry (for Phase-2 restore). Persists
    /// only when the geometry actually changed (so the periodic capture poll
    /// is a no-op while a window sits still — no disk churn). Returns `true`
    /// when it wrote. Unknown labels are ignored.
    pub fn update_geometry(&self, label: &str, geometry: WindowGeometry) -> bool {
        let snapshot = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on update_geometry");
                    return false;
                }
            };
            match s.windows.get_mut(label) {
                Some(rec) if rec.geometry.as_ref() != Some(&geometry) => {
                    rec.geometry = Some(geometry);
                }
                _ => return false, // unknown window, or geometry unchanged
            }
            s.clone()
        };
        self.persist(&snapshot);
        true
    }

    /// Drop `session_owner` entries whose target window no longer exists in the
    /// registry (e.g. a hand-edited or partially-written state file), reverting
    /// those sessions to the default `"main"`. Run once on boot, after the
    /// window set is known. Returns the session ids that were reassigned.
    /// `"main"` is always a valid target even before `ensure_main`.
    pub fn reconcile_orphans(&self) -> Vec<String> {
        let (orphans, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "window_assignments: lock poisoned on reconcile_orphans");
                    return Vec::new();
                }
            };
            let orphans: Vec<String> = s
                .session_owner
                .iter()
                .filter(|(_, owner)| {
                    owner.as_str() != MAIN_WINDOW_LABEL && !s.windows.contains_key(owner.as_str())
                })
                .map(|(sid, _)| sid.clone())
                .collect();
            if orphans.is_empty() {
                return Vec::new();
            }
            for sid in &orphans {
                s.session_owner.remove(sid);
            }
            (orphans, s.clone())
        };
        self.persist(&snapshot);
        orphans
    }

    fn persist(&self, state: &WindowAssignmentsState) {
        if let Err(e) = write_state(&self.path, state) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "window_assignments: persist failed — state kept in memory only"
            );
        }
    }
}

/// Next monotonic `term-N` label: one greater than the highest existing
/// `term-N` (so labels are stable and reused on restore — a closed `term-2`
/// frees its number only if it's the highest). Starts at `term-1`.
///
/// The highest `N` in use counts **both** committed records and outstanding
/// reservations. Reservations must be included or two concurrent opens would
/// derive the same label while neither has persisted a record yet — see
/// [`WindowAssignments::reserved_labels`].
fn next_popout_label(
    windows: &HashMap<WindowLabel, WindowRecord>,
    reserved: &HashSet<WindowLabel>,
) -> WindowLabel {
    let max_n = windows
        .keys()
        .chain(reserved.iter())
        .filter_map(|k| k.strip_prefix(POPOUT_LABEL_PREFIX))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{POPOUT_LABEL_PREFIX}{}", max_n + 1)
}

/// How far past `max(N) + 1` the search for a free label will walk before
/// giving up and returning the last candidate anyway.
///
/// A bound rather than an open loop because `label_is_taken` is supplied by
/// the caller and this runs while BOTH of this store's locks are held: a
/// predicate that answered `true` forever would otherwise deadlock every other
/// window operation instead of failing one window open. The number only has to
/// exceed the count of labels the runtime can hold with no matching record,
/// which is bounded by the pop-outs a session opens; 1024 is far past that and
/// the walk is a hash lookup per step.
const MAX_LABEL_PROBES: u32 = 1024;

/// [`next_popout_label`], then walk forward while `label_is_taken` says the
/// candidate is still held by a live webview.
///
/// See [`WindowAssignments::reserve_popout_label_where`] for why the records
/// alone cannot answer this.
fn next_free_popout_label(
    windows: &HashMap<WindowLabel, WindowRecord>,
    reserved: &HashSet<WindowLabel>,
    label_is_taken: impl Fn(&str) -> bool,
) -> WindowLabel {
    let first = next_popout_label(windows, reserved);
    if !label_is_taken(&first) {
        return first;
    }

    // `first` is `term-<n>`; keep the prefix and climb. Parsing it back is
    // cheaper than threading `max_n` out of `next_popout_label` and keeps the
    // monotonic derivation in exactly one place.
    let Some(start) = first
        .strip_prefix(POPOUT_LABEL_PREFIX)
        .and_then(|n| n.parse::<u32>().ok())
    else {
        return first;
    };

    let mut candidate = first;
    for n in start + 1..start.saturating_add(MAX_LABEL_PROBES) {
        candidate = format!("{POPOUT_LABEL_PREFIX}{n}");
        if !label_is_taken(&candidate) {
            warn!(
                label = %candidate,
                skipped_from = %format!("{POPOUT_LABEL_PREFIX}{start}"),
                "window_assignments: pop-out labels below this one are still held by a live \
                 webview with no record — a window was closed without being destroyed"
            );
            return candidate;
        }
    }
    warn!(
        probes = MAX_LABEL_PROBES,
        label = %candidate,
        "window_assignments: no free pop-out label found; returning the last candidate, whose \
         build is expected to fail with WebviewLabelAlreadyExists"
    );
    candidate
}

fn load_state(path: &Path) -> WindowAssignmentsState {
    if !path.exists() {
        return WindowAssignmentsState::default();
    }
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<WindowAssignmentsState>(&bytes).unwrap_or_else(|e| {
            warn!(
                error = %e,
                path = %path.display(),
                "window_assignments: corrupt state file — starting empty"
            );
            WindowAssignmentsState::default()
        }),
        Err(e) => {
            warn!(error = %e, "window_assignments: read failed — starting empty");
            WindowAssignmentsState::default()
        }
    }
}

fn write_state(path: &Path, state: &WindowAssignmentsState) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store() -> (tempfile::TempDir, WindowAssignments) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("window-assignments.json");
        let wa = WindowAssignments::open(&path).unwrap();
        (dir, wa)
    }

    /// Reserve + commit in one step, for the tests that only need a pop-out
    /// record to exist and do not care about the two-phase split.
    ///
    /// This used to be a production method (`WindowAssignments::create_window`).
    /// Phase 3 of `2026-08-10-popout-webview2-creation-failure` left it with no
    /// production callers — a record must not be persisted until its window's
    /// webview is proven — so it moved here rather than surviving as a public
    /// API nothing ships (delete-over-deprecate).
    fn create_window(
        wa: &WindowAssignments,
        title: Option<String>,
        geometry: Option<WindowGeometry>,
        bound_page: Option<String>,
        now_ms: i64,
    ) -> WindowRecord {
        let label = wa.reserve_popout_label();
        wa.create_reserved_window(label, title, geometry, bound_page, now_ms)
    }

    #[test]
    fn owner_of_defaults_to_main() {
        let (_d, wa) = store();
        assert_eq!(wa.owner_of("unknown-session"), "main");
    }

    #[test]
    fn create_window_allocates_monotonic_term_labels() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let a = create_window(&wa, None, None, None, 10);
        let b = create_window(&wa, None, None, None, 20);
        assert_eq!(a.label, "term-1");
        assert_eq!(b.label, "term-2");
        assert_eq!(a.kind, WindowKind::PopOut);
    }

    /// The poisoned-lock fallback must be unique and must not poison the
    /// monotonic namespace.
    ///
    /// The `term-<unix_millis>` spelling this replaced failed both: two
    /// reservations in the same millisecond produced the *same* label, and
    /// `HashSet::insert` swallowed the collision silently — two callers would
    /// each believe they held it, and the second `build()` would fail with
    /// `WebviewLabelAlreadyExists` for no visible reason.
    #[test]
    fn the_poisoned_lock_fallback_label_is_unique_and_out_of_band() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        // Back to back — the wall-clock spelling collided exactly here.
        let a = wa.fallback_label();
        let b = wa.fallback_label();
        assert_ne!(a, b, "two fallback labels in the same instant must differ");

        // And a fallback label must not become the `max(N)` every later
        // `term-N` has to climb past.
        let mut reserved = HashSet::new();
        reserved.insert(a);
        reserved.insert(b);
        assert_eq!(
            next_popout_label(&HashMap::new(), &reserved),
            "term-1",
            "a fallback label is outside the numeric namespace, so it does not \
             advance the monotonic counter"
        );
    }

    #[test]
    fn assign_session_moves_owner_and_reports_prior() {
        let (_d, wa) = store();
        // First move from default main → term-1.
        let from = wa.assign_session("sess-A", "term-1");
        assert_eq!(from.as_deref(), Some("main"));
        assert_eq!(wa.owner_of("sess-A"), "term-1");
        // Re-assigning to the same window is a no-op.
        assert_eq!(wa.assign_session("sess-A", "term-1"), None);
        // Move back to main removes the explicit entry (default rule).
        let from = wa.assign_session("sess-A", "main");
        assert_eq!(from.as_deref(), Some("term-1"));
        assert_eq!(wa.owner_of("sess-A"), "main");
        assert!(!wa.snapshot().session_owner.contains_key("sess-A"));
    }

    #[test]
    fn close_window_reassigns_all_its_sessions_to_main() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10);
        wa.assign_session("sess-A", &w.label);
        wa.assign_session("sess-B", &w.label);
        wa.assign_session("sess-C", "term-2"); // a different window

        let closed = wa.close_window(&w.label);
        assert!(closed.removed);
        let moved: std::collections::HashSet<_> =
            closed.reassigned.iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(moved, ["sess-A".to_string(), "sess-B".to_string()].into());
        assert!(closed.reassigned.iter().all(|(_, from)| from == &w.label));
        assert_eq!(wa.owner_of("sess-A"), "main");
        assert_eq!(wa.owner_of("sess-B"), "main");
        assert_eq!(wa.owner_of("sess-C"), "term-2"); // untouched
        assert!(!wa.snapshot().windows.contains_key(&w.label));
    }

    /// `close_window` must be safely repeatable.
    ///
    /// `close_terminal_window` calls `handle_window_close` explicitly (because
    /// `WebviewWindow::destroy()` emits no `CloseRequested`), and the OS-close
    /// path calls it from the window-event handler — so on a window closed by
    /// its titlebar X the teardown can legitimately run twice. The second call
    /// must be a clean no-op rather than re-emitting reassignments for sessions
    /// that already moved back to `"main"`.
    #[test]
    fn close_window_is_idempotent() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10);
        wa.assign_session("sess-A", &w.label);

        let first = wa.close_window(&w.label);
        assert!(first.removed);
        assert_eq!(first.reassigned.len(), 1);

        let second = wa.close_window(&w.label);
        assert!(!second.removed, "second close must not claim a removal");
        assert!(
            second.reassigned.is_empty(),
            "second close must not re-emit reassignments"
        );
        assert_eq!(wa.owner_of("sess-A"), "main");
        assert!(!wa.snapshot().windows.contains_key(&w.label));
    }

    /// A PAGE-BOUND pop-out's record must go away when the window closes.
    ///
    /// While the record survives, the frontend's `pageId → windowLabel` mirror
    /// (derived from this registry) keeps hiding that page in every live
    /// window. With every page hidden, `useTerminalPages` mints a fresh empty
    /// one and the zone grid renders ZERO zones permanently — a refresh cannot
    /// recover, because the mirror is re-derived from this same registry.
    #[test]
    fn close_window_drops_the_page_binding() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, Some("default".into()), 10);
        assert_eq!(
            wa.snapshot().windows[&w.label].bound_page.as_deref(),
            Some("default")
        );

        wa.close_window(&w.label);
        assert!(
            !wa.snapshot().windows.contains_key(&w.label),
            "a page-bound pop-out's record must not outlive its window",
        );
    }

    #[test]
    fn close_main_is_noop() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        wa.assign_session("sess-A", "main"); // no-op (default)
        let closed = wa.close_window("main");
        assert!(!closed.removed);
        assert!(closed.reassigned.is_empty());
        assert!(wa.snapshot().windows.contains_key("main"));
    }

    #[test]
    fn state_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("window-assignments.json");
        {
            let wa = WindowAssignments::open(&path).unwrap();
            wa.ensure_main(1);
            let w = create_window(&wa, Some("Pop 1".into()), None, None, 10);
            wa.assign_session("sess-A", &w.label);
        }
        let wa = WindowAssignments::open(&path).unwrap();
        assert_eq!(wa.owner_of("sess-A"), "term-1");
        assert!(wa.snapshot().windows.contains_key("term-1"));
        // Next allocation continues monotonically after restore.
        assert_eq!(create_window(&wa, None, None, None, 20).label, "term-2");
    }

    #[test]
    fn update_geometry_persists_only_on_change() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10);
        let g = WindowGeometry {
            x: 100,
            y: 200,
            w: 800,
            h: 600,
            maximized: false,
        };
        // First write persists.
        assert!(wa.update_geometry(&w.label, g.clone()));
        // Identical write is a no-op (no disk churn during the idle poll).
        assert!(!wa.update_geometry(&w.label, g.clone()));
        // A different geometry persists again.
        let g2 = WindowGeometry { x: 150, ..g };
        assert!(wa.update_geometry(&w.label, g2.clone()));
        // Unknown window is ignored.
        assert!(!wa.update_geometry("term-999", g2.clone()));
        // Survives reopen.
        let records = wa.pop_out_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].geometry.as_ref(), Some(&g2));
    }

    #[test]
    fn pop_out_records_excludes_main() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        create_window(&wa, None, None, None, 10);
        create_window(&wa, None, None, None, 20);
        let pops = wa.pop_out_records();
        assert_eq!(pops.len(), 2);
        assert!(pops.iter().all(|r| r.kind == WindowKind::PopOut));
        assert!(pops.iter().all(|r| r.label != "main"));
    }

    #[test]
    fn reconcile_orphans_reassigns_dangling_owners_to_main() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10); // term-1 exists
        wa.assign_session("sess-live", &w.label); // valid owner
        wa.assign_session("sess-orphan", "term-7"); // term-7 never existed
        let reassigned = wa.reconcile_orphans();
        assert_eq!(reassigned, vec!["sess-orphan".to_string()]);
        assert_eq!(wa.owner_of("sess-orphan"), "main"); // reverted
        assert_eq!(wa.owner_of("sess-live"), "term-1"); // untouched
                                                        // Idempotent: a second pass finds nothing.
        assert!(wa.reconcile_orphans().is_empty());
    }

    #[test]
    fn has_assigned_sessions_reflects_owner_map() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let w = create_window(&wa, None, None, None, 10); // term-1, no sessions yet
        assert!(
            !wa.has_assigned_sessions(&w.label),
            "a freshly-created pop-out owns no sessions"
        );
        wa.assign_session("sess-A", &w.label);
        assert!(wa.has_assigned_sessions(&w.label), "now owns sess-A");
        // Moving the only session away empties it again.
        wa.assign_session("sess-A", "main");
        assert!(
            !wa.has_assigned_sessions(&w.label),
            "empty again after the tab moved to main"
        );
        assert!(
            !wa.has_assigned_sessions("term-999"),
            "unknown label is empty"
        );
    }

    #[test]
    fn is_page_bound_is_true_only_for_windows_with_a_bound_page() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let bound = create_window(&wa, None, None, Some("page-A".to_string()), 10);
        let per_id = create_window(&wa, None, None, None, 20);

        assert!(
            wa.is_page_bound(&bound.label),
            "a window created with a bound_page IS page-bound"
        );
        assert!(
            !wa.is_page_bound(&per_id.label),
            "a plain per-id pop-out is NOT page-bound"
        );
        assert!(
            !wa.is_page_bound(MAIN_WINDOW_LABEL),
            "main's record carries bound_page: None, so it is not page-bound"
        );
        assert!(
            !wa.is_page_bound("term-999"),
            "an unknown label is not page-bound"
        );
    }

    #[test]
    fn prune_empty_pop_outs_removes_only_session_less_popouts() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let empty1 = create_window(&wa, None, None, None, 10); // term-1, empty
        let live = create_window(&wa, None, None, None, 20); // term-2, will hold a session
        let empty2 = create_window(&wa, None, None, None, 30); // term-3, empty
        wa.assign_session("sess-live", &live.label);

        let mut pruned = wa.prune_empty_pop_outs();
        pruned.sort();
        assert_eq!(pruned, vec![empty1.label.clone(), empty2.label.clone()]);

        let remaining: std::collections::HashSet<String> =
            wa.pop_out_records().into_iter().map(|r| r.label).collect();
        assert_eq!(remaining, [live.label.clone()].into());
        // main is never pruned.
        assert!(wa.snapshot().windows.contains_key("main"));
        // The live window's session is untouched.
        assert_eq!(wa.owner_of("sess-live"), live.label);
        // Idempotent: a second pass finds nothing new.
        assert!(wa.prune_empty_pop_outs().is_empty());
    }

    #[test]
    fn clear_session_owners_then_prune_removes_all_popouts_on_boot() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let a = create_window(&wa, None, None, None, 10);
        let b = create_window(&wa, None, None, None, 20);
        // Simulate the persisted (now-stale) owner map from a prior session.
        wa.assign_session("dead-tid-1", &a.label);
        wa.assign_session("dead-tid-2", &b.label);
        assert!(wa.has_assigned_sessions(&a.label));

        // Boot sweep: clear stale owners → every pop-out becomes empty → pruned.
        let cleared = wa.clear_session_owners();
        assert_eq!(cleared, 2);
        let pruned = wa.prune_empty_pop_outs();
        assert_eq!(
            pruned.len(),
            2,
            "both pop-outs are empty after clear → pruned"
        );
        assert!(wa.pop_out_records().is_empty());
        // main survives; owner map is empty.
        assert!(wa.snapshot().windows.contains_key("main"));
        assert!(wa.snapshot().session_owner.is_empty());
        // Idempotent.
        assert_eq!(wa.clear_session_owners(), 0);
    }

    #[test]
    fn prune_empty_pop_outs_noop_when_all_have_sessions() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let a = create_window(&wa, None, None, None, 10);
        let b = create_window(&wa, None, None, None, 20);
        wa.assign_session("s1", &a.label);
        wa.assign_session("s2", &b.label);
        assert!(wa.prune_empty_pop_outs().is_empty());
        assert_eq!(wa.pop_out_records().len(), 2);
    }

    #[test]
    fn prune_keeps_page_bound_popouts_but_removes_empty_per_id_ones() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        // A page-bound pop-out owns NO session_owner entry (it claims terminals
        // by page_id) yet must survive the prune.
        let bound = create_window(&wa, None, None, Some("page-A".into()), 10);
        // A per-id pop-out with no sessions is genuinely empty → pruned.
        let empty = create_window(&wa, None, None, None, 20);

        let pruned = wa.prune_empty_pop_outs();
        assert_eq!(pruned, vec![empty.label.clone()]);
        let remaining: std::collections::HashSet<String> =
            wa.pop_out_records().into_iter().map(|r| r.label).collect();
        assert_eq!(remaining, [bound.label.clone()].into());
    }

    #[test]
    fn page_bound_window_survives_boot_sweep_and_persists_binding() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("window-assignments.json");
        {
            let wa = WindowAssignments::open(&path).unwrap();
            wa.ensure_main(1);
            create_window(&wa, None, None, Some("page-A".into()), 10); // page-bound
            create_window(&wa, None, None, None, 20); // per-id, will be empty on boot
                                                      // Simulate the prior session's (now-stale) owner map.
            wa.assign_session("dead-tid", "term-2");
        }
        // Reopen (process restart) and run the boot sweep order from
        // `restore_pop_out_windows`: clear stale owners, then prune empties.
        let wa = WindowAssignments::open(&path).unwrap();
        assert_eq!(wa.clear_session_owners(), 1);
        wa.prune_empty_pop_outs();
        // The page-bound window survives with its binding intact; the per-id
        // window is gone.
        let pops = wa.pop_out_records();
        assert_eq!(pops.len(), 1, "only the page-bound pop-out survives");
        assert_eq!(pops[0].label, "term-1");
        assert_eq!(pops[0].bound_page.as_deref(), Some("page-A"));
    }

    /// The manual-test-loop iteration-2 regression, in the store's own terms.
    ///
    /// An X-closed pop-out has its record removed while `tauri` still holds a
    /// live webview for its label (a pop-out's React tree registers
    /// `onCloseRequested`, which `tauri` turns into an automatic
    /// `prevent_close`). `next_popout_label` counts RECORDS, so it re-derives
    /// the label that is still taken, and the next `build()` fails with
    /// `WebviewLabelAlreadyExists`.
    ///
    /// The negative control asserts the broken behaviour deliberately: without
    /// it, the real assertion passes against a store that never had the
    /// collision to begin with, and this test would pin nothing.
    #[test]
    fn reserve_skips_a_label_whose_webview_is_still_alive() {
        let (_d, wa) = store();
        wa.ensure_main(1);

        // Open and then X-close `term-1`: record created, record removed, but
        // the webview outlives both.
        let first = wa.reserve_popout_label();
        assert_eq!(first, "term-1");
        wa.create_reserved_window(first.clone(), None, None, None, 1);
        wa.close_window(&first);

        // NEGATIVE CONTROL, on an identically-driven second store: blind to
        // liveness, it hands back the label that is still taken.
        let (_d2, wa2) = store();
        wa2.ensure_main(1);
        let f2 = wa2.reserve_popout_label();
        wa2.create_reserved_window(f2.clone(), None, None, None, 1);
        wa2.close_window(&f2);
        assert_eq!(
            wa2.reserve_popout_label(),
            "term-1",
            "negative control failed: the liveness-blind path was supposed to re-hand `term-1`. \
             If it does not, this test proves nothing about the skip."
        );

        // With liveness, `term-1` is skipped.
        let alive = ["term-1"];
        assert_eq!(
            wa.reserve_popout_label_where(|l| alive.contains(&l)),
            "term-2",
            "a label whose webview is still alive must not be handed out again"
        );
    }

    /// Several stranded labels in a row are all skipped, not just the first.
    #[test]
    fn reserve_walks_past_a_run_of_live_labels() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let alive = ["term-1", "term-2", "term-3"];
        assert_eq!(
            wa.reserve_popout_label_where(|l| alive.contains(&l)),
            "term-4"
        );
    }

    /// The skip does not perturb the normal case: with nothing stranded, the
    /// predicate path is exactly the monotonic derivation it replaced.
    #[test]
    fn reserve_with_a_liveness_predicate_is_monotonic_when_nothing_is_stranded() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        assert_eq!(wa.reserve_popout_label_where(|_| false), "term-1");
        assert_eq!(wa.reserve_popout_label_where(|_| false), "term-2");
        // A committed record still counts toward `max(N)`.
        wa.create_reserved_window("term-3".to_string(), None, None, None, 1);
        assert_eq!(wa.reserve_popout_label_where(|_| false), "term-4");
    }

    /// A predicate that never yields must fail ONE window open, not hang: the
    /// walk runs while BOTH of this store's locks are held, so an unbounded
    /// loop there would wedge every other window operation.
    #[test]
    fn reserve_gives_up_after_a_bounded_number_of_probes() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let label = wa.reserve_popout_label_where(|_| true);
        assert_eq!(
            label,
            format!("{POPOUT_LABEL_PREFIX}{MAX_LABEL_PROBES}"),
            "the walk must stop at the probe bound and return the last candidate"
        );
    }

    #[test]
    fn corrupt_file_starts_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("window-assignments.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let wa = WindowAssignments::open(&path).unwrap();
        assert_eq!(wa.owner_of("x"), "main");
        wa.ensure_main(1);
        assert!(wa.snapshot().windows.contains_key("main"));
    }
}
