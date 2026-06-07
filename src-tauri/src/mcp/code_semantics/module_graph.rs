//! Shared substrate for the CLI-backed **stochastic-kernel** twin observers
//! (`Ξ_Arch` arch-layers, `Ξ_Domain` domains — Phase 3 / Pillar 3 of the twin-ast
//! plan).
//!
//! Both observers do the same three observer-agnostic things and differ only in
//! their prompt + response shape:
//!   1. summarize the resolved `Ξ_AST` graph into per-directory **module**
//!      structure ([`ModuleSummary`] via [`summarize_modules`] /
//!      [`build_module_graph`]),
//!   2. run one structure-only classification prompt through the Claude-CLI
//!      provider — *forced*, so there is **no API key, no `@anthropic-ai/sdk`**
//!      ([`run_cli_structured`]),
//!   3. tolerantly extract the JSON answer from possibly-prose model output
//!      ([`extract_json`]).
//!
//! The kernel envelope itself (`kernel:true`, `posterior<1`, credibility
//! `(causal:medium, authorial:low, boundary:low)`) lives on
//! [`super::Envelope::kernel`]. The model only ever sees this derived structure —
//! never raw source.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::ai_provider::{run_structured_prompt, StructuredPrompt};
use crate::ai_router::TaskContext;
use crate::workflow_generation::code_graph::CodeGraph;

/// Default cap on modules sent to the model in one pass — bounds prompt size +
/// token cost. Modules beyond the cap (lowest export-count first) are omitted and
/// reflected honestly in each observer's `coverage`.
pub(super) const DEFAULT_MAX_MODULES: usize = 60;

/// Uncalibrated kernel posterior (vet Q5): a fixed "model hint" confidence, NOT a
/// measured accuracy. `kernel:true` + `authorial:low` is what tells a consumer
/// the answer is advisory; the posterior is here to be calibrated against
/// acceptance data later, not trusted today.
pub(super) const KERNEL_POSTERIOR: f64 = 0.5;

// Per-module summary caps (keep the prompt bounded regardless of repo size).
const MAX_EXPORTS_PER_MODULE: usize = 12;
const MAX_DEPS_PER_MODULE: usize = 8;
const MAX_EXTERNAL_PER_MODULE: usize = 8;
const MAX_FILES_PER_MODULE: usize = 10;

/// A module = a directory of files, summarized for a classifier. The model sees
/// only this structure, never source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModuleSummary {
    /// Module key — the directory path (repo-relative, forward slashes; `"."` for
    /// repo-root files).
    pub(super) module: String,
    /// File basenames in the module (sorted, capped).
    pub(super) files: Vec<String>,
    /// Exported symbol names declared in the module (sorted, capped).
    pub(super) exports: Vec<String>,
    /// Other in-repo module dirs this module imports from, via *resolved* edges
    /// (sorted, capped).
    pub(super) internal_deps: Vec<String>,
    /// External package specifiers the module imports (sorted, capped).
    pub(super) external_pkgs: Vec<String>,
    /// Total exports before the cap — the ranking + tie-break signal.
    pub(super) export_count: usize,
}

/// The directory component of a repo-relative path (forward slashes). Files at
/// the repo root map to `"."`.
pub(super) fn module_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => ".".to_string(),
    }
}

/// The basename of a repo-relative path.
fn basename(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, b)| b).unwrap_or(path)
}

/// Group the resolved graph's files into per-directory module summaries, sorted
/// by export count (desc), then module name (asc) for a stable, high-signal-first
/// ordering. Internal vectors are sorted + de-duplicated so the output (and its
/// fingerprint) is deterministic.
pub(super) fn summarize_modules(graph: &CodeGraph) -> Vec<ModuleSummary> {
    // Accumulate per-module sets.
    struct Acc {
        files: BTreeSet<String>,
        exports: BTreeSet<String>,
        deps: BTreeSet<String>,
        external: BTreeSet<String>,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    let ensure = |acc: &mut BTreeMap<String, Acc>, m: String| {
        acc.entry(m).or_insert_with(|| Acc {
            files: BTreeSet::new(),
            exports: BTreeSet::new(),
            deps: BTreeSet::new(),
            external: BTreeSet::new(),
        });
    };

    for f in &graph.files {
        let m = module_dir(&f.path);
        ensure(&mut acc, m.clone());
        acc.get_mut(&m)
            .unwrap()
            .files
            .insert(basename(&f.path).to_string());
    }
    for e in &graph.exports {
        let m = module_dir(&e.file_path);
        ensure(&mut acc, m.clone());
        acc.get_mut(&m).unwrap().exports.insert(e.name.clone());
    }
    for imp in &graph.imports {
        let m = module_dir(&imp.from_file);
        ensure(&mut acc, m.clone());
        match &imp.resolved_target {
            Some(target) => {
                let target_mod = module_dir(target);
                if target_mod != m {
                    acc.get_mut(&m).unwrap().deps.insert(target_mod);
                }
            }
            None => {
                use crate::workflow_generation::code_graph::ResolutionKind;
                // Only count honestly-external specifiers as external packages;
                // an `Unresolved` internal-looking edge is a coverage hole, not a
                // package signal, so it is deliberately dropped here.
                if matches!(imp.resolution, ResolutionKind::External) {
                    acc.get_mut(&m)
                        .unwrap()
                        .external
                        .insert(imp.to_module.clone());
                }
            }
        }
    }

    let mut out: Vec<ModuleSummary> = acc
        .into_iter()
        .map(|(module, a)| {
            let export_count = a.exports.len();
            ModuleSummary {
                module,
                files: cap(a.files, MAX_FILES_PER_MODULE),
                exports: cap(a.exports, MAX_EXPORTS_PER_MODULE),
                internal_deps: cap(a.deps, MAX_DEPS_PER_MODULE),
                external_pkgs: cap(a.external, MAX_EXTERNAL_PER_MODULE),
                export_count,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.export_count
            .cmp(&a.export_count)
            .then_with(|| a.module.cmp(&b.module))
    });
    out
}

/// Take the first `n` of a sorted set into a `Vec` (the set is already ordered).
fn cap(set: BTreeSet<String>, n: usize) -> Vec<String> {
    set.into_iter().take(n).collect()
}

/// A stable fingerprint of the module structure — same structure → same value, so
/// each observer's LLM pass runs once per distinct graph per process. Hashes a
/// name-ordered view of every module's (files, exports, internal deps); ignores
/// the cap-driven `export_count` ordering so only *content* changes invalidate the
/// cache.
pub(super) fn fingerprint_modules(mods: &[ModuleSummary]) -> u64 {
    let mut ordered: Vec<&ModuleSummary> = mods.iter().collect();
    ordered.sort_by(|a, b| a.module.cmp(&b.module));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for m in ordered {
        m.module.hash(&mut hasher);
        m.files.hash(&mut hasher);
        m.exports.hash(&mut hasher);
        m.internal_deps.hash(&mut hasher);
    }
    hasher.finish()
}

/// Build the resolved `Ξ_AST` graph + module summaries off the async runtime.
/// Returns `(summaries, total_module_count, fingerprint)`, or `None` if the
/// blocking build task panicked (the caller degrades to a cold envelope).
pub(super) async fn build_module_graph(dir: PathBuf) -> Option<(Vec<ModuleSummary>, usize, u64)> {
    tokio::task::spawn_blocking(move || {
        let graph = CodeGraph::build(&dir);
        let mods = summarize_modules(&graph);
        let total = mods.len();
        let fp = fingerprint_modules(&mods);
        (mods, total, fp)
    })
    .await
    .ok()
}

/// Render the numbered module-list block shared by both observers' prompts.
/// Deterministic (modules in the given order, internal lists pre-sorted) so the
/// prompt + tests are stable.
pub(super) fn render_modules(modules: &[ModuleSummary]) -> String {
    let mut s = String::new();
    for (i, m) in modules.iter().enumerate() {
        s.push_str(&format!("{}. module: {}\n", i + 1, m.module));
        if !m.files.is_empty() {
            s.push_str(&format!("   files: {}\n", m.files.join(", ")));
        }
        if !m.exports.is_empty() {
            s.push_str(&format!("   exports: {}\n", m.exports.join(", ")));
        }
        if !m.internal_deps.is_empty() {
            s.push_str(&format!(
                "   imports-from: {}\n",
                m.internal_deps.join(", ")
            ));
        }
        if !m.external_pkgs.is_empty() {
            s.push_str(&format!("   external: {}\n", m.external_pkgs.join(", ")));
        }
    }
    s
}

/// Run a structure-only classification prompt through the Claude-CLI provider,
/// off the async runtime. The provider is **forced** to `claude_cli` so the
/// observer NEVER makes a keyed API call (the operator constraint), and
/// temperature is pinned to 0 for deterministic classification. Returns the raw
/// model output on success; `None` on task panic OR provider failure (the caller
/// degrades honestly to coverage 0 — never a confident false answer).
pub(super) async fn run_cli_structured(prompt: String) -> Option<String> {
    match tokio::task::spawn_blocking(move || {
        let context = TaskContext::from_prompt(&prompt);
        let structured = StructuredPrompt::uncached(prompt);
        run_structured_prompt(
            &structured,
            &context,
            None,
            None,
            Some("claude_cli"), // provider_override — force CLI, no API key
            Some(0.0),          // temperature_override — deterministic
            None,
            None,
            None,
        )
    })
    .await
    {
        Ok(resp) if resp.success => Some(resp.output),
        _ => None,
    }
}

/// Extract the first balanced JSON object/array from a string, ignoring prose and
/// ```json code fences and respecting string literals (so a `}` inside a string
/// doesn't close the scan early).
pub(super) fn extract_json(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

// ===========================================================================
// Tests (pure — no LLM call)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::code_graph::{
        ExportNode, FileNode, ImportEdge, ResolutionKind,
    };

    fn file(path: &str) -> FileNode {
        FileNode {
            path: path.into(),
            language: "typescript".into(),
            line_count: 10,
        }
    }
    fn export(name: &str, file: &str) -> ExportNode {
        ExportNode {
            name: name.into(),
            file_path: file.into(),
            kind: "function".into(),
            line: 1,
        }
    }
    fn import(
        from: &str,
        to_module: &str,
        target: Option<&str>,
        kind: ResolutionKind,
    ) -> ImportEdge {
        ImportEdge {
            from_file: from.into(),
            to_module: to_module.into(),
            imported_names: vec![],
            line: 1,
            resolved_target: target.map(|s| s.into()),
            resolution: kind,
        }
    }

    #[test]
    fn module_dir_and_basename() {
        assert_eq!(module_dir("src/api/auth.ts"), "src/api");
        assert_eq!(module_dir("main.rs"), ".");
        assert_eq!(basename("src/api/auth.ts"), "auth.ts");
        assert_eq!(basename("main.rs"), "main.rs");
    }

    #[test]
    fn summarize_groups_files_exports_deps_and_external() {
        let graph = CodeGraph {
            files: vec![
                file("src/api/routes.ts"),
                file("src/api/handlers.ts"),
                file("src/services/auth.ts"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![
                // api → services (resolved internal dep)
                import(
                    "src/api/routes.ts",
                    "../services/auth",
                    Some("src/services/auth.ts"),
                    ResolutionKind::Relative,
                ),
                // api → external package
                import(
                    "src/api/routes.ts",
                    "express",
                    None,
                    ResolutionKind::External,
                ),
                // an unresolved internal-looking edge must NOT become an external pkg
                import(
                    "src/api/routes.ts",
                    "./missing",
                    None,
                    ResolutionKind::Unresolved,
                ),
            ],
            exports: vec![
                export("registerRoutes", "src/api/routes.ts"),
                export("handle", "src/api/handlers.ts"),
                export("authenticate", "src/services/auth.ts"),
            ],
            build_duration_ms: 0,
        };
        let mods = summarize_modules(&graph);
        // Two modules: src/api (2 exports) ranks before src/services (1 export).
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].module, "src/api");
        assert_eq!(mods[0].export_count, 2);
        assert!(mods[0].files.contains(&"routes.ts".to_string()));
        assert!(mods[0].internal_deps.contains(&"src/services".to_string()));
        assert!(mods[0].external_pkgs.contains(&"express".to_string()));
        // The Unresolved edge is dropped from external packages (coverage hole, not a pkg).
        assert!(!mods[0].external_pkgs.contains(&"./missing".to_string()));
        assert_eq!(mods[1].module, "src/services");
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        let g1 = CodeGraph {
            files: vec![file("src/a/x.ts")],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![export("foo", "src/a/x.ts")],
            build_duration_ms: 0,
        };
        let g2 = g1.clone();
        let fp1 = fingerprint_modules(&summarize_modules(&g1));
        let fp2 = fingerprint_modules(&summarize_modules(&g2));
        assert_eq!(fp1, fp2, "identical structure → identical fingerprint");

        let mut g3 = g1.clone();
        g3.exports.push(export("bar", "src/a/x.ts"));
        let fp3 = fingerprint_modules(&summarize_modules(&g3));
        assert_ne!(fp1, fp3, "an added export must change the fingerprint");
    }

    #[test]
    fn render_modules_lists_structure() {
        let mods = vec![ModuleSummary {
            module: "src/api".into(),
            files: vec!["routes.ts".into()],
            exports: vec!["registerRoutes".into()],
            internal_deps: vec!["src/services".into()],
            external_pkgs: vec!["express".into()],
            export_count: 1,
        }];
        let r = render_modules(&mods);
        assert!(r.contains("1. module: src/api"));
        assert!(r.contains("files: routes.ts"));
        assert!(r.contains("exports: registerRoutes"));
        assert!(r.contains("imports-from: src/services"));
        assert!(r.contains("external: express"));
    }

    #[test]
    fn extract_json_respects_string_literals() {
        // A `}` inside a string must not close the object early.
        let s = "prefix {\"k\":\"a}b\",\"n\":1} suffix";
        assert_eq!(extract_json(s), Some("{\"k\":\"a}b\",\"n\":1}"));
    }

    #[test]
    fn extract_json_none_on_garbage() {
        assert!(extract_json("no json here").is_none());
        assert!(extract_json("").is_none());
    }
}
