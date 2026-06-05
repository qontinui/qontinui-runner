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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// Window geometry, persisted for Phase-2 restore. Logical pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
                    created_at: now_ms,
                },
            );
            s.clone()
        };
        self.persist(&snapshot);
        true
    }

    /// Allocate the next monotonic `term-N` label and insert a pop-out window
    /// record. Persists and returns the new record (the command layer creates
    /// the actual `WebviewWindow` and emits `window-opened`).
    pub fn create_window(
        &self,
        title: Option<String>,
        geometry: Option<WindowGeometry>,
        now_ms: i64,
    ) -> WindowRecord {
        let (record, snapshot) = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(e) => {
                    // Poisoned lock: still return a usable label derived from
                    // the count so the caller can proceed; persistence is
                    // skipped this round.
                    warn!(error = %e, "window_assignments: lock poisoned on create_window");
                    let label = format!("{POPOUT_LABEL_PREFIX}{}", now_ms);
                    return WindowRecord {
                        label,
                        kind: WindowKind::PopOut,
                        title,
                        geometry,
                        created_at: now_ms,
                    };
                }
            };
            let label = next_popout_label(&s.windows);
            let record = WindowRecord {
                label: label.clone(),
                kind: WindowKind::PopOut,
                title,
                geometry,
                created_at: now_ms,
            };
            s.windows.insert(label, record.clone());
            (record, s.clone())
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
fn next_popout_label(windows: &HashMap<WindowLabel, WindowRecord>) -> WindowLabel {
    let max_n = windows
        .keys()
        .filter_map(|k| k.strip_prefix(POPOUT_LABEL_PREFIX))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{POPOUT_LABEL_PREFIX}{}", max_n + 1)
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

    #[test]
    fn owner_of_defaults_to_main() {
        let (_d, wa) = store();
        assert_eq!(wa.owner_of("unknown-session"), "main");
    }

    #[test]
    fn create_window_allocates_monotonic_term_labels() {
        let (_d, wa) = store();
        wa.ensure_main(1);
        let a = wa.create_window(None, None, 10);
        let b = wa.create_window(None, None, 20);
        assert_eq!(a.label, "term-1");
        assert_eq!(b.label, "term-2");
        assert_eq!(a.kind, WindowKind::PopOut);
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
        let w = wa.create_window(None, None, 10);
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
            let w = wa.create_window(Some("Pop 1".into()), None, 10);
            wa.assign_session("sess-A", &w.label);
        }
        let wa = WindowAssignments::open(&path).unwrap();
        assert_eq!(wa.owner_of("sess-A"), "term-1");
        assert!(wa.snapshot().windows.contains_key("term-1"));
        // Next allocation continues monotonically after restore.
        assert_eq!(wa.create_window(None, None, 20).label, "term-2");
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
