//! Runner shim over the extracted `qontinui-code-graph` crate.
//!
//! All graph *computation* (tree-sitter parsing, import resolution, blast-radius,
//! the fingerprint-incremental algorithm) now lives in the standalone
//! [`qontinui_code_graph`] crate, which is filesystem-persistence-free. This
//! module:
//!   1. Re-exports the crate's public API so existing caller paths like
//!      `crate::workflow_generation::code_graph::{CodeGraph, BlastRadius, ...}`
//!      keep resolving unchanged.
//!   2. Owns the runner-side persistence (`PersistedCodeGraph` save/load via
//!      [`crate::paths::get_code_graph_path`]) — the only filesystem coupling.
//!   3. Provides [`build_incremental`] (a free fn) wrapping the crate's
//!      `CodeGraph::build_incremental(path, prior)` with the OLD on-disk
//!      load -> build -> save behavior, so runner call sites are unchanged in
//!      effect (only routed through a free fn instead of an inherent method,
//!      since you cannot add inherent methods to the foreign `CodeGraph` type).

// Re-export the crate's full public API so existing caller paths like
// `crate::workflow_generation::code_graph::{CodeGraph, BlastRadius, ...}` keep
// resolving. This glob also brings `CodeGraph`, `FileFingerprint`,
// `IncrementalInput`, `compute_fingerprints`, `repo_hash`, etc. into this
// module's scope for the runner-only persistence code below.
pub use qontinui_code_graph::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

// ============================================================================
// Runner-side persistence (the ONLY filesystem coupling; the crate is pure)
// ============================================================================

/// The resolved code graph plus per-file fingerprints, persisted under app-data
/// so a rebuild re-parses only changed files (UA's knowledge-graph.json pattern).
///
/// Runner-only: the `qontinui-code-graph` crate is persistence-free, so this
/// save/load wrapper (keyed by [`crate::paths::get_code_graph_path`]) lives here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedCodeGraph {
    /// Schema version, so a format change invalidates stale caches.
    pub version: u32,
    /// Absolute project path this graph was built for (sanity check on load).
    pub project_path: String,
    /// Repo-relative file path -> content fingerprint.
    pub fingerprints: HashMap<String, FileFingerprint>,
    /// The resolved graph.
    pub graph: CodeGraph,
    /// Count of files parsed on the most recent (incremental or full) build.
    /// Diagnostic only — lets callers/tests assert incremental work.
    pub last_parsed_count: usize,
}

const PERSISTED_GRAPH_VERSION: u32 = 1;

impl PersistedCodeGraph {
    /// Save this persisted graph to its app-data cache file (`<repo-hash>.json`).
    pub fn save(&self, project_path: &Path) -> std::io::Result<()> {
        let path = crate::paths::get_code_graph_path(&repo_hash(project_path));
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Load a persisted graph from its app-data cache file, if present and valid
    /// for this project path + schema version.
    pub fn load(project_path: &Path) -> Option<PersistedCodeGraph> {
        let path = crate::paths::get_code_graph_path(&repo_hash(project_path));
        let json = std::fs::read_to_string(path).ok()?;
        let persisted: PersistedCodeGraph = serde_json::from_str(&json).ok()?;
        if persisted.version != PERSISTED_GRAPH_VERSION {
            return None;
        }
        Some(persisted)
    }
}

// ============================================================================
// Persisted incremental build (runner behavior preserved)
// ============================================================================

/// Build the resolved code graph using the persisted app-data cache for
/// fingerprint-incremental re-analysis, preserving the runner's OLD behavior:
/// load the prior [`PersistedCodeGraph`] (if any & valid for this project path)
/// -> call the pure [`qontinui_code_graph::CodeGraph::build_incremental`] with it
/// -> recompute fingerprints -> persist -> return.
///
/// Returns the resolved graph and the number of files parsed this run
/// (0 if nothing changed since the last persisted build).
///
/// This is a free function (not an inherent method on `CodeGraph`) because the
/// `CodeGraph` type is foreign to this crate. Callers that used
/// `CodeGraph::build_incremental(path)` now call this `build_incremental(path)`.
pub fn build_incremental(project_path: &Path) -> (CodeGraph, usize) {
    // Load the previous persisted graph (if any & valid for this exact path).
    let prior = PersistedCodeGraph::load(project_path).and_then(|p| {
        if p.project_path == project_path.to_string_lossy() {
            Some(IncrementalInput {
                graph: p.graph,
                fingerprints: p.fingerprints,
            })
        } else {
            None
        }
    });

    // Pure computation in the crate — no filesystem persistence touched there.
    let (graph, parsed_count) = CodeGraph::build_incremental(project_path, prior);

    // Persist the freshly-built graph + fingerprints for the next run.
    let persisted = PersistedCodeGraph {
        version: PERSISTED_GRAPH_VERSION,
        project_path: project_path.to_string_lossy().to_string(),
        fingerprints: compute_fingerprints(project_path, &graph),
        graph: graph.clone(),
        last_parsed_count: parsed_count,
    };
    if let Err(e) = persisted.save(project_path) {
        warn!("CodeGraph: failed to persist resolved graph: {}", e);
    }

    (graph, parsed_count)
}

// ============================================================================
// Persistence tests (the pure-computation tests live in the crate)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    fn sorted_paths(g: &CodeGraph) -> Vec<String> {
        let mut v: Vec<String> = g.files.iter().map(|f| f.path.clone()).collect();
        v.sort();
        v
    }

    fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
        v.sort();
        v
    }

    /// Round-trip save/load of the runner-side `PersistedCodeGraph`.
    #[test]
    fn test_persisted_code_graph_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "twinast-persist-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("src/auth.ts"),
            "export function authenticate() { return true; }\n",
        )
        .unwrap();

        // Clean any pre-existing cache for this path.
        let _ = std::fs::remove_file(crate::paths::get_code_graph_path(&repo_hash(&tmp)));

        let graph = CodeGraph::build(&tmp);
        let persisted = PersistedCodeGraph {
            version: PERSISTED_GRAPH_VERSION,
            project_path: tmp.to_string_lossy().to_string(),
            fingerprints: compute_fingerprints(&tmp, &graph),
            graph: graph.clone(),
            last_parsed_count: graph.files.len(),
        };
        persisted.save(&tmp).expect("save persisted graph");

        let loaded = PersistedCodeGraph::load(&tmp).expect("load persisted graph");
        assert_eq!(loaded.version, PERSISTED_GRAPH_VERSION);
        assert_eq!(loaded.project_path, tmp.to_string_lossy());
        assert_eq!(
            sorted_paths(&loaded.graph),
            sorted_paths(&graph),
            "loaded graph file set matches saved"
        );
        assert_eq!(loaded.fingerprints.len(), persisted.fingerprints.len());

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(crate::paths::get_code_graph_path(&repo_hash(&tmp)));
    }

    /// The persisted incremental builder re-parses only changed files across runs
    /// (load -> build -> save round-trip) and reproduces a full rebuild.
    #[test]
    fn test_build_incremental_persisted_reparses_only_changed_and_reproduces_full() {
        let tmp = std::env::temp_dir().join(format!(
            "twinast-persist-incr-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("src/auth.ts"),
            "export function authenticate() { return true; }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("src/routes.ts"),
            "import { authenticate } from './auth';\nexport const router = 1;\n",
        )
        .unwrap();
        std::fs::write(tmp.join("src/util.ts"), "export function noop() {}\n").unwrap();

        // Ensure a clean cache.
        let _ = std::fs::remove_file(crate::paths::get_code_graph_path(&repo_hash(&tmp)));

        // First build: full — parses all 3 files (no cache yet).
        let (_g1, parsed1) = build_incremental(&tmp);
        assert_eq!(parsed1, 3, "first persisted build parses all files");

        // No-op rebuild: cache hit, nothing changed -> 0 parsed.
        let (_g_noop, parsed_noop) = build_incremental(&tmp);
        assert_eq!(parsed_noop, 0, "unchanged rebuild re-parses nothing");

        // Edit ONE file (change its content so the hash differs).
        std::fs::write(
            tmp.join("src/auth.ts"),
            "export function authenticate() { return false; }\nexport function logout() {}\n",
        )
        .unwrap();

        let (g_incr, parsed2) = build_incremental(&tmp);
        assert_eq!(parsed2, 1, "editing 1 file re-parses exactly 1 file");

        // Reproduces the same graph as a full rebuild from scratch.
        let g_full = CodeGraph::build(&tmp);
        assert_eq!(
            sorted_paths(&g_incr),
            sorted_paths(&g_full),
            "incremental file set matches full rebuild"
        );
        assert_eq!(
            g_incr.functions.len(),
            g_full.functions.len(),
            "incremental functions match full rebuild"
        );
        assert!(
            g_incr.exports.iter().any(|e| e.name == "logout"),
            "edited file's new export reflected in incremental graph"
        );
        let incr_resolved: Vec<_> = g_incr
            .imports
            .iter()
            .map(|i| (i.from_file.clone(), i.resolved_target.clone()))
            .collect();
        let full_resolved: Vec<_> = g_full
            .imports
            .iter()
            .map(|i| (i.from_file.clone(), i.resolved_target.clone()))
            .collect();
        assert_eq!(
            sorted(incr_resolved),
            sorted(full_resolved),
            "incremental resolved edges match full rebuild"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(crate::paths::get_code_graph_path(&repo_hash(&tmp)));
    }
}
