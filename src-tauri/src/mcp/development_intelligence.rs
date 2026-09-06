//! Development Intelligence HTTP API handlers.
//!
//! Provides endpoints for coverage gap analysis, complexity scoring,
//! drift detection, and dead feature identification. All analysis works
//! offline by reading spec files, test files, component source, and git
//! history — no UI Bridge connection required.

use axum::{extract::State, http::StatusCode, response::Json, routing::post, Router};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::mcp::types::ApiState;
use crate::str_utils::truncate_str;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRequest {
    /// Registered app id whose specs/ root to scan
    /// (spec-multi-app Stream C). Required.
    pub app_id: String,
    pub project_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageGap {
    page: String,
    spec_assertion_count: usize,
    critical_assertion_count: usize,
    test_file_count: usize,
    test_assertion_count: usize,
    coverage_score: f64,
    gaps: Vec<GapDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GapDetail {
    spec_group_id: String,
    spec_group_name: String,
    category: String,
    assertion_count: usize,
    has_corresponding_test: bool,
    severity: String,
}

#[derive(Debug, Serialize)]
struct CoverageAnalysisResult {
    gaps: Vec<CoverageGap>,
    summary: CoverageSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageSummary {
    total_pages: usize,
    well_covered: usize,
    partially_covered: usize,
    uncovered: usize,
    average_coverage_score: f64,
}

#[derive(Debug, Serialize)]
struct ComplexityScore {
    page: String,
    route: String,
    scores: ComplexityScores,
    composite: u32,
    tier: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexityScores {
    element_count: usize,
    assertion_count: usize,
    interaction_count: usize,
    form_field_count: usize,
    api_call_count: usize,
    state_variable_count: usize,
    component_depth: usize,
}

#[derive(Debug, Serialize)]
struct ComplexityAnalysisResult {
    scores: Vec<ComplexityScore>,
    summary: ComplexitySummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexitySummary {
    total_pages: usize,
    simple: usize,
    moderate: usize,
    complex: usize,
    critical: usize,
    average_composite: f64,
    drift_alerts: Vec<DriftAlert>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftAlert {
    page: String,
    route: String,
    warning: String,
    velocity_per_week: f64,
    current_composite: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeatureHealth {
    page: String,
    route: String,
    component_path: String,
    /// `"active"` | `"stale"` | `"abandoned"` | `"spec-drift"` | `"unknown"`.
    ///
    /// `"unknown"` is NOT a classification of the feature — it means the git
    /// history probe did not answer, so no classification was possible. It
    /// exists because the alternative was publishing "abandoned" off a
    /// fabricated 1970 timestamp.
    status: String,
    last_code_change: String,
    last_spec_change: String,
    code_commit_count_30d: usize,
    /// Whether `code_commit_count_30d` was actually MEASURED. `false` means
    /// the probe degraded and the count is a placeholder zero, not an
    /// observation. Additive on the wire; older clients ignore it.
    code_commit_count_known: bool,
    spec_age: f64,
    code_age: f64,
    staleness: f64,
    signals: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FeatureHealthResult {
    features: Vec<FeatureHealth>,
    summary: FeatureHealthSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeatureHealthSummary {
    total: usize,
    active: usize,
    stale: usize,
    abandoned: usize,
    spec_drift: usize,
    /// Features whose git history could not be read at all. Counted
    /// separately so a degraded run is visible in the summary instead of
    /// inflating `abandoned`.
    unknown: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrendEntry {
    page_route: String,
    score: f64,
    analysis_type: String,
    created_at: String,
}

// ============================================================================
// Spec file parsing
// ============================================================================

#[derive(Debug, Deserialize)]
struct SpecFile {
    id: Option<String>,
    metadata: SpecMetadata,
    groups: Option<Vec<SpecGroup>>,
}

#[derive(Debug, Deserialize)]
struct SpecMetadata {
    component: Option<String>,
    #[serde(rename = "pageId")]
    page_id: Option<String>,
    #[serde(rename = "pageUrl")]
    page_url: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SpecGroup {
    id: Option<String>,
    name: Option<String>,
    category: Option<String>,
    assertions: Option<Vec<SpecAssertion>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SpecAssertion {
    id: Option<String>,
    category: Option<String>,
    severity: Option<String>,
    enabled: Option<bool>,
}

async fn load_specs(app_id: &str, _project_path: &Path) -> Vec<SpecFile> {
    let pg = crate::database::pg::PgDb::global();
    let specs_root = match crate::spec_api::storage::resolve_specs_root(&pg, app_id).await {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to resolve specs_root for app {}: {:?}", app_id, e);
            return vec![];
        }
    };
    let page_ids = match crate::spec_api::storage::list_pages(&specs_root, app_id) {
        Ok(ids) => ids,
        Err(e) => {
            warn!("Failed to list pages under {:?}: {}", specs_root, e);
            return vec![];
        }
    };

    let mut specs = vec![];
    for page_id in &page_ids {
        match crate::spec_api::storage::read_projection(&specs_root, app_id, page_id) {
            Ok(Some(value)) => match serde_json::from_value::<SpecFile>(value) {
                Ok(spec) => specs.push(spec),
                Err(e) => warn!("Failed to parse spec for page {}: {}", page_id, e),
            },
            Ok(None) => {}
            Err(e) => warn!("Failed to read projection for page {}: {}", page_id, e),
        }
    }

    info!(
        "Loaded {} spec files from {} pages under {:?}",
        specs.len(),
        page_ids.len(),
        specs_root,
    );
    specs
}

// ============================================================================
// Test file scanning
// ============================================================================

struct TestFileInfo {
    file_path: String,
    assertion_count: usize,
    referenced_components: Vec<String>,
    content: String,
}

fn scan_test_files(project_path: &Path) -> Vec<TestFileInfo> {
    let src_dir = project_path.join("src");
    if !src_dir.exists() {
        return vec![];
    }

    let mut test_files = vec![];
    scan_test_files_recursive(&src_dir, &mut test_files);
    info!("Found {} test files", test_files.len());
    test_files
}

fn scan_test_files_recursive(dir: &Path, results: &mut Vec<TestFileInfo>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_test_files_recursive(&path, results);
        } else if let Some(name) = path.file_name().and_then(|f| f.to_str()) {
            let is_test = name.ends_with(".test.ts")
                || name.ends_with(".test.tsx")
                || name.ends_with(".spec.ts")
                || name.ends_with(".spec.tsx");

            // Skip UI Bridge spec files
            if name.ends_with(".uibridge.json") {
                continue;
            }

            if is_test {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let assertion_count = count_test_assertions(&content);
                    let referenced = extract_component_refs(&content);
                    results.push(TestFileInfo {
                        file_path: path.to_string_lossy().into_owned(),
                        assertion_count,
                        referenced_components: referenced,
                        content,
                    });
                }
            }
        }
    }
}

fn count_test_assertions(content: &str) -> usize {
    let patterns = ["expect(", "assert(", "assert.", "it(", "test("];
    patterns.iter().map(|p| content.matches(p).count()).sum()
}

fn extract_component_refs(content: &str) -> Vec<String> {
    // Extract import references — look for component names in imports
    let mut refs = vec![];
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("import") {
            // Extract what's between { } or the default import
            if let Some(start) = trimmed.find('{') {
                if let Some(end) = trimmed.find('}') {
                    let imports = &trimmed[start + 1..end];
                    for name in imports.split(',') {
                        let name = name.trim().split(" as ").next().unwrap_or("").trim();
                        if !name.is_empty() {
                            refs.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    refs
}

fn extract_keywords(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 3)
        .map(|w| w.to_lowercase())
        .collect()
}

// ============================================================================
// Git history helpers
// ============================================================================

/// Budget for ONE `git log` child.
const GIT_HISTORY_CHILD_TIMEOUT: Duration = Duration::from_secs(20);

/// Aggregate budget for ALL git history probes in a single feature-health
/// request.
///
/// Bounding each child was not enough. `feature_health` loops over
/// `specs.iter()` — an unbounded collection — and issues two `git log` calls
/// per spec, so 50 specs against a wedged git was 50 x 2 x 20s ≈ **33
/// minutes** of one blocking-pool thread for one HTTP request, and the route
/// is pollable. This caps the git portion of the whole request; once it is
/// spent the remaining specs report their history as UNKNOWN instead of
/// spawning more children.
const GIT_HISTORY_TOTAL_BUDGET: Duration = Duration::from_secs(45);

/// Wall-clock budget shared by every git probe of one request.
///
/// Not a thread pool or a semaphore — just a deadline the probes consult, so
/// the aggregate cost of an unbounded loop is bounded by construction.
struct GitBudget {
    deadline: Instant,
}

impl GitBudget {
    fn new(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
        }
    }

    /// How long the next child may run: the smaller of the per-child budget
    /// and what is left of the aggregate. `None` once the aggregate is spent —
    /// callers must then answer UNKNOWN **without spawning**.
    fn next_child(&self) -> Option<Duration> {
        let left = self.deadline.checked_duration_since(Instant::now());
        match left {
            Some(left) if !left.is_zero() => Some(left.min(GIT_HISTORY_CHILD_TIMEOUT)),
            _ => None,
        }
    }
}

/// When a file was last touched, per git — tri-state.
///
/// The two-state `Option<String>` this replaced could not tell "git says this
/// path has no commits" from "git never answered", and the caller mapped BOTH
/// onto the literal `"1970-01-01T00:00:00Z"`. That sentinel is ~20,000 days
/// old, so `code_age > 90 && !has_test_files` fired and the feature was
/// published as **abandoned** with the specific-sounding prose "No git commits
/// touching this component since 1970-01-01" — a pure probe artifact,
/// indistinguishable from a real finding.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LastChange {
    /// git answered with a commit date (RFC3339).
    At(String),
    /// git answered, and no commit touches this path.
    Never,
    /// The probe did not answer: killed at its budget, unspawnable, or skipped
    /// because the request's aggregate git budget was already spent.
    Unknown,
}

fn git_last_change(project_path: &Path, file_path: &str, budget: &GitBudget) -> LastChange {
    // Bounded per child AND in aggregate: this is called twice per spec from
    // the feature-health handler, over an unbounded spec list.
    let Some(child_budget) = budget.next_child() else {
        return LastChange::Unknown;
    };
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["log", "-1", "--format=%aI", "--", file_path])
        .current_dir(project_path);
    let crate::process_helpers::ProbeOutcome::Captured(stdout) =
        crate::process_helpers::run_probe(cmd, child_budget, "development_intelligence: git log")
    else {
        return LastChange::Unknown;
    };

    let date = String::from_utf8_lossy(&stdout).trim().to_string();
    if date.is_empty() {
        LastChange::Never
    } else {
        LastChange::At(date)
    }
}

/// Commits touching `file_path` in the last `days`. `None` when the probe did
/// not answer — `0` is a real measurement and must not be manufactured.
fn git_commit_count_since(
    project_path: &Path,
    file_path: &str,
    days: u32,
    budget: &GitBudget,
) -> Option<usize> {
    let child_budget = budget.next_child()?;
    let since = format!("--since={} days ago", days);
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["log", "--oneline", &since, "--", file_path])
        .current_dir(project_path);
    let output = crate::process_helpers::run_probe(
        cmd,
        child_budget,
        "development_intelligence: git log --oneline",
    );

    match output {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => {
            Some(String::from_utf8_lossy(&stdout).lines().count())
        }
        crate::process_helpers::ProbeOutcome::Degraded(_) => None,
    }
}

/// Render a [`LastChange`] into the plain string the response carries.
///
/// * `At(d)`   → the real date.
/// * `Never`   → the epoch. This one IS a genuine "arbitrarily long ago": git
///   answered and no commit touches the path, so the abandoned classification
///   it produces is a real finding (the prose says "has ever" rather than
///   naming 1970).
/// * `Unknown` → **now**. An unreadable probe must not push a feature toward
///   stale/abandoned, and the caller has already forced `status = "unknown"`
///   plus an explicit signal, so the numeric fields only need to be inert.
fn render_last_change(c: &LastChange) -> String {
    match c {
        LastChange::At(d) => d.clone(),
        LastChange::Never => NEVER_COMMITTED_SENTINEL.to_string(),
        LastChange::Unknown => chrono::Utc::now().to_rfc3339(),
    }
}

/// The timestamp that stands for "git answered: no commit has ever touched
/// this path". Named rather than inlined so it can never again be reused as
/// the value for "the probe failed".
const NEVER_COMMITTED_SENTINEL: &str = "1970-01-01T00:00:00Z";

fn days_since(iso_date: &str) -> f64 {
    use chrono::{DateTime, Utc};
    match iso_date.parse::<DateTime<Utc>>() {
        Ok(dt) => {
            let now = Utc::now();
            (now - dt).num_hours() as f64 / 24.0
        }
        Err(_) => 999.0,
    }
}

// ============================================================================
// POST /development-intelligence/coverage-analysis
// ============================================================================

pub async fn coverage_analysis(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_path = PathBuf::from(&request.project_path);

    // Load specs on the async side (registry lookup is async); the
    // CPU-bound test-file scan still goes to spawn_blocking.
    let specs = load_specs(&request.app_id, &project_path).await;
    let result = spawn_blocking_tracked(move || {
        let test_files = scan_test_files(&project_path);

        let mut gaps: Vec<CoverageGap> = vec![];

        for spec in &specs {
            let page_id = spec
                .metadata
                .page_id
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            let component = spec.metadata.component.as_deref().unwrap_or("").to_string();

            let groups = spec.groups.as_deref().unwrap_or(&[]);

            // Find matching test files
            let matching_tests: Vec<&TestFileInfo> = test_files
                .iter()
                .filter(|tf| {
                    tf.referenced_components
                        .iter()
                        .any(|r| r == &component || r.to_lowercase() == page_id.to_lowercase())
                        || tf
                            .file_path
                            .to_lowercase()
                            .contains(&page_id.to_lowercase())
                })
                .collect();

            let total_assertions: usize = groups
                .iter()
                .map(|g| {
                    g.assertions
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|a| a.enabled.unwrap_or(true))
                        .count()
                })
                .sum();

            let critical_assertions: usize = groups
                .iter()
                .map(|g| {
                    g.assertions
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|a| {
                            a.enabled.unwrap_or(true) && a.severity.as_deref() == Some("critical")
                        })
                        .count()
                })
                .sum();

            let total_test_assertions: usize =
                matching_tests.iter().map(|tf| tf.assertion_count).sum();

            // Analyze per-group coverage
            let group_gaps: Vec<GapDetail> = groups
                .iter()
                .filter_map(|group| {
                    let enabled_count = group
                        .assertions
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|a| a.enabled.unwrap_or(true))
                        .count();

                    if enabled_count == 0 {
                        return None;
                    }

                    let group_name = group.name.as_deref().unwrap_or("").to_string();
                    let keywords = extract_keywords(&group_name);
                    let has_test = matching_tests.iter().any(|tf| {
                        keywords
                            .iter()
                            .any(|kw| tf.content.to_lowercase().contains(kw))
                    });

                    let max_severity = group
                        .assertions
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .filter(|a| a.enabled.unwrap_or(true))
                        .filter_map(|a| a.severity.as_deref())
                        .max_by_key(|s| match *s {
                            "critical" => 3,
                            "warning" => 2,
                            _ => 1,
                        })
                        .unwrap_or("info")
                        .to_string();

                    if !has_test {
                        Some(GapDetail {
                            spec_group_id: group.id.clone().unwrap_or_default(),
                            spec_group_name: group_name,
                            category: group.category.clone().unwrap_or_default(),
                            assertion_count: enabled_count,
                            has_corresponding_test: false,
                            severity: max_severity,
                        })
                    } else {
                        None
                    }
                })
                .collect();

            let coverage_score = if total_assertions > 0 {
                (total_test_assertions as f64 / total_assertions as f64).min(1.0)
            } else {
                0.0
            };

            gaps.push(CoverageGap {
                page: page_id,
                spec_assertion_count: total_assertions,
                critical_assertion_count: critical_assertions,
                test_file_count: matching_tests.len(),
                test_assertion_count: total_test_assertions,
                coverage_score,
                gaps: group_gaps,
            });
        }

        // Sort by critical gaps first, then by coverage score
        gaps.sort_by(|a, b| {
            let a_critical = a.gaps.iter().filter(|g| g.severity == "critical").count();
            let b_critical = b.gaps.iter().filter(|g| g.severity == "critical").count();
            b_critical.cmp(&a_critical).then(
                a.coverage_score
                    .partial_cmp(&b.coverage_score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });

        let well_covered = gaps.iter().filter(|g| g.coverage_score > 0.7).count();
        let partially_covered = gaps
            .iter()
            .filter(|g| g.coverage_score > 0.3 && g.coverage_score <= 0.7)
            .count();
        let uncovered = gaps.iter().filter(|g| g.coverage_score <= 0.3).count();
        let avg = if gaps.is_empty() {
            0.0
        } else {
            gaps.iter().map(|g| g.coverage_score).sum::<f64>() / gaps.len() as f64
        };

        CoverageAnalysisResult {
            summary: CoverageSummary {
                total_pages: gaps.len(),
                well_covered,
                partially_covered,
                uncovered,
                average_coverage_score: avg,
            },
            gaps,
        }
    })
    .await
    .map_err(|e| {
        error!("Coverage analysis task failed: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Analysis failed: {}", e),
        )
    })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": result,
    })))
}

// ============================================================================
// POST /development-intelligence/complexity-scores
// ============================================================================

pub async fn complexity_scores(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_path = PathBuf::from(&request.project_path);

    let specs = load_specs(&request.app_id, &project_path).await;
    let result = spawn_blocking_tracked(move || {
        let scores: Vec<ComplexityScore> = specs
            .iter()
            .map(|spec| {
                let page_id = spec
                    .metadata
                    .page_id
                    .as_deref()
                    .unwrap_or("unknown")
                    .to_string();
                let route = spec.metadata.page_url.as_deref().unwrap_or("/").to_string();

                let groups = spec.groups.as_deref().unwrap_or(&[]);
                let all_assertions: Vec<&SpecAssertion> = groups
                    .iter()
                    .flat_map(|g| g.assertions.as_deref().unwrap_or(&[]))
                    .filter(|a| a.enabled.unwrap_or(true))
                    .collect();

                let element_count = groups
                    .iter()
                    .filter(|g| g.category.as_deref() == Some("element-presence"))
                    .map(|g| {
                        g.assertions
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .filter(|a| a.enabled.unwrap_or(true))
                            .count()
                    })
                    .sum();

                let interaction_count = all_assertions
                    .iter()
                    .filter(|a| {
                        matches!(
                            a.category.as_deref(),
                            Some("interaction") | Some("behavior")
                        )
                    })
                    .count();

                let form_field_count = groups
                    .iter()
                    .filter(|g| g.category.as_deref() == Some("interaction"))
                    .filter(|g| {
                        let group_json = serde_json::to_string(g).unwrap_or_default();
                        group_json.to_lowercase().contains("form")
                            || group_json.to_lowercase().contains("input")
                            || group_json.to_lowercase().contains("select")
                            || group_json.to_lowercase().contains("textarea")
                    })
                    .map(|g| {
                        g.assertions
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .filter(|a| a.enabled.unwrap_or(true))
                            .count()
                    })
                    .sum();

                let scores = ComplexityScores {
                    element_count,
                    assertion_count: all_assertions.len(),
                    interaction_count,
                    form_field_count,
                    api_call_count: 0,
                    state_variable_count: 0,
                    component_depth: 0,
                };

                let raw = scores.element_count * 2
                    + scores.assertion_count
                    + scores.interaction_count * 3
                    + scores.form_field_count * 2
                    + scores.api_call_count * 4
                    + scores.state_variable_count * 2
                    + scores.component_depth * 5;

                let composite = ((100 * raw) / (raw + 100)) as u32;

                let tier = if composite < 25 {
                    "simple"
                } else if composite < 50 {
                    "moderate"
                } else if composite < 75 {
                    "complex"
                } else {
                    "critical"
                }
                .to_string();

                ComplexityScore {
                    page: page_id,
                    route,
                    scores,
                    composite,
                    tier,
                }
            })
            .collect();

        let total = scores.len();
        let simple = scores.iter().filter(|s| s.tier == "simple").count();
        let moderate = scores.iter().filter(|s| s.tier == "moderate").count();
        let complex = scores.iter().filter(|s| s.tier == "complex").count();
        let critical = scores.iter().filter(|s| s.tier == "critical").count();
        let avg = if total > 0 {
            scores.iter().map(|s| s.composite as f64).sum::<f64>() / total as f64
        } else {
            0.0
        };

        ComplexityAnalysisResult {
            summary: ComplexitySummary {
                total_pages: total,
                simple,
                moderate,
                complex,
                critical,
                average_composite: avg,
                drift_alerts: vec![],
            },
            scores,
        }
    })
    .await
    .map_err(|e| {
        error!("Complexity analysis task failed: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Analysis failed: {}", e),
        )
    })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": result,
    })))
}

// ============================================================================
// POST /development-intelligence/feature-health
// ============================================================================

pub async fn feature_health(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_path = PathBuf::from(&request.project_path);

    let specs = load_specs(&request.app_id, &project_path).await;
    let result = spawn_blocking_tracked(move || {
        let test_files = scan_test_files(&project_path);
        // ONE wall-clock budget for every git child of this request — see
        // `GIT_HISTORY_TOTAL_BUDGET`. `specs` is unbounded, so a per-child
        // bound alone leaves the aggregate unbounded.
        let budget = GitBudget::new(GIT_HISTORY_TOTAL_BUDGET);

        let features: Vec<FeatureHealth> = specs
            .iter()
            .map(|spec| {
                let page_id = spec
                    .metadata
                    .page_id
                    .as_deref()
                    .unwrap_or("unknown")
                    .to_string();
                let route = spec.metadata.page_url.as_deref().unwrap_or("/").to_string();
                let component = spec.metadata.component.as_deref().unwrap_or("").to_string();

                // Spec file path within the runner repo, used for git history lookup.
                // Reflects the Spec API storage layout under <runner>/specs/pages/.
                let spec_file = format!("specs/pages/{}/spec.uibridge.json", page_id);

                // Component path (best guess)
                let component_path = format!("src/components/{}", page_id);

                // Git history — tri-state, so a probe that never answered can
                // never be rendered as a date.
                let code_change = git_last_change(&project_path, &component_path, &budget);
                let spec_change = match spec.metadata.updated_at.clone() {
                    // The spec's own metadata is authoritative and costs no git.
                    Some(updated) => LastChange::At(updated),
                    None => git_last_change(&project_path, &spec_file, &budget),
                };
                let code_commits_30d =
                    git_commit_count_since(&project_path, &component_path, 30, &budget);

                let has_test_files = test_files.iter().any(|tf| {
                    tf.referenced_components.iter().any(|r| r == &component)
                        || tf
                            .file_path
                            .to_lowercase()
                            .contains(&page_id.to_lowercase())
                });

                // Wire shapes: `lastCodeChange` is a plain string and
                // `codeAge` a plain number in the response the dashboard
                // parses (it re-derives its own ages from `lastCodeChange`),
                // so an UNKNOWN cannot be spelled as null without a frontend
                // change. It is instead spelled as `status = "unknown"` plus an
                // explicit signal, and the numeric fields are rendered as
                // "just now" — the one value that cannot masquerade as a
                // finding. What must NEVER reappear is the 1970 sentinel, which
                // read as a 20,000-day-old file and classified the feature as
                // abandoned.
                let history_unknown =
                    code_change == LastChange::Unknown || spec_change == LastChange::Unknown;

                let last_code_change = render_last_change(&code_change);
                let last_spec_change = render_last_change(&spec_change);
                let code_age = days_since(&last_code_change);
                let spec_age = days_since(&last_spec_change);

                // Classification
                let mut signals = vec![];
                let status;

                if history_unknown {
                    // Say so, in the status AND in the prose. Anything else
                    // publishes a classification derived from a probe that
                    // never ran.
                    status = "unknown".to_string();
                    signals.push(
                        "Git history for this feature could not be read (the probe timed out, \
                         failed to spawn, or the request's git budget was exhausted) — its age \
                         is UNKNOWN, not old"
                            .to_string(),
                    );
                } else if code_age <= 30.0 && spec_age <= 60.0 {
                    status = "active".to_string();
                } else if code_age <= 30.0 && spec_age > 90.0 {
                    status = "spec-drift".to_string();
                    signals.push(match code_commits_30d {
                        Some(n) => format!(
                            "Component modified {} times in 30 days but spec unchanged for {} months",
                            n,
                            (spec_age / 30.0).round() as u32,
                        ),
                        None => format!(
                            "Component changed recently but spec unchanged for {} months \
                             (30-day commit count unavailable — the git probe degraded)",
                            (spec_age / 30.0).round() as u32,
                        ),
                    });
                } else if code_age > 90.0 && !has_test_files {
                    status = "abandoned".to_string();
                    signals.push(match &code_change {
                        LastChange::Never => {
                            "No git commit has ever touched this component path".to_string()
                        }
                        _ => format!(
                            "No git commits touching this component since {}",
                            truncate_str(&last_code_change, 10),
                        ),
                    });
                    signals.push("No test files reference this component".to_string());
                } else if code_age > 60.0 {
                    status = "stale".to_string();
                    signals.push(format!(
                        "No code changes in {} days",
                        code_age.round() as u32
                    ));
                } else {
                    status = "active".to_string();
                }

                let staleness = ((code_age / 180.0) + (spec_age / 180.0)) / 2.0;
                let staleness = staleness.min(1.0);

                FeatureHealth {
                    page: page_id,
                    route,
                    component_path,
                    status,
                    last_code_change,
                    last_spec_change,
                    code_commit_count_30d: code_commits_30d.unwrap_or(0),
                    code_commit_count_known: code_commits_30d.is_some(),
                    spec_age,
                    code_age,
                    staleness,
                    signals,
                }
            })
            .collect();

        let total = features.len();
        let active = features.iter().filter(|f| f.status == "active").count();
        let stale = features.iter().filter(|f| f.status == "stale").count();
        let abandoned = features.iter().filter(|f| f.status == "abandoned").count();
        let spec_drift = features.iter().filter(|f| f.status == "spec-drift").count();
        let unknown = features.iter().filter(|f| f.status == "unknown").count();
        if unknown > 0 {
            warn!(
                "feature_health: {unknown}/{total} features have UNREADABLE git history \
                 (probe degraded or the {}s aggregate git budget was exhausted)",
                GIT_HISTORY_TOTAL_BUDGET.as_secs()
            );
        }

        FeatureHealthResult {
            summary: FeatureHealthSummary {
                total,
                active,
                stale,
                abandoned,
                spec_drift,
                unknown,
            },
            features,
        }
    })
    .await
    .map_err(|e| {
        error!("Feature health analysis task failed: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Analysis failed: {}", e),
        )
    })?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": result,
    })))
}

// ============================================================================
// GET /development-intelligence/trends
// ============================================================================

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
pub async fn get_trends(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let project_path = request.project_path;

    // Query historical data from PostgreSQL via pool
    let trends: Vec<TrendEntry> = match state.app_state.pg_db.pool().get().await {
        Ok(client) => {
            match client
                .query(
                    "SELECT page_route, score, analysis_type, created_at::text as created_at_text \
                     FROM development_intelligence \
                     WHERE project_path = $1 \
                     ORDER BY created_at DESC \
                     LIMIT 500",
                    &[&project_path],
                )
                .await
            {
                Ok(rows) => rows
                    .iter()
                    .map(|row| TrendEntry {
                        page_route: row.get::<_, String>("page_route"),
                        score: row.get::<_, f64>("score"),
                        analysis_type: row.get::<_, String>("analysis_type"),
                        created_at: row.get::<_, String>("created_at_text"),
                    })
                    .collect(),
                Err(e) => {
                    warn!("Failed to query trends: {}", e);
                    vec![]
                }
            }
        }
        Err(e) => {
            warn!("Failed to get PG connection for trends: {}", e);
            vec![]
        }
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "data": trends,
    })))
}

// ============================================================================
// Route registration
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/development-intelligence/coverage-analysis",
            post(coverage_analysis),
        )
        .route(
            "/development-intelligence/complexity-scores",
            post(complexity_scores),
        )
        .route(
            "/development-intelligence/feature-health",
            post(feature_health),
        )
        .route("/development-intelligence/trends", post(get_trends))
}
