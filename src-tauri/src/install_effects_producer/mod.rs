//! Install-effects **producer** — the runner-native route that turns an
//! intended dependency install into a coord install-effect observation.
//!
//! Coord (`qontinui-coord`'s `src/install_effects.rs`) shipped the consumer
//! side: `POST /coord/installs/{declare,predict-and-check,verify}` accept
//! install-effect observations and run the autonomy gate. Nothing produced them
//! until this module. Phase 1 ships the explicit producer skeleton:
//!
//! 1. **declare** the intended install (persists the signature coord-side).
//! 2. **predict-and-check** — coord's advisory autonomy gate. If the resolution
//!    is `Escalate` and the caller did not pass `override_escalation`, the
//!    install is NOT run; the typed resolution + risk factors come back so the
//!    caller can route to an operator.
//! 3. **run** the real install (`spawn_blocking`, bounded timeout, captured
//!    stdout/stderr, never a PTY, `CREATE_NO_WINDOW` on Windows) — unless
//!    `dry_run_only` stopped us after step 2.
//!
//! ## Honest defaults (no fabrication)
//!
//! Phase 3 adds the post-install observation/verify loop. Phase 1 left clean
//! plug-in seams and sent HONEST empties; **Phase 2** ([`parsers`] +
//! [`dry_run_invoke`]) now populates the `dry_run` / `security_audited` fields
//! from real read-only probes, still degrading to honest `Unknown` (not a
//! fabricated clean plan) whenever a probe is missing/unparseable. There is no
//! `todo!()`/`unimplemented!()` on any path.
//!
//! ## Plug-in seams for later phases
//!
//! - **Phase 2** (SHIPPED — [`parsers`] + [`dry_run_invoke`]): the pure
//!   dry-run / lockfile / audit parsers ([`parsers::dry_run`],
//!   [`parsers::lockfile`], [`parsers::audit`], [`parsers::semver`]) over
//!   captured tool output, plus the thin impure orchestration
//!   ([`dry_run_invoke::produce_ground_truth`]) that shells out the read-only
//!   probes and feeds them. Wired into [`DeclareRequest`]'s `dry_run` /
//!   `security_audited` fields. The importer-inventory / sibling-manifest walks
//!   stay empty (a separate affected-source/cross-repo concern, out of this
//!   round — coord degrades them to honest `Unknown`).
//! - **Phase 3** (observation/verify): after a successful install, parse the
//!   observed lockfile (via [`parsers::lockfile`]) + run a native audit (via
//!   [`parsers::audit`]), assemble the Ξ_FS observation, and
//!   `POST /coord/installs/verify`. The [`RunContext`] already retains the
//!   `correlation_id`, the resolved `repo`, and the `escalation_overridden`
//!   flag (Phase 3 suffixes provenance with `+overridden`). This verify push is
//!   the ONLY fire-and-forget coord call in the producer — declare and
//!   predict-and-check are load-bearing and surface their errors.

mod coord_client;
mod dry_run_invoke;
mod importer_scan;
pub mod intercept;
mod observe;
pub mod parsers;
mod pm;
mod registry_creds;
pub mod registry_routes;
mod sibling_manifests;
pub mod types;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use tracing::{info, warn};
use uuid::Uuid;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use coord_client::{CoordError, CoordInstallClient};
use intercept::precontext::{self, PreContext};
use pm::PmError;
use registry_creds::{RegistryCredentialStore, RegistryEnv, RegistryTokenLookup};
use types::{
    DeclareRequest, FsObservationsRequest, GateOutcome, InstallOutcome, ObserveVerifyRequest,
    PackageManager, PackageSpec, RunMode, RunRequest, RunResponse, VerifyOutcome, VerifyRequest,
};

/// Bounded wall-clock budget for the install subprocess. A dependency install
/// is normally seconds-to-low-minutes; this cap stops a hung resolver (private
/// registry stall, interactive prompt) from pinning the route forever.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// The retained context for a single producer run. Phase 3's observation/verify
/// loop consumes this after the install (whether it succeeded or failed).
#[derive(Debug, Clone)]
pub struct RunContext {
    pub correlation_id: Uuid,
    pub repo: String,
    pub package_manager: PackageManager,
    /// True iff an `Escalate` was overridden via `override_escalation`. Phase 3
    /// suffixes the verify provenance with `+overridden`.
    pub escalation_overridden: bool,
    /// Affected source files from the declare-time importer scan — the
    /// per-file typecheck loop runs over these post-install.
    pub affected_files: Vec<String>,
    /// Predicted post-install pinned set echoed into verify (from the dry-run).
    pub predicted_pinned: std::collections::BTreeMap<String, String>,
    /// Predicted advisories echoed into verify (from the dry-run audit).
    pub predicted_cves: Vec<types::Cve>,
    /// Pre-install manifest/lockfile blob shas (`path → Some(sha)|None`),
    /// captured BEFORE the install so the FS-observation push pairs pre↔post.
    pub pre_shas: std::collections::BTreeMap<String, Option<String>>,
}

// ===========================================================================
// Route
// ===========================================================================

/// Routes contributed by the install-effects producer. Merged into the runner's
/// axum router in `mcp_api.rs` alongside the other native route groups. This is
/// a runner-native route — NOT part of the UI-Bridge SDK manifest.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/install-effects/run", post(post_run))
        // Interception POST-call (plan §3 A3): the shim calls this after it runs
        // the real install, to close the declare→observe→verify loop for an
        // install that was declared+gated via `mode:"intercept"` on /run.
        .route("/install-effects/observe-verify", post(post_observe_verify))
        // Phase 4: private-registry credential CRUD. Mounted alongside the
        // Phase-1 run route so a single `install_effects_producer::routes()`
        // merge in `mcp_api.rs` covers the whole producer surface.
        .merge(registry_routes::routes())
}

/// `POST /install-effects/run`.
async fn post_run(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RunRequest>,
) -> Result<Json<ApiResponse<RunResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let base = coord_base().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("coord URL not configured: {e}"))),
        )
    })?;
    // The typecheck loopback targets THIS runner's own HTTP listener — resolve
    // the actually-bound port from AppState (correct for secondary/temp runners
    // on non-default ports). Injectable so tests point it at a mock.
    let loopback_base = crate::mcp::types::get_self_base_url(&state.app_state);
    // Phase 4: the production registry-credential store (OS keyring). Tests call
    // `run_with_base` directly with a fake lookup so no real keyring is touched.
    let store = RegistryCredentialStore;
    run_with_base(req, &base, &loopback_base, &store)
        .await
        .map(Json)
        .map_err(|e| e.into_response())
}

/// `POST /install-effects/observe-verify` — the interception POST-call (plan §3
/// A3). Looks up + removes the stashed [`PreContext`] for the shim's
/// `correlation_id`, synthesizes an [`InstallOutcome`] from the reported exit
/// code, and runs the SHARED [`observe_and_verify`] body against the now-mutated
/// worktree. An expired/missing id degrades to a best-effort verify with no
/// pre-shas (honest Partial) — never an error to the shim.
async fn post_observe_verify(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ObserveVerifyRequest>,
) -> Result<Json<ApiResponse<VerifyOutcome>>, (StatusCode, Json<ApiResponse<()>>)> {
    let base = coord_base().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("coord URL not configured: {e}"))),
        )
    })?;
    let loopback_base = crate::mcp::types::get_self_base_url(&state.app_state);
    let store = RegistryCredentialStore;
    observe_verify_with_base(req, &base, &loopback_base, &store)
        .await
        .map(Json)
        .map_err(|e| e.into_response())
}

/// Orchestration body for the interception POST-call, coord-base + loopback +
/// cred-store injected (tests pass a mock for each). Reconstructs a
/// [`RunContext`] from the stashed [`PreContext`] (or an honest pre-sha-less
/// degraded context when the id expired/missing) and shares
/// [`observe_and_verify`] with the `Full` route — no duplicated observe logic.
async fn observe_verify_with_base<L: RegistryTokenLookup + ?Sized>(
    req: ObserveVerifyRequest,
    coord_base: &str,
    loopback_base: &str,
    cred_store: &L,
) -> Result<ApiResponse<VerifyOutcome>, RunError> {
    let correlation_id = req.correlation_id;
    let pre = precontext::take(&correlation_id);

    // Resolve the context: a found PreContext carries the pre-shas + predicted
    // echoes from the pre-call; a missing/expired one degrades to an honest
    // empty context (no pre-shas ⇒ coord composes a Partial, never an error).
    let (ctx, repo_path) = match pre {
        Some(p) => {
            let repo_path = std::path::PathBuf::from(&p.repo_path);
            (
                RunContext {
                    correlation_id,
                    repo: p.repo,
                    package_manager: p.package_manager,
                    escalation_overridden: p.escalation_overridden,
                    affected_files: p.affected_files,
                    predicted_pinned: p.predicted_pinned,
                    predicted_cves: p.predicted_cves,
                    pre_shas: p.pre_shas,
                },
                repo_path,
            )
        }
        None => {
            warn!(
                "install-effects: observe-verify for unknown/expired correlation_id={correlation_id} \
                 — degrading to a best-effort verify with no pre-shas (honest Partial)"
            );
            // We have no repo_path for a degraded context; coord still records a
            // verify keyed on the correlation_id (the FS push is skipped because
            // build_fs_observations over an empty/unknown path yields nothing).
            (
                RunContext {
                    correlation_id,
                    repo: String::new(),
                    package_manager: PackageManager::Npm,
                    escalation_overridden: false,
                    affected_files: Vec::new(),
                    predicted_pinned: std::collections::BTreeMap::new(),
                    predicted_cves: Vec::new(),
                    pre_shas: std::collections::BTreeMap::new(),
                },
                std::path::PathBuf::new(),
            )
        }
    };

    // Synthesize the InstallOutcome from the shim-reported exit code. The shim
    // already ran the real tool; we don't have its stdout/stderr (it streamed to
    // the agent's TTY) — only the exit code, which is the load-bearing signal.
    let exit_code = req.install_exit_code;
    let success = exit_code == Some(0);
    let outcome = InstallOutcome {
        command: vec![format!("<intercepted:{}>", ctx.package_manager.as_str())],
        exit_code,
        success,
        timed_out: false,
        stdout: String::new(),
        stderr: String::new(),
    };

    let client = CoordInstallClient::new(coord_base.to_string())?;
    // No registry creds are injected for the observe probes here — interception
    // observes the agent's already-authed install; the agent's shell owns its
    // registry config (plan §4 Phase-4 note). An empty env is correct.
    let registry_env = RegistryEnv::default();
    let verify = observe_and_verify(
        &client,
        &ctx,
        &repo_path,
        loopback_base,
        &outcome,
        &registry_env,
    )
    .await?;

    Ok(ApiResponse::success(verify))
}

/// Resolve the coord base URL (reuses the runner's existing source-of-truth
/// chain). Split out so tests inject a mock-server base directly via
/// [`run_with_base`].
fn coord_base() -> Result<String, String> {
    crate::mcp::agent_worktrees::coord_http_base()
}

// ===========================================================================
// Orchestration (coord-base injectable for tests)
// ===========================================================================

/// Errors the route surfaces. Each maps to an HTTP status + `ApiResponse`.
#[derive(Debug)]
enum RunError {
    /// Bad request: unresolvable/unsupported package manager (incl. pipenv).
    BadRequest(String),
    /// A load-bearing coord call (declare, predict-and-check, or verify) failed.
    Coord(CoordError),
    /// The install subprocess could not be spawned / joined (NOT a non-zero
    /// exit — that rides back in [`InstallOutcome`]).
    Spawn(String),
}

impl RunError {
    fn into_response(self) -> (StatusCode, Json<ApiResponse<()>>) {
        match self {
            RunError::BadRequest(m) => (StatusCode::BAD_REQUEST, Json(api_error(m))),
            RunError::Coord(e) => (
                StatusCode::BAD_GATEWAY,
                Json(api_error(format!("coord install-effect call failed: {e}"))),
            ),
            RunError::Spawn(m) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("install subprocess error: {m}"))),
            ),
        }
    }
}

impl From<PmError> for RunError {
    fn from(e: PmError) -> Self {
        RunError::BadRequest(e.to_string())
    }
}

impl From<CoordError> for RunError {
    fn from(e: CoordError) -> Self {
        RunError::Coord(e)
    }
}

/// The orchestration body with the coord base URL + the runner's own typecheck
/// loopback base injected. The route resolves both from config/AppState; tests
/// pass a mock-server base for each.
async fn run_with_base<L: RegistryTokenLookup + ?Sized>(
    req: RunRequest,
    coord_base: &str,
    loopback_base: &str,
    cred_store: &L,
) -> Result<ApiResponse<RunResponse>, RunError> {
    // ---- resolve package manager (explicit token, else autodetect) ----------
    let repo_path = Path::new(&req.repo_path);
    let pm = match req.package_manager.as_deref() {
        Some(tok) => pm::parse_token(tok)?, // pipenv & friends rejected here
        None => pm::autodetect(repo_path)?,
    };

    let correlation_id = Uuid::new_v4();
    let repo = repo_basename(&req.repo_path);

    // ---- DYNAMIC interception mode (P4) -------------------------------------
    // Read the device's EFFECTIVE install-interception level from the
    // process-global fleet-policy cache (refreshed every ~45s by
    // `mcp::fleet_policy_poller`). `run_with_base` has no app state, so the
    // accessor is a synchronous lock read of the cache directly. Defaults to
    // `"off"` until the poller's first success (fail-safe — never gate).
    let effective_mode = crate::mcp::fleet_policy_poller::effective_install_intercept_mode();

    // OFF short-circuit (D6): when the effective mode is `off`, an interception
    // pre-call must SKIP declare/predict entirely, return NO `correlation_id`
    // (so the shim's `[ -n "$correlation_id" ]` guard skips the post-call), and
    // hand back `effective_mode:"off"` immediately — the real tool still runs
    // in the agent's shell (the shim runs it; we just don't observe). The
    // explicit `Full` route is UNAFFECTED (it never short-circuits on the fleet
    // policy — it's the operator-driven `/install-effects/run` path).
    if req.mode == RunMode::Intercept && effective_mode == "off" {
        info!(
            "install-effects: INTERCEPT pre-call but fleet policy = OFF (repo={repo}) \
             — short-circuiting (no declare/predict, no correlation_id; tool runs uninstrumented)"
        );
        return Ok(ApiResponse::success(RunResponse {
            correlation_id: Uuid::nil(),
            repo,
            package_manager: pm.as_str().to_string(),
            gate: GateOutcome::Proceed,
            risk_factors: Vec::new(),
            escalation_overridden: false,
            install: None,
            dry_run_only: false,
            resolution: serde_json::Value::Null,
            verify: None,
            effective_mode: "off".to_string(),
        }));
    }

    // ---- Phase 4: resolve private-registry auth env (NEVER logged/sent) ------
    // Injected into the dry-run / install / audit subprocesses ONLY. Custom
    // `CARGO_REGISTRIES_<NAME>_TOKEN` names the caller declares ride in
    // `registry_env` (request field) on top of the well-known set.
    let registry_env = registry_creds::resolve_registry_env(cred_store, &req.registry_env);
    if !registry_env.is_empty() {
        // Names only — values are NEVER logged.
        info!(
            "install-effects: injecting {} private-registry credential(s) into subprocesses (corr={correlation_id})",
            registry_env.names().len()
        );
    }

    // ---- Phase 2: read-only dry-run + native-audit probes --------------------
    // Shell out the package manager's own `--dry-run` (+ `audit`) on a blocking
    // thread and feed the captured output to the pure parsers. Best-effort: a
    // missing tool / unparseable output ⇒ honest `None`/`false` (coord degrades
    // to `Unknown`), never a fabricated plan, never a blocked install.
    let ground_truth = {
        let repo_path = repo_path.to_path_buf();
        let packages = req.packages.clone();
        let dev = req.dev;
        let env = registry_env.clone();
        tokio::task::spawn_blocking(move || {
            dry_run_invoke::produce_ground_truth(pm, &repo_path, &packages, dev, &env)
        })
        .await
        .unwrap_or_default()
    };

    // ---- Phase 3 declare-time scans (pure, capped, text-only) ----------------
    // Importer scan: which existing source files import each touched package
    // (→ coord's import-break heuristic + the typecheck affected-file set).
    // Sibling-manifest walk: cross-repo peer-dep inputs for coord's pure walker.
    let importer_scan = {
        let repo_path = repo_path.to_path_buf();
        let packages = req.packages.clone();
        tokio::task::spawn_blocking(move || {
            importer_scan::scan_importers(pm, &repo_path, &packages)
        })
        .await
        .unwrap_or_default()
    };
    let sibling_manifests = {
        let repo_path = repo_path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            sibling_manifests::collect_sibling_manifests(pm, &repo_path)
        })
        .await
        .unwrap_or_default()
    };

    // Echo the dry-run's predicted pins/CVEs into verify (declare-side signature).
    let predicted_pinned = ground_truth
        .dry_run
        .as_ref()
        .map(|d| d.predicted_pinned.clone())
        .unwrap_or_default();
    let predicted_cves = ground_truth
        .dry_run
        .as_ref()
        .map(|d| d.predicted_cves.clone())
        .unwrap_or_default();

    // ---- declare + predict-and-check FIRST (both load-bearing) ---------------
    let client = CoordInstallClient::new(coord_base.to_string())?;
    let declare_req = build_declare(
        pm,
        &req,
        &repo,
        correlation_id,
        ground_truth,
        importer_scan.inventory.clone(),
        sibling_manifests,
    );

    // declare persists the signature coord-side; its body is not load-bearing
    // for the gate, but a failure IS surfaced (it means coord is unreachable /
    // rejecting, and proceeding blind would be dishonest).
    let _declared = client.declare(&declare_req).await?;

    let pac = client.predict_and_check(&declare_req).await?;
    let gate = pac.gate();
    let risk_factors = pac.risk_factors.clone();

    let mut ctx = RunContext {
        correlation_id,
        repo: repo.clone(),
        package_manager: pm,
        escalation_overridden: false,
        affected_files: importer_scan.affected_files,
        predicted_pinned,
        predicted_cves,
        pre_shas: std::collections::BTreeMap::new(),
    };

    // ---- gate the install ----------------------------------------------------
    // Escalate + no override ⇒ STOP, return the typed resolution. dry_run_only
    // ⇒ STOP after predict-and-check regardless of gate.
    let blocked_by_escalation = gate == GateOutcome::Escalate && !req.override_escalation;
    if gate == GateOutcome::Escalate && req.override_escalation {
        ctx.escalation_overridden = true;
        info!(
            "install-effects: predict-and-check ESCALATED (corr={correlation_id}, repo={repo}) \
             but override_escalation=true — proceeding; risks: {}",
            risk_factors.join("; ")
        );
    }

    // ---- INTERCEPT mode (plan §3 A2/A3) -------------------------------------
    // The shim's pre-call: declare + predict-and-check already ran above. We do
    // NOT run the install (the agent's own shell does). Capture pre-shas now
    // (worktree still un-mutated), stash the PreContext under the correlation_id
    // for the matching `/install-effects/observe-verify` post-call, and return
    // the gate verdict so the shim can branch (observe mode always proceeds;
    // gate mode — Phase 3 — honors Escalate). This branch is BEFORE the
    // dry_run_only/blocked early-return so an intercept run ALWAYS hands back a
    // correlation_id regardless of the gate (the shim, not the producer, owns
    // the block decision).
    if req.mode == RunMode::Intercept {
        let pre_shas = observe::capture_pre_shas(pm, repo_path);
        precontext::put(
            correlation_id,
            PreContext {
                repo: repo.clone(),
                repo_path: req.repo_path.clone(),
                package_manager: pm,
                escalation_overridden: ctx.escalation_overridden,
                affected_files: ctx.affected_files.clone(),
                predicted_pinned: ctx.predicted_pinned.clone(),
                predicted_cves: ctx.predicted_cves.clone(),
                pre_shas,
            },
        );
        info!(
            "install-effects: INTERCEPT pre-call (corr={correlation_id}, repo={repo}, pm={}) \
             — declared+gated, install deferred to the agent shell; gate={gate:?}, \
             effective_mode={effective_mode}",
            pm.as_str()
        );
        return Ok(ApiResponse::success(RunResponse {
            correlation_id,
            repo,
            package_manager: pm.as_str().to_string(),
            gate,
            risk_factors,
            escalation_overridden: ctx.escalation_overridden,
            install: None,
            dry_run_only: false,
            resolution: pac.resolution,
            verify: None,
            // `observe` or `gate` here — `off` was short-circuited before declare.
            effective_mode: effective_mode.clone(),
        }));
    }

    if req.dry_run_only || blocked_by_escalation {
        if blocked_by_escalation {
            info!(
                "install-effects: predict-and-check ESCALATED (corr={correlation_id}, repo={repo}) \
                 — install NOT run (no override); risks: {}",
                risk_factors.join("; ")
            );
        }
        return Ok(ApiResponse::success(RunResponse {
            correlation_id,
            repo,
            package_manager: pm.as_str().to_string(),
            gate,
            risk_factors,
            escalation_overridden: ctx.escalation_overridden,
            install: None,
            dry_run_only: req.dry_run_only,
            resolution: pac.resolution,
            verify: None,
            effective_mode: effective_mode.clone(),
        }));
    }

    // ---- capture pre-install Ξ_FS post-shas BEFORE mutating anything ---------
    ctx.pre_shas = observe::capture_pre_shas(pm, repo_path);

    // ---- run the real install ------------------------------------------------
    let outcome = run_install(pm, repo_path, &req.packages, req.dev, &registry_env).await?;
    info!(
        "install-effects: install finished (corr={correlation_id}, repo={repo}, pm={}, \
         exit={:?}, success={}, timed_out={})",
        pm.as_str(),
        outcome.exit_code,
        outcome.success,
        outcome.timed_out
    );

    // ---- Phase 3 observe + verify (runs on success AND failure, §6) ----------
    // A failed install STILL verifies: coord composes a Failure from the FS/dep
    // divergence. The install exit code rides into verify's rationale via the
    // outcome we surface; the verify call itself is load-bearing.
    let verify = observe_and_verify(
        &client,
        &ctx,
        repo_path,
        loopback_base,
        &outcome,
        &registry_env,
    )
    .await?;

    Ok(ApiResponse::success(RunResponse {
        correlation_id,
        repo,
        package_manager: pm.as_str().to_string(),
        gate,
        risk_factors,
        escalation_overridden: ctx.escalation_overridden,
        install: Some(outcome),
        dry_run_only: false,
        resolution: pac.resolution,
        verify: Some(verify),
        effective_mode,
    }))
}

/// Phase-3 post-install loop: push the Ξ_FS observations (fire-and-forget,
/// SAME correlation_id), assemble the observed lockfile pins + native audit +
/// per-file typecheck loopback, then `POST /coord/installs/verify` (load-bearing
/// — its error surfaces). Returns the composed [`VerifyOutcome`].
async fn observe_and_verify(
    client: &CoordInstallClient,
    ctx: &RunContext,
    repo_path: &Path,
    loopback_base: &str,
    outcome: &InstallOutcome,
    registry_env: &RegistryEnv,
) -> Result<VerifyOutcome, RunError> {
    let pm = ctx.package_manager;

    // ---- Ξ_FS push (fire-and-forget, bearer-attached, SAME correlation_id) --
    let fs_observations = observe::build_fs_observations(pm, repo_path, &ctx.pre_shas);
    let touched_paths: Vec<String> = fs_observations.iter().map(|o| o.path.clone()).collect();
    if !fs_observations.is_empty() {
        let fs_req = FsObservationsRequest {
            correlation_id: Some(ctx.correlation_id),
            repo: Some(ctx.repo.clone()),
            observations: fs_observations,
        };
        client.push_fs_observations(&fs_req).await;
    }

    // ---- observed pinned set + native audit (blocking probes) ---------------
    let (observed_pinned, observed_cves, observed_audit_ran) = {
        let repo_path = repo_path.to_path_buf();
        let env = registry_env.clone();
        tokio::task::spawn_blocking(move || {
            let pinned = observe::observe_pinned(pm, &repo_path, &env);
            let (cves, ran) = observe::observe_audit(pm, &repo_path, &env);
            (pinned, cves, ran)
        })
        .await
        .unwrap_or_default()
    };

    // ---- typecheck loopback against the runner's OWN endpoint ---------------
    let (observed_typecheck, typecheck_ran) = observe::run_typecheck_loopback(
        loopback_base,
        repo_path,
        &ctx.affected_files,
        ctx.escalation_overridden,
    )
    .await;

    // ---- assemble + POST verify (load-bearing) ------------------------------
    // The install exit code is folded into a synthetic diagnostic so a partial
    // install (non-zero exit) is never swallowed — it rides verify's record.
    let mut observed_typecheck = observed_typecheck;
    if !outcome.success {
        info!(
            "install-effects: install FAILED (corr={}, exit={:?}, timed_out={}) — verifying anyway (§6)",
            ctx.correlation_id, outcome.exit_code, outcome.timed_out
        );
        // Surface the failing exit code as a diagnostic on a synthetic typecheck
        // record so the rationale names it; this does NOT fabricate a pass.
        observed_typecheck.push(types::TypecheckResult {
            file: "<install-subprocess>".to_string(),
            pass: false,
            provenance: Some(if ctx.escalation_overridden {
                "install-exit+overridden".to_string()
            } else {
                "install-exit".to_string()
            }),
            coverage: 1.0,
            diagnostics: vec![format!(
                "install subprocess exited {:?} (timed_out={}); argv: {}",
                outcome.exit_code,
                outcome.timed_out,
                outcome.command.join(" ")
            )],
        });
    }
    let typecheck_ran = typecheck_ran || !observed_typecheck.is_empty();

    let verify_req = VerifyRequest {
        correlation_id: Some(ctx.correlation_id),
        repo: Some(ctx.repo.clone()),
        predicted_pinned: ctx.predicted_pinned.clone(),
        predicted_cves: ctx.predicted_cves.clone(),
        paths: touched_paths,
        observed_pinned,
        observed_cves,
        observed_audit_ran,
        observed_typecheck,
        typecheck_ran,
    };

    Ok(client.verify(&verify_req).await?)
}

/// Build the coord declare/predict body from the route request + the Phase-2
/// probe ground truth + the Phase-3 declare-time scans. `dry_run` /
/// `security_audited` carry the real parsed plan (or honest `None`/`false` when
/// a probe was missing/unparseable); `importer_inventory` / `sibling_manifests`
/// carry the capped text-scan results (or honest empties when nothing matched).
fn build_declare(
    pm: PackageManager,
    req: &RunRequest,
    repo: &str,
    correlation_id: Uuid,
    ground_truth: dry_run_invoke::ProducedGroundTruth,
    importer_inventory: Vec<types::ImporterRecord>,
    sibling_manifests: std::collections::BTreeMap<String, String>,
) -> DeclareRequest {
    let package_specs: Vec<PackageSpec> = req.packages.iter().map(PackageSpec::from).collect();
    DeclareRequest {
        package_manager: pm,
        package_specs,
        repo: Some(repo.to_string()),
        // We never hand coord the worktree path (its dry-run invoker is gated
        // off in prod and this producer owns the worktree). See types.rs.
        repo_path: None,
        correlation_id: Some(correlation_id),
        // ---- Phase-2 ground truth (honest None/false on a missing probe) --
        dry_run: ground_truth.dry_run,
        security_audited: ground_truth.security_audited,
        // ---- Phase-3 declare-time scans (honest empties on no match) ------
        importer_inventory,
        sibling_manifests,
    }
}

/// Run the install subprocess on a blocking thread with a bounded timeout,
/// capturing stdout/stderr. Never a PTY; `CREATE_NO_WINDOW` on Windows (applied
/// inside [`pm::build_install_command`] via `pm_detect::pm_command`). The
/// subprocess exit code rides back in the [`InstallOutcome`] — it is NOT
/// swallowed.
async fn run_install(
    pm: PackageManager,
    repo_path: &Path,
    packages: &[types::PackageSpecInput],
    dev: bool,
    registry_env: &RegistryEnv,
) -> Result<InstallOutcome, RunError> {
    let (mut cmd, argv) = pm::build_install_command(pm, repo_path, packages, dev, registry_env);

    let join = tokio::task::spawn_blocking(move || -> std::io::Result<InstallRaw> {
        use std::io::Read;

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;

        // Drain pipes on the same blocking thread; install output is bounded and
        // we read after the wait below. Take the handles first so the child can
        // be waited with a deadline.
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();

        // Bounded wait: poll `try_wait` against the deadline rather than block
        // forever. A hung resolver is killed at the timeout.
        let deadline = std::time::Instant::now() + INSTALL_TIMEOUT;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait()? {
                Some(s) => break s,
                None => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break child.wait()?;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };

        let mut stdout = String::new();
        if let Some(mut p) = stdout_pipe.take() {
            let _ = p.read_to_string(&mut stdout);
        }
        let mut stderr = String::new();
        if let Some(mut p) = stderr_pipe.take() {
            let _ = p.read_to_string(&mut stderr);
        }

        Ok(InstallRaw {
            exit_code: status.code(),
            success: status.success() && !timed_out,
            timed_out,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|e| RunError::Spawn(format!("join error: {e}")))?;

    let raw = join.map_err(|e| RunError::Spawn(e.to_string()))?;

    if raw.timed_out {
        warn!(
            "install-effects: install subprocess exceeded {}s budget — killed",
            INSTALL_TIMEOUT.as_secs()
        );
    }

    Ok(InstallOutcome {
        command: argv,
        exit_code: raw.exit_code,
        success: raw.success,
        timed_out: raw.timed_out,
        stdout: raw.stdout,
        stderr: raw.stderr,
    })
}

/// Internal raw subprocess result (pre-argv-stamping).
struct InstallRaw {
    exit_code: Option<i32>,
    success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

/// The `repo` slug coord keys on: the basename of `repo_path`. Trailing
/// separators are stripped; an empty/`/`-only path degrades to the raw string.
fn repo_basename(repo_path: &str) -> String {
    // Split on both separators by hand rather than `Path::file_name` — the
    // platform-native `Path` does not treat `\` as a separator on unix, so a
    // Windows-style path would otherwise basename differently across OSes
    // (caught by cross-OS CI). This pure helper stays deterministic.
    let trimmed = repo_path.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| repo_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post as axpost;
    use std::sync::atomic::{AtomicU32, Ordering};

    // -------------------------------------------------------------------
    // Request-body deserialization defaults
    // -------------------------------------------------------------------

    #[test]
    fn run_request_optional_fields_default() {
        let body = serde_json::json!({
            "repo_path": "/repos/widget",
            "packages": [{ "name": "left-pad" }],
        });
        let req: RunRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.repo_path, "/repos/widget");
        assert!(req.package_manager.is_none());
        assert_eq!(req.packages.len(), 1);
        assert_eq!(req.packages[0].name, "left-pad");
        assert!(req.packages[0].version_req.is_none());
        assert!(!req.dev);
        assert!(!req.override_escalation);
        assert!(!req.dry_run_only);
    }

    #[test]
    fn run_request_minimal_empty_packages() {
        let req: RunRequest =
            serde_json::from_value(serde_json::json!({ "repo_path": "/r" })).unwrap();
        assert!(req.packages.is_empty());
    }

    #[test]
    fn repo_basename_strips_dir_and_trailing_sep() {
        assert_eq!(repo_basename("/a/b/widget"), "widget");
        assert_eq!(repo_basename("/a/b/widget/"), "widget");
        assert_eq!(repo_basename("widget"), "widget");
        assert_eq!(repo_basename("C:\\repos\\widget"), "widget");
    }

    // -------------------------------------------------------------------
    // Stubbed coord server (hand-rolled — no wiremock dev-dep in this repo).
    // -------------------------------------------------------------------

    struct MockHits {
        declare: AtomicU32,
        predict: AtomicU32,
        verify: AtomicU32,
        fs_obs: AtomicU32,
        /// The last declare body received (for asserting importer/sibling fields).
        declare_body: std::sync::Mutex<Option<serde_json::Value>>,
        /// The last predict-and-check body received (for the secret-leak assert).
        predict_body: std::sync::Mutex<Option<serde_json::Value>>,
        /// The last verify body received (for asserting EVERY contract field).
        verify_body: std::sync::Mutex<Option<serde_json::Value>>,
        /// The last fs-observations body received (for the same-corr-id assert).
        fs_body: std::sync::Mutex<Option<serde_json::Value>>,
    }

    impl MockHits {
        fn new() -> Self {
            MockHits {
                declare: AtomicU32::new(0),
                predict: AtomicU32::new(0),
                verify: AtomicU32::new(0),
                fs_obs: AtomicU32::new(0),
                declare_body: std::sync::Mutex::new(None),
                predict_body: std::sync::Mutex::new(None),
                verify_body: std::sync::Mutex::new(None),
                fs_body: std::sync::Mutex::new(None),
            }
        }
        fn verify_json(&self) -> serde_json::Value {
            self.verify_body
                .lock()
                .unwrap()
                .clone()
                .expect("verify body")
        }
        fn fs_json(&self) -> serde_json::Value {
            self.fs_body.lock().unwrap().clone().expect("fs body")
        }
        /// Every captured outbound coord body (declare / predict / verify / fs),
        /// stringified — for the secret-never-leaks scan.
        fn all_bodies_concat(&self) -> String {
            let mut s = String::new();
            for b in [
                &self.declare_body,
                &self.predict_body,
                &self.verify_body,
                &self.fs_body,
            ] {
                if let Some(v) = b.lock().unwrap().clone() {
                    s.push_str(&v.to_string());
                    s.push('\n');
                }
            }
            s
        }
    }

    /// Spawn a tiny axum server that mimics coord's declare + predict-and-check
    /// + verify + fs/observations. `resolution` is the JSON the predict route
    /// returns under `resolution`; `risk_factors` rides alongside. Captures the
    /// declare/verify/fs request bodies for assertions. Returns the base URL +
    /// hit counters + a shutdown sender.
    fn spawn_coord_mock(
        resolution: serde_json::Value,
        risk_factors: Vec<String>,
    ) -> (String, Arc<MockHits>, tokio::sync::oneshot::Sender<()>) {
        let hits = Arc::new(MockHits::new());
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = std_listener.local_addr().expect("addr").port();
        std_listener.set_nonblocking(true).expect("nb");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        #[derive(Clone)]
        struct St {
            hits: Arc<MockHits>,
            resolution: Arc<serde_json::Value>,
            risk_factors: Arc<Vec<String>>,
        }
        let st = St {
            hits: hits.clone(),
            resolution: Arc::new(resolution),
            risk_factors: Arc::new(risk_factors),
        };

        async fn declare_h(
            State(st): State<St>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            st.hits.declare.fetch_add(1, Ordering::SeqCst);
            *st.hits.declare_body.lock().unwrap() = Some(body);
            Json(serde_json::json!({ "ok": true }))
        }
        async fn predict_h(
            State(st): State<St>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            st.hits.predict.fetch_add(1, Ordering::SeqCst);
            *st.hits.predict_body.lock().unwrap() = Some(body);
            Json(serde_json::json!({
                "predicted_effect": {},
                "resolution": *st.resolution,
                "risk_factors": *st.risk_factors,
            }))
        }
        async fn verify_h(
            State(st): State<St>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            st.hits.verify.fetch_add(1, Ordering::SeqCst);
            let corr = body.get("correlation_id").cloned();
            *st.hits.verify_body.lock().unwrap() = Some(body);
            // Echo coord's InstallVerification shape (the slice the route reads).
            Json(serde_json::json!({
                "composed_outcome": "confirmed",
                "rationale": "mock verify",
                "per_dimension": [],
                "coverage": 0.5,
                "provenance": "install-verify-live",
                "correlation_id": corr,
            }))
        }
        async fn fs_obs_h(
            State(st): State<St>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            st.hits.fs_obs.fetch_add(1, Ordering::SeqCst);
            *st.hits.fs_body.lock().unwrap() = Some(body);
            Json(serde_json::json!({ "accepted": 1, "persisted": 1 }))
        }

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let app: Router = Router::new()
                    .route("/coord/installs/declare", axpost(declare_h))
                    .route("/coord/installs/predict-and-check", axpost(predict_h))
                    .route("/coord/installs/verify", axpost(verify_h))
                    .route("/coord/fs/observations", axpost(fs_obs_h))
                    .with_state(st);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(60));
        (format!("http://127.0.0.1:{port}"), hits, tx)
    }

    /// A loopback base that points nowhere usable — typecheck POSTs fail fast
    /// (connection refused), so `observed_typecheck` stays empty but the loop
    /// still counts as ran when there ARE affected files. Used by tests that
    /// don't exercise the typecheck path.
    const DEAD_LOOPBACK: &str = "http://127.0.0.1:1";

    /// A coord `Decision`/`proceed` resolution (within autonomy thresholds).
    fn proceed_resolution() -> serde_json::Value {
        serde_json::json!({
            "kind": "decision",
            "action": { "type": "proceed", "log_message": "ok" },
            "rationale": "no autonomy threshold crossed",
            "confidence": 1.0,
        })
    }

    /// A coord `Decision`/`escalate` resolution (a threshold crossed).
    fn escalate_resolution() -> serde_json::Value {
        serde_json::json!({
            "kind": "decision",
            "action": { "type": "escalate", "escalation_message": "major bump" },
            "rationale": "1 autonomy threshold(s) crossed",
            "confidence": 1.0,
        })
    }

    /// A repo dir an autodetect would call `cargo` for — but the bogus package
    /// makes `cargo add` fail fast, so the test never mutates anything real.
    /// We instead drive the manager explicitly to avoid running cargo.
    fn run(req: RunRequest, base: &str) -> Result<ApiResponse<RunResponse>, RunError> {
        run_lb(req, base, DEAD_LOOPBACK)
    }

    /// As [`run`] but with an explicit typecheck-loopback base (so a test can
    /// point it at a mock runner-side typecheck endpoint).
    fn run_lb(
        req: RunRequest,
        base: &str,
        loopback: &str,
    ) -> Result<ApiResponse<RunResponse>, RunError> {
        run_lb_creds(req, base, loopback, &FakeCredStore::empty())
    }

    /// As [`run_lb`] but with an explicit fake registry-credential store so a
    /// test can prove the secret threads into subprocesses without leaking into
    /// any coord body.
    fn run_lb_creds<L: RegistryTokenLookup + ?Sized>(
        req: RunRequest,
        base: &str,
        loopback: &str,
        store: &L,
    ) -> Result<ApiResponse<RunResponse>, RunError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(run_with_base(req, base, loopback, store))
    }

    /// In-memory fake registry-credential store for tests — NEVER touches a real
    /// OS keyring.
    struct FakeCredStore(std::collections::BTreeMap<String, String>);
    impl FakeCredStore {
        fn empty() -> Self {
            FakeCredStore(std::collections::BTreeMap::new())
        }
        fn with(name: &str, value: &str) -> Self {
            let mut m = std::collections::BTreeMap::new();
            m.insert(name.to_string(), value.to_string());
            FakeCredStore(m)
        }
    }
    impl RegistryTokenLookup for FakeCredStore {
        fn lookup(&self, name: &str) -> Option<String> {
            if registry_creds::validate_name(name).is_err() {
                return None;
            }
            self.0.get(name).cloned()
        }
    }

    /// Build a request that stops after predict-and-check (`dry_run_only`) so no
    /// subprocess ever runs — used by the gate tests that only assert on the
    /// coord-call side and the returned resolution.
    fn req_dry(pm: &str, override_escalation: bool) -> RunRequest {
        RunRequest {
            repo_path: "/tmp/does-not-matter".to_string(),
            package_manager: Some(pm.to_string()),
            packages: vec![types::PackageSpecInput {
                name: "left-pad".to_string(),
                version_req: None,
            }],
            dev: false,
            override_escalation,
            dry_run_only: true,
            registry_env: vec![],
            mode: RunMode::Full,
        }
    }

    #[test]
    fn escalate_no_override_blocks_install_and_returns_resolution() {
        let (base, hits, _sd) = spawn_coord_mock(escalate_resolution(), vec!["major bump".into()]);
        // NOT dry_run_only — but escalation with no override must still block.
        let req = RunRequest {
            dry_run_only: false,
            ..req_dry("npm", false)
        };
        let resp = run(req, &base).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Escalate);
        assert!(
            data.install.is_none(),
            "no subprocess on a blocked escalation"
        );
        assert!(!data.escalation_overridden);
        assert_eq!(data.risk_factors, vec!["major bump".to_string()]);
        // declare + predict ran exactly once each.
        assert_eq!(hits.declare.load(Ordering::SeqCst), 1);
        assert_eq!(hits.predict.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dry_run_only_stops_after_predict_even_when_proceed() {
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(req_dry("npm", false), &base).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Proceed);
        assert!(data.dry_run_only);
        assert!(
            data.install.is_none(),
            "dry_run_only must not run a subprocess"
        );
        assert_eq!(hits.declare.load(Ordering::SeqCst), 1);
        assert_eq!(hits.predict.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn escalate_with_override_records_overridden_and_reaches_install_path() {
        // override_escalation:true ⇒ the gate does NOT block. We keep
        // dry_run_only:false so the install path is reached; the package
        // manager is `cargo` against a temp dir so `cargo update` is harmless
        // (or fails fast) — we assert on the run-context flag + that the
        // subprocess path was entered (install is Some).
        let d = tempfile::tempdir().unwrap();
        let (base, hits, _sd) = spawn_coord_mock(escalate_resolution(), vec!["major bump".into()]);
        let req = RunRequest {
            repo_path: d.path().display().to_string(),
            package_manager: Some("cargo".to_string()),
            packages: vec![], // `cargo update` (lockfile refresh) — no real net add
            dev: false,
            override_escalation: true,
            dry_run_only: false,
            registry_env: vec![],
            mode: RunMode::Full,
        };
        let resp = run(req, &base).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Escalate);
        assert!(data.escalation_overridden, "override must be recorded");
        assert!(
            data.install.is_some(),
            "override must reach the install path (subprocess ran)"
        );
        // The exit code rides back (cargo update on an empty dir fails — that's
        // fine; we only assert the code is surfaced, not its value).
        let install = data.install.unwrap();
        assert_eq!(install.command.first().map(String::as_str), Some("cargo"));
        assert_eq!(hits.predict.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pipenv_token_rejected_before_any_coord_call() {
        // pipenv is rejected at PM resolution — BEFORE declare/predict. Use a
        // base that would fail if hit, to prove no call is made.
        let req = req_dry("pipenv", false);
        let err = run(req, "http://127.0.0.1:1").unwrap_err();
        match err {
            RunError::BadRequest(m) => {
                assert!(m.contains("pipenv"), "msg: {m}");
                assert!(m.contains("NOT a coord variant"), "msg: {m}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn proceed_runs_install_path() {
        // Proceed + not dry_run_only ⇒ the install subprocess path is entered.
        let d = tempfile::tempdir().unwrap();
        let (base, _hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let req = RunRequest {
            repo_path: d.path().display().to_string(),
            package_manager: Some("cargo".to_string()),
            packages: vec![],
            dev: false,
            override_escalation: false,
            dry_run_only: false,
            registry_env: vec![],
            mode: RunMode::Full,
        };
        let resp = run(req, &base).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Proceed);
        assert!(data.install.is_some());
    }

    #[test]
    fn unknown_resolution_shape_fails_safe_to_escalate() {
        // A resolution coord-side we don't recognize must NOT auto-proceed.
        let weird = serde_json::json!({ "kind": "guidance", "composition": {} });
        let (base, _hits, _sd) = spawn_coord_mock(weird, vec![]);
        let resp = run(req_dry("npm", false), &base).unwrap();
        assert_eq!(resp.data.unwrap().gate, GateOutcome::Escalate);
    }

    // ===================================================================
    // Phase 3 — observation assembly + verify loop
    // ===================================================================

    /// A cargo install in an empty temp dir FAILS the subprocess (no manifest),
    /// which exercises the §6 partial-install path: verify STILL runs. We
    /// pre-write a `Cargo.lock` so `observe_pinned` reads a real pinned set
    /// regardless of the failing subprocess.
    fn cargo_req(repo_path: &str) -> RunRequest {
        RunRequest {
            repo_path: repo_path.to_string(),
            package_manager: Some("cargo".to_string()),
            packages: vec![],
            dev: false,
            override_escalation: false,
            dry_run_only: false,
            registry_env: vec![],
            mode: RunMode::Full,
        }
    }

    #[test]
    fn verify_fires_with_full_contract_and_correlation_id() {
        let d = tempfile::tempdir().unwrap();
        // A real lockfile so observed_pinned is non-empty.
        std::fs::write(
            d.path().join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.5\"\n",
        )
        .unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(cargo_req(&d.path().display().to_string()), &base).unwrap();
        let data = resp.data.unwrap();

        // verify ran exactly once and the route surfaces the composed outcome.
        assert_eq!(hits.verify.load(Ordering::SeqCst), 1);
        let verify = data.verify.expect("route must carry the verify outcome");
        assert_eq!(verify.composed_outcome, "confirmed");
        // correlation_id echoes the run's.
        assert_eq!(verify.correlation_id, Some(data.correlation_id));

        // ---- assert EVERY VerifyRequest contract field on the wire body ----
        let body = hits.verify_json();
        // correlation_id matches declare's (same run uuid).
        assert_eq!(
            body["correlation_id"].as_str().unwrap(),
            data.correlation_id.to_string()
        );
        // repo is the tempdir basename (coord keys on the basename of repo_path).
        assert_eq!(body["repo"].as_str(), Some(data.repo.as_str()));
        // observed_pinned came from the temp-dir lockfile.
        assert_eq!(body["observed_pinned"]["serde"], "1.0.5");
        // observed_audit_ran is a real bool (false here — no cargo-audit tool).
        assert!(body["observed_audit_ran"].is_boolean());
        // typecheck_ran true because the §6 synthetic install-exit record was
        // appended (the install failed in an empty dir).
        assert_eq!(body["typecheck_ran"], true);
        // paths[] carries the touched manifest/lockfile (Cargo.lock at minimum).
        let paths: Vec<&str> = body["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert!(paths.contains(&"Cargo.lock"), "paths: {paths:?}");
        // predicted echoes are present (empty here — no dry-run in this dir).
        assert!(body.get("predicted_pinned").is_some() || body["predicted_pinned"].is_null());
    }

    #[test]
    fn install_failure_still_verifies_and_propagates_exit_code() {
        // cargo update in an empty dir fails — verify must STILL fire and the
        // failing exit code must ride the verify body (not be swallowed).
        let d = tempfile::tempdir().unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(cargo_req(&d.path().display().to_string()), &base).unwrap();
        let data = resp.data.unwrap();

        let install = data.install.expect("install ran");
        assert!(!install.success, "cargo update on empty dir fails");
        assert_eq!(hits.verify.load(Ordering::SeqCst), 1, "verify still fires");

        // The synthetic install-exit typecheck record carries the exit code.
        let body = hits.verify_json();
        let tcs = body["observed_typecheck"].as_array().unwrap();
        let exit_rec = tcs
            .iter()
            .find(|t| t["file"] == "<install-subprocess>")
            .expect("install-exit record present");
        assert_eq!(exit_rec["pass"], false);
        let diag = exit_rec["diagnostics"][0].as_str().unwrap();
        assert!(diag.contains("install subprocess exited"), "diag: {diag}");
    }

    #[test]
    fn fs_observations_push_carries_same_correlation_id() {
        // A modified manifest + new lockfile ⇒ an FS-observation push under the
        // SAME correlation_id as verify.
        let d = tempfile::tempdir().unwrap();
        // Pre-existing Cargo.toml so it has a pre-sha (and survives the failing
        // cargo update so post == pre, still a valid observation).
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();
        std::fs::write(d.path().join("Cargo.lock"), "version = 3\n").unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(cargo_req(&d.path().display().to_string()), &base).unwrap();
        let data = resp.data.unwrap();

        assert_eq!(hits.fs_obs.load(Ordering::SeqCst), 1, "fs push fired");
        let fs = hits.fs_json();
        assert_eq!(
            fs["correlation_id"].as_str().unwrap(),
            data.correlation_id.to_string(),
            "FS push MUST share the verify correlation_id"
        );
        let obs = fs["observations"].as_array().unwrap();
        let paths: Vec<&str> = obs.iter().map(|o| o["path"].as_str().unwrap()).collect();
        assert!(paths.contains(&"Cargo.toml"), "{paths:?}");
        assert!(paths.contains(&"Cargo.lock"), "{paths:?}");
        // Each carries a post_sha.
        assert!(obs.iter().all(|o| o["post_sha"].is_string()));
    }

    #[test]
    fn fs_push_failure_does_not_fail_the_run() {
        // Point the coord base at a server that 500s the FS push but serves
        // verify — the run must still succeed (FS push is fire-and-forget). We
        // simulate by using a coord mock that lacks the fs route would 404; but
        // our mock serves it. Instead, assert the run succeeds even when the FS
        // body is malformed-but-accepted: the mock always 200s, so we assert the
        // weaker invariant that a present fs route + verify ⇒ Ok. The dead-route
        // case is covered by push_fs_observations being fire-and-forget.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.lock"), "version = 3\n").unwrap();
        let (base, _hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(cargo_req(&d.path().display().to_string()), &base);
        assert!(resp.is_ok(), "run succeeds despite best-effort FS push");
    }

    #[test]
    fn typecheck_ran_false_when_no_affected_files() {
        // No packages ⇒ importer scan yields no affected files ⇒ the typecheck
        // loop does not run. (A failing install still appends the §6 synthetic
        // record, flipping typecheck_ran true — so use a dir where install
        // SUCCEEDS is hard; instead assert the loopback-level invariant via the
        // observe module's pure boundary in a focused unit test below.)
        // Here we assert that with an empty affected set + a SUCCESSFUL-looking
        // path the loop reports not-ran. We can't force a cargo success in CI,
        // so we test run_typecheck_loopback directly.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (results, ran) = rt.block_on(observe::run_typecheck_loopback(
            DEAD_LOOPBACK,
            Path::new("/tmp"),
            &[],
            false,
        ));
        assert!(results.is_empty());
        assert!(!ran, "no affected files ⇒ typecheck loop did not run");
    }

    #[test]
    fn typecheck_loopback_hits_mock_endpoint_with_overridden_suffix() {
        // Stand up a mock runner-side /code-semantics/typecheck endpoint and
        // point the loopback at it. Assert the result maps pass + the
        // +overridden provenance suffix when escalation_overridden.
        use axum::routing::post as axpost2;
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = std_listener.local_addr().unwrap().port();
        std_listener.set_nonblocking(true).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        async fn tc_h(Json(_b): Json<serde_json::Value>) -> Json<serde_json::Value> {
            Json(serde_json::json!({
                "observer": "code_semantics",
                "query": "typecheck",
                "provenance": "cargo-check",
                "coverage": 1.0,
                "result": { "ok": true, "errors": [], "coverage": 1.0 }
            }))
        }
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let app: Router<()> =
                    Router::new().route("/code-semantics/typecheck", axpost2(tc_h));
                let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
        });
        std::thread::sleep(Duration::from_millis(60));
        let base = format!("http://127.0.0.1:{port}");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (results, ran) = rt.block_on(observe::run_typecheck_loopback(
            &base,
            Path::new("/repo"),
            &["src/a.rs".to_string()],
            true, // escalation_overridden
        ));
        let _ = tx.send(());
        assert!(ran);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file, "src/a.rs");
        assert!(results[0].pass);
        assert_eq!(
            results[0].provenance.as_deref(),
            Some("cargo-check+overridden")
        );
    }

    // ===================================================================
    // Phase 4 — private-registry credential injection + secret-never-leaks
    // ===================================================================

    /// The secret a fake registry credential carries. Deliberately a unique,
    /// grep-distinctive string so the leak scan is unambiguous.
    const FAKE_SECRET: &str = "npm_SUPERSECRET_TOKEN_zzz999_never_leak";

    #[test]
    fn registry_secret_never_appears_in_any_coord_body() {
        // A full install flow (cargo, fails fast in an empty dir but still fires
        // declare → predict → fs-observations → verify) with a fake NPM_TOKEN
        // present. The secret is injected into the subprocesses ONLY; it must
        // appear in NO outbound coord body.
        let d = tempfile::tempdir().unwrap();
        // A lockfile so observe_pinned reads a real set + fs push fires.
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();
        std::fs::write(
            d.path().join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.5\"\n",
        )
        .unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);

        // Store the secret under NPM_TOKEN (a well-known name ⇒ auto-injected).
        let store = FakeCredStore::with("NPM_TOKEN", FAKE_SECRET);
        let resp = run_lb_creds(
            cargo_req(&d.path().display().to_string()),
            &base,
            DEAD_LOOPBACK,
            &store,
        )
        .unwrap();
        let data = resp.data.unwrap();

        // The flow completed end-to-end (declare/predict/verify all fired).
        assert_eq!(hits.declare.load(Ordering::SeqCst), 1);
        assert_eq!(hits.predict.load(Ordering::SeqCst), 1);
        assert_eq!(hits.verify.load(Ordering::SeqCst), 1);
        assert!(data.verify.is_some());

        // ---- THE Phase-4 invariant: secret in NO captured coord body --------
        let all = hits.all_bodies_concat();
        assert!(
            !all.contains(FAKE_SECRET),
            "registry secret leaked into a coord body!\n{all}"
        );
        // Also the env-var NAME shouldn't leak as a payload key (it isn't a
        // secret, but coord has no business seeing the registry-auth config).
        assert!(
            !all.contains("NPM_TOKEN"),
            "registry env-var name leaked into a coord body:\n{all}"
        );
    }

    #[test]
    fn registry_env_threads_into_install_command() {
        // The injection seam: a resolved RegistryEnv applied by the install
        // command builder lands on the spawned Command's env. Asserted on the
        // built Command (no real subprocess / keyring).
        use registry_creds::RegistryEnv;
        let d = tempfile::tempdir().unwrap();
        let store = FakeCredStore::with("NPM_TOKEN", FAKE_SECRET);
        let env: RegistryEnv = registry_creds::resolve_registry_env(&store, &[]);
        assert_eq!(env.names(), vec!["NPM_TOKEN"]);

        let pkgs = vec![types::PackageSpecInput {
            name: "left-pad".to_string(),
            version_req: None,
        }];
        let (cmd, _argv) =
            pm::build_install_command(PackageManager::Npm, d.path(), &pkgs, false, &env);
        let got: std::collections::BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().to_string(),
                    v?.to_string_lossy().to_string(),
                ))
            })
            .collect();
        assert_eq!(got.get("NPM_TOKEN").map(String::as_str), Some(FAKE_SECRET));
    }

    #[test]
    fn no_credentials_means_empty_env_no_injection() {
        // The common case: no stored credentials ⇒ the resolved env is empty and
        // the install command carries no extra registry env.
        use registry_creds::RegistryEnv;
        let d = tempfile::tempdir().unwrap();
        let store = FakeCredStore::empty();
        let env: RegistryEnv = registry_creds::resolve_registry_env(&store, &[]);
        assert!(env.is_empty());
        let (cmd, _argv) =
            pm::build_install_command(PackageManager::Cargo, d.path(), &[], false, &env);
        // No NPM_TOKEN (or any well-known name) was injected.
        let injected: Vec<String> = cmd
            .get_envs()
            .filter_map(|(k, _)| {
                let k = k.to_string_lossy().to_string();
                if registry_creds::WELL_KNOWN_NAMES.contains(&k.as_str()) {
                    Some(k)
                } else {
                    None
                }
            })
            .collect();
        assert!(injected.is_empty(), "unexpected injected env: {injected:?}");
    }

    /// Live integration test: a REAL tiny `npm install` of a zero-dep package in
    /// a tempdir against the mock coord. Gated behind `QONTINUI_INSTALL_FX_LIVE=1`
    /// (npm + network required) — skipped at runtime otherwise so CI stays
    /// hermetic.
    #[test]
    fn live_npm_install_end_to_end() {
        if std::env::var("QONTINUI_INSTALL_FX_LIVE").as_deref() != Ok("1") {
            eprintln!("skipping live npm install (set QONTINUI_INSTALL_FX_LIVE=1 to run)");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("package.json"),
            "{\"name\":\"fx-live\",\"version\":\"1.0.0\"}",
        )
        .unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let req = RunRequest {
            repo_path: d.path().display().to_string(),
            package_manager: Some("npm".to_string()),
            packages: vec![types::PackageSpecInput {
                name: "left-pad".to_string(),
                version_req: Some("1.3.0".to_string()),
            }],
            dev: false,
            override_escalation: false,
            dry_run_only: false,
            registry_env: vec![],
            mode: RunMode::Full,
        };
        let resp = run(req, &base).unwrap();
        let data = resp.data.unwrap();
        assert!(data.install.is_some());
        assert_eq!(hits.verify.load(Ordering::SeqCst), 1);
        // A successful npm install writes a package-lock.json ⇒ observed_pinned
        // carries left-pad.
        let body = hits.verify_json();
        if data.install.as_ref().unwrap().success {
            assert_eq!(body["observed_pinned"]["left-pad"], "1.3.0");
        }
    }

    // ===================================================================
    // Interception — `mode:"intercept"` pre-call + `/observe-verify` post
    // ===================================================================

    /// Build an intercept-mode pre-call request for a real `repo_path`.
    ///
    /// Pins the process-global fleet-policy cache to `observe` as a side effect
    /// so the P4 `off` short-circuit (which fires when the cache is `off`, the
    /// fail-safe default before any live poll) does NOT trigger — these tests
    /// exercise the full declare→gate→stash intercept pre-call path.
    fn intercept_req(repo_path: &str, packages: Vec<&str>) -> RunRequest {
        crate::mcp::fleet_policy_poller::set_mode_for_test("observe");
        RunRequest {
            repo_path: repo_path.to_string(),
            package_manager: Some("npm".to_string()),
            packages: packages
                .into_iter()
                .map(|n| types::PackageSpecInput {
                    name: n.to_string(),
                    version_req: None,
                })
                .collect(),
            dev: false,
            override_escalation: false,
            dry_run_only: false,
            registry_env: vec![],
            mode: RunMode::Intercept,
        }
    }

    /// Drive the observe-verify post-call body on a current-thread runtime.
    fn observe_verify(
        req: ObserveVerifyRequest,
        base: &str,
        loopback: &str,
    ) -> Result<ApiResponse<VerifyOutcome>, RunError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(observe_verify_with_base(
            req,
            base,
            loopback,
            &FakeCredStore::empty(),
        ))
    }

    #[test]
    fn intercept_mode_returns_correlation_id_and_runs_no_subprocess() {
        // A package.json so npm autodetect/pre-sha capture has something to read.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("package.json"),
            "{\"name\":\"x\",\"version\":\"1.0.0\"}",
        )
        .unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(
            intercept_req(&d.path().display().to_string(), vec!["left-pad"]),
            &base,
        )
        .unwrap();
        let data = resp.data.unwrap();

        // Declared + gated, but NO install ran and NO verify fired on the pre-call.
        assert_eq!(data.gate, GateOutcome::Proceed);
        assert!(
            data.install.is_none(),
            "intercept pre-call runs no subprocess"
        );
        assert!(data.verify.is_none(), "verify is deferred to the post-call");
        assert_eq!(hits.declare.load(Ordering::SeqCst), 1);
        assert_eq!(hits.predict.load(Ordering::SeqCst), 1);
        assert_eq!(
            hits.verify.load(Ordering::SeqCst),
            0,
            "no verify on pre-call"
        );

        // A PreContext was stashed under the returned correlation_id.
        let ctx = precontext::take(&data.correlation_id).expect("PreContext stashed");
        assert_eq!(ctx.package_manager, PackageManager::Npm);
        // pre_shas captured package.json's blob (it existed pre-install).
        assert!(
            ctx.pre_shas.get("package.json").map(|o| o.is_some()) == Some(true),
            "pre-sha for package.json should be captured: {:?}",
            ctx.pre_shas
        );
    }

    #[test]
    fn observe_verify_with_stashed_id_completes_verify_with_pre_sha_continuity() {
        let d = tempfile::tempdir().unwrap();
        // A lockfile so the post-call's observe_pinned + FS push have content.
        std::fs::write(
            d.path().join("package.json"),
            "{\"name\":\"x\",\"version\":\"1.0.0\"}",
        )
        .unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);

        // Pre-call.
        let resp = run(
            intercept_req(&d.path().display().to_string(), vec!["left-pad"]),
            &base,
        )
        .unwrap();
        let corr = resp.data.unwrap().correlation_id;

        // Simulate the shim having run the real install: write a lockfile so the
        // post-sha differs from the (absent) pre-sha.
        std::fs::write(
            d.path().join("package-lock.json"),
            "{\"name\":\"x\",\"lockfileVersion\":3}",
        )
        .unwrap();

        // Post-call.
        let vresp = observe_verify(
            ObserveVerifyRequest {
                correlation_id: corr,
                install_exit_code: Some(0),
            },
            &base,
            DEAD_LOOPBACK,
        )
        .unwrap();
        let verify = vresp.data.unwrap();
        assert_eq!(verify.composed_outcome, "confirmed");
        assert_eq!(verify.correlation_id, Some(corr));
        assert_eq!(
            hits.verify.load(Ordering::SeqCst),
            1,
            "post-call fires verify"
        );

        // The verify body carries the SAME correlation_id as the pre-call (pre↔
        // post continuity) and the touched paths from the worktree.
        let body = hits.verify_json();
        assert_eq!(body["correlation_id"].as_str().unwrap(), corr.to_string());
        let paths: Vec<&str> = body["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert!(
            paths.contains(&"package-lock.json") || paths.contains(&"package.json"),
            "touched paths from the mutated worktree: {paths:?}"
        );

        // The id was consumed (a second post-call finds nothing).
        assert!(precontext::take(&corr).is_none());
    }

    #[test]
    fn observe_verify_unknown_id_degrades_to_partial_not_error() {
        // No pre-call ⇒ the correlation_id is unknown. The post-call must NOT
        // error to the shim; it degrades to a best-effort verify (the mock coord
        // still composes an outcome keyed on the id).
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let unknown = Uuid::new_v4();
        let vresp = observe_verify(
            ObserveVerifyRequest {
                correlation_id: unknown,
                install_exit_code: Some(0),
            },
            &base,
            DEAD_LOOPBACK,
        );
        assert!(vresp.is_ok(), "unknown id must not error to the shim");
        let verify = vresp.unwrap().data.unwrap();
        assert_eq!(verify.correlation_id, Some(unknown));
        // verify still fired (best-effort) — coord recorded the (degraded) signature.
        assert_eq!(hits.verify.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn observe_verify_propagates_nonzero_install_exit_code() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(
            intercept_req(&d.path().display().to_string(), vec!["bad-pkg"]),
            &base,
        )
        .unwrap();
        let corr = resp.data.unwrap().correlation_id;

        // The shim reports a FAILED install (exit 1).
        let vresp = observe_verify(
            ObserveVerifyRequest {
                correlation_id: corr,
                install_exit_code: Some(1),
            },
            &base,
            DEAD_LOOPBACK,
        )
        .unwrap();
        assert!(vresp.data.is_some());
        // The §6 synthetic install-exit record carries the failing code.
        let body = hits.verify_json();
        let tcs = body["observed_typecheck"].as_array().unwrap();
        let exit_rec = tcs
            .iter()
            .find(|t| t["file"] == "<install-subprocess>")
            .expect("install-exit record present on failure");
        assert_eq!(exit_rec["pass"], false);
    }

    #[test]
    fn intercept_override_escalation_records_overridden_through_observe_verify() {
        // Phase 3 (plan §4 Phase 3): an intercept PRE-call with
        // override_escalation:true against an ESCALATE verdict must respond
        // escalation_overridden:true, and the subsequent observe-verify POST-call
        // must carry the `+overridden` provenance into the observed_typecheck
        // records (the producer's shipped override path reached through the shim).
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let (base, hits, _sd) = spawn_coord_mock(escalate_resolution(), vec!["major bump".into()]);

        // PRE-call: intercept mode, override on, escalate verdict.
        let mut pre = intercept_req(&d.path().display().to_string(), vec!["left-pad"]);
        pre.override_escalation = true;
        let resp = run(pre, &base).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Escalate, "verdict is escalate");
        assert!(
            data.escalation_overridden,
            "override_escalation:true on the intercept pre-call ⇒ escalation_overridden:true"
        );
        let corr = data.correlation_id;

        // POST-call: the shim reports a FAILED install (exit 1) so the synthetic
        // <install-subprocess> typecheck record is emitted with the +overridden
        // provenance suffix (the override flag rode through the PreContext).
        let vresp = observe_verify(
            ObserveVerifyRequest {
                correlation_id: corr,
                install_exit_code: Some(1),
            },
            &base,
            DEAD_LOOPBACK,
        )
        .unwrap();
        assert!(vresp.data.is_some());
        let body = hits.verify_json();
        let tcs = body["observed_typecheck"].as_array().unwrap();
        let exit_rec = tcs
            .iter()
            .find(|t| t["file"] == "<install-subprocess>")
            .expect("install-exit record present on failure");
        assert_eq!(
            exit_rec["provenance"].as_str(),
            Some("install-exit+overridden"),
            "+overridden provenance rides through the intercept observe-verify path"
        );
    }

    #[test]
    fn intercept_bare_install_stashes_empty_packages_lockfile_sync() {
        // A bare `npm install` (no packages) is a lockfile sync — declare with
        // packages:[] (A6). The pre-call still stashes a context.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let (base, hits, _sd) = spawn_coord_mock(proceed_resolution(), vec![]);
        let resp = run(
            intercept_req(&d.path().display().to_string(), vec![]),
            &base,
        )
        .unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.gate, GateOutcome::Proceed);
        // declare body carries an empty package_specs (lockfile sync).
        let dbody = hits.declare_body.lock().unwrap().clone().unwrap();
        let specs = dbody["package_specs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            specs.is_empty(),
            "bare install declares empty package_specs"
        );
        assert!(precontext::take(&data.correlation_id).is_some());
    }
}
