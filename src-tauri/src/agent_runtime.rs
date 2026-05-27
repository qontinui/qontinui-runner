//! Coord-driven Claude Code subprocess runtime
//! (Phase 4 of `2026-05-19-coordinator-production-readiness.md`).
//!
//! Per `feedback_no_anthropic_api`: NEVER use `api.anthropic.com` or
//! `ANTHROPIC_API_KEY`. This module spawns the operator's `claude` CLI
//! as a subprocess; the operator's existing Claude Code subscription
//! covers all token cost. The subprocess inherits whatever auth the
//! operator's `claude` install has.
//!
//! ## Flow
//!
//! 1. `spawn_runtime()` connects this runner to coord's `/ws` and
//!    subscribes to `events.agent.spawn_requested.<this_device_id>`.
//! 2. On a spawn-request event, parse the `LaunchPayload`. For each
//!    allocated worktree, `git worktree add <path> <branch>` from the
//!    repo's primary tree (resolved by `QONTINUI_ROOT/<repo>`).
//! 3. Spawn `claude` CLI as a tokio child in the first worktree.
//!    Pipe initial_prompt into stdin; capture stdout+stderr line-by-line.
//! 4. Append each line to a per-agent log file AND POST to
//!    `/agents/:agent_id/log` (Phase 5 endpoint).
//! 5. Heartbeat the claim every 30s via `/claims/heartbeat`.
//! 6. On subprocess exit, POST `spawn-failed` (non-zero) or
//!    `spawn-complete` (clean exit). Restart up to 3× on crash.
//!
//! ## Identity
//!
//! Reads `~/.qontinui/machine.json` for `device_id` (the same source
//! `fleet::heartbeat_to_coord` uses). The WS subscription filter is
//! per-device so a multi-machine fleet doesn't broadcast every spawn
//! to every runner.
//!
//! ## Failure posture
//!
//! - WS disconnects: reconnect with capped exponential backoff
//!   (2s → 60s). The loop never panics; failures `warn!` and retry.
//! - Coord unreachable on log/lifecycle POST: queue locally to a per-
//!   agent file and flush on the next successful POST. The runner
//!   doesn't block on coord ingest.
//! - `claude` binary not on PATH: log + POST `spawn-failed` with a
//!   clear reason; do not restart.
//!
//! ## Phase-5 coordination
//!
//! `/agents/:id/log` is owned by Wave 1 Agent D (Phase 5). If their
//! endpoint isn't live yet, POSTs return 404 and the local queue grows
//! until flush succeeds. The local file is the source of truth; coord
//! ingest is the streaming surface.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

// =============================================================================
// Wire shapes (mirror of qontinui-coord/src/agents_spawn.rs)
// =============================================================================

/// Inbound launch payload. Mirrors `agents_spawn::LaunchPayload` in coord.
/// Kept in lockstep manually: coord is the canonical wire owner, runner
/// is a structural duplicate so this module compiles without a path-dep.
#[derive(Debug, Clone, Deserialize)]
pub struct LaunchPayload {
    pub agent_id: uuid::Uuid,
    #[serde(default)]
    pub agent_session_id: Option<uuid::Uuid>,
    pub target_device_id: uuid::Uuid,
    pub worktrees: Vec<AllocatedWorktree>,
    pub jwt: String,
    pub jwt_exp: i64,
    pub initial_prompt: String,
    pub claim_token: String,
    #[serde(default)]
    pub plan_slug: Option<String>,
    #[serde(default)]
    pub plan_phase: Option<u32>,
    #[serde(default)]
    pub correlation_topic: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AllocatedWorktree {
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    pub worktree_path: String,
    pub status: String,
    #[serde(default)]
    pub push_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SpawnCompleteBody {
    pid: Option<i64>,
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SpawnFailedBody {
    reason: String,
    exit_code: Option<i64>,
    restarts_attempted: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct LogLine {
    /// `stdout` or `stderr`.
    stream: String,
    line: String,
    at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct ClaimHeartbeat {
    kind: String,
    resource_key: String,
    machine_id: String,
    ttl_seconds: i64,
}

// =============================================================================
// Configuration
// =============================================================================

/// Max consecutive crash-restarts before we give up on a subprocess.
const MAX_RESTARTS: u32 = 3;

/// Heartbeat cadence for the claim.
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Maximum number of buffered log lines we'll keep when coord is
/// unreachable. Older lines drop FIFO; the per-agent log file keeps the
/// full record regardless.
const LOG_FLUSH_QUEUE_CAP: usize = 1024;

/// Directory under `~/.qontinui` where per-agent run logs live. Created
/// on first use; one file per agent_id.
fn agent_logs_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("agent-runs"))
}

fn agent_log_path(agent_id: uuid::Uuid) -> Option<PathBuf> {
    agent_logs_root().map(|d| d.join(format!("{agent_id}.log")))
}

/// Resolve the coord HTTP base from the active profile (mirrors the
/// resolver in `fleet.rs`). Returns `None` when no profile or no
/// coord_url is configured — runtime no-ops in that case.
fn coord_http_base() -> Option<String> {
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
    let trimmed = coord_url.trim_end_matches("/ws");
    let with_http = trimmed
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            trimmed
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .unwrap_or_else(|| trimmed.to_string());
    Some(with_http)
}

/// Resolve the coord WS URL from the active profile's `coord_url`,
/// normalizing the scheme to `ws://` / `wss://` and appending the
/// `/ws` path with an `events.agent.*` pattern filter.
///
/// Mirrors `session::handoff::coord_ws_url` which appends
/// `/ws?pattern=qontinui.sessions.*`. The coord `/ws` endpoint is a
/// Redis pub/sub bridge at the `/ws` path (not the root); connecting to
/// the root returns 401 from the ALB.
fn coord_ws_url(device_id: uuid::Uuid) -> Option<String> {
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
    let base = coord_url.trim().trim_end_matches('/');
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    Some(format!(
        "{ws_base}/ws?pattern=events.agent.spawn_requested.{device_id}"
    ))
}

/// Read `~/.qontinui/machine.json` → device_id. Falls back to None.
fn load_local_device_id() -> Option<uuid::Uuid> {
    #[derive(Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
    }
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(&path).ok()?;
    let f: DeviceFile = serde_json::from_slice(&bytes).ok()?;
    uuid::Uuid::parse_str(&f.device_id).ok()
}

/// Path to the `claude` binary. Default: `claude` (PATH-resolved).
/// Override with `QONTINUI_CLAUDE_BIN` for testing (e.g. the
/// `mock_claude_cli` fixture in `bin/`).
fn claude_bin_path() -> String {
    std::env::var("QONTINUI_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

// =============================================================================
// Public entrypoint
// =============================================================================

/// Spawn the agent-runtime tokio task. Subscribes to spawn-request
/// events for the local `device_id`; on each event, runs a subprocess
/// against the allocated worktrees.
///
/// Wired in `main.rs` next to `fleet::spawn_heartbeat()`.
pub fn spawn_runtime() {
    let device_id = match load_local_device_id() {
        Some(d) => d,
        None => {
            info!(
                "agent_runtime: ~/.qontinui/machine.json missing or device_id \
                 unparseable — agent spawn runtime disabled. Skipping."
            );
            return;
        }
    };

    if coord_http_base().is_none() {
        info!(
            "agent_runtime: profile has no coord_url — agent spawn runtime \
             disabled. Skipping."
        );
        return;
    }

    info!("agent_runtime: starting for device_id={}", device_id);
    tokio::spawn(async move {
        if let Err(e) = subscribe_to_spawn_requests(device_id).await {
            error!("agent_runtime: subscriber exited with error: {e:#}");
        }
    });
}

/// Subscribe loop: connects to coord WS, filters for this device's
/// spawn-request channel, dispatches each accepted payload to
/// `run_agent_subprocess` in its own task. Reconnects with capped
/// exponential backoff.
async fn subscribe_to_spawn_requests(device_id: uuid::Uuid) -> anyhow::Result<()> {
    let ws_url = match coord_ws_url(device_id) {
        Some(u) => u,
        None => {
            warn!("agent_runtime: no coord_url; subscriber loop exiting");
            return Ok(());
        }
    };
    let mut backoff_ms: u64 = 2_000;
    loop {
        match connect_and_pump(&ws_url, device_id).await {
            Ok(()) => {
                debug!("agent_runtime: WS pump returned cleanly; reconnecting");
                backoff_ms = 2_000;
            }
            Err(e) => {
                warn!(
                    "agent_runtime: WS pump error ({e:#}); retrying in {}s",
                    backoff_ms / 1000
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(60_000);
    }
}

/// Single connect-and-pump iteration: opens the WS, listens for events,
/// dispatches spawn-requests. Returns on disconnect.
///
/// Coord's `/ws` endpoint is a Redis pub/sub bridge; the pattern filter
/// is set via the `?pattern=` query param at upgrade time (client-sent
/// Text frames are silently ignored). The URL already carries the
/// device-scoped pattern from [`coord_ws_url`].
async fn connect_and_pump(ws_url: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    use futures_util::StreamExt;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url).await?;
    info!("agent_runtime: WS connected for device_id={device_id}");

    while let Some(msg) = ws.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("agent_runtime: WS recv error: {e:#}");
                return Err(anyhow::anyhow!("WS recv: {e}"));
            }
        };
        let txt = match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                String::from_utf8_lossy(&b).to_string()
            }
            tokio_tungstenite::tungstenite::Message::Ping(_)
            | tokio_tungstenite::tungstenite::Message::Pong(_) => continue,
            tokio_tungstenite::tungstenite::Message::Close(_) => {
                info!("agent_runtime: WS closed by peer");
                return Ok(());
            }
            tokio_tungstenite::tungstenite::Message::Frame(_) => continue,
        };
        if let Err(e) = handle_message(&txt, device_id).await {
            warn!("agent_runtime: handle_message error (continuing): {e:#}");
        }
    }
    Ok(())
}

/// Parse a WS frame; if it's a spawn-request for this device, dispatch
/// it. Coord's WS surface wraps payloads in a `{channel, body}` envelope
/// for filtering; we attempt to peel that, but fall back to a direct
/// `LaunchPayload` parse for compatibility with simpler test fixtures.
async fn handle_message(txt: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(txt)?;

    // Envelope shape from coord's `/ws` Redis->WS fanout (coord/src/ws.rs):
    //   {"channel":"events.agent.spawn_requested.<id>","payload":"<json-string>"}
    // where `payload` is the raw Redis message — the LaunchPayload serialized
    // as a STRING. Older/test fixtures used {"channel","body":<object>}; we
    // accept both (see `parse_envelope_payload`). On a miss we log + return Ok:
    // a malformed or foreign frame must never kill the subscribe loop, and
    // because coord's default `events.*` subscription delivers every event,
    // most frames legitimately are not ours.
    if let Some(channel) = value.get("channel").and_then(|c| c.as_str()) {
        let expected = format!("events.agent.spawn_requested.{device_id}");
        if channel != expected {
            // Not our channel — silently ignore.
            return Ok(());
        }
        match parse_envelope_payload(&value) {
            Some(payload) => spawn_run_task(payload),
            None => {
                warn!("agent_runtime: spawn envelope on {channel} had no parseable payload/body")
            }
        }
        return Ok(());
    }

    // Bare-payload fallback (used by test fixtures + the no-envelope
    // coord WS variant).
    if value.get("agent_id").is_some() && value.get("worktrees").is_some() {
        let payload: LaunchPayload = serde_json::from_value(value)?;
        if payload.target_device_id != device_id {
            return Ok(());
        }
        spawn_run_task(payload);
    }
    Ok(())
}

/// Extract a `LaunchPayload` from a coord `/ws` envelope.
///
/// Coord's fanout (`coord/src/ws.rs`) emits `{"channel", "payload": "<json>"}`
/// where `payload` is the raw Redis message — the LaunchPayload serialized as
/// a STRING. Older/test fixtures used `{"channel", "body": <object>}`. Accept
/// both field names, and accept the inner value as a JSON string (`from_str`)
/// or a nested object (`from_value`). Returns `None` if neither yields a
/// parseable payload.
fn parse_envelope_payload(envelope: &serde_json::Value) -> Option<LaunchPayload> {
    let inner = envelope.get("payload").or_else(|| envelope.get("body"))?;
    match inner.as_str() {
        Some(s) => serde_json::from_str(s).ok(),
        None => serde_json::from_value(inner.clone()).ok(),
    }
}

fn spawn_run_task(payload: LaunchPayload) {
    info!(
        "agent_runtime: spawn-request received agent_id={} worktrees={}",
        payload.agent_id,
        payload.worktrees.len()
    );
    tokio::spawn(async move {
        if let Err(e) = run_agent_subprocess(payload).await {
            error!("agent_runtime: run_agent_subprocess failed: {e:#}");
        }
    });
}

// =============================================================================
// Subprocess lifecycle
// =============================================================================

/// End-to-end run of one agent subprocess:
/// 1. `git worktree add` each allocated worktree.
/// 2. Spawn `claude` CLI in the first worktree with the initial prompt.
/// 3. Start heartbeat + log-forwarder background tasks.
/// 4. Wait for exit; restart up to 3× on crash; report final status.
async fn run_agent_subprocess(mut payload: LaunchPayload) -> anyhow::Result<()> {
    // Coord emits each worktree's `repo` as a full `owner/name` slug and its
    // `worktree_path` for COORD's OWN host (a Linux `/root/qontinui-root.wt/...`
    // path) — neither is valid on this runner's filesystem. The runner owns its
    // local worktree layout, so rewrite every path to a local, platform-correct
    // one (`<QONTINUI_ROOT>/.agent-worktrees/<agent_id>/<repo-name>`) before
    // materializing or using it as the agent's cwd.
    let agent_id = payload.agent_id;
    if let Some(root) = qontinui_root_dir() {
        for wt in &mut payload.worktrees {
            wt.worktree_path = local_worktree_path(&root, agent_id, &wt.repo)
                .to_string_lossy()
                .into_owned();
        }
    }

    // Step 1: materialize worktrees.
    if let Err(e) = materialize_worktrees(&payload).await {
        report_spawn_failed(
            payload.agent_id,
            &format!("worktree materialization failed: {e:#}"),
            None,
            0,
        )
        .await;
        return Err(e);
    }

    let primary_wt = payload
        .worktrees
        .first()
        .ok_or_else(|| anyhow::anyhow!("no worktrees allocated"))?
        .worktree_path
        .clone();

    // Write .mcp.json so the spawned claude process auto-discovers
    // the coord MCP server for coordination tooling.
    write_coord_mcp_config(&primary_wt, &payload);

    let log_path = agent_log_path(payload.agent_id);

    // Step 2: heartbeat task — runs for the agent's whole life.
    let hb_payload = payload.clone();
    let hb_task = tokio::spawn(async move { run_heartbeat_loop(hb_payload).await });

    // Step 3: subprocess + restart loop.
    let mut restarts = 0u32;
    let mut final_exit_code: Option<i64> = None;
    let mut final_reason: Option<String> = None;

    loop {
        match spawn_claude_child(&primary_wt, &payload.initial_prompt).await {
            Ok(mut child) => {
                let pid = child.id().map(|p| p as i64);
                // First successful spawn = post spawn-complete with pid.
                if restarts == 0 {
                    report_spawn_complete(payload.agent_id, pid, None).await;
                } else {
                    info!(
                        "agent_runtime: restart {restarts} succeeded agent_id={}",
                        payload.agent_id
                    );
                }
                let exit = pump_subprocess(payload.agent_id, &mut child, log_path.as_deref()).await;
                match exit {
                    Ok(0) => {
                        info!(
                            "agent_runtime: agent_id={} exited cleanly (code=0); not restarting",
                            payload.agent_id
                        );
                        final_exit_code = Some(0);
                        break;
                    }
                    Ok(code) => {
                        warn!(
                            "agent_runtime: agent_id={} exited with code {code} restarts={}",
                            payload.agent_id, restarts,
                        );
                        final_exit_code = Some(code);
                        final_reason = Some(format!("non-zero exit code {code}"));
                    }
                    Err(e) => {
                        warn!(
                            "agent_runtime: agent_id={} pump failed: {e:#}",
                            payload.agent_id
                        );
                        final_reason = Some(format!("pump failure: {e}"));
                    }
                }
            }
            Err(e) => {
                warn!(
                    "agent_runtime: spawn_claude_child failed agent_id={} attempt={}: {e:#}",
                    payload.agent_id, restarts
                );
                final_reason = Some(format!("spawn failure: {e}"));
            }
        }

        restarts += 1;
        if restarts >= MAX_RESTARTS {
            break;
        }
        // Brief back-off between restarts.
        tokio::time::sleep(Duration::from_secs(2u64.pow(restarts.min(3)))).await;
    }

    hb_task.abort();

    if final_exit_code == Some(0) {
        // Clean exit was already reported via spawn-complete; nothing
        // more to send. The runner can release the claim explicitly via
        // a follow-up POST if desired.
        return Ok(());
    }

    report_spawn_failed(
        payload.agent_id,
        final_reason
            .as_deref()
            .unwrap_or("subprocess exited with no recorded reason"),
        final_exit_code,
        restarts,
    )
    .await;
    Ok(())
}

/// `git worktree add` each allocated worktree from its repo's primary
/// tree under `QONTINUI_ROOT`.
async fn materialize_worktrees(payload: &LaunchPayload) -> anyhow::Result<()> {
    let root = qontinui_root_dir()
        .ok_or_else(|| anyhow::anyhow!("no qontinui-root directory configured"))?;
    for wt in &payload.worktrees {
        let repo_root = root.join(local_repo_name(&wt.repo));
        if !repo_root.exists() {
            return Err(anyhow::anyhow!(
                "primary repo {} not found at {}",
                wt.repo,
                repo_root.display()
            ));
        }
        let wt_path = Path::new(&wt.worktree_path);
        if wt_path.exists() {
            info!(
                "agent_runtime: worktree path {} already exists; skipping add",
                wt.worktree_path
            );
            continue;
        }
        let out = Command::new("git")
            .args([
                "-C",
                repo_root
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("repo_root not UTF-8"))?,
                "worktree",
                "add",
                wt_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("worktree path not UTF-8"))?,
                "-b",
                &wt.branch,
                &wt.parent_sha,
            ])
            .output()
            .await?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow::anyhow!(
                "git worktree add failed for {}: {}",
                wt.repo,
                err.trim()
            ));
        }
        info!(
            "agent_runtime: materialized worktree {} at {}",
            wt.repo, wt.worktree_path
        );
    }
    Ok(())
}

fn qontinui_root_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("qontinui-root"))
        .filter(|p| p.is_dir())
}

/// The local primary-checkout directory NAME for a coord repo slug. Coord uses
/// `owner/name` slugs (e.g. `qontinui/qontinui-runner`); the runner's primary
/// checkouts live at `<QONTINUI_ROOT>/<name>`. A bare name passes through.
fn local_repo_name(repo: &str) -> &str {
    repo.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(repo)
}

/// The local worktree path the runner materializes for an agent's repo. The
/// runner owns this layout; coord's emitted `worktree_path` (computed for its
/// own host) is ignored. Scheme:
/// `<QONTINUI_ROOT>/.agent-worktrees/<agent_id>/<repo-name>`.
fn local_worktree_path(root: &Path, agent_id: uuid::Uuid, repo: &str) -> PathBuf {
    root.join(".agent-worktrees")
        .join(agent_id.to_string())
        .join(local_repo_name(repo))
}

/// Write `.mcp.json` into the agent's primary worktree directory so the
/// spawned `claude` process auto-discovers the coord MCP server.
///
/// Targets the **coord-native** streamable-HTTP MCP server at
/// `{COORD_HTTP_URL}/mcp` (Bearer-authenticated with the agent's own JWT),
/// rather than spawning a local `coord-mcp.mjs` Node sidecar. The agent's
/// identity (device_id, tenant_id, correlation topic) is derived server-side
/// from the validated JWT claims, so no `AGENT_NAME`/`AGENT_LANE`/`TOPIC`/
/// `DEVICE_ID` env vars are needed in the config.
///
/// The coord-native `/mcp` endpoint is live (coord PR #277 Phase-2 cutover),
/// so this no longer carries the prior "do not deploy" gate. If a target coord
/// ever lacks the `/mcp` route, agents get an unreachable MCP server: Claude
/// Code degrades gracefully and runs *without* coord tools (a silent
/// coordination regression) — so always sequence a coord `/mcp` deploy ahead
/// of pointing runners at a new coord.
///
/// Coord base resolution: `COORD_HTTP_URL` env → active profile's `coord_url`
/// → localhost fallback. Previously this read ONLY the env var, so a
/// profile-configured runner (production → `coord.qontinui.io`) wrote a
/// `localhost` MCP url into the agent's `.mcp.json`, silently pointing spawned
/// agents at the wrong coord.
fn write_coord_mcp_config(primary_wt: &str, payload: &LaunchPayload) {
    let coord_url = std::env::var("COORD_HTTP_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(coord_http_base)
        .unwrap_or_else(|| "http://localhost:9870".to_string());
    let mcp_url = format!("{}/mcp", coord_url.trim_end_matches('/'));

    let mcp_config = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": mcp_url,
                "headers": {
                    "Authorization": format!("Bearer {}", payload.jwt),
                }
            }
        }
    });

    let mcp_path = Path::new(primary_wt).join(".mcp.json");
    match std::fs::write(
        &mcp_path,
        serde_json::to_string_pretty(&mcp_config).unwrap_or_default(),
    ) {
        Ok(()) => {
            info!(
                "agent_runtime: wrote .mcp.json for coord-mcp in {}",
                primary_wt
            );
        }
        Err(e) => {
            warn!(
                "agent_runtime: failed to write .mcp.json in {}: {e}",
                primary_wt
            );
        }
    }
}

/// Spawn `claude` CLI as a tokio child. `initial_prompt` is piped to
/// stdin. stdout/stderr are inherited as pipes so the caller can stream
/// them.
async fn spawn_claude_child(workdir: &str, initial_prompt: &str) -> anyhow::Result<Child> {
    let bin = claude_bin_path();
    let mut cmd = Command::new(&bin);
    cmd.current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `-p` / `--print` means "single-shot prompt mode" for Claude Code
    // CLI; not all versions support stdin-as-prompt cleanly, so we send
    // the prompt over stdin AND close stdin after.
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn `{bin}` in {workdir}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = initial_prompt.to_string();
        tokio::spawn(async move {
            let body = if prompt.ends_with('\n') {
                prompt
            } else {
                format!("{prompt}\n")
            };
            if let Err(e) = stdin.write_all(body.as_bytes()).await {
                debug!("agent_runtime: stdin write failed: {e}");
            }
            // Drop stdin to signal EOF.
            drop(stdin);
        });
    }
    Ok(child)
}

/// Pump stdout + stderr to the per-agent log file AND POST each line to
/// `/agents/:agent_id/log` (Phase 5 endpoint). Returns the child's exit
/// code on clean exit; Err on pump failure.
async fn pump_subprocess(
    agent_id: uuid::Uuid,
    child: &mut Child,
    log_path: Option<&Path>,
) -> anyhow::Result<i64> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Lazy-create the per-agent log file.
    if let Some(p) = log_path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let log_file = log_path.and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)))
    });

    // Shared queue for log-forwarder retry on coord failure.
    let queue: Arc<Mutex<VecDeque<LogLine>>> = Arc::new(Mutex::new(VecDeque::new()));

    let q_out = queue.clone();
    let f_out = log_file.clone();
    let out_task = tokio::spawn(async move {
        if let Some(stream) = stdout {
            forward_stream("stdout", stream, agent_id, q_out, f_out).await;
        }
    });

    let q_err = queue.clone();
    let f_err = log_file.clone();
    let err_task = tokio::spawn(async move {
        if let Some(stream) = stderr {
            forward_stream("stderr", stream, agent_id, q_err, f_err).await;
        }
    });

    // Periodic flusher: every 5s, drain the queue to coord.
    let q_flush = queue.clone();
    let flush_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            flush_log_queue(agent_id, &q_flush).await;
        }
    });

    let status = child.wait().await?;
    let _ = out_task.await;
    let _ = err_task.await;
    flush_task.abort();
    // One final flush after the child exits.
    flush_log_queue(agent_id, &queue).await;
    Ok(status.code().map(|c| c as i64).unwrap_or(-1))
}

async fn forward_stream<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    stream_name: &'static str,
    reader: R,
    agent_id: uuid::Uuid,
    queue: Arc<Mutex<VecDeque<LogLine>>>,
    log_file: Option<Arc<Mutex<std::fs::File>>>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let log = LogLine {
            stream: stream_name.to_string(),
            line: line.clone(),
            at: chrono::Utc::now(),
        };
        // Append to local log file (best-effort).
        if let Some(f) = &log_file {
            use std::io::Write;
            let mut guard = f.lock().await;
            let _ = writeln!(&mut *guard, "[{stream_name}] {line}");
        }
        // Try-immediate POST to coord; on failure, enqueue.
        if !post_log_line(agent_id, &log).await {
            let mut q = queue.lock().await;
            if q.len() >= LOG_FLUSH_QUEUE_CAP {
                q.pop_front();
            }
            q.push_back(log);
        }
    }
}

async fn flush_log_queue(agent_id: uuid::Uuid, queue: &Arc<Mutex<VecDeque<LogLine>>>) {
    let mut q = queue.lock().await;
    while let Some(log) = q.pop_front() {
        if !post_log_line(agent_id, &log).await {
            // Put it back at the head and stop trying for this flush.
            q.push_front(log);
            return;
        }
    }
}

async fn post_log_line(agent_id: uuid::Uuid, line: &LogLine) -> bool {
    let Some(base) = coord_http_base() else {
        return false;
    };
    let url = format!("{base}/agents/{agent_id}/log");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.post(&url).json(line).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn run_heartbeat_loop(payload: LaunchPayload) {
    let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(e) = heartbeat_once(&payload).await {
            warn!(
                "agent_runtime: heartbeat agent_id={} failed: {e:#}",
                payload.agent_id
            );
        }
    }
}

async fn heartbeat_once(payload: &LaunchPayload) -> anyhow::Result<()> {
    let base = coord_http_base().ok_or_else(|| anyhow::anyhow!("no coord_url"))?;
    let body = ClaimHeartbeat {
        kind: "phase".to_string(),
        resource_key: payload.claim_token.clone(),
        machine_id: payload.target_device_id.to_string(),
        ttl_seconds: 3600,
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .post(format!("{base}/claims/heartbeat"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "claims heartbeat returned {}",
            resp.status()
        ));
    }
    Ok(())
}

async fn report_spawn_complete(agent_id: uuid::Uuid, pid: Option<i64>, note: Option<&str>) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let body = SpawnCompleteBody {
        pid,
        note: note.map(|s| s.to_string()),
    };
    let url = format!("{base}/agents/{agent_id}/spawn-complete");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!("agent_runtime: spawn-complete posted agent_id={agent_id}");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: spawn-complete POST agent_id={agent_id} returned {}",
                resp.status()
            );
        }
        Err(e) => warn!("agent_runtime: spawn-complete POST agent_id={agent_id} failed: {e:#}"),
    }
}

async fn report_spawn_failed(
    agent_id: uuid::Uuid,
    reason: &str,
    exit_code: Option<i64>,
    restarts_attempted: u32,
) {
    let Some(base) = coord_http_base() else {
        return;
    };
    let body = SpawnFailedBody {
        reason: reason.to_string(),
        exit_code,
        restarts_attempted: Some(restarts_attempted),
    };
    let url = format!("{base}/agents/{agent_id}/spawn-failed");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            warn!("agent_runtime: spawn-failed posted agent_id={agent_id} reason={reason}");
        }
        Ok(resp) => {
            warn!(
                "agent_runtime: spawn-failed POST agent_id={agent_id} returned {}",
                resp.status()
            );
        }
        Err(e) => warn!("agent_runtime: spawn-failed POST agent_id={agent_id} failed: {e:#}"),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_payload_round_trips_through_envelope() {
        let payload = LaunchPayload {
            agent_id: uuid::Uuid::nil(),
            agent_session_id: None,
            target_device_id: uuid::Uuid::nil(),
            worktrees: vec![AllocatedWorktree {
                repo: "qontinui-runner".to_string(),
                branch: "agent/abc-def".to_string(),
                parent_sha: "deadbeef".to_string(),
                worktree_path: "/tmp/wt".to_string(),
                status: "allocated".to_string(),
                push_ref: Some("refs/agent/abc-def".to_string()),
            }],
            jwt: "tok".to_string(),
            jwt_exp: 0,
            initial_prompt: "go".to_string(),
            claim_token: "agent:00000000-0000-0000-0000-000000000000".to_string(),
            plan_slug: Some("readiness".to_string()),
            plan_phase: Some(4),
            correlation_topic: Some("my-coordination-topic".to_string()),
        };
        let serialized = serde_json::to_value(serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{}", payload.target_device_id),
            "body": serde_json::json!({
                "agent_id": payload.agent_id,
                "agent_session_id": payload.agent_session_id,
                "target_device_id": payload.target_device_id,
                "worktrees": payload.worktrees,
                "jwt": payload.jwt,
                "jwt_exp": payload.jwt_exp,
                "initial_prompt": payload.initial_prompt,
                "claim_token": payload.claim_token,
                "plan_slug": payload.plan_slug,
                "plan_phase": payload.plan_phase,
            }),
        }))
        .unwrap();
        // Use the same envelope parse as handle_message.
        let channel = serialized.get("channel").and_then(|v| v.as_str()).unwrap();
        assert!(channel.starts_with("events.agent.spawn_requested."));
        let body = serialized.get("body").cloned().unwrap();
        let round_tripped: LaunchPayload = serde_json::from_value(body).unwrap();
        assert_eq!(round_tripped.worktrees.len(), 1);
        assert_eq!(round_tripped.worktrees[0].repo, "qontinui-runner");
        assert_eq!(round_tripped.plan_phase, Some(4));
    }

    #[test]
    fn coord_ws_string_payload_envelope_parses() {
        // Reproduces coord's ACTUAL `/ws` fanout shape: a {channel, payload}
        // envelope where `payload` is the LaunchPayload serialized as a STRING
        // (coord/src/ws.rs relays the raw Redis message text). The pre-fix
        // consumer looked only for `body` as an object and dropped this frame
        // with "envelope missing body" — the bug that blocked the live spawn.
        let device = uuid::Uuid::now_v7();
        let inner = serde_json::json!({
            "agent_id": uuid::Uuid::now_v7(),
            "target_device_id": device,
            "worktrees": [],
            "jwt": "t",
            "jwt_exp": 0,
            "initial_prompt": "go",
            "claim_token": "agent:x",
        });
        // payload as a STRING (coord's real shape)
        let envelope = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        let parsed =
            parse_envelope_payload(&envelope).expect("coord string-payload envelope must parse");
        assert_eq!(parsed.target_device_id, device);

        // Legacy {channel, body:<object>} still parses.
        let legacy = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{device}"),
            "body": inner,
        });
        assert!(parse_envelope_payload(&legacy).is_some());

        // Garbage payload yields None (and must not panic).
        let junk = serde_json::json!({ "channel": "x", "payload": "not json" });
        assert!(parse_envelope_payload(&junk).is_none());
    }

    #[test]
    fn local_paths_strip_owner_slug() {
        assert_eq!(
            local_repo_name("qontinui/qontinui-runner"),
            "qontinui-runner"
        );
        assert_eq!(local_repo_name("qontinui-runner"), "qontinui-runner");
        let root = Path::new("D:/qontinui-root");
        let p = local_worktree_path(root, uuid::Uuid::nil(), "qontinui/qontinui-runner");
        assert!(p.ends_with(Path::new("qontinui-runner")));
        assert!(p.to_string_lossy().contains(".agent-worktrees"));
    }

    #[test]
    fn agent_log_path_uses_agent_id() {
        // Force HOME to a known place so the path is deterministic.
        let id = uuid::Uuid::parse_str("0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70").unwrap();
        // Just exercise the constructor — exact path content depends on
        // the environment's HOME, which is irrelevant here.
        let p = agent_log_path(id);
        if let Some(p) = p {
            let s = p.to_string_lossy().to_string();
            assert!(
                s.contains("0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70.log"),
                "expected agent_id-named log file, got {}",
                s
            );
        }
    }

    #[test]
    fn claude_bin_respects_env_override() {
        let prev = std::env::var("QONTINUI_CLAUDE_BIN").ok();
        std::env::set_var("QONTINUI_CLAUDE_BIN", "/some/fake/claude-fixture");
        assert_eq!(claude_bin_path(), "/some/fake/claude-fixture");
        match prev {
            Some(v) => std::env::set_var("QONTINUI_CLAUDE_BIN", v),
            None => std::env::remove_var("QONTINUI_CLAUDE_BIN"),
        }
    }

    #[test]
    fn write_coord_mcp_config_emits_http_bearer_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-cfg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&tmp).unwrap();
        let primary_wt = tmp.to_string_lossy().to_string();

        let prev = std::env::var("COORD_HTTP_URL").ok();
        std::env::set_var("COORD_HTTP_URL", "https://coord.example.test/");

        let payload = LaunchPayload {
            agent_id: uuid::Uuid::now_v7(),
            agent_session_id: None,
            target_device_id: uuid::Uuid::now_v7(),
            worktrees: vec![],
            jwt: "header.payload.sig".to_string(),
            jwt_exp: 0,
            initial_prompt: "go".to_string(),
            claim_token: "agent:x".to_string(),
            plan_slug: None,
            plan_phase: None,
            correlation_topic: Some("my-coordination-topic".to_string()),
        };

        write_coord_mcp_config(&primary_wt, &payload);

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];

        // HTTP transport pointing at coord /mcp, Bearer-authenticated.
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "https://coord.example.test/mcp");
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer header.payload.sig"
        );

        // No Node-sidecar/subprocess residue, and no identity env vars
        // (identity is derived server-side from the JWT claims).
        assert!(server.get("command").is_none(), "must not spawn a command");
        assert!(server.get("args").is_none(), "must not pass node args");
        assert!(server.get("env").is_none(), "identity must come from JWT");
        assert!(
            !written.contains("node") && !written.contains("coord-mcp.mjs"),
            "config must not reference the Node sidecar: {written}"
        );

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
        match prev {
            Some(p) => std::env::set_var("COORD_HTTP_URL", p),
            None => std::env::remove_var("COORD_HTTP_URL"),
        }
    }

    /// Smoke test using a fake claude binary fixture (the existing
    /// `mock_claude_cli` binary in `bin/`). Run via cargo so the test
    /// runtime resolves the fixture path automatically.
    ///
    /// Gated behind `QONTINUI_AGENT_RUNTIME_E2E=1` because the fixture
    /// path is build-target-dependent.
    #[tokio::test]
    async fn fake_claude_e2e_smoke() {
        if std::env::var("QONTINUI_AGENT_RUNTIME_E2E").ok().as_deref() != Some("1") {
            return;
        }
        // Build a child via a portable echo command — verifies the
        // spawn-and-pump shape end-to-end without depending on the
        // real claude binary.
        std::env::set_var(
            "QONTINUI_CLAUDE_BIN",
            if cfg!(target_os = "windows") {
                "cmd"
            } else {
                "sh"
            },
        );
        let tmp = std::env::temp_dir();
        let mut child = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/c", "echo agent-runtime-fake-output"]);
            c.current_dir(&tmp);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c.spawn().unwrap()
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "echo agent-runtime-fake-output"]);
            c.current_dir(&tmp);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c.spawn().unwrap()
        };
        let agent_id = uuid::Uuid::now_v7();
        let exit = pump_subprocess(agent_id, &mut child, None).await.unwrap();
        assert_eq!(exit, 0);
    }
}
