//! Session Recap API — semantic analysis of recent development activity.
//!
//! Analyzes git diffs across repos in qontinui-root to produce a structured
//! recap: files changed, types defined, endpoints added, database changes,
//! cross-language dependency edges, and an optional AI narrative.

use axum::{extract::State, http::StatusCode, response::Json, routing::post, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/session-recap/analyze", post(analyze_handler))
}

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Debug, Deserialize)]
struct SessionRecapRequest {
    /// How far back to look. Accepts: "3 hours", "1 day", "since last commit", or a commit ref like "HEAD~10".
    /// Defaults to "3 hours".
    lookback: Option<String>,
    /// Specific repo directory names to analyze (e.g. ["qontinui-runner", "ui-bridge"]).
    /// Defaults to all git repos under qontinui-root.
    repos: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Clone)]
struct SessionRecap {
    timespan: TimeSpan,
    /// `true` when the aggregate git budget actually COST the scan something,
    /// so the recap is PARTIAL. Published rather than swallowed: an empty
    /// recap and a truncated one look identical otherwise, and "nothing
    /// changed" is the wrong conclusion to hand a caller.
    ///
    /// Exactly `!repos_skipped.is_empty()`. Crossing the deadline on the way
    /// out of a complete scan is NOT exhaustion — nothing was lost — and used
    /// to be reported as such.
    git_budget_exhausted: bool,
    /// Repos whose scan was skipped **or cut short**, each carrying WHICH of
    /// the two it was — see [`RepoScanGapState`].
    ///
    /// It used to be a flat `Vec<String>` holding both, which conflated two
    /// materially different outcomes: a repo nothing was learned about, and a
    /// repo whose partial findings are already in `repos_touched` and in
    /// `summary`. The second appears in BOTH lists, so a reader of the flat
    /// list could not tell whether a name meant "absent from the recap" or
    /// "present in the recap but incomplete" — and `summary.total_repos`
    /// counted it as a fully scanned repo either way.
    ///
    /// Empty on a complete scan, and empty is the ONLY reading of "complete".
    repos_skipped: Vec<RepoScanGap>,
    repos_touched: Vec<RepoSummary>,
    files_created: Vec<FileChange>,
    files_modified: Vec<FileChange>,
    files_deleted: Vec<FileChange>,
    types_defined: Vec<TypeDefinition>,
    endpoints_added: Vec<EndpointInfo>,
    database_changes: Vec<DbChange>,
    ui_components: Vec<ComponentInfo>,
    dependency_graph: Vec<DependencyEdge>,
    summary: RecapSummary,
}

/// Which kind of hole the git budget left in one repo's scan.
///
/// The distinction is the difference between "this repo is missing from the
/// recap" and "this repo is IN the recap and what it says is incomplete" —
/// two different follow-up actions, and only one of them contaminates the
/// numbers the caller is reading.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum RepoScanGapState {
    /// The aggregate budget was already spent when this repo came up, so NO
    /// git child ran for it at all. It contributes nothing anywhere: no
    /// `repos_touched` entry, no files, no types. Its absence from the recap
    /// is a budget artefact, not a statement that it was untouched.
    NotStarted,
    /// The scan STARTED and at least one of its git children was then refused
    /// for want of budget, so this repo contributes a PARTIAL summary —
    /// typically the `--numstat` landed and `get_diff_content` did not,
    /// leaving `types_defined` / `endpoints_added` / `database_changes` /
    /// `ui_components` empty for a reason that is not "there were none".
    ///
    /// A repo in this state appears in `repos_touched` **as well as** here.
    CutShort,
}

impl RepoScanGapState {
    /// One operator-facing sentence, so a consumer never has to re-derive the
    /// meaning of the token.
    fn detail(self) -> &'static str {
        match self {
            RepoScanGapState::NotStarted => {
                "Not scanned at all — the aggregate git budget was already spent when this \
                 repo came up. Its absence from this recap says nothing about whether it changed."
            }
            RepoScanGapState::CutShort => {
                "Scanned only in part — a git child was refused for want of budget partway \
                 through. What this repo contributes here is incomplete, and empty type / \
                 endpoint / table lists for it do not mean there were none."
            }
        }
    }
}

/// One repo the git budget cost the scan something on.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RepoScanGap {
    repo: String,
    state: RepoScanGapState,
    detail: &'static str,
}

impl RepoScanGap {
    fn new(repo: String, state: RepoScanGapState) -> Self {
        Self {
            repo,
            state,
            detail: state.detail(),
        }
    }
}

/// The partiality triple a scan's gap list implies:
/// `(scan_complete, repos_not_started, repos_cut_short)`.
///
/// Pure, and the ONLY place the three are derived, so
/// [`RecapSummary::scan_complete`] can never disagree with
/// [`SessionRecap::git_budget_exhausted`] or with the list itself.
fn partiality(gaps: &[RepoScanGap]) -> (bool, u32, u32) {
    let not_started = gaps
        .iter()
        .filter(|g| g.state == RepoScanGapState::NotStarted)
        .count() as u32;
    let cut_short = gaps
        .iter()
        .filter(|g| g.state == RepoScanGapState::CutShort)
        .count() as u32;
    (gaps.is_empty(), not_started, cut_short)
}

#[derive(Debug, Serialize, Clone)]
struct TimeSpan {
    start: String,
    end: String,
    lookback_spec: String,
}

#[derive(Debug, Serialize, Clone)]
struct RepoSummary {
    name: String,
    files_changed: u32,
    lines_added: u32,
    lines_removed: u32,
}

#[derive(Debug, Serialize, Clone)]
struct FileChange {
    path: String,
    repo: String,
    language: String,
    change_type: String,
    lines_added: u32,
    lines_removed: u32,
    category: String,
}

#[derive(Debug, Serialize, Clone)]
struct TypeDefinition {
    name: String,
    kind: String,
    file: String,
    repo: String,
    language: String,
}

#[derive(Debug, Serialize, Clone)]
struct EndpointInfo {
    path: String,
    method: String,
    file: String,
    repo: String,
}

#[derive(Debug, Serialize, Clone)]
struct DbChange {
    table_name: String,
    change_type: String,
    file: String,
    repo: String,
}

#[derive(Debug, Serialize, Clone)]
struct ComponentInfo {
    name: String,
    file: String,
    repo: String,
    component_type: String,
}

#[derive(Debug, Serialize, Clone)]
struct DependencyEdge {
    from_file: String,
    to_file: String,
    relationship: String,
    cross_language: bool,
}

#[derive(Debug, Serialize, Clone)]
struct RecapSummary {
    total_files: u32,
    /// Repos that contributed at least one file change. **Not** "repos
    /// scanned": a repo counted here may still have been CUT SHORT, in which
    /// case its numbers are partial — see [`Self::repos_cut_short`].
    total_repos: u32,
    total_lines_added: u32,
    total_lines_removed: u32,
    new_types: u32,
    new_endpoints: u32,
    new_tables: u32,
    new_components: u32,
    categories: HashMap<String, u32>,

    // --- partiality, INSIDE the summary ------------------------------------
    //
    // These live here rather than only beside it because the summary is the
    // part that gets DETACHED and persisted: the recap page's "Save to
    // Memory" writes `JSON.stringify(recap.summary)` into the observations
    // store. Without them, a truncated scan is filed as a complete one, and
    // every later reader of that observation sees "3 repos, 0 new endpoints"
    // with nothing to say the endpoints were never looked for.
    /// `false` ⇒ this recap is PARTIAL. Exactly `repos_skipped.is_empty()`
    /// negated, i.e. the same statement as `git_budget_exhausted`, carried
    /// where it survives serialization of the summary alone.
    scan_complete: bool,
    /// Repos no git child ran for at all (`RepoScanGapState::NotStarted`).
    /// They are missing from every list in this recap.
    repos_not_started: u32,
    /// Repos whose scan began and was cut short
    /// (`RepoScanGapState::CutShort`). They ARE counted in
    /// [`Self::total_repos`], and their contribution is incomplete.
    repos_cut_short: u32,
}

// ============================================================================
// POST /session-recap/analyze
// ============================================================================

async fn analyze_handler(
    State(_state): State<Arc<ApiState>>,
    Json(input): Json<SessionRecapRequest>,
) -> Result<Json<ApiResponse<SessionRecap>>, (StatusCode, Json<ApiResponse<()>>)> {
    let lookback = input.lookback.unwrap_or_else(|| "3 hours".to_string());

    // Discover qontinui-root (parent of src-tauri)
    let root = find_qontinui_root().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Cannot find qontinui-root: {}", e))),
        )
    })?;

    // Discover repos
    let repos = discover_repos(&root, input.repos.as_deref()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Repo discovery failed: {}", e))),
        )
    })?;

    // Every git child below runs on the BLOCKING POOL, under ONE aggregate
    // budget. Before this the whole scan ran inline on the async handler, i.e.
    // on a reactor worker: ~5-7 bounded-but-30s git children per repo, over
    // every repo, TWICE (the two loops each resolved the lookback ref and
    // re-shelled out). Fourteen repos behind a wedged git was ~49 minutes of
    // one worker, and the route's own contract is that any client may poll it —
    // a handful of concurrent polls exhausted the workers and stalled every
    // `:9876` route INCLUDING `/health`, which is what the wedge watchdog
    // reads. Blinding the diagnostic subsystem is the worst possible failure
    // mode for a diagnostic endpoint.
    let scan = spawn_blocking_tracked({
        let lookback = lookback.clone();
        move || scan_repos(&repos, &lookback)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Recap scan task failed: {}", e))),
        )
    })?;

    let RepoScan {
        all_files,
        repo_summaries,
        types_defined,
        endpoints_added,
        database_changes,
        ui_components,
        dependency_graph,
        repos_skipped,
        git_budget_exhausted,
    } = scan;

    // Separate by change type
    let files_created: Vec<FileChange> = all_files
        .iter()
        .filter(|f| f.change_type == "created")
        .cloned()
        .collect();
    let files_modified: Vec<FileChange> = all_files
        .iter()
        .filter(|f| f.change_type == "modified")
        .cloned()
        .collect();
    let files_deleted: Vec<FileChange> = all_files
        .iter()
        .filter(|f| f.change_type == "deleted")
        .cloned()
        .collect();

    // Category breakdown
    let mut categories: HashMap<String, u32> = HashMap::new();
    for f in &all_files {
        *categories.entry(f.category.clone()).or_insert(0) += 1;
    }

    let (scan_complete, repos_not_started, repos_cut_short) = partiality(&repos_skipped);

    let summary = RecapSummary {
        total_files: all_files.len() as u32,
        total_repos: repo_summaries.len() as u32,
        total_lines_added: repo_summaries.iter().map(|r| r.lines_added).sum(),
        total_lines_removed: repo_summaries.iter().map(|r| r.lines_removed).sum(),
        new_types: types_defined.len() as u32,
        new_endpoints: endpoints_added.len() as u32,
        new_tables: database_changes.len() as u32,
        new_components: ui_components.len() as u32,
        categories,
        // ONE statement, three fields: `git_budget_exhausted` is
        // `!repos_skipped.is_empty()`, so `scan_complete` is its negation and
        // the two counts partition the list.
        scan_complete,
        repos_not_started,
        repos_cut_short,
    };

    let now = chrono::Utc::now();
    let recap = SessionRecap {
        timespan: TimeSpan {
            start: compute_start_time(&lookback, now).to_rfc3339(),
            end: now.to_rfc3339(),
            lookback_spec: lookback,
        },
        git_budget_exhausted,
        repos_skipped,
        repos_touched: repo_summaries,
        files_created,
        files_modified,
        files_deleted,
        types_defined,
        endpoints_added,
        database_changes,
        ui_components,
        dependency_graph,
        summary,
    };

    Ok(Json(ApiResponse::success(recap)))
}

// ============================================================================
// Helpers
// ============================================================================

fn compute_start_time(
    lookback: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    let parts: Vec<&str> = lookback.split_whitespace().collect();
    if parts.len() == 2 {
        if let Ok(amount) = parts[0].parse::<i64>() {
            let unit = parts[1].trim_end_matches('s');
            let duration = match unit {
                "hour" => chrono::Duration::hours(amount),
                "day" => chrono::Duration::days(amount),
                "minute" => chrono::Duration::minutes(amount),
                _ => chrono::Duration::hours(3),
            };
            return now - duration;
        }
    }
    // Fallback: 3 hours ago
    now - chrono::Duration::hours(3)
}

/// The workspace root this recap reads repos from.
///
/// **A sixth answer to "where is the workspace root", found at Phase 2 and not
/// in the plan's inventory** — the plan's audit grepped `D:/qontinui-root`, and
/// this copy spelled the literal with backslashes (`D:\qontinui-root`), so no
/// forward-slash search could ever have matched it. It was the worst of the six
/// on three counts, each of which the shared resolver already answers:
///
/// - its ancestor-walk predicate was `<dir>/qontinui-runner` **and**
///   `<dir>/ui-bridge` both being directories — no `.git` check at all, so any
///   directory happening to hold two same-named children anchored it, while a
///   perfectly good workspace without a `ui-bridge` checkout did not;
/// - it fell back to the **inherited cwd**, which `env_agent/collectors.rs`
///   documents as deliberately-never-correct: it makes resolution a function of
///   how the process was launched;
/// - the walk was capped at 10 ancestors for no stated reason.
///
/// Fails closed, because the recap's error is surfaced straight to the caller as
/// a 500 and a fabricated root would silently produce an empty recap that reads
/// like "nothing changed".
fn find_qontinui_root() -> Result<PathBuf, String> {
    crate::workspace_paths::require_workspace_root()
}

fn discover_repos(
    root: &Path,
    filter: Option<&[String]>,
) -> Result<Vec<(String, PathBuf)>, String> {
    let mut repos = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| e.to_string())?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        // Skip non-repo directories
        if name.starts_with('.') || name == "node_modules" || name == "docs" {
            continue;
        }

        // If filter specified, only include matching repos
        if let Some(allowed) = filter {
            if !allowed.iter().any(|a| a == &name) {
                continue;
            }
        }

        // Must have a .git directory
        if path.join(".git").exists() {
            repos.push((name, path));
        }
    }

    Ok(repos)
}

/// Resolves a lookback spec into a concrete git commit ref for a given repo.
/// For time-based lookbacks ("3 hours"), finds the oldest commit in that range.
/// For ref-based ("HEAD~10"), returns it directly.
fn resolve_lookback_ref(repo_path: &Path, lookback: &str, budget: &RecapBudget) -> String {
    match lookback {
        s if s.starts_with("HEAD~") || s.starts_with("HEAD^") => s.to_string(),
        "since last commit" => "HEAD".to_string(),
        s => {
            // Parse "N hours" / "N day(s)" into --since arg
            let parts: Vec<&str> = s.split_whitespace().collect();
            if parts.len() == 2 {
                let amount = parts[0];
                let unit = parts[1].trim_end_matches('s');
                let since = format!("{} {} ago", amount, unit);
                // Find the oldest commit in this time range
                let result = run_git(
                    repo_path,
                    &[
                        "log",
                        "--reverse",
                        "--format=%H",
                        &format!("--since={}", since),
                        "-1",
                    ],
                    budget,
                );
                match result {
                    Ok(hash) if !hash.trim().is_empty() => {
                        // Use <commit>^ to include that commit's changes
                        format!("{}^", hash.trim())
                    }
                    _ => {
                        // No commits in range — fall back to working tree diff
                        "HEAD".to_string()
                    }
                }
            } else {
                // Treat as a direct ref
                s.to_string()
            }
        }
    }
}

fn collect_file_changes(
    repo_name: &str,
    repo_path: &Path,
    diff_ref: &str,
    budget: &RecapBudget,
) -> Vec<FileChange> {
    let mut changes = Vec::new();

    // Use git diff --numstat <ref> to get per-file stats
    let output = run_git(repo_path, &["diff", "--numstat", diff_ref], budget);
    let output = match output {
        Ok(o) => o,
        Err(_) => return changes,
    };

    // Get lists of added/deleted files for accurate change_type
    let new_files = get_files_by_filter(repo_path, diff_ref, "A", budget);
    let deleted_files = get_files_by_filter(repo_path, diff_ref, "D", budget);

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // numstat format: <added>\t<removed>\t<file>
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let added: u32 = parts[0].parse().unwrap_or(0);
        let removed: u32 = parts[1].parse().unwrap_or(0);
        let file_path = parts[2].to_string();

        // Skip binary files (shown as -)
        if parts[0] == "-" {
            continue;
        }

        let change_type = if new_files.contains(&file_path) {
            "created"
        } else if deleted_files.contains(&file_path) {
            "deleted"
        } else {
            "modified"
        };

        changes.push(FileChange {
            path: file_path.clone(),
            repo: repo_name.to_string(),
            language: detect_language(&file_path),
            change_type: change_type.to_string(),
            lines_added: added,
            lines_removed: removed,
            category: categorize_file(&file_path),
        });
    }

    changes
}

fn get_files_by_filter(
    repo_path: &Path,
    diff_ref: &str,
    filter: &str,
    budget: &RecapBudget,
) -> Vec<String> {
    let filter_arg = format!("--diff-filter={}", filter);
    run_git(
        repo_path,
        &["diff", &filter_arg, "--name-only", diff_ref],
        budget,
    )
    .unwrap_or_default()
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty())
    .collect()
}

fn get_diff_content(repo_path: &Path, diff_ref: &str, budget: &RecapBudget) -> String {
    run_git(repo_path, &["diff", "-U0", diff_ref], budget).unwrap_or_default()
}

fn detect_language(path: &str) -> String {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "sql" => "sql",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "css" | "scss" => "css",
        "html" => "html",
        "md" => "markdown",
        _ => "other",
    }
    .to_string()
}

fn categorize_file(path: &str) -> String {
    let p = path.to_lowercase();
    if p.contains("migration") || p.ends_with(".sql") || p.contains("schema") {
        "database"
    } else if p.contains("test") || p.contains("spec.") || p.ends_with("_test.rs") {
        "test"
    } else if p.contains("src-tauri") || p.ends_with(".rs") {
        "backend"
    } else if p.ends_with(".tsx") || p.ends_with(".jsx") || p.contains("components/") {
        "frontend"
    } else if p.ends_with(".ts") || p.ends_with(".js") {
        if p.contains("lib/") || p.contains("hooks/") {
            "frontend"
        } else {
            "frontend"
        }
    } else if p.ends_with(".py") {
        "python"
    } else if p.contains("spec.uibridge") || p.contains("architecture.uibridge") {
        "spec"
    } else if p.ends_with(".toml")
        || p.ends_with(".json")
        || p.ends_with(".yaml")
        || p.ends_with(".yml")
    {
        "config"
    } else {
        "other"
    }
    .to_string()
}

fn extract_types(diff_content: &str, repo_name: &str) -> Vec<TypeDefinition> {
    let mut types = Vec::new();
    let mut current_file = String::new();

    for line in diff_content.lines() {
        if line.starts_with("+++ b/") {
            current_file = line[6..].to_string();
            continue;
        }

        if !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }

        let content = &line[1..];
        let lang = detect_language(&current_file);

        // Rust structs/enums/traits
        if lang == "rust" {
            if let Some(name) = extract_after_keyword(content, "struct ") {
                types.push(TypeDefinition {
                    name,
                    kind: "struct".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                    language: lang.clone(),
                });
            }
            if let Some(name) = extract_after_keyword(content, "enum ") {
                types.push(TypeDefinition {
                    name,
                    kind: "enum".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                    language: lang.clone(),
                });
            }
            if let Some(name) = extract_after_keyword(content, "trait ") {
                types.push(TypeDefinition {
                    name,
                    kind: "trait".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                    language: lang.clone(),
                });
            }
        }

        // TypeScript interfaces/types
        if lang == "typescript" || lang == "javascript" {
            if let Some(name) = extract_after_keyword(content, "interface ") {
                types.push(TypeDefinition {
                    name,
                    kind: "interface".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                    language: lang.clone(),
                });
            }
            if let Some(name) = extract_after_keyword(content, "type ") {
                // Filter out "type: " (property assignments)
                if !name.contains(':') {
                    types.push(TypeDefinition {
                        name,
                        kind: "type".into(),
                        file: current_file.clone(),
                        repo: repo_name.into(),
                        language: lang.clone(),
                    });
                }
            }
        }

        // SQL tables
        if lang == "sql" {
            let upper = content.to_uppercase();
            if upper.contains("CREATE TABLE") {
                if let Some(name) = extract_table_name(content) {
                    types.push(TypeDefinition {
                        name,
                        kind: "table".into(),
                        file: current_file.clone(),
                        repo: repo_name.into(),
                        language: lang.clone(),
                    });
                }
            }
        }
    }

    types
}

fn extract_endpoints(diff_content: &str, repo_name: &str) -> Vec<EndpointInfo> {
    let mut endpoints = Vec::new();
    let mut current_file = String::new();

    for line in diff_content.lines() {
        if line.starts_with("+++ b/") {
            current_file = line[6..].to_string();
            continue;
        }

        if !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }

        let content = &line[1..];

        // Rust axum routes: .route("/path", get/post/put/delete(handler))
        if content.contains(".route(\"") {
            if let Some(ep) = parse_axum_route(content, &current_file, repo_name) {
                endpoints.push(ep);
            }
        }

        // Tauri commands: #[tauri::command]
        if content.contains("#[tauri::command]") || content.contains("tauri::command") {
            // The function name is usually on the next line, but we capture what we can
            endpoints.push(EndpointInfo {
                path: "[tauri command]".into(),
                method: "IPC".into(),
                file: current_file.clone(),
                repo: repo_name.into(),
            });
        }
    }

    endpoints
}

fn extract_db_changes(diff_content: &str, repo_name: &str) -> Vec<DbChange> {
    let mut changes = Vec::new();
    let mut current_file = String::new();

    for line in diff_content.lines() {
        if line.starts_with("+++ b/") {
            current_file = line[6..].to_string();
            continue;
        }

        if !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }

        let upper = line[1..].to_uppercase();

        if upper.contains("CREATE TABLE") {
            if let Some(name) = extract_table_name(&line[1..]) {
                changes.push(DbChange {
                    table_name: name,
                    change_type: "created".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                });
            }
        } else if upper.contains("ALTER TABLE") {
            if let Some(name) = extract_table_name_after(&line[1..], "ALTER TABLE") {
                changes.push(DbChange {
                    table_name: name,
                    change_type: "altered".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                });
            }
        } else if upper.contains("DROP TABLE") {
            if let Some(name) = extract_table_name_after(&line[1..], "DROP TABLE") {
                changes.push(DbChange {
                    table_name: name,
                    change_type: "dropped".into(),
                    file: current_file.clone(),
                    repo: repo_name.into(),
                });
            }
        } else if upper.contains("CREATE INDEX") {
            changes.push(DbChange {
                table_name: "[index]".into(),
                change_type: "index_created".into(),
                file: current_file.clone(),
                repo: repo_name.into(),
            });
        }
    }

    changes
}

fn extract_components(diff_content: &str, repo_name: &str) -> Vec<ComponentInfo> {
    let mut components = Vec::new();
    let mut current_file = String::new();

    for line in diff_content.lines() {
        if line.starts_with("+++ b/") {
            current_file = line[6..].to_string();
            continue;
        }

        if !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }

        let content = &line[1..];

        // React components: export function ComponentName or export const ComponentName
        if (content.contains("export function ") || content.contains("export const "))
            && (current_file.ends_with(".tsx") || current_file.ends_with(".jsx"))
        {
            let name = if let Some(n) = extract_after_keyword(content, "export function ") {
                Some(n)
            } else {
                extract_after_keyword(content, "export const ")
            };

            if let Some(name) = name {
                // Must start with uppercase (React component convention)
                if name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    components.push(ComponentInfo {
                        name,
                        file: current_file.clone(),
                        repo: repo_name.into(),
                        component_type: "react".into(),
                    });
                }
            }
        }
    }

    components
}

fn build_dependency_graph(
    files: &[FileChange],
    repos: &[(String, PathBuf)],
) -> Vec<DependencyEdge> {
    let mut edges = Vec::new();
    let file_set: HashMap<String, &FileChange> = files
        .iter()
        .map(|f| (format!("{}/{}", f.repo, f.path), f))
        .collect();

    for (repo_name, repo_path) in repos {
        for file in files.iter().filter(|f| f.repo == *repo_name) {
            let full_path = repo_path.join(&file.path);
            let content = std::fs::read_to_string(&full_path).unwrap_or_default();

            // Look for imports that reference other changed files
            for import_path in extract_imports(&content, &file.language) {
                // Try to resolve the import to a changed file
                for (key, target) in &file_set {
                    if key.contains(&import_path) && key != &format!("{}/{}", file.repo, file.path)
                    {
                        let from_lang = &file.language;
                        let to_lang = &target.language;
                        let cross_lang = from_lang != to_lang;

                        let relationship = if cross_lang {
                            if from_lang == "typescript" && to_lang == "rust" {
                                "tauri_ipc"
                            } else if from_lang == "rust" && to_lang == "python" {
                                "python_bridge"
                            } else {
                                "cross_language"
                            }
                        } else {
                            "imports"
                        };

                        edges.push(DependencyEdge {
                            from_file: format!("{}/{}", file.repo, file.path),
                            to_file: key.clone(),
                            relationship: relationship.into(),
                            cross_language: cross_lang,
                        });
                    }
                }
            }
        }
    }

    // Dedup
    edges.sort_by(|a, b| (&a.from_file, &a.to_file).cmp(&(&b.from_file, &b.to_file)));
    edges.dedup_by(|a, b| a.from_file == b.from_file && a.to_file == b.to_file);

    edges
}

fn extract_imports(content: &str, language: &str) -> Vec<String> {
    let mut imports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        match language {
            "typescript" | "javascript" => {
                // import ... from "path" or require("path")
                if let Some(path) = extract_quoted_after(trimmed, "from ") {
                    imports.push(normalize_import_path(&path));
                }
                if let Some(path) = extract_quoted_after(trimmed, "require(") {
                    imports.push(normalize_import_path(&path));
                }
            }
            "rust" => {
                // use crate::module::...
                if trimmed.starts_with("use crate::") {
                    let module = trimmed
                        .trim_start_matches("use crate::")
                        .split("::")
                        .next()
                        .unwrap_or("");
                    if !module.is_empty() {
                        imports.push(module.to_string());
                    }
                }
                // mod module_name;
                if trimmed.starts_with("pub mod ") || trimmed.starts_with("mod ") {
                    let name = trimmed
                        .trim_start_matches("pub ")
                        .trim_start_matches("mod ")
                        .trim_end_matches(';')
                        .trim();
                    if !name.is_empty() {
                        imports.push(name.to_string());
                    }
                }
            }
            "python" => {
                if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                    let module = trimmed
                        .trim_start_matches("from ")
                        .trim_start_matches("import ")
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    if !module.is_empty() {
                        imports.push(module.replace('.', "/"));
                    }
                }
            }
            _ => {}
        }
    }

    imports
}

// ============================================================================
// String parsing helpers
// ============================================================================

fn extract_after_keyword(content: &str, keyword: &str) -> Option<String> {
    let trimmed = content.trim();
    if let Some(idx) = trimmed.find(keyword) {
        let rest = &trimmed[idx + keyword.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn extract_table_name(sql: &str) -> Option<String> {
    extract_table_name_after(sql, "CREATE TABLE")
        .or_else(|| extract_table_name_after(sql, "create table"))
}

fn extract_table_name_after(sql: &str, keyword: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let kw_upper = keyword.to_uppercase();
    if let Some(idx) = upper.find(&kw_upper) {
        let rest = &sql[idx + keyword.len()..];
        let rest = rest.trim();
        // Skip "IF NOT EXISTS" / "IF EXISTS"
        let rest_upper = rest.to_uppercase();
        let rest = if rest_upper.starts_with("IF NOT EXISTS") {
            rest["IF NOT EXISTS".len()..].trim()
        } else if rest_upper.starts_with("IF EXISTS") {
            rest["IF EXISTS".len()..].trim()
        } else {
            rest
        };
        let name: String = rest
            .chars()
            .skip_while(|c| !c.is_alphanumeric() && *c != '_')
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn parse_axum_route(content: &str, file: &str, repo: &str) -> Option<EndpointInfo> {
    // .route("/path", get(handler))
    let route_start = content.find(".route(\"")?;
    let rest = &content[route_start + 8..];
    let path_end = rest.find('"')?;
    let path = rest[..path_end].to_string();

    let after_path = &rest[path_end..];
    let method = if after_path.contains("get(") {
        "GET"
    } else if after_path.contains("post(") {
        "POST"
    } else if after_path.contains("put(") {
        "PUT"
    } else if after_path.contains("delete(") {
        "DELETE"
    } else if after_path.contains("patch(") {
        "PATCH"
    } else {
        "ANY"
    };

    Some(EndpointInfo {
        path,
        method: method.into(),
        file: file.into(),
        repo: repo.into(),
    })
}

fn extract_quoted_after(content: &str, keyword: &str) -> Option<String> {
    let idx = content.find(keyword)?;
    let rest = &content[idx + keyword.len()..];
    let quote_char = rest.chars().find(|c| *c == '"' || *c == '\'')?;
    let start = rest.find(quote_char)? + 1;
    let end = rest[start..].find(quote_char)?;
    Some(rest[start..start + end].to_string())
}

fn normalize_import_path(path: &str) -> String {
    path.trim_start_matches("@/")
        .trim_start_matches("./")
        .trim_start_matches("../")
        .replace('/', "::")
        .to_string()
}

/// Budget for ONE git child in the recap scan.
const RECAP_GIT_CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// Aggregate budget for the git portion of ONE recap request.
///
/// Bounding each child left the aggregate unbounded: the scan loops over every
/// discovered repo (14 on this fleet) issuing ~5 git calls each, so a wedged
/// git cost 14 x 5 x 30s ≈ 35 minutes for a single poll of a route "any client
/// can poll". This caps the whole request; repos not reached — and repos whose
/// scan was cut short part-way — are reported in `repos_skipped` rather than
/// silently contributing nothing.
const RECAP_GIT_TOTAL_BUDGET: Duration = Duration::from_secs(120);

/// The smallest slice worth spawning a git child for.
///
/// The old floor was "anything above zero", so a budget with 1ms left handed
/// that 1ms to a child that git could not possibly finish inside: the process
/// spawns, is killed before it has finished reading its config, and returns
/// nothing — a fork+exec+kill bought in exchange for an `Err` we could have
/// produced for free. Refusing below this floor yields the SAME `Err`, is
/// counted as a refusal exactly like any other, and costs nothing.
const RECAP_GIT_MIN_CHILD: Duration = Duration::from_millis(250);

/// Wall-clock budget shared by every git child of one recap request.
struct RecapBudget {
    deadline: Instant,
    /// How many git children the aggregate budget REFUSED (no usable slice
    /// left). This is the only honest way to tell "this repo's scan was cut
    /// short" from "this repo genuinely had no changes" — both produce an
    /// empty summary otherwise.
    ///
    /// `Cell` because the budget is shared by `&` through the whole scan,
    /// which runs on ONE blocking-pool thread (`scan_repos` owns it start to
    /// finish and never hands it across a thread boundary).
    refusals: std::cell::Cell<u32>,
    /// Test-only: force the aggregate to read as spent once this many children
    /// have actually been SERVED. A wall-clock budget cannot express "alive at
    /// the top of the repo, spent three children later" deterministically, and
    /// that is precisely the CUT SHORT case — the one the old `repos_skipped`
    /// missed. `u32::MAX` in every other test, and the field does not exist at
    /// all in a release build.
    #[cfg(test)]
    serve_limit: std::cell::Cell<u32>,
    #[cfg(test)]
    served: std::cell::Cell<u32>,
}

impl RecapBudget {
    fn new(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
            refusals: std::cell::Cell::new(0),
            #[cfg(test)]
            serve_limit: std::cell::Cell::new(u32::MAX),
            #[cfg(test)]
            served: std::cell::Cell::new(0),
        }
    }

    /// Pure PEEK at the wall clock: how long a child could run, or `None` when
    /// the aggregate has no slice left worth spawning into (see
    /// [`RECAP_GIT_MIN_CHILD`]). Has no side effects and never counts a serve.
    fn remaining(&self) -> Option<Duration> {
        let left = self.deadline.checked_duration_since(Instant::now())?;
        (left >= RECAP_GIT_MIN_CHILD).then(|| left.min(RECAP_GIT_CHILD_TIMEOUT))
    }

    /// How long the next child may run, or `None` when the aggregate is spent.
    /// Unlike [`Self::remaining`] this is a CLAIM, not a peek.
    fn next_child(&self) -> Option<Duration> {
        #[cfg(test)]
        if self.served.get() >= self.serve_limit.get() {
            return None;
        }
        let slice = self.remaining()?;
        #[cfg(test)]
        self.served.set(self.served.get() + 1);
        Some(slice)
    }

    /// Is the aggregate spent? Read at the TOP of each repo, which is what
    /// decides NOT-STARTED vs CUT-SHORT.
    ///
    /// The test-only `serve_limit` is honoured here as well as in
    /// [`Self::next_child`], so the seam cannot contradict itself: without
    /// this, a limit-driven test left `exhausted()` reading the (still alive)
    /// wall clock while every child was refused, so a repo nothing ran for was
    /// classified CUT-SHORT — the very conflation the two states exist to
    /// remove, reintroduced by the harness.
    fn exhausted(&self) -> bool {
        #[cfg(test)]
        if self.served.get() >= self.serve_limit.get() {
            return true;
        }
        self.remaining().is_none()
    }

    /// Record that a child was refused for want of budget. Called ONLY from
    /// [`run_git`] — never from [`Self::exhausted`], which is a peek and must
    /// not inflate the count.
    fn note_refusal(&self) {
        self.refusals.set(self.refusals.get().saturating_add(1));
    }

    fn refusals(&self) -> u32 {
        self.refusals.get()
    }
}

/// Everything one blocking scan pass produces.
struct RepoScan {
    all_files: Vec<FileChange>,
    repo_summaries: Vec<RepoSummary>,
    types_defined: Vec<TypeDefinition>,
    endpoints_added: Vec<EndpointInfo>,
    database_changes: Vec<DbChange>,
    ui_components: Vec<ComponentInfo>,
    dependency_graph: Vec<DependencyEdge>,
    /// Repos skipped outright or cut short mid-scan, each typed with which —
    /// see [`SessionRecap::repos_skipped`], which this is published as
    /// verbatim.
    repos_skipped: Vec<RepoScanGap>,
    /// `!repos_skipped.is_empty()` — see [`SessionRecap::git_budget_exhausted`].
    git_budget_exhausted: bool,
}

/// The whole git-shelling half of a recap, in ONE pass per repo.
///
/// Runs on the blocking pool (see the call site). Two things changed beyond
/// the move:
///
/// * **One pass, not two.** The handler used to loop over the repos twice and
///   call `resolve_lookback_ref` in BOTH, so every repo paid for the lookback
///   resolution — a `git log` — twice for identical output. The diff ref is
///   now resolved once per repo and reused.
/// * **An aggregate budget.** Once [`RECAP_GIT_TOTAL_BUDGET`] is spent the
///   remaining repos are recorded in `repos_skipped` instead of spawning more
///   children, so the cost of the unbounded repo list is bounded. A repo that
///   STARTED and was then cut short lands in the same list — it contributes a
///   partial summary, which is exactly the case the field exists to disclose.
fn scan_repos(repos: &[(String, PathBuf)], lookback: &str) -> RepoScan {
    scan_repos_with(repos, lookback, RecapBudget::new(RECAP_GIT_TOTAL_BUDGET))
}

/// [`scan_repos`] with the budget injected — the seam the regression tests use
/// to drive an already-spent budget and a mid-repo cut-off deterministically.
fn scan_repos_with(repos: &[(String, PathBuf)], lookback: &str, budget: RecapBudget) -> RepoScan {
    let mut all_files: Vec<FileChange> = Vec::new();
    let mut repo_summaries: Vec<RepoSummary> = Vec::new();
    let mut types_defined = Vec::new();
    let mut endpoints_added = Vec::new();
    let mut database_changes = Vec::new();
    let mut ui_components = Vec::new();
    let mut repos_skipped: Vec<RepoScanGap> = Vec::new();

    for (repo_name, repo_path) in repos {
        if budget.exhausted() {
            // NOT STARTED — no child was spawned for this repo at all.
            repos_skipped.push(RepoScanGap::new(
                repo_name.clone(),
                RepoScanGapState::NotStarted,
            ));
            continue;
        }

        // Watermark for the CUT SHORT case below. A repo whose scan begins and
        // then runs out of budget mid-way contributes a PARTIAL summary — the
        // `--numstat` landed, `get_diff_content` was refused, so its
        // `types_defined` / `endpoints_added` / `database_changes` /
        // `ui_components` are empty for a reason that has nothing to do with
        // the diff. Listing it is the difference between "this repo added no
        // types" and "we never looked".
        let refusals_before = budget.refusals();

        // Resolved ONCE per repo and reused by both halves below.
        let diff_ref = resolve_lookback_ref(repo_path, lookback, &budget);

        let changes = collect_file_changes(repo_name, repo_path, &diff_ref, &budget);
        if !changes.is_empty() {
            let lines_added: u32 = changes.iter().map(|f| f.lines_added).sum();
            let lines_removed: u32 = changes.iter().map(|f| f.lines_removed).sum();
            repo_summaries.push(RepoSummary {
                name: repo_name.clone(),
                files_changed: changes.len() as u32,
                lines_added,
                lines_removed,
            });
            all_files.extend(changes);
        }

        let diff_content = get_diff_content(repo_path, &diff_ref, &budget);
        types_defined.extend(extract_types(&diff_content, repo_name));
        endpoints_added.extend(extract_endpoints(&diff_content, repo_name));
        database_changes.extend(extract_db_changes(&diff_content, repo_name));
        ui_components.extend(extract_components(&diff_content, repo_name));

        // CUT SHORT: at least one of this repo's git children was refused for
        // want of budget, so what it contributed above is incomplete.
        if budget.refusals() > refusals_before {
            repos_skipped.push(RepoScanGap::new(
                repo_name.clone(),
                RepoScanGapState::CutShort,
            ));
        }
    }

    // Evaluated HERE — at the end of the GIT loop, and specifically BEFORE
    // `build_dependency_graph`, which shells out to nothing and can therefore
    // never exhaust a git budget.
    //
    // It is `!repos_skipped.is_empty()`, not `budget.exhausted()`. The two
    // differ exactly where it matters: a scan that reached every repo and
    // spawned every child, but happened to cross the 120s deadline on its way
    // out, leaves `exhausted() == true` while nothing was actually lost — and
    // the old code published `git_budget_exhausted: true` with
    // `repos_skipped: []` and WARNed "0 repo(s) skipped, the recap is
    // PARTIAL". Nothing was partial. Since every real loss now lands in
    // `repos_skipped` (not-started at the top of the loop, cut-short at the
    // bottom), that list IS the predicate.
    let git_budget_exhausted = !repos_skipped.is_empty();
    if git_budget_exhausted {
        tracing::warn!(
            "session_recap: the {}s aggregate git budget was exhausted — the recap is \
             PARTIAL: {} repo(s) skipped or cut short ({}), {} git child(ren) refused",
            RECAP_GIT_TOTAL_BUDGET.as_secs(),
            repos_skipped.len(),
            repos_skipped
                .iter()
                .map(|g| format!("{} ({:?})", g.repo, g.state))
                .collect::<Vec<_>>()
                .join(", "),
            budget.refusals()
        );
    }

    let dependency_graph = build_dependency_graph(&all_files, repos);

    RepoScan {
        all_files,
        repo_summaries,
        types_defined,
        endpoints_added,
        database_changes,
        ui_components,
        dependency_graph,
        repos_skipped,
        git_budget_exhausted,
    }
}

/// One bounded git child, charged against the request's aggregate budget.
///
/// Returns `Err` when the child failed, was killed at its budget, OR when the
/// aggregate budget has no slice left worth spawning into ([`RECAP_GIT_MIN_CHILD`]) —
/// in which case nothing is spawned at all and the refusal is COUNTED, so
/// [`scan_repos_with`] can name the repo it cut short. Every caller already
/// degrades on `Err`.
fn run_git(repo_path: &Path, args: &[&str], budget: &RecapBudget) -> Result<String, String> {
    let Some(child_budget) = budget.next_child() else {
        // Counted, so the caller can report WHICH repo was cut short rather
        // than silently contributing a partial summary.
        budget.note_refusal();
        return Err(format!(
            "git {} skipped: the request's aggregate git budget is spent",
            args.join(" ")
        ));
    };
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(args).current_dir(repo_path);
    let output = crate::process_helpers::output_with_timeout(cmd, child_budget)
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A budget with nothing left. `Duration::ZERO` puts the deadline at
    /// construction time, so it is spent before the first peek.
    fn spent() -> RecapBudget {
        RecapBudget::new(Duration::ZERO)
    }

    fn repo_list(names: &[&str]) -> Vec<(String, PathBuf)> {
        names
            .iter()
            .map(|n| (n.to_string(), PathBuf::from(".")))
            .collect()
    }

    // ── The 1ms child (2026-08-30 round-3 review) ──────────────────────────

    /// A slice below [`RECAP_GIT_MIN_CHILD`] buys nothing: the child is killed
    /// before git finishes reading its config, so a fork+exec+kill is spent to
    /// produce the same `Err` a refusal produces for free.
    #[test]
    fn a_sub_floor_slice_is_refused_rather_than_handed_to_a_doomed_child() {
        let nearly_spent = RecapBudget::new(RECAP_GIT_MIN_CHILD / 5);
        assert_eq!(
            nearly_spent.next_child(),
            None,
            "a ~50ms remainder must be refused, not handed out"
        );
        assert!(nearly_spent.exhausted());

        let alive = RecapBudget::new(Duration::from_secs(10));
        let slice = alive.next_child().expect("a 10s remainder is usable");
        assert!(slice >= RECAP_GIT_MIN_CHILD);
        assert!(
            slice <= RECAP_GIT_CHILD_TIMEOUT,
            "no child may outlive the per-child cap"
        );
    }

    /// `exhausted()` is a PEEK. It must not consume a serve, or the top-of-loop
    /// check would itself eat the budget it is testing.
    #[test]
    fn exhausted_is_a_peek_and_never_counts_a_serve_or_a_refusal() {
        let budget = RecapBudget::new(Duration::from_secs(10));
        for _ in 0..5 {
            assert!(!budget.exhausted());
        }
        assert_eq!(budget.refusals(), 0);
        assert!(budget.next_child().is_some(), "five peeks spent nothing");
    }

    /// The watermark the loop keys on: a SERVED child is not a refusal; a
    /// refused one moves the counter exactly once.
    #[test]
    fn only_a_refused_child_moves_the_refusal_watermark() {
        let alive = RecapBudget::new(Duration::from_secs(30));
        let _ = run_git(Path::new("."), &["--version"], &alive);
        assert_eq!(
            alive.refusals(),
            0,
            "a child that was SERVED is not a refusal, however it then fared"
        );

        let spent = spent();
        let err = run_git(Path::new("."), &["--version"], &spent)
            .expect_err("a spent budget spawns nothing");
        assert!(err.contains("aggregate git budget is spent"));
        assert_eq!(spent.refusals(), 1);
        run_git(Path::new("."), &["--version"], &spent).ok();
        assert_eq!(spent.refusals(), 2, "each refusal counts");
    }

    // ── `repos_skipped` and `git_budget_exhausted` ─────────────────────────

    /// `(name, state)` for each gap — what the flat `Vec<String>` could not say.
    fn gaps(scan: &RepoScan) -> Vec<(&str, RepoScanGapState)> {
        scan.repos_skipped
            .iter()
            .map(|g| (g.repo.as_str(), g.state))
            .collect()
    }

    /// A repo the budget never reached: no child spawned, and it is listed —
    /// as NOT-STARTED, which is the half of the old flat list that meant
    /// "absent from the recap entirely".
    #[test]
    fn a_repo_the_budget_never_reached_is_listed_as_skipped() {
        let scan = scan_repos_with(&repo_list(&["repo-a", "repo-b"]), "1 day", spent());
        assert_eq!(
            gaps(&scan),
            vec![
                ("repo-a", RepoScanGapState::NotStarted),
                ("repo-b", RepoScanGapState::NotStarted),
            ]
        );
        assert!(scan.git_budget_exhausted);
        assert!(scan.repo_summaries.is_empty());
    }

    /// **The miss.** A repo whose scan STARTS and is then cut short mid-way
    /// contributes a partial summary — the `--numstat` may have landed while
    /// `get_diff_content` was refused, leaving `types_defined` /
    /// `endpoints_added` empty for a reason that is not "there were none". The
    /// field's own doc says "skipped **or cut short**"; only the first half was
    /// ever recorded, because the push happened at the TOP of the loop.
    ///
    /// `serve_limit` expresses this deterministically: the budget is alive
    /// when the repo comes up (so the scan starts), and spent by the second
    /// child.
    #[test]
    fn a_repo_that_started_and_was_cut_short_mid_scan_is_listed_too() {
        let budget = RecapBudget::new(Duration::from_secs(30));
        budget.serve_limit.set(1); // one child served, every later one refused

        let scan = scan_repos_with(&repo_list(&["repo-cut-short"]), "1 day", budget);

        assert_eq!(
            gaps(&scan),
            vec![("repo-cut-short", RepoScanGapState::CutShort)],
            "the repo whose scan was actually cut short must be the one listed, \
             and it must be TYPED as cut-short rather than sharing one flat list \
             with the repos nothing ran for"
        );
        assert!(scan.git_budget_exhausted);
    }

    /// **The conflation.** The two states are not interchangeable, and the
    /// flat `Vec<String>` made them look it:
    ///
    /// * a NOT-STARTED repo is absent from `repos_touched` — its absence is a
    ///   budget artefact;
    /// * a CUT-SHORT repo is PRESENT in `repos_touched` and in the summary
    ///   totals, with partial numbers — so the same name in the old list meant
    ///   two opposite things, and a reader could not tell which.
    ///
    /// Driven through the real seam: `repo-cut-short` is scanned in `.`, a
    /// real git repo, so it produces a summary; `repo-never-reached` comes up
    /// after the budget is spent.
    #[test]
    fn a_cut_short_repo_is_typed_apart_from_one_that_never_started() {
        let budget = RecapBudget::new(Duration::from_secs(30));
        // Enough children for the first repo to produce a summary, then spent.
        budget.serve_limit.set(2);

        let scan = scan_repos_with(
            &repo_list(&["repo-cut-short", "repo-never-reached"]),
            "1 day",
            budget,
        );

        assert_eq!(
            gaps(&scan),
            vec![
                ("repo-cut-short", RepoScanGapState::CutShort),
                ("repo-never-reached", RepoScanGapState::NotStarted),
            ],
            "one list, two states, each named"
        );

        // And the states are distinguishable in the way that matters: the
        // cut-short repo also appears in `repos_touched`, the other cannot.
        assert!(
            !scan
                .repo_summaries
                .iter()
                .any(|r| r.name == "repo-never-reached"),
            "a repo no child ran for cannot contribute a summary"
        );

        // Every gap carries its own operator sentence — a consumer never has
        // to re-derive what the token means.
        assert!(scan.repos_skipped[0].detail.contains("only in part"));
        assert!(scan.repos_skipped[1].detail.contains("Not scanned at all"));
    }

    /// **The false positive.** A scan that reached every repo and spawned every
    /// child, but happened to cross the deadline on its way out, lost nothing.
    /// The old `git_budget_exhausted = budget.exhausted()` — evaluated AFTER
    /// the non-git `build_dependency_graph`, which cannot exhaust a git budget
    /// — reported `true` with `repos_skipped: []` and WARNed "0 repo(s)
    /// skipped, the recap is PARTIAL".
    #[test]
    fn a_complete_scan_that_merely_crossed_the_deadline_is_not_exhausted() {
        // Nothing to scan ⇒ nothing was skipped or cut short, yet the budget
        // is spent by the time the flag is computed — exactly the old
        // false-positive shape.
        let budget = spent();
        assert!(budget.exhausted(), "precondition: the wall clock IS spent");

        let scan = scan_repos_with(&[], "1 day", budget);

        assert!(
            scan.repos_skipped.is_empty(),
            "nothing was skipped or cut short"
        );
        assert!(
            !scan.git_budget_exhausted,
            "a spent clock with nothing lost is NOT a partial recap"
        );
    }

    /// The two fields are one statement, not two: `git_budget_exhausted` is
    /// exactly `!repos_skipped.is_empty()`, so a reader can never be told the
    /// recap is PARTIAL without being told which repos it is partial about.
    #[test]
    fn git_budget_exhausted_is_exactly_the_non_emptiness_of_repos_skipped() {
        for (repos, limit) in [
            (repo_list(&[]), u32::MAX),
            (repo_list(&["a"]), u32::MAX),
            (repo_list(&["a", "b"]), 1),
            (repo_list(&["a", "b"]), 0),
        ] {
            let budget = RecapBudget::new(Duration::from_secs(30));
            budget.serve_limit.set(limit);
            let scan = scan_repos_with(&repos, "1 day", budget);
            assert_eq!(
                scan.git_budget_exhausted,
                !scan.repos_skipped.is_empty(),
                "flag and list disagreed for {repos:?} @ limit {limit}"
            );
        }
    }

    /// **Partiality must survive the summary being detached.** The recap page
    /// persists `JSON.stringify(recap.summary)` alone into the observations
    /// store, so a truncated scan whose only partiality signal lives beside
    /// the summary is filed as a complete one — permanently, and read back
    /// later as "we looked and there was nothing".
    ///
    /// `partiality()` is what the handler folds into `RecapSummary`, so
    /// asserting it here asserts what gets written.
    #[test]
    fn the_summary_partiality_triple_partitions_the_gap_list() {
        // Complete scan: nothing lost.
        assert_eq!(partiality(&[]), (true, 0, 0));

        // One of each, driven through the real scan seam.
        let budget = RecapBudget::new(Duration::from_secs(30));
        budget.serve_limit.set(2);
        let scan = scan_repos_with(
            &repo_list(&["repo-cut-short", "repo-never-reached"]),
            "1 day",
            budget,
        );
        let (scan_complete, not_started, cut_short) = partiality(&scan.repos_skipped);
        assert!(
            !scan_complete,
            "a scan that lost two repos is not a complete one"
        );
        assert_eq!(not_started, 1);
        assert_eq!(cut_short, 1);
        assert_eq!(
            (not_started + cut_short) as usize,
            scan.repos_skipped.len(),
            "the two counts must partition the list — no row unaccounted for"
        );
        assert_eq!(
            scan_complete, !scan.git_budget_exhausted,
            "the summary's completeness and the recap's flag are ONE statement"
        );
    }
}
