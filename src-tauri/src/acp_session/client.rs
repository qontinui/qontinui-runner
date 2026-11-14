//! The ACP client: spawn an ACP-native agent CLI as our own child, connect to
//! it over stdio with the crate's `ByteStreams` transport, and drive one
//! session through `initialize` → `session/new` → `session/prompt` → close.
//!
//! Built on the `agent-client-protocol` 5.x role/builder API
//! (`Client.v1().connect_with(..)`). The crate's own `AcpAgent` spawner
//! (feature `process`) is deliberately NOT used: it can only ADD environment
//! variables, so the runner's credential scrub could not apply to its child.
//! The child here goes through [`finalize_headless_child_env`] exactly as the
//! headless Claude child does.
//!
//! [`finalize_headless_child_env`]: crate::agent_runtime::finalize_headless_child_env

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ClientCapabilities, CloseSessionRequest, ContentBlock, FileSystemCapabilities, Implementation,
    InitializeRequest, InitializeResponse, McpServer, NewSessionRequest, PermissionOption,
    PermissionOptionKind, PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SystemPrompt, TextContent, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectTo, JsonRpcMessage, UntypedMessage,
};
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::map::{
    map_notification, stop_reason_from_wire, usage_events, AcpStopReason, AcpUsage,
    PermissionDecision, TranscriptLine,
};
use crate::process_helpers::ChildTreeGuard;

/// Bound on `initialize` and `session/new`. A healthy agent answers both in
/// well under a second; one that never answers must not park the caller.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on the courtesy `session/close` request during [`AcpSession::close`].
const CLOSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the connection driver gets to wind down after it is told to stop.
const DRIVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long the child gets to exit on its own once its stdin is closed, before
/// it is killed.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long the stderr copier gets to reach EOF after the child exited.
const STDERR_DRAIN_GRACE: Duration = Duration::from_secs(1);
/// How many trailing stderr lines of the child are kept for diagnostics.
const STDERR_TAIL_LINES: usize = 50;

/// What to launch: an ACP agent CLI and its working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Extra environment for the child, applied BEFORE the headless env
    /// finalizer — so the credential scrub still wins over anything here.
    pub env: Vec<(String, String)>,
    /// The session's working directory, and the root `fs/*` is confined to.
    pub cwd: PathBuf,
}

/// What the agent advertised in its `initialize` response that the runner acts
/// on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentAdvertised {
    pub protocol_version: u16,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    /// `sessionCapabilities.systemPrompt` — the agent accepts `systemPrompt`
    /// on `session/new`.
    pub system_prompt: bool,
    /// `mcpCapabilities.http` — the agent accepts HTTP MCP servers.
    pub mcp_http: bool,
    /// `loadSession`.
    pub load_session: bool,
    /// `sessionCapabilities.close` — the agent accepts `session/close`.
    pub session_close: bool,
}

impl AgentAdvertised {
    fn from_initialize(response: &InitializeResponse) -> Self {
        let caps = &response.agent_capabilities;
        Self {
            protocol_version: response.protocol_version.as_u16(),
            agent_name: response.agent_info.as_ref().map(|i| i.name.clone()),
            agent_version: response.agent_info.as_ref().map(|i| i.version.clone()),
            system_prompt: caps.session_capabilities.system_prompt.is_some(),
            mcp_http: caps.mcp_capabilities.http,
            load_session: caps.load_session,
            session_close: caps.session_capabilities.close.is_some(),
        }
    }
}

/// How the briefing reached the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BriefingDelivery {
    /// No briefing was given.
    None,
    /// Sent as `systemPrompt` (append mode) on `session/new`.
    SystemPrompt,
    /// The agent does not advertise `systemPrompt`, so the briefing is
    /// prepended to the first prompt.
    PromptPrefix,
}

/// What `session/new` is asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct AcpSessionRequest {
    pub mcp_servers: Vec<McpServer>,
    /// The runner briefing. Delivery depends on what the agent advertised; see
    /// [`BriefingDelivery`].
    pub briefing: Option<String>,
}

/// The outcome of `session/new`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StartedSession {
    /// The agent-minted session id (ACP has no client-side id pinning).
    pub agent_session_id: String,
    pub briefing_delivery: BriefingDelivery,
}

/// The outcome of one `session/prompt` turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PromptOutcome {
    pub stop_reason: AcpStopReason,
    /// Every transcript line received since the previous turn, in order.
    pub updates: Vec<TranscriptLine>,
    /// The usage events among `updates`.
    pub usage: Vec<AcpUsage>,
}

/// How the child ended, as observed by [`AcpSession::close`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ChildExit {
    /// The exit code, when the child exited with one.
    pub code: Option<i32>,
    /// The runner killed the child because it did not exit within the grace.
    pub killed: bool,
    /// The last lines the child wrote to stderr.
    pub stderr_tail: Vec<String>,
}

/// The outcome of [`AcpSession::close`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CloseOutcome {
    /// `None` for an in-process transport, which has no child.
    pub child: Option<ChildExit>,
}

/// Why an ACP operation failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpError {
    #[error("failed to spawn ACP agent {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("ACP session cwd {cwd} is unusable: {source}")]
    Cwd {
        cwd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("ACP agent transport lost during {during}")]
    TransportLost { during: &'static str },
    #[error("ACP agent exited during {during} (exit code {code:?}); stderr tail: {tail}", code = .exit.code, tail = .exit.stderr_tail.join(" | "))]
    AgentExited {
        during: &'static str,
        exit: ChildExit,
    },
    #[error("ACP {during} timed out after {secs}s")]
    Timeout { during: &'static str, secs: u64 },
    #[error("ACP {during} failed: {message}")]
    Protocol {
        during: &'static str,
        message: String,
    },
    #[error("ACP agent speaks protocol version {0}; the runner speaks only v1")]
    ProtocolVersion(u16),
    #[error("ACP agent does not advertise {0}")]
    Unsupported(&'static str),
    #[error("no ACP session: session/new has not succeeded")]
    NoSession,
    #[error("an ACP session already exists on this connection")]
    SessionExists,
}

/// Whether a permission request is DENIED. Evaluated per request, so a change
/// of the tenant dial takes effect on the next request.
pub(crate) type PermissionPolicy = Arc<dyn Fn() -> bool + Send + Sync>;

/// The production policy: the tenant's `permission_auto_accept` dial, exactly
/// as the headless Claude path's `control_response_for` reads it
/// (`claude_session/dispatcher.rs`).
pub(crate) fn tenant_permission_policy() -> PermissionPolicy {
    Arc::new(crate::session::permission_accept::stream_json_denies)
}

/// The answer to one `session/request_permission`. PURE.
///
/// Allowed (the dial does not deny): the narrowest allow option offered —
/// `allow_once`, else `allow_always`. Denied: `reject_once`, else
/// `reject_always`. Options are chosen by `kind`, never by position or name.
/// When no option of the needed kind is offered the answer is `cancelled`, so
/// the runner never selects an option that means the opposite of its decision.
pub(crate) fn permission_outcome(
    options: &[PermissionOption],
    denies: bool,
) -> (PermissionDecision, RequestPermissionOutcome) {
    let (decision, preferred) = if denies {
        (
            PermissionDecision::Deny,
            [
                PermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways,
            ],
        )
    } else {
        (
            PermissionDecision::Allow,
            [
                PermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways,
            ],
        )
    };
    let chosen = preferred
        .iter()
        .find_map(|kind| options.iter().find(|o| o.kind == *kind));
    match chosen {
        Some(option) => (
            decision,
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
            )),
        ),
        None => (
            PermissionDecision::Cancelled,
            RequestPermissionOutcome::Cancelled,
        ),
    }
}

/// Why an `fs/*` path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FsRefusal {
    #[error("path {0} contains a `..` component")]
    ParentComponent(String),
    #[error("path {0} resolves outside the session cwd")]
    OutsideCwd(String),
    #[error("path {path} cannot be resolved: {reason}")]
    Unresolvable { path: String, reason: String },
}

/// Resolve `requested` inside `root`, refusing anything that escapes it.
///
/// `root` must already be canonical. A relative `requested` is taken relative
/// to `root`. Any `..` component is refused outright. The longest prefix of
/// the path that exists (by `symlink_metadata`, so a dangling symlink counts
/// as existing) is canonicalized — which resolves every symlink in it — and
/// must lie inside `root`; the not-yet-existing tail is plain names by then,
/// so it cannot escape. A dangling symlink cannot be canonicalized and is
/// refused, because writing through it would land wherever it points.
pub(crate) async fn confine_path(root: &Path, requested: &Path) -> Result<PathBuf, FsRefusal> {
    let shown = requested.display().to_string();
    if requested
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(FsRefusal::ParentComponent(shown));
    }
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    let mut existing = joined.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while tokio::fs::symlink_metadata(existing).await.is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent;
            }
            _ => {
                return Err(FsRefusal::Unresolvable {
                    path: shown,
                    reason: "no existing ancestor".into(),
                })
            }
        }
    }
    let canonical =
        tokio::fs::canonicalize(existing)
            .await
            .map_err(|e| FsRefusal::Unresolvable {
                path: shown.clone(),
                reason: e.to_string(),
            })?;
    if !canonical.starts_with(root) {
        return Err(FsRefusal::OutsideCwd(shown));
    }
    Ok(tail
        .iter()
        .rev()
        .fold(canonical, |path, name| path.join(name)))
}

/// `content` limited to `limit` lines starting at 1-based `line`.
fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_string();
    }
    let skip = line.map_or(0, |l| l.saturating_sub(1) as usize);
    let take = limit.map_or(usize::MAX, |l| l as usize);
    content
        .split_inclusive('\n')
        .skip(skip)
        .take(take)
        .collect()
}

fn fs_error(message: impl ToString) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(serde_json::json!({
        "reason": message.to_string()
    }))
}

/// State the connection's handlers share with the [`AcpSession`].
struct ClientState {
    /// Canonical session cwd: the `fs/*` confinement root.
    root: PathBuf,
    denies: PermissionPolicy,
    /// Transcript lines received and not yet returned by a prompt.
    pending: Mutex<Vec<TranscriptLine>>,
    /// Trailing stderr lines of the child (empty for an in-process transport).
    stderr_tail: Mutex<VecDeque<String>>,
}

impl ClientState {
    fn push(&self, line: TranscriptLine) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(line);
        }
    }

    fn drain(&self) -> Vec<TranscriptLine> {
        self.pending
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default()
    }

    fn push_stderr(&self, line: String) {
        if let Ok(mut tail) = self.stderr_tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn answer_permission(&self, request: &RequestPermissionRequest) -> RequestPermissionResponse {
        let (decision, outcome) = permission_outcome(&request.options, (self.denies)());
        let option_id = match &outcome {
            RequestPermissionOutcome::Selected(s) => Some(s.option_id.0.to_string()),
            _ => None,
        };
        self.push(TranscriptLine::Permission {
            session_id: request.session_id.0.to_string(),
            tool_call_id: request.tool_call.tool_call_id.0.to_string(),
            title: request.tool_call.fields.title.clone(),
            decision,
            option_id,
        });
        RequestPermissionResponse::new(outcome)
    }

    async fn read_text_file(
        &self,
        request: &ReadTextFileRequest,
    ) -> Result<ReadTextFileResponse, agent_client_protocol::Error> {
        let path = confine_path(&self.root, &request.path)
            .await
            .map_err(fs_error)?;
        let content = tokio::fs::read_to_string(&path).await.map_err(fs_error)?;
        Ok(ReadTextFileResponse::new(slice_lines(
            &content,
            request.line,
            request.limit,
        )))
    }

    async fn write_text_file(
        &self,
        request: &WriteTextFileRequest,
    ) -> Result<WriteTextFileResponse, agent_client_protocol::Error> {
        let path = confine_path(&self.root, &request.path)
            .await
            .map_err(fs_error)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(fs_error)?;
        }
        tokio::fs::write(&path, request.content.as_bytes())
            .await
            .map_err(fs_error)?;
        Ok(WriteTextFileResponse::new())
    }
}

/// The spawned agent process and the guard that reaps its tree.
struct AgentChild {
    child: tokio::process::Child,
    tree: ChildTreeGuard,
    /// The task copying the child's stderr into the tail; awaited (bounded)
    /// after the child exits so the last lines are in the tail.
    stderr_reader: Option<JoinHandle<()>>,
}

/// One connection to an ACP agent, carrying at most one session.
///
/// An ACP agent does NOT exit at `end_turn`: the caller must call
/// [`AcpSession::close`], which ends the session and reaps the child. Dropping
/// an unclosed session still kills the child (`kill_on_drop` plus the tree
/// guard), but loses the exit observation.
pub(crate) struct AcpSession {
    cx: ConnectionTo<Agent>,
    state: Arc<ClientState>,
    stop: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<Result<(), agent_client_protocol::Error>>>,
    child: Option<AgentChild>,
    advertised: AgentAdvertised,
    session_id: Option<SessionId>,
    /// The briefing still owed to the first prompt (`PromptPrefix` delivery).
    pending_briefing: Option<String>,
}

/// Build the agent's command: program, args, cwd, the launch env, then the
/// headless env finalizer (runner context, git posture, credential scrub —
/// last, so it wins), stdio pipes, and the process-tree arm.
pub(crate) fn build_acp_command(
    launch: &AcpLaunch,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    served: &crate::served_corpus::ServedCorpus,
) -> tokio::process::Command {
    let mut cmd = crate::process_helpers::tokio_no_window(&launch.program);
    cmd.args(&launch.args).current_dir(&launch.cwd);
    for (key, value) in &launch.env {
        cmd.env(key, value);
    }
    crate::agent_runtime::finalize_headless_child_env(&mut cmd, coord_mcp, served);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    ChildTreeGuard::arm_tokio(&mut cmd);
    cmd
}

/// Spawn the agent described by `launch` and complete `initialize` with it.
///
/// `coord_mcp` and `served` are the caller's per-session measurements that the
/// headless env finalizer renders into the child's `QONTINUI_RUNNER_CONTEXT`,
/// exactly as for the headless Claude child. Permission requests are answered
/// by the tenant's `permission_auto_accept` dial.
pub(crate) async fn spawn_acp_agent(
    launch: &AcpLaunch,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    served: &crate::served_corpus::ServedCorpus,
) -> Result<AcpSession, AcpError> {
    spawn_acp_agent_with_policy(launch, coord_mcp, served, tenant_permission_policy()).await
}

/// [`spawn_acp_agent`] with an explicit permission policy.
pub(crate) async fn spawn_acp_agent_with_policy(
    launch: &AcpLaunch,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    served: &crate::served_corpus::ServedCorpus,
    policy: PermissionPolicy,
) -> Result<AcpSession, AcpError> {
    let root = canonical_cwd(&launch.cwd).await?;
    let mut cmd = build_acp_command(launch, coord_mcp, served);
    let program = launch.program.display().to_string();
    let mut child = cmd.spawn().map_err(|source| AcpError::Spawn {
        program: program.clone(),
        source,
    })?;
    let tree = ChildTreeGuard::attach_armed_tokio(&child);
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(AcpError::Spawn {
            program,
            source: std::io::Error::other("child stdio was not piped"),
        });
    };
    let state = new_state(root, policy);
    let stderr_reader = child.stderr.take().map(|stderr| {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp_session", "agent stderr: {line}");
                state.push_stderr(line);
            }
        })
    });
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let agent_child = AgentChild {
        child,
        tree,
        stderr_reader,
    };
    match AcpSession::connect_with_state(transport, state, Some(agent_child)).await {
        Ok(session) => Ok(session),
        Err((error, session)) => {
            let exit = match session {
                Some(session) => session.close().await.child,
                None => None,
            };
            // A transport lost during the handshake is the child dying: say so
            // with its exit code and last stderr lines, which name the cause
            // (a missing login, an unknown flag) far better than "transport".
            Err(match (error, exit) {
                (AcpError::TransportLost { during }, Some(exit)) => {
                    AcpError::AgentExited { during, exit }
                }
                (error, _) => error,
            })
        }
    }
}

async fn canonical_cwd(cwd: &Path) -> Result<PathBuf, AcpError> {
    tokio::fs::canonicalize(cwd)
        .await
        .map_err(|source| AcpError::Cwd {
            cwd: cwd.display().to_string(),
            source,
        })
}

fn new_state(root: PathBuf, denies: PermissionPolicy) -> Arc<ClientState> {
    Arc::new(ClientState {
        root,
        denies,
        pending: Mutex::new(Vec::new()),
        stderr_tail: Mutex::new(VecDeque::new()),
    })
}

fn protocol_error(during: &'static str) -> impl FnOnce(agent_client_protocol::Error) -> AcpError {
    move |e| AcpError::Protocol {
        during,
        message: e.to_string(),
    }
}

impl AcpSession {
    /// Connect over an arbitrary transport (no child) and complete
    /// `initialize`. `cwd` is the `fs/*` confinement root.
    pub(crate) async fn connect(
        transport: impl ConnectTo<Client> + 'static,
        cwd: &Path,
        policy: PermissionPolicy,
    ) -> Result<Self, AcpError> {
        let state = new_state(canonical_cwd(cwd).await?, policy);
        match Self::connect_with_state(transport, state, None).await {
            Ok(session) => Ok(session),
            Err((error, session)) => {
                if let Some(session) = session {
                    session.close().await;
                }
                Err(error)
            }
        }
    }

    /// Start the connection driver, then `initialize`. On an `initialize`
    /// failure the half-built session is handed back so the caller can close
    /// it (and reap the child).
    async fn connect_with_state(
        transport: impl ConnectTo<Client> + 'static,
        state: Arc<ClientState>,
        child: Option<AgentChild>,
    ) -> Result<Self, (AcpError, Option<Self>)> {
        let (cx_tx, cx_rx) = oneshot::channel::<ConnectionTo<Agent>>();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();

        let permission_state = Arc::clone(&state);
        let read_state = Arc::clone(&state);
        let write_state = Arc::clone(&state);
        let notify_state = Arc::clone(&state);
        let builder = Client
            .v1()
            .name("qontinui-runner")
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _cx| {
                    responder.respond(permission_state.answer_permission(&request))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: ReadTextFileRequest, responder, _cx| {
                    responder.respond_with_result(read_state.read_text_file(&request).await)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: WriteTextFileRequest, responder, _cx| {
                    responder.respond_with_result(write_state.write_text_file(&request).await)
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |notification: UntypedMessage, _cx| {
                    notify_state.push(map_notification(
                        notification.method(),
                        notification.params(),
                    ));
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            );

        let driver = tokio::spawn(async move {
            builder
                .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
                    // The receiver only disappears if the session was dropped
                    // mid-construction; there is then nothing to serve.
                    if cx_tx.send(cx.clone()).is_err() {
                        return Ok(());
                    }
                    tokio::select! {
                        _ = stop_rx => {}
                        () = cx.incoming_closed() => {}
                    }
                    Ok(())
                })
                .await
        });

        let cx = match cx_rx.await {
            Ok(cx) => cx,
            Err(_) => {
                // The driver ended before handing over a connection; keep its
                // error when it has one.
                let message = match driver.await {
                    Ok(Err(e)) => e.to_string(),
                    Ok(Ok(())) => "connection ended before it started".into(),
                    Err(join) => join.to_string(),
                };
                if let Some(agent) = child {
                    reap(agent, Arc::clone(&state)).await;
                }
                return Err((
                    AcpError::Protocol {
                        during: "connect",
                        message,
                    },
                    None,
                ));
            }
        };

        let mut session = Self {
            cx,
            state,
            stop: Some(stop_tx),
            driver: Some(driver),
            child,
            advertised: AgentAdvertised {
                protocol_version: 0,
                agent_name: None,
                agent_version: None,
                system_prompt: false,
                mcp_http: false,
                load_session: false,
                session_close: false,
            },
            session_id: None,
            pending_briefing: None,
        };
        match session.initialize().await {
            Ok(advertised) => {
                session.advertised = advertised;
                Ok(session)
            }
            Err(e) => Err((e, Some(session))),
        }
    }

    async fn initialize(&self) -> Result<AgentAdvertised, AcpError> {
        let request = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(
                ClientCapabilities::new()
                    .fs(FileSystemCapabilities::new()
                        .read_text_file(true)
                        .write_text_file(true))
                    // `terminal/*` is not offered: the agent uses its own shell
                    // tool, which the runner's env scrub already covers.
                    .terminal(false),
            )
            .client_info(Implementation::new(
                "qontinui-runner",
                env!("CARGO_PKG_VERSION"),
            ));
        let response = self
            .bounded("initialize", self.cx.send_request(request).block_task())
            .await?;
        let advertised = AgentAdvertised::from_initialize(&response);
        if response.protocol_version != ProtocolVersion::V1 {
            return Err(AcpError::ProtocolVersion(advertised.protocol_version));
        }
        Ok(advertised)
    }

    /// Await a handshake request under [`HANDSHAKE_TIMEOUT`], classifying a
    /// failure as transport loss or a protocol error.
    async fn bounded<T>(
        &self,
        during: &'static str,
        request: impl std::future::Future<Output = Result<T, agent_client_protocol::Error>>,
    ) -> Result<T, AcpError> {
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, request).await {
            Err(_) => Err(AcpError::Timeout {
                during,
                secs: HANDSHAKE_TIMEOUT.as_secs(),
            }),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) if self.is_transport_lost(&e) => Err(AcpError::TransportLost { during }),
            Ok(Err(e)) => Err(protocol_error(during)(e)),
        }
    }

    fn is_transport_lost(&self, error: &agent_client_protocol::Error) -> bool {
        agent_client_protocol::is_connection_terminated(error)
            || agent_client_protocol::is_incoming_transport_closed(error)
            || self.cx.is_incoming_closed()
            || self.cx.termination().is_some()
    }

    /// What the agent advertised in `initialize`.
    pub(crate) fn advertised(&self) -> &AgentAdvertised {
        &self.advertised
    }

    /// The agent-minted session id, once `session/new` has succeeded.
    pub(crate) fn agent_session_id(&self) -> Option<&str> {
        self.session_id.as_ref().map(|id| &*id.0)
    }

    /// `session/new { cwd, mcpServers, systemPrompt? }`.
    ///
    /// The briefing is sent as `systemPrompt` only when the agent advertises
    /// it (the protocol forbids sending it otherwise); else it is held and
    /// prepended to the first [`Self::prompt`], and the returned
    /// [`BriefingDelivery::PromptPrefix`] says so. An HTTP MCP server is
    /// refused unless the agent advertises HTTP MCP.
    pub(crate) async fn new_session(
        &mut self,
        request: AcpSessionRequest,
    ) -> Result<StartedSession, AcpError> {
        if self.session_id.is_some() {
            return Err(AcpError::SessionExists);
        }
        if !self.advertised.mcp_http
            && request
                .mcp_servers
                .iter()
                .any(|s| matches!(s, McpServer::Http(_)))
        {
            return Err(AcpError::Unsupported("mcpCapabilities.http"));
        }
        let briefing = request.briefing.filter(|b| !b.trim().is_empty());
        let mut new_session =
            NewSessionRequest::new(self.state.root.clone()).mcp_servers(request.mcp_servers);
        let delivery = match (&briefing, self.advertised.system_prompt) {
            (None, _) => BriefingDelivery::None,
            (Some(text), true) => {
                new_session =
                    new_session.system_prompt(SystemPrompt::new(vec![ContentBlock::Text(
                        TextContent::new(text.clone()),
                    )]));
                BriefingDelivery::SystemPrompt
            }
            (Some(_), false) => BriefingDelivery::PromptPrefix,
        };
        let response = self
            .bounded(
                "session/new",
                self.cx.send_request(new_session).block_task(),
            )
            .await?;
        if delivery == BriefingDelivery::PromptPrefix {
            self.pending_briefing = briefing;
        }
        let started = StartedSession {
            agent_session_id: response.session_id.0.to_string(),
            briefing_delivery: delivery,
        };
        self.session_id = Some(response.session_id);
        Ok(started)
    }

    /// One `session/prompt` turn. Returns when the agent answers with a
    /// `stopReason`, or with [`AcpStopReason::TransportLost`] when the
    /// transport closes first. Unbounded in time by design: a continuation
    /// turn may legitimately run for a long time, and the caller owns that
    /// budget.
    ///
    /// The response is read untyped so a `stopReason` outside ACP v1 maps to
    /// [`AcpStopReason::Unrecognised`] instead of failing the whole turn.
    pub(crate) async fn prompt(&mut self, text: &str) -> Result<PromptOutcome, AcpError> {
        let session_id = self.session_id.clone().ok_or(AcpError::NoSession)?;
        let text = match self.pending_briefing.take() {
            Some(briefing) => format!("{briefing}\n\n{text}"),
            None => text.to_string(),
        };
        let request =
            PromptRequest::new(session_id, vec![ContentBlock::Text(TextContent::new(text))])
                .to_untyped_message()
                .map_err(protocol_error("session/prompt"))?;
        let result = self.cx.send_request(request).block_task().await;
        let stop_reason = match result {
            Ok(response) => match response
                .get("stopReason")
                .and_then(serde_json::Value::as_str)
            {
                Some(raw) => stop_reason_from_wire(raw),
                None => {
                    return Err(AcpError::Protocol {
                        during: "session/prompt",
                        message: format!("response has no stopReason: {response}"),
                    })
                }
            },
            Err(e) if self.is_transport_lost(&e) => AcpStopReason::TransportLost,
            Err(e) => return Err(protocol_error("session/prompt")(e)),
        };
        // Every notification the agent sent before its answer has been
        // dispatched by now: the connection's dispatch loop handles incoming
        // messages one at a time, in order.
        let updates = self.state.drain();
        Ok(PromptOutcome {
            stop_reason,
            usage: usage_events(&updates),
            updates,
        })
    }

    /// Transcript lines received since the last prompt returned (for example
    /// a trailing update after `end_turn`), drained.
    pub(crate) fn take_pending_updates(&self) -> Vec<TranscriptLine> {
        self.state.drain()
    }

    /// End the session and reap the child: a courtesy `session/close` when the
    /// agent advertises it, then the connection is shut down (closing the
    /// child's stdin), the child gets [`CHILD_EXIT_GRACE`] to exit, and is
    /// killed — with its whole process tree — if it has not.
    pub(crate) async fn close(mut self) -> CloseOutcome {
        if let (Some(id), true) = (self.session_id.clone(), self.advertised.session_close) {
            let closing = self
                .cx
                .send_request(CloseSessionRequest::new(id))
                .block_task();
            let _ = tokio::time::timeout(CLOSE_REQUEST_TIMEOUT, closing).await;
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut driver) = self.driver.take() {
            if tokio::time::timeout(DRIVER_SHUTDOWN_GRACE, &mut driver)
                .await
                .is_err()
            {
                driver.abort();
            }
        }
        let child = match self.child.take() {
            Some(agent) => Some(reap(agent, Arc::clone(&self.state)).await),
            None => None,
        };
        CloseOutcome { child }
    }
}

/// Wait for the child to exit, killing it after [`CHILD_EXIT_GRACE`]; then drop
/// the tree guard, which kills any descendant still in its process tree.
async fn reap(mut agent: AgentChild, state: Arc<ClientState>) -> ChildExit {
    let (status, killed) = match tokio::time::timeout(CHILD_EXIT_GRACE, agent.child.wait()).await {
        Ok(status) => (status.ok(), false),
        Err(_) => {
            let _ = agent.child.start_kill();
            (agent.child.wait().await.ok(), true)
        }
    };
    drop(agent.tree);
    if let Some(mut reader) = agent.stderr_reader.take() {
        // Bounded: a surviving grandchild may still hold the stderr pipe open.
        if tokio::time::timeout(STDERR_DRAIN_GRACE, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    ChildExit {
        code: status.and_then(|s| s.code()),
        killed,
        stderr_tail: state.stderr_tail(),
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(driver) = self.driver.take() {
            driver.abort();
        }
        // `kill_on_drop` on the child and the tree guard's own drop reap an
        // unclosed child.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::PermissionOption;

    fn option(id: &'static str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(id, id, kind)
    }

    fn selected(outcome: &RequestPermissionOutcome) -> Option<&str> {
        match outcome {
            RequestPermissionOutcome::Selected(s) => Some(&s.option_id.0),
            _ => None,
        }
    }

    fn all_options() -> Vec<PermissionOption> {
        vec![
            option("always", PermissionOptionKind::AllowAlways),
            option("once", PermissionOptionKind::AllowOnce),
            option("reject-always", PermissionOptionKind::RejectAlways),
            option("reject", PermissionOptionKind::RejectOnce),
        ]
    }

    #[test]
    fn acp_session_permission_allow_prefers_allow_once() {
        let (decision, outcome) = permission_outcome(&all_options(), false);
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(selected(&outcome), Some("once"));
    }

    #[test]
    fn acp_session_permission_deny_prefers_reject_once() {
        let (decision, outcome) = permission_outcome(&all_options(), true);
        assert_eq!(decision, PermissionDecision::Deny);
        assert_eq!(selected(&outcome), Some("reject"));
    }

    #[test]
    fn acp_session_permission_never_selects_the_opposite_kind() {
        let only_reject = vec![option("reject", PermissionOptionKind::RejectOnce)];
        let (decision, outcome) = permission_outcome(&only_reject, false);
        assert_eq!(decision, PermissionDecision::Cancelled);
        assert_eq!(outcome, RequestPermissionOutcome::Cancelled);

        let only_allow = vec![option("always", PermissionOptionKind::AllowAlways)];
        let (decision, outcome) = permission_outcome(&only_allow, true);
        assert_eq!(decision, PermissionDecision::Cancelled);
        assert_eq!(outcome, RequestPermissionOutcome::Cancelled);

        let (_, outcome) = permission_outcome(&only_allow, false);
        assert_eq!(selected(&outcome), Some("always"));
    }

    #[test]
    fn acp_session_slice_lines_is_one_based() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(slice_lines(text, None, None), text);
        assert_eq!(slice_lines(text, Some(2), Some(2)), "b\nc\n");
        assert_eq!(slice_lines(text, Some(4), None), "d\n");
        assert_eq!(slice_lines(text, None, Some(1)), "a\n");
        assert_eq!(slice_lines(text, Some(9), None), "");
    }

    #[tokio::test]
    async fn acp_session_fs_confine_inside_and_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = tokio::fs::canonicalize(dir.path()).await.unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        assert_eq!(
            confine_path(&root, &root.join("a.txt")).await.unwrap(),
            root.join("a.txt")
        );
        assert_eq!(
            confine_path(&root, Path::new("a.txt")).await.unwrap(),
            root.join("a.txt")
        );
        // A not-yet-existing file in a not-yet-existing directory is allowed.
        assert_eq!(
            confine_path(&root, &root.join("new/dir/b.txt"))
                .await
                .unwrap(),
            root.join("new/dir/b.txt")
        );
    }

    #[tokio::test]
    async fn acp_session_fs_confine_refuses_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = tokio::fs::canonicalize(dir.path()).await.unwrap();
        let outside = tempfile::tempdir().unwrap();

        assert!(matches!(
            confine_path(&root, &root.join("../x")).await,
            Err(FsRefusal::ParentComponent(_))
        ));
        assert!(matches!(
            confine_path(&root, &root.join("sub/../../x")).await,
            Err(FsRefusal::ParentComponent(_))
        ));
        assert!(matches!(
            confine_path(&root, &outside.path().join("x")).await,
            Err(FsRefusal::OutsideCwd(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_session_fs_confine_refuses_symlink_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let root = tokio::fs::canonicalize(dir.path()).await.unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();

        // A symlinked directory pointing outside.
        std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
        assert!(matches!(
            confine_path(&root, &root.join("link/secret")).await,
            Err(FsRefusal::OutsideCwd(_))
        ));
        assert!(matches!(
            confine_path(&root, &root.join("link/new-file")).await,
            Err(FsRefusal::OutsideCwd(_))
        ));
        // A dangling symlink: writing through it would land outside.
        std::os::unix::fs::symlink(outside.path().join("absent"), root.join("dangling")).unwrap();
        assert!(matches!(
            confine_path(&root, &root.join("dangling")).await,
            Err(FsRefusal::Unresolvable { .. })
        ));
        // A symlink that stays inside is fine.
        std::fs::write(root.join("real"), "r").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        assert_eq!(
            confine_path(&root, &root.join("alias")).await.unwrap(),
            root.join("real")
        );
    }

    #[test]
    fn acp_session_command_scrubs_credentials_and_sets_runner_context() {
        let launch = AcpLaunch {
            program: PathBuf::from("gemini"),
            args: vec!["--experimental-acp".into()],
            env: crate::terminal::CREDENTIAL_VALUE_ENV_VARS
                .iter()
                .map(|name| (name.to_string(), "hunter2".to_string()))
                .collect(),
            cwd: PathBuf::from("."),
        };
        let cmd = build_acp_command(
            &launch,
            crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
            &crate::served_corpus::ServedCorpus::unknown("test"),
        );
        // The launch env carried every credential; the finalizer's scrub ran
        // after it, so none survives — the same assertion the Claude seam's
        // test makes.
        crate::terminal::assert_credentials_scrubbed_tokio(&cmd, "build_acp_command");
        crate::credential_helper::assert_non_interactive_git_posture_tokio(
            &cmd,
            "build_acp_command",
        );
        let has_context = cmd
            .as_std()
            .get_envs()
            .any(|(k, v)| k == "QONTINUI_RUNNER_CONTEXT" && v.is_some_and(|v| !v.is_empty()));
        assert!(
            has_context,
            "the ACP child must carry QONTINUI_RUNNER_CONTEXT"
        );
        assert_eq!(
            cmd.as_std().get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("--experimental-acp")]
        );
    }
}
