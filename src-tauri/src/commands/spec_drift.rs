//! Spec drift detection.
//!
//! Scans the runner source tree for `useUIElement(...)` registrations and
//! compares them against the assertions declared in every page projection
//! served by the Spec API (`<runner>/specs/pages/<id>/spec.uibridge.json`).
//! Reports:
//! - `missing_from_spec`: elements registered in code but not referenced by
//!   any assertion's `target.elementId` (covers the case where a new button
//!   was added in code but the spec wasn't updated).
//! - `orphans_in_spec`: assertions that reference element IDs that no longer
//!   appear in the source tree (covers the case where a button was removed
//!   or renamed but its assertion was left behind).
//!
//! This command is intentionally read-only: it produces a report and does
//! not modify spec files. The user (or a future tool) decides whether to
//! add missing assertions or remove orphan ones.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::database::pg::PgDb;
use crate::spec_api::storage;

#[derive(Debug, Serialize, Deserialize)]
pub struct RegisteredElement {
    pub id: String,
    pub label: Option<String>,
    pub file: String,
    pub line: usize,
    /// Suggested assertion JSON fragment the developer can paste into a spec.
    pub suggested_assertion: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpecOrphan {
    pub element_id: String,
    pub spec_id: String,
    pub assertion_id: String,
    pub label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DriftReport {
    /// spec-multi-app Stream E.7 — the registered app this report describes.
    /// Threaded from the `scan_spec_drift` argument so downstream consumers
    /// (proposals.rs patch routing, CLI surfaces) can group/filter by app.
    pub app_id: String,
    pub scanned_at: String,
    pub source_files_scanned: usize,
    pub spec_files_scanned: usize,
    pub total_registered_elements: usize,
    pub total_asserted_ids: usize,
    pub missing_from_spec: Vec<RegisteredElement>,
    pub orphans_in_spec: Vec<SpecOrphan>,
}

fn extract_field_value(body: &str, field: &str) -> Option<String> {
    // Match field: "value" tolerating whitespace/commas. Uses a lazy match
    // so multi-line arg objects don't leak across sibling fields.
    let pat = format!(r#"\b{}\s*:\s*"([^"\n]*)""#, regex::escape(field));
    let re = Regex::new(&pat).ok()?;
    re.captures(body)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn count_lines_up_to(src: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(src.len());
    src[..clamped].bytes().filter(|&b| b == b'\n').count() + 1
}

fn scan_source_file(path: &Path, root: &Path) -> Vec<RegisteredElement> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    // Match `useUIElement({...})` tolerating multi-line objects. The
    // `(?s)` flag makes `.` match newlines; we rely on `?` for laziness
    // to stop at the first closing `}`.
    let call_re = match Regex::new(r"(?s)useUIElement\s*\(\s*(\{.*?\})\s*\)") {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut out = Vec::new();
    for cap in call_re.captures_iter(&src) {
        let Some(arg_match) = cap.get(1) else {
            continue;
        };
        let body = arg_match.as_str();
        let Some(id) = extract_field_value(body, "id") else {
            continue;
        };
        let label = extract_field_value(body, "label");
        let elem_type = extract_field_value(body, "type").unwrap_or_else(|| "button".to_string());
        let line = count_lines_up_to(&src, arg_match.start());
        let suggested = serde_json::json!({
            "id": format!("assert-{}", id),
            "description": format!("{} is registered", label.clone().unwrap_or_else(|| id.clone())),
            "severity": "info",
            "category": "element-exists",
            "target": { "type": "elementId", "elementId": id, "label": label },
            "assertionType": "visible",
            "source": "manual",
            "reviewed": false,
            "enabled": true
        });
        out.push(RegisteredElement {
            id,
            label,
            file: rel.clone(),
            line,
            suggested_assertion: serde_json::to_string_pretty(&suggested)
                .unwrap_or_else(|_| "{}".to_string()),
        });
    }
    out
}

fn scan_source_tree(root: &Path) -> (Vec<RegisteredElement>, usize) {
    let mut results: Vec<RegisteredElement> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (id, file)
    let mut files_scanned = 0;
    let src_dir = root.join("src");
    for entry in WalkDir::new(&src_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(x) if x == "tsx" || x == "ts" => x,
            _ => continue,
        };
        // Skip generated / vendor / test files
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        if rel.contains("node_modules/")
            || rel.contains("/dist/")
            || rel.ends_with(".d.ts")
            || rel.ends_with(".test.ts")
            || rel.ends_with(".test.tsx")
        {
            continue;
        }
        files_scanned += 1;
        let _ = ext;
        for elem in scan_source_file(path, root) {
            let key = (elem.id.clone(), elem.file.clone());
            if seen.insert(key) {
                results.push(elem);
            }
        }
    }
    (results, files_scanned)
}

#[derive(Debug, Default)]
struct SpecAssertedIndex {
    /// elementId -> (specId, assertionId, label) for `target.type: "elementId"`
    by_id: HashMap<String, (String, String, Option<String>)>,
    /// lowercased text -> (specId, assertionId) for `target.type: "search"`
    /// entries whose criteria contains textContent/text/ariaLabel. Used as a
    /// secondary match when code-side labels line up with text-based assertions
    /// (most specs in this project use text search, not elementId).
    by_text: HashMap<String, (String, String)>,
}

fn collect_assertion_text(target: &serde_json::Value) -> Vec<String> {
    let Some(criteria) = target.get("criteria") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for field in ["textContent", "text", "ariaLabel"] {
        if let Some(s) = criteria.get(field).and_then(|v| v.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_lowercase());
            }
        }
    }
    // target.label itself also often carries the human-readable name
    if let Some(s) = target.get("label").and_then(|v| v.as_str()) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_lowercase());
        }
    }
    out
}

async fn scan_specs(pg: &PgDb, app_id: &str) -> (SpecAssertedIndex, usize) {
    let mut idx = SpecAssertedIndex::default();
    let mut files = 0;
    let specs_root = match storage::resolve_specs_root(pg, app_id).await {
        Ok(p) => p,
        Err(e) => {
            warn!("spec_drift: resolve_specs_root({}) failed: {:?}", app_id, e);
            return (idx, 0);
        }
    };
    let page_ids = match storage::list_pages(&specs_root, app_id) {
        Ok(ids) => ids,
        Err(e) => {
            warn!("spec_drift: list_pages failed: {}", e);
            return (idx, 0);
        }
    };
    for page_id in page_ids {
        let root_v = match storage::read_projection(&specs_root, app_id, &page_id) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => {
                warn!(
                    "spec_drift: failed to read projection for {}: {}",
                    page_id, e
                );
                continue;
            }
        };
        files += 1;
        let spec_id = page_id.clone();
        let Some(groups) = root_v.get("groups").and_then(|g| g.as_array()) else {
            continue;
        };
        for group in groups {
            let Some(assertions) = group.get("assertions").and_then(|a| a.as_array()) else {
                continue;
            };
            for a in assertions {
                let assertion_id = a
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let target = a.get("target").cloned().unwrap_or(serde_json::Value::Null);
                let t_type = target.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if t_type == "elementId" {
                    if let Some(eid) = target.get("elementId").and_then(|v| v.as_str()) {
                        let label = target
                            .get("label")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        idx.by_id
                            .entry(eid.to_string())
                            .or_insert_with(|| (spec_id.clone(), assertion_id.clone(), label));
                    }
                }
                // Regardless of target.type, collect any searchable text so we
                // can match code-side labels even when specs don't use elementId.
                for text in collect_assertion_text(&target) {
                    idx.by_text
                        .entry(text)
                        .or_insert_with(|| (spec_id.clone(), assertion_id.clone()));
                }
            }
        }
    }
    (idx, files)
}

/// Scan the project and produce a drift report.
///
/// Arguments:
/// - `app_id`: registered app whose specs to compare against the source tree.
///   Spec-multi-app Stream C: every spec read is scoped to a registered app.
/// - `project_root`: absolute path to the project root (the directory
///   containing `src/` and `src-tauri/`).
#[tauri::command]
pub async fn scan_spec_drift(
    app_id: String,
    project_root: String,
) -> Result<DriftReport, String> {
    let root = PathBuf::from(&project_root);
    if !root.is_dir() {
        return Err(format!("project_root does not exist: {}", project_root));
    }
    info!(
        "scan_spec_drift: app_id={} scanning {}",
        app_id, project_root
    );
    let (elements, source_files) = scan_source_tree(&root);
    let pg = crate::database::pg::PgDb::global();
    let (asserted, spec_files) = scan_specs(&pg, &app_id).await;
    debug!(
        "scan_spec_drift: {} elements, {} asserted ids, {} source files, {} spec files",
        elements.len(),
        asserted.by_id.len(),
        source_files,
        spec_files
    );

    // Missing: registered in code, not covered by any assertion. Coverage is:
    //   - assertion.target.type == "elementId" with matching elementId, OR
    //   - any assertion whose collected text (textContent / text / ariaLabel /
    //     target.label) matches the registered element's label (case-insensitive).
    let mut missing_from_spec: Vec<RegisteredElement> = Vec::new();
    let mut registered_ids: HashSet<String> = HashSet::new();
    for elem in &elements {
        registered_ids.insert(elem.id.clone());
        let by_id_hit = asserted.by_id.contains_key(&elem.id);
        let by_text_hit = elem
            .label
            .as_ref()
            .map(|l| asserted.by_text.contains_key(&l.trim().to_lowercase()))
            .unwrap_or(false);
        if !by_id_hit && !by_text_hit {
            missing_from_spec.push(RegisteredElement {
                id: elem.id.clone(),
                label: elem.label.clone(),
                file: elem.file.clone(),
                line: elem.line,
                suggested_assertion: elem.suggested_assertion.clone(),
            });
        }
    }

    // Orphans: asserted id no longer in any source file.
    let mut orphans_in_spec: Vec<SpecOrphan> = Vec::new();
    for (eid, (spec_id, assertion_id, label)) in &asserted.by_id {
        if !registered_ids.contains(eid) {
            orphans_in_spec.push(SpecOrphan {
                element_id: eid.clone(),
                spec_id: spec_id.clone(),
                assertion_id: assertion_id.clone(),
                label: label.clone(),
            });
        }
    }

    // Stable ordering for UI.
    missing_from_spec.sort_by(|a, b| a.file.cmp(&b.file).then(a.id.cmp(&b.id)));
    orphans_in_spec.sort_by(|a, b| {
        a.spec_id
            .cmp(&b.spec_id)
            .then(a.element_id.cmp(&b.element_id))
    });

    let report = DriftReport {
        app_id: app_id.clone(),
        scanned_at: chrono::Utc::now().to_rfc3339(),
        source_files_scanned: source_files,
        spec_files_scanned: spec_files,
        total_registered_elements: elements.len(),
        total_asserted_ids: asserted.by_id.len(),
        missing_from_spec,
        orphans_in_spec,
    };
    info!(
        "scan_spec_drift: {} missing, {} orphans",
        report.missing_from_spec.len(),
        report.orphans_in_spec.len()
    );
    Ok(report)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_spec_drift")
        .invoke_handler(tauri::generate_handler![scan_spec_drift,])
        .build()
}
