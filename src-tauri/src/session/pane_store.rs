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

    /// Window-aware key (Phase 2 of the pop-out-terminal-windows plan).
    ///
    /// For the `"main"` window — and the legacy single-window world where the
    /// caller passes no label — this is **byte-identical** to [`from_create`],
    /// so existing `pane-sessions.json` entries keep resolving and there is no
    /// migration. For a `"term-N"` pop-out the window label is folded into the
    /// key, so two windows showing the same `(page_id, title, working_dir)`
    /// pane no longer collide on a single coord session (the bug called out in
    /// the plan's "Relationship to pane_store").
    pub fn from_create_in_window(
        page_id: &str,
        title: &str,
        working_dir: &str,
        window_label: &str,
    ) -> Self {
        let wl = window_label.trim();
        if wl.is_empty() || wl == crate::window_assignments::MAIN_WINDOW_LABEL {
            return Self::from_create(page_id, title, working_dir);
        }
        const SEP: char = '\u{1f}';
        PaneKey(format!(
            "{}{SEP}{}{SEP}{}{SEP}{}",
            page_id.trim(),
            title.trim(),
            working_dir.trim(),
            wl,
        ))
    }

    /// Remote-pane key (plan `2026-08-31-remote-session-tabs-in-runner-terminal`,
    /// Phase 4).
    ///
    /// A tab attached to a session on ANOTHER device round-trips the same
    /// `(page_id, title, working_dir)` triple as a local pane would, so
    /// without a device segment a restored remote tab would alias the local
    /// pane with the same title and cwd — and resume ITS coord row. The
    /// device id is folded in behind a literal `remote` segment so the key can
    /// never equal a [`from_create_in_window`] key either (a pop-out label
    /// occupies the fourth slot there; this key has five). [`from_create`] is
    /// untouched, so local restart identity is preserved byte-for-byte.
    pub fn from_create_remote(
        page_id: &str,
        title: &str,
        working_dir: &str,
        device_id: &str,
    ) -> Self {
        const SEP: char = '\u{1f}';
        PaneKey(format!(
            "{}{SEP}{}{SEP}{}{SEP}remote{SEP}{}",
            page_id.trim(),
            title.trim(),
            working_dir.trim(),
            device_id.trim(),
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
    /// The coord binding of each LIVE holder-backed terminal, keyed by
    /// terminal id ([`TerminalCoordBinding`]), persisted beside the pane map
    /// in [`terminal_bindings_path`].
    terminals: Mutex<HashMap<String, TerminalCoordBinding>>,
}

/// What a HOLDER-backed terminal's coord session was registered with, kept
/// under its terminal id for the one restore that reuses a terminal id: the
/// next runner's boot sweep adopting the pane (plan
/// `2026-10-21-graceful-runner-restart-detaches-holder-panes-instead-of-killing-them`,
/// D8). The create triple cannot serve there — the pane's working dir may have
/// been reassigned to a worktree after that key was taken and its title renamed
/// since, and the holder's restore metadata carries neither original — and a
/// resume needs more than the row id: the TENANT (a non-default tenant's row
/// probed under the default credential 404s and is re-registered in the wrong
/// tenant) and the intent fields the row was declared with. Removed when the
/// terminal really exits, so it does not accumulate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalCoordBinding {
    pub coord_id: Uuid,
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub work_unit_slug: Option<String>,
    #[serde(default)]
    pub correlation_topic: Option<String>,
}

/// The terminal-binding file beside the pane map at `pane_map_path`:
/// `<stem>-terminals.json`.
pub fn terminal_bindings_path(pane_map_path: &Path) -> PathBuf {
    let stem = pane_map_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "pane-sessions".to_string());
    pane_map_path.with_file_name(format!("{stem}-terminals.json"))
}

impl PaneSessionStore {
    /// Open (or initialize) the store at `path`, loading any existing map.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let map = load_map(&path);
        let terminals = load_json(&terminal_bindings_path(&path));
        Ok(Self {
            path,
            map: Mutex::new(map),
            terminals: Mutex::new(terminals),
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

impl PaneSessionStore {
    /// The recorded coord binding of terminal `terminal_id`, if any.
    pub fn get_terminal(&self, terminal_id: &str) -> Option<TerminalCoordBinding> {
        self.terminals
            .lock()
            .ok()
            .and_then(|m| m.get(terminal_id.trim()).cloned())
    }

    /// Record (or overwrite) terminal `terminal_id`'s coord binding.
    pub fn put_terminal(&self, terminal_id: &str, binding: TerminalCoordBinding) {
        self.mutate_terminals(|m| {
            m.insert(terminal_id.trim().to_string(), binding.clone()) != Some(binding)
        });
    }

    /// Forget terminal `terminal_id`'s coord binding (its real exit).
    pub fn remove_terminal(&self, terminal_id: &str) {
        self.mutate_terminals(|m| m.remove(terminal_id.trim()).is_some());
    }

    /// Apply `f` — which answers whether it CHANGED the map — and persist
    /// only on a change, while still holding the lock. A remove of an absent
    /// id (every in-process pane's exit, many at once during a handoff
    /// teardown) therefore writes nothing, and two writers can never land a
    /// stale snapshot over a newer one (review round 2).
    fn mutate_terminals(&self, f: impl FnOnce(&mut HashMap<String, TerminalCoordBinding>) -> bool) {
        let mut m = match self.terminals.lock() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "pane_store: terminal-binding lock poisoned");
                return;
            }
        };
        if !f(&mut m) {
            return;
        }
        if let Err(e) = write_json(&terminal_bindings_path(&self.path), &*m) {
            warn!(error = %e, "pane_store: terminal-binding persist failed — kept in memory only");
        }
    }
}

fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = std::fs::read(path) else {
        return T::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        warn!(error = %e, path = %path.display(), "pane_store: corrupt file — starting empty");
        T::default()
    })
}

/// Atomic replace through a temp file UNIQUE to this write (pid + counter), so
/// no two writes — in this process or another — ever share one.
fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
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
    fn window_aware_key_is_backward_compatible_for_main() {
        // "main", empty, and whitespace-only labels must yield the legacy
        // 3-tuple key byte-for-byte (no migration of existing entries).
        let legacy = PaneKey::from_create("default", "Terminal 1", "C:/repo");
        for label in ["main", "", "   "] {
            assert_eq!(
                PaneKey::from_create_in_window("default", "Terminal 1", "C:/repo", label),
                legacy,
                "label {label:?} must not change the main/legacy key"
            );
        }
    }

    #[test]
    fn window_aware_key_disambiguates_pop_outs() {
        // Same (page, title, cwd) in two different pop-outs → distinct keys,
        // and both distinct from the main-window key.
        let main = PaneKey::from_create_in_window("default", "T", "C:/repo", "main");
        let t1 = PaneKey::from_create_in_window("default", "T", "C:/repo", "term-1");
        let t2 = PaneKey::from_create_in_window("default", "T", "C:/repo", "term-2");
        assert_ne!(t1, t2);
        assert_ne!(t1, main);
        assert_ne!(t2, main);
        // Stable for the same (triple, window).
        assert_eq!(
            t1,
            PaneKey::from_create_in_window("default", "T", "C:/repo", "term-1")
        );
    }

    /// Phase 4: a remote key never equals the local key for the same triple,
    /// never equals a pop-out key whose label happens to match the device id,
    /// and `from_create` is byte-identical to its pre-Phase-4 shape.
    #[test]
    fn remote_key_never_aliases_local_or_window_keys() {
        let local = PaneKey::from_create("p", "Terminal 1", "/w");
        let remote = PaneKey::from_create_remote("p", "Terminal 1", "/w", "dev-a");
        let window = PaneKey::from_create_in_window("p", "Terminal 1", "/w", "dev-a");
        assert_ne!(remote, local);
        assert_ne!(remote, window);
        assert_ne!(
            remote,
            PaneKey::from_create_remote("p", "Terminal 1", "/w", "dev-b")
        );
        assert_eq!(
            remote,
            PaneKey::from_create_remote(" p ", "Terminal 1 ", "/w", " dev-a ")
        );
        assert_eq!(local.as_str(), "p\u{1f}Terminal 1\u{1f}/w");
        assert_eq!(
            remote.as_str(),
            "p\u{1f}Terminal 1\u{1f}/w\u{1f}remote\u{1f}dev-a"
        );
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

    #[test]
    fn terminal_bindings_round_trip_through_their_own_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        let binding = TerminalCoordBinding {
            coord_id: Uuid::new_v4(),
            tenant_id: Some(Uuid::new_v4()),
            repo: Some("qontinui-runner".into()),
            work_unit_slug: Some("wu".into()),
            correlation_topic: None,
        };
        {
            let store = PaneSessionStore::open(&path).unwrap();
            store.put_terminal("term-1", binding.clone());
        }
        assert!(terminal_bindings_path(&path).exists());
        let store = PaneSessionStore::open(&path).unwrap();
        assert_eq!(store.get_terminal("term-1"), Some(binding));
        assert!(store.get(&PaneKey::from_create("", "", "")).is_none());
        store.remove_terminal("term-1");
        assert_eq!(
            PaneSessionStore::open(&path)
                .unwrap()
                .get_terminal("term-1"),
            None
        );
    }

    /// Review round 2: removing an id that was never recorded (an in-process
    /// pane's exit) writes NOTHING — not even an empty file — and a write
    /// leaves no temp file behind.
    #[test]
    fn removing_an_absent_terminal_binding_does_not_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pane-sessions.json");
        let store = PaneSessionStore::open(&path).unwrap();
        store.remove_terminal("never-recorded");
        assert!(
            !terminal_bindings_path(&path).exists(),
            "no write for a no-op remove"
        );

        let binding = TerminalCoordBinding {
            coord_id: Uuid::new_v4(),
            tenant_id: None,
            repo: None,
            work_unit_slug: None,
            correlation_topic: None,
        };
        store.put_terminal("t", binding);
        let file = terminal_bindings_path(&path);
        std::fs::write(&file, b"sentinel").unwrap();
        store.remove_terminal("still-absent");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"sentinel",
            "an absent remove leaves the file untouched"
        );
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "no temp file left behind"
        );
    }
}
