//! Spec accuracy analysis methods.
//!
//! Five independent methods for measuring whether specs correctly describe what
//! the app should do: element coverage, cross-page consistency, mutation testing,
//! freshness detection, and AI completeness review.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::database::pg::PgDb;

// ── Types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementCoverageResult {
    pub spec_target_coverage: f64,
    pub element_coverage: f64,
    pub total_elements: u32,
    pub interactive_elements: u32,
    pub elements_with_assertions: u32,
    pub unmatched_targets: Vec<String>,
    pub uncovered_elements: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossPageConsistencyResult {
    pub overall_consistency: f64,
    pub category_coverage: HashMap<String, f64>,
    pub per_spec_scores: Vec<SpecConsistencyScore>,
    pub outlier_specs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecConsistencyScore {
    pub spec_id: String,
    pub covered_categories: Vec<String>,
    pub missing_categories: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationTestResult {
    pub total_mutations: u32,
    pub killed_mutations: u32,
    pub survived_mutations: u32,
    pub mutation_score: f64,
    pub mutation_details: Vec<MutationDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationDetail {
    pub assertion_id: String,
    pub mutation_type: String,
    pub killed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshnessResult {
    pub is_stale: bool,
    pub spec_modified: Option<String>,
    pub newest_component: Option<String>,
    pub staleness_days: i64,
    pub component_files_checked: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletenessResult {
    pub existing_assertions: u32,
    pub suggested_additions: Vec<SuggestedAssertion>,
    pub completeness_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedAssertion {
    pub description: String,
    pub category: String,
    pub severity: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecAccuracyRecord {
    pub id: String,
    pub spec_id: String,
    pub analysis_type: String,
    pub score: f64,
    pub detail_json: String,
    pub created_at: String,
}

// ── Coverage categories for cross-page consistency ────────────────────

const COVERAGE_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "error_handling",
        &["error", "fail", "invalid", "reject", "exception"],
    ),
    (
        "loading_states",
        &["loading", "spinner", "skeleton", "pending", "progress"],
    ),
    (
        "empty_states",
        &["empty", "no data", "no results", "nothing", "placeholder"],
    ),
    (
        "accessibility",
        &[
            "aria",
            "role",
            "label",
            "accessible",
            "screen reader",
            "tab",
            "focus",
        ],
    ),
    (
        "navigation",
        &["nav", "link", "route", "breadcrumb", "menu", "sidebar"],
    ),
];

// ── Element Coverage (Method 1) ───────────────────────────────────────

/// Analyze element coverage: how well do spec assertions cover the actual page elements.
pub fn analyze_element_coverage(
    spec_config: &serde_json::Value,
    snapshot_elements: &[serde_json::Value],
) -> Result<ElementCoverageResult, String> {
    // Extract assertion targets from spec
    let mut targets: Vec<String> = Vec::new();
    let mut target_criteria: Vec<serde_json::Value> = Vec::new();

    if let Some(groups) = spec_config.get("groups").and_then(|g| g.as_array()) {
        for group in groups {
            if let Some(assertions) = group.get("assertions").and_then(|a| a.as_array()) {
                for assertion in assertions {
                    if let Some(enabled) = assertion.get("enabled").and_then(|e| e.as_bool()) {
                        if !enabled {
                            continue;
                        }
                    }
                    if let Some(target) = assertion.get("target") {
                        let label = target
                            .get("label")
                            .and_then(|l| l.as_str())
                            .or_else(|| assertion.get("description").and_then(|d| d.as_str()))
                            .unwrap_or("unknown")
                            .to_string();
                        targets.push(label);

                        if let Some(criteria) = target.get("criteria") {
                            target_criteria.push(criteria.clone());
                        }
                    }
                }
            }
        }
    }

    // Also check top-level assertions
    if let Some(assertions) = spec_config.get("assertions").and_then(|a| a.as_array()) {
        for assertion in assertions {
            if let Some(enabled) = assertion.get("enabled").and_then(|e| e.as_bool()) {
                if !enabled {
                    continue;
                }
            }
            if let Some(target) = assertion.get("target") {
                let label = target
                    .get("label")
                    .and_then(|l| l.as_str())
                    .or_else(|| assertion.get("description").and_then(|d| d.as_str()))
                    .unwrap_or("unknown")
                    .to_string();
                targets.push(label);

                if let Some(criteria) = target.get("criteria") {
                    target_criteria.push(criteria.clone());
                }
            }
        }
    }

    // Identify interactive elements from snapshot
    let interactive_types = [
        "button", "input", "select", "textarea", "a", "link", "checkbox", "radio", "switch",
        "slider", "tab", "menuitem",
    ];

    let interactive_elements: Vec<&serde_json::Value> = snapshot_elements
        .iter()
        .filter(|el| {
            let el_type = el.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let el_role = el
                .get("state")
                .and_then(|s| s.get("role"))
                .and_then(|r| r.as_str())
                .unwrap_or("");
            interactive_types
                .iter()
                .any(|t| el_type.eq_ignore_ascii_case(t) || el_role.eq_ignore_ascii_case(t))
        })
        .collect();

    // Check which targets match elements
    let mut unmatched_targets = Vec::new();
    let mut matched_count = 0;

    for (i, criteria) in target_criteria.iter().enumerate() {
        let label = targets.get(i).cloned().unwrap_or_default();
        let has_match = snapshot_elements
            .iter()
            .any(|el| element_matches_spec_criteria(el, criteria));
        if has_match {
            matched_count += 1;
        } else {
            unmatched_targets.push(label);
        }
    }

    // Check which interactive elements have assertions covering them
    let mut covered_elements = 0;
    let mut uncovered_elements = Vec::new();

    for el in &interactive_elements {
        let is_covered = target_criteria
            .iter()
            .any(|criteria| element_matches_spec_criteria(el, criteria));
        if is_covered {
            covered_elements += 1;
        } else {
            let label = el
                .get("label")
                .and_then(|l| l.as_str())
                .or_else(|| {
                    el.get("state")
                        .and_then(|s| s.get("textContent"))
                        .and_then(|t| t.as_str())
                })
                .unwrap_or("(unlabeled)")
                .to_string();
            let el_type = el.get("type").and_then(|t| t.as_str()).unwrap_or("unknown");
            uncovered_elements.push(format!("{} [{}]", label, el_type));
        }
    }

    let spec_target_coverage = if targets.is_empty() {
        1.0
    } else {
        matched_count as f64 / targets.len() as f64
    };

    let element_coverage = if interactive_elements.is_empty() {
        1.0
    } else {
        covered_elements as f64 / interactive_elements.len() as f64
    };

    // Limit uncovered list to avoid huge payloads
    uncovered_elements.truncate(50);
    unmatched_targets.truncate(50);

    Ok(ElementCoverageResult {
        spec_target_coverage,
        element_coverage,
        total_elements: snapshot_elements.len() as u32,
        interactive_elements: interactive_elements.len() as u32,
        elements_with_assertions: covered_elements as u32,
        unmatched_targets,
        uncovered_elements,
    })
}

// ── Cross-Page Consistency (Method 2) ─────────────────────────────────

/// Analyze cross-page consistency across multiple specs.
pub fn analyze_cross_page_consistency(
    specs: &[(String, serde_json::Value)],
) -> CrossPageConsistencyResult {
    if specs.is_empty() {
        return CrossPageConsistencyResult {
            overall_consistency: 1.0,
            category_coverage: HashMap::new(),
            per_spec_scores: Vec::new(),
            outlier_specs: Vec::new(),
        };
    }

    // For each spec, check which categories are covered
    let mut spec_categories: HashMap<String, Vec<String>> = HashMap::new();

    for (spec_id, config) in specs {
        let mut covered = Vec::new();

        // Collect all assertion descriptions
        let descriptions = collect_assertion_descriptions(config);

        for (category, keywords) in COVERAGE_CATEGORIES {
            let desc_lower: Vec<String> = descriptions.iter().map(|d| d.to_lowercase()).collect();
            let is_covered = keywords
                .iter()
                .any(|kw| desc_lower.iter().any(|d| d.contains(kw)));
            if is_covered {
                covered.push(category.to_string());
            }
        }

        spec_categories.insert(spec_id.clone(), covered);
    }

    // Compute per-category coverage: what % of specs cover each category
    let total_specs = specs.len() as f64;
    let mut category_coverage: HashMap<String, f64> = HashMap::new();

    for (category, _) in COVERAGE_CATEGORIES {
        let count = spec_categories
            .values()
            .filter(|cats| cats.contains(&category.to_string()))
            .count();
        category_coverage.insert(category.to_string(), count as f64 / total_specs);
    }

    // Flag specs missing categories covered by >70% of other specs
    let common_categories: Vec<String> = category_coverage
        .iter()
        .filter(|(_, coverage)| **coverage > 0.7)
        .map(|(cat, _)| cat.clone())
        .collect();

    let total_categories = COVERAGE_CATEGORIES.len() as f64;
    let mut per_spec_scores = Vec::new();
    let mut outlier_specs = Vec::new();

    for (spec_id, covered) in &spec_categories {
        let missing: Vec<String> = common_categories
            .iter()
            .filter(|c| !covered.contains(c))
            .cloned()
            .collect();

        let score = covered.len() as f64 / total_categories;

        if !missing.is_empty() {
            outlier_specs.push(spec_id.clone());
        }

        per_spec_scores.push(SpecConsistencyScore {
            spec_id: spec_id.clone(),
            covered_categories: covered.clone(),
            missing_categories: missing,
            score,
        });
    }

    let overall_consistency = if per_spec_scores.is_empty() {
        1.0
    } else {
        per_spec_scores.iter().map(|s| s.score).sum::<f64>() / per_spec_scores.len() as f64
    };

    CrossPageConsistencyResult {
        overall_consistency,
        category_coverage,
        per_spec_scores,
        outlier_specs,
    }
}

// ── Mutation Testing (Method 3) ───────────────────────────────────────

/// Run in-memory mutation testing against a spec's assertions.
///
/// For each assertion, create a mutation of the snapshot elements that should
/// cause the assertion to fail. If the assertion correctly detects the mutation,
/// it's "killed". If it still passes, the assertion is weak.
pub fn run_mutation_test(
    spec_config: &serde_json::Value,
    snapshot_elements: &[serde_json::Value],
) -> Result<MutationTestResult, String> {
    let mut details = Vec::new();
    let mut killed = 0u32;
    let mut survived = 0u32;

    // Extract assertions from spec groups
    let assertions = collect_spec_assertions(spec_config);

    for assertion in &assertions {
        let assertion_type = assertion
            .get("assertionType")
            .and_then(|t| t.as_str())
            .unwrap_or("exists");
        let assertion_id = assertion
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();

        let criteria = assertion
            .get("target")
            .and_then(|t| t.get("criteria"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let (mutation_type, mutated_elements) =
            create_mutation(assertion_type, &criteria, snapshot_elements);

        if mutation_type.is_empty() {
            continue; // Can't create meaningful mutation for this assertion type
        }

        // Re-evaluate the assertion against mutated elements
        let (still_passes, detail) =
            evaluate_assertion_against_elements(assertion, &mutated_elements);

        let is_killed = !still_passes;

        details.push(MutationDetail {
            assertion_id: assertion_id.clone(),
            mutation_type: mutation_type.clone(),
            killed: is_killed,
            detail: if is_killed {
                format!("Mutation correctly detected: {}", detail)
            } else {
                format!("Mutation survived: {}", detail)
            },
        });

        if is_killed {
            killed += 1;
        } else {
            survived += 1;
        }
    }

    let total = killed + survived;
    let mutation_score = if total > 0 {
        killed as f64 / total as f64
    } else {
        1.0
    };

    Ok(MutationTestResult {
        total_mutations: total,
        killed_mutations: killed,
        survived_mutations: survived,
        mutation_score,
        mutation_details: details,
    })
}

// ── Freshness Detection (Method 4) ────────────────────────────────────

/// Analyze freshness of a spec file relative to its component files.
pub fn analyze_freshness(spec_file_path: &Path, component_paths: &[PathBuf]) -> FreshnessResult {
    let spec_mtime = std::fs::metadata(spec_file_path)
        .and_then(|m| m.modified())
        .ok();

    let mut newest_component_time = None;
    let mut newest_component_path = None;
    let mut files_checked = 0u32;

    for path in component_paths {
        if let Ok(meta) = std::fs::metadata(path) {
            if let Ok(mtime) = meta.modified() {
                files_checked += 1;
                if newest_component_time.is_none() || Some(mtime) > newest_component_time {
                    newest_component_time = Some(mtime);
                    newest_component_path = Some(path.clone());
                }
            }
        }
    }

    let (is_stale, staleness_days) = match (spec_mtime, newest_component_time) {
        (Some(spec_t), Some(comp_t)) => {
            if comp_t > spec_t {
                let duration = comp_t
                    .duration_since(spec_t)
                    .unwrap_or(std::time::Duration::ZERO);
                (true, duration.as_secs() as i64 / 86400)
            } else {
                (false, 0)
            }
        }
        _ => (false, 0),
    };

    let format_time = |t: std::time::SystemTime| -> String {
        let datetime: chrono::DateTime<chrono::Utc> = t.into();
        datetime.to_rfc3339()
    };

    FreshnessResult {
        is_stale,
        spec_modified: spec_mtime.map(format_time),
        newest_component: newest_component_path.map(|p| p.to_string_lossy().to_string()),
        staleness_days,
        component_files_checked: files_checked,
    }
}

/// Analyze freshness for all spec files.
///
/// `specs_root` is the Spec API storage root (e.g. `<runner>/specs/`); page
/// projections live at `<specs_root>/pages/<id>/spec.uibridge.json`. `app_id`
/// scopes the embedded-snapshot fallback (PLAN.md §B.6 — only the
/// `qontinui-runner` app has embedded pages).
pub fn analyze_all_freshness(
    app_id: &str,
    specs_root: &Path,
    components_dir: &Path,
) -> Vec<(String, FreshnessResult)> {
    let mut results = Vec::new();

    let page_ids = match crate::spec_api::storage::list_pages(specs_root, app_id) {
        Ok(ids) => ids,
        Err(_) => return results,
    };

    for page_id in page_ids {
        let projection_path = specs_root
            .join("pages")
            .join(&page_id)
            .join("spec.uibridge.json");
        if !projection_path.exists() {
            // Page exists only in the embedded snapshot — no on-disk mtime to
            // compare against, so skip it for freshness analysis.
            continue;
        }

        // Heuristically find component files using the page id (same convention
        // as the legacy filename without the `.spec.uibridge` suffix).
        let component_paths = find_component_files(&page_id, components_dir);

        let result = analyze_freshness(&projection_path, &component_paths);
        results.push((page_id, result));
    }

    results
}

// ── Persistence ───────────────────────────────────────────────────────

/// Store a spec accuracy result.
pub async fn store_accuracy_result(
    pg: &PgDb,
    spec_id: &str,
    analysis_type: &str,
    score: f64,
    detail_json: &str,
) -> Result<String, String> {
    let id = format!("sar-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    pg.insert_spec_accuracy_result(&id, spec_id, analysis_type, score, detail_json, &now)
        .await?;

    Ok(id)
}

/// Get accuracy results for a spec.
pub async fn get_accuracy_results(
    pg: &PgDb,
    spec_id: Option<&str>,
    analysis_type: Option<&str>,
) -> Result<Vec<SpecAccuracyRecord>, String> {
    let rows = pg.get_accuracy_results(spec_id, analysis_type).await?;
    Ok(rows
        .into_iter()
        .map(|r| SpecAccuracyRecord {
            id: r.id,
            spec_id: r.spec_id,
            analysis_type: r.analysis_type,
            score: r.score,
            detail_json: r.detail_json,
            created_at: r.created_at,
        })
        .collect())
}

// ── Helpers ───────────────────────────────────────────────────────────

fn collect_assertion_descriptions(config: &serde_json::Value) -> Vec<String> {
    let mut descriptions = Vec::new();

    if let Some(groups) = config.get("groups").and_then(|g| g.as_array()) {
        for group in groups {
            if let Some(assertions) = group.get("assertions").and_then(|a| a.as_array()) {
                for assertion in assertions {
                    if let Some(desc) = assertion.get("description").and_then(|d| d.as_str()) {
                        descriptions.push(desc.to_string());
                    }
                }
            }
            // Include group-level description too
            if let Some(desc) = group.get("description").and_then(|d| d.as_str()) {
                descriptions.push(desc.to_string());
            }
            // Include category
            if let Some(cat) = group.get("category").and_then(|c| c.as_str()) {
                descriptions.push(cat.to_string());
            }
        }
    }

    descriptions
}

fn collect_spec_assertions(config: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut assertions = Vec::new();

    if let Some(groups) = config.get("groups").and_then(|g| g.as_array()) {
        for group in groups {
            if let Some(group_assertions) = group.get("assertions").and_then(|a| a.as_array()) {
                for assertion in group_assertions {
                    let enabled = assertion
                        .get("enabled")
                        .and_then(|e| e.as_bool())
                        .unwrap_or(true);
                    if enabled {
                        assertions.push(assertion.clone());
                    }
                }
            }
        }
    }

    if let Some(top_assertions) = config.get("assertions").and_then(|a| a.as_array()) {
        for assertion in top_assertions {
            let enabled = assertion
                .get("enabled")
                .and_then(|e| e.as_bool())
                .unwrap_or(true);
            if enabled {
                assertions.push(assertion.clone());
            }
        }
    }

    assertions
}

/// Simplified element matching for spec criteria against snapshot elements.
fn element_matches_spec_criteria(
    element: &serde_json::Value,
    criteria: &serde_json::Value,
) -> bool {
    let criteria_map = match criteria.as_object() {
        Some(m) => m,
        None => return false,
    };

    if criteria_map.is_empty() {
        return false;
    }

    for (key, value) in criteria_map {
        let value_str = value.as_str().unwrap_or("");
        if value_str.is_empty() {
            continue;
        }

        let matches = match key.as_str() {
            "role" => {
                let el_type = element.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let el_role = element
                    .get("state")
                    .and_then(|s| s.get("role"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                el_type.eq_ignore_ascii_case(value_str) || el_role.eq_ignore_ascii_case(value_str)
            }
            "textContent" | "text" => {
                let text = element
                    .get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|t| t.as_str())
                    .or_else(|| element.get("label").and_then(|l| l.as_str()))
                    .unwrap_or("");
                text.to_lowercase().contains(&value_str.to_lowercase())
            }
            "label" => {
                let label = element.get("label").and_then(|l| l.as_str()).unwrap_or("");
                label.to_lowercase().contains(&value_str.to_lowercase())
            }
            "type" => {
                let el_type = element.get("type").and_then(|t| t.as_str()).unwrap_or("");
                el_type.eq_ignore_ascii_case(value_str)
            }
            "id" | "elementId" => {
                let el_id = element.get("id").and_then(|i| i.as_str()).unwrap_or("");
                el_id == value_str
            }
            _ => true, // Unknown criteria key → don't filter on it
        };

        if !matches {
            return false;
        }
    }

    true
}

/// Create a mutation of snapshot elements for testing an assertion.
/// Returns (mutation_type, mutated_elements).
fn create_mutation(
    assertion_type: &str,
    criteria: &serde_json::Value,
    elements: &[serde_json::Value],
) -> (String, Vec<serde_json::Value>) {
    match assertion_type {
        "exists" => {
            // Remove matching elements
            let mutated: Vec<serde_json::Value> = elements
                .iter()
                .filter(|el| !element_matches_spec_criteria(el, criteria))
                .cloned()
                .collect();
            if mutated.len() < elements.len() {
                ("remove_element".to_string(), mutated)
            } else {
                (String::new(), Vec::new()) // No element matched, can't mutate
            }
        }
        "visible" => {
            // Set matching elements' visible to false
            let mutated: Vec<serde_json::Value> = elements
                .iter()
                .map(|el| {
                    if element_matches_spec_criteria(el, criteria) {
                        let mut el = el.clone();
                        if let Some(state) = el.get_mut("state") {
                            if let Some(obj) = state.as_object_mut() {
                                obj.insert("visible".to_string(), serde_json::Value::Bool(false));
                            }
                        }
                        el
                    } else {
                        el.clone()
                    }
                })
                .collect();
            ("hide_element".to_string(), mutated)
        }
        "contains" => {
            // Change text content of matching elements
            let mutated: Vec<serde_json::Value> = elements
                .iter()
                .map(|el| {
                    if element_matches_spec_criteria(el, criteria) {
                        let mut el = el.clone();
                        if let Some(state) = el.get_mut("state") {
                            if let Some(obj) = state.as_object_mut() {
                                obj.insert(
                                    "textContent".to_string(),
                                    serde_json::Value::String(
                                        "__MUTATED_TEXT_CONTENT__".to_string(),
                                    ),
                                );
                            }
                        }
                        el
                    } else {
                        el.clone()
                    }
                })
                .collect();
            ("change_text".to_string(), mutated)
        }
        "not_exists" => {
            // Add a matching element
            let mut mutated = elements.to_vec();
            let fake_element = serde_json::json!({
                "type": "div",
                "id": "mutated-fake-element",
                "label": "Mutated Element",
                "state": {
                    "visible": true,
                    "textContent": "Mutated"
                }
            });
            mutated.push(fake_element);
            ("add_element".to_string(), mutated)
        }
        _ => (String::new(), Vec::new()),
    }
}

/// Simplified assertion evaluation for mutation testing.
fn evaluate_assertion_against_elements(
    assertion: &serde_json::Value,
    elements: &[serde_json::Value],
) -> (bool, String) {
    let assertion_type = assertion
        .get("assertionType")
        .and_then(|t| t.as_str())
        .unwrap_or("exists");

    let criteria = assertion
        .get("target")
        .and_then(|t| t.get("criteria"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let matching: Vec<&serde_json::Value> = elements
        .iter()
        .filter(|el| element_matches_spec_criteria(el, &criteria))
        .collect();

    match assertion_type {
        "exists" => {
            if matching.is_empty() {
                (false, "No matching element found".to_string())
            } else {
                (true, format!("Found {} element(s)", matching.len()))
            }
        }
        "not_exists" => {
            if matching.is_empty() {
                (true, "No matching element (as expected)".to_string())
            } else {
                (
                    false,
                    format!("Found {} unexpected element(s)", matching.len()),
                )
            }
        }
        "visible" => {
            let visible = matching.iter().any(|el| {
                el.get("state")
                    .and_then(|s| s.get("visible"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            });
            if visible {
                (true, "Visible element found".to_string())
            } else {
                (false, "No visible matching element".to_string())
            }
        }
        "contains" => {
            let expected = assertion
                .get("expected")
                .and_then(|e| e.as_str())
                .unwrap_or("");
            let has_text = matching.iter().any(|el| {
                let text = el
                    .get("state")
                    .and_then(|s| s.get("textContent"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                text.contains(expected)
            });
            if has_text {
                (true, format!("Found element containing '{}'", expected))
            } else {
                (false, format!("No element contains '{}'", expected))
            }
        }
        _ => (
            true,
            "Unsupported assertion type for mutation test".to_string(),
        ),
    }
}

/// Heuristically find component files related to a spec name.
fn find_component_files(spec_name: &str, components_dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();

    // Convert spec name patterns like "active" → possible component files
    let patterns = [
        format!("{}.tsx", spec_name),
        format!("{}.ts", spec_name),
        format!("{}Page.tsx", capitalize(spec_name)),
        format!("{}Tab.tsx", capitalize(spec_name)),
        format!("{}Panel.tsx", capitalize(spec_name)),
    ];

    // Walk the components directory
    if let Ok(entries) = walkdir(components_dir) {
        for entry in entries {
            let file_name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if patterns.iter().any(|p| file_name == p.as_str())
                || file_name.to_lowercase().contains(&spec_name.to_lowercase())
                    && (file_name.ends_with(".tsx") || file_name.ends_with(".ts"))
            {
                results.push(entry);
            }
        }
    }

    results
}

fn walkdir(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(walkdir(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
