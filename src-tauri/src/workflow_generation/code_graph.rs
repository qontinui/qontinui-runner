//! Codebase Knowledge Graph
//!
//! Uses tree-sitter to parse source files and build a lightweight in-memory
//! graph of functions, classes, imports, and call relationships. Used by the
//! Discovery phase to provide rich context to downstream generation phases.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, info, warn};

// ============================================================================
// Graph types
// ============================================================================

/// A lightweight codebase knowledge graph built from tree-sitter AST analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraph {
    pub files: Vec<FileNode>,
    pub functions: Vec<FunctionNode>,
    pub classes: Vec<ClassNode>,
    pub imports: Vec<ImportEdge>,
    pub exports: Vec<ExportNode>,
    /// Total parse time in milliseconds
    pub build_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub path: String,
    pub language: String,
    pub line_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionNode {
    pub name: String,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub is_exported: bool,
    pub is_async: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassNode {
    pub name: String,
    pub file_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub is_exported: bool,
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEdge {
    pub from_file: String,
    pub to_module: String,
    pub imported_names: Vec<String>,
    pub line: usize,
    /// Repo-relative file path the specifier resolves to, or `None` if external/unresolved.
    /// Same normalization as `FileNode.path` (forward slashes, project-prefix-stripped).
    pub resolved_target: Option<String>,
    /// How (or whether) `to_module` was bound to a file by the resolver pass.
    pub resolution: ResolutionKind,
}

/// How an `ImportEdge`'s specifier was bound to a file by the deterministic resolver.
///
/// `External` is honest — a third-party/stdlib specifier with no in-repo target
/// (not a failure). `Unresolved` flags an *internal-looking* specifier the resolver
/// could not bind: a coverage hole to surface, never to hide.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionKind {
    /// TS/JS relative specifier (`./`, `../`) resolved against the importing file's dir.
    Relative,
    /// TS/JS bare specifier resolved via tsconfig `paths`/`baseUrl` alias.
    TsconfigPath,
    /// TS/JS specifier resolved to a directory `index.*` entry.
    PackageIndex,
    /// Python dotted/relative module resolved to a `.py`/`__init__.py` file.
    PythonModule,
    /// Rust `mod`/`use` path resolved within the crate.
    RustMod,
    /// Third-party package / stdlib — honestly external, no in-repo target.
    External,
    /// Internal-looking specifier the resolver could not bind (a coverage hole).
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportNode {
    pub name: String,
    pub file_path: String,
    pub kind: String, // "function", "class", "variable", "type"
    pub line: usize,
}

// ============================================================================
// Blast radius analysis
// ============================================================================

/// Result of blast radius analysis for a set of changed files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlastRadius {
    /// Files that directly import from changed files
    pub directly_affected: Vec<String>,
    /// Files that import from directly affected files (2-hop)
    pub transitively_affected: Vec<String>,
    /// Specific exported symbols from changed files
    pub affected_exports: Vec<String>,
    /// Overall risk level based on fan-out
    pub risk_level: RiskLevel,
    /// Total number of files potentially impacted
    pub total_impact_count: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,    // 0-2 affected files
    Medium, // 3-7 affected files
    High,   // 8+ affected files
}

// ============================================================================
// Graph building
// ============================================================================

impl CodeGraph {
    /// Build a code graph from a project directory.
    ///
    /// Parses TypeScript/JavaScript, Python, and Rust files using tree-sitter.
    /// Skips: node_modules, target, .git, dist, build, __pycache__, .venv
    pub fn build(project_path: &Path) -> Self {
        let start = std::time::Instant::now();
        let mut graph = CodeGraph {
            files: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            build_duration_ms: 0,
        };

        let skip_dirs = [
            "node_modules",
            "target",
            ".git",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            "venv",
            ".next",
            ".turbo",
            "coverage",
            ".worktrees",
        ];

        // Walk source files
        let source_files = collect_source_files(project_path, &skip_dirs);
        info!("CodeGraph: scanning {} source files", source_files.len());

        for file_path in &source_files {
            let rel_path = file_path
                .strip_prefix(project_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");

            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let line_count = content.lines().count();
            let language = match ext {
                "ts" | "tsx" => "typescript",
                "js" | "jsx" => "javascript",
                "py" => "python",
                "rs" => "rust",
                _ => continue,
            };

            graph.files.push(FileNode {
                path: rel_path.clone(),
                language: language.to_string(),
                line_count,
            });

            match (language, ext) {
                ("typescript", "tsx") | ("javascript", "jsx") => {
                    parse_typescript(&content, &rel_path, &mut graph, true);
                }
                ("typescript" | "javascript", _) => {
                    parse_typescript(&content, &rel_path, &mut graph, false);
                }
                ("python", _) => {
                    parse_python(&content, &rel_path, &mut graph);
                }
                ("rust", _) => {
                    parse_rust(&content, &rel_path, &mut graph);
                }
                _ => {}
            }
        }

        // Resolve imports once over the fully-walked graph (the import map).
        let resolver = super::import_resolver::ImportResolver::new(&graph, project_path);
        resolver.resolve_graph(&mut graph);

        graph.build_duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "CodeGraph built in {}ms: {} files, {} functions, {} classes, {} imports, {} exports",
            graph.build_duration_ms,
            graph.files.len(),
            graph.functions.len(),
            graph.classes.len(),
            graph.imports.len(),
            graph.exports.len(),
        );

        graph
    }

    /// Compute blast radius for a set of changed files.
    ///
    /// Uses the resolved import graph (`ImportEdge.resolved_target`) for exact
    /// dependency lookups — no substring heuristics. An edge contributes to the
    /// blast radius only when its `resolved_target` is exactly a file in the
    /// affected set, so alias/index imports are caught and substring collisions
    /// (e.g. `to_module:"user"` vs `user_settings.ts`) never produce false hits.
    pub fn blast_radius(&self, changed_files: &[String]) -> BlastRadius {
        let changed_set: std::collections::HashSet<&str> =
            changed_files.iter().map(|s| s.as_str()).collect();

        // Find exports from changed files
        let affected_exports: Vec<String> = self
            .exports
            .iter()
            .filter(|e| changed_set.contains(e.file_path.as_str()))
            .map(|e| format!("{} ({})", e.name, e.file_path))
            .collect();

        // Find files that import from changed files (1-hop) via exact resolved targets.
        let directly_affected: Vec<String> = self
            .imports
            .iter()
            .filter(|imp| {
                imp.resolved_target
                    .as_deref()
                    .map(|t| changed_set.contains(t))
                    .unwrap_or(false)
            })
            .map(|imp| imp.from_file.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|f| !changed_set.contains(f.as_str()))
            .collect();

        // Find files that import from directly affected files (2-hop).
        let direct_set: std::collections::HashSet<&str> =
            directly_affected.iter().map(|s| s.as_str()).collect();
        let transitively_affected: Vec<String> = self
            .imports
            .iter()
            .filter(|imp| {
                imp.resolved_target
                    .as_deref()
                    .map(|t| direct_set.contains(t))
                    .unwrap_or(false)
            })
            .map(|imp| imp.from_file.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|f| !changed_set.contains(f.as_str()) && !direct_set.contains(f.as_str()))
            .collect();

        let total = directly_affected.len() + transitively_affected.len();
        let risk_level = match total {
            0..=2 => RiskLevel::Low,
            3..=7 => RiskLevel::Medium,
            _ => RiskLevel::High,
        };

        BlastRadius {
            directly_affected,
            transitively_affected,
            affected_exports,
            risk_level,
            total_impact_count: total,
        }
    }

    /// Format the code graph as context for the Builder agent prompt.
    pub fn format_for_prompt(&self, description: &str) -> String {
        let mut output = String::from("## Codebase Structure (from AST analysis)\n\n");

        // Summary stats
        output.push_str(&format!(
            "**Project:** {} files ({} TypeScript, {} Python, {} Rust), {} functions, {} classes\n\n",
            self.files.len(),
            self.files
                .iter()
                .filter(|f| f.language == "typescript" || f.language == "javascript")
                .count(),
            self.files
                .iter()
                .filter(|f| f.language == "python")
                .count(),
            self.files
                .iter()
                .filter(|f| f.language == "rust")
                .count(),
            self.functions.len(),
            self.classes.len(),
        ));

        // Find relevant functions/classes based on description keywords
        let desc_lower = description.to_lowercase();
        let keywords: Vec<&str> = desc_lower
            .split_whitespace()
            .filter(|w| w.len() >= 3)
            .filter(|w| !["the", "and", "for", "that", "with", "from", "this"].contains(w))
            .collect();

        // Show relevant exports
        let relevant_exports: Vec<&ExportNode> = self
            .exports
            .iter()
            .filter(|e| {
                let name_lower = e.name.to_lowercase();
                keywords.iter().any(|k| name_lower.contains(k))
            })
            .take(20)
            .collect();

        if !relevant_exports.is_empty() {
            output.push_str("### Relevant Exports\n");
            for exp in &relevant_exports {
                output.push_str(&format!(
                    "- `{}` ({}) in `{}`\n",
                    exp.name, exp.kind, exp.file_path
                ));
            }
            output.push('\n');
        }

        // Show relevant functions
        let relevant_fns: Vec<&FunctionNode> = self
            .functions
            .iter()
            .filter(|f| {
                let name_lower = f.name.to_lowercase();
                keywords.iter().any(|k| name_lower.contains(k))
            })
            .take(15)
            .collect();

        if !relevant_fns.is_empty() {
            output.push_str("### Relevant Functions\n");
            for func in &relevant_fns {
                let async_marker = if func.is_async { "async " } else { "" };
                let export_marker = if func.is_exported { " (exported)" } else { "" };
                output.push_str(&format!(
                    "- `{}{}` in `{}` (lines {}-{}){}\n",
                    async_marker,
                    func.name,
                    func.file_path,
                    func.line_start,
                    func.line_end,
                    export_marker
                ));
            }
            output.push('\n');
        }

        // Show relevant classes
        let relevant_classes: Vec<&ClassNode> = self
            .classes
            .iter()
            .filter(|c| {
                let name_lower = c.name.to_lowercase();
                keywords.iter().any(|k| name_lower.contains(k))
            })
            .take(10)
            .collect();

        if !relevant_classes.is_empty() {
            output.push_str("### Relevant Classes/Structs\n");
            for cls in &relevant_classes {
                let methods = if cls.methods.is_empty() {
                    String::new()
                } else {
                    format!(" — methods: {}", cls.methods.join(", "))
                };
                output.push_str(&format!(
                    "- `{}` in `{}` (lines {}-{}){}\n",
                    cls.name, cls.file_path, cls.line_start, cls.line_end, methods
                ));
            }
            output.push('\n');
        }

        // Show import structure (top importing files)
        let mut import_counts: HashMap<&str, usize> = HashMap::new();
        for imp in &self.imports {
            *import_counts.entry(imp.from_file.as_str()).or_default() += 1;
        }
        let mut top_importers: Vec<(&&str, &usize)> = import_counts.iter().collect();
        top_importers.sort_by(|a, b| b.1.cmp(a.1));

        if !top_importers.is_empty() {
            output.push_str("### Key Files (by import count)\n");
            for (file, count) in top_importers.iter().take(10) {
                output.push_str(&format!("- `{}` ({} imports)\n", file, count));
            }
            output.push('\n');
        }

        output
    }

    /// Check if the graph is empty (no meaningful content parsed).
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty() && self.classes.is_empty() && self.imports.is_empty()
    }

    /// Format blast radius analysis as regression criteria hints for the Specification phase.
    ///
    /// Given a set of changed files (inferred from the task description), produces
    /// structured text that the specification agent can use to auto-generate
    /// regression criteria for affected callers.
    pub fn format_blast_radius_for_specification(&self, description: &str) -> Option<String> {
        // Infer changed files from description keywords matching file paths
        let desc_lower = description.to_lowercase();
        let candidate_files: Vec<String> = self
            .files
            .iter()
            .filter(|f| {
                let file_name = f
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&f.path)
                    .trim_end_matches(".ts")
                    .trim_end_matches(".tsx")
                    .trim_end_matches(".py")
                    .trim_end_matches(".rs")
                    .trim_end_matches(".js")
                    .trim_end_matches(".jsx")
                    .to_lowercase();
                // Match if the file stem (>= 4 chars to avoid false positives) appears in description
                file_name.len() >= 4 && desc_lower.contains(&file_name)
            })
            .map(|f| f.path.clone())
            .collect();

        if candidate_files.is_empty() {
            return None;
        }

        let br = self.blast_radius(&candidate_files);
        if br.total_impact_count == 0 && br.affected_exports.is_empty() {
            return None;
        }

        let mut output =
            String::from("## Blast Radius Analysis (auto-generated regression hints)\n\n");
        output.push_str(&format!(
            "**Risk Level:** {:?} ({} files potentially affected)\n\n",
            br.risk_level, br.total_impact_count
        ));

        if !br.affected_exports.is_empty() {
            output.push_str(
                "**Exported symbols in changed files** (callers may need regression checks):\n",
            );
            for exp in &br.affected_exports {
                output.push_str(&format!("- {}\n", exp));
            }
            output.push('\n');
        }

        if !br.directly_affected.is_empty() {
            output.push_str("**Directly importing files** (1-hop, high regression risk):\n");
            for f in &br.directly_affected {
                output.push_str(&format!("- `{}`\n", f));
            }
            output.push('\n');
        }

        if !br.transitively_affected.is_empty() {
            output.push_str("**Transitively affected files** (2-hop, moderate regression risk):\n");
            for f in br.transitively_affected.iter().take(10) {
                output.push_str(&format!("- `{}`\n", f));
            }
            if br.transitively_affected.len() > 10 {
                output.push_str(&format!(
                    "- ... and {} more\n",
                    br.transitively_affected.len() - 10
                ));
            }
            output.push('\n');
        }

        output.push_str(
            "**Recommendation:** Generate regression acceptance criteria ensuring \
             the above importing files continue to function correctly after changes.\n",
        );

        Some(output)
    }
}

// ============================================================================
// Cached code graph with mtime-based invalidation
// ============================================================================

/// Cached wrapper around CodeGraph with file mtime invalidation and incremental updates.
#[derive(Debug, Clone)]
pub struct CachedCodeGraph {
    pub graph: CodeGraph,
    /// File path -> last known modification time
    pub file_mtimes: HashMap<String, SystemTime>,
    /// When this cache was built
    pub built_at: SystemTime,
    /// Project path this cache was built for
    pub project_path: PathBuf,
}

impl CachedCodeGraph {
    /// Build a new cached code graph, recording mtimes for all parsed files.
    pub fn build(project_path: &Path) -> Self {
        let graph = CodeGraph::build(project_path);
        let mut file_mtimes = HashMap::new();

        for file_node in &graph.files {
            let full_path = project_path.join(&file_node.path);
            if let Ok(meta) = std::fs::metadata(&full_path) {
                if let Ok(mtime) = meta.modified() {
                    file_mtimes.insert(file_node.path.clone(), mtime);
                }
            }
        }

        CachedCodeGraph {
            graph,
            file_mtimes,
            built_at: SystemTime::now(),
            project_path: project_path.to_path_buf(),
        }
    }

    /// Check if any source file has been modified since the cache was built.
    pub fn is_stale(&self) -> bool {
        for (rel_path, cached_mtime) in &self.file_mtimes {
            let full_path = self.project_path.join(rel_path);
            match std::fs::metadata(&full_path) {
                Ok(meta) => {
                    if let Ok(current_mtime) = meta.modified() {
                        if current_mtime > *cached_mtime {
                            debug!("Cache stale: {} modified", rel_path);
                            return true;
                        }
                    }
                }
                Err(_) => {
                    // File was deleted
                    debug!("Cache stale: {} deleted", rel_path);
                    return true;
                }
            }
        }

        // Also check for new source files not in the cache
        let skip_dirs = [
            "node_modules",
            "target",
            ".git",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            "venv",
            ".next",
            ".turbo",
            "coverage",
            ".worktrees",
        ];
        let current_files = collect_source_files(&self.project_path, &skip_dirs);
        for file_path in &current_files {
            let rel_path = file_path
                .strip_prefix(&self.project_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");
            if !self.file_mtimes.contains_key(&rel_path) {
                debug!("Cache stale: new file {}", rel_path);
                return true;
            }
        }

        false
    }

    /// Incrementally update only the changed files, keeping the rest of the graph intact.
    pub fn incremental_update(&mut self) {
        let mut changed_files = Vec::new();
        let mut deleted_files = Vec::new();

        // Detect changed and deleted files
        for (rel_path, cached_mtime) in &self.file_mtimes {
            let full_path = self.project_path.join(rel_path);
            match std::fs::metadata(&full_path) {
                Ok(meta) => {
                    if let Ok(current_mtime) = meta.modified() {
                        if current_mtime > *cached_mtime {
                            changed_files.push(rel_path.clone());
                        }
                    }
                }
                Err(_) => {
                    deleted_files.push(rel_path.clone());
                }
            }
        }

        // Detect new files
        let skip_dirs = [
            "node_modules",
            "target",
            ".git",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            "venv",
            ".next",
            ".turbo",
            "coverage",
            ".worktrees",
        ];
        let current_files = collect_source_files(&self.project_path, &skip_dirs);
        let mut new_files = Vec::new();
        for file_path in &current_files {
            let rel_path = file_path
                .strip_prefix(&self.project_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");
            if !self.file_mtimes.contains_key(&rel_path) {
                new_files.push(rel_path);
            }
        }

        if changed_files.is_empty() && deleted_files.is_empty() && new_files.is_empty() {
            return;
        }

        info!(
            "CodeGraph incremental update: {} changed, {} deleted, {} new files",
            changed_files.len(),
            deleted_files.len(),
            new_files.len()
        );

        // Remove data for deleted and changed files
        let remove_set: std::collections::HashSet<&str> = changed_files
            .iter()
            .chain(deleted_files.iter())
            .map(|s| s.as_str())
            .collect();

        self.graph
            .files
            .retain(|f| !remove_set.contains(f.path.as_str()));
        self.graph
            .functions
            .retain(|f| !remove_set.contains(f.file_path.as_str()));
        self.graph
            .classes
            .retain(|c| !remove_set.contains(c.file_path.as_str()));
        self.graph
            .imports
            .retain(|i| !remove_set.contains(i.from_file.as_str()));
        self.graph
            .exports
            .retain(|e| !remove_set.contains(e.file_path.as_str()));

        for path in &deleted_files {
            self.file_mtimes.remove(path);
        }

        // Re-parse changed and new files
        let files_to_parse: Vec<String> = changed_files.into_iter().chain(new_files).collect();
        let start = std::time::Instant::now();

        for rel_path in &files_to_parse {
            let full_path = self.project_path.join(rel_path);
            let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");

            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Skip very large files
            if content.len() > 100_000 {
                continue;
            }

            let line_count = content.lines().count();
            let language = match ext {
                "ts" | "tsx" => "typescript",
                "js" | "jsx" => "javascript",
                "py" => "python",
                "rs" => "rust",
                _ => continue,
            };

            self.graph.files.push(FileNode {
                path: rel_path.clone(),
                language: language.to_string(),
                line_count,
            });

            match (language, ext) {
                ("typescript", "tsx") | ("javascript", "jsx") => {
                    parse_typescript(&content, rel_path, &mut self.graph, true);
                }
                ("typescript" | "javascript", _) => {
                    parse_typescript(&content, rel_path, &mut self.graph, false);
                }
                ("python", _) => {
                    parse_python(&content, rel_path, &mut self.graph);
                }
                ("rust", _) => {
                    parse_rust(&content, rel_path, &mut self.graph);
                }
                _ => {}
            }

            // Update mtime
            if let Ok(meta) = std::fs::metadata(&full_path) {
                if let Ok(mtime) = meta.modified() {
                    self.file_mtimes.insert(rel_path.clone(), mtime);
                }
            }
        }

        // Re-resolve imports over the updated graph. Resolution is in-memory and
        // cheap; this reproduces the same resolved edges a full rebuild would
        // produce (the parse count above is what's bounded to changed files).
        let resolver = super::import_resolver::ImportResolver::new(&self.graph, &self.project_path);
        resolver.resolve_graph(&mut self.graph);

        self.graph.build_duration_ms = start.elapsed().as_millis() as u64;
        self.built_at = SystemTime::now();

        info!(
            "CodeGraph incremental update complete in {}ms: {} files re-parsed",
            self.graph.build_duration_ms,
            files_to_parse.len()
        );
    }

    /// Get or refresh the graph, returning a reference to the inner CodeGraph.
    pub fn get_or_refresh(&mut self) -> &CodeGraph {
        if self.is_stale() {
            self.incremental_update();
        }
        &self.graph
    }
}

// ============================================================================
// Fingerprint-incremental persistence (resolved Ξ_AST, app-data local file)
// ============================================================================

/// Per-file content fingerprint used for incremental invalidation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFingerprint {
    /// File size in bytes.
    pub size: u64,
    /// Modification time in seconds since the unix epoch (best-effort; 0 if unavailable).
    pub mtime_secs: u64,
    /// SHA-256 of the file contents (hex). The authoritative change signal —
    /// mtime+size are a fast pre-check, the hash is the tie-breaker so a rebuild
    /// reproduces the exact same graph regardless of clock skew.
    pub hash: String,
}

/// The resolved code graph plus per-file fingerprints, persisted under app-data
/// so a rebuild re-parses only changed files (UA's knowledge-graph.json pattern).
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

/// Stable repo hash for the cache filename: SHA-256 of the normalized absolute path.
pub fn repo_hash(project_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let normalized = project_path
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn fingerprint_content(content: &str, meta: Option<&std::fs::Metadata>) -> FileFingerprint {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = hex_encode(&hasher.finalize());
    let size = meta.map(|m| m.len()).unwrap_or(content.len() as u64);
    let mtime_secs = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    FileFingerprint {
        size,
        mtime_secs,
        hash,
    }
}

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

impl CodeGraph {
    /// Build the resolved code graph using the persisted app-data cache for
    /// fingerprint-incremental re-analysis: only files whose content fingerprint
    /// changed (or new files) are re-parsed; deleted files are dropped; imports
    /// are then re-resolved over the whole graph. The result is identical to a
    /// full [`CodeGraph::build`].
    ///
    /// Returns the resolved graph and the number of files parsed this run
    /// (0 if nothing changed since the last persisted build).
    pub fn build_incremental(project_path: &Path) -> (CodeGraph, usize) {
        let start = std::time::Instant::now();
        let skip_dirs = [
            "node_modules",
            "target",
            ".git",
            "dist",
            "build",
            "__pycache__",
            ".venv",
            "venv",
            ".next",
            ".turbo",
            "coverage",
            ".worktrees",
        ];

        let source_files = collect_source_files(project_path, &skip_dirs);

        // Build current relative-path -> (full path, content, fingerprint) map.
        let mut current: HashMap<String, (PathBuf, String, FileFingerprint)> = HashMap::new();
        for file_path in &source_files {
            let rel_path = file_path
                .strip_prefix(project_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .replace('\\', "/");
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "py" | "rs") {
                continue;
            }
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let meta = std::fs::metadata(file_path).ok();
            let fp = fingerprint_content(&content, meta.as_ref());
            current.insert(rel_path, (file_path.clone(), content, fp));
        }

        // Load the previous persisted graph (if any & valid).
        let prev = PersistedCodeGraph::load(project_path);

        let (mut graph, mut fingerprints, to_parse): (
            CodeGraph,
            HashMap<String, FileFingerprint>,
            Vec<String>,
        ) = match prev {
            Some(p) if p.project_path == project_path.to_string_lossy() => {
                let mut graph = p.graph;
                let mut fingerprints = p.fingerprints;

                // Drop data for deleted files.
                let deleted: Vec<String> = fingerprints
                    .keys()
                    .filter(|k| !current.contains_key(*k))
                    .cloned()
                    .collect();
                if !deleted.is_empty() {
                    let del_set: std::collections::HashSet<&str> =
                        deleted.iter().map(|s| s.as_str()).collect();
                    graph.files.retain(|f| !del_set.contains(f.path.as_str()));
                    graph
                        .functions
                        .retain(|f| !del_set.contains(f.file_path.as_str()));
                    graph
                        .classes
                        .retain(|c| !del_set.contains(c.file_path.as_str()));
                    graph
                        .imports
                        .retain(|i| !del_set.contains(i.from_file.as_str()));
                    graph
                        .exports
                        .retain(|e| !del_set.contains(e.file_path.as_str()));
                    for d in &deleted {
                        fingerprints.remove(d);
                    }
                }

                // Changed + new files (fingerprint differs).
                let to_parse: Vec<String> = current
                    .iter()
                    .filter(|(rel, (_, _, fp))| {
                        fingerprints.get(*rel).map(|old| old != fp).unwrap_or(true)
                    })
                    .map(|(rel, _)| rel.clone())
                    .collect();

                // Remove stale per-file data for changed files before re-parsing.
                let change_set: std::collections::HashSet<&str> =
                    to_parse.iter().map(|s| s.as_str()).collect();
                graph
                    .files
                    .retain(|f| !change_set.contains(f.path.as_str()));
                graph
                    .functions
                    .retain(|f| !change_set.contains(f.file_path.as_str()));
                graph
                    .classes
                    .retain(|c| !change_set.contains(c.file_path.as_str()));
                graph
                    .imports
                    .retain(|i| !change_set.contains(i.from_file.as_str()));
                graph
                    .exports
                    .retain(|e| !change_set.contains(e.file_path.as_str()));

                (graph, fingerprints, to_parse)
            }
            _ => {
                // No valid cache — full build (parse everything).
                let graph = CodeGraph {
                    files: Vec::new(),
                    functions: Vec::new(),
                    classes: Vec::new(),
                    imports: Vec::new(),
                    exports: Vec::new(),
                    build_duration_ms: 0,
                };
                let to_parse: Vec<String> = current.keys().cloned().collect();
                (graph, HashMap::new(), to_parse)
            }
        };

        // Parse only the changed/new files.
        let parsed_count = to_parse.len();
        for rel_path in &to_parse {
            if let Some((full_path, content, fp)) = current.get(rel_path) {
                parse_file_into(&mut graph, rel_path, full_path, content);
                fingerprints.insert(rel_path.clone(), fp.clone());
            }
        }

        // Re-resolve imports over the whole (updated) graph — produces the same
        // resolved edges as a full rebuild.
        let resolver = super::import_resolver::ImportResolver::new(&graph, project_path);
        resolver.resolve_graph(&mut graph);

        graph.build_duration_ms = start.elapsed().as_millis() as u64;

        // Persist.
        let persisted = PersistedCodeGraph {
            version: PERSISTED_GRAPH_VERSION,
            project_path: project_path.to_string_lossy().to_string(),
            fingerprints,
            graph: graph.clone(),
            last_parsed_count: parsed_count,
        };
        if let Err(e) = persisted.save(project_path) {
            warn!("CodeGraph: failed to persist resolved graph: {}", e);
        }

        info!(
            "CodeGraph incremental build in {}ms: {} files parsed ({} total)",
            graph.build_duration_ms,
            parsed_count,
            graph.files.len()
        );

        (graph, parsed_count)
    }
}

/// Parse a single source file into the graph (language dispatch shared by the
/// full and incremental builders). Adds the FileNode and all parsed entities.
fn parse_file_into(graph: &mut CodeGraph, rel_path: &str, full_path: &Path, content: &str) {
    let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let line_count = content.lines().count();
    let language = match ext {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "rs" => "rust",
        _ => return,
    };

    graph.files.push(FileNode {
        path: rel_path.to_string(),
        language: language.to_string(),
        line_count,
    });

    match (language, ext) {
        ("typescript", "tsx") | ("javascript", "jsx") => {
            parse_typescript(content, rel_path, graph, true);
        }
        ("typescript" | "javascript", _) => {
            parse_typescript(content, rel_path, graph, false);
        }
        ("python", _) => {
            parse_python(content, rel_path, graph);
        }
        ("rust", _) => {
            parse_rust(content, rel_path, graph);
        }
        _ => {}
    }
}

// ============================================================================
// File collection
// ============================================================================

fn collect_source_files(root: &Path, skip_dirs: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_recursive(root, skip_dirs, &mut files, 0);
    files
}

fn collect_files_recursive(dir: &Path, skip_dirs: &[&str], files: &mut Vec<PathBuf>, depth: usize) {
    if depth > 10 {
        return;
    } // Prevent infinite recursion

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if path.is_dir() {
            if skip_dirs.iter().any(|s| name == *s) || name.starts_with('.') {
                continue;
            }
            collect_files_recursive(&path, skip_dirs, files, depth + 1);
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "ts" | "tsx" | "js" | "jsx" | "py" | "rs") {
                // Skip very large files (>100KB) to avoid slow parsing
                if let Ok(meta) = std::fs::metadata(&path) {
                    if meta.len() > 100_000 {
                        continue;
                    }
                }
                files.push(path);
            }
        }
    }
}

// ============================================================================
// Language-specific parsers
// ============================================================================

fn parse_typescript(content: &str, file_path: &str, graph: &mut CodeGraph, is_tsx: bool) {
    let mut parser = tree_sitter::Parser::new();
    let language = if is_tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    if parser.set_language(&language.into()).is_err() {
        return;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return,
    };

    let root = tree.root_node();
    let bytes = content.as_bytes();

    // Walk all top-level children
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            // Function declarations
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    let is_async = node.child(0).map(|c| c.kind() == "async").unwrap_or(false);
                    graph.functions.push(FunctionNode {
                        name,
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported: false,
                        is_async,
                    });
                }
            }
            // Export statements
            "export_statement" => {
                // Check for exported function/class/variable
                if let Some(declaration) = node.child_by_field_name("declaration") {
                    match declaration.kind() {
                        "function_declaration" => {
                            if let Some(name_node) = declaration.child_by_field_name("name") {
                                let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                                let is_async = declaration
                                    .child(0)
                                    .map(|c| c.kind() == "async")
                                    .unwrap_or(false);
                                graph.functions.push(FunctionNode {
                                    name: name.clone(),
                                    file_path: file_path.to_string(),
                                    line_start: declaration.start_position().row + 1,
                                    line_end: declaration.end_position().row + 1,
                                    is_exported: true,
                                    is_async,
                                });
                                graph.exports.push(ExportNode {
                                    name,
                                    file_path: file_path.to_string(),
                                    kind: "function".to_string(),
                                    line: declaration.start_position().row + 1,
                                });
                            }
                        }
                        "class_declaration" => {
                            if let Some(name_node) = declaration.child_by_field_name("name") {
                                let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                                let methods = extract_ts_class_methods(&declaration, bytes);
                                graph.classes.push(ClassNode {
                                    name: name.clone(),
                                    file_path: file_path.to_string(),
                                    line_start: declaration.start_position().row + 1,
                                    line_end: declaration.end_position().row + 1,
                                    is_exported: true,
                                    methods,
                                });
                                graph.exports.push(ExportNode {
                                    name,
                                    file_path: file_path.to_string(),
                                    kind: "class".to_string(),
                                    line: declaration.start_position().row + 1,
                                });
                            }
                        }
                        "lexical_declaration" => {
                            // export const/let/var
                            let mut decl_cursor = declaration.walk();
                            for child in declaration.children(&mut decl_cursor) {
                                if child.kind() == "variable_declarator" {
                                    if let Some(name_node) = child.child_by_field_name("name") {
                                        let name =
                                            name_node.utf8_text(bytes).unwrap_or("").to_string();
                                        graph.exports.push(ExportNode {
                                            name,
                                            file_path: file_path.to_string(),
                                            kind: "variable".to_string(),
                                            line: child.start_position().row + 1,
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Import statements
            "import_statement" => {
                let source = node
                    .child_by_field_name("source")
                    .and_then(|s| s.utf8_text(bytes).ok())
                    .map(|s| s.trim_matches(|c| c == '\'' || c == '"').to_string())
                    .unwrap_or_default();

                let mut imported_names = Vec::new();
                let mut imp_cursor = node.walk();
                for child in node.children(&mut imp_cursor) {
                    if child.kind() == "import_clause" {
                        let mut clause_cursor = child.walk();
                        for clause_child in child.children(&mut clause_cursor) {
                            if clause_child.kind() == "named_imports" {
                                let mut named_cursor = clause_child.walk();
                                for spec in clause_child.children(&mut named_cursor) {
                                    if spec.kind() == "import_specifier" {
                                        if let Some(name_node) = spec.child_by_field_name("name") {
                                            if let Ok(name) = name_node.utf8_text(bytes) {
                                                imported_names.push(name.to_string());
                                            }
                                        }
                                    }
                                }
                            } else if clause_child.kind() == "identifier" {
                                if let Ok(name) = clause_child.utf8_text(bytes) {
                                    imported_names.push(name.to_string());
                                }
                            }
                        }
                    }
                }

                if !source.is_empty() {
                    graph.imports.push(ImportEdge {
                        from_file: file_path.to_string(),
                        to_module: source,
                        imported_names,
                        line: node.start_position().row + 1,
                        resolved_target: None,
                        resolution: ResolutionKind::Unresolved,
                    });
                }
            }
            // Class declarations (non-exported)
            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    let methods = extract_ts_class_methods(&node, bytes);
                    graph.classes.push(ClassNode {
                        name,
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported: false,
                        methods,
                    });
                }
            }
            _ => {}
        }
    }
}

fn extract_ts_class_methods(class_node: &tree_sitter::Node, bytes: &[u8]) -> Vec<String> {
    let mut methods = Vec::new();
    if let Some(body) = class_node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "method_definition" || child.kind() == "public_field_definition" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(bytes) {
                        methods.push(name.to_string());
                    }
                }
            }
        }
    }
    methods
}

fn parse_python(content: &str, file_path: &str, graph: &mut CodeGraph) {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_python::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return,
    };

    let root = tree.root_node();
    let bytes = content.as_bytes();

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    // In Python, async is a separate node wrapping the function_definition
                    // at top-level, but tree-sitter may parse `async def` as just
                    // "function_definition" with a child. Check the text.
                    let node_text = node.utf8_text(bytes).unwrap_or("");
                    let is_async = node_text.starts_with("async ");
                    // In Python, top-level functions without _ prefix are effectively exported
                    let is_exported = !name.starts_with('_');
                    graph.functions.push(FunctionNode {
                        name: name.clone(),
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported,
                        is_async,
                    });
                    if is_exported {
                        graph.exports.push(ExportNode {
                            name,
                            file_path: file_path.to_string(),
                            kind: "function".to_string(),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    let is_exported = !name.starts_with('_');
                    let methods = extract_py_class_methods(&node, bytes);
                    graph.classes.push(ClassNode {
                        name: name.clone(),
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported,
                        methods,
                    });
                    if is_exported {
                        graph.exports.push(ExportNode {
                            name,
                            file_path: file_path.to_string(),
                            kind: "class".to_string(),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
            "import_statement" | "import_from_statement" => {
                let full_text = node.utf8_text(bytes).unwrap_or("").to_string();
                // Simple extraction: get module name from "from X import Y" or "import X"
                let parts: Vec<&str> = full_text.split_whitespace().collect();
                if parts.len() >= 2 {
                    let (module, names) = if parts[0] == "from" && parts.len() >= 4 {
                        let module = parts[1].to_string();
                        let names: Vec<String> = parts[3..]
                            .iter()
                            .map(|s| s.trim_matches(',').to_string())
                            .filter(|s| !s.is_empty() && s != "import")
                            .collect();
                        (module, names)
                    } else {
                        (parts[1].trim_matches(',').to_string(), vec![])
                    };

                    graph.imports.push(ImportEdge {
                        from_file: file_path.to_string(),
                        to_module: module,
                        imported_names: names,
                        line: node.start_position().row + 1,
                        resolved_target: None,
                        resolution: ResolutionKind::Unresolved,
                    });
                }
            }
            _ => {}
        }
    }
}

fn extract_py_class_methods(class_node: &tree_sitter::Node, bytes: &[u8]) -> Vec<String> {
    let mut methods = Vec::new();
    if let Some(body) = class_node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "function_definition" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(bytes) {
                        methods.push(name.to_string());
                    }
                }
            }
        }
    }
    methods
}

fn parse_rust(content: &str, file_path: &str, graph: &mut CodeGraph) {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return,
    };

    let root = tree.root_node();
    let bytes = content.as_bytes();

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            "function_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    let is_pub = node
                        .child(0)
                        .map(|c| c.kind() == "visibility_modifier")
                        .unwrap_or(false);
                    let is_async = content[node.byte_range()].contains("async fn");
                    graph.functions.push(FunctionNode {
                        name: name.clone(),
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported: is_pub,
                        is_async,
                    });
                    if is_pub {
                        graph.exports.push(ExportNode {
                            name,
                            file_path: file_path.to_string(),
                            kind: "function".to_string(),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
            "struct_item" | "enum_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                    let is_pub = node
                        .child(0)
                        .map(|c| c.kind() == "visibility_modifier")
                        .unwrap_or(false);
                    let kind = if node.kind() == "struct_item" {
                        "struct"
                    } else {
                        "enum"
                    };
                    graph.classes.push(ClassNode {
                        name: name.clone(),
                        file_path: file_path.to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_exported: is_pub,
                        methods: vec![],
                    });
                    if is_pub {
                        graph.exports.push(ExportNode {
                            name,
                            file_path: file_path.to_string(),
                            kind: kind.to_string(),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
            "use_declaration" => {
                let text = node.utf8_text(bytes).unwrap_or("").to_string();
                // Extract module path from "use crate::foo::bar" or "use std::collections::HashMap"
                let path = text
                    .trim_start_matches("use ")
                    .trim_end_matches(';')
                    .to_string();
                let module = path.split("::").take(2).collect::<Vec<_>>().join("::");
                let names: Vec<String> = if path.contains('{') {
                    // use foo::{A, B}
                    path.split('{')
                        .nth(1)
                        .unwrap_or("")
                        .trim_end_matches('}')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                } else {
                    // use foo::bar
                    path.split("::")
                        .last()
                        .map(|s| s.to_string())
                        .into_iter()
                        .collect()
                };

                graph.imports.push(ImportEdge {
                    from_file: file_path.to_string(),
                    to_module: module,
                    imported_names: names,
                    line: node.start_position().row + 1,
                    resolved_target: None,
                    resolution: ResolutionKind::Unresolved,
                });
            }
            // `mod foo;` declarations — a module-tree edge the resolver binds to foo.rs/foo/mod.rs.
            // (An inline `mod foo { ... }` has a body and is NOT a file edge — skip it.)
            "mod_item" => {
                let has_body = node.child_by_field_name("body").is_some();
                if !has_body {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let name = name_node.utf8_text(bytes).unwrap_or("").to_string();
                        if !name.is_empty() {
                            graph.imports.push(ImportEdge {
                                from_file: file_path.to_string(),
                                to_module: format!("mod {}", name),
                                imported_names: vec![name],
                                line: node.start_position().row + 1,
                                resolved_target: None,
                                resolution: ResolutionKind::Unresolved,
                            });
                        }
                    }
                }
            }
            // impl blocks - extract methods
            "impl_item" => {
                if let Some(type_node) = node.child_by_field_name("type") {
                    let type_name = type_node.utf8_text(bytes).unwrap_or("").to_string();
                    // Find the corresponding class and add methods
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut body_cursor = body.walk();
                        for child in body.children(&mut body_cursor) {
                            if child.kind() == "function_item" {
                                if let Some(name_node) = child.child_by_field_name("name") {
                                    let method_name =
                                        name_node.utf8_text(bytes).unwrap_or("").to_string();
                                    // Add method to existing class if found
                                    if let Some(cls) = graph
                                        .classes
                                        .iter_mut()
                                        .find(|c| c.name == type_name && c.file_path == file_path)
                                    {
                                        cls.methods.push(method_name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_typescript() {
        let content = r#"
import { useState } from 'react';
import axios from 'axios';

export function fetchData(url: string): Promise<any> {
    return axios.get(url);
}

export class DataService {
    async getData() { return []; }
    processData(data: any[]) { return data; }
}

function helperFn() {}
"#;
        let mut graph = CodeGraph {
            files: vec![],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        parse_typescript(content, "src/data.ts", &mut graph, false);

        assert!(
            !graph.functions.is_empty(),
            "Should find fetchData function"
        );
        assert!(!graph.classes.is_empty(), "Should find DataService class");
        assert!(
            graph.imports.len() >= 2,
            "Should find react and axios imports"
        );
        assert!(
            graph.exports.len() >= 2,
            "Should find function and class exports"
        );
    }

    #[test]
    fn test_parse_python() {
        let content = r#"
from os import path
import json

def process_data(items):
    return [x for x in items]

class DataProcessor:
    def __init__(self):
        pass
    def run(self):
        pass

def _private_helper():
    pass
"#;
        let mut graph = CodeGraph {
            files: vec![],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        parse_python(content, "src/processor.py", &mut graph);

        assert!(
            graph.functions.len() >= 2,
            "Should find process_data and _private_helper"
        );
        assert!(!graph.classes.is_empty(), "Should find DataProcessor class");
        assert!(graph.imports.len() >= 2, "Should find os and json imports");

        // _private_helper should not be exported
        let private = graph.functions.iter().find(|f| f.name == "_private_helper");
        assert!(private.is_some());
        assert!(!private.unwrap().is_exported);
    }

    #[test]
    fn test_blast_radius_empty() {
        let graph = CodeGraph {
            files: vec![],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        let br = graph.blast_radius(&["src/foo.ts".to_string()]);
        assert_eq!(br.risk_level, RiskLevel::Low);
        assert_eq!(br.total_impact_count, 0);
    }

    #[test]
    fn test_format_for_prompt() {
        let graph = CodeGraph {
            files: vec![FileNode {
                path: "src/auth.ts".into(),
                language: "typescript".into(),
                line_count: 100,
            }],
            functions: vec![FunctionNode {
                name: "authenticate".into(),
                file_path: "src/auth.ts".into(),
                line_start: 10,
                line_end: 30,
                is_exported: true,
                is_async: true,
            }],
            classes: vec![],
            imports: vec![],
            exports: vec![ExportNode {
                name: "authenticate".into(),
                file_path: "src/auth.ts".into(),
                kind: "function".into(),
                line: 10,
            }],
            build_duration_ms: 5,
        };
        let prompt = graph.format_for_prompt("fix authenticate flow");
        assert!(prompt.contains("authenticate"));
        assert!(prompt.contains("Codebase Structure"));
    }

    #[test]
    fn test_parse_tsx() {
        let content = r#"
import React from 'react';

export function MyComponent({ name }: { name: string }) {
    return <div>{name}</div>;
}

export class App extends React.Component {
    render() { return <div />; }
}
"#;
        let mut graph = CodeGraph {
            files: vec![],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        parse_typescript(content, "src/App.tsx", &mut graph, true);

        assert!(
            !graph.functions.is_empty(),
            "Should find MyComponent function in TSX"
        );
        assert!(!graph.classes.is_empty(), "Should find App class in TSX");
        assert!(!graph.imports.is_empty(), "Should find React import in TSX");
    }

    #[test]
    fn test_blast_radius_with_imports() {
        // Resolved-edge blast radius: edges carry exact `resolved_target` file
        // paths (as the ImportResolver pass would populate), so lookups are exact —
        // no substring heuristic. routes.ts -> auth.ts (1-hop), app.ts -> routes.ts
        // (2-hop from auth.ts).
        let graph = CodeGraph {
            files: vec![
                FileNode {
                    path: "src/auth.ts".into(),
                    language: "typescript".into(),
                    line_count: 50,
                },
                FileNode {
                    path: "src/routes.ts".into(),
                    language: "typescript".into(),
                    line_count: 80,
                },
                FileNode {
                    path: "src/app.ts".into(),
                    language: "typescript".into(),
                    line_count: 120,
                },
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![
                ImportEdge {
                    from_file: "src/routes.ts".into(),
                    to_module: "./auth".into(),
                    imported_names: vec!["authenticate".into()],
                    line: 1,
                    resolved_target: Some("src/auth.ts".into()),
                    resolution: ResolutionKind::Relative,
                },
                ImportEdge {
                    from_file: "src/app.ts".into(),
                    to_module: "./routes".into(),
                    imported_names: vec!["router".into()],
                    line: 2,
                    resolved_target: Some("src/routes.ts".into()),
                    resolution: ResolutionKind::Relative,
                },
            ],
            exports: vec![ExportNode {
                name: "authenticate".into(),
                file_path: "src/auth.ts".into(),
                kind: "function".into(),
                line: 10,
            }],
            build_duration_ms: 0,
        };

        let br = graph.blast_radius(&["src/auth.ts".to_string()]);
        assert!(
            br.directly_affected.contains(&"src/routes.ts".to_string()),
            "routes.ts should be directly affected"
        );
        assert!(
            !br.affected_exports.is_empty(),
            "authenticate should be an affected export"
        );
        assert!(br.total_impact_count >= 1);
        // 2-hop: app.ts imports routes.ts which imports auth.ts
        assert!(
            br.transitively_affected.contains(&"src/app.ts".to_string()),
            "app.ts should be transitively affected"
        );
    }

    #[test]
    fn test_blast_radius_no_substring_false_positive() {
        // The OLD substring heuristic matched `to_module:"user"` against any file
        // path containing "user" (e.g. user_settings.ts). With resolved edges, an
        // import that resolves to a *different* file produces ZERO false positives.
        let graph = CodeGraph {
            files: vec![
                FileNode {
                    path: "src/user.ts".into(),
                    language: "typescript".into(),
                    line_count: 10,
                },
                FileNode {
                    path: "src/user_settings.ts".into(),
                    language: "typescript".into(),
                    line_count: 10,
                },
                FileNode {
                    path: "src/profile.ts".into(),
                    language: "typescript".into(),
                    line_count: 10,
                },
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![
                // profile.ts imports user_settings.ts (NOT user.ts).
                ImportEdge {
                    from_file: "src/profile.ts".into(),
                    to_module: "./user_settings".into(),
                    imported_names: vec!["settings".into()],
                    line: 1,
                    resolved_target: Some("src/user_settings.ts".into()),
                    resolution: ResolutionKind::Relative,
                },
            ],
            exports: vec![],
            build_duration_ms: 0,
        };

        // Changing user.ts must NOT mark profile.ts as affected (old heuristic would
        // have, because "user" is a substring of "user_settings").
        let br = graph.blast_radius(&["src/user.ts".to_string()]);
        assert!(
            br.directly_affected.is_empty(),
            "no file imports user.ts; expected zero directly-affected, got {:?}",
            br.directly_affected
        );
        assert_eq!(br.total_impact_count, 0);

        // Changing user_settings.ts DOES mark profile.ts as affected (exact edge).
        let br2 = graph.blast_radius(&["src/user_settings.ts".to_string()]);
        assert!(
            br2.directly_affected
                .contains(&"src/profile.ts".to_string()),
            "profile.ts imports user_settings.ts and should be directly affected"
        );
    }

    #[test]
    fn test_blast_radius_catches_alias_import() {
        // An `@/...` tsconfig alias import the OLD substring heuristic would have
        // missed (the raw specifier "@/auth" shares no substring with "src/auth.ts").
        // With the resolver having bound resolved_target, the edge is caught.
        let graph = CodeGraph {
            files: vec![
                FileNode {
                    path: "src/auth.ts".into(),
                    language: "typescript".into(),
                    line_count: 10,
                },
                FileNode {
                    path: "src/page.ts".into(),
                    language: "typescript".into(),
                    line_count: 10,
                },
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![ImportEdge {
                from_file: "src/page.ts".into(),
                to_module: "@/auth".into(),
                imported_names: vec!["login".into()],
                line: 1,
                resolved_target: Some("src/auth.ts".into()),
                resolution: ResolutionKind::TsconfigPath,
            }],
            exports: vec![],
            build_duration_ms: 0,
        };

        let br = graph.blast_radius(&["src/auth.ts".to_string()]);
        assert!(
            br.directly_affected.contains(&"src/page.ts".to_string()),
            "alias import @/auth -> src/auth.ts should be caught; got {:?}",
            br.directly_affected
        );
    }

    #[test]
    fn test_blast_radius_for_specification() {
        let graph = CodeGraph {
            files: vec![
                FileNode {
                    path: "src/auth.ts".into(),
                    language: "typescript".into(),
                    line_count: 50,
                },
                FileNode {
                    path: "src/routes.ts".into(),
                    language: "typescript".into(),
                    line_count: 80,
                },
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![ImportEdge {
                from_file: "src/routes.ts".into(),
                to_module: "./auth".into(),
                imported_names: vec!["authenticate".into()],
                line: 1,
                resolved_target: Some("src/auth.ts".into()),
                resolution: ResolutionKind::Relative,
            }],
            exports: vec![ExportNode {
                name: "authenticate".into(),
                file_path: "src/auth.ts".into(),
                kind: "function".into(),
                line: 10,
            }],
            build_duration_ms: 0,
        };

        // Description mentioning "auth" should match "auth.ts"
        let result = graph.format_blast_radius_for_specification("fix the auth module login flow");
        assert!(
            result.is_some(),
            "Should produce blast radius context for auth"
        );
        let text = result.unwrap();
        assert!(
            text.contains("Blast Radius"),
            "Should contain blast radius header"
        );
        assert!(text.contains("regression"), "Should mention regression");
    }

    #[test]
    fn test_rust_impl_methods() {
        let content = r#"
pub struct MyService {
    name: String,
}

impl MyService {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn get_name(&self) -> &str {
        &self.name
    }
}
"#;
        let mut graph = CodeGraph {
            files: vec![],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        parse_rust(content, "src/service.rs", &mut graph);

        assert!(!graph.classes.is_empty(), "Should find MyService struct");
        let cls = graph.classes.iter().find(|c| c.name == "MyService");
        assert!(cls.is_some());
        let methods = &cls.unwrap().methods;
        assert!(
            methods.contains(&"new".to_string()),
            "Should find new method"
        );
        assert!(
            methods.contains(&"get_name".to_string()),
            "Should find get_name method"
        );
    }

    // ---- resolver integration through CodeGraph::build -------------------

    #[test]
    fn test_build_resolves_imports_end_to_end() {
        let tmp = std::env::temp_dir().join(format!(
            "twinast-build-{}-{}",
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
            "import { authenticate } from './auth';\nimport axios from 'axios';\nexport const router = 1;\n",
        )
        .unwrap();

        let graph = CodeGraph::build(&tmp);

        let rel = graph
            .imports
            .iter()
            .find(|i| i.to_module == "./auth")
            .expect("relative import present");
        assert_eq!(rel.resolved_target.as_deref(), Some("src/auth.ts"));
        assert_eq!(rel.resolution, ResolutionKind::Relative);

        let ext = graph
            .imports
            .iter()
            .find(|i| i.to_module == "axios")
            .expect("external import present");
        assert_eq!(ext.resolved_target, None);
        assert_eq!(ext.resolution, ResolutionKind::External);

        // Blast radius via resolved edges: changing auth.ts affects routes.ts.
        let br = graph.blast_radius(&["src/auth.ts".to_string()]);
        assert!(br.directly_affected.contains(&"src/routes.ts".to_string()));

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(crate::paths::get_code_graph_path(&repo_hash(&tmp)));
    }

    #[test]
    fn test_build_incremental_reparses_only_changed_and_reproduces_full() {
        let tmp = std::env::temp_dir().join(format!(
            "twinast-incr-{}-{}",
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

        // First build: full — parses all 3 files.
        let (_g1, parsed1) = CodeGraph::build_incremental(&tmp);
        assert_eq!(parsed1, 3, "first incremental build parses all files");

        // No-op rebuild: nothing changed -> 0 parsed.
        let (_g_noop, parsed_noop) = CodeGraph::build_incremental(&tmp);
        assert_eq!(parsed_noop, 0, "unchanged rebuild re-parses nothing");

        // Edit ONE file (change its content so the hash differs).
        std::fs::write(
            tmp.join("src/auth.ts"),
            "export function authenticate() { return false; }\nexport function logout() {}\n",
        )
        .unwrap();

        let (g_incr, parsed2) = CodeGraph::build_incremental(&tmp);
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
        // The new logout export from the edited file must be present incrementally.
        assert!(
            g_incr.exports.iter().any(|e| e.name == "logout"),
            "edited file's new export reflected in incremental graph"
        );
        // Resolved edges reproduced.
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
}
