//! Parallel Comparison Run Coordinator
//!
//! Launches N copies of the same workflow in isolated worktrees,
//! waits for all to complete, then triggers AI comparison analysis.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

// =============================================================================
// Types
// =============================================================================

/// Configuration for a comparison run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonConfig {
    /// Workflow ID to run.
    pub workflow_id: String,
    /// Number of parallel runs.
    pub run_count: usize,
    /// What varies between runs.
    pub variation: ComparisonVariation,
    /// Maximum time to wait for all runs (seconds).
    pub timeout_seconds: u64,
}

/// What differs between comparison runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComparisonVariation {
    /// Identical config — tests implementation variance / non-determinism.
    Same,
    /// One run with multi_agent_mode on, one off.
    MultiAgent,
    /// Different AI models for each run.
    Model { models: Vec<String> },
    /// Different context token limits.
    ContextTokens { limits: Vec<usize> },
    /// Custom per-run overrides.
    Custom { overrides: Vec<serde_json::Value> },
}

/// Tracks the state of a comparison run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonRun {
    /// Unique comparison ID.
    pub id: String,
    /// Source workflow ID.
    pub workflow_id: String,
    /// Workflow name.
    pub workflow_name: String,
    /// Git branch all runs started from.
    pub source_branch: String,
    /// Git commit all runs started from.
    pub source_commit: String,
    /// Individual run entries.
    pub entries: Vec<ComparisonEntry>,
    /// Overall status.
    pub status: ComparisonStatus,
    /// AI comparison report (populated after all runs complete).
    pub comparison_report: Option<String>,
    /// AI recommendation.
    pub recommendation: Option<ComparisonRecommendation>,
    /// Timestamps.
    pub created_at: String,
    pub updated_at: String,
    /// Meta-optimizer recommendation ID (if created from comparison bridge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation_id: Option<String>,
    /// How this comparison was triggered: "manual", "meta_optimizer".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One run within a comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntry {
    /// Task run ID for this entry.
    pub task_run_id: String,
    /// Branch name in the worktree.
    pub branch_name: String,
    /// Worktree path.
    pub worktree_path: String,
    /// What config overrides were applied for this entry.
    pub config_overrides: serde_json::Value,
    /// Run status.
    pub status: ComparisonEntryStatus,
    /// Results (populated after run completes).
    pub result: Option<ComparisonEntryResult>,
}

/// Status of a comparison entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonEntryStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Status of the overall comparison.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    /// Runs are in progress.
    Running,
    /// All runs done, AI comparison in progress.
    Comparing,
    /// Comparison complete with report.
    Completed,
    /// Something went wrong.
    Failed,
}

/// Results from a single comparison entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonEntryResult {
    pub success: bool,
    pub verification_passed: bool,
    pub iterations: u32,
    pub duration_ms: u64,
    pub files_changed: usize,
}

/// AI recommendation from comparison analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonRecommendation {
    pub branch_name: String,
    pub confidence: f64,
    pub reasoning: String,
}

// =============================================================================
// Structured comparison report (machine-readable metrics alongside prose)
// =============================================================================

/// Structured comparison output with statistical analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredComparisonReport {
    /// Per-metric deltas across all entries.
    pub metric_deltas: Vec<MetricDelta>,
    /// Cost per entry (label, cost_usd).
    pub cost_breakdown: Vec<(String, f64)>,
    /// Winning variant (if any).
    pub winner: Option<String>,
    /// Confidence in the winner (0.0–1.0).
    pub confidence: f64,
    /// P-value for success rate difference (if ≥2 entries with ≥2 runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
}

/// A single metric compared across variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDelta {
    /// Metric name (e.g., "success_rate", "iterations", "duration_ms").
    pub name: String,
    /// (variant_label, value) pairs.
    pub values: Vec<(String, f64)>,
    /// Best variant for this metric.
    pub best: String,
    /// Delta between best and worst as a percentage.
    pub delta_pct: f64,
}

/// Build a structured comparison report from completed entries.
pub fn build_structured_report(entries: &[ComparisonEntry]) -> Option<StructuredComparisonReport> {
    let completed: Vec<&ComparisonEntry> = entries.iter().filter(|e| e.result.is_some()).collect();

    if completed.len() < 2 {
        return None;
    }

    let labels: Vec<String> = completed.iter().map(|e| e.branch_name.clone()).collect();
    let results: Vec<&ComparisonEntryResult> = completed
        .iter()
        .map(|e| e.result.as_ref().unwrap())
        .collect();

    // Success rate metric
    let success_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), if r.success { 1.0 } else { 0.0 }))
        .collect();
    let best_success = success_values
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Iterations metric (lower is better)
    let iter_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.iterations as f64))
        .collect();
    let best_iter = iter_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Duration metric (lower is better)
    let dur_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.duration_ms as f64))
        .collect();
    let best_dur = dur_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    // Files changed metric
    let files_values: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.files_changed as f64))
        .collect();
    let best_files = files_values
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(l, _)| l.clone())
        .unwrap_or_default();

    let metric_deltas = vec![
        build_metric_delta("success_rate", success_values, &best_success),
        build_metric_delta("iterations", iter_values, &best_iter),
        build_metric_delta("duration_ms", dur_values, &best_dur),
        build_metric_delta("files_changed", files_values, &best_files),
    ];

    // Winner: variant with most "best" wins, tie-broken by success
    let mut win_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for md in &metric_deltas {
        *win_counts.entry(&md.best).or_default() += 1;
    }
    let winner = win_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(label, _)| label.to_string());

    // Confidence: proportion of metrics won by the winner
    let winner_wins = metric_deltas
        .iter()
        .filter(|md| winner.as_deref() == Some(&md.best))
        .count();
    let confidence = winner_wins as f64 / metric_deltas.len() as f64;

    // Cost breakdown (use duration as proxy since we don't have direct cost per entry)
    let cost_breakdown: Vec<(String, f64)> = labels
        .iter()
        .zip(results.iter())
        .map(|(l, r)| (l.clone(), r.duration_ms as f64 / 1000.0)) // duration as cost proxy
        .collect();

    Some(StructuredComparisonReport {
        metric_deltas,
        cost_breakdown,
        winner,
        confidence,
        p_value: None, // Requires multiple trials per variant
    })
}

fn build_metric_delta(name: &str, values: Vec<(String, f64)>, best: &str) -> MetricDelta {
    let max_val = values
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_val = values.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
    let delta_pct = if min_val.abs() > f64::EPSILON {
        ((max_val - min_val) / min_val.abs()) * 100.0
    } else if max_val.abs() > f64::EPSILON {
        100.0
    } else {
        0.0
    };

    MetricDelta {
        name: name.to_string(),
        values,
        best: best.to_string(),
        delta_pct,
    }
}

// =============================================================================
// Coordinator
// =============================================================================

/// Build config overrides for each run based on the variation type.
pub fn build_run_overrides(
    variation: &ComparisonVariation,
    run_count: usize,
) -> Vec<serde_json::Value> {
    match variation {
        ComparisonVariation::Same => (0..run_count).map(|_| serde_json::json!({})).collect(),
        ComparisonVariation::MultiAgent => {
            vec![
                serde_json::json!({"multi_agent_mode": true, "label": "multi-agent"}),
                serde_json::json!({"multi_agent_mode": false, "label": "monolithic"}),
            ]
        }
        ComparisonVariation::Model { models } => models
            .iter()
            .map(|m| serde_json::json!({"model": m, "label": m}))
            .collect(),
        ComparisonVariation::ContextTokens { limits } => limits
            .iter()
            .map(
                |l| serde_json::json!({"max_context_tokens": l, "label": format!("{}K", l / 1000)}),
            )
            .collect(),
        ComparisonVariation::Custom { overrides } => overrides.clone(),
    }
}

/// Check if all entries in a comparison are done (completed or failed).
pub fn all_entries_done(entries: &[ComparisonEntry]) -> bool {
    entries.iter().all(|e| {
        e.status == ComparisonEntryStatus::Completed || e.status == ComparisonEntryStatus::Failed
    })
}

/// Build a comparison summary for the AI comparison prompt.
pub fn build_entry_summaries(entries: &[ComparisonEntry]) -> Vec<(String, String, String)> {
    Vec::new()
}

// =============================================================================
// Computed treatment axes
// =============================================================================
//
// `ComparisonVariation` *declares* what varies between the arms of a comparison
// run. Nothing checks that declaration against reality. The functions below
// compute what actually differs, from the raw per-arm override blobs — the live
// per-arm config is `ComparisonEntryJson.overrides` (a `serde_json::Value`
// persisted in the `entries_json` column of `project.comparison_runs`), so these
// take `&[serde_json::Value]` and stay callable from the HTTP path, the Tauri
// path, and a historical-row probe alike.

/// Path reported for an arm whose override blob is not an object or array
/// (a bare string, number, bool or null at the root).
pub const ROOT_AXIS_PATH: &str = "<root>";

/// Override paths that are per-arm distinct *by construction* and are therefore
/// never a real treatment axis.
///
/// `label` is the only such key today: the HTTP path reads an arm's own `label`
/// from inside its override object and carries it into the stored blob, and
/// [`build_run_overrides`] writes `label` into the override JSON on the
/// `MultiAgent`, `Model` and `ContextTokens` arms. Without ignoring it every
/// custom-arm comparison would report a spurious extra axis.
///
/// Note that `use_worktree` is deliberately *not* here: the HTTP path injects
/// the same value into every arm, so it is constant across arms and can never
/// register as differing.
pub const DEFAULT_IGNORED_AXIS_PATHS: &[&str] = &["label"];

/// Compute the set of JSON key paths that actually differ across the arms of a
/// comparison, ignoring [`DEFAULT_IGNORED_AXIS_PATHS`].
///
/// Returns a sorted, de-duplicated list of dotted paths (`a.b.c` for nested
/// objects, `a[0]` for array elements). A key present in some arms and absent
/// in others counts as differing. Fewer than two arms can never differ, so the
/// result is empty.
pub fn computed_treatment_axes(arm_overrides: &[serde_json::Value]) -> Vec<String> {
    computed_treatment_axes_with_ignored(arm_overrides, DEFAULT_IGNORED_AXIS_PATHS)
}

/// Same as [`computed_treatment_axes`], but with an explicit ignore-list.
///
/// An entry in `ignored_paths` suppresses the path itself and everything
/// beneath it: `"label"` ignores `label`, `label.name` and `label[0]`, but not
/// `labelling`.
pub fn computed_treatment_axes_with_ignored(
    arm_overrides: &[serde_json::Value],
    ignored_paths: &[&str],
) -> Vec<String> {
    if arm_overrides.len() < 2 {
        return Vec::new();
    }

    let flattened: Vec<BTreeMap<String, &serde_json::Value>> = arm_overrides
        .iter()
        .map(|arm| {
            let mut leaves = BTreeMap::new();
            flatten_json_leaves("", arm, &mut leaves);
            leaves
        })
        .collect();

    let all_paths: BTreeSet<&String> = flattened.iter().flat_map(|arm| arm.keys()).collect();

    let mut axes: Vec<String> = all_paths
        .into_iter()
        .filter(|path| !is_ignored_axis_path(path.as_str(), ignored_paths))
        .filter(|path| {
            let first = flattened[0].get(*path);
            flattened.iter().any(|arm| arm.get(*path) != first)
        })
        .cloned()
        .collect();

    axes.sort();
    axes
}

/// True when `path` is the ignored path itself or nested beneath it.
fn is_ignored_axis_path(path: &str, ignored_paths: &[&str]) -> bool {
    ignored_paths.iter().any(|ignored| {
        path == *ignored
            || (path.len() > ignored.len()
                && path.starts_with(ignored)
                && matches!(path.as_bytes()[ignored.len()], b'.' | b'['))
    })
}

/// Flatten a JSON value into leaf paths.
///
/// Objects recurse as `prefix.key`, arrays as `prefix[index]`. Scalars, empty
/// objects and empty arrays are leaves. A scalar at the root (no prefix) is
/// reported under [`ROOT_AXIS_PATH`]; an empty object or array at the root
/// contributes no leaf at all, so `{}` compares equal to `{}` and differs from
/// `{"model": "x"}` only by the `model` path.
fn flatten_json_leaves<'a>(
    prefix: &str,
    value: &'a serde_json::Value,
    out: &mut BTreeMap<String, &'a serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                let child_prefix = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };
                flatten_json_leaves(&child_prefix, child, out);
            }
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            for (index, child) in items.iter().enumerate() {
                let child_prefix = format!("{}[{}]", prefix, index);
                flatten_json_leaves(&child_prefix, child, out);
            }
        }
        // Empty container: a leaf only when it is nested under a real key.
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), value);
            }
        }
        _ => {
            let path = if prefix.is_empty() {
                ROOT_AXIS_PATH.to_string()
            } else {
                prefix.to_string()
            };
            out.insert(path, value);
        }
    }
}

// =============================================================================
// Phase 2a — declared vs actual: classify the drift
// =============================================================================
//
// `computed_treatment_axes` says what actually differs. `variation_type` says
// what the author CLAIMED would differ. This section compares the two and names
// the discrepancy.
//
// The vocabulary is coord's `CanonicalDriftClass` (shipped by
// `2026-05-30-twin-declared-vs-actual-generalization`), reproduced here as a
// runner-local enum over the same seven wire tokens rather than imported.
//
// That is deliberate, and it follows two existing precedents rather than
// inventing a third convention:
//
//   * `CanonicalDriftClass` lives in the `qontinui-coord` PACKAGE, and the
//     dependency runs coord -> qontinui-schemas, never the reverse. The runner
//     has no dependency on coord and should not gain one — coord is the server.
//   * `qontinui-schemas`'s own `completeness_verdict.rs` hit this exact wall and
//     documented the answer as "deliberate field-compatibility, NOT reuse".
//   * The runner ALREADY speaks this vocabulary the same way:
//     `orchestration_loop::coord_gate::DriftClass` parses coord's wire token and
//     keeps the raw string, without importing coord's type.
//
// The tokens are what travel, so a local enum over identical tokens costs
// nothing and keeps the fleet's classification language consistent.

/// The declared-vs-actual classification of a comparison run's treatment axis.
///
/// Wire-token-compatible with `qontinui-coord`'s `CanonicalDriftClass`; see the
/// section comment above for why this is a local type rather than an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisDriftClass {
    /// Declared and computed agree. The run tested what it claimed to test.
    None,
    /// The computed set strictly contains the declared one — **multi-axis**.
    /// The run moved more than it claimed, so it cannot claim a clean
    /// single-axis comparison.
    BenignAdd,
    /// A declared axis is absent from the computed set: declared is ahead of
    /// actual. Absence, not negation.
    Pending,
    /// Declared `same`, but the arms actually differ.
    InPlace,
    /// The computed set is empty for a declaration that promised a treatment —
    /// a comparison that compared nothing. The apply-block signal.
    ActiveNegation,
    /// The declared side is itself inconsistent: a "comparison" with fewer than
    /// two arms cannot be one, whatever it declares.
    Divergent,
    /// Could not be determined — an unrecognized `variation_type`, or arms that
    /// could not be parsed. A coverage gap, **not** an assertion of agreement.
    Unknown,
}

impl AxisDriftClass {
    /// The lowercase wire token, identical to coord's `CanonicalDriftClass`.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            AxisDriftClass::None => "none",
            AxisDriftClass::BenignAdd => "benign_add",
            AxisDriftClass::Pending => "pending",
            AxisDriftClass::InPlace => "in_place",
            AxisDriftClass::ActiveNegation => "active_negation",
            AxisDriftClass::Divergent => "divergent",
            AxisDriftClass::Unknown => "unknown",
        }
    }

    /// Parse a stored/wire token. An unrecognized or empty token maps to
    /// [`AxisDriftClass::Unknown`] — the coverage-gap class — so a row carrying
    /// a value this build does not know is reported honestly rather than
    /// silently read as agreement. Same rule as coord's `from_wire_str`.
    pub fn from_wire_str(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "none" => AxisDriftClass::None,
            "benign_add" => AxisDriftClass::BenignAdd,
            "pending" => AxisDriftClass::Pending,
            "in_place" => AxisDriftClass::InPlace,
            "active_negation" => AxisDriftClass::ActiveNegation,
            "divergent" => AxisDriftClass::Divergent,
            _ => AxisDriftClass::Unknown,
        }
    }

    /// True only for [`AxisDriftClass::None`].
    ///
    /// Note what this deliberately does NOT do: `Unknown` is not clean. "We
    /// could not tell" must never be consumed as "the run was fine" — that
    /// substitution is the whole defect this plan closes.
    pub fn is_clean(self) -> bool {
        matches!(self, AxisDriftClass::None)
    }
}

/// What a declared `variation_type` claims should differ between the arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredAxes {
    /// The declaration names an exact expected key-path set. `same` is
    /// `Exact(empty)` — it claims that *nothing* differs.
    Exact(BTreeSet<String>),
    /// `custom`: the author supplied arbitrary overrides and declared no
    /// particular axis, so there is no exact set to compare against.
    Unconstrained,
    /// A `variation_type` this build does not know.
    Unrecognized,
}

/// Map a declared `variation_type` string to the key paths it promises.
///
/// The tokens are the union of the three grammars in the tree: the HTTP surface
/// and the Tauri command speak `architecture` / `same` / `custom`, while
/// [`ComparisonVariation`] additionally names multi-agent, model and
/// context-token variation. The expected paths are the keys the corresponding
/// arm builders actually write — see [`build_run_overrides`] and
/// `mcp::comparison_api`'s arm construction.
pub fn declared_axes(variation_type: &str) -> DeclaredAxes {
    let exact = |paths: &[&str]| {
        DeclaredAxes::Exact(
            paths
                .iter()
                .map(|p| (*p).to_string())
                .collect::<BTreeSet<_>>(),
        )
    };
    match variation_type.trim().to_ascii_lowercase().as_str() {
        "same" => DeclaredAxes::Exact(BTreeSet::new()),
        "architecture" => exact(&["workflow_architecture"]),
        "multi_agent" => exact(&["multi_agent_mode"]),
        "model" => exact(&["model"]),
        "context_tokens" => exact(&["max_context_tokens"]),
        "custom" => DeclaredAxes::Unconstrained,
        _ => DeclaredAxes::Unrecognized,
    }
}

/// Classify a comparison run's declared `variation_type` against the axes its
/// arms actually moved.
///
/// `arm_overrides` are the per-arm override blobs — the same input
/// [`computed_treatment_axes`] takes.
pub fn classify_axis_drift(
    variation_type: &str,
    arm_overrides: &[serde_json::Value],
) -> AxisDriftClass {
    // A comparison needs two arms to be a comparison at all. Whatever it
    // declared, the declared side is internally inconsistent.
    if arm_overrides.len() < 2 {
        return AxisDriftClass::Divergent;
    }

    let computed: BTreeSet<String> = computed_treatment_axes(arm_overrides).into_iter().collect();

    match declared_axes(variation_type) {
        DeclaredAxes::Unrecognized => AxisDriftClass::Unknown,

        // `same`: the claim is that nothing differs.
        DeclaredAxes::Exact(declared) if declared.is_empty() => {
            if computed.is_empty() {
                AxisDriftClass::None
            } else {
                AxisDriftClass::InPlace
            }
        }

        DeclaredAxes::Exact(declared) => {
            if computed.is_empty() {
                // Promised a treatment; delivered none.
                AxisDriftClass::ActiveNegation
            } else if !declared.is_subset(&computed) {
                // A declared axis never moved.
                AxisDriftClass::Pending
            } else if computed.len() > declared.len() {
                // Everything declared moved, and then some.
                AxisDriftClass::BenignAdd
            } else {
                AxisDriftClass::None
            }
        }

        // `custom` declares no specific axis, so only the degenerate cases are
        // decidable: nothing moved at all, or more than one thing moved (which
        // still forfeits any claim to a clean single-axis comparison).
        DeclaredAxes::Unconstrained => {
            if computed.is_empty() {
                AxisDriftClass::ActiveNegation
            } else if computed.len() > 1 {
                AxisDriftClass::BenignAdd
            } else {
                AxisDriftClass::None
            }
        }
    }
}

// =============================================================================
// Phase 3 — a non-clean axis must not underwrite an autonomous rollout
// =============================================================================

/// The confidence at or above which `meta_optimizer::parser::auto_apply_high_confidence`
/// sweeps a pending `config_change` recommendation into an automatic canary
/// rollout with **no further human step** (`meta_optimizer/parser.rs:1114`,
/// which then calls `start_canary(pg_db, rec_id, 10)` at `:1122`).
///
/// This constant is the reason Phase 3 exists. A comparison whose declared axis
/// does not match what actually moved must not produce a recommendation that
/// clears this bar, because everything past it is autonomous: the canary is
/// auto-promoted or rolled back by `trigger.rs`, on a sweep that re-fires on
/// every workflow run.
pub const AUTO_CANARY_CONFIDENCE_THRESHOLD: f64 = 0.75;

/// The confidence a non-clean comparison is clamped to.
///
/// Deliberately below [`AUTO_CANARY_CONFIDENCE_THRESHOLD`] rather than zero: the
/// comparison still produced real data and a human may still want to act on it.
/// What it loses is the right to act *without* a human.
pub const NON_CLEAN_AXIS_CONFIDENCE_CEILING: f64 = 0.5;

/// Adjust a comparison-derived recommendation's confidence for the trust its
/// treatment axis actually earns.
///
/// Returns the confidence to record, plus `Some(reason)` when it was reduced —
/// the reason is meant to be written into the recommendation's description so
/// the downgrade is a recorded fact rather than a silent number change.
///
/// A clean axis passes through untouched. Every other class — including
/// [`AxisDriftClass::Unknown`] — is capped below the autonomous-canary
/// threshold. Capping `Unknown` is the point: "we could not determine what this
/// run tested" is not a licence to promote it.
pub fn axis_adjusted_confidence(
    declared_confidence: f64,
    class: AxisDriftClass,
) -> (f64, Option<String>) {
    if class.is_clean() {
        return (declared_confidence, None);
    }
    if declared_confidence <= NON_CLEAN_AXIS_CONFIDENCE_CEILING {
        // Already below the bar — nothing to clamp, but the class is still
        // worth recording, so callers get a reason either way.
        return (
            declared_confidence,
            Some(format!(
                "comparison treatment axis is `{}` (not `none`); confidence left at {:.2}, \
                 already below the {:.2} autonomous-canary threshold",
                class.as_wire_str(),
                declared_confidence,
                AUTO_CANARY_CONFIDENCE_THRESHOLD
            )),
        );
    }
    (
        NON_CLEAN_AXIS_CONFIDENCE_CEILING,
        Some(format!(
            "comparison treatment axis is `{}` (not `none`): the arms did not vary the way the \
             run declared, so this result cannot underwrite an autonomous rollout. Confidence \
             reduced {:.2} -> {:.2}, below the {:.2} autonomous-canary threshold. A human may \
             still apply it deliberately.",
            class.as_wire_str(),
            declared_confidence,
            NON_CLEAN_AXIS_CONFIDENCE_CEILING,
            AUTO_CANARY_CONFIDENCE_THRESHOLD
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn axes(arms: Vec<serde_json::Value>) -> Vec<String> {
        computed_treatment_axes(&arms)
    }

    #[test]
    fn computed_treatment_axes_table() {
        let cases: Vec<(&str, Vec<serde_json::Value>, Vec<&str>)> = vec![
            ("zero arms", vec![], vec![]),
            ("single arm", vec![json!({"model": "opus"})], vec![]),
            (
                "identical arms",
                vec![
                    json!({"model": "opus", "max_context_tokens": 100_000}),
                    json!({"model": "opus", "max_context_tokens": 100_000}),
                ],
                vec![],
            ),
            (
                "one differing key",
                vec![
                    json!({"model": "opus", "max_context_tokens": 100_000}),
                    json!({"model": "sonnet", "max_context_tokens": 100_000}),
                ],
                vec!["model"],
            ),
            (
                "two differing keys are sorted",
                vec![
                    json!({"model": "opus", "max_context_tokens": 100_000}),
                    json!({"model": "sonnet", "max_context_tokens": 200_000}),
                ],
                vec!["max_context_tokens", "model"],
            ),
            (
                "key present in one arm and absent in another",
                vec![json!({"model": "opus"}), json!({})],
                vec!["model"],
            ),
            (
                "nested object difference uses a dotted path",
                vec![
                    json!({"runner": {"limits": {"max_iterations": 3}}}),
                    json!({"runner": {"limits": {"max_iterations": 5}}}),
                ],
                vec!["runner.limits.max_iterations"],
            ),
            (
                "array element difference uses an indexed path",
                vec![
                    json!({"tools": ["read", "write"]}),
                    json!({"tools": ["read", "bash"]}),
                ],
                vec!["tools[1]"],
            ),
            (
                "label differing alone is not an axis",
                vec![
                    json!({"label": "multi-agent", "model": "opus"}),
                    json!({"label": "monolithic", "model": "opus"}),
                ],
                vec![],
            ),
            (
                "label differing alongside a real key reports only the real key",
                vec![
                    json!({"label": "multi-agent", "multi_agent_mode": true}),
                    json!({"label": "monolithic", "multi_agent_mode": false}),
                ],
                vec!["multi_agent_mode"],
            ),
            (
                "ignore-list matches nested paths under label",
                vec![
                    json!({"label": {"text": "one"}, "model": "opus"}),
                    json!({"label": {"text": "two"}, "model": "opus"}),
                ],
                vec![],
            ),
            (
                "a key merely prefixed by an ignored path is still an axis",
                vec![json!({"labelling": "a"}), json!({"labelling": "b"})],
                vec!["labelling"],
            ),
            (
                "use_worktree injected identically into every arm is not an axis",
                vec![
                    json!({"workflow_architecture": "traditional", "use_worktree": true}),
                    json!({"workflow_architecture": "agentic_verification", "use_worktree": true}),
                ],
                vec!["workflow_architecture"],
            ),
            (
                "non-object arms are handled without panicking",
                vec![json!("just-a-string"), json!(null)],
                vec!["<root>"],
            ),
            (
                "identical non-object arms differ nowhere",
                vec![json!("same"), json!("same")],
                vec![],
            ),
            (
                "object arm against a null arm",
                vec![json!({"model": "opus"}), json!(null)],
                vec!["<root>", "model"],
            ),
            (
                "three arms differing only in the third",
                vec![
                    json!({"model": "opus"}),
                    json!({"model": "opus"}),
                    json!({"model": "haiku"}),
                ],
                vec!["model"],
            ),
            (
                "empty objects compare equal",
                vec![json!({}), json!({})],
                vec![],
            ),
        ];

        for (name, arms, expected) in cases {
            let expected: Vec<String> = expected.into_iter().map(|s| s.to_string()).collect();
            assert_eq!(axes(arms), expected, "case: {}", name);
        }
    }

    #[test]
    fn one_differing_key_is_exactly_that_axis() {
        let arms = vec![json!({"model": "opus"}), json!({"model": "sonnet"})];
        assert_eq!(
            computed_treatment_axes(&arms),
            vec!["model".to_string()],
            "a single differing key must report exactly one axis"
        );
    }

    #[test]
    fn label_only_difference_is_empty() {
        let arms = vec![
            json!({"label": "100K", "max_context_tokens": 100_000}),
            json!({"label": "100K-again", "max_context_tokens": 100_000}),
        ];
        let empty: Vec<String> = Vec::new();
        assert_eq!(computed_treatment_axes(&arms), empty);
    }

    #[test]
    fn explicit_ignore_list_is_honoured() {
        let arms = vec![
            json!({"label": "a", "model": "opus"}),
            json!({"label": "b", "model": "sonnet"}),
        ];

        // No ignore-list: both keys are axes.
        assert_eq!(
            computed_treatment_axes_with_ignored(&arms, &[]),
            vec!["label".to_string(), "model".to_string()]
        );

        // Ignoring the real key leaves only the label.
        assert_eq!(
            computed_treatment_axes_with_ignored(&arms, &["model"]),
            vec!["label".to_string()]
        );

        // Ignoring both leaves nothing.
        let empty: Vec<String> = Vec::new();
        assert_eq!(
            computed_treatment_axes_with_ignored(&arms, &["label", "model"]),
            empty
        );
    }

    #[test]
    fn default_ignore_list_is_label_only() {
        assert_eq!(DEFAULT_IGNORED_AXIS_PATHS, &["label"]);
    }

    #[test]
    fn build_run_overrides_arms_report_only_their_real_axis() {
        // The declared variations must compute back to the axis they claim,
        // with the by-construction `label` suppressed.
        let multi_agent = build_run_overrides(&ComparisonVariation::MultiAgent, 2);
        assert_eq!(
            computed_treatment_axes(&multi_agent),
            vec!["multi_agent_mode".to_string()]
        );

        let model = build_run_overrides(
            &ComparisonVariation::Model {
                models: vec!["opus".to_string(), "sonnet".to_string()],
            },
            2,
        );
        assert_eq!(computed_treatment_axes(&model), vec!["model".to_string()]);

        let tokens = build_run_overrides(
            &ComparisonVariation::ContextTokens {
                limits: vec![100_000, 200_000],
            },
            2,
        );
        assert_eq!(
            computed_treatment_axes(&tokens),
            vec!["max_context_tokens".to_string()]
        );

        let same = build_run_overrides(&ComparisonVariation::Same, 3);
        let empty: Vec<String> = Vec::new();
        assert_eq!(computed_treatment_axes(&same), empty);
    }

    // ---------------------------------------------------------------------
    // Phase 2a — classification
    // ---------------------------------------------------------------------

    /// Two arms differing exactly on `path`, plus a distinct per-arm `label`
    /// (which the ignore-list must suppress, exactly as the live paths emit it).
    fn arms_differing_on(path: &str) -> Vec<serde_json::Value> {
        vec![
            json!({"label": "a", path: "one"}),
            json!({"label": "b", path: "two"}),
        ]
    }

    #[test]
    fn axis_drift_class_wire_tokens_are_coord_s() {
        // Literal tokens, asserted against coord's CanonicalDriftClass wire
        // taxonomy -- NOT re-derived from the enum, so a rename breaks this.
        assert_eq!(AxisDriftClass::None.as_wire_str(), "none");
        assert_eq!(AxisDriftClass::BenignAdd.as_wire_str(), "benign_add");
        assert_eq!(AxisDriftClass::Pending.as_wire_str(), "pending");
        assert_eq!(AxisDriftClass::InPlace.as_wire_str(), "in_place");
        assert_eq!(
            AxisDriftClass::ActiveNegation.as_wire_str(),
            "active_negation"
        );
        assert_eq!(AxisDriftClass::Divergent.as_wire_str(), "divergent");
        assert_eq!(AxisDriftClass::Unknown.as_wire_str(), "unknown");
    }

    #[test]
    fn axis_drift_class_round_trips_and_fails_closed() {
        for class in [
            AxisDriftClass::None,
            AxisDriftClass::BenignAdd,
            AxisDriftClass::Pending,
            AxisDriftClass::InPlace,
            AxisDriftClass::ActiveNegation,
            AxisDriftClass::Divergent,
            AxisDriftClass::Unknown,
        ] {
            assert_eq!(AxisDriftClass::from_wire_str(class.as_wire_str()), class);
        }
        // Case and whitespace tolerated.
        assert_eq!(
            AxisDriftClass::from_wire_str("  In_Place "),
            AxisDriftClass::InPlace
        );
        // Anything unrecognized fails CLOSED to Unknown, never to None.
        for token in ["", "   ", "no", "NONE_OF_IT", "clean", "ok"] {
            assert_eq!(
                AxisDriftClass::from_wire_str(token),
                AxisDriftClass::Unknown,
                "token {:?} must fail closed to Unknown",
                token
            );
        }
    }

    #[test]
    fn only_none_is_clean() {
        assert!(AxisDriftClass::None.is_clean());
        for class in [
            AxisDriftClass::BenignAdd,
            AxisDriftClass::Pending,
            AxisDriftClass::InPlace,
            AxisDriftClass::ActiveNegation,
            AxisDriftClass::Divergent,
            AxisDriftClass::Unknown,
        ] {
            assert!(!class.is_clean(), "{:?} must not read as clean", class);
        }
    }

    #[test]
    fn declared_axes_maps_every_known_variation_token() {
        let exact = |v: &str| match declared_axes(v) {
            DeclaredAxes::Exact(set) => set.into_iter().collect::<Vec<_>>(),
            other => panic!("{} should be Exact, got {:?}", v, other),
        };
        assert_eq!(exact("same"), Vec::<String>::new());
        assert_eq!(
            exact("architecture"),
            vec!["workflow_architecture".to_string()]
        );
        assert_eq!(exact("multi_agent"), vec!["multi_agent_mode".to_string()]);
        assert_eq!(exact("model"), vec!["model".to_string()]);
        assert_eq!(
            exact("context_tokens"),
            vec!["max_context_tokens".to_string()]
        );
        assert_eq!(declared_axes("custom"), DeclaredAxes::Unconstrained);
        assert_eq!(declared_axes("ARCHITECTURE"), declared_axes("architecture"));
        assert_eq!(declared_axes("nonsense"), DeclaredAxes::Unrecognized);
        assert_eq!(declared_axes(""), DeclaredAxes::Unrecognized);
    }

    #[test]
    fn classify_axis_drift_table() {
        let cases: Vec<(&str, &str, Vec<serde_json::Value>, AxisDriftClass)> = vec![
            (
                "same, arms identical -> none",
                "same",
                vec![json!({"use_worktree": true}), json!({"use_worktree": true})],
                AxisDriftClass::None,
            ),
            (
                "same, arms actually differ -> in_place",
                "same",
                arms_differing_on("model"),
                AxisDriftClass::InPlace,
            ),
            (
                "same, only the label differs -> none (label is not an axis)",
                "same",
                vec![json!({"label": "Run 1"}), json!({"label": "Run 2"})],
                AxisDriftClass::None,
            ),
            (
                "architecture, the declared axis moved -> none",
                "architecture",
                arms_differing_on("workflow_architecture"),
                AxisDriftClass::None,
            ),
            (
                "architecture, nothing moved -> active_negation",
                "architecture",
                vec![json!({"use_worktree": true}), json!({"use_worktree": true})],
                AxisDriftClass::ActiveNegation,
            ),
            (
                "architecture, a DIFFERENT axis moved -> pending",
                "architecture",
                arms_differing_on("model"),
                AxisDriftClass::Pending,
            ),
            (
                "architecture, declared axis moved AND another -> benign_add (multi-axis)",
                "architecture",
                vec![
                    json!({"workflow_architecture": "traditional", "model": "opus"}),
                    json!({"workflow_architecture": "multi_agent_pipeline", "model": "sonnet"}),
                ],
                AxisDriftClass::BenignAdd,
            ),
            (
                "model, the declared axis moved -> none",
                "model",
                arms_differing_on("model"),
                AxisDriftClass::None,
            ),
            (
                "context_tokens, the declared axis moved -> none",
                "context_tokens",
                arms_differing_on("max_context_tokens"),
                AxisDriftClass::None,
            ),
            (
                "multi_agent, the declared axis moved -> none",
                "multi_agent",
                arms_differing_on("multi_agent_mode"),
                AxisDriftClass::None,
            ),
            (
                "custom, exactly one axis moved -> none",
                "custom",
                arms_differing_on("config_override"),
                AxisDriftClass::None,
            ),
            (
                "custom, nothing moved -> active_negation (compared nothing)",
                "custom",
                vec![json!({"label": "baseline"}), json!({"label": "candidate"})],
                AxisDriftClass::ActiveNegation,
            ),
            (
                "custom, two axes moved -> benign_add (multi-axis)",
                "custom",
                vec![
                    json!({"model": "opus", "max_context_tokens": 100_000}),
                    json!({"model": "sonnet", "max_context_tokens": 200_000}),
                ],
                AxisDriftClass::BenignAdd,
            ),
            (
                "unrecognized variation_type -> unknown",
                "teleportation",
                arms_differing_on("model"),
                AxisDriftClass::Unknown,
            ),
            (
                "single arm -> divergent (not a comparison at all)",
                "model",
                vec![json!({"model": "opus"})],
                AxisDriftClass::Divergent,
            ),
            (
                "zero arms -> divergent",
                "same",
                vec![],
                AxisDriftClass::Divergent,
            ),
        ];

        for (name, variation, arms, expected) in cases {
            assert_eq!(
                classify_axis_drift(variation, &arms),
                expected,
                "case: {}",
                name
            );
        }
    }

    #[test]
    fn classify_axis_drift_over_real_build_run_overrides_arms() {
        // The declared enum's own arm builder must classify clean against the
        // matching declared token -- this pins the classifier to the real
        // construction path, not to hand-written fixtures.
        let model = build_run_overrides(
            &ComparisonVariation::Model {
                models: vec!["opus".to_string(), "sonnet".to_string()],
            },
            2,
        );
        assert_eq!(classify_axis_drift("model", &model), AxisDriftClass::None);

        let tokens = build_run_overrides(
            &ComparisonVariation::ContextTokens {
                limits: vec![100_000, 200_000],
            },
            2,
        );
        assert_eq!(
            classify_axis_drift("context_tokens", &tokens),
            AxisDriftClass::None
        );

        let multi = build_run_overrides(&ComparisonVariation::MultiAgent, 2);
        assert_eq!(
            classify_axis_drift("multi_agent", &multi),
            AxisDriftClass::None
        );

        // ...and the same arms declared as something else are caught.
        assert_eq!(classify_axis_drift("same", &model), AxisDriftClass::InPlace);
        assert_eq!(
            classify_axis_drift("architecture", &model),
            AxisDriftClass::Pending
        );

        // `Same` really does emit identical arms.
        let same = build_run_overrides(&ComparisonVariation::Same, 3);
        assert_eq!(classify_axis_drift("same", &same), AxisDriftClass::None);
    }

    // ---------------------------------------------------------------------
    // Phase 3 — confidence clamping
    // ---------------------------------------------------------------------

    #[test]
    fn auto_canary_threshold_matches_the_sweep_it_guards() {
        // Literal, not re-derived: this is the bar in
        // meta_optimizer/parser.rs:1114 that sweeps a pending config_change
        // into an automatic 10% canary. If that constant moves, this must too.
        assert_eq!(AUTO_CANARY_CONFIDENCE_THRESHOLD, 0.75);
        assert_eq!(NON_CLEAN_AXIS_CONFIDENCE_CEILING, 0.5);
        assert!(NON_CLEAN_AXIS_CONFIDENCE_CEILING < AUTO_CANARY_CONFIDENCE_THRESHOLD);
    }

    #[test]
    fn clean_axis_confidence_passes_through_untouched() {
        for c in [0.0, 0.5, 0.6, 0.75, 0.9, 1.0] {
            let (adjusted, reason) = axis_adjusted_confidence(c, AxisDriftClass::None);
            assert_eq!(adjusted, c, "clean axis must not clamp {}", c);
            assert!(reason.is_none(), "clean axis must not produce a reason");
        }
    }

    #[test]
    fn every_non_clean_class_is_pushed_below_the_autonomous_threshold() {
        for class in [
            AxisDriftClass::BenignAdd,
            AxisDriftClass::Pending,
            AxisDriftClass::InPlace,
            AxisDriftClass::ActiveNegation,
            AxisDriftClass::Divergent,
            AxisDriftClass::Unknown,
        ] {
            // A confidence that WOULD have self-driven.
            let (adjusted, reason) = axis_adjusted_confidence(0.95, class);
            assert_eq!(adjusted, 0.5, "{:?} must clamp to the ceiling", class);
            assert!(
                adjusted < AUTO_CANARY_CONFIDENCE_THRESHOLD,
                "{:?} must land below the autonomous-canary threshold",
                class
            );
            let reason = reason.expect("a clamped confidence must carry a reason");
            assert!(
                reason.contains(class.as_wire_str()),
                "reason must name the class, got: {}",
                reason
            );
        }
    }

    #[test]
    fn exactly_at_the_threshold_is_still_clamped() {
        // 0.75 is swept (the comparison is `>=`), so the boundary must clamp.
        let (adjusted, reason) =
            axis_adjusted_confidence(AUTO_CANARY_CONFIDENCE_THRESHOLD, AxisDriftClass::InPlace);
        assert_eq!(adjusted, 0.5);
        assert!(adjusted < AUTO_CANARY_CONFIDENCE_THRESHOLD);
        assert!(reason.is_some());
    }

    #[test]
    fn an_already_low_confidence_is_not_raised_but_is_still_explained() {
        let (adjusted, reason) = axis_adjusted_confidence(0.2, AxisDriftClass::Unknown);
        assert_eq!(adjusted, 0.2, "clamping must never RAISE a confidence");
        let reason = reason.expect("a non-clean class must always explain itself");
        assert!(reason.contains("unknown"), "got: {}", reason);
    }

    #[test]
    fn unknown_axis_is_not_treated_as_clean() {
        // The whole defect class: "we could not tell" must never be consumed as
        // "it was fine".
        let (adjusted, reason) = axis_adjusted_confidence(0.99, AxisDriftClass::Unknown);
        assert!(adjusted < AUTO_CANARY_CONFIDENCE_THRESHOLD);
        assert!(reason.is_some());
    }
}
