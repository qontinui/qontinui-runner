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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComparisonVariation {
    /// Identical config — tests implementation variance / non-determinism.
    Same,
    /// The three workflow architectures, side by side. This is the default the
    /// HTTP surface has always used; it lives in the enum now rather than only
    /// in two hand-written string matches.
    Architecture,
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

/// One arm of a comparison: a human label plus the config overrides applied to
/// that run.
///
/// The label is a SIBLING of the overrides, never a key inside them. Burying it
/// in the override JSON is what made `label` a spurious treatment axis on the
/// custom path (see [`DEFAULT_IGNORED_AXIS_PATHS`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonArm {
    /// Human-readable arm name.
    pub label: String,
    /// The config overrides applied to this run.
    pub overrides: serde_json::Value,
}

/// Build the arms of a comparison from the declared variation.
///
/// **This is the single derivation path.** The HTTP surface
/// (`mcp::comparison_api`) and the Tauri command (`commands::comparison`) both
/// call it, so `variation_type` means exactly one thing across the tree instead
/// of being re-interpreted by three divergent string matches.
///
/// `use_worktree` is injected identically into every arm — it is a run-wide
/// setting, not a treatment, which is why it can never register as a differing
/// axis.
pub fn build_comparison_arms(
    variation: &ComparisonVariation,
    run_count: usize,
    use_worktree: bool,
) -> Vec<ComparisonArm> {
    let arch = |label: &str, kind: &str| ComparisonArm {
        label: label.to_string(),
        overrides: serde_json::json!({ "workflow_architecture": kind }),
    };

    let mut arms = match variation {
        ComparisonVariation::Architecture => vec![
            arch("Traditional", "traditional"),
            arch("Agentic Verification", "agentic_verification"),
            arch("Multi-Agent Pipeline", "multi_agent_pipeline"),
        ],
        ComparisonVariation::Same => (0..run_count)
            .map(|i| ComparisonArm {
                label: format!("Run {}", i + 1),
                overrides: serde_json::json!({}),
            })
            .collect(),
        ComparisonVariation::MultiAgent => vec![
            ComparisonArm {
                label: "multi-agent".to_string(),
                overrides: serde_json::json!({"multi_agent_mode": true}),
            },
            ComparisonArm {
                label: "monolithic".to_string(),
                overrides: serde_json::json!({"multi_agent_mode": false}),
            },
        ],
        ComparisonVariation::Model { models } => models
            .iter()
            .map(|m| ComparisonArm {
                label: m.clone(),
                overrides: serde_json::json!({ "model": m }),
            })
            .collect(),
        ComparisonVariation::ContextTokens { limits } => limits
            .iter()
            .map(|l| ComparisonArm {
                label: format!("{}K", l / 1000),
                overrides: serde_json::json!({ "max_context_tokens": l }),
            })
            .collect(),
        ComparisonVariation::Custom { overrides } => overrides
            .iter()
            .enumerate()
            .map(|(i, ov)| ComparisonArm {
                label: ov
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Custom {}", i + 1)),
                overrides: ov.clone(),
            })
            .collect(),
    };

    for arm in &mut arms {
        if let Some(obj) = arm.overrides.as_object_mut() {
            obj.insert(
                "use_worktree".to_string(),
                serde_json::Value::Bool(use_worktree),
            );
        }
    }
    arms
}

/// The per-arm override blobs alone, without labels.
///
/// A thin projection of [`build_comparison_arms`] — kept because the axis
/// computation consumes overrides, not arms.
pub fn build_run_overrides(
    variation: &ComparisonVariation,
    run_count: usize,
) -> Vec<serde_json::Value> {
    build_comparison_arms(variation, run_count, true)
        .into_iter()
        .map(|arm| arm.overrides)
        .collect()
}

/// Parse the wire `variation_type` string into the typed variation.
///
/// This is the ONLY place a `variation_type` string becomes a variation. Both
/// call sites use it, so an unknown token is rejected identically everywhere —
/// previously the HTTP surface accepted `custom` and the Tauri command rejected
/// it, for the same input.
pub fn parse_variation(
    variation_type: &str,
    custom_overrides: Vec<serde_json::Value>,
) -> Result<ComparisonVariation, String> {
    match variation_type.trim().to_ascii_lowercase().as_str() {
        "architecture" => Ok(ComparisonVariation::Architecture),
        "same" => Ok(ComparisonVariation::Same),
        "multi_agent" => Ok(ComparisonVariation::MultiAgent),
        "custom" => Ok(ComparisonVariation::Custom {
            overrides: custom_overrides,
        }),
        other => Err(format!("Unknown variation_type: {}", other)),
    }
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

// =============================================================================
// Phase 2b — the axis facts as a PERSISTED, queryable pair
// =============================================================================
//
// Phases 1-3 compute the observed axis and its classification on the fly, at
// the one place that consumes them (`meta_optimizer::comparison_bridge`). That
// leaves the fact unrecorded: nothing can ask "how often do declared and actual
// disagree?" across runs, which is exactly the observation the plan's Phase
// 2->3 rate check needs, and exactly what a second untyped blob could not
// answer.
//
// So the pair is written into `project.comparison_runs` beside the DECLARED
// `variation_type` — `computed_axis jsonb NULL` and
// `axis_drift_class text NOT NULL DEFAULT 'unknown'` (qontinui-web alembic
// revision `cmpaxis_01_comparison_computed_axis`). Both writers go through
// [`axis_facts_from_entries_json`] so the stored pair is always derived from
// the same bytes the row itself stores, by the same code the bridge reads with.

/// The declared-vs-actual axis facts recorded for one comparison run.
///
/// The two fields are not independent, and the `Option` is load-bearing:
///
/// * `computed_axis: None` means the axis was **never computed** — the arms
///   could not be parsed, or there were fewer than two of them, so there was
///   nothing to diff. It is stored as SQL `NULL`.
/// * `computed_axis: Some(vec![])` means the axis **was** computed and nothing
///   differed. It is stored as an empty JSON array.
///
/// Collapsing those two into one value is the absence-is-not-zero mistake this
/// whole plan exists to stop (`verification-and-evidence`
/// `silent-empty-is-unknown`); `drift_class` says which case a row is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisFacts {
    /// The key paths observed to actually differ across the arms, or `None`
    /// when no axis could be computed at all.
    pub computed_axis: Option<Vec<String>>,
    /// How the computed axis relates to the declared `variation_type`.
    pub drift_class: AxisDriftClass,
}

impl AxisFacts {
    /// The facts for a run whose arms could not be read at all.
    pub fn unknown() -> Self {
        AxisFacts {
            computed_axis: None,
            drift_class: AxisDriftClass::Unknown,
        }
    }

    /// `computed_axis` as the value to bind to the `jsonb` column.
    ///
    /// `None` is SQL `NULL` — the never-computed case. It is deliberately NOT
    /// `Some(Value::Null)`, which would store the JSON scalar `null` and make
    /// "we never computed it" indistinguishable from a row that stored a null
    /// axis on purpose.
    pub fn computed_axis_json(&self) -> Option<serde_json::Value> {
        self.computed_axis.as_ref().map(|axes| {
            serde_json::Value::Array(
                axes.iter()
                    .map(|a| serde_json::Value::String(a.clone()))
                    .collect(),
            )
        })
    }
}

/// Extract the per-arm override blobs from a stored `entries_json` payload.
///
/// The live paths persist `Vec<ComparisonEntryJson>`, whose per-arm config
/// field is `overrides`. This reads that shape structurally rather than through
/// the struct, so it stays honest about what it could not read: `None` means
/// "these bytes are not an array of arm objects carrying `overrides`", which is
/// a coverage gap, not an empty comparison.
///
/// Note what it deliberately does NOT do: fall back to `unwrap_or_default()`.
/// An unreadable blob that silently becomes an empty `Vec` is the defect at
/// `comparison_bridge.rs`'s old inline parse — it reads as "no arms", which a
/// classifier can only report as agreement-shaped nonsense.
pub fn arm_overrides_from_entries_json(entries_json: &str) -> Option<Vec<serde_json::Value>> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(entries_json).ok()?;
    entries
        .into_iter()
        .map(|entry| match entry {
            serde_json::Value::Object(mut map) => map.remove("overrides"),
            _ => None,
        })
        .collect()
}

/// Derive the [`AxisFacts`] to persist for a run, from its DECLARED
/// `variation_type` and the arms it actually stored.
///
/// This is the single derivation used by every writer and by the meta-optimizer
/// bridge, so a row's `computed_axis` / `axis_drift_class` pair can never
/// disagree with what a reader would compute from the same `entries_json`.
pub fn axis_facts_from_entries_json(variation_type: &str, entries_json: &str) -> AxisFacts {
    let Some(arm_overrides) = arm_overrides_from_entries_json(entries_json) else {
        return AxisFacts::unknown();
    };
    axis_facts_from_arms(variation_type, &arm_overrides)
}

/// Same as [`axis_facts_from_entries_json`], for a caller that already holds
/// the per-arm override blobs.
///
/// Fewer than two arms yields `computed_axis: None` rather than an empty list:
/// with nothing to diff against, no axis was computed, and reporting `[]` there
/// would claim "nothing differed" about a comparison that never compared. The
/// class is [`AxisDriftClass::Divergent`] — whatever such a run declared, the
/// declared side is internally inconsistent.
pub fn axis_facts_from_arms(
    variation_type: &str,
    arm_overrides: &[serde_json::Value],
) -> AxisFacts {
    if arm_overrides.len() < 2 {
        return AxisFacts {
            computed_axis: None,
            drift_class: AxisDriftClass::Divergent,
        };
    }
    AxisFacts {
        computed_axis: Some(computed_treatment_axes(arm_overrides)),
        drift_class: classify_axis_drift(variation_type, arm_overrides),
    }
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

    // ---------------------------------------------------------------------
    // Phase 4 — one grammar, one derivation path
    // ---------------------------------------------------------------------

    #[test]
    fn parse_variation_accepts_every_wire_token_both_call_sites_use() {
        assert_eq!(
            parse_variation("architecture", vec![]).unwrap(),
            ComparisonVariation::Architecture
        );
        assert_eq!(
            parse_variation("same", vec![]).unwrap(),
            ComparisonVariation::Same
        );
        assert_eq!(
            parse_variation("multi_agent", vec![]).unwrap(),
            ComparisonVariation::MultiAgent
        );
        assert_eq!(
            parse_variation("custom", vec![json!({"model": "opus"})]).unwrap(),
            ComparisonVariation::Custom {
                overrides: vec![json!({"model": "opus"})]
            }
        );
        // Case/whitespace tolerated identically for both callers.
        assert_eq!(
            parse_variation("  ARCHITECTURE ", vec![]).unwrap(),
            ComparisonVariation::Architecture
        );
        // Unknown is rejected with the same message everywhere.
        let err = parse_variation("teleportation", vec![]).unwrap_err();
        assert_eq!(err, "Unknown variation_type: teleportation");
    }

    /// The regression this phase exists for: `custom` used to be accepted over
    /// HTTP and REJECTED by the Tauri command, for byte-identical input. Both
    /// now resolve through this one function, so parity is structural.
    #[test]
    fn custom_is_no_longer_rejected_by_one_call_site_and_accepted_by_the_other() {
        assert!(parse_variation("custom", vec![json!({"a": 1})]).is_ok());
    }

    #[test]
    fn architecture_arms_are_the_three_the_http_route_always_built() {
        let arms = build_comparison_arms(&ComparisonVariation::Architecture, 3, true);
        let labels: Vec<&str> = arms.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Traditional",
                "Agentic Verification",
                "Multi-Agent Pipeline"
            ]
        );
        let kinds: Vec<&str> = arms
            .iter()
            .map(|a| a.overrides["workflow_architecture"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "traditional",
                "agentic_verification",
                "multi_agent_pipeline"
            ]
        );
        // ...and the declared axis is exactly the one that moved.
        let overrides: Vec<serde_json::Value> = arms.iter().map(|a| a.overrides.clone()).collect();
        assert_eq!(
            computed_treatment_axes(&overrides),
            vec!["workflow_architecture".to_string()]
        );
        assert_eq!(
            classify_axis_drift("architecture", &overrides),
            AxisDriftClass::None
        );
    }

    #[test]
    fn use_worktree_is_injected_identically_into_every_arm() {
        for flag in [true, false] {
            for variation in [
                ComparisonVariation::Architecture,
                ComparisonVariation::Same,
                ComparisonVariation::MultiAgent,
            ] {
                let arms = build_comparison_arms(&variation, 3, flag);
                assert!(!arms.is_empty());
                for arm in &arms {
                    assert_eq!(
                        arm.overrides["use_worktree"],
                        serde_json::Value::Bool(flag),
                        "every arm carries the run-wide use_worktree"
                    );
                }
                // Being identical across arms, it can never be a treatment axis.
                let overrides: Vec<serde_json::Value> =
                    arms.iter().map(|a| a.overrides.clone()).collect();
                assert!(!computed_treatment_axes(&overrides).contains(&"use_worktree".to_string()));
            }
        }
    }

    #[test]
    fn label_is_a_sibling_of_the_overrides_not_a_key_inside_them() {
        // The old builder buried `label` INSIDE the override JSON on three arms,
        // which is what made it a spurious axis. It must not be there now.
        for variation in [
            ComparisonVariation::MultiAgent,
            ComparisonVariation::Model {
                models: vec!["opus".to_string(), "sonnet".to_string()],
            },
            ComparisonVariation::ContextTokens {
                limits: vec![100_000, 200_000],
            },
            ComparisonVariation::Same,
            ComparisonVariation::Architecture,
        ] {
            for arm in build_comparison_arms(&variation, 2, true) {
                assert!(
                    arm.overrides.get("label").is_none(),
                    "label leaked into the overrides of arm {:?}",
                    arm.label
                );
                assert!(!arm.label.is_empty(), "every arm needs a label");
            }
        }
    }

    #[test]
    fn custom_arms_take_their_label_from_the_caller_or_fall_back_by_index() {
        let arms = build_comparison_arms(
            &ComparisonVariation::Custom {
                overrides: vec![
                    json!({"label": "baseline", "config_override": "a"}),
                    json!({"config_override": "b"}),
                ],
            },
            2,
            true,
        );
        assert_eq!(arms[0].label, "baseline");
        assert_eq!(arms[1].label, "Custom 2");
        // The caller's own `label` stays inside its overrides on this path, which
        // is exactly why the ignore-list is not optional.
        assert_eq!(arms[0].overrides["label"], json!("baseline"));
        let overrides: Vec<serde_json::Value> = arms.iter().map(|a| a.overrides.clone()).collect();
        assert_eq!(
            computed_treatment_axes(&overrides),
            vec!["config_override".to_string()],
            "label must not surface as an axis even when the caller embeds it"
        );
    }

    #[test]
    fn same_emits_run_count_identical_arms() {
        let arms = build_comparison_arms(&ComparisonVariation::Same, 3, true);
        assert_eq!(arms.len(), 3);
        assert_eq!(arms[0].label, "Run 1");
        assert_eq!(arms[2].label, "Run 3");
        let overrides: Vec<serde_json::Value> = arms.iter().map(|a| a.overrides.clone()).collect();
        let empty: Vec<String> = Vec::new();
        assert_eq!(computed_treatment_axes(&overrides), empty);
        assert_eq!(
            classify_axis_drift("same", &overrides),
            AxisDriftClass::None
        );
    }

    #[test]
    fn build_run_overrides_is_the_label_free_projection_of_the_arms() {
        let variation = ComparisonVariation::Model {
            models: vec!["opus".to_string(), "sonnet".to_string()],
        };
        let arms = build_comparison_arms(&variation, 2, true);
        let projected = build_run_overrides(&variation, 2);
        let from_arms: Vec<serde_json::Value> = arms.into_iter().map(|a| a.overrides).collect();
        assert_eq!(projected, from_arms);
    }

    // =========================================================================
    // Phase 2b — the axis facts as a persisted pair
    // =========================================================================

    /// The exact `entries_json` shape the live paths store: a JSON array of
    /// `ComparisonEntryJson`, whose per-arm config field is `overrides`.
    fn stored_entries_json(arms: &[serde_json::Value]) -> String {
        let entries: Vec<serde_json::Value> = arms
            .iter()
            .enumerate()
            .map(|(i, overrides)| {
                serde_json::json!({
                    "label": format!("arm-{}", i),
                    "overrides": overrides,
                    "task_run_id": null,
                    "status": "pending",
                    "result": null,
                })
            })
            .collect();
        serde_json::to_string(&entries).unwrap()
    }

    #[test]
    fn arm_overrides_are_read_out_of_the_shape_the_live_paths_actually_store() {
        let arms = vec![json!({"model": "opus"}), json!({"model": "sonnet"})];
        let stored = stored_entries_json(&arms);
        assert_eq!(arm_overrides_from_entries_json(&stored), Some(arms));
    }

    #[test]
    fn a_real_built_arm_set_round_trips_through_the_stored_blob() {
        // Not a hand-rolled fixture: the arms the HTTP/Tauri surfaces actually
        // build, serialized the way they actually persist them.
        let variation = ComparisonVariation::Model {
            models: vec!["opus".to_string(), "sonnet".to_string()],
        };
        let overrides: Vec<serde_json::Value> = build_comparison_arms(&variation, 2, true)
            .into_iter()
            .map(|a| a.overrides)
            .collect();
        let stored = stored_entries_json(&overrides);

        let facts = axis_facts_from_entries_json("model", &stored);
        assert_eq!(facts.computed_axis, Some(vec!["model".to_string()]));
        assert_eq!(facts.drift_class, AxisDriftClass::None);
    }

    #[test]
    fn an_unreadable_blob_is_unknown_never_an_empty_comparison() {
        // The three ways the bytes can fail to be an arm array. Each must be
        // UNKNOWN — the coverage-gap class — and must NOT produce an axis.
        for blob in ["", "not json", "{}", r#"[{"label":"a"}]"#] {
            assert_eq!(
                arm_overrides_from_entries_json(blob),
                None,
                "blob {:?} should not parse as arms",
                blob
            );
            let facts = axis_facts_from_entries_json("same", blob);
            assert_eq!(facts, AxisFacts::unknown(), "blob {:?}", blob);
            assert_eq!(facts.drift_class, AxisDriftClass::Unknown);
            assert_eq!(facts.computed_axis, None);
            assert!(!facts.drift_class.is_clean());
        }
    }

    #[test]
    fn nothing_differed_and_never_computed_are_different_stored_values() {
        // The whole point of the `Option`. `[]` is a computed answer; `NULL` is
        // the absence of one. Collapsing them is the absence-is-not-zero bug.
        let same = stored_entries_json(&[json!({"a": 1}), json!({"a": 1})]);
        let computed = axis_facts_from_entries_json("same", &same);
        assert_eq!(computed.computed_axis, Some(Vec::new()));
        assert_eq!(computed.drift_class, AxisDriftClass::None);
        assert_eq!(computed.computed_axis_json(), Some(json!([])));

        let never = axis_facts_from_entries_json("same", "garbage");
        assert_eq!(never.computed_axis, None);
        assert_eq!(never.computed_axis_json(), None);

        assert_ne!(computed.computed_axis_json(), never.computed_axis_json());
    }

    #[test]
    fn fewer_than_two_arms_records_no_axis_and_says_divergent() {
        for arms in [vec![], vec![json!({"model": "opus"})]] {
            let stored = stored_entries_json(&arms);
            let facts = axis_facts_from_entries_json("model", &stored);
            // NOT `Some([])`: with nothing to diff against, no axis was
            // computed. Claiming "nothing differed" about a run that never
            // compared would be the same lie the declared label tells.
            assert_eq!(facts.computed_axis, None, "arms: {:?}", arms);
            assert_eq!(facts.drift_class, AxisDriftClass::Divergent);
            assert!(!facts.drift_class.is_clean());
        }
    }

    #[test]
    fn the_stored_pair_is_what_the_classifier_would_say_for_every_case() {
        // Every classification the plan names, driven end to end from stored
        // bytes rather than from an in-memory arm slice — this is the path both
        // writers and the meta-optimizer bridge take.
        let cases: Vec<(&str, Vec<serde_json::Value>, AxisDriftClass, Vec<&str>)> = vec![
            (
                "same",
                vec![json!({"a": 1}), json!({"a": 1})],
                AxisDriftClass::None,
                vec![],
            ),
            (
                "same",
                vec![json!({"a": 1}), json!({"a": 2})],
                AxisDriftClass::InPlace,
                vec!["a"],
            ),
            (
                "model",
                vec![json!({"model": "a"}), json!({"model": "b"})],
                AxisDriftClass::None,
                vec!["model"],
            ),
            (
                "model",
                vec![
                    json!({"model": "a", "temp": 1}),
                    json!({"model": "b", "temp": 2}),
                ],
                AxisDriftClass::BenignAdd,
                vec!["model", "temp"],
            ),
            (
                "model",
                vec![json!({"temp": 1}), json!({"temp": 2})],
                AxisDriftClass::Pending,
                vec!["temp"],
            ),
            (
                "model",
                vec![json!({"model": "a"}), json!({"model": "a"})],
                AxisDriftClass::ActiveNegation,
                vec![],
            ),
            (
                "no_such_variation",
                vec![json!({"model": "a"}), json!({"model": "b"})],
                AxisDriftClass::Unknown,
                vec!["model"],
            ),
        ];

        for (variation, arms, expected_class, expected_axis) in cases {
            let stored = stored_entries_json(&arms);
            let facts = axis_facts_from_entries_json(variation, &stored);
            assert_eq!(
                facts.drift_class, expected_class,
                "variation={} arms={:?}",
                variation, arms
            );
            assert_eq!(
                facts.computed_axis,
                Some(expected_axis.iter().map(|s| s.to_string()).collect()),
                "variation={} arms={:?}",
                variation,
                arms
            );
            // The persisted pair must agree with what a reader recomputes from
            // the same arms — that identity is why the column can be trusted.
            assert_eq!(facts, axis_facts_from_arms(variation, &arms));
        }
    }
}
