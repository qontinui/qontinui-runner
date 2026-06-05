//! Runner-side **ambient edit-effect loop** — the flag-gated `predict → verify`
//! D3 loop wrapped around every agent-worktree edit session.
//!
//! Plan reference: `plans/2026-06-05-edit-effect-loop-adoption-plan.md` §"Phase
//! 3 — Ambient runner loop (flag-gated)". The entire edit-effect calculus
//! (coord's `predict` / `predict-and-check` / `verify` routes + the Ξ_FS /
//! Ξ_Type / Ξ_Test sub-spaces feeding them) is live and fed but had **zero
//! production callers** until this loop wired it into the agent workflow. This
//! is the D3 loop the calculus exists for: predict an edit's cross-sub-space
//! effect at acquisition time, then verify the realized effect at session end —
//! automatically, for ALL agent-worktree edits (not just skill-driven ones).
//!
//! ## What it does
//!
//! - **Predict** (`spawn_predict`) — fired right after an
//!   [`super::isolated_edit::IsolatedEditContext`] materializes (BOTH the
//!   `acquire` and `acquire_for_terminal` entry points). POSTs the context's
//!   declared paths + base sha to coord's
//!   `POST /coord/edits/predict-and-check`; parses the response best-effort and
//!   stashes the `affected_tests` + `predicted_post_blob_shas` onto a shared
//!   slot carried by the context, keyed by one `correlation_id`.
//! - **Verify** (`spawn_verify`) — fired from the same context's `Drop` (right
//!   after the Ξ_FS observations push), under the same `correlation_id`. POSTs
//!   the realized HEAD sha + the stashed prediction fields to
//!   `POST /coord/edits/verify` and logs the composed verification outcome at
//!   INFO so an unattended loop's verdict is observable.
//!
//! `escalate_to_questions: true` on the predict body is the **ambient-loop
//! distinguisher** (plan Phase 4): an unattended loop whose `Escalate`
//! decisions nobody sees is silent drift, so the ambient caller asks coord to
//! materialize a `coord.agent_questions` row, whereas interactive
//! predict-and-check callers (skill agents already reporting to a coordinator)
//! stay advisory. The coord-side flag ships in a parallel PR; coord ignores the
//! unknown field until then (serde-default), so sending it early is safe.
//!
//! ## Gating
//!
//! Behind env flag [`EDIT_EFFECT_LOOP_FLAG_ENV`]
//! (`QONTINUI_EDIT_EFFECT_LOOP_ENABLED`), **default off** — same posture as
//! [`super::fs_observer::FS_OBSERVER_FLAG_ENV`]. The two flags flip together
//! when the loop proves quiet (plan §"Flag"): the loop's verify side reads the
//! same Ξ_FS observations the fs_observer producer pushes, so enabling verify
//! without the producer would verify against stale FS data.
//!
//! ## Degrade honestly / drop-safety
//!
//! Every coord call is best-effort and fire-and-forget — a coord failure
//! (unreachable, malformed repo, non-2xx) logs at `debug`/`warn` and returns;
//! it NEVER blocks or crashes the agent's edit path. The verify call fires from
//! `Drop`, so [`spawn_verify`] spawns onto the current Tokio runtime via
//! `Handle::try_current` (no blocking in `Drop`), copying
//! [`super::fs_observer::spawn_push_worktree_observations`]'s exact mechanism.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};

/// Env flag that turns the ambient edit-effect loop on. Default off. Accepts
/// the usual truthy values; anything else (including unset) is false. Matches
/// the truthy-set used by [`super::fs_observer::fs_observer_enabled`].
pub const EDIT_EFFECT_LOOP_FLAG_ENV: &str = "QONTINUI_EDIT_EFFECT_LOOP_ENABLED";

/// HTTP client timeout for the predict / verify calls. Same 10s budget the
/// fs_observer producer uses — generous enough for a cold coord, short enough
/// that a hung coord can't pin a session-teardown spawn for long.
const COORD_CALL_TIMEOUT_SECS: u64 = 10;

/// Returns true iff the ambient edit-effect loop is enabled. Default off.
pub fn edit_effect_loop_enabled() -> bool {
    matches!(
        std::env::var(EDIT_EFFECT_LOOP_FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// The prediction fields the verify side needs, stashed by [`spawn_predict`]
/// onto the context and read by [`spawn_verify`] at `Drop`. Carried behind an
/// `Arc<Mutex<Option<_>>>` so the fire-and-forget predict task can fill it
/// after the synchronous `acquire` returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StashedPrediction {
    /// The tests coord predicted this edit would affect (from the predict
    /// response's `tests` sub-prediction `detail.affected_tests`). Empty when
    /// coord couldn't predict them (the honest D4 gap) or the response was
    /// unparseable.
    pub affected_tests: Vec<String>,
    /// The post-image blob shas coord predicted per path (echoed from the FS
    /// sub-prediction `detail.post_blob_shas`). Empty when none were predicted.
    pub predicted_post_blob_shas: BTreeMap<String, String>,
}

/// The shared slot carried on [`super::isolated_edit::IsolatedEditContext`].
/// `None` until the predict task fills it (or forever, if predict is disabled
/// or the call failed — the verify side then sends empty prediction fields,
/// which coord treats as the post-hoc informational path).
pub type PredictionStash = Arc<Mutex<Option<StashedPrediction>>>;

/// Construct an empty prediction stash slot. The context creates one of these
/// unconditionally at construction; [`spawn_predict`] fills it iff the loop is
/// enabled.
pub fn new_stash() -> PredictionStash {
    Arc::new(Mutex::new(None))
}

/// Body of `POST /coord/edits/predict-and-check` — mirrors coord's
/// `PredictRequest` (`qontinui-coord/src/edit_effects.rs`) plus the ambient
/// loop's `escalate_to_questions` distinguisher. Field names are load-bearing
/// (coord deserializes exactly these keys; unknown keys are serde-ignored, so
/// `escalate_to_questions` is safe to send before coord adopts it).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PredictAndCheckRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub declared_globs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<uuid::Uuid>,
    /// The ambient-loop distinguisher (plan Phase 4): ask coord to materialize a
    /// `coord.agent_questions` row on `Escalate` instead of staying advisory.
    /// Always `true` from this runner caller.
    pub escalate_to_questions: bool,
}

/// Body of `POST /coord/edits/verify` — mirrors coord's `VerifyRequest`. Field
/// names are load-bearing. `tests_predicted` is the stashed `affected_tests`
/// (coord ignores it as an unknown field until its verify side reads it — fine,
/// per the same forward-compatible posture as `escalate_to_questions`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VerifyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests_predicted: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub predicted_post_blob_shas: BTreeMap<String, String>,
}

/// Derive a bare repo basename from a coord repo slug. Coord stores `repo` as
/// an optional basename; we send the leaf dir name (`owner/qontinui-runner` ⇒
/// `qontinui-runner`). Mirrors [`super::fs_observer::repo_basename`] — kept
/// local so the loop's request construction is self-contained + unit-testable.
fn repo_basename(repo_slug: &str) -> Option<String> {
    repo_slug
        .replace('\\', "/")
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Parse coord's `predict-and-check` response body into the fields the verify
/// side needs. Best-effort: any missing/mis-shaped field degrades to an empty
/// vec/map rather than failing the loop. `affected_tests` lives in the `tests`
/// sub-prediction's opaque `detail` (`predicted_effect.tests.detail.affected_tests`);
/// `predicted_post_blob_shas` echoes from the FS sub-prediction
/// (`predicted_effect.fs.detail.post_blob_shas`).
fn parse_prediction(body: &serde_json::Value) -> StashedPrediction {
    let effect = body.get("predicted_effect");

    let affected_tests = effect
        .and_then(|e| e.get("tests"))
        .and_then(|t| t.get("detail"))
        .and_then(|d| d.get("affected_tests"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let predicted_post_blob_shas = effect
        .and_then(|e| e.get("fs"))
        .and_then(|f| f.get("detail"))
        .and_then(|d| d.get("post_blob_shas"))
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    StashedPrediction {
        affected_tests,
        predicted_post_blob_shas,
    }
}

/// Read a worktree's current HEAD sha via libgit2 — used by the verify side to
/// report the realized post-edit commit. Returns `None` on any error (no repo,
/// unborn HEAD, detached state we can't resolve): the verify call then omits
/// `head_sha`, which coord tolerates (it's `Option` on the wire).
fn worktree_head_sha(worktree_path: &Path) -> Option<String> {
    let repo = git2::Repository::open(worktree_path).ok()?;
    let head = repo.head().ok()?;
    let oid = head.target()?;
    Some(oid.to_string())
}

/// POST the pre-built predict body to coord and return the parsed prediction.
/// Best-effort: any transport error or non-2xx logs and returns `None`.
async fn post_predict_and_check(
    coord_http_base: &str,
    body: &PredictAndCheckRequest,
) -> Option<StashedPrediction> {
    let url = format!(
        "{}/coord/edits/predict-and-check",
        coord_http_base.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(COORD_CALL_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("edit_effect_loop: build http client failed: {e}");
            return None;
        }
    };
    match client.post(&url).json(body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let txt = resp.text().await.unwrap_or_default();
                warn!(
                    url = %url,
                    status = status.as_u16(),
                    body = %txt,
                    "edit_effect_loop: predict-and-check non-2xx (soft failure)"
                );
                return None;
            }
            match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    let pred = parse_prediction(&v);
                    debug!(
                        url = %url,
                        affected_tests = pred.affected_tests.len(),
                        predicted_shas = pred.predicted_post_blob_shas.len(),
                        "edit_effect_loop: predict-and-check ok"
                    );
                    Some(pred)
                }
                Err(e) => {
                    debug!(url = %url, error = %e, "edit_effect_loop: predict response parse failed");
                    None
                }
            }
        }
        Err(e) => {
            // Coord unreachable is the expected degraded case — debug, not warn.
            debug!(url = %url, error = %e, "edit_effect_loop: predict failed (coord unreachable)");
            None
        }
    }
}

/// POST the pre-built verify body to coord and log the composed outcome. Best-
/// effort: any transport error or non-2xx logs and returns.
async fn post_verify(coord_http_base: &str, body: &VerifyRequest) {
    let url = format!(
        "{}/coord/edits/verify",
        coord_http_base.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(COORD_CALL_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("edit_effect_loop: build http client failed: {e}");
            return;
        }
    };
    match client.post(&url).json(body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let txt = resp.text().await.unwrap_or_default();
                warn!(
                    url = %url,
                    status = status.as_u16(),
                    body = %txt,
                    "edit_effect_loop: verify non-2xx (soft failure)"
                );
                return;
            }
            let composed = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("composed_outcome")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());
            info!(
                url = %url,
                correlation_id = ?body.correlation_id,
                composed_outcome = %composed,
                "edit_effect_loop: verify composed outcome"
            );
        }
        Err(e) => {
            debug!(url = %url, error = %e, "edit_effect_loop: verify failed (coord unreachable)");
        }
    }
}

/// Build the predict-and-check request body for one materialized worktree.
/// `declared_overlap_paths` feeds BOTH `paths` (the predicted touch set) and
/// `declared_globs` (the declared scope to check `paths ⊆ globs` against) —
/// they're the same declared set at acquisition time, before any edit has
/// happened. Pure for unit-testing.
fn build_predict_request(
    repo_slug: &str,
    parent_sha: &str,
    declared_overlap_paths: &[String],
    declared_intent: Option<&str>,
    correlation_id: uuid::Uuid,
) -> PredictAndCheckRequest {
    let paths: Vec<String> = declared_overlap_paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .collect();
    PredictAndCheckRequest {
        repo: repo_basename(repo_slug),
        declared_globs: paths.clone(),
        paths,
        head_sha: Some(parent_sha.to_string()),
        declared_intent: declared_intent.map(|s| s.to_string()),
        correlation_id: Some(correlation_id),
        escalate_to_questions: true,
    }
}

/// Build the verify request body from the stashed prediction + the realized
/// worktree HEAD. Pure for unit-testing.
fn build_verify_request(
    repo_slug: &str,
    declared_overlap_paths: &[String],
    head_sha: Option<String>,
    correlation_id: uuid::Uuid,
    stash: &StashedPrediction,
) -> VerifyRequest {
    let paths: Vec<String> = declared_overlap_paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .cloned()
        .collect();
    VerifyRequest {
        repo: repo_basename(repo_slug),
        paths,
        correlation_id: Some(correlation_id),
        head_sha,
        tests_predicted: stash.affected_tests.clone(),
        predicted_post_blob_shas: stash.predicted_post_blob_shas.clone(),
    }
}

/// **Predict** side of the ambient loop. Fire-and-forget: spawns a best-effort
/// task that POSTs `predict-and-check` for one materialized worktree and stashes
/// the result for the verify side. No-op when the loop is disabled or there's no
/// Tokio runtime (so call sites stay unconditional one-liners).
///
/// Called from BOTH [`super::isolated_edit::IsolatedEditContext::acquire`] and
/// `acquire_for_terminal` after a successful materialization, once per per-repo
/// worktree, sharing one `correlation_id` with the eventual verify call.
#[allow(clippy::too_many_arguments)]
pub fn spawn_predict(
    coord_http_base: String,
    repo_slug: String,
    parent_sha: String,
    declared_overlap_paths: Vec<String>,
    declared_intent: Option<String>,
    correlation_id: uuid::Uuid,
    stash: PredictionStash,
) {
    if !edit_effect_loop_enabled() {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        debug!("edit_effect_loop: no tokio runtime — skipping predict");
        return;
    }
    tokio::spawn(async move {
        let body = build_predict_request(
            &repo_slug,
            &parent_sha,
            &declared_overlap_paths,
            declared_intent.as_deref(),
            correlation_id,
        );
        if let Some(pred) = post_predict_and_check(&coord_http_base, &body).await {
            if let Ok(mut slot) = stash.lock() {
                *slot = Some(pred);
            }
        }
    });
}

/// **Verify** side of the ambient loop. Fire-and-forget: spawns a best-effort
/// task that reads the worktree's realized HEAD, pulls the stashed prediction,
/// and POSTs `verify`. No-op when the loop is disabled or there's no Tokio
/// runtime — so the `Drop` call site stays an unconditional one-liner and is
/// drop-safe (never blocks in `Drop`; spawns onto the current runtime exactly
/// like [`super::fs_observer::spawn_push_worktree_observations`]).
///
/// Called from [`super::isolated_edit::IsolatedEditContext`]'s `Drop`, after the
/// Ξ_FS observations push, under the SAME `correlation_id` so the prediction and
/// its verification tie together in coord.
pub fn spawn_verify(
    coord_http_base: String,
    worktree_path: PathBuf,
    repo_slug: String,
    declared_overlap_paths: Vec<String>,
    correlation_id: uuid::Uuid,
    stash: PredictionStash,
) {
    if !edit_effect_loop_enabled() {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        debug!("edit_effect_loop: no tokio runtime in drop context — skipping verify");
        return;
    }
    tokio::spawn(async move {
        let head_sha = worktree_head_sha(&worktree_path);
        let stashed = stash
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_default();
        let body = build_verify_request(
            &repo_slug,
            &declared_overlap_paths,
            head_sha,
            correlation_id,
            &stashed,
        );
        post_verify(&coord_http_base, &body).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- flag parsing -----------------------------------------------------

    #[test]
    fn enabled_is_false_by_default() {
        // Holds as long as no concurrent test set the flag (env is global; we
        // don't mutate it here to stay parallel-safe — mirrors fs_observer's
        // `enabled_is_false_by_default`).
        if std::env::var(EDIT_EFFECT_LOOP_FLAG_ENV).is_err() {
            assert!(!edit_effect_loop_enabled());
        }
    }

    // ---- repo basename ----------------------------------------------------

    #[test]
    fn repo_basename_strips_owner() {
        assert_eq!(
            repo_basename("qontinui/qontinui-runner").as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(
            repo_basename("qontinui-runner").as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(repo_basename("").as_deref(), None);
    }

    // ---- predict request construction -------------------------------------

    #[test]
    fn predict_request_serializes_to_coord_contract() {
        let cid = uuid::Uuid::nil();
        let req = build_predict_request(
            "qontinui/qontinui-runner",
            "abc123",
            &[
                "src/a.rs".to_string(),
                "  ".to_string(),
                "src/b.rs".to_string(),
            ],
            Some("refactor the loop"),
            cid,
        );
        // Blank-only paths are dropped; paths feed both paths + declared_globs.
        assert_eq!(req.paths, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(req.declared_globs, vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(req.repo.as_deref(), Some("qontinui-runner"));
        assert_eq!(req.head_sha.as_deref(), Some("abc123"));
        assert!(req.escalate_to_questions, "ambient loop always escalates");

        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["repo"], "qontinui-runner");
        assert_eq!(v["head_sha"], "abc123");
        assert_eq!(v["declared_intent"], "refactor the loop");
        assert_eq!(v["escalate_to_questions"], true);
        let paths = v["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        let globs = v["declared_globs"].as_array().unwrap();
        assert_eq!(globs.len(), 2);
    }

    #[test]
    fn predict_request_omits_empty_optionals() {
        let req = build_predict_request("repo", "sha", &[], None, uuid::Uuid::nil());
        let v = serde_json::to_value(&req).expect("serialize");
        // paths always present (required by coord); empty globs/intent omitted.
        assert!(v.get("paths").is_some());
        assert!(v["paths"].as_array().unwrap().is_empty());
        assert!(v.get("declared_globs").is_none());
        assert!(v.get("declared_intent").is_none());
        // escalate_to_questions is non-skipped — always on the wire.
        assert_eq!(v["escalate_to_questions"], true);
    }

    // ---- verify request construction --------------------------------------

    #[test]
    fn verify_request_serializes_to_coord_contract() {
        let cid = uuid::Uuid::nil();
        let stash = StashedPrediction {
            affected_tests: vec!["tests::a".to_string(), "tests::b".to_string()],
            predicted_post_blob_shas: [("src/a.rs".to_string(), "deadbeef".to_string())]
                .into_iter()
                .collect(),
        };
        let req = build_verify_request(
            "owner/qontinui-runner",
            &["src/a.rs".to_string()],
            Some("newhead".to_string()),
            cid,
            &stash,
        );
        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["repo"], "qontinui-runner");
        assert_eq!(v["head_sha"], "newhead");
        assert_eq!(v["paths"].as_array().unwrap().len(), 1);
        assert_eq!(v["tests_predicted"].as_array().unwrap().len(), 2);
        assert_eq!(v["predicted_post_blob_shas"]["src/a.rs"], "deadbeef");
    }

    #[test]
    fn verify_request_omits_empty_prediction_fields() {
        // No prediction stashed (predict disabled / failed) ⇒ post-hoc path:
        // tests_predicted + predicted_post_blob_shas omitted; head_sha omitted.
        let req = build_verify_request(
            "repo",
            &["src/a.rs".to_string()],
            None,
            uuid::Uuid::nil(),
            &StashedPrediction::default(),
        );
        let v = serde_json::to_value(&req).expect("serialize");
        assert!(v.get("tests_predicted").is_none());
        assert!(v.get("predicted_post_blob_shas").is_none());
        assert!(v.get("head_sha").is_none());
        assert!(v.get("paths").is_some());
    }

    // ---- response parsing -------------------------------------------------

    #[test]
    fn parse_prediction_extracts_tests_and_shas() {
        // Mirrors coord's PredictAndCheckResponse → predicted_effect shape.
        let body = serde_json::json!({
            "predicted_effect": {
                "fs": {
                    "coverage": 1.0,
                    "detail": {
                        "modified_paths": ["src/a.rs"],
                        "post_blob_shas": { "src/a.rs": "abc", "src/b.rs": "def" }
                    }
                },
                "tests": {
                    "coverage": 1.0,
                    "detail": { "affected_tests": ["tests::x", "tests::y"] }
                }
            },
            "resolution": { "Decision": {} },
            "risk_factors": []
        });
        let pred = parse_prediction(&body);
        assert_eq!(pred.affected_tests, vec!["tests::x", "tests::y"]);
        assert_eq!(
            pred.predicted_post_blob_shas
                .get("src/a.rs")
                .map(|s| s.as_str()),
            Some("abc")
        );
        assert_eq!(
            pred.predicted_post_blob_shas
                .get("src/b.rs")
                .map(|s| s.as_str()),
            Some("def")
        );
    }

    #[test]
    fn parse_prediction_degrades_to_empty_on_unknown_shape() {
        // The honest D4 gap: coord returns `tests: unknown` (no affected_tests).
        let body = serde_json::json!({
            "predicted_effect": {
                "fs": { "coverage": 1.0, "detail": { "post_blob_shas": {} } },
                "tests": { "coverage": 0.0, "detail": { "status": "unknown" } }
            }
        });
        let pred = parse_prediction(&body);
        assert!(pred.affected_tests.is_empty());
        assert!(pred.predicted_post_blob_shas.is_empty());

        // Totally malformed body ⇒ all-empty, never a panic.
        let pred2 = parse_prediction(&serde_json::json!({ "garbage": 1 }));
        assert_eq!(pred2, StashedPrediction::default());
    }

    // ---- stash round-trip -------------------------------------------------

    #[test]
    fn stash_round_trips_through_shared_slot() {
        let stash = new_stash();
        assert!(stash.lock().unwrap().is_none(), "fresh stash is empty");

        let pred = StashedPrediction {
            affected_tests: vec!["tests::z".to_string()],
            predicted_post_blob_shas: [("p".to_string(), "s".to_string())].into_iter().collect(),
        };
        // Predict side fills it.
        *stash.lock().unwrap() = Some(pred.clone());

        // Verify side reads it back (clone, the same op spawn_verify does).
        let read = stash.lock().unwrap().clone();
        assert_eq!(read, Some(pred));

        // Verify side's unwrap_or_default when never filled ⇒ empty prediction.
        let empty = new_stash();
        let read_empty = empty.lock().unwrap().clone().unwrap_or_default();
        assert_eq!(read_empty, StashedPrediction::default());
    }
}
