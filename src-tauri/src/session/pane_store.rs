//! Pane → coord-session-id persistence (R2, session-lifecycle-cleanup).
//!
//! ## Problem
//!
//! Every runner restart re-creates terminal panes, and the legacy
//! `terminal_create` → [`crate::session::SessionRegistry::register_external`]
//! path mints a **fresh** `uuid_v7` per pane each time. The old coord
//! `coord.sessions` rows orphan and accumulate — the dashboard fills with
//! duplicate "Terminal shell session" rows, one per (restart × pane).
//!
//! ## Fix
//!
//! Persist each pane's coord session id keyed by a **stable pane identity**
//! and, on restore, RESUME the prior coord session (PATCH `state=active` +
//! heartbeat) instead of POSTing a brand-new row. See
//! [`crate::session::SessionRegistry::resume_external`].
//!
//! ## Stable pane identity
//!
//! The runner's terminal panes are NOT persisted backend-side — the
//! frontend (`components/terminal/useSessionPersistence.ts`) saves each
//! tab's `{title, workingDir, zoneIndex, …}` into `localStorage` and, on
//! restore, re-invokes `terminal_create` with the same `title` / `pageId`
//! / `workingDir`. The terminal `id` is a fresh `uuid_v4` each launch, so
//! it is NOT stable. The stable triple available at the `terminal_create`
//! boundary — and the one the frontend round-trips verbatim on restore —
//! is `(page_id, title, working_dir)`. We derive a deterministic
//! [`PaneKey`] from it so a restored pane maps back to its prior coord id.
//!
//! Collisions: two panes with the identical `(page_id, title, working_dir)`
//! triple are genuinely indistinguishable to the operator and to coord's
//! intent; mapping them to the same resumed row is the desired behavior
//! (one logical "Terminal 1 in workspace X" session), not a bug.
//!
//! ## Storage
//!
//! Mirrors the [`crate::session::local_store`] precedent: a single JSON
//! file at `<.qontinui>/runner/pane-sessions.json` (an object map
//! `pane_key -> uuid`), rewritten atomically via temp-file + rename. The
//! map is tiny (one entry per live pane), so we read+rewrite the whole
//! file per mutation rather than appending.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::warn;
use uuid::Uuid;

/// Deterministic, stable-across-restart identity for a terminal pane,
/// derived from the `(page_id, title, working_dir)` triple the frontend
/// round-trips through its `localStorage` layout on restore.
///
/// Stored as the JSON object key in the pane-session map. We use a
/// delimiter that cannot appear in a normalized field (`\u{1f}`, unit
/// separator) so distinct triples never alias.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneKey(String);

impl PaneKey {
    /// Build a key from the create-time triple. `None` fields normalize to
    /// the empty string so a pane created with `working_dir: None` maps to
    /// the same key on restore (the frontend persists `workingDir` and
    /// replays it, but tolerate the absent case).
    pub fn from_create(page_id: &str, title: &str, working_dir: &str) -> Self {
        // Unit separator (0x1F) — not a legal char in any of these fields
        // as the operator would type them, so the join is unambiguous.
        const SEP: char = '\u{1f}';
        PaneKey(format!(
            "{}{SEP}{}{SEP}{}",
            page_id.trim(),
            title.trim(),
            working_dir.trim()
        ))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Append-/rewrite-style JSON map persisting `PaneKey -> coord session id`.
///
/// Cheap to clone-share via `Arc`; all mutations take the internal lock and
/// rewrite the backing file atomically. Resilient to a missing / corrupt
/// file (treated as empty — a fresh map is the safe default, the resume
/// path falls back to a fresh register).
#[derive(Debug)]
pub struct PaneSessionStore {
    path: PathBuf,
    map: Mutex<HashMap<String, Uuid>>,
}

impl PaneSessionStore {
    /// Open (or initialize) the store at `path`, loading any existing map.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let map = load_map(&path);
        Ok(Self {
            path,
            map: Mutex::new(map),
        })
    }

    /// Look up the persisted coord session id for a pane, if any.
    pub fn get(&self, key: &PaneKey) -> Option<Uuid> {
        self.map
            .lock()
            .ok()
            .and_then(|m| m.get(key.as_str()).copied())
    }

    /// Persist (or overwrite) the coord session id for a pane and flush to
    /// disk. Best-effort: a write failure is logged, not propagated — the
    /// in-memory map still reflects the mapping for this process's lifetime.
    pub fn put(&self, key: &PaneKey, id: Uuid) {
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "pane_store: map lock poisoned on put");
                    return;
                }
            };
            m.insert(key.as_str().to_string(), id);
            m.clone()
        };
        if let Err(e) = write_map(&self.path, &snapshot) {
            warn!(
                error = %e,
                path = %self.path.display(),
                "pane_store: persist failed — mapping kept in memory only"
            );
        }
    }

    /// Drop a pane's mapping (e.g. when its coord row was GC'd and the
    /// resume fell back to a fresh id — the fresh id is `put` over the top,
    /// so explicit removal is only needed if a caller wants to forget a
    /// pane entirely). Best-effort flush.
    #[allow(dead_code)]
    pub fn remove(&self, key: &PaneKey) {
        let snapshot = {
            let mut m = match self.map.lock() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "pane_store: map lock poisoned on remove");
                    return;
                }
            };
            m.remove(key.as_str());
            m.clone()
        };
        if let Err(e) = write_map(&self.path, &snapshot) {
            warn!(error = %e, "pane_store: persist failed on remove");
        }
    }
}

fn load_map(path: &Path) -> HashMap<String, Uuid> {
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<HashMap<String, Uuid>>(&bytes) {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "pane_store: corrupt map file — starting empty (resume falls back to fresh register)"
                );
                HashMap::new()
            }
        },
        Err(e) => {
            warn!(error = %e, "pane_store: read failed — starting empty");
            HashMap::new()
        }
    }
}

fn write_map(path: &Path, map: &HashMap<String, Uuid>) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn pane_key_is_stable_for_same_triple() {
        let a = PaneKey::from_create("default", "Terminal 1", "C:/repo");
        let b = PaneKey::from_create("default", "Terminal 1", "C:/repo");
        assert_eq!(a, b);
    }

    #[test]
    fn pane_key_trims_and_differs_on_distinct_triples() {
        let a = PaneKey::from_create(" default ", " Terminal 1 ", " C:/repo ");
        let b = PaneKey::from_create("default", "Terminal 1", "C:/repo");
        assert_eq!(a, b, "leading/trailing whitespace must not change identity");

        let c = PaneKey::from_create("page-2", "Terminal 1", "C:/repo");
        assert_ne!(a, c, "distinct page_id → distinct pane");

        // Field-boundary aliasing guard: "a|b" + "c" must not equal "a" + "b|c".
        let x = PaneKey::from_create("a", "b", "c");
        let y = PaneKey::from_create("ab", "", "c");
        assert_ne!(x, y);
    }

    #[test]
    fn put_get_round_trips_and_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        let key = PaneKey::from_create("default", "Terminal 1", "C:/repo");
        let id = Uuid::new_v4();

        {
            let store = PaneSessionStore::open(&path).unwrap();
            assert!(store.get(&key).is_none());
            store.put(&key, id);
            assert_eq!(store.get(&key), Some(id));
        }

        // Reopen — the mapping survives a "restart".
        let store = PaneSessionStore::open(&path).unwrap();
        assert_eq!(store.get(&key), Some(id));
    }

    #[test]
    fn put_overwrites_prior_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        let store = PaneSessionStore::open(&path).unwrap();
        let key = PaneKey::from_create("default", "Terminal 1", "");
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        store.put(&key, id1);
        store.put(&key, id2);
        assert_eq!(store.get(&key), Some(id2));
    }

    #[test]
    fn remove_forgets_mapping() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        let store = PaneSessionStore::open(&path).unwrap();
        let key = PaneKey::from_create("default", "T", "");
        store.put(&key, Uuid::new_v4());
        store.remove(&key);
        assert!(store.get(&key).is_none());
    }

    #[test]
    fn corrupt_file_starts_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        let store = PaneSessionStore::open(&path).unwrap();
        let key = PaneKey::from_create("default", "T", "");
        assert!(store.get(&key).is_none());
        // Still usable after recovery.
        let id = Uuid::new_v4();
        store.put(&key, id);
        assert_eq!(store.get(&key), Some(id));
    }
}
