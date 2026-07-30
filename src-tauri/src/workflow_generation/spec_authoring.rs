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
use uuid::Uuid;

use crate::database::pg::PgDb;
use crate::spec_api::types::{
    IrElementCriteria, IrPageSpec, IrProvenance, IrState, IrTransition, ProposalStatus,
};

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
    /// No observation anywhere in the corpus carries this page's label, so
    /// nothing can be attributed to it. Distinct from [`Self::EmptyArtifact`]
    /// and from [`Self::NoActiveStates`] because the fix is different: the page
    /// needs visiting (or capture needs to be labelling it), not derivation.
    #[error("page '{page_label}' has no labelled observations")]
    NoPageObservations { page_label: String },
    /// The page has observations **newer than the artifact**, and none of them
    /// is in its render-set. The visits genuinely postdate the derivation, so
    /// the fix is a re-derive, not more visits.
    ///
    /// The `captured_at > derived_at` half of the predicate is what earns the
    /// prescription. Without it this variant also caught pages whose
    /// observations predate the artifact and were *skipped* by it — see
    /// [`Self::ObservedButUnclustered`] — and told them to re-derive, which
    /// would never have helped.
    #[error("none of {total_states} discovered states are active on page '{page_label}'")]
    NoActiveStates {
        page_label: String,
        total_states: usize,
    },
    /// The page has labelled observations, all of them older than the newest
    /// artifact, and none of them made it into that artifact's render-set.
    ///
    /// Re-deriving cannot fix this: the derivation already saw these rows and
    /// declined them. The reachable cause is an observation that carries no
    /// fingerprints — `state_discovery::derive::load_observations` `continue`s
    /// on an empty `fingerprints` array, so the row is labelled for the page,
    /// non-invalidated, and can never appear in any artifact. (A cluster whose
    /// `screenshotIds` are all non-UUID lands here too; that one is warned
    /// about separately at load.)
    #[error(
        "page '{page_label}' has observations, but the newest artifact was derived over them \
         and clustered none"
    )]
    ObservedButUnclustered {
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

    // The artifact's own render-set is what bounds the page query. `S_Ξ` is an
    // intersection, so it can never contain a render this artifact was not
    // derived over — every other row the old query returned was materialised
    // only to be discarded. Bounding by the artifact also removes the *second*
    // time window that query carried (`captured_at >= now() - 90 days`), which
    // could only ever disagree with the window the artifact was really derived
    // over. See [`load_page_render_ids`] for the full argument.
    let normalized = normalize_render_ids(&clusters);
    if !normalized.rejected.is_empty() {
        tracing::warn!(
            "spec_authoring: discovery artifact {} carries {} non-UUID screenshotId(s) \
             (e.g. {:?}) — they name no observation row and are ignored",
            artifact.artifact_id,
            normalized.rejected.len(),
            normalized.rejected.iter().take(3).collect::<Vec<_>>()
        );
    }
    if normalized.union.is_empty() {
        // Both shapes are "no bindable render id", but they are different
        // defects and the message must not assert the wrong one: `rejected`
        // non-empty means discovery emitted ids of the wrong *kind*
        // (`render_0`, `screenshot_000`), while `rejected` empty means the
        // `screenshotIds` arrays were themselves empty — ids dropped in
        // passthrough rather than mangled.
        //
        // The all-empty shape is normally caught by the `all(render_ids
        // .is_empty())` guard above, so this second branch is unreachable as
        // the code stands today (every raw id either resolves or is rejected).
        // It exists so the message stays true rather than merely lucky: relax
        // that guard — to tolerate one empty cluster, say — and the wrong
        // half of this message would otherwise start being printed.
        let detail = if normalized.rejected.is_empty() {
            format!(
                "discovery artifact {} has states whose screenshotIds are all empty — the \
                 render ids were dropped in passthrough, not mangled",
                artifact.artifact_id
            )
        } else {
            format!(
                "discovery artifact {} has {} screenshotId(s) and none of them is a UUID \
                 (e.g. {:?}) — discovery ran over a render log whose ids are not \
                 observation ids",
                artifact.artifact_id,
                normalized.rejected.len(),
                normalized.rejected.iter().take(3).collect::<Vec<_>>()
            )
        };
        return Err(AuthoringError::MalformedAiOutput(format!(
            "{detail}; page scoping is impossible"
        )));
    }
    if normalized.union.len() > MAX_BOUND_RENDERS {
        // Fail loudly rather than degrade silently. See [`MAX_BOUND_RENDERS`].
        return Err(AuthoringError::Database(format!(
            "discovery artifact {} names {} renders, above the {} bind ceiling for the \
             page-scoping query — narrow the derivation window so the artifact fits",
            artifact.artifact_id,
            normalized.union.len(),
            MAX_BOUND_RENDERS
        )));
    }

    // Project S down to S_Ξ.
    let page_renders = load_page_render_ids(pg_db, &candidate_id, &normalized.union).await?;
    if page_renders.is_empty() {
        // Three different causes land here, and collapsing them is what made a
        // producer/consumer key mismatch require a code read to diagnose. Since
        // the query is now bounded by the artifact, an empty result no longer
        // means "this page has no observations" — it means "no observation THIS
        // ARTIFACT was derived from carries this page's label". So ask PG the
        // one extra question that separates them, bounded by the artifact's own
        // `derived_at` so the answer names the cause rather than guessing at
        // it. It runs only on the failure path, and both `EXISTS` halves
        // short-circuit on `idx_observations_spec`.
        let seen = page_observation_history(pg_db, &candidate_id, artifact.derived_at).await?;
        return Err(if !seen.ever {
            // Nothing was ever labelled for this page. Fix: visit it, or make
            // capture label it.
            AuthoringError::NoPageObservations {
                page_label: candidate_id,
            }
        } else if seen.since_derivation {
            // The page has been observed since the artifact was derived, so a
            // re-derive really can pick those visits up.
            AuthoringError::NoActiveStates {
                page_label: candidate_id,
                total_states: clusters.len(),
            }
        } else {
            // Observed, but every observation predates the artifact — the
            // derivation already saw them and clustered none. Re-deriving would
            // do exactly the same thing again.
            AuthoringError::ObservedButUnclustered {
                page_label: candidate_id,
                total_states: clusters.len(),
            }
        });
    }

    // Non-empty by construction: `page_renders ⊆ normalized.union`, and every
    // id in the union came from some cluster's render-set, so any id that comes
    // back selects at least the cluster it came from.
    let active = select_active_clusters(&clusters, &normalized.per_cluster, &page_renders);
    debug_assert!(
        !active.is_empty(),
        "page_renders ⊆ normalized.union, so a non-empty page_renders must select a cluster"
    );

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
             selected by intersecting each state's render-set with the {} of \
             the artifact's {} resolvable renders that are labelled for this \
             page (global corpus at derivation time: {} observations).",
            states.len(),
            clusters.len(),
            page_renders.len(),
            normalized.union.len(),
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
    /// When the derivation ran. Load-bearing on the failure path: it is the
    /// boundary that separates "this page's visits postdate the derivation"
    /// (re-derive) from "the derivation saw them and clustered none"
    /// (re-deriving changes nothing). See [`page_observation_history`].
    derived_at: chrono::DateTime<chrono::Utc>,
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
///
/// `per_cluster_renders` is [`NormalizedRenders::per_cluster`] — positionally
/// aligned with `clusters`. It is passed in rather than re-derived here so the
/// ids that bound the query and the ids that answer it come from a single
/// normalization: two independent passes over the same raw strings is exactly
/// the producer/consumer drift this module keeps being bitten by.
fn select_active_clusters<'a>(
    clusters: &'a [Cluster],
    per_cluster_renders: &[Vec<Uuid>],
    page_renders: &HashSet<Uuid>,
) -> Vec<&'a Cluster> {
    debug_assert_eq!(
        clusters.len(),
        per_cluster_renders.len(),
        "per-cluster render ids must be positionally aligned with the clusters they came from"
    );
    clusters
        .iter()
        .zip(per_cluster_renders)
        .filter(|(_, renders)| renders.iter().any(|id| page_renders.contains(id)))
        .map(|(cluster, _)| cluster)
        .collect()
}

/// Canonicalize one artifact render id into the `co_occurrence_observations.id`
/// it names.
///
/// `screenshotIds` are passed through discovery verbatim from
/// [`crate::state_discovery::derive`], which writes the observation's UUID as
/// the render id. The Python adapter prefixes *element* ids with `reg:` and not
/// render ids, but strip it defensively so a future adapter change degrades to
/// a match rather than to a silent miss.
///
/// Returns `None` for ids that are not UUID-shaped. Those name no observation
/// row at all — the adapter's positional `render_<i>` fallback and the pixel
/// analyzers' `screenshot_000` are both of that shape — and binding one into a
/// `uuid[]` parameter would fail the whole query rather than just missing.
fn normalize_render_id(raw: &str) -> Option<Uuid> {
    Uuid::parse_str(raw.strip_prefix("reg:").unwrap_or(raw)).ok()
}

/// The single normalization of an artifact's raw `screenshotIds`.
///
/// Both consumers read from this one pass: [`union`](Self::union) bounds the
/// page query, [`per_cluster`](Self::per_cluster) answers it in
/// [`select_active_clusters`]. An earlier revision parsed the raw ids twice —
/// once to build the bound, once to test membership — which left two
/// independent implementations of "which observation does this id name" free to
/// drift apart. That is the same failure mode as the producer/consumer key
/// mismatch this module exists to have fixed.
struct NormalizedRenders {
    /// Every resolvable render id across all clusters, deduped and sorted.
    union: Vec<Uuid>,
    /// Ids that are not UUID-shaped, deduped and sorted. Reported, not bound.
    rejected: Vec<String>,
    /// Per-cluster resolvable render ids, positionally aligned with the
    /// `clusters` slice this was built from. Deduped and sorted per cluster.
    per_cluster: Vec<Vec<Uuid>>,
}

/// Normalize every cluster's render-set once, keeping both the union (the bound
/// on [`load_page_render_ids`]) and the per-cluster breakdown (the membership
/// test in [`select_active_clusters`]).
///
/// Every output is sorted and deduped, so the same artifact always produces the
/// same bound parameter and the same warning — the determinism contract extends
/// to the query.
fn normalize_render_ids(clusters: &[Cluster]) -> NormalizedRenders {
    let mut resolved: BTreeSet<Uuid> = BTreeSet::new();
    let mut rejected: BTreeSet<String> = BTreeSet::new();
    let mut per_cluster: Vec<Vec<Uuid>> = Vec::with_capacity(clusters.len());
    for cluster in clusters {
        let mut mine: BTreeSet<Uuid> = BTreeSet::new();
        for raw in &cluster.render_ids {
            match normalize_render_id(raw) {
                Some(id) => {
                    resolved.insert(id);
                    mine.insert(id);
                }
                None => {
                    rejected.insert(raw.clone());
                }
            }
        }
        per_cluster.push(mine.into_iter().collect());
    }
    NormalizedRenders {
        union: resolved.into_iter().collect(),
        rejected: rejected.into_iter().collect(),
        per_cluster,
    }
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
            r#"SELECT id::text, artifact, observation_count, derived_at
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
    // Selected even though the projection itself never looks at it: it is the
    // boundary the failure-path diagnosis in [`page_observation_history`] needs,
    // and fetching it here costs nothing on a row we are already reading.
    let derived_at: chrono::DateTime<chrono::Utc> = row.get(3);

    Ok(LoadedArtifact {
        artifact_id,
        observation_count,
        derived_at,
        body,
    })
}

/// Ceiling on how many render ids may be bound into [`PAGE_RENDERS_SQL`].
///
/// The bound parameter is the union of every cluster's render-set — roughly
/// every clustered observation across *all* pages — and it is re-sent on every
/// `load_and_project_skeleton` call. At 100k observations that is a ~1.6 MB
/// binary `uuid[]` per call plus a `ScalarArrayOpExpr` evaluated over the whole
/// `idx_observations_spec` scan. That is the deliberate trade documented on
/// [`load_page_render_ids`], but a trade with no ceiling eventually stops being
/// one, so the ceiling is explicit and it fails loudly: silently truncating the
/// bound would shrink `S_Ξ` and read as "this page is unobserved", which is
/// precisely the class of silent wrongness this whole change removes.
///
/// 50k is chosen to sit an order of magnitude above the corpora we actually
/// see, so hitting it means the observation retention policy needs attention,
/// not that the query needs tuning.
const MAX_BOUND_RENDERS: usize = 50_000;

/// The page-scoping query, hoisted to a `const` so a test can pin its shape.
///
/// The A4 defect was a `captured_at` predicate here, and it is invisible in any
/// unit test that does not reach PG — so the regression guard is an assertion
/// that this string bounds by `id`, not by time. See
/// `page_query_is_bounded_by_artifact_ids_not_by_time`.
const PAGE_RENDERS_SQL: &str = r#"SELECT id
   FROM co_occurrence_observations
   WHERE spec_id = $1
     AND invalidated_at IS NULL
     AND id = ANY($2::uuid[])"#;

/// Observation ids labelled for `page_label`, **bounded by the artifact's own
/// render-set** rather than by a time window.
///
/// These are the render ids that a state's `screenshotIds` must intersect for
/// the state to count as active on this page. `artifact_renders` is the union
/// of those `screenshotIds` (see [`normalize_render_ids`]), so pushing it into
/// the predicate makes PG return *exactly* the intersection: previously every
/// observation labelled for the page in the trailing window came back, and
/// everything outside the artifact's render-set was materialised into a
/// `HashSet` only to be discarded by [`select_active_clusters`].
///
/// More importantly it removes the *second* time window. Windowing on
/// `now() - 90 days` here while the artifact was derived at an arbitrary
/// earlier `derived_at` over an arbitrary per-request `window_days` meant the
/// two windows could only ever disagree: an artifact derived 60 days ago over
/// a 90-day window references renders 0–150 days old, and the trailing filter
/// dropped the 90–150-day slice of them — silently shrinking `S_Ξ`, and
/// emptying it entirely for a page whose visits all fall in that slice, which
/// reads as "this page is unobserved". Bounding by the artifact leaves no
/// second window to drift: the set is defined by the artifact that produced
/// the clusters, so it cannot disagree with itself.
///
/// The trade this makes is deliberate. The bind parameter grows with the
/// artifact (≈ every clustered observation across all pages), so on a page
/// that is a small share of traffic more ids go up than rows come back, and
/// `spec_id = $1 AND id = ANY(...)` is an `idx_observations_spec` scan plus a
/// `ScalarArrayOpExpr` filter rather than a pure index seek. Correctness by
/// construction is worth that: the alternative (bind `derived_at` +
/// `window_days` off the artifact row) keeps a time predicate here and so
/// keeps the whole drift class alive, merely narrowing it. It is a trade with
/// a ceiling, though — see [`MAX_BOUND_RENDERS`], which the caller enforces
/// before it gets here.
///
/// Two filters are still load-bearing. Observations captured before page
/// labelling existed carry `spec_id IS NULL` and are correctly excluded — they
/// contribute to the global clustering but cannot be attributed to a page. And
/// `invalidated_at IS NULL` must stay: an operator-invalidated observation has
/// to stop counting even though an artifact derived before the invalidation
/// still names it.
async fn load_page_render_ids(
    pg_db: &PgDb,
    page_label: &str,
    artifact_renders: &[Uuid],
) -> Result<HashSet<Uuid>, AuthoringError> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| AuthoringError::Database(format!("PG pool error: {}", e)))?;

    let rows = conn
        .query(PAGE_RENDERS_SQL, &[&page_label, &artifact_renders])
        .await
        .map_err(|e| {
            AuthoringError::Database(format!("PG query co_occurrence_observations: {}", e))
        })?;

    Ok(rows.into_iter().map(|r| r.get::<_, Uuid>(0)).collect())
}

/// What the page's observation history looks like relative to one artifact.
struct PageObservationHistory {
    /// Has this page EVER carried a non-invalidated observation?
    ever: bool,
    /// Has it carried one captured *after* the artifact was derived?
    since_derivation: bool,
}

/// Ask PG the two questions that separate the three causes of an empty
/// intersection, in one round trip.
///
/// Only called when [`load_page_render_ids`] came back empty. That query is
/// bounded by the artifact, so an empty result no longer distinguishes the
/// causes on its own, and they need genuinely different fixes:
///
/// | `ever` | `since_derivation` | cause | fix |
/// |--------|--------------------|-------|-----|
/// | false  | –                  | nothing was ever labelled for this page | visit it, or make capture label it |
/// | true   | true               | the page's visits postdate the derivation | re-derive |
/// | true   | false              | the derivation saw these rows and clustered none | *not* a re-derive |
///
/// The third row is why `since_derivation` exists. A time-unbounded "has this
/// page been observed" probe cannot see it, so the caller used to answer
/// "re-derive" for it — and re-deriving would never fix it. The reachable cause
/// is `state_discovery::derive::load_observations` skipping observations with an
/// empty `fingerprints` array: those rows are labelled, non-invalidated, and
/// structurally unable to appear in any artifact.
///
/// `ever` stays deliberately unbounded in time — the question really is "EVER",
/// and a window there would reintroduce exactly the drift the caller's query
/// just removed. `since_derivation` is bounded by the artifact's own
/// `derived_at` rather than by a wall-clock window, for the same reason.
///
/// Both halves are `EXISTS`, so both short-circuit on `idx_observations_spec`.
async fn page_observation_history(
    pg_db: &PgDb,
    page_label: &str,
    derived_at: chrono::DateTime<chrono::Utc>,
) -> Result<PageObservationHistory, AuthoringError> {
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| AuthoringError::Database(format!("PG pool error: {}", e)))?;

    let row = conn
        .query_one(
            r#"SELECT EXISTS(
                   SELECT 1
                   FROM co_occurrence_observations
                   WHERE spec_id = $1
                     AND invalidated_at IS NULL
               ),
               EXISTS(
                   SELECT 1
                   FROM co_occurrence_observations
                   WHERE spec_id = $1
                     AND invalidated_at IS NULL
                     AND captured_at > $2
               )"#,
            &[&page_label, &derived_at],
        )
        .await
        .map_err(|e| {
            AuthoringError::Database(format!("PG query co_occurrence_observations: {}", e))
        })?;

    Ok(PageObservationHistory {
        ever: row.get::<_, bool>(0),
        since_derivation: row.get::<_, bool>(1),
    })
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
        cluster_seen_in(id, elements, &[0])
    }

    /// Deterministic stand-in for an observation id. Render ids in a real
    /// artifact are `co_occurrence_observations.id` UUIDs passed through
    /// discovery verbatim, and the page query now binds them as `uuid[]` — so
    /// fixtures have to be UUID-shaped too, or they'd exercise a shape
    /// production can never produce.
    fn rid(tag: u128) -> Uuid {
        Uuid::from_u128(tag)
    }

    fn cluster_seen_in(id: &str, elements: &[&str], renders: &[u128]) -> serde_json::Value {
        let element_ids: Vec<String> = elements.iter().map(|e| format!("reg:{e}")).collect();
        let render_ids: Vec<String> = renders.iter().map(|r| rid(*r).to_string()).collect();
        serde_json::json!({
            "id": id,
            "name": format!("cluster-{}", id),
            "stateImageIds": element_ids,
            "screenshotIds": render_ids,
        })
    }

    fn render_set(ids: &[u128]) -> HashSet<Uuid> {
        ids.iter().map(|i| rid(*i)).collect()
    }

    /// Run the production selection path end to end: normalize once, then
    /// select from that normalization. Tests call this rather than
    /// [`select_active_clusters`] directly so they exercise the same single
    /// pass production does — a test-local re-parse of the raw ids would
    /// re-open exactly the two-implementations drift `NormalizedRenders`
    /// closes.
    fn active_on<'a>(clusters: &'a [Cluster], page_renders: &HashSet<Uuid>) -> Vec<&'a Cluster> {
        let normalized = normalize_render_ids(clusters);
        select_active_clusters(clusters, &normalized.per_cluster, page_renders)
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
                "Skeleton projected from {} observations.",
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
            cluster_seen_in("abc", &["a", "b", "c"], &[1, 2, 3, 4]),
            cluster_seen_in("de", &["d", "e"], &[2, 3]),
        ]);
        let clusters = extract_clusters(&body).expect("clusters parse");

        let on_p1 = active_on(&clusters, &render_set(&[1]));
        assert_eq!(
            on_p1.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["abc"],
            "page 1 sees only the state spanning pages 1-4"
        );

        let on_p2 = active_on(&clusters, &render_set(&[2]));
        assert_eq!(
            on_p2.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["abc", "de"],
            "page 2 has both states active at once"
        );

        let on_p4 = active_on(&clusters, &render_set(&[4]));
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
        let body = artifact_json(vec![cluster_seen_in("abc", &["a"], &[1])]);
        let clusters = extract_clusters(&body).expect("clusters parse");
        assert!(active_on(&clusters, &render_set(&[])).is_empty());
        assert!(active_on(&clusters, &render_set(&[9])).is_empty());
    }

    // -----------------------------------------------------------------
    // Artifact-bounded render intersection (A4/A5)
    // -----------------------------------------------------------------

    #[test]
    fn normalize_render_id_accepts_uuids_and_strips_reg_prefix() {
        let raw = "0fe4e39c-1b98-49e7-a73d-7a5f572a636c";
        let expected = Uuid::parse_str(raw).unwrap();
        assert_eq!(normalize_render_id(raw), Some(expected));
        // Defensive: element ids are `reg:`-prefixed by the Python adapter, so
        // a future adapter change that prefixes render ids too must degrade to
        // a match rather than to a silent miss.
        assert_eq!(normalize_render_id(&format!("reg:{raw}")), Some(expected));
    }

    #[test]
    fn normalize_render_id_rejects_non_uuid_shapes() {
        // The adapter's positional fallback and the pixel analyzers' synthetic
        // ids name no observation row. They must be rejected rather than bound
        // into a `uuid[]` parameter, which would fail the whole query instead
        // of just missing on one id.
        assert_eq!(normalize_render_id("render_0"), None);
        assert_eq!(normalize_render_id("screenshot_000"), None);
        assert_eq!(normalize_render_id(""), None);
    }

    #[test]
    fn normalize_render_ids_unions_across_clusters_deduped_and_sorted() {
        // The bound parameter is the union of every state's render-set — that
        // is exactly the set the page intersection can draw from, so anything
        // outside it is a row the query never needed to return.
        let body = artifact_json(vec![
            cluster_seen_in("abc", &["a"], &[3, 1, 2]),
            cluster_seen_in("de", &["d"], &[2, 3]),
        ]);
        let clusters = extract_clusters(&body).expect("clusters parse");
        let normalized = normalize_render_ids(&clusters);
        assert_eq!(
            normalized.union,
            vec![rid(1), rid(2), rid(3)],
            "union is deduped and deterministically ordered"
        );
        assert!(normalized.rejected.is_empty());
        // The per-cluster breakdown is what `select_active_clusters` tests
        // membership against, so it must be aligned and per-cluster sorted —
        // not the union repeated.
        assert_eq!(
            normalized.per_cluster,
            vec![vec![rid(1), rid(2), rid(3)], vec![rid(2), rid(3)]],
            "each cluster keeps its own render-set, positionally aligned"
        );
    }

    #[test]
    fn normalize_render_ids_partitions_out_unbindable_ids() {
        let body = serde_json::json!({
            "states": [{
                "id": "s1",
                "stateImageIds": ["reg:a"],
                "screenshotIds": [
                    "0fe4e39c-1b98-49e7-a73d-7a5f572a636c",
                    "render_0",
                    "screenshot_000",
                ],
            }],
            "elements": [],
            "elementToRenders": {},
        });
        let clusters = extract_clusters(&body).expect("clusters parse");
        let normalized = normalize_render_ids(&clusters);
        assert_eq!(normalized.union.len(), 1, "only the UUID is bindable");
        assert_eq!(
            normalized.rejected,
            vec!["render_0".to_string(), "screenshot_000".to_string()],
            "non-UUID ids are reported, not silently dropped into the query"
        );
    }

    #[test]
    fn bind_ceiling_is_a_ceiling_and_not_a_truncation() {
        // The value must stay well clear of realistic corpora — hitting it
        // should mean "retention needs attention", not "normal Tuesday".
        assert!(
            MAX_BOUND_RENDERS >= 50_000,
            "a ceiling below the corpora we actually see would fail healthy artifacts"
        );
        // And it must be a hard stop: nothing anywhere may `truncate`/`take`
        // the bound to fit, because a silently shortened bound shrinks S_Ξ and
        // reads as "this page is unobserved" — the exact silent wrongness this
        // module keeps being bitten by. The guard is that the ceiling is only
        // ever compared against, never used as a length.
        let body = artifact_json(vec![cluster_seen_in("abc", &["a"], &[1, 2, 3])]);
        let clusters = extract_clusters(&body).expect("clusters parse");
        let normalized = normalize_render_ids(&clusters);
        assert_eq!(
            normalized.union.len(),
            3,
            "normalization never caps; the caller rejects an oversized artifact outright"
        );
    }

    #[test]
    fn page_query_is_bounded_by_artifact_ids_not_by_time() {
        // A4 regression guard. The defect WAS a time predicate in this query:
        // it bounded the page's observations by `now() - 90 days` while the
        // artifact had been derived at some earlier `derived_at` over some
        // other `window_days`, so the two could only disagree — an artifact
        // derived 60 days ago over a 90-day window references renders 0–150
        // days old and the trailing filter dropped the 90–150-day slice.
        //
        // That predicate is invisible to every test that does not reach PG,
        // which is why the guard is on the statement text: re-adding any time
        // bound here fails this test.
        assert!(
            PAGE_RENDERS_SQL.contains("id = ANY($2::uuid[])"),
            "the page query must be bounded by the artifact's render ids"
        );
        assert!(
            !PAGE_RENDERS_SQL.contains("captured_at"),
            "the page query must NOT re-introduce a time window — that is the \
             A4 drift class; bound by the artifact instead. Got: {PAGE_RENDERS_SQL}"
        );
        // The two filters that must survive any rewrite of this query.
        assert!(
            PAGE_RENDERS_SQL.contains("spec_id = $1"),
            "page scoping must stay"
        );
        assert!(
            PAGE_RENDERS_SQL.contains("invalidated_at IS NULL"),
            "an operator-invalidated observation must stop counting even when \
             an older artifact still names it"
        );
    }

    #[test]
    fn an_aged_out_render_is_still_selected() {
        // The behavioural half of A4: a state whose renders all predate the
        // old trailing window is still selected, because selection is now
        // driven by what the artifact names rather than by capture age.
        let body = artifact_json(vec![
            cluster_seen_in("old", &["a"], &[100, 101]),
            cluster_seen_in("recent", &["b"], &[200]),
        ]);
        let clusters = extract_clusters(&body).expect("clusters parse");

        // What PG returns is the intersection of the page's labelled rows with
        // the artifact's render-set; here the page carries only "old" renders.
        let active = active_on(&clusters, &render_set(&[100, 101]));
        assert_eq!(
            active.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["old"]
        );
    }

    #[test]
    fn selection_matches_prefixed_and_uppercase_render_ids() {
        // Normalization is applied on BOTH sides of the comparison, so an
        // artifact that ever emits a `reg:`-prefixed or uppercase render id
        // still matches the canonical uuid PG returns. The old string equality
        // would have silently missed these.
        let canonical = rid(7);
        let body = serde_json::json!({
            "states": [{
                "id": "prefixed",
                "stateImageIds": ["reg:a"],
                "screenshotIds": [format!("reg:{}", canonical.to_string().to_uppercase())],
            }],
            "elements": [],
            "elementToRenders": {},
        });
        let clusters = extract_clusters(&body).expect("clusters parse");
        let active = active_on(&clusters, &render_set(&[7]));
        assert_eq!(
            active.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["prefixed"]
        );
    }

    #[test]
    fn cluster_with_only_non_uuid_renders_is_never_selected() {
        // Such a cluster contributes nothing to the bind array, so no id PG
        // can return will ever select it. It must not fall through into S_Ξ.
        let body = serde_json::json!({
            "states": [
                {
                    "id": "synthetic",
                    "stateImageIds": ["reg:a"],
                    "screenshotIds": ["render_0", "screenshot_000"],
                },
                {
                    "id": "real",
                    "stateImageIds": ["reg:b"],
                    "screenshotIds": [rid(5).to_string()],
                },
            ],
            "elements": [],
            "elementToRenders": {},
        });
        let clusters = extract_clusters(&body).expect("clusters parse");
        let normalized = normalize_render_ids(&clusters);
        assert_eq!(
            normalized.union,
            vec![rid(5)],
            "only the real render is bindable"
        );
        assert_eq!(normalized.rejected.len(), 2);
        assert_eq!(
            normalized.per_cluster,
            vec![vec![], vec![rid(5)]],
            "the synthetic-render cluster resolves to an empty render-set"
        );

        let active = active_on(&clusters, &render_set(&[5]));
        assert_eq!(
            active.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["real"],
            "the synthetic-render cluster must not leak into S_Ξ"
        );
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
