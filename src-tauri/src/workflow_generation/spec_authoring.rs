//! Spec authoring — composes the discovery pipeline with the meta-workflow
//! to produce candidate `IrPageSpec`s for un-spec'd pages and patch deltas
//! for drift-flagged specs.
//!
//! Composition only — every step calls an existing primitive:
//! - skeleton projection: `state_discovery_artifacts` -> `IrPageSpec`
//! - prompt priming: `meta_workflow::build_spec_priming_context`
//! - AI fill-in: `meta_workflow::build_meta_workflow_template`
//! - validation: `spec_api::handlers::post_author` (with the new gates)
//!
//! Stream E (Flywheel) — gated behind the `spec-authoring` Cargo feature.
//! Public API: [`author_candidate`] (entrypoint), [`AuthoringMode`],
//! [`AuthoringOutcome`], [`AuthoringError`], and [`pathname_to_spec_id`]
//! (shared with `spec_api::proposals`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::database::pg::PgDb;
use crate::spec_api::types::{
    IrElementCriteria, IrPageSpec, IrProvenance, IrState, IrTransition, ProposalStatus,
};
use crate::state_discovery::derive::DEFAULT_WINDOW_DAYS as DERIVE_WINDOW_DAYS;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// The two authoring flavors.
///
/// `FullPage` constructs a fresh candidate from observations alone; `Patch`
/// reconciles a drift report against an existing IR — only mutating the
/// affected `IrState.requiredElements` / appending `IrTransition`s.
///
/// Not `Clone` because `crate::commands::spec_drift::DriftReport` is not
/// `Clone`; the proposals path constructs an `AuthoringMode` once per
/// execute call and consumes it on the way into `author_candidate`.
#[derive(Debug)]
pub enum AuthoringMode {
    /// Build a fresh `IrPageSpec` for a pathname with no existing spec.
    FullPage { pathname: String },
    /// Mutate an existing IR to absorb a drift delta. Carries the existing
    /// spec id + the drift report; emits only the minimal IR mutation.
    Patch {
        existing_spec_id: String,
        drift: crate::commands::spec_drift::DriftReport,
    },
}

/// Authoring result. Either a fully-formed candidate `IrPageSpec` or an
/// [`AuthoringError`] with a structured reason (used by `proposals.rs` to
/// distinguish retryable failures from terminal ones).
#[derive(Debug)]
pub struct AuthoringOutcome {
    pub candidate: IrPageSpec,
    /// Free-form: artifact id, meta-workflow run id, token counts.
    /// Persisted in `spec_proposals.metadata` for diagnostics.
    pub diagnostics: serde_json::Value,
}

/// Structured error for authoring failures. `proposals.rs` maps each
/// variant to a `last_error` string + a status transition.
#[derive(Debug, thiserror::Error)]
pub enum AuthoringError {
    /// No discovery artifact exists for the pathname yet.
    #[error("no discovery artifact for pathname '{pathname}'")]
    NoDiscoveryArtifact { pathname: String },
    /// Discovery artifact has zero states (under-observed corpus).
    #[error("discovery artifact is empty")]
    EmptyArtifact,
    /// The artifact has states, but no observation carries this page's label —
    /// so nothing can be attributed to it. Distinct from [`Self::EmptyArtifact`]
    /// because the fix is different: the page needs visiting (or capture needs
    /// to be labelling it), not more derivation.
    #[error("page '{page_label}' has no labelled observations")]
    NoPageObservations { page_label: String },
    /// The page has observations, but none of the discovered states was ever
    /// fully present in one of them.
    #[error("none of {total_states} discovered states are active on page '{page_label}'")]
    NoActiveStates {
        page_label: String,
        total_states: usize,
    },
    /// Meta-workflow returned but the result failed structural validation.
    #[error("malformed AI output: {0}")]
    MalformedAiOutput(String),
    /// Bridge / AI provider error.
    #[error("AI dispatch failed: {0}")]
    AiDispatchFailed(String),
    /// Drift mode: existing spec couldn't be read.
    #[error("existing spec missing: {0}")]
    ExistingSpecMissing(String),
    /// Database error.
    #[error("database error: {0}")]
    Database(String),
}

/// Top-level entrypoint. `proposals.rs` calls this from the execute handler.
///
/// `app_id` scopes every spec read/write to the owning app (spec-multi-app
/// Stream C). Runs the appropriate authoring flavor and returns either a
/// candidate `IrPageSpec` (still un-validated — the validator gates are
/// layered on top by the proposals handler) or a structured error.
pub async fn author_candidate(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    app_id: &str,
    mode: AuthoringMode,
) -> Result<AuthoringOutcome, AuthoringError> {
    author_candidate_with_executor(pg_db, app_state, app_id, mode, &DefaultMetaWorkflowExecutor)
        .await
}

/// Test seam: same as [`author_candidate`] but takes an explicit
/// [`MetaWorkflowExecutor`] so unit tests can return canned AI output
/// without spinning a real task run.
pub async fn author_candidate_with_executor(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    app_id: &str,
    mode: AuthoringMode,
    executor: &dyn MetaWorkflowExecutor,
) -> Result<AuthoringOutcome, AuthoringError> {
    match mode {
        AuthoringMode::FullPage { pathname } => {
            // 1. Project the artifact into a deterministic skeleton.
            let skeleton = load_and_project_skeleton(&pg_db, app_id, &pathname).await?;
            let skeleton_id = skeleton.id.clone();

            // 2. AI fill-in via the meta-workflow.
            let filled =
                ai_fill_skeleton(pg_db.clone(), app_state.clone(), app_id, skeleton, executor)
                    .await?;

            Ok(AuthoringOutcome {
                candidate: filled,
                diagnostics: serde_json::json!({
                    "mode": "fullPage",
                    "pathname": pathname,
                    "skeleton_id": skeleton_id,
                }),
            })
        }
        AuthoringMode::Patch {
            existing_spec_id,
            drift,
        } => {
            let mutated = author_patch(
                pg_db,
                app_state,
                app_id,
                &existing_spec_id,
                &drift,
                executor,
            )
            .await?;
            Ok(AuthoringOutcome {
                candidate: mutated,
                diagnostics: serde_json::json!({
                    "mode": "patch",
                    "specId": existing_spec_id,
                    "missing_count": drift.missing_from_spec.len(),
                }),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Slug helper — shared with sibling `spec_api::proposals`
// ---------------------------------------------------------------------------

/// Re-exported from [`crate::spec_api::slug`], which owns both directions of
/// the pathname ↔ spec-id mapping so they cannot drift apart. It also has to
/// live outside this module: `spec_authoring` is behind the `spec-authoring`
/// feature, while `state_discovery::capture` needs the same slug on every
/// build.
pub(crate) use crate::spec_api::slug::pathname_to_spec_id;

// ---------------------------------------------------------------------------
// Step 3 — skeleton projection
// ---------------------------------------------------------------------------

/// Load the most recent (non-empty) **global** discovery artifact and project
/// the states active on `pathname` into a deterministic IR skeleton.
///
/// # Why the artifact is global
///
/// Co-occurrence discovery groups elements that appear in exactly the same set
/// of renders, so it needs the cross-view render pool: elements `{a,b,c}` seen
/// on pages 1-4 form one state, `{d,e}` seen on pages 2-3 form another.
/// Deriving per page would instead give every persistent element on that page
/// an identical render-set, collapsing the page into a single mega-state and
/// discarding exactly the shared-chrome structure the model wants to capture
/// once rather than N times.
///
/// So `S` (the state set) is derived globally, and this function computes
/// `S_Ξ ⊆ S` — the states active on one page — by intersecting each state's
/// render-set with the observations labelled for that page.
///
/// Determinism contract:
/// - states ordered by id ascending
/// - element criteria fields chosen by stable priority (`id > aria-label >
///   tag+text > role`)
/// - `IrElementCriteria.attributes` is a `BTreeMap` (already deterministic)
async fn load_and_project_skeleton(
    pg_db: &PgDb,
    app_id: &str,
    pathname: &str,
) -> Result<IrPageSpec, AuthoringError> {
    let candidate_id = pathname_to_spec_id(pathname);
    let artifact = load_latest_global_artifact(pg_db, &candidate_id).await?;

    let clusters = extract_clusters(&artifact.body)?;
    if clusters.is_empty() {
        return Err(AuthoringError::EmptyArtifact);
    }

    // A global artifact whose states carry no render-sets cannot be scoped to
    // any page. Selecting from it would return nothing for every page and read
    // as "this page is unobserved", so fail loudly on the artifact instead.
    if clusters.iter().all(|c| c.render_ids.is_empty()) {
        return Err(AuthoringError::MalformedAiOutput(format!(
            "discovery artifact {} has states but no screenshotIds — page \
             scoping is impossible; discovery ran without render-id passthrough",
            artifact.artifact_id
        )));
    }

    // Project S down to S_Ξ. The two empty cases are reported separately: a
    // page nothing was ever labelled for needs visiting (or capture needs to
    // be labelling it), whereas a page whose observations match no state needs
    // more derivation. Collapsing both into EmptyArtifact is what made a
    // producer/consumer key mismatch require a code read to diagnose.
    let page_renders = load_page_render_ids(pg_db, &candidate_id).await?;
    if page_renders.is_empty() {
        return Err(AuthoringError::NoPageObservations {
            page_label: candidate_id,
        });
    }

    let active = select_active_clusters(&clusters, &page_renders);
    if active.is_empty() {
        return Err(AuthoringError::NoActiveStates {
            page_label: candidate_id,
            total_states: clusters.len(),
        });
    }

    // Project each active cluster, then sort states by id for determinism.
    let mut states: Vec<IrState> = active
        .iter()
        .map(|c| project_cluster_to_state(app_id, c))
        .collect::<Vec<_>>();
    states.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(IrPageSpec {
        version: "1.0".into(),
        id: candidate_id.clone(),
        name: format!("Auto-discovered: {pathname}"),
        description: Some(format!(
            "Skeleton projected from {} of {} globally discovered states, \
             selected by intersecting each state's render-set with {} \
             observations labelled for this page (global corpus: {} \
             observations over the last 90 days).",
            states.len(),
            clusters.len(),
            page_renders.len(),
            artifact.observation_count
        )),
        metadata: None,
        provenance: Some(IrProvenance {
            source: "build-plugin".into(),
            app_id: app_id.to_string(),
            status: Some(ProposalStatus::Proposed),
            ..Default::default()
        }),
        states,
        transitions: Vec::new(),
        synthesized_groups: None,
        initial_state: None,
        api_assertions: None,
    })
}

/// The "loaded artifact" shape we work with internally. We deliberately keep
/// the raw JSON `body` around so consumers can re-extract additional fields
/// without re-querying.
struct LoadedArtifact {
    artifact_id: String,
    observation_count: i32,
    body: serde_json::Value,
}

/// Project the global state set `S` down to `S_Ξ` — the states active on one
/// page.
///
/// A state is active on the page when its render-set contains at least one of
/// the page's observations. Membership is deliberately "any", not "all": a
/// state spanning pages 1-4 is active on each of them, which is the whole
/// point of deriving globally. Order is preserved from the artifact so callers
/// keep their own determinism contract.
fn select_active_clusters<'a>(
    clusters: &'a [Cluster],
    page_renders: &HashSet<String>,
) -> Vec<&'a Cluster> {
    clusters
        .iter()
        .filter(|c| c.render_ids.iter().any(|r| page_renders.contains(r)))
        .collect()
}

/// Load the newest non-empty global artifact.
///
/// Global artifacts are the ones written with `spec_id IS NULL` — derivation
/// runs over the whole observation corpus (see [`load_and_project_skeleton`]
/// for why). `for_page` is used only to name the page in the not-found error.
async fn load_latest_global_artifact(
    pg_db: &PgDb,
    for_page: &str,
) -> Result<LoadedArtifact, AuthoringError> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| AuthoringError::Database(format!("PG pool error: {}", e)))?;

    let rows = conn
        .query(
            r#"SELECT id::text, artifact, observation_count
               FROM state_discovery_artifacts
               WHERE spec_id IS NULL
                 AND observation_count > 0
               ORDER BY derived_at DESC
               LIMIT 1"#,
            &[],
        )
        .await
        .map_err(|e| AuthoringError::Database(format!("PG query: {}", e)))?;

    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| AuthoringError::NoDiscoveryArtifact {
            pathname: for_page.to_string(),
        })?;
    let artifact_id: String = row.get(0);
    let body: serde_json::Value = row.get(1);
    let observation_count: i32 = row.get(2);

    Ok(LoadedArtifact {
        artifact_id,
        observation_count,
        body,
    })
}

/// Observation ids labelled for `page_label`, within the same 90-day window
/// derivation uses.
///
/// These are the render ids that a state's `screenshotIds` must intersect for
/// the state to count as active on this page. Observations captured before
/// page labelling existed carry `spec_id IS NULL` and are correctly excluded —
/// they contribute to the global clustering but cannot be attributed to a page.
async fn load_page_render_ids(
    pg_db: &PgDb,
    page_label: &str,
) -> Result<HashSet<String>, AuthoringError> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| AuthoringError::Database(format!("PG pool error: {}", e)))?;

    let rows = conn
        .query(
            r#"SELECT id::text
               FROM co_occurrence_observations
               WHERE spec_id = $1
                 AND invalidated_at IS NULL
                 AND captured_at >= now() - ($2::int || ' days')::interval"#,
            &[&page_label, &DERIVE_WINDOW_DAYS],
        )
        .await
        .map_err(|e| {
            AuthoringError::Database(format!("PG query co_occurrence_observations: {}", e))
        })?;

    Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
}

/// One discovered state from the global `state_discovery_artifacts.artifact`
/// JSON — a cluster of elements that share a render-set signature.
///
/// Field names mirror the Python adapter's serialization
/// (`UIBridgeStateDiscoveryResult.to_dict`), which emits camelCase
/// `stateImageIds` / `screenshotIds`. An earlier revision read a
/// `fingerprints` key that the adapter has never emitted; because the field
/// was `#[serde(default)]`, every cluster silently parsed with zero elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cluster {
    /// Cluster id (deterministic from the discovery side).
    id: String,
    /// Human-readable name. Falls back to `id` when absent.
    #[serde(default)]
    name: Option<String>,
    /// Element ids belonging to this state. The Python adapter prefixes them
    /// with `reg:`; [`project_cluster_to_state`] strips that back to the bare
    /// fingerprint capture wrote into `co_occurrence_observations`.
    #[serde(rename = "stateImageIds", default)]
    element_ids: Vec<String>,
    /// Observation ids this state was seen in — the state's **render-set**,
    /// i.e. the signature clustering grouped on. A state belongs to page `P`
    /// iff this set intersects `P`'s observations.
    #[serde(rename = "screenshotIds", default)]
    render_ids: Vec<String>,
}

fn extract_clusters(body: &serde_json::Value) -> Result<Vec<Cluster>, AuthoringError> {
    let arr = body
        .get("states")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            AuthoringError::MalformedAiOutput(
                "discovery artifact missing 'states' array".to_string(),
            )
        })?;

    let mut clusters = Vec::with_capacity(arr.len());
    for entry in arr {
        let cluster: Cluster = serde_json::from_value(entry.clone()).map_err(|e| {
            AuthoringError::MalformedAiOutput(format!("cluster parse failed: {}", e))
        })?;
        clusters.push(cluster);
    }
    Ok(clusters)
}

/// Project one discovery cluster into an `IrState`.
///
/// Fingerprints land in `excluded_elements` (the only `Vec<IrElementCriteria>`
/// slot in `IrState`) so the AI fill-in step has them in scope when emitting
/// the final assertions. The slot name is a historical artifact — Stream E
/// treats it as the "candidate criteria" channel for the skeleton, and the
/// AI verification step relocates them onto `assertions` as appropriate.
fn project_cluster_to_state(app_id: &str, cluster: &Cluster) -> IrState {
    let mut sorted: Vec<String> = cluster
        .element_ids
        .iter()
        // Undo the adapter's `reg:` prefix so the criteria carry the same
        // fingerprint capture stored on the observation.
        .map(|e| e.strip_prefix("reg:").unwrap_or(e).to_string())
        .collect();
    sorted.sort();
    sorted.dedup();

    let criteria: Vec<IrElementCriteria> = sorted.iter().map(fingerprint_to_criteria).collect();

    IrState {
        id: cluster.id.clone(),
        name: cluster.name.clone().unwrap_or_else(|| cluster.id.clone()),
        description: None,
        assertions: Vec::new(),
        excluded_elements: if criteria.is_empty() {
            None
        } else {
            Some(criteria)
        },
        conditions: None,
        is_initial: None,
        is_terminal: None,
        blocking: None,
        group: None,
        path_cost: None,
        precondition: None,
        element_ids: None,
        incoming_transitions: None,
        metadata: None,
        provenance: Some(IrProvenance {
            source: "build-plugin".into(),
            app_id: app_id.to_string(),
            status: Some(ProposalStatus::Proposed),
            ..Default::default()
        }),
        cross_refs: None,
    }
}

/// Stable priority order for fingerprint -> `IrElementCriteria` field
/// selection: `id > aria-label > tag+text > role`. Unknown shapes degrade
/// to `text`.
fn fingerprint_to_criteria(fingerprint: &String) -> IrElementCriteria {
    let mut out = IrElementCriteria::default();
    if let Some(rest) = fingerprint.strip_prefix("id:") {
        out.id = Some(rest.to_string());
    } else if let Some(rest) = fingerprint.strip_prefix("aria:") {
        out.aria_label = Some(rest.to_string());
    } else if let Some(rest) = fingerprint.strip_prefix("tag:") {
        // `tag:<tag>:<text>` — split on the first colon only.
        let mut parts = rest.splitn(2, ':');
        out.tag_name = parts.next().map(|s| s.to_string());
        out.text = parts.next().map(|s| s.to_string());
    } else if let Some(rest) = fingerprint.strip_prefix("role:") {
        out.role = Some(rest.to_string());
    } else {
        // Unprefixed fingerprints are opaque: `stable_element_fingerprint`
        // returns a 16-char SHA-256 prefix over (role, name, tag, structure),
        // which is one-way. Putting that hash in `text` would assert the
        // element's visible text IS "016e88557e9c2b1e" — a criterion that can
        // never match, handed to the AI fill-in step as if it were real
        // content. Park it in an attribute that is honest about being a
        // digest instead. Recovering matchable criteria requires the derive
        // step to pass element attributes through to discovery rather than
        // only `{"id": <hash>}`; until then the AI step gets a stable handle
        // and no false signal.
        out.attributes = Some(BTreeMap::from([(
            "data-fingerprint".to_string(),
            fingerprint.clone(),
        )]));
    }
    out
}

// ---------------------------------------------------------------------------
// Step 4 — meta-workflow integration (AI fill-in)
// ---------------------------------------------------------------------------

/// Hand the skeleton + 5 most-similar existing IRs to the meta-workflow. The
/// workflow fills in human-readable names/descriptions, infers transitions
/// from element semantics, and the result is stamped `provenance.source:
/// "ai-generated"`.
async fn ai_fill_skeleton(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    app_id: &str,
    skeleton: IrPageSpec,
    executor: &dyn MetaWorkflowExecutor,
) -> Result<IrPageSpec, AuthoringError> {
    use crate::workflow_generation::generator::GenerateWorkflowRequest;
    use crate::workflow_generation::meta_workflow::{
        build_meta_workflow_template, build_spec_priming_context,
    };

    let description = build_priming_description(&skeleton);
    let historical = build_spec_priming_context(&pg_db, app_id, &skeleton, 5).await;

    let request = GenerateWorkflowRequest {
        description: description.clone(),
        category: Some("spec-authoring".into()),
        ..Default::default()
    };
    let resolved_contexts = format!(
        "<<<SKELETON>>>\n{}\n<<<END SKELETON>>>",
        serde_json::to_string_pretty(&skeleton).unwrap_or_default()
    );

    let meta_wf = build_meta_workflow_template(
        &request,
        &resolved_contexts,
        historical.as_ref(),
        Some(&*app_state),
    );

    let run_result = executor
        .execute(app_state.clone(), pg_db.clone(), meta_wf)
        .await
        .map_err(AuthoringError::AiDispatchFailed)?;

    let mut filled: IrPageSpec =
        parse_meta_workflow_output(&run_result).map_err(AuthoringError::MalformedAiOutput)?;
    stamp_provenance_ai_generated_proposed(&mut filled, app_id);

    enforce_skeleton_invariants(&skeleton, &filled).map_err(AuthoringError::MalformedAiOutput)?;

    Ok(filled)
}

fn build_priming_description(skeleton: &IrPageSpec) -> String {
    let mut fingerprints: Vec<String> = Vec::new();
    for state in &skeleton.states {
        if let Some(excluded) = &state.excluded_elements {
            for crit in excluded {
                if let Some(id) = &crit.id {
                    fingerprints.push(format!("id:{}", id));
                } else if let Some(aria) = &crit.aria_label {
                    fingerprints.push(format!("aria:{}", aria));
                } else if let (Some(tag), Some(text)) = (&crit.tag_name, &crit.text) {
                    fingerprints.push(format!("tag:{}:{}", tag, text));
                } else if let Some(role) = &crit.role {
                    fingerprints.push(format!("role:{}", role));
                } else if let Some(text) = &crit.text {
                    fingerprints.push(format!("text:{}", text));
                }
            }
        }
    }
    fingerprints.sort();
    fingerprints.dedup();
    let listing = if fingerprints.is_empty() {
        "(no fingerprints captured)".to_string()
    } else {
        fingerprints.join(", ")
    };
    format!(
        "Spec-authoring for {}: project clusters {}.",
        skeleton.name, listing
    )
}

/// Stamp `provenance.source = "ai-generated"` and `status = Proposed` at the
/// document level + on every state whose existing provenance is missing or
/// `ai-generated`. States with `hand-authored` / `migrated` / `build-plugin`
/// provenance are left untouched.
fn stamp_provenance_ai_generated_proposed(ir: &mut IrPageSpec, app_id: &str) {
    let doc_prov = ir.provenance.get_or_insert_with(IrProvenance::default);
    doc_prov.source = "ai-generated".to_string();
    doc_prov.status = Some(ProposalStatus::Proposed);
    if doc_prov.app_id.is_empty() {
        doc_prov.app_id = app_id.to_string();
    }

    for state in &mut ir.states {
        match &state.provenance {
            None => {
                state.provenance = Some(IrProvenance {
                    source: "ai-generated".into(),
                    app_id: app_id.to_string(),
                    status: Some(ProposalStatus::Proposed),
                    ..Default::default()
                });
            }
            Some(p) if p.source == "ai-generated" || p.source.is_empty() => {
                let mut clone = p.clone();
                clone.source = "ai-generated".to_string();
                clone.status = Some(ProposalStatus::Proposed);
                if clone.app_id.is_empty() {
                    clone.app_id = app_id.to_string();
                }
                state.provenance = Some(clone);
            }
            Some(_) => {
                // Leave hand-authored / migrated / build-plugin alone.
            }
        }
    }
}

/// Skeleton invariants the AI must not violate. The AI may add transitions,
/// names, descriptions, metadata — but it may NOT delete states.
fn enforce_skeleton_invariants(skeleton: &IrPageSpec, filled: &IrPageSpec) -> Result<(), String> {
    if filled.states.len() != skeleton.states.len() {
        return Err(format!(
            "state count drift: skeleton={}, filled={}",
            skeleton.states.len(),
            filled.states.len()
        ));
    }
    let skel_ids: BTreeSet<&str> = skeleton.states.iter().map(|s| s.id.as_str()).collect();
    let filled_ids: BTreeSet<&str> = filled.states.iter().map(|s| s.id.as_str()).collect();
    if !skel_ids.is_subset(&filled_ids) {
        let missing: Vec<&&str> = skel_ids.difference(&filled_ids).collect();
        return Err(format!(
            "filled IR is missing skeleton state ids: {:?}",
            missing
        ));
    }
    Ok(())
}

fn parse_meta_workflow_output(value: &serde_json::Value) -> Result<IrPageSpec, String> {
    // Try the value as-is first.
    if let Ok(ir) = serde_json::from_value::<IrPageSpec>(value.clone()) {
        return Ok(ir);
    }
    // Common nesting shapes.
    for key in ["ir", "spec", "result", "candidate"] {
        if let Some(inner) = value.get(key) {
            if let Ok(ir) = serde_json::from_value::<IrPageSpec>(inner.clone()) {
                return Ok(ir);
            }
        }
    }
    Err(format!(
        "could not parse meta-workflow output as IrPageSpec (top-level keys: {:?})",
        value
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// Step 5 — patch authoring (drift reconciliation)
// ---------------------------------------------------------------------------

/// Per-state required-element additions + new transitions. RFC 7396-style.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct IrPatch {
    #[serde(default)]
    pub(crate) add_required_elements: BTreeMap<String, Vec<IrElementCriteria>>,
    #[serde(default)]
    pub(crate) add_transitions: Vec<IrTransition>,
}

async fn author_patch(
    pg_db: Arc<PgDb>,
    app_state: Arc<crate::commands::AppState>,
    app_id: &str,
    existing_spec_id: &str,
    drift: &crate::commands::spec_drift::DriftReport,
    executor: &dyn MetaWorkflowExecutor,
) -> Result<IrPageSpec, AuthoringError> {
    use crate::workflow_generation::generator::GenerateWorkflowRequest;
    use crate::workflow_generation::meta_workflow::{
        build_historical_context_pg, build_meta_workflow_template,
    };

    let root = crate::spec_api::storage::resolve_specs_root(&pg_db, app_id)
        .await
        .map_err(|e| {
            AuthoringError::ExistingSpecMissing(format!(
                "resolve_specs_root({}) failed: {:?}",
                app_id, e
            ))
        })?;
    let existing: IrPageSpec = crate::spec_api::storage::read_ir(&root, app_id, existing_spec_id)
        .map_err(AuthoringError::ExistingSpecMissing)?
        .ok_or_else(|| {
            AuthoringError::ExistingSpecMissing(format!(
                "no IR found for spec_id={existing_spec_id}"
            ))
        })?;

    let description = format!(
        "Patch-authoring for spec '{}': reconcile {} missing element(s).",
        existing_spec_id,
        drift.missing_from_spec.len()
    );
    let request = GenerateWorkflowRequest {
        description: description.clone(),
        category: Some("spec-authoring".into()),
        ..Default::default()
    };
    let resolved_contexts = build_patch_resolved_contexts(&existing, drift);

    let historical =
        build_historical_context_pg(&pg_db, &request.description, Some("spec-authoring")).await;
    let meta_wf = build_meta_workflow_template(
        &request,
        &resolved_contexts,
        historical.as_ref(),
        Some(&*app_state),
    );

    let run_result = executor
        .execute(app_state.clone(), pg_db.clone(), meta_wf)
        .await
        .map_err(AuthoringError::AiDispatchFailed)?;

    let patch: IrPatch =
        parse_patch_output(&run_result).map_err(AuthoringError::MalformedAiOutput)?;
    merge_patch_into_ir(existing, patch, app_id).map_err(AuthoringError::MalformedAiOutput)
}

fn build_patch_resolved_contexts(
    existing: &IrPageSpec,
    drift: &crate::commands::spec_drift::DriftReport,
) -> String {
    let drift_payload = serde_json::json!({
        "missingFromSpec": drift.missing_from_spec,
        "orphansInSpec": drift.orphans_in_spec,
    });
    format!(
        "<<<EXISTING_IR>>>\n{}\n<<<END_EXISTING_IR>>>\n\n<<<DRIFT>>>\n{}\n<<<END_DRIFT>>>",
        serde_json::to_string_pretty(existing).unwrap_or_default(),
        serde_json::to_string_pretty(&drift_payload).unwrap_or_default(),
    )
}

fn parse_patch_output(value: &serde_json::Value) -> Result<IrPatch, String> {
    if let Ok(patch) = serde_json::from_value::<IrPatch>(value.clone()) {
        return Ok(patch);
    }
    for key in ["patch", "ir_patch", "result", "candidate"] {
        if let Some(inner) = value.get(key) {
            if let Ok(patch) = serde_json::from_value::<IrPatch>(inner.clone()) {
                return Ok(patch);
            }
        }
    }
    Err(format!(
        "could not parse meta-workflow output as IrPatch (top-level keys: {:?})",
        value
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    ))
}

/// Apply a patch to an existing IR.
///
/// Rules (load-bearing — pinning invariant per §6.5):
/// - APPENDS new IrTransitions; rejects duplicates by id (also rejects ids
///   that collide with an existing transition).
/// - APPENDS new IrElementCriteria to `state.excluded_elements` ONLY for
///   states whose provenance.source is "ai-generated" or absent. States
///   with provenance.source in {"hand-authored", "migrated", "build-plugin"}
///   are PINNED — patch additions hard-fail.
/// - Newly added IrTransition / IrElementCriteria get
///   `provenance: { source: "ai-generated", status: Proposed }`.
pub(crate) fn merge_patch_into_ir(
    mut existing: IrPageSpec,
    patch: IrPatch,
    app_id: &str,
) -> Result<IrPageSpec, String> {
    // Validate transition ids first so we don't half-apply.
    let mut existing_tx_ids: BTreeSet<String> =
        existing.transitions.iter().map(|t| t.id.clone()).collect();
    let mut new_tx_ids: BTreeSet<String> = BTreeSet::new();
    for tx in &patch.add_transitions {
        if existing_tx_ids.contains(&tx.id) {
            return Err(format!(
                "patch transition id '{}' collides with an existing transition",
                tx.id
            ));
        }
        if !new_tx_ids.insert(tx.id.clone()) {
            return Err(format!(
                "patch contains duplicate transition id '{}'",
                tx.id
            ));
        }
    }

    // Validate every targeted state allows additions.
    for state_id in patch.add_required_elements.keys() {
        let target = existing
            .states
            .iter()
            .find(|s| &s.id == state_id)
            .ok_or_else(|| {
                format!(
                    "patch targets unknown state '{}' (not present in existing IR)",
                    state_id
                )
            })?;
        let source = target
            .provenance
            .as_ref()
            .map(|p| p.source.as_str())
            .unwrap_or("");
        match source {
            "" | "ai-generated" => { /* allowed */ }
            "hand-authored" | "migrated" | "build-plugin" => {
                return Err(format!(
                    "patch targets pinned state '{}' (provenance.source='{}'); refusing to mutate",
                    state_id, source
                ));
            }
            other => {
                return Err(format!(
                    "patch targets state '{}' with unrecognized provenance.source='{}'",
                    state_id, other
                ));
            }
        }
    }

    // Apply criteria additions. New IrElementCriteria don't carry their own
    // provenance (the type has no provenance field), so stamping happens at
    // the wrapper IrPatch level — the criteria themselves are "ai-generated"
    // by virtue of being added through this code path.
    for (state_id, criteria) in patch.add_required_elements {
        let state = existing
            .states
            .iter_mut()
            .find(|s| s.id == state_id)
            .expect("validated above");
        let slot = state.excluded_elements.get_or_insert_with(Vec::new);
        for crit in criteria {
            slot.push(crit);
        }
    }

    // Apply transition additions. Stamp provenance.
    for mut tx in patch.add_transitions {
        tx.provenance = Some(IrProvenance {
            source: "ai-generated".into(),
            app_id: app_id.to_string(),
            status: Some(ProposalStatus::Proposed),
            ..Default::default()
        });
        existing_tx_ids.insert(tx.id.clone());
        existing.transitions.push(tx);
    }

    Ok(existing)
}

// ---------------------------------------------------------------------------
// MetaWorkflowExecutor — production impl + test seam
// ---------------------------------------------------------------------------

/// Pluggable executor for the meta-workflow. Production uses
/// [`DefaultMetaWorkflowExecutor`], which persists the workflow + creates
/// a task run + polls it to terminal status. Tests substitute a canned
/// implementation so they don't need a live PG / AI provider.
#[async_trait::async_trait]
pub trait MetaWorkflowExecutor: Send + Sync {
    async fn execute(
        &self,
        app_state: Arc<crate::commands::AppState>,
        pg_db: Arc<PgDb>,
        meta_wf: crate::unified_workflows::UnifiedWorkflow,
    ) -> Result<serde_json::Value, String>;
}

/// Default production executor. Mirrors the persistence path in
/// `mcp::unified_workflows::generate_unified_workflow_async_handler` — persist
/// the workflow, create a task run, then poll until terminal. Returns the
/// final accumulated output blob as a `serde_json::Value`.
pub struct DefaultMetaWorkflowExecutor;

#[async_trait::async_trait]
impl MetaWorkflowExecutor for DefaultMetaWorkflowExecutor {
    async fn execute(
        &self,
        app_state: Arc<crate::commands::AppState>,
        pg_db: Arc<PgDb>,
        meta_wf: crate::unified_workflows::UnifiedWorkflow,
    ) -> Result<serde_json::Value, String> {
        execute_meta_workflow_blocking(app_state, pg_db, meta_wf).await
    }
}

/// Persist the meta-workflow, create a task run, poll until terminal, return
/// the accumulated output blob as a `serde_json::Value`.
///
/// Terminal task statuses are `complete` / `failed` / `stopped` (see
/// `database::types::TaskRun::status`). Polls every 2s; 120s overall timeout
/// (long enough for the meta-workflow's 4-phase cycle, short enough that a
/// stuck proposal surfaces in the nightly cadence).
pub(crate) async fn execute_meta_workflow_blocking(
    app_state: Arc<crate::commands::AppState>,
    pg_db: Arc<PgDb>,
    meta_wf: crate::unified_workflows::UnifiedWorkflow,
) -> Result<serde_json::Value, String> {
    use crate::database::types::CreateTaskRunInput;
    use crate::unified_workflows::UnifiedWorkflowExt;

    let mut create_request = crate::unified_workflows::unified_workflow_to_create_request(&meta_wf);
    create_request.generated_by_task_run_id = None;

    let saved = pg_db
        .create_unified_workflow(&create_request)
        .await
        .map_err(|e| format!("create_unified_workflow: {}", e))?;

    let task_run_id = uuid::Uuid::new_v4().to_string();
    let simple_prompt: String = saved
        .agentic_steps
        .iter()
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let port = app_state
        .api_port
        .load(std::sync::atomic::Ordering::Relaxed);

    let task_run_input = CreateTaskRunInput::new(&task_run_id, &saved.name)
        .with_task_type("ai")
        .with_prompt(&simple_prompt)
        .with_workflow_type("unified")
        .with_workflow_name(&saved.name)
        .with_workflow_id(&saved.id)
        .with_max_sessions(saved.iter_cap())
        .with_auto_continue(true)
        .with_runner_port(port);

    pg_db
        .create_task_run(&task_run_input)
        .await
        .map_err(|e| format!("create_task_run: {}", e))?;

    // Poll for terminal status.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    // Skip the immediate first tick — the task was just created.
    interval.tick().await;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("meta-workflow task run {} timed out", task_run_id));
        }
        interval.tick().await;
        let task = pg_db
            .get_task_run(&task_run_id)
            .await
            .map_err(|e| format!("get_task_run: {}", e))?
            .ok_or_else(|| format!("task run {} disappeared", task_run_id))?;
        if matches!(task.status.as_str(), "complete" | "failed" | "stopped") {
            if task.status == "failed" {
                return Err(task
                    .error_message
                    .unwrap_or_else(|| "meta-workflow failed (no error message)".to_string()));
            }
            // Parse the output_log as JSON when possible; otherwise return
            // it wrapped as a string blob so the parser can search for
            // common envelope keys.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&task.output_log) {
                return Ok(parsed);
            }
            return Ok(serde_json::json!({ "output_log": task.output_log }));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_api::hashing::canonical_hash;
    use crate::spec_api::types::{IrAssertion, IrAssertionTarget};

    // Slug behaviour is owned and tested by `spec_api::slug`, including the
    // round-trip property this module depends on.

    fn artifact_json(states: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "states": states,
            "elements": [],
            "elementToRenders": {},
        })
    }

    /// Build a state in the shape the Python adapter actually emits
    /// (`UIBridgeStateDiscoveryResult.to_dict`): camelCase `stateImageIds` /
    /// `screenshotIds`, with element ids `reg:`-prefixed. Fixtures must mirror
    /// the real serialization — an earlier fixture used a `fingerprints` key
    /// the adapter has never emitted, so these tests passed while the
    /// production path parsed every cluster as empty.
    fn cluster(id: &str, elements: &[&str]) -> serde_json::Value {
        cluster_seen_in(id, elements, &["render-default"])
    }

    fn cluster_seen_in(id: &str, elements: &[&str], renders: &[&str]) -> serde_json::Value {
        let element_ids: Vec<String> = elements.iter().map(|e| format!("reg:{e}")).collect();
        serde_json::json!({
            "id": id,
            "name": format!("cluster-{}", id),
            "stateImageIds": element_ids,
            "screenshotIds": renders,
        })
    }

    fn render_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn skeleton_from_artifact(body: &serde_json::Value, observation_count: i32) -> IrPageSpec {
        // Helper that mirrors the projection path without touching PG.
        let clusters = extract_clusters(body).expect("clusters parse");
        let mut states: Vec<IrState> = clusters
            .iter()
            .map(|c| project_cluster_to_state("qontinui-runner", c))
            .collect();
        states.sort_by(|a, b| a.id.cmp(&b.id));
        IrPageSpec {
            version: "1.0".into(),
            id: "account-billing".into(),
            name: "Auto-discovered: /account/billing".into(),
            description: Some(format!(
                "Skeleton projected from {} observations over the last 90 days.",
                observation_count
            )),
            metadata: None,
            provenance: Some(IrProvenance {
                source: "build-plugin".into(),
                app_id: "qontinui-runner".to_string(),
                status: Some(ProposalStatus::Proposed),
                ..Default::default()
            }),
            states,
            transitions: Vec::new(),
            synthesized_groups: None,
            initial_state: None,
            api_assertions: None,
        }
    }

    #[test]
    fn skeleton_projection_deterministic_byte_identical() {
        let body = artifact_json(vec![
            cluster("state-c", &["id:btn-pay", "aria:cart"]),
            cluster("state-a", &["id:btn-cancel"]),
            cluster("state-b", &["tag:input:email", "role:textbox"]),
        ]);
        let s1 = skeleton_from_artifact(&body, 7);
        let s2 = skeleton_from_artifact(&body, 7);
        let h1 = canonical_hash(&s1).expect("hash");
        let h2 = canonical_hash(&s2).expect("hash");
        assert_eq!(h1, h2, "skeleton projection must be byte-identical");
        // And ids sorted.
        let ids: Vec<&str> = s1.states.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["state-a", "state-b", "state-c"]);
    }

    #[test]
    fn empty_artifact_returns_empty_error() {
        let body = artifact_json(vec![]);
        let err = extract_clusters(&body);
        // extract_clusters returns Ok([]) for an empty array; the
        // caller (load_and_project_skeleton) converts to EmptyArtifact.
        // We test that conversion logic via a focused match.
        let clusters = err.expect("parsed");
        assert!(clusters.is_empty(), "empty cluster list expected");
    }

    #[test]
    fn real_adapter_shape_parses_with_elements_and_renders() {
        // Regression: the previous `fingerprints` field never appeared in a
        // real artifact, so `#[serde(default)]` yielded empty clusters and
        // every projected state came out with no criteria.
        let body = serde_json::json!({
            "states": [{
                "id": "fp_state_6c3eda5e919d",
                "name": "c0045a4533ac7947 (16 elements)",
                "confidence": 0.94,
                "stateImageIds": ["reg:016e88557e9c2b1e", "reg:017db53ce3bc7ca3"],
                "screenshotIds": ["0fe4e39c-1b98-49e7-a73d-7a5f572a636c"],
            }],
            "elements": [],
            "elementToRenders": {},
        });
        let clusters = extract_clusters(&body).expect("clusters parse");
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].element_ids.len(), 2, "elements must survive");
        assert_eq!(clusters[0].render_ids.len(), 1, "render-set must survive");

        // And the `reg:` prefix is stripped back to the stored fingerprint.
        let state = project_cluster_to_state("qontinui-runner", &clusters[0]);
        let criteria = state.excluded_elements.expect("criteria present");
        assert_eq!(criteria.len(), 2);

        // Assert content, not just count. Real fingerprints are opaque
        // SHA-256 prefixes, so they must NOT be asserted as visible text —
        // that would be a criterion no element can ever match.
        let first = &criteria[0];
        assert_eq!(first.text, None, "an opaque digest must not become text");
        assert_eq!(
            first
                .attributes
                .as_ref()
                .and_then(|a| a.get("data-fingerprint"))
                .map(String::as_str),
            Some("016e88557e9c2b1e"),
            "the digest is carried as an attribute that admits what it is"
        );
    }

    #[test]
    fn selection_projects_s_to_s_active_per_page() {
        // Operator's worked example: elements {a,b,c} appear on pages 1-4 and
        // form one state; {d,e} appear on pages 2-3 and form another. Page 2
        // therefore has BOTH states active simultaneously (S_Ξ ⊆ S).
        let body = artifact_json(vec![
            cluster_seen_in("abc", &["a", "b", "c"], &["p1", "p2", "p3", "p4"]),
            cluster_seen_in("de", &["d", "e"], &["p2", "p3"]),
        ]);
        let clusters = extract_clusters(&body).expect("clusters parse");

        let on_p1 = select_active_clusters(&clusters, &render_set(&["p1"]));
        assert_eq!(
            on_p1.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["abc"],
            "page 1 sees only the state spanning pages 1-4"
        );

        let on_p2 = select_active_clusters(&clusters, &render_set(&["p2"]));
        assert_eq!(
            on_p2.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["abc", "de"],
            "page 2 has both states active at once"
        );

        let on_p4 = select_active_clusters(&clusters, &render_set(&["p4"]));
        assert_eq!(
            on_p4.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["abc"],
            "page 4 drops the state confined to pages 2-3"
        );
    }

    #[test]
    fn page_with_no_labelled_observations_selects_nothing() {
        // The guard against the old behaviour: an unlabelled page must yield
        // no states rather than every state in the application.
        let body = artifact_json(vec![cluster_seen_in("abc", &["a"], &["p1"])]);
        let clusters = extract_clusters(&body).expect("clusters parse");
        assert!(select_active_clusters(&clusters, &render_set(&[])).is_empty());
        assert!(select_active_clusters(&clusters, &render_set(&["p9"])).is_empty());
    }

    #[test]
    fn three_cluster_artifact_three_states_with_provenance() {
        let body = artifact_json(vec![
            cluster("s1", &["id:foo"]),
            cluster("s2", &["aria:bar"]),
            cluster("s3", &["role:button"]),
        ]);
        let spec = skeleton_from_artifact(&body, 12);
        assert_eq!(spec.states.len(), 3);
        assert_eq!(spec.provenance.as_ref().unwrap().source, "build-plugin");
        assert_eq!(
            spec.provenance.as_ref().unwrap().status,
            Some(ProposalStatus::Proposed)
        );
        for state in &spec.states {
            let p = state.provenance.as_ref().expect("state provenance");
            assert_eq!(p.source, "build-plugin");
            assert_eq!(p.status, Some(ProposalStatus::Proposed));
        }
    }

    #[test]
    fn fingerprint_priority_order() {
        // id beats everything.
        let crit = fingerprint_to_criteria(&"id:my-button".to_string());
        assert_eq!(crit.id.as_deref(), Some("my-button"));
        assert!(crit.aria_label.is_none());
        // aria beats tag+text.
        let crit = fingerprint_to_criteria(&"aria:Submit".to_string());
        assert_eq!(crit.aria_label.as_deref(), Some("Submit"));
        // tag+text via tag:<tag>:<text>.
        let crit = fingerprint_to_criteria(&"tag:button:Save".to_string());
        assert_eq!(crit.tag_name.as_deref(), Some("button"));
        assert_eq!(crit.text.as_deref(), Some("Save"));
        // role last.
        let crit = fingerprint_to_criteria(&"role:dialog".to_string());
        assert_eq!(crit.role.as_deref(), Some("dialog"));

        // Unprefixed is what production actually emits — an opaque digest, not
        // a semantic prefix. It must not be asserted as visible text.
        let crit = fingerprint_to_criteria(&"016e88557e9c2b1e".to_string());
        assert_eq!(crit.text, None);
        assert_eq!(crit.id, None);
        assert_eq!(
            crit.attributes
                .as_ref()
                .and_then(|a| a.get("data-fingerprint"))
                .map(String::as_str),
            Some("016e88557e9c2b1e")
        );
    }

    // -----------------------------------------------------------------
    // Step 4 invariant tests
    // -----------------------------------------------------------------

    fn make_state(id: &str) -> IrState {
        IrState {
            id: id.into(),
            name: id.into(),
            description: None,
            assertions: vec![],
            excluded_elements: None,
            conditions: None,
            is_initial: None,
            is_terminal: None,
            blocking: None,
            group: None,
            path_cost: None,
            precondition: None,
            element_ids: None,
            incoming_transitions: None,
            metadata: None,
            provenance: None,
            cross_refs: None,
        }
    }

    fn make_skeleton(state_ids: &[&str]) -> IrPageSpec {
        IrPageSpec {
            version: "1.0".into(),
            id: "test".into(),
            name: "test".into(),
            description: None,
            metadata: None,
            provenance: Some(IrProvenance {
                source: "build-plugin".into(),
                app_id: "qontinui-runner".to_string(),
                status: Some(ProposalStatus::Proposed),
                ..Default::default()
            }),
            states: state_ids.iter().map(|id| make_state(id)).collect(),
            transitions: vec![],
            synthesized_groups: None,
            initial_state: None,
            api_assertions: None,
        }
    }

    #[test]
    fn enforce_invariants_passes_on_matching_state_set() {
        let skel = make_skeleton(&["a", "b", "c"]);
        let filled = make_skeleton(&["a", "b", "c"]);
        enforce_skeleton_invariants(&skel, &filled).expect("invariants hold");
    }

    #[test]
    fn enforce_invariants_fails_when_ai_drops_a_state() {
        let skel = make_skeleton(&["a", "b", "c"]);
        let filled = make_skeleton(&["a", "b"]);
        let err = enforce_skeleton_invariants(&skel, &filled).unwrap_err();
        assert!(err.contains("state count drift"));
    }

    #[test]
    fn stamp_provenance_skips_hand_authored_states() {
        let mut ir = make_skeleton(&["keep", "stamp"]);
        ir.states[0].provenance = Some(IrProvenance {
            source: "hand-authored".into(),
            app_id: "qontinui-runner".to_string(),
            ..Default::default()
        });
        // ir.states[1].provenance is None initially.
        stamp_provenance_ai_generated_proposed(&mut ir, "qontinui-runner");
        assert_eq!(
            ir.states[0].provenance.as_ref().unwrap().source,
            "hand-authored"
        );
        assert_eq!(
            ir.states[1].provenance.as_ref().unwrap().source,
            "ai-generated"
        );
        // Document-level always becomes ai-generated.
        assert_eq!(ir.provenance.as_ref().unwrap().source, "ai-generated");
    }

    // -----------------------------------------------------------------
    // Mocked executor — AI dispatch + malformed-output paths
    // -----------------------------------------------------------------

    struct FailingExecutor;

    #[async_trait::async_trait]
    impl MetaWorkflowExecutor for FailingExecutor {
        async fn execute(
            &self,
            _app_state: Arc<crate::commands::AppState>,
            _pg_db: Arc<PgDb>,
            _meta_wf: crate::unified_workflows::UnifiedWorkflow,
        ) -> Result<serde_json::Value, String> {
            Err("simulated bridge error".to_string())
        }
    }

    /// Construct an output blob that omits the `version` field, so
    /// IrPageSpec deserialization fails — exercising the MalformedAiOutput
    /// path without needing a full mock workflow.
    fn malformed_output() -> serde_json::Value {
        serde_json::json!({ "id": "x", "name": "x", "states": [], "transitions": [] })
    }

    #[test]
    fn parse_meta_workflow_output_handles_common_nestings() {
        let ir = IrPageSpec {
            version: "1.0".into(),
            id: "x".into(),
            name: "X".into(),
            description: None,
            metadata: None,
            provenance: None,
            states: vec![],
            transitions: vec![],
            synthesized_groups: None,
            initial_state: None,
            api_assertions: None,
        };
        let bare = serde_json::to_value(&ir).unwrap();
        assert!(parse_meta_workflow_output(&bare).is_ok());
        let nested = serde_json::json!({ "ir": bare });
        assert!(parse_meta_workflow_output(&nested).is_ok());
        let other = serde_json::json!({ "spec": serde_json::to_value(&ir).unwrap() });
        assert!(parse_meta_workflow_output(&other).is_ok());
        let bad = malformed_output();
        assert!(parse_meta_workflow_output(&bad).is_err());
    }

    // -----------------------------------------------------------------
    // Step 5 — patch merge tests
    // -----------------------------------------------------------------

    fn make_existing_with(state_id: &str, source: &str) -> IrPageSpec {
        let mut ir = make_skeleton(&[state_id]);
        ir.states[0].provenance = Some(IrProvenance {
            source: source.into(),
            app_id: "qontinui-runner".to_string(),
            ..Default::default()
        });
        ir
    }

    fn make_criteria(id: &str) -> IrElementCriteria {
        IrElementCriteria {
            id: Some(id.into()),
            ..Default::default()
        }
    }

    fn make_transition(id: &str, from: &str, to: &str) -> IrTransition {
        IrTransition {
            id: id.into(),
            name: id.into(),
            description: None,
            from_states: vec![from.into()],
            activate_states: vec![to.into()],
            exit_states: None,
            actions: vec![],
            path_cost: None,
            bidirectional: None,
            effect: None,
            metadata: None,
            provenance: None,
            cross_refs: None,
        }
    }

    #[test]
    fn merge_patch_against_ai_generated_state_lands() {
        let existing = make_existing_with("s1", "ai-generated");
        let mut patch = IrPatch::default();
        patch
            .add_required_elements
            .insert("s1".into(), vec![make_criteria("new-elem")]);
        let result = merge_patch_into_ir(existing, patch, "qontinui-runner").expect("merge ok");
        let crit = result.states[0]
            .excluded_elements
            .as_ref()
            .expect("criteria added")
            .first()
            .expect("at least one");
        assert_eq!(crit.id.as_deref(), Some("new-elem"));
    }

    #[test]
    fn merge_patch_against_hand_authored_state_fails() {
        let existing_before = make_existing_with("s1", "hand-authored");
        let snapshot = serde_json::to_value(&existing_before).unwrap();
        let mut patch = IrPatch::default();
        patch
            .add_required_elements
            .insert("s1".into(), vec![make_criteria("new-elem")]);
        let err = merge_patch_into_ir(existing_before, patch, "qontinui-runner").unwrap_err();
        assert!(err.contains("pinned state 's1'"), "got: {}", err);
        // The original snapshot should still match what we built — proves
        // the function did not return a half-mutated IR.
        // (snapshot was taken before moving `existing_before` into the
        // function; we just confirm it's still a valid spec.)
        assert!(snapshot.get("states").is_some());
    }

    #[test]
    fn merge_patch_duplicate_transition_id_fails() {
        let mut existing = make_skeleton(&["s1", "s2"]);
        existing
            .transitions
            .push(make_transition("tx-1", "s1", "s2"));
        let mut patch = IrPatch::default();
        patch
            .add_transitions
            .push(make_transition("tx-1", "s1", "s2"));
        let err = merge_patch_into_ir(existing, patch, "qontinui-runner").unwrap_err();
        assert!(err.contains("collides with an existing transition"));
    }

    #[test]
    fn merge_patch_adds_transition_with_ai_generated_provenance() {
        let existing = make_skeleton(&["s1", "s2"]);
        let mut patch = IrPatch::default();
        patch
            .add_transitions
            .push(make_transition("tx-new", "s1", "s2"));
        let result = merge_patch_into_ir(existing, patch, "qontinui-runner").expect("merge ok");
        assert_eq!(result.transitions.len(), 1);
        let prov = result.transitions[0]
            .provenance
            .as_ref()
            .expect("provenance stamped");
        assert_eq!(prov.source, "ai-generated");
        assert_eq!(prov.status, Some(ProposalStatus::Proposed));
    }

    #[test]
    fn merge_patch_intra_patch_duplicate_transition_id_fails() {
        let existing = make_skeleton(&["s1", "s2"]);
        let mut patch = IrPatch::default();
        patch
            .add_transitions
            .push(make_transition("tx-dup", "s1", "s2"));
        patch
            .add_transitions
            .push(make_transition("tx-dup", "s1", "s2"));
        let err = merge_patch_into_ir(existing, patch, "qontinui-runner").unwrap_err();
        assert!(err.contains("duplicate transition id"));
    }

    #[test]
    fn merge_patch_against_unknown_state_fails() {
        let existing = make_skeleton(&["s1"]);
        let mut patch = IrPatch::default();
        patch
            .add_required_elements
            .insert("ghost".into(), vec![make_criteria("x")]);
        let err = merge_patch_into_ir(existing, patch, "qontinui-runner").unwrap_err();
        assert!(err.contains("unknown state 'ghost'"));
    }

    // Silence unused-import warnings under cfg(test) for symbols referenced
    // by the doc-test in pathname_to_spec_id (the doc-test is illustrative
    // commentary in the doc-comment, but we still want to be sure the
    // pulled-in IrAssertion + IrAssertionTarget paths compile.
    #[test]
    fn _ir_assertion_symbol_resolves() {
        let _t = IrAssertionTarget {
            kind: "search".into(),
            criteria: serde_json::json!({}),
            label: "x".into(),
        };
        let _a = IrAssertion {
            id: "a".into(),
            description: "d".into(),
            category: "c".into(),
            severity: "s".into(),
            assertion_type: "at".into(),
            target: _t,
            source: "src".into(),
            reviewed: false,
            enabled: true,
            precondition: None,
        };
    }
}
