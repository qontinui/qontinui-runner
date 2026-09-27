//! How the Orchestration Loop restarts its target between iterations —
//! decided ONCE, before the loop starts.
//!
//! Plan `2026-09-22-orchestration-loop-restart-modes-depend-on-the-dev-only-supervisor`.
//! The restart modes used to POST to the dev supervisor (`:9875`)
//! unconditionally, which a published-runner user does not have, so a loop
//! died the first time it restarted. Now:
//!
//! - a target this runner spawned (an [`InstanceManager`] child) is restarted
//!   IN-PROCESS — on dev boxes too, so the dev box runs the path users get;
//! - the supervisor is used only for `rebuild: true`, which compiles from a
//!   source checkout and is intrinsically a development capability;
//! - everything else is refused BEFORE start with a typed
//!   [`RestartUnsupportedCode`] rather than failing mid-loop.
//!
//! The runner never restarts itself (the orchestrator) and never restarts a
//! runner it does not own. Before killing an owned child it reads the child's
//! `GET /restart-readiness`, so a restart never takes down work the loop did
//! not start (see [`check_readiness`]).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{info, warn};

use super::remote_client::{SupervisorClient, DETACHED_REBUILD_TIMEOUT};
use super::types::{
    BetweenIterations, OrchestrationLoopConfig, RestartCapability, RestartPathKind,
    RestartUnsupportedCode,
};
use crate::instance_manager::{InstanceLifecycle, InstanceManager, InstanceRestartError};

/// How long the resolver waits for the dev supervisor's `/health`.
pub const SUPERVISOR_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Tail-sized timeout for the target's `/restart-readiness` (it computes a
/// fresh process census, and `/health` alone has been sampled up to ~10 s on a
/// loaded box).
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for EACH `/health` fallback probe when readiness is unobtainable.
/// Sized against the tail: `/health` has been sampled at ~10 s on a loaded
/// box, so a shorter timeout would call a slow-but-alive runner wedged.
pub const HEALTH_FALLBACK_TIMEOUT: Duration = Duration::from_secs(15);
/// How many `/health` probes must ALL be refused before a target is treated as
/// wedged, and the gap between them (~30 s end to end).
pub const HEALTH_FALLBACK_ATTEMPTS: usize = 3;
pub const HEALTH_PROBE_GAP: Duration = Duration::from_secs(10);

// ============================================================================
// The resolved path
// ============================================================================

/// How the target will be restarted between iterations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPath {
    /// The target is a live child this runner spawned; restart = stop +
    /// wait for the port to free + relaunch the same slot.
    InstanceManager { instance_id: String, port: u16 },
    /// `rebuild: true` on a box whose dev supervisor answers: only the
    /// supervisor can rebuild from source. `supervisor_port` is the
    /// supervisor's own port, not the target's.
    DevSupervisor {
        runner_id: String,
        supervisor_port: u16,
    },
    /// The mode never restarts (`WaitHealthy` / `None`).
    NotNeeded,
}

impl RestartPath {
    pub fn kind(&self) -> RestartPathKind {
        match self {
            Self::InstanceManager { .. } => RestartPathKind::InstanceManager,
            Self::DevSupervisor { .. } => RestartPathKind::DevSupervisor,
            Self::NotNeeded => RestartPathKind::NotNeeded,
        }
    }
}

/// Why the configured restart mode cannot run against this target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartUnsupported {
    pub code: RestartUnsupportedCode,
    pub reason: String,
}

/// The wire token of a code — identical to its serde (`snake_case`) form, so
/// the HTTP 409 `code` and `RestartCapability.code` read the same.
pub fn code_token(code: RestartUnsupportedCode) -> &'static str {
    match code {
        RestartUnsupportedCode::TargetIsOrchestrator => "target_is_orchestrator",
        RestartUnsupportedCode::TargetNotRunnerManaged => "target_not_runner_managed",
        RestartUnsupportedCode::RebuildNeedsDevSupervisor => "rebuild_needs_dev_supervisor",
    }
}

impl std::fmt::Display for RestartUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unsupported here: {}: {}",
            code_token(self.code),
            self.reason
        )
    }
}

impl std::error::Error for RestartUnsupported {}

/// The port this runner's HTTP API is actually bound to: `AppState.api_port`
/// (stored by the MCP server once it binds — it may have fallen back off
/// `$QONTINUI_PORT`), else the bootstrap env/default while it is still 0.
pub fn self_port(bound_api_port: u16) -> u16 {
    if bound_api_port != 0 {
        bound_api_port
    } else {
        crate::mcp::types::get_mcp_api_port()
    }
}

/// The target a config points at: its explicit port, else this runner.
pub fn target_port(config: &OrchestrationLoopConfig, self_port: u16) -> u16 {
    config.target_runner_port.unwrap_or(self_port)
}

/// `Some(rebuild)` for the two modes that restart, `None` otherwise.
fn restart_rebuild(mode: &BetweenIterations) -> Option<bool> {
    match mode {
        BetweenIterations::RestartRunner { rebuild }
        | BetweenIterations::RestartOnSignal { rebuild } => Some(*rebuild),
        BetweenIterations::WaitHealthy | BetweenIterations::None => None,
    }
}

// ============================================================================
// Probes (injectable so tests need no network)
// ============================================================================

/// One row of the dev supervisor's `GET /runners` — only the fields the
/// resolver reads (the supervisor serves many more).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SupervisorRunnerRow {
    pub id: String,
    pub port: u16,
    /// The supervisor's `RunnerKind`, serialized `{"type": "primary" | "named"
    /// | "temp" | "external", ...}`. Absent on an older supervisor.
    #[serde(default)]
    pub kind: Option<serde_json::Value>,
}

impl SupervisorRunnerRow {
    /// The supervisor's own classification says this is the dev PRIMARY.
    ///
    /// When `kind` is absent (a supervisor predating the field) the id is the
    /// only signal left, and the supervisor's primary is always registered as
    /// id `"primary"` (`RunnerConfig::default_primary`) — so that id counts as
    /// primary rather than letting an older supervisor's primary through.
    pub fn is_primary(&self) -> bool {
        match self
            .kind
            .as_ref()
            .and_then(|k| k.get("type"))
            .and_then(|t| t.as_str())
        {
            Some(kind) => kind == "primary",
            None => self.id == "primary",
        }
    }
}

/// Reads the dev supervisor: is it listening, and which runners does it
/// manage (by id and port)?
#[async_trait]
pub trait SupervisorProbe: Send + Sync {
    /// `GET /health` answered 2xx.
    async fn answers(&self, supervisor_port: u16) -> bool;
    /// `GET /runners`, as `(id, port)` rows. `Err` = unreadable.
    async fn runners(&self, supervisor_port: u16) -> Result<Vec<SupervisorRunnerRow>, String>;
}

/// Parse the supervisor's `GET /runners` body: a bare array of runner rows
/// (qontinui-supervisor `routes::runners::list_runners`). An envelope carrying
/// the array under `runners` or `data` is tolerated.
pub fn parse_supervisor_runners(
    body: &serde_json::Value,
) -> Result<Vec<SupervisorRunnerRow>, String> {
    let rows = if body.is_array() {
        body
    } else if let Some(inner) = body.get("runners").or_else(|| body.get("data")) {
        inner
    } else {
        return Err("the supervisor's /runners body is not a runner list".to_string());
    };
    serde_json::from_value(rows.clone())
        .map_err(|e| format!("unparseable supervisor /runners body: {e}"))
}

/// `GET http://127.0.0.1:{port}/health` with [`SUPERVISOR_PROBE_TIMEOUT`].
pub struct HttpSupervisorProbe;

#[async_trait]
impl SupervisorProbe for HttpSupervisorProbe {
    async fn answers(&self, supervisor_port: u16) -> bool {
        let Ok(client) = reqwest::Client::builder()
            .timeout(SUPERVISOR_PROBE_TIMEOUT)
            .build()
        else {
            return false;
        };
        client
            .get(format!("http://127.0.0.1:{supervisor_port}/health"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn runners(&self, supervisor_port: u16) -> Result<Vec<SupervisorRunnerRow>, String> {
        let client = reqwest::Client::builder()
            .timeout(SUPERVISOR_PROBE_TIMEOUT)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(format!("http://127.0.0.1:{supervisor_port}/runners"))
            .send()
            .await
            .map_err(|e| format!("GET /runners failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET /runners returned HTTP {status}"));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("unparseable supervisor /runners body: {e}"))?;
        parse_supervisor_runners(&body)
    }
}

/// The parts of a target's `GET /restart-readiness` a restart decision reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessSummary {
    /// `safe_to_restart` — the target's own verdict.
    pub safe: bool,
    /// `reason` — the target's own explanation of that verdict.
    pub reason: String,
    /// `live_claude.blocking` — every live `claude` process counted as work in
    /// flight (terminal-hosted not declared `finished`, AI-plane, headless and
    /// unclassified alike). The target's verdict reads exactly this.
    pub blocking: usize,
    /// `terminal_sessions.blocking_count`.
    pub terminal_blocking: usize,
    /// `headless_sessions.count`.
    pub headless: usize,
    /// `live_claude.unclassified`.
    pub unclassified: usize,
    /// `live_claude.ai_plane` — live `claude` processes under AI-plane roots.
    pub ai_plane_processes: usize,
    /// `ai_sessions.sessions[].id` — each is a `task_run_id` (the AI plane is
    /// keyed by it; see `mcp/restart_readiness.rs` `AiSessionInput::id`).
    pub ai_session_ids: Vec<String>,
}

/// What a restart would destroy that the loop cannot prove is its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignSessions {
    pub terminal_blocking: usize,
    pub headless: usize,
    pub unclassified: usize,
    /// Live AI-plane `claude` PROCESSES. The census gives a process no
    /// `task_run_id`, so none can be attributed to the loop: every one counts.
    pub ai_plane_processes: usize,
    /// AI-plane sessions whose `task_run_id` this loop did not start.
    pub ai_sessions_not_ours: usize,
}

impl ForeignSessions {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

impl ReadinessSummary {
    /// The restart rule: the target's own `safe_to_restart` verdict
    /// (`live_claude.blocking == 0 && ai_sessions.count == 0`), relaxed in
    /// exactly ONE way — an AI-plane session whose `task_run_id` is in
    /// `own_runs` (a run this loop started on the target, or a reflection /
    /// fixer of one) does not block. Everything else does: every blocking
    /// process of every plane (an AI-plane process included — it carries no
    /// run id to attribute it by), and every AI-plane session this loop did
    /// not start.
    pub fn foreign(&self, own_runs: &BTreeSet<String>) -> ForeignSessions {
        ForeignSessions {
            terminal_blocking: self.terminal_blocking,
            headless: self.headless,
            unclassified: self.unclassified,
            // Whatever of `blocking` the per-plane counts do not explain is
            // attributed to the AI plane, so the rule can never read LESS
            // than the target's own blocking total.
            ai_plane_processes: self.ai_plane_processes.max(
                self.blocking
                    .saturating_sub(self.terminal_blocking + self.headless + self.unclassified),
            ),
            ai_sessions_not_ours: self
                .ai_session_ids
                .iter()
                .filter(|id| !own_runs.contains(*id))
                .count(),
        }
    }
}

/// What one `/health` probe observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthAnswer {
    /// The connection was refused / could not be made: nothing is listening.
    Refused,
    /// A connection was made (or attempted) but no answer came in time —
    /// something may be alive and slow.
    TimedOut,
    /// An HTTP answer, 2xx or not — something is alive.
    Answered(u16),
    /// Any other transport failure — not proof of death.
    Failed(String),
}

/// Reads a target runner's readiness, liveness and identity.
#[async_trait]
pub trait TargetProbe: Send + Sync {
    /// `GET /restart-readiness`, summarised. `Err` = unobtainable (transport
    /// failure, non-2xx, unparseable, or a census with an unknown plane).
    async fn readiness(&self, port: u16) -> Result<ReadinessSummary, String>;
    /// One `GET /health` with [`HEALTH_FALLBACK_TIMEOUT`].
    async fn health(&self, port: u16) -> HealthAnswer;
    /// The PID of the process answering `GET /health` on `port` (its `pid`).
    async fn answering_pid(&self, port: u16) -> Result<u32, String>;
    /// Gap between the fallback `/health` probes.
    fn health_probe_gap(&self) -> Duration {
        HEALTH_PROBE_GAP
    }
}

/// Production [`TargetProbe`] over loopback HTTP.
pub struct HttpTargetProbe;

#[derive(Deserialize)]
struct ReadinessWire {
    #[serde(default)]
    safe_to_restart: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    terminal_sessions: Option<TerminalWire>,
    #[serde(default)]
    headless_sessions: Option<CountWire>,
    #[serde(default)]
    ai_sessions: Option<AiWire>,
    #[serde(default)]
    live_claude: Option<LiveClaudeWire>,
}

#[derive(Deserialize)]
struct TerminalWire {
    blocking_count: usize,
}

#[derive(Deserialize)]
struct CountWire {
    count: usize,
}

#[derive(Deserialize)]
struct AiWire {
    count: usize,
    #[serde(default)]
    sessions: Vec<AiSessionWire>,
}

#[derive(Deserialize)]
struct AiSessionWire {
    id: String,
}

#[derive(Deserialize)]
struct LiveClaudeWire {
    blocking: usize,
    #[serde(default)]
    ai_plane: usize,
    unclassified: usize,
}

/// Parse a `/restart-readiness` body. A plane serialized as `null` means the
/// target could not determine it — that is UNKNOWN, never zero, so it is an
/// `Err` here and the caller falls back to the liveness arm. So is an AI plane
/// whose session list does not account for its count.
pub fn parse_readiness(body: &serde_json::Value) -> Result<ReadinessSummary, String> {
    // Tolerate an `ApiResponse` envelope, though the route serves the bare body.
    let body = match body.get("data") {
        Some(inner) if body.get("success").is_some() => inner,
        _ => body,
    };
    let wire: ReadinessWire = serde_json::from_value(body.clone())
        .map_err(|e| format!("unparseable /restart-readiness body: {e}"))?;
    let why = || {
        wire.reason
            .clone()
            .unwrap_or_else(|| "no reason given".into())
    };
    let (Some(terminal), Some(headless), Some(ai), Some(live)) = (
        wire.terminal_sessions.as_ref(),
        wire.headless_sessions.as_ref(),
        wire.ai_sessions.as_ref(),
        wire.live_claude.as_ref(),
    ) else {
        return Err(format!(
            "the target's session census has an undetermined plane ({})",
            why()
        ));
    };
    if ai.sessions.len() != ai.count {
        return Err(format!(
            "the target reports {} AI-plane session(s) but lists {} — they cannot all be \
             attributed",
            ai.count,
            ai.sessions.len()
        ));
    }
    Ok(ReadinessSummary {
        safe: wire.safe_to_restart,
        reason: wire.reason.clone().unwrap_or_default(),
        blocking: live.blocking,
        terminal_blocking: terminal.blocking_count,
        headless: headless.count,
        unclassified: live.unclassified,
        ai_plane_processes: live.ai_plane,
        ai_session_ids: ai.sessions.iter().map(|s| s.id.clone()).collect(),
    })
}

/// Classify a reqwest transport error for the wedged-target rule.
fn classify_health_error(e: &reqwest::Error) -> HealthAnswer {
    if e.is_timeout() {
        HealthAnswer::TimedOut
    } else if e.is_connect() {
        HealthAnswer::Refused
    } else {
        HealthAnswer::Failed(e.to_string())
    }
}

#[async_trait]
impl TargetProbe for HttpTargetProbe {
    async fn readiness(&self, port: u16) -> Result<ReadinessSummary, String> {
        let client = reqwest::Client::builder()
            .timeout(READINESS_TIMEOUT)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .get(format!("http://127.0.0.1:{port}/restart-readiness"))
            .send()
            .await
            .map_err(|e| format!("GET /restart-readiness failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET /restart-readiness returned HTTP {status}"));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("unparseable /restart-readiness body: {e}"))?;
        parse_readiness(&body)
    }

    async fn health(&self, port: u16) -> HealthAnswer {
        let client = match reqwest::Client::builder()
            .timeout(HEALTH_FALLBACK_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => return HealthAnswer::Failed(format!("http client: {e}")),
        };
        match client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
        {
            Ok(resp) => HealthAnswer::Answered(resp.status().as_u16()),
            Err(e) => classify_health_error(&e),
        }
    }

    async fn answering_pid(&self, port: u16) -> Result<u32, String> {
        let client = reqwest::Client::builder()
            .timeout(HEALTH_FALLBACK_TIMEOUT)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let body: serde_json::Value = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .map_err(|e| format!("GET /health failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("unparseable /health body: {e}"))?;
        body.pointer("/data/pid")
            .or_else(|| body.get("pid"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| "the /health answer carries no pid".to_string())
    }
}

// ============================================================================
// Host context threaded into the loop
// ============================================================================

/// What the loop engine needs from the runner hosting it to resolve and
/// perform restarts.
#[derive(Clone)]
pub struct LoopHost {
    /// This runner's children (`InstanceManager` in production).
    pub instances: Arc<dyn InstanceLifecycle>,
    /// Passed to relaunches so a slot's `spawn_placement` survives a restart.
    pub app: Option<tauri::AppHandle>,
    /// The port THIS runner's API is bound to (see [`self_port`]).
    pub self_port: u16,
    pub supervisor_probe: Arc<dyn SupervisorProbe>,
    pub target_probe: Arc<dyn TargetProbe>,
}

impl LoopHost {
    /// The production host: real instance manager, real HTTP probes.
    pub fn new(
        instances: Arc<InstanceManager>,
        app: Option<tauri::AppHandle>,
        bound_api_port: u16,
    ) -> Self {
        Self {
            instances,
            app,
            self_port: self_port(bound_api_port),
            supervisor_probe: Arc::new(HttpSupervisorProbe),
            target_probe: Arc::new(HttpTargetProbe),
        }
    }
}

// ============================================================================
// Resolution
// ============================================================================

/// Decide how `config`'s between-iterations mode will restart its target.
/// The rules are ordered; the first that matches decides:
///
/// 1. `WaitHealthy` / `None` → [`RestartPath::NotNeeded`].
/// 2. target port == this runner's port → `TargetIsOrchestrator` (restarting
///    it would end the loop — on dev boxes too).
/// 3. `rebuild: false` and a live child of THIS runner owns the port →
///    [`RestartPath::InstanceManager`], resolved BY PORT.
/// 4. `rebuild: true` → the dev supervisor answers its `/health` AND its
///    `GET /runners` lists exactly one runner on the target port →
///    [`RestartPath::DevSupervisor`] carrying THAT runner's id. The supervisor
///    runner id is resolved BY PORT and never defaulted: a defaulted id
///    (formerly `"primary"`) would have rebuilt the supervisor's primary — on a
///    dev box, the orchestrator itself — for a loop aimed at a secondary. An
///    explicit `target_runner_id` that disagrees with the port-matched id, no
///    matching row, an ambiguous match, an unreadable list, or no supervisor at
///    all → `RebuildNeedsDevSupervisor`.
/// 5. otherwise → `TargetNotRunnerManaged`. No silent supervisor fallback.
pub async fn resolve_restart_path(
    config: &OrchestrationLoopConfig,
    instances: &(impl InstanceLifecycle + ?Sized),
    self_port: u16,
    supervisor: &(impl SupervisorProbe + ?Sized),
) -> Result<RestartPath, RestartUnsupported> {
    let Some(rebuild) = restart_rebuild(&config.between_iterations) else {
        return Ok(RestartPath::NotNeeded);
    };
    let port = target_port(config, self_port);

    if port == self_port {
        return Err(RestartUnsupported {
            code: RestartUnsupportedCode::TargetIsOrchestrator,
            reason: format!(
                "the loop runs inside this runner (port {port}); restarting it would end the \
                 loop — target a secondary runner instance, or use Wait Healthy"
            ),
        });
    }

    if !rebuild {
        if let Some(owned) = instances.owned_instance_on_port(port).await {
            return Ok(RestartPath::InstanceManager {
                instance_id: owned.id,
                port,
            });
        }
    } else if supervisor.answers(config.supervisor_port).await {
        let runner_id = supervisor_runner_on_port(config, port, supervisor).await?;
        return Ok(RestartPath::DevSupervisor {
            runner_id,
            supervisor_port: config.supervisor_port,
        });
    } else {
        return Err(RestartUnsupported {
            code: RestartUnsupportedCode::RebuildNeedsDevSupervisor,
            reason: format!(
                "rebuild compiles the runner from a source checkout, which needs the dev \
                 supervisor (nothing answered on port {}); choose the no-rebuild variant",
                config.supervisor_port
            ),
        });
    }

    Err(RestartUnsupported {
        code: RestartUnsupportedCode::TargetNotRunnerManaged,
        reason: format!(
            "the runner on port {port} was not started by this runner, so it cannot restart \
             it; launch it from Settings → Runner Instances"
        ),
    })
}

/// The supervisor runner id that owns `port`, read from the supervisor's own
/// `GET /runners` — never defaulted, never taken from the config on trust.
async fn supervisor_runner_on_port(
    config: &OrchestrationLoopConfig,
    port: u16,
    supervisor: &(impl SupervisorProbe + ?Sized),
) -> Result<String, RestartUnsupported> {
    let refuse = |reason: String| RestartUnsupported {
        code: RestartUnsupportedCode::RebuildNeedsDevSupervisor,
        reason,
    };
    let sup = config.supervisor_port;
    let rows = supervisor.runners(sup).await.map_err(|cause| {
        refuse(format!(
            "the dev supervisor on port {sup} answered /health but its runner list is \
             unreadable ({cause}), so the runner to rebuild cannot be identified"
        ))
    })?;
    let on_port: Vec<SupervisorRunnerRow> = rows.into_iter().filter(|r| r.port == port).collect();
    if let Some(primary) = on_port.iter().find(|r| r.is_primary()) {
        return Err(refuse(format!(
            "the runner on :{port} is the dev supervisor's PRIMARY ('{}'); the loop never asks \
             the supervisor to restart the primary",
            primary.id
        )));
    }
    let mut ids: Vec<String> = on_port.into_iter().map(|r| r.id).collect();
    ids.sort();
    ids.dedup();
    let matched = match ids.as_slice() {
        [] => {
            return Err(refuse(format!(
                "the dev supervisor does not manage the runner on :{port}"
            )))
        }
        [one] => one.clone(),
        many => {
            return Err(refuse(format!(
                "the dev supervisor lists more than one runner on :{port} ({}); refusing to \
                 guess which to rebuild",
                many.join(", ")
            )))
        }
    };
    match config.target_runner_id.as_deref() {
        Some(named) if named != matched => Err(refuse(format!(
            "the configured supervisor runner '{named}' is not the runner on :{port} (the dev \
             supervisor lists '{matched}' there); fix the target so both name the same runner"
        ))),
        _ => Ok(matched),
    }
}

/// The wire verdict for a resolution.
pub fn to_capability(
    resolved: &Result<RestartPath, RestartUnsupported>,
    target_port: u16,
) -> RestartCapability {
    match resolved {
        Ok(path) => RestartCapability {
            supported: true,
            path: Some(path.kind()),
            code: None,
            reason: None,
            target_port,
            instance_id: match path {
                RestartPath::InstanceManager { instance_id, .. } => Some(instance_id.clone()),
                _ => None,
            },
        },
        Err(unsupported) => RestartCapability {
            supported: false,
            path: None,
            code: Some(unsupported.code),
            reason: Some(unsupported.reason.clone()),
            target_port,
            instance_id: None,
        },
    }
}

/// Resolve `config` against `host` and return the wire verdict — the body of
/// `POST /orchestration-loop/restart-capability` and its Tauri twin. Read-only:
/// it starts nothing and restarts nothing.
pub async fn restart_capability(
    config: &OrchestrationLoopConfig,
    host: &LoopHost,
) -> RestartCapability {
    let resolved = resolve_restart_path(
        config,
        host.instances.as_ref(),
        host.self_port,
        host.supervisor_probe.as_ref(),
    )
    .await;
    to_capability(&resolved, target_port(config, host.self_port))
}

// ============================================================================
// Performing an in-process restart
// ============================================================================

/// Why one between-iterations restart of a runner-owned target failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartIterationError {
    /// The port is no longer a live child of this runner with the resolved
    /// slot id — someone stopped (or replaced) it by hand.
    NoLongerRunnerManaged { port: u16, instance_id: String },
    /// The target runs sessions the loop cannot prove are its own; restarting
    /// would destroy them.
    TargetHasForeignSessions { port: u16, foreign: ForeignSessions },
    /// `/restart-readiness` was unobtainable while something still answers on
    /// the port (or might) — the runner cannot say what it is running.
    TargetReadinessUnknown { port: u16, cause: String },
    /// The stop/relaunch itself failed.
    Restart(InstanceRestartError),
    /// After the relaunch the process answering on the port is not the child
    /// that was just launched.
    NotTheNewChild {
        port: u16,
        expected_pid: u32,
        found: Result<u32, String>,
    },
}

impl std::fmt::Display for RestartIterationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLongerRunnerManaged { port, instance_id } => write!(
                f,
                "target no longer runner-managed (stopped by hand?): port {port} is not a live \
                 child '{instance_id}' of this runner any more"
            ),
            Self::TargetHasForeignSessions { port, foreign } => write!(
                f,
                "target_has_foreign_sessions: the runner on port {port} is running work this \
                 loop did not start ({} terminal-hosted in flight, {} headless, {} unclassified, \
                 {} AI-plane process(es), {} AI-plane session(s) not started by this loop); not \
                 restarting it",
                foreign.terminal_blocking,
                foreign.headless,
                foreign.unclassified,
                foreign.ai_plane_processes,
                foreign.ai_sessions_not_ours
            ),
            Self::TargetReadinessUnknown { port, cause } => write!(
                f,
                "target_readiness_unknown: the runner on port {port} did not answer \
                 /restart-readiness and is not provably dead ({cause}); not restarting work the \
                 loop cannot see"
            ),
            Self::Restart(e) => write!(f, "in-process restart failed: {e}"),
            Self::NotTheNewChild {
                port,
                expected_pid,
                found,
            } => match found {
                Ok(pid) => write!(
                    f,
                    "after the restart, port {port} is answered by PID {pid}, not the relaunched \
                     child PID {expected_pid}"
                ),
                Err(e) => write!(
                    f,
                    "after the restart, the process on port {port} could not be identified as the \
                     relaunched child PID {expected_pid}: {e}"
                ),
            },
        }
    }
}

impl std::error::Error for RestartIterationError {}

/// What the readiness gate concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessOutcome {
    /// Readiness answered and nothing foreign is running.
    Clear,
    /// Readiness was unobtainable AND every `/health` probe was refused —
    /// nothing is listening, the child's sessions are already dark, and the
    /// restart IS the recovery.
    WedgedProceed,
}

/// The gate before `stop_instance` (Design §3). `own_runs` are the task runs
/// this loop started on the target (see [`ReadinessSummary::foreign`]).
///
/// An unobtainable readiness answer never licenses a kill on its own: the
/// target is treated as wedged only when [`HEALTH_FALLBACK_ATTEMPTS`] `/health`
/// probes over ~30 s were ALL refused (nothing listening). A timeout, any HTTP
/// answer (2xx or not) or any other transport failure means something may be
/// alive → [`RestartIterationError::TargetReadinessUnknown`].
pub async fn check_readiness(
    probe: &(impl TargetProbe + ?Sized),
    port: u16,
    own_runs: &BTreeSet<String>,
) -> Result<ReadinessOutcome, RestartIterationError> {
    let cause = match probe.readiness(port).await {
        Ok(summary) => {
            let foreign = summary.foreign(own_runs);
            // The target's own verdict is authoritative for anything this
            // gate does not model. With nothing foreign, the ONLY unsafe
            // condition the gate can explain is AI sessions this loop
            // launched; an unsafe verdict with none of those is a condition
            // the gate cannot see, so it refuses rather than kills.
            if foreign.is_empty() && !summary.safe && summary.ai_session_ids.is_empty() {
                return Err(RestartIterationError::TargetReadinessUnknown {
                    port,
                    cause: format!(
                        "the target reports safe_to_restart=false for a reason this gate does \
                         not model ({})",
                        if summary.reason.is_empty() {
                            "no reason given"
                        } else {
                            summary.reason.as_str()
                        }
                    ),
                });
            }
            return if foreign.is_empty() {
                if !summary.ai_session_ids.is_empty() {
                    info!(
                        "Restart target :{port}: {} AI-plane session(s) remain, all started by \
                         this loop",
                        summary.ai_session_ids.len()
                    );
                }
                Ok(ReadinessOutcome::Clear)
            } else {
                Err(RestartIterationError::TargetHasForeignSessions { port, foreign })
            };
        }
        Err(cause) => cause,
    };

    for attempt in 0..HEALTH_FALLBACK_ATTEMPTS {
        match probe.health(port).await {
            HealthAnswer::Refused => {}
            other => {
                return Err(RestartIterationError::TargetReadinessUnknown {
                    port,
                    cause: format!("{cause}; /health probe {} → {other:?}", attempt + 1),
                })
            }
        }
        if attempt + 1 < HEALTH_FALLBACK_ATTEMPTS {
            tokio::time::sleep(probe.health_probe_gap()).await;
        }
    }
    warn!(
        "Restart target :{port} answers neither /restart-readiness ({cause}) nor any of \
         {HEALTH_FALLBACK_ATTEMPTS} /health probes (all refused) — treating it as wedged and \
         restarting it as the recovery"
    );
    Ok(ReadinessOutcome::WedgedProceed)
}

/// One between-iterations restart of a runner-owned target: re-check it is
/// still ours → readiness gate → [`crate::instance_manager::restart_with`].
/// Returns the new child's PID; the caller waits for it to be healthy and then
/// confirms it with [`verify_new_child`].
pub async fn restart_owned_target(
    instances: &(impl InstanceLifecycle + ?Sized),
    probe: &(impl TargetProbe + ?Sized),
    instance_id: &str,
    port: u16,
    own_runs: &BTreeSet<String>,
    app: Option<&tauri::AppHandle>,
) -> Result<u32, RestartIterationError> {
    match instances.owned_instance_on_port(port).await {
        Some(owned) if owned.id == instance_id => {}
        _ => {
            return Err(RestartIterationError::NoLongerRunnerManaged {
                port,
                instance_id: instance_id.to_string(),
            })
        }
    }

    check_readiness(probe, port, own_runs).await?;

    crate::instance_manager::restart_with(instances, instance_id, app)
        .await
        .map_err(RestartIterationError::Restart)
}

/// After a relaunch has come up healthy: the process answering `/health` on
/// `port` must be the child just launched (`expected_pid`), not a stale or
/// foreign process that grabbed the port.
pub async fn verify_new_child(
    probe: &(impl TargetProbe + ?Sized),
    port: u16,
    expected_pid: u32,
) -> Result<(), RestartIterationError> {
    match probe.answering_pid(port).await {
        Ok(pid) if pid == expected_pid => Ok(()),
        found => Err(RestartIterationError::NotTheNewChild {
            port,
            expected_pid,
            found,
        }),
    }
}

/// What a completed restart left for the caller to confirm once the target
/// is healthy again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartOutcome {
    /// The PID the answering process must have (in-process restarts only).
    pub expect_pid: Option<u32>,
}

/// Performs the restart a loop resolved at start. Built once per loop; a
/// [`SupervisorClient`] exists ONLY on the `DevSupervisor` path.
pub struct TargetRestarter {
    mode: RestarterMode,
    host: LoopHost,
}

enum RestarterMode {
    InstanceManager {
        instance_id: String,
        port: u16,
    },
    DevSupervisor {
        client: SupervisorClient,
        runner_id: String,
    },
    NotNeeded,
}

impl TargetRestarter {
    pub fn new(path: &RestartPath, host: LoopHost) -> Self {
        let mode = match path {
            RestartPath::InstanceManager { instance_id, port } => RestarterMode::InstanceManager {
                instance_id: instance_id.clone(),
                port: *port,
            },
            RestartPath::DevSupervisor {
                runner_id,
                supervisor_port,
            } => RestarterMode::DevSupervisor {
                client: SupervisorClient::new(*supervisor_port),
                runner_id: runner_id.clone(),
            },
            RestartPath::NotNeeded => RestarterMode::NotNeeded,
        };
        Self { mode, host }
    }

    /// A short description for logs.
    pub fn describe(&self) -> String {
        match &self.mode {
            RestarterMode::InstanceManager { instance_id, port } => {
                format!("runner-managed instance '{instance_id}' on :{port}")
            }
            RestarterMode::DevSupervisor { runner_id, .. } => {
                format!("supervisor runner '{runner_id}'")
            }
            RestarterMode::NotNeeded => "no restart target".to_string(),
        }
    }

    /// Restart the target. `rebuild` is honoured only by the supervisor path;
    /// resolution guarantees the in-process path is only chosen for
    /// `rebuild: false`. `own_runs` are the task runs this loop started on the
    /// target. The supervisor path returns only once a detached rebuild-restart
    /// is terminal.
    pub async fn restart(
        &self,
        rebuild: bool,
        own_runs: &BTreeSet<String>,
        stop_rx: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<RestartOutcome, String> {
        match &self.mode {
            RestarterMode::InstanceManager { instance_id, port } => {
                let pid = restart_owned_target(
                    self.host.instances.as_ref(),
                    self.host.target_probe.as_ref(),
                    instance_id,
                    *port,
                    own_runs,
                    self.host.app.as_ref(),
                )
                .await
                .map_err(|e| e.to_string())?;
                info!("Relaunched runner-managed instance '{instance_id}' on :{port} (PID {pid})");
                Ok(RestartOutcome {
                    expect_pid: Some(pid),
                })
            }
            RestarterMode::DevSupervisor { client, runner_id } => {
                client
                    .restart_runner(runner_id, rebuild, DETACHED_REBUILD_TIMEOUT, stop_rx)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(RestartOutcome { expect_pid: None })
            }
            RestarterMode::NotNeeded => Err(
                "internal: a restart was requested for a loop whose mode was resolved as \
                 never restarting"
                    .to_string(),
            ),
        }
    }

    /// Once the target is healthy after [`Self::restart`], confirm the
    /// process answering is the relaunched child.
    pub async fn confirm_restarted(&self, outcome: RestartOutcome) -> Result<(), String> {
        match (&self.mode, outcome.expect_pid) {
            (RestarterMode::InstanceManager { port, .. }, Some(pid)) => {
                verify_new_child(self.host.target_probe.as_ref(), *port, pid)
                    .await
                    .map_err(|e| e.to_string())
            }
            _ => Ok(()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::RunnerInstanceConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    const SELF_PORT: u16 = 9876;

    fn config(between: BetweenIterations, target: Option<u16>) -> OrchestrationLoopConfig {
        let mut value = serde_json::json!({
            "workflowId": "wf-1",
            "betweenIterations": serde_json::to_value(&between).unwrap(),
        });
        if let Some(p) = target {
            value["targetRunnerPort"] = serde_json::json!(p);
        }
        serde_json::from_value(value).expect("minimal OrchestrationLoopConfig")
    }

    fn slot(id: &str, port: u16) -> RunnerInstanceConfig {
        RunnerInstanceConfig {
            id: id.to_string(),
            name: id.to_string(),
            port,
            spawn_placement: None,
        }
    }

    /// A fixed table of live children plus a scripted restart.
    #[derive(Default)]
    struct FakeInstances {
        live: StdMutex<Vec<RunnerInstanceConfig>>,
        calls: StdMutex<Vec<String>>,
    }

    impl FakeInstances {
        fn with(live: Vec<RunnerInstanceConfig>) -> Self {
            Self {
                live: StdMutex::new(live),
                calls: StdMutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl InstanceLifecycle for FakeInstances {
        async fn owned_instance_on_port(&self, port: u16) -> Option<RunnerInstanceConfig> {
            self.live
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.port == port)
                .cloned()
        }
        async fn owned_instance(&self, id: &str) -> Option<RunnerInstanceConfig> {
            self.live
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.id == id)
                .cloned()
        }
        async fn stop(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("stop:{id}"));
            Ok(())
        }
        async fn wait_port_free(&self, port: u16, _timeout: Duration) -> bool {
            self.calls.lock().unwrap().push(format!("wait_free:{port}"));
            true
        }
        async fn launch(
            &self,
            config: &RunnerInstanceConfig,
            _app: Option<&tauri::AppHandle>,
        ) -> Result<u32, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("launch:{}", config.id));
            Ok(777)
        }
    }

    /// A supervisor probe that counts calls and answers a fixed value.
    struct FakeSupervisor {
        up: bool,
        rows: Result<Vec<SupervisorRunnerRow>, String>,
        calls: AtomicUsize,
    }

    impl FakeSupervisor {
        fn new(up: bool) -> Self {
            Self::with_rows(up, Ok(Vec::new()))
        }
        fn with_rows(up: bool, rows: Result<Vec<SupervisorRunnerRow>, String>) -> Self {
            Self {
                up,
                rows,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SupervisorProbe for FakeSupervisor {
        async fn answers(&self, _port: u16) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.up
        }
        async fn runners(&self, _port: u16) -> Result<Vec<SupervisorRunnerRow>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.rows.clone()
        }
    }

    fn row(id: &str, port: u16) -> SupervisorRunnerRow {
        SupervisorRunnerRow {
            id: id.to_string(),
            port,
            kind: None,
        }
    }

    fn kinded(id: &str, port: u16, kind: &str) -> SupervisorRunnerRow {
        SupervisorRunnerRow {
            kind: Some(serde_json::json!({ "type": kind })),
            ..row(id, port)
        }
    }

    /// The dev box's shape: the supervisor's primary on the orchestrator's own
    /// port, and two test runners.
    fn dev_box_rows() -> Result<Vec<SupervisorRunnerRow>, String> {
        Ok(vec![
            kinded("primary", SELF_PORT, "primary"),
            kinded("test-1", 9877, "temp"),
            kinded("test-2", 9878, "temp"),
        ])
    }

    fn rebuild_cfg(target: u16, named: Option<&str>) -> OrchestrationLoopConfig {
        let mut cfg = config(
            BetweenIterations::RestartOnSignal { rebuild: true },
            Some(target),
        );
        cfg.supervisor_port = 19875;
        cfg.target_runner_id = named.map(str::to_string);
        cfg
    }

    /// A scripted [`TargetProbe`]: a readiness answer, a queue of `/health`
    /// answers (the last one repeats), and an answering PID.
    struct FakeProbe {
        readiness: Result<ReadinessSummary, String>,
        health: StdMutex<Vec<HealthAnswer>>,
        health_calls: AtomicUsize,
        pid: Result<u32, String>,
    }

    impl FakeProbe {
        fn new(readiness: Result<ReadinessSummary, String>, health: Vec<HealthAnswer>) -> Self {
            Self {
                readiness,
                health: StdMutex::new(health),
                health_calls: AtomicUsize::new(0),
                pid: Ok(777),
            }
        }
        fn quiet() -> Self {
            Self::new(Ok(quiet()), vec![HealthAnswer::Answered(200)])
        }
    }

    #[async_trait]
    impl TargetProbe for FakeProbe {
        async fn readiness(&self, _port: u16) -> Result<ReadinessSummary, String> {
            self.readiness.clone()
        }
        async fn health(&self, _port: u16) -> HealthAnswer {
            self.health_calls.fetch_add(1, Ordering::SeqCst);
            let mut q = self.health.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            }
        }
        async fn answering_pid(&self, _port: u16) -> Result<u32, String> {
            self.pid.clone()
        }
        fn health_probe_gap(&self) -> Duration {
            Duration::ZERO
        }
    }

    fn quiet() -> ReadinessSummary {
        ReadinessSummary {
            safe: true,
            reason: String::new(),
            blocking: 0,
            terminal_blocking: 0,
            headless: 0,
            unclassified: 0,
            ai_plane_processes: 0,
            ai_session_ids: Vec::new(),
        }
    }

    fn no_runs() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn runs(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    // --- resolver arms ------------------------------------------------------

    #[tokio::test]
    async fn wait_healthy_and_none_never_need_a_restart_even_against_self() {
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::new(false);
        for mode in [BetweenIterations::WaitHealthy, BetweenIterations::None] {
            let got = resolve_restart_path(&config(mode, None), &inst, SELF_PORT, &sup).await;
            assert_eq!(got, Ok(RestartPath::NotNeeded));
        }
        assert_eq!(sup.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_restart_aimed_at_this_runner_is_target_is_orchestrator_even_with_a_supervisor() {
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::new(true);
        for mode in [
            BetweenIterations::RestartRunner { rebuild: false },
            BetweenIterations::RestartRunner { rebuild: true },
            BetweenIterations::RestartOnSignal { rebuild: true },
        ] {
            // Both the implicit default (None) and the explicit self port.
            for target in [None, Some(SELF_PORT)] {
                let err =
                    resolve_restart_path(&config(mode.clone(), target), &inst, SELF_PORT, &sup)
                        .await
                        .unwrap_err();
                assert_eq!(err.code, RestartUnsupportedCode::TargetIsOrchestrator);
                assert!(err
                    .to_string()
                    .starts_with("unsupported here: target_is_orchestrator: "));
            }
        }
        assert_eq!(
            sup.calls.load(Ordering::SeqCst),
            0,
            "the self check precedes the supervisor probe"
        );
    }

    #[tokio::test]
    async fn a_no_rebuild_restart_of_an_owned_child_resolves_by_port_to_the_instance_manager() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877), slot("slot-b", 9878)]);
        let sup = FakeSupervisor::new(true);
        let mut cfg = config(
            BetweenIterations::RestartOnSignal { rebuild: false },
            Some(9878),
        );
        // A supervisor id that names something else entirely must not matter.
        cfg.target_runner_id = Some("test-runner-xyz".into());
        let got = resolve_restart_path(&cfg, &inst, SELF_PORT, &sup).await;
        assert_eq!(
            got,
            Ok(RestartPath::InstanceManager {
                instance_id: "slot-b".into(),
                port: 9878
            })
        );
        assert_eq!(
            sup.calls.load(Ordering::SeqCst),
            0,
            "a runner-managed restart never consults the supervisor, even when one is up"
        );
    }

    #[tokio::test]
    async fn a_no_rebuild_restart_of_an_unowned_port_is_target_not_runner_managed() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let sup = FakeSupervisor::new(true);
        let cfg = config(
            BetweenIterations::RestartRunner { rebuild: false },
            Some(9890),
        );
        let err = resolve_restart_path(&cfg, &inst, SELF_PORT, &sup)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::TargetNotRunnerManaged);
        assert!(err.reason.contains("9890"));
        assert_eq!(
            sup.calls.load(Ordering::SeqCst),
            0,
            "no silent supervisor fallback for a plain restart"
        );
    }

    #[tokio::test]
    async fn a_rebuild_restart_uses_the_supervisor_only_when_it_answers() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let mut cfg = config(
            BetweenIterations::RestartRunner { rebuild: true },
            Some(9877),
        );
        cfg.supervisor_port = 19875;
        cfg.target_runner_id = Some("test-1".into());

        let up = FakeSupervisor::with_rows(true, dev_box_rows());
        assert_eq!(
            resolve_restart_path(&cfg, &inst, SELF_PORT, &up).await,
            Ok(RestartPath::DevSupervisor {
                runner_id: "test-1".into(),
                supervisor_port: 19875
            })
        );

        let down = FakeSupervisor::new(false);
        let err = resolve_restart_path(&cfg, &inst, SELF_PORT, &down)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(err.reason.contains("19875"));
    }

    #[tokio::test]
    async fn a_rebuild_uses_the_supervisor_runner_matched_by_port_when_the_config_names_none() {
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::with_rows(true, dev_box_rows());
        assert_eq!(
            resolve_restart_path(&rebuild_cfg(9878, None), &inst, SELF_PORT, &sup).await,
            Ok(RestartPath::DevSupervisor {
                runner_id: "test-2".into(),
                supervisor_port: 19875
            })
        );
    }

    #[tokio::test]
    async fn the_supervisors_primary_is_never_chosen_for_a_secondarys_port() {
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::with_rows(true, dev_box_rows());
        for target in [9877, 9878] {
            match resolve_restart_path(&rebuild_cfg(target, None), &inst, SELF_PORT, &sup).await {
                Ok(RestartPath::DevSupervisor { runner_id, .. }) => {
                    assert_ne!(runner_id, "primary", "target :{target}")
                }
                other => panic!("target :{target}: {other:?}"),
            }
        }
        // The primary's own port resolves to TargetIsOrchestrator first (rule 2);
        // a supervisor primary on a DIFFERENT port is refused by kind.
        let sup_primary_elsewhere =
            FakeSupervisor::with_rows(true, Ok(vec![kinded("primary", 9879, "primary")]));
        let err = resolve_restart_path(
            &rebuild_cfg(9879, Some("primary")),
            &inst,
            SELF_PORT,
            &sup_primary_elsewhere,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(err.reason.contains("PRIMARY"), "{}", err.reason);
        // And naming "primary" for a secondary's port is refused, not obeyed.
        let err = resolve_restart_path(&rebuild_cfg(9877, Some("primary")), &inst, SELF_PORT, &sup)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(
            err.reason.contains("'primary'") && err.reason.contains("'test-1'"),
            "{}",
            err.reason
        );
    }

    #[tokio::test]
    async fn a_rebuild_of_a_port_the_supervisor_does_not_manage_is_refused() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9890)]);
        let sup = FakeSupervisor::with_rows(true, dev_box_rows());
        let err = resolve_restart_path(&rebuild_cfg(9890, None), &inst, SELF_PORT, &sup)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert_eq!(
            err.reason,
            "the dev supervisor does not manage the runner on :9890"
        );
    }

    #[tokio::test]
    async fn an_explicit_supervisor_id_that_disagrees_with_the_port_is_refused() {
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::with_rows(true, dev_box_rows());
        let err = resolve_restart_path(&rebuild_cfg(9877, Some("test-2")), &inst, SELF_PORT, &sup)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(err.reason.contains("'test-2'") && err.reason.contains("'test-1'"));
        // The agreeing id is accepted.
        assert!(matches!(
            resolve_restart_path(&rebuild_cfg(9877, Some("test-1")), &inst, SELF_PORT, &sup).await,
            Ok(RestartPath::DevSupervisor { ref runner_id, .. }) if runner_id == "test-1"
        ));
    }

    #[tokio::test]
    async fn an_unreadable_or_ambiguous_supervisor_list_is_refused() {
        let inst = FakeInstances::default();
        let unreadable = FakeSupervisor::with_rows(true, Err("HTTP 500".into()));
        let err = resolve_restart_path(&rebuild_cfg(9877, None), &inst, SELF_PORT, &unreadable)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(err.reason.contains("unreadable"));

        let ambiguous = FakeSupervisor::with_rows(true, Ok(vec![row("a", 9877), row("b", 9877)]));
        let err = resolve_restart_path(&rebuild_cfg(9877, None), &inst, SELF_PORT, &ambiguous)
            .await
            .unwrap_err();
        assert!(err.reason.contains("more than one"));
    }

    #[test]
    fn the_supervisor_runner_list_parses_the_bare_array_it_serves() {
        let body = serde_json::json!([
            {"id": "primary", "name": "Primary", "port": 9876, "running": true, "kind": {"type": "primary"}},
            {"id": "test-1", "name": "t", "port": 9877, "running": false},
        ]);
        let rows = parse_supervisor_runners(&body).unwrap();
        assert_eq!(rows[0], kinded("primary", 9876, "primary"));
        assert!(rows[0].is_primary());
        assert_eq!(rows[1], row("test-1", 9877));
        assert!(!rows[1].is_primary());
        // An older supervisor serves no `kind`: the id is the only signal.
        assert!(row("primary", 9876).is_primary());
        assert!(parse_supervisor_runners(&serde_json::json!({"x": 1})).is_err());
    }

    #[tokio::test]
    async fn the_real_supervisor_probe_reads_a_closed_port_as_absent() {
        // Bind then drop, so the port is (almost certainly) closed.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(!HttpSupervisorProbe.answers(port).await);
    }

    #[test]
    fn capability_mirrors_the_resolution() {
        let ok = to_capability(
            &Ok(RestartPath::InstanceManager {
                instance_id: "slot-a".into(),
                port: 9877,
            }),
            9877,
        );
        assert!(ok.supported);
        assert_eq!(ok.path, Some(RestartPathKind::InstanceManager));
        assert_eq!(ok.instance_id.as_deref(), Some("slot-a"));
        assert_eq!(ok.code, None);

        let refused = to_capability(
            &Err(RestartUnsupported {
                code: RestartUnsupportedCode::TargetIsOrchestrator,
                reason: "r".into(),
            }),
            9876,
        );
        assert!(!refused.supported);
        assert_eq!(refused.path, None);
        assert_eq!(
            refused.code,
            Some(RestartUnsupportedCode::TargetIsOrchestrator)
        );
        assert_eq!(refused.reason.as_deref(), Some("r"));
        assert_eq!(refused.target_port, 9876);
    }

    #[test]
    fn code_tokens_match_the_serde_wire_form() {
        for code in [
            RestartUnsupportedCode::TargetIsOrchestrator,
            RestartUnsupportedCode::TargetNotRunnerManaged,
            RestartUnsupportedCode::RebuildNeedsDevSupervisor,
        ] {
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::json!(code_token(code))
            );
        }
    }

    // --- readiness parsing --------------------------------------------------

    #[test]
    fn readiness_parses_the_planes_and_treats_a_null_plane_as_unknown() {
        let body = serde_json::json!({
            "safe_to_restart": false,
            "reason": "x",
            "terminal_sessions": {"count": 3, "blocking_count": 2},
            "headless_sessions": {"count": 1},
            "ai_sessions": {"count": 2, "sessions": [{"id": "run-a"}, {"id": "run-b"}]},
            "live_claude": {"total": 10, "blocking": 6, "ai_plane": 2, "unclassified": 1},
        });
        let s = parse_readiness(&body).unwrap();
        assert_eq!(
            s,
            ReadinessSummary {
                safe: false,
                reason: "x".into(),
                blocking: 6,
                terminal_blocking: 2,
                headless: 1,
                unclassified: 1,
                ai_plane_processes: 2,
                ai_session_ids: vec!["run-a".into(), "run-b".into()],
            }
        );

        let unknown = serde_json::json!({
            "reason": "process table unreadable",
            "terminal_sessions": null,
            "headless_sessions": null,
            "ai_sessions": {"count": 0, "sessions": []},
            "live_claude": null,
        });
        let err = parse_readiness(&unknown).unwrap_err();
        assert!(err.contains("process table unreadable"), "{err}");

        let unlisted = serde_json::json!({
            "terminal_sessions": {"count": 0, "blocking_count": 0},
            "headless_sessions": {"count": 0},
            "ai_sessions": {"count": 1, "sessions": []},
            "live_claude": {"total": 0, "blocking": 0, "ai_plane": 0, "unclassified": 0},
        });
        assert!(
            parse_readiness(&unlisted).is_err(),
            "an unlisted AI session is unattributable"
        );
    }

    // --- the foreign-session rule -----------------------------------------

    #[test]
    fn only_ai_sessions_this_loop_started_are_discounted() {
        let own = runs(&["run-a"]);
        let mine = ReadinessSummary {
            safe: false,
            ai_session_ids: vec!["run-a".into()],
            ..quiet()
        };
        assert!(mine.foreign(&own).is_empty(), "the loop's own AI session");

        let theirs = ReadinessSummary {
            safe: false,
            ai_session_ids: vec!["run-a".into(), "run-x".into()],
            ..quiet()
        };
        assert_eq!(theirs.foreign(&own).ai_sessions_not_ours, 1);
        assert_eq!(theirs.foreign(&no_runs()).ai_sessions_not_ours, 2);
    }

    #[test]
    fn every_blocking_process_is_foreign_including_ai_plane_processes() {
        // An AI-plane `claude` process carries no run id, so even while every
        // AI session is the loop's own, the process blocks.
        let s = ReadinessSummary {
            safe: false,
            blocking: 1,
            ai_plane_processes: 1,
            ai_session_ids: vec!["run-a".into()],
            ..quiet()
        };
        assert_eq!(s.foreign(&runs(&["run-a"])).ai_plane_processes, 1);

        // Blocking the per-plane counts do not explain is never dropped.
        let unexplained = ReadinessSummary {
            safe: false,
            blocking: 3,
            terminal_blocking: 1,
            ..quiet()
        };
        let f = unexplained.foreign(&no_runs());
        assert_eq!(f.terminal_blocking + f.ai_plane_processes, 3);
    }

    // --- restart_owned_target arms ----------------------------------------

    #[tokio::test]
    async fn an_owned_quiet_target_is_stopped_freed_and_relaunched() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let pid =
            restart_owned_target(&inst, &FakeProbe::quiet(), "slot-a", 9877, &no_runs(), None)
                .await
                .unwrap();
        assert_eq!(pid, 777);
        assert_eq!(
            inst.calls(),
            vec!["stop:slot-a", "wait_free:9877", "launch:slot-a"]
        );
    }

    #[tokio::test]
    async fn a_child_stopped_by_hand_between_iterations_is_a_typed_error() {
        let inst = FakeInstances::default(); // nothing live any more
        let err =
            restart_owned_target(&inst, &FakeProbe::quiet(), "slot-a", 9877, &no_runs(), None)
                .await
                .unwrap_err();
        assert!(matches!(
            err,
            RestartIterationError::NoLongerRunnerManaged { .. }
        ));
        assert!(err.to_string().contains("stopped by hand?"));
        assert!(inst.calls().is_empty());
    }

    #[tokio::test]
    async fn a_port_now_owned_by_a_different_slot_is_not_restarted() {
        let inst = FakeInstances::with(vec![slot("slot-other", 9877)]);
        let err =
            restart_owned_target(&inst, &FakeProbe::quiet(), "slot-a", 9877, &no_runs(), None)
                .await
                .unwrap_err();
        assert!(matches!(
            err,
            RestartIterationError::NoLongerRunnerManaged { .. }
        ));
        assert!(inst.calls().is_empty());
    }

    #[tokio::test]
    async fn foreign_sessions_on_the_target_refuse_the_restart_naming_the_counts() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(
            Ok(ReadinessSummary {
                safe: false,
                blocking: 3,
                terminal_blocking: 2,
                headless: 1,
                ..quiet()
            }),
            vec![HealthAnswer::Answered(200)],
        );
        let err = restart_owned_target(&inst, &probe, "slot-a", 9877, &no_runs(), None)
            .await
            .unwrap_err();
        match &err {
            RestartIterationError::TargetHasForeignSessions { port, foreign } => {
                assert_eq!(*port, 9877);
                assert_eq!(foreign.terminal_blocking, 2);
                assert_eq!(foreign.headless, 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(err.to_string().contains("2 terminal-hosted"));
        assert!(inst.calls().is_empty(), "nothing is stopped");
    }

    #[tokio::test]
    async fn an_ai_session_the_loop_did_not_start_refuses_the_restart() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(
            Ok(ReadinessSummary {
                safe: false,
                ai_session_ids: vec!["someone-elses-run".into()],
                ..quiet()
            }),
            vec![HealthAnswer::Answered(200)],
        );
        let err = restart_owned_target(&inst, &probe, "slot-a", 9877, &runs(&["run-a"]), None)
            .await
            .unwrap_err();
        match err {
            RestartIterationError::TargetHasForeignSessions { foreign, .. } => {
                assert_eq!(foreign.ai_sessions_not_ours, 1)
            }
            other => panic!("{other:?}"),
        }
        assert!(inst.calls().is_empty());
    }

    #[tokio::test]
    async fn the_loops_own_ai_sessions_alone_do_not_block_the_restart() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(
            Ok(ReadinessSummary {
                safe: false,
                ai_session_ids: vec!["run-a".into(), "run-b".into()],
                ..quiet()
            }),
            vec![HealthAnswer::Answered(200)],
        );
        restart_owned_target(
            &inst,
            &probe,
            "slot-a",
            9877,
            &runs(&["run-a", "run-b"]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(inst.calls().len(), 3);
    }

    #[tokio::test]
    async fn an_unsafe_verdict_the_gate_cannot_explain_refuses() {
        // Nothing foreign by the gate's own accounting, no AI session of the
        // loop's either — yet the target says unsafe: refuse, never kill.
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(
            Ok(ReadinessSummary {
                safe: false,
                reason: "an unreadable lifecycle store".into(),
                ..quiet()
            }),
            vec![HealthAnswer::Answered(200)],
        );
        let err = restart_owned_target(&inst, &probe, "slot-a", 9877, &no_runs(), None)
            .await
            .unwrap_err();
        match &err {
            RestartIterationError::TargetReadinessUnknown { cause, .. } => {
                assert!(cause.contains("an unreadable lifecycle store"), "{cause}")
            }
            other => panic!("{other:?}"),
        }
        assert!(inst.calls().is_empty(), "nothing is stopped");
    }

    #[tokio::test]
    async fn unsafe_only_because_of_the_loops_own_ai_sessions_proceeds() {
        let probe = FakeProbe::new(
            Ok(ReadinessSummary {
                safe: false,
                ai_session_ids: vec!["run-a".into()],
                ..quiet()
            }),
            vec![HealthAnswer::Answered(200)],
        );
        assert_eq!(
            check_readiness(&probe, 9877, &runs(&["run-a"])).await,
            Ok(ReadinessOutcome::Clear)
        );
    }

    async fn unknown_readiness_with(
        answers: Vec<HealthAnswer>,
    ) -> (FakeProbe, Result<ReadinessOutcome, RestartIterationError>) {
        let probe = FakeProbe::new(Err("timed out".into()), answers);
        let got = check_readiness(&probe, 9877, &no_runs()).await;
        (probe, got)
    }

    #[tokio::test]
    async fn unobtainable_readiness_with_health_answering_2xx_fails_typed() {
        let (_, got) = unknown_readiness_with(vec![HealthAnswer::Answered(200)]).await;
        assert!(matches!(
            got,
            Err(RestartIterationError::TargetReadinessUnknown { port: 9877, .. })
        ));
    }

    #[tokio::test]
    async fn unobtainable_readiness_with_health_answering_non_2xx_fails_typed() {
        let (_, got) = unknown_readiness_with(vec![HealthAnswer::Answered(503)]).await;
        assert!(matches!(
            got,
            Err(RestartIterationError::TargetReadinessUnknown { .. })
        ));
    }

    #[tokio::test]
    async fn a_health_timeout_is_not_death() {
        // Two refusals, then a timeout: something may be alive and slow.
        let (probe, got) = unknown_readiness_with(vec![
            HealthAnswer::Refused,
            HealthAnswer::Refused,
            HealthAnswer::TimedOut,
        ])
        .await;
        assert!(matches!(
            got,
            Err(RestartIterationError::TargetReadinessUnknown { .. })
        ));
        assert_eq!(probe.health_calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn another_transport_failure_is_not_death() {
        let (_, got) =
            unknown_readiness_with(vec![HealthAnswer::Failed("reset by peer".into())]).await;
        assert!(matches!(
            got,
            Err(RestartIterationError::TargetReadinessUnknown { .. })
        ));
    }

    #[tokio::test]
    async fn unobtainable_readiness_with_health_alive_never_kills() {
        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(Err("timed out".into()), vec![HealthAnswer::Answered(200)]);
        let err = restart_owned_target(&inst, &probe, "slot-a", 9877, &no_runs(), None)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RestartIterationError::TargetReadinessUnknown { port: 9877, .. }
        ));
        assert!(inst.calls().is_empty(), "nothing is stopped");
    }

    #[tokio::test]
    async fn a_wedged_child_refusing_every_probe_is_restarted_as_the_recovery() {
        let (probe, got) = unknown_readiness_with(vec![HealthAnswer::Refused]).await;
        assert_eq!(got, Ok(ReadinessOutcome::WedgedProceed));
        assert_eq!(
            probe.health_calls.load(Ordering::SeqCst),
            HEALTH_FALLBACK_ATTEMPTS,
            "all probes must be refused before a wedge is declared"
        );

        let inst = FakeInstances::with(vec![slot("slot-a", 9877)]);
        let probe = FakeProbe::new(
            Err("connection refused".into()),
            vec![HealthAnswer::Refused],
        );
        restart_owned_target(&inst, &probe, "slot-a", 9877, &no_runs(), None)
            .await
            .unwrap();
        assert_eq!(
            inst.calls(),
            vec!["stop:slot-a", "wait_free:9877", "launch:slot-a"]
        );
    }

    #[tokio::test]
    async fn the_real_health_probe_classifies_a_closed_port_as_refused() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(HttpTargetProbe.health(port).await, HealthAnswer::Refused);
    }

    // --- identity after the relaunch --------------------------------------

    #[tokio::test]
    async fn the_relaunched_child_must_be_the_process_answering() {
        let mut probe = FakeProbe::quiet();
        assert_eq!(verify_new_child(&probe, 9877, 777).await, Ok(()));

        probe.pid = Ok(4242); // some other process holds the port
        let err = verify_new_child(&probe, 9877, 777).await.unwrap_err();
        assert_eq!(
            err,
            RestartIterationError::NotTheNewChild {
                port: 9877,
                expected_pid: 777,
                found: Ok(4242)
            }
        );

        probe.pid = Err("the /health answer carries no pid".into());
        assert!(matches!(
            verify_new_child(&probe, 9877, 777).await,
            Err(RestartIterationError::NotTheNewChild { found: Err(_), .. })
        ));
    }

    #[tokio::test]
    async fn the_restarter_confirms_the_new_child_after_a_restart() {
        let inst: Arc<FakeInstances> = Arc::new(FakeInstances::with(vec![slot("slot-a", 9877)]));
        let mut impostor = FakeProbe::quiet();
        impostor.pid = Ok(1); // launch reports 777; PID 1 answers
        let host = LoopHost {
            instances: inst.clone(),
            app: None,
            self_port: SELF_PORT,
            supervisor_probe: Arc::new(FakeSupervisor::new(false)),
            target_probe: Arc::new(impostor),
        };
        let restarter = TargetRestarter::new(
            &RestartPath::InstanceManager {
                instance_id: "slot-a".into(),
                port: 9877,
            },
            host,
        );
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let outcome = restarter.restart(false, &no_runs(), &rx).await.unwrap();
        assert_eq!(outcome.expect_pid, Some(777));
        let err = restarter.confirm_restarted(outcome).await.unwrap_err();
        assert!(err.contains("PID 1"), "{err}");
    }

    /// Phase 2 acceptance: the in-process path never touches the supervisor
    /// port. A listener bound on the configured supervisor port must accept
    /// ZERO connections across resolution AND a restart through the REAL
    /// HTTP supervisor probe and `TargetRestarter`.
    #[tokio::test]
    async fn the_instance_manager_path_makes_no_connection_to_the_supervisor_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let supervisor_port = listener.local_addr().unwrap().port();

        let inst: Arc<FakeInstances> = Arc::new(FakeInstances::with(vec![slot("slot-a", 9877)]));
        let host = LoopHost {
            instances: inst.clone(),
            app: None,
            self_port: SELF_PORT,
            supervisor_probe: Arc::new(HttpSupervisorProbe),
            target_probe: Arc::new(FakeProbe::quiet()),
        };
        let mut cfg = config(
            BetweenIterations::RestartRunner { rebuild: false },
            Some(9877),
        );
        cfg.supervisor_port = supervisor_port;

        let cap = restart_capability(&cfg, &host).await;
        assert!(cap.supported);
        assert_eq!(cap.path, Some(RestartPathKind::InstanceManager));

        let path = resolve_restart_path(
            &cfg,
            host.instances.as_ref(),
            host.self_port,
            host.supervisor_probe.as_ref(),
        )
        .await
        .unwrap();
        let restarter = TargetRestarter::new(&path, host.clone());
        let (_tx, rx) = tokio::sync::watch::channel(false);
        for _ in 0..2 {
            let outcome = restarter.restart(false, &no_runs(), &rx).await.unwrap();
            restarter.confirm_restarted(outcome).await.unwrap();
        }
        assert_eq!(inst.calls().len(), 6, "two full restarts");

        match listener.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok((_, peer)) => panic!("the supervisor port received a connection from {peer}"),
            Err(e) => panic!("unexpected accept error: {e}"),
        }
    }

    /// The real target probe against a live loopback server: readiness body
    /// parsed from `/restart-readiness`, `/health` answered, PID read.
    #[tokio::test]
    async fn the_http_target_probe_reads_a_live_readiness_endpoint() {
        use axum::{routing::get, Json, Router};
        let app = Router::new()
            .route(
                "/restart-readiness",
                get(|| async {
                    Json(serde_json::json!({
                        "safe_to_restart": false,
                        "reason": "1 live",
                        "terminal_sessions": {"count": 1, "blocking_count": 1},
                        "headless_sessions": {"count": 0},
                        "ai_sessions": {"count": 0, "sessions": []},
                        "live_claude": {"total": 1, "blocking": 1, "ai_plane": 0, "unclassified": 0},
                    }))
                }),
            )
            .route(
                "/health",
                get(|| async { Json(serde_json::json!({"success": true, "data": {"pid": 31337}})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let summary = HttpTargetProbe.readiness(port).await.unwrap();
        assert_eq!(summary.terminal_blocking, 1);
        assert_eq!(
            HttpTargetProbe.health(port).await,
            HealthAnswer::Answered(200)
        );
        assert_eq!(HttpTargetProbe.answering_pid(port).await, Ok(31337));
        match check_readiness(&HttpTargetProbe, port, &no_runs()).await {
            Err(RestartIterationError::TargetHasForeignSessions { foreign, .. }) => {
                assert_eq!(foreign.terminal_blocking, 1)
            }
            other => panic!("{other:?}"),
        }
        server.abort();
    }

    // --- supervisor rebuild-restart completion ----------------------------

    // The fake supervisor's paths live in consts, not `.route("…")` literals:
    // the runner's route census parses `.route("…")` out of every source file
    // and would count these as RUNNER routes.
    const SUPERVISOR_RESTART_ROUTE: &str = "/runners/{id}/restart";
    const SUPERVISOR_BUILD_STATUS_ROUTE: &str = "/build/{id}/status";

    /// A fake supervisor: `POST /runners/{id}/restart` answers 202 with a
    /// submission id; `GET /build/{id}/status` walks `states`.
    async fn fake_supervisor(
        terminal: serde_json::Value,
    ) -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        use axum::{
            http::StatusCode,
            routing::{get, post},
            Json, Router,
        };
        let polls = Arc::new(AtomicUsize::new(0));
        let polls_in = polls.clone();
        let app = Router::new()
            .route(
                SUPERVISOR_RESTART_ROUTE,
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        Json(serde_json::json!({"status": "rebuilding", "submission_id": "sub-1"})),
                    )
                }),
            )
            .route(
                SUPERVISOR_BUILD_STATUS_ROUTE,
                get(move || {
                    let polls = polls_in.clone();
                    let terminal = terminal.clone();
                    async move {
                        if polls.fetch_add(1, Ordering::SeqCst) < 2 {
                            Json(serde_json::json!({"status": {"state": "running"}}))
                        } else {
                            Json(terminal)
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, polls, server)
    }

    #[tokio::test]
    async fn a_202_rebuild_is_not_done_until_the_submission_succeeds() {
        let (port, polls, server) = fake_supervisor(serde_json::json!({
            "status": {"state": "succeeded"},
            "detached": {"http_status": 200, "body": {"status": "restarted"}},
        }))
        .await;
        let client = SupervisorClient::new(port).with_poll_interval(Duration::from_millis(10));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        client
            .restart_runner("test-1", true, Duration::from_secs(10), &rx)
            .await
            .unwrap();
        assert!(
            polls.load(Ordering::SeqCst) >= 3,
            "polled past the running states"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_failed_detached_rebuild_is_a_typed_error() {
        let (port, _, server) = fake_supervisor(serde_json::json!({
            "status": {"state": "succeeded"},
            "detached": {"http_status": 500, "body": {"error": "cargo build failed"}},
        }))
        .await;
        let client = SupervisorClient::new(port).with_poll_interval(Duration::from_millis(10));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let err = client
            .restart_runner("test-1", true, Duration::from_secs(10), &rx)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            crate::orchestration_loop::remote_client::SupervisorRestartError::BuildFailed {
                submission_id: "sub-1".into(),
                error: "cargo build failed".into()
            }
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_detached_rebuild_that_never_finishes_times_out_and_honours_stop() {
        let (port, _, server) =
            fake_supervisor(serde_json::json!({"status": {"state": "running"}})).await;
        let client = SupervisorClient::new(port).with_poll_interval(Duration::from_millis(10));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let err = client
            .restart_runner("test-1", true, Duration::from_millis(200), &rx)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            crate::orchestration_loop::remote_client::SupervisorRestartError::TimedOut { .. }
        ));
        tx.send(true).unwrap();
        let err = client
            .restart_runner("test-1", true, Duration::from_secs(10), &rx)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            crate::orchestration_loop::remote_client::SupervisorRestartError::Stopped
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_4xx_status_poll_is_terminal_not_polled_to_timeout() {
        use axum::{
            http::StatusCode,
            routing::{get, post},
            Json, Router,
        };
        let app = Router::new()
            .route(
                SUPERVISOR_RESTART_ROUTE,
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        Json(serde_json::json!({"submission_id": "not-a-uuid"})),
                    )
                }),
            )
            .route(
                SUPERVISOR_BUILD_STATUS_ROUTE,
                get(|| async { (StatusCode::BAD_REQUEST, "invalid uuid: not-a-uuid") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = SupervisorClient::new(port).with_poll_interval(Duration::from_millis(10));
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let started = std::time::Instant::now();
        let err = client
            .restart_runner("test-1", true, Duration::from_secs(60), &rx)
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "terminal, not timed out"
        );
        match err {
            crate::orchestration_loop::remote_client::SupervisorRestartError::BuildFailed {
                error,
                ..
            } => assert!(error.contains("400"), "{error}"),
            other => panic!("{other:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn an_older_supervisors_primary_without_kind_is_refused_by_id() {
        // An older supervisor serves no `kind`; its primary is still id
        // "primary", and the resolver must not ask it to rebuild that runner.
        let inst = FakeInstances::default();
        let sup = FakeSupervisor::with_rows(true, Ok(vec![row("primary", 9879)]));
        let err = resolve_restart_path(&rebuild_cfg(9879, None), &inst, SELF_PORT, &sup)
            .await
            .unwrap_err();
        assert_eq!(err.code, RestartUnsupportedCode::RebuildNeedsDevSupervisor);
        assert!(err.reason.contains("PRIMARY"), "{}", err.reason);
    }

    #[test]
    fn detached_status_parsing_covers_every_terminal_shape() {
        use crate::orchestration_loop::remote_client::{
            parse_detached_build_status as parse, DetachedBuildState as S,
        };
        assert_eq!(
            parse(&serde_json::json!({"status": {"state": "queued"}})),
            S::Pending
        );
        assert_eq!(
            parse(&serde_json::json!({"status": {"state": "succeeded"}})),
            S::Pending,
            "terminal but no outcome attached yet"
        );
        assert_eq!(
            parse(&serde_json::json!({"status": {"state": "failed", "error": "boom"}})),
            S::Failed("boom".into())
        );
        assert_eq!(
            parse(
                &serde_json::json!({"status": {"state": "succeeded"}, "detached": {"http_status": 204}})
            ),
            S::Succeeded
        );
    }
}
