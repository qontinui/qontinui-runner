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
use tracing::{error, info, warn};

use crate::mcp::types::ApiState;

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
    status: String,
    last_code_change: String,
    last_spec_change: String,
    code_commit_count_30d: usize,
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

fn git_last_change(project_path: &Path, file_path: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%aI", "--", file_path])
        .current_dir(project_path)
        .output()
        .ok()?;

    let date = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if date.is_empty() {
        None
    } else {
        Some(date)
    }
}

fn git_commit_count_since(project_path: &Path, file_path: &str, days: u32) -> usize {
    let since = format!("--since={} days ago", days);
    let output = std::process::Command::new("git")
        .args(["log", "--oneline", &since, "--", file_path])
        .current_dir(project_path)
        .output();

    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().count(),
        Err(_) => 0,
    }
}

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
    let result = tokio::task::spawn_blocking(move || {
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
    let result = tokio::task::spawn_blocking(move || {
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
    let result = tokio::task::spawn_blocking(move || {
        let test_files = scan_test_files(&project_path);

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

                // Git history
                let last_code_change = git_last_change(&project_path, &component_path)
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
                let last_spec_change = spec
                    .metadata
                    .updated_at
                    .clone()
                    .or_else(|| git_last_change(&project_path, &spec_file))
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

                let code_age = days_since(&last_code_change);
                let spec_age = days_since(&last_spec_change);
                let code_commits_30d = git_commit_count_since(&project_path, &component_path, 30);

                let has_test_files = test_files.iter().any(|tf| {
                    tf.referenced_components.iter().any(|r| r == &component)
                        || tf
                            .file_path
                            .to_lowercase()
                            .contains(&page_id.to_lowercase())
                });

                // Classification
                let mut signals = vec![];
                let status;

                if code_age <= 30.0 && spec_age <= 60.0 {
                    status = "active".to_string();
                } else if code_age <= 30.0 && spec_age > 90.0 {
                    status = "spec-drift".to_string();
                    signals.push(format!(
                        "Component modified {} times in 30 days but spec unchanged for {} months",
                        code_commits_30d,
                        (spec_age / 30.0).round() as u32,
                    ));
                } else if code_age > 90.0 && !has_test_files {
                    status = "abandoned".to_string();
                    signals.push(format!(
                        "No git commits touching this component since {}",
                        &last_code_change[..10],
                    ));
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
                    code_commit_count_30d: code_commits_30d,
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

        FeatureHealthResult {
            summary: FeatureHealthSummary {
                total,
                active,
                stale,
                abandoned,
                spec_drift,
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
