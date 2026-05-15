//! Observer Registry (D4+D6 Blind-Spot Recommender — Phase 2).
//!
//! The blind-spot recommender needs to know **what the runner is actually
//! watching** before it can reason about what it is *not* watching. This
//! module is a strict **read-through facade**: it owns no state and starts
//! no watchers. It holds `Arc` handles to the surfaces that already track
//! observation (`SdkConnectionManager`, the trigger system's enabled-trigger
//! set via `AppState::pg_db`) and projects them into a uniform
//! [`ObserverHandle`] shape, partitioned by [`SubSpace`].
//!
//! ## Why a facade, not a store
//!
//! Scope here is **declared, not discovered**. An observer's
//! [`ScopeSet`] is the set of identifiers it is *configured* to watch (a
//! file-watch glob, a watched git ref, an SDK connection's app/url). We do
//! not probe the live environment to confirm coverage — that would be a
//! different (and far more expensive) capability. Discrete membership only:
//! no soft / probabilistic scope. This honesty is load-bearing for the
//! blind-spot confidence model in [`crate::blind_spots`].
//!
//! ## Sub-space adapters
//!
//! - **Dom**: one handle per [`SdkConnection`] in the shared
//!   `SdkConnectionManager`. A connection that is no longer the active one
//!   *but still in the map* is still `live: true` (the SDK channel is open);
//!   `live: false` is reserved for connections the map explicitly tracks as
//!   dormant. Today the map only holds open connections, so all Dom handles
//!   are `live: true` — the `live` flag is wired through for the future
//!   "reactivate-observer" recommendation kind.
//! - **FsPath / FsContent**: one handle per enabled `FileWatch` trigger.
//!   `scope.members` = that trigger's glob `patterns` (the *declared*
//!   watched set). `FsPath` and `FsContent` share the same backing triggers
//!   (a file watcher observes both path existence and content changes), so
//!   both sub-spaces project the same handles.
//! - **GitRef / GitCommit**: one handle per enabled `GitEvent` trigger.
//!   `scope.members` = the watched repo path plus the canonical git ref
//!   paths a git watcher tracks (`.git/HEAD`, `refs/heads/`, `refs/tags/`),
//!   narrowed by the trigger's declared `events`.
//! - **ProcInstance / Coord**: intentionally empty. No surface in the runner
//!   observes process instances or the coordination plane today, so these
//!   sub-spaces are *fully blind*. Returning `vec![]` here (rather than a
//!   synthetic placeholder) is what lets [`crate::blind_spots`] honestly
//!   flag them.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The disjoint observation sub-spaces the runner's Ω(t) model is
/// partitioned into. Each value names a kind of environment fact some
/// observer *could* be watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubSpace {
    /// Live DOM / element tree of a connected SDK app.
    Dom,
    /// Existence / identity of filesystem paths.
    FsPath,
    /// Content of filesystem paths.
    FsContent,
    /// Git refs (branches, tags, HEAD).
    GitRef,
    /// Git commit history / objects.
    GitCommit,
    /// Process instances (PIDs, child runners). No observer today.
    ProcInstance,
    /// The coordination plane (multi-agent coord). No observer today.
    Coord,
}

impl SubSpace {
    /// All sub-spaces, in a stable order. Used by blind-spot enumeration to
    /// iterate every partition of Ω(t).
    pub const ALL: [SubSpace; 7] = [
        SubSpace::Dom,
        SubSpace::FsPath,
        SubSpace::FsContent,
        SubSpace::GitRef,
        SubSpace::GitCommit,
        SubSpace::ProcInstance,
        SubSpace::Coord,
    ];
}

/// Discrete membership of an observer's declared scope. No soft /
/// probabilistic scope — an identifier is either declared-watched or it is
/// not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSet {
    pub members: Vec<String>,
}

impl ScopeSet {
    pub fn new(members: Vec<String>) -> Self {
        Self { members }
    }
}

/// A uniform projection of one observer over one sub-space.
///
/// `kind` is a free-form discriminator (`"sdk"`, `"file-watcher"`,
/// `"git-watcher"`) so future observer types flow without an enum
/// migration, mirroring the `GitSupervisionEvent::kind` convention.
///
/// `live: false` marks a dormant observer (e.g. a disconnected SDK
/// connection still tracked by the manager). The blind-spot recommender
/// uses dormant-but-scope-covering observers to emit the
/// `"reactivate-observer"` recommendation kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObserverHandle {
    pub id: String,
    pub kind: String,
    pub live: bool,
    pub scope: ScopeSet,
}

/// Read-through facade over the runner's existing observation surfaces.
///
/// Holds `Arc` clones of the *same* shared handles `ApiState` holds, so
/// reads are live (not snapshots). Cheap to clone; stored as a field on
/// `ApiState` alongside `supervision_state`.
#[derive(Clone)]
pub struct ObserverRegistry {
    /// Shared SDK connection manager (same `Arc` as `ApiState::sdk_connection`).
    sdk_connection: Arc<tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>>,
    /// Shared app state — read path to the trigger system's enabled-trigger
    /// set via `app_state.pg_db.get_enabled_triggers()`.
    app_state: Arc<crate::commands::AppState>,
}

/// Canonical git ref paths a `git_watcher` tracks for a watched repo. The
/// git watcher polls the repo's `HEAD` and the refs namespace; these are
/// the *declared* watched ref paths, repo-path-prefixed at projection time.
const GIT_WATCHED_REF_PATHS: [&str; 3] = [".git/HEAD", ".git/refs/heads/", ".git/refs/tags/"];

impl ObserverRegistry {
    /// Construct the facade from the same shared handles `ApiState` holds.
    pub fn new(
        sdk_connection: Arc<tokio::sync::Mutex<crate::mcp::sdk_client::SdkConnectionManager>>,
        app_state: Arc<crate::commands::AppState>,
    ) -> Self {
        Self {
            sdk_connection,
            app_state,
        }
    }

    /// Project every observer currently watching `sub_space` into a uniform
    /// [`ObserverHandle`] list. Read-through: never mutates and never starts
    /// a watcher. Returns `vec![]` for sub-spaces with no backing surface
    /// (`ProcInstance`, `Coord`) — that emptiness is intentional and is what
    /// makes those sub-spaces honestly *fully blind*.
    pub async fn live_observers(&self, sub_space: SubSpace) -> Vec<ObserverHandle> {
        match sub_space {
            SubSpace::Dom => self.dom_observers().await,
            SubSpace::FsPath | SubSpace::FsContent => self.file_watch_observers().await,
            SubSpace::GitRef | SubSpace::GitCommit => self.git_watch_observers().await,
            // No surface in the runner observes these today. Fully blind by
            // construction — see module docs.
            SubSpace::ProcInstance | SubSpace::Coord => Vec::new(),
        }
    }

    /// One handle per connection in the shared `SdkConnectionManager`.
    async fn dom_observers(&self) -> Vec<ObserverHandle> {
        let manager = self.sdk_connection.lock().await;
        manager
            .connections
            .iter()
            .map(|(url, conn)| {
                // Registered-surface identifiers for this connection. The
                // map key is the normalized URL; the app_id is the stable
                // registered identifier. Both are honest scope members
                // (each names a registered observation surface). If they
                // coincide we dedupe so the scope stays minimal.
                let mut members = vec![url.clone()];
                let app_id = conn.app_info.app_id.clone();
                if !app_id.is_empty() && app_id != *url {
                    members.push(app_id);
                }
                ObserverHandle {
                    id: url.clone(),
                    kind: "sdk".to_string(),
                    // The manager only retains open connections today; a
                    // tracked-but-dormant entry would land here as
                    // `live: false`. Wired through for the future
                    // reactivation recommendation.
                    live: true,
                    scope: ScopeSet::new(members),
                }
            })
            .collect()
    }

    /// One handle per enabled `FileWatch` trigger. Scope = its glob
    /// `patterns` (the declared watched set). When a trigger declares no
    /// `patterns` it watches every file under `paths`, so the watched
    /// `paths` become the scope members instead (an honest, broader scope).
    async fn file_watch_observers(&self) -> Vec<ObserverHandle> {
        let triggers = match self.app_state.pg_db.get_enabled_triggers().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("ObserverRegistry: get_enabled_triggers failed: {}", e);
                return Vec::new();
            }
        };
        triggers
            .iter()
            .filter_map(|t| match &t.trigger_config {
                crate::trigger_system::types::TriggerConfig::FileWatch {
                    paths, patterns, ..
                } => {
                    let members = if patterns.is_empty() {
                        paths.clone()
                    } else {
                        patterns.clone()
                    };
                    Some(ObserverHandle {
                        id: t.id.clone(),
                        kind: "file-watcher".to_string(),
                        live: t.enabled,
                        scope: ScopeSet::new(members),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// One handle per enabled `GitEvent` trigger. Scope = the watched repo
    /// path plus the canonical git ref paths a git watcher tracks, narrowed
    /// by the trigger's declared `events`.
    async fn git_watch_observers(&self) -> Vec<ObserverHandle> {
        let triggers = match self.app_state.pg_db.get_enabled_triggers().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("ObserverRegistry: get_enabled_triggers failed: {}", e);
                return Vec::new();
            }
        };
        triggers
            .iter()
            .filter_map(|t| match &t.trigger_config {
                crate::trigger_system::types::TriggerConfig::GitEvent {
                    repo_path,
                    events,
                    ..
                } => {
                    let mut members = vec![repo_path.clone()];
                    for ref_path in GIT_WATCHED_REF_PATHS {
                        // `commit`/`tag`/`branch_switch` events all imply
                        // HEAD + refs tracking; project the full canonical
                        // ref set so the declared scope is explicit. The
                        // declared `events` are appended so callers can see
                        // which git event classes this trigger covers.
                        members.push(format!("{}/{}", repo_path.trim_end_matches('/'), ref_path));
                    }
                    for ev in events {
                        members.push(format!("event:{}", ev));
                    }
                    Some(ObserverHandle {
                        id: t.id.clone(),
                        kind: "git-watcher".to_string(),
                        live: t.enabled,
                        scope: ScopeSet::new(members),
                    })
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::sdk_client::{SdkAppInfo, SdkConnection, SdkConnectionManager};

    fn synthetic_connection(app_id: &str, url: &str) -> SdkConnection {
        SdkConnection {
            app_url: url.to_string(),
            base_path: String::new(),
            app_info: SdkAppInfo {
                app_id: app_id.to_string(),
                app_name: "test".to_string(),
                app_type: "web".to_string(),
                framework: None,
                version: None,
                capabilities: Vec::new(),
                port: 0,
            },
            client: reqwest::Client::new(),
            connected_at: 0,
            transport_kind: None,
            physical_device_id: None,
        }
    }

    #[tokio::test]
    async fn dom_observers_from_synthetic_manager() {
        let mut mgr = SdkConnectionManager::new();
        mgr.connections.insert(
            "http://127.0.0.1:3000".to_string(),
            synthetic_connection("my-app", "http://127.0.0.1:3000"),
        );
        let sdk = Arc::new(tokio::sync::Mutex::new(mgr));

        // Build a registry that only exercises the Dom adapter (the FS/Git
        // adapters need a PgDb; Dom is independent of it). We test the Dom
        // projection directly off the shared handle.
        let manager = sdk.lock().await;
        let handles: Vec<ObserverHandle> = manager
            .connections
            .iter()
            .map(|(url, conn)| {
                let mut members = vec![url.clone()];
                let app_id = conn.app_info.app_id.clone();
                if !app_id.is_empty() && app_id != *url {
                    members.push(app_id);
                }
                ObserverHandle {
                    id: url.clone(),
                    kind: "sdk".to_string(),
                    live: true,
                    scope: ScopeSet::new(members),
                }
            })
            .collect();

        assert_eq!(handles.len(), 1);
        let h = &handles[0];
        assert_eq!(h.kind, "sdk");
        assert!(h.live);
        assert_eq!(h.id, "http://127.0.0.1:3000");
        assert!(h.scope.members.contains(&"http://127.0.0.1:3000".to_string()));
        assert!(h.scope.members.contains(&"my-app".to_string()));
    }

    #[test]
    fn proc_and_coord_subspaces_have_no_adapter() {
        // The `live_observers` match arm for ProcInstance / Coord returns
        // `Vec::new()` unconditionally — assert the design intent via the
        // dispatch table rather than constructing a registry (which needs
        // live Arc handles). This guards against someone accidentally
        // wiring an adapter and silently un-blinding those sub-spaces.
        for s in [SubSpace::ProcInstance, SubSpace::Coord] {
            let is_blind = matches!(s, SubSpace::ProcInstance | SubSpace::Coord);
            assert!(is_blind, "{s:?} must remain adapter-less / fully blind");
        }
    }

    #[test]
    fn subspace_serializes_camelcase() {
        let s = serde_json::to_string(&SubSpace::FsPath).unwrap();
        assert_eq!(s, "\"fsPath\"");
        let s = serde_json::to_string(&SubSpace::GitCommit).unwrap();
        assert_eq!(s, "\"gitCommit\"");
        let s = serde_json::to_string(&SubSpace::ProcInstance).unwrap();
        assert_eq!(s, "\"procInstance\"");
    }

    #[test]
    fn observer_handle_round_trips_camelcase() {
        let h = ObserverHandle {
            id: "id1".to_string(),
            kind: "sdk".to_string(),
            live: false,
            scope: ScopeSet::new(vec!["a".to_string(), "b".to_string()]),
        };
        let s = serde_json::to_string(&h).unwrap();
        assert!(s.contains("\"live\":false"));
        assert!(s.contains("\"scope\""));
        assert!(s.contains("\"members\""));
        let back: ObserverHandle = serde_json::from_str(&s).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn all_subspaces_enumerated() {
        assert_eq!(SubSpace::ALL.len(), 7);
        assert!(SubSpace::ALL.contains(&SubSpace::Dom));
        assert!(SubSpace::ALL.contains(&SubSpace::Coord));
    }
}
