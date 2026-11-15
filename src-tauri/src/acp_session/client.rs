//! The ACP client: spawn, handshake, prompt, close.
//!
//! Built on the v5 role/builder API (`Client.v1().connect_with(..)`); there is
//! no `Agent` trait and no `ClientSideConnection` in 5.x. The connection runs
//! in its own task and the [`AcpSession`] handle talks to it over a command
//! channel, because `connect_with` owns the connection for exactly as long as
//! its foreground closure runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, EnvVariable, FileSystemCapabilities,
    HttpHeader, Implementation, InitializeRequest, McpServer, McpServerHttp, McpServerStdio,
    NewSessionRequest, PermissionOptionKind, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SystemPrompt, TextContent,
    WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{
    is_connection_terminated, is_incoming_transport_closed, Agent, ByteStreams, Client, ConnectTo,
    UntypedMessage,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::map::{map_notification, AcpUsage};
use super::{
    AcpError, AcpErrorKind, AcpLaunch, AcpStopReason, AgentCaps, BriefingDelivery, McpEntry,
    PromptOutcome, SessionInfo,
};

/// How long a turn that was told to cancel gets to answer with a stop reason
/// before the turn is declared timed out.
const CANCEL_GRACE: Duration = Duration::from_secs(30);

/// How long `close` waits for the connection task, then for the child, before
/// escalating.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// The tenant's permission policy as a predicate: does it DENY a request right
/// now? Production passes `permission_accept::stream_json_denies` — the same
/// answer `claude_session::dispatcher::control_response_for` acts on — so the
/// ACP lane and the Claude headless lane cannot disagree about the
/// `permission_auto_accept` dial. A test injects its own.
pub(crate) type PermissionPolicy = Arc<dyn Fn() -> bool + Send + Sync>;

/// Picks the coord-mcp server to hand the agent once its capabilities are
/// known (HTTP is optional in ACP, stdio mandatory).
pub(crate) type McpChooser = Box<dyn FnOnce(&AgentCaps) -> Option<McpEntry> + Send>;

/// Everything `session/new` needs, besides the transport.
pub(crate) struct SessionConfig {
    /// Absolute working directory of the session; `fs/*` is confined to it.
    pub cwd: PathBuf,
    /// The runner briefing. Delivered as `systemPrompt` when advertised, else
    /// prepended to the first prompt.
    pub briefing: Option<String>,
    pub mcp: Option<McpChooser>,
    /// JSONL transcript of every `session/update`. `None` records nothing.
    pub transcript: Option<PathBuf>,
    pub permission: PermissionPolicy,
}

enum Cmd {
    Prompt {
        text: String,
        bound: Duration,
        reply: oneshot::Sender<Result<PromptOutcome, AcpError>>,
    },
    Close,
}

/// A live ACP session. Owns the connection task and (when the agent is a child
/// process) the child.
pub(crate) struct AcpSession {
    info: SessionInfo,
    cmd: mpsc::Sender<Cmd>,
    task: Option<tokio::task::JoinHandle<()>>,
    child: Option<ChildRuntime>,
}

/// The child process and its tree guard. The guard kills the process tree when
/// dropped, so a session dropped without `close` still cannot leak the agent.
struct ChildRuntime {
    child: tokio::process::Child,
    _tree: crate::process_helpers::ChildTreeGuard,
}

impl AcpSession {
    pub(crate) fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// The child's pid, when the agent is a child process still running.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.child.id())
    }

    /// Run one turn: `session/prompt`, streaming updates to the transcript, and
    /// answer with its stop reason. `bound` is the wall-clock bound: past it the
    /// turn is cancelled, and a turn that still does not stop within
    /// [`CANCEL_GRACE`] is [`AcpErrorKind::TimedOut`].
    pub(crate) async fn prompt(
        &mut self,
        text: &str,
        bound: Duration,
    ) -> Result<PromptOutcome, AcpError> {
        // `PromptPrefix` briefing delivery is applied by the connection task to
        // the first prompt it serves, so a caller never has to remember it.
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Prompt {
                text: text.to_string(),
                bound,
                reply,
            })
            .await
            .map_err(|_| {
                AcpError::new(AcpErrorKind::TransportLost, "the connection task has ended")
            })?;
        rx.await.map_err(|_| {
            AcpError::new(
                AcpErrorKind::TransportLost,
                "the connection task ended before answering the prompt",
            )
        })?
    }

    /// End the session and reap the child. Always reaps: a connection task that
    /// does not end in [`CLOSE_WAIT`] is aborted, and a child that does not exit
    /// in [`CLOSE_WAIT`] after its stdin closed is killed with its tree.
    pub(crate) async fn close(mut self) {
        // A full / closed channel means the task is already gone.
        let _ = self.cmd.send(Cmd::Close).await;
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(CLOSE_WAIT, &mut task).await.is_err() {
                task.abort();
            }
        }
        if let Some(mut rt) = self.child.take() {
            // The connection task dropped the transport, so the child's stdin is
            // closed: a well-behaved agent exits on EOF.
            if tokio::time::timeout(CLOSE_WAIT, rt.child.wait())
                .await
                .is_err()
            {
                let _ = rt.child.start_kill();
                let _ = rt.child.wait().await;
            }
            // `_tree` drops here and kills any descendants the agent left.
        }
    }
}

/// Build the agent child's command: a tokio `Command` run through the runner's
/// headless env finalizer, so the credential scrub and git posture are the
/// Claude path's. Extracted from [`spawn_acp_agent`] because spawning cannot run
/// in a unit test and the scrub must be assertable.
pub(crate) fn build_acp_command(
    launch: &AcpLaunch,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    served: &crate::served_corpus::ServedCorpus,
) -> tokio::process::Command {
    let mut cmd = crate::process_helpers::tokio_no_window(&launch.program);
    crate::process_helpers::ChildTreeGuard::arm_tokio(&mut cmd);
    cmd.args(&launch.args)
        .current_dir(&launch.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &launch.env {
        cmd.env(k, v);
    }
    // LAST: the runner-context marker, the git posture, then the credential
    // scrub (last-write-wins by construction).
    crate::agent_runtime::finalize_headless_child_env(&mut cmd, coord_mcp, served);
    cmd
}

/// Spawn the agent child and open an ACP session over its stdio.
pub(crate) async fn spawn_acp_agent(
    launch: AcpLaunch,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    // What `<cwd>/.claude/` serves, measured by the caller (it needs I/O the
    // briefing renderer forbids, and the caller renders the briefing from it
    // too, so one measurement feeds both).
    served: &crate::served_corpus::ServedCorpus,
    cfg: SessionConfig,
) -> Result<AcpSession, AcpError> {
    let mut cmd = build_acp_command(&launch, coord_mcp, served);
    let mut child = cmd.spawn().map_err(|e| {
        AcpError::new(
            AcpErrorKind::Spawn,
            format!("could not spawn {}: {e}", launch.program.display()),
        )
    })?;
    let tree = crate::process_helpers::ChildTreeGuard::attach_armed_tokio(&child);
    let (stdin, stdout, stderr) =
        match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(i), Some(o), Some(e)) => (i, o, e),
            _ => {
                return Err(AcpError::new(
                    AcpErrorKind::Spawn,
                    "the agent child has no piped stdio",
                ))
            }
        };
    // Drain stderr so a chatty agent cannot fill the pipe and stall; keep it at
    // debug level (it is diagnostics, and may echo prompt text).
    let program = launch.program.display().to_string();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("acp agent {program} stderr: {line}");
        }
    });
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let runtime = ChildRuntime { child, _tree: tree };
    match start_acp_session(transport, cfg, Some(runtime)).await {
        Ok(s) => Ok(s),
        Err((e, runtime)) => {
            // The handshake failed: reap the child here, `start` has no handle.
            if let Some(mut rt) = runtime {
                let _ = rt.child.start_kill();
                let _ = rt.child.wait().await;
            }
            Err(e)
        }
    }
}

/// Open an ACP session over any transport (a child's stdio in production, an
/// in-memory pair in the fake-agent tests). `child` is handed back on failure so
/// the caller can reap it.
#[cfg(test)]
pub(crate) async fn start_acp_session_over(
    transport: impl ConnectTo<Client> + 'static,
    cfg: SessionConfig,
) -> Result<AcpSession, AcpError> {
    start_acp_session(transport, cfg, None)
        .await
        .map_err(|(e, _)| e)
}

async fn start_acp_session(
    transport: impl ConnectTo<Client> + 'static,
    cfg: SessionConfig,
    child: Option<ChildRuntime>,
) -> Result<AcpSession, (AcpError, Option<ChildRuntime>)> {
    let (ready_tx, ready_rx) = oneshot::channel::<Result<SessionInfo, AcpError>>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(4);
    let (upd_tx, upd_rx) = mpsc::unbounded_channel::<(String, Value)>();

    let permission = cfg.permission.clone();
    let fs_root = cfg.cwd.clone();
    let fs_root_w = cfg.cwd.clone();
    let task = tokio::spawn(async move {
        let result = Client
            .v1()
            .on_receive_request(
                async move |req: RequestPermissionRequest, responder, _cx| {
                    responder.respond(permission_response(&req, (permission)()))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: ReadTextFileRequest, responder, _cx| match read_confined(
                    &fs_root, &req,
                ) {
                    Ok(content) => responder.respond(ReadTextFileResponse::new(content)),
                    Err(e) => responder.respond_with_error(e),
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: WriteTextFileRequest, responder, _cx| match write_confined(
                    &fs_root_w, &req,
                ) {
                    Ok(()) => responder.respond(WriteTextFileResponse::new()),
                    Err(e) => responder.respond_with_error(e),
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |n: UntypedMessage, _cx| {
                    // Every notification, whatever its method, is queued
                    // untouched; `map_notification` decides what it is. A closed
                    // receiver (session closing) is not an error here.
                    let (method, params) = n.into_parts();
                    let _ = upd_tx.send((method, params));
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
                Ok(drive(cx, cfg, ready_tx, cmd_rx, upd_rx).await)
            })
            .await;
        if let Err(e) = result {
            // The connection ended abnormally (handler or transport error).
            // Callers learn of it through their own failed request; log it.
            tracing::debug!("acp: connection ended with an error: {e}");
        }
    });

    match ready_rx.await {
        Ok(Ok(info)) => Ok(AcpSession {
            info,
            cmd: cmd_tx,
            task: Some(task),
            child,
        }),
        Ok(Err(e)) => {
            task.abort();
            Err((e, child))
        }
        Err(_) => {
            task.abort();
            Err((
                AcpError::new(
                    AcpErrorKind::TransportLost,
                    "the connection ended before the session was established",
                ),
                child,
            ))
        }
    }
}

/// Translate an SDK error from a request into the lane's error kinds.
fn classify(e: &agent_client_protocol::Error, what: &str) -> AcpError {
    if is_connection_terminated(e) || is_incoming_transport_closed(e) {
        AcpError::new(
            AcpErrorKind::TransportLost,
            format!("{what}: the agent connection closed ({})", e.message),
        )
    } else {
        AcpError::new(
            AcpErrorKind::Protocol,
            format!("{what}: {} (code {})", e.message, i32::from(e.code)),
        )
    }
}

fn text_block(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(text))
}

fn to_mcp_server(entry: &McpEntry) -> McpServer {
    match entry {
        McpEntry::Http { name, url, headers } => McpServer::Http(
            McpServerHttp::new(name.clone(), url.clone()).headers(
                headers
                    .iter()
                    .map(|(k, v)| HttpHeader::new(k.clone(), v.clone()))
                    .collect(),
            ),
        ),
        McpEntry::Stdio {
            name,
            command,
            args,
            env,
        } => McpServer::Stdio(
            McpServerStdio::new(name.clone(), command.clone())
                .args(args.clone())
                .env(
                    env.iter()
                        .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
                        .collect(),
                ),
        ),
    }
}

/// The connection's foreground: handshake, then serve commands until `Close`.
async fn drive(
    cx: ConnectionTo<Agent>,
    cfg: SessionConfig,
    ready: oneshot::Sender<Result<SessionInfo, AcpError>>,
    mut cmds: mpsc::Receiver<Cmd>,
    mut updates: mpsc::UnboundedReceiver<(String, Value)>,
) {
    let mut transcript = Transcript::open(cfg.transcript.as_deref());
    let setup = handshake(&cx, cfg).await;
    let (session_id, prefix) = match setup {
        Ok((info, sid, prefix)) => {
            let _ = ready.send(Ok(info));
            (sid, prefix)
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut prefix = prefix;
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::Close => break,
            Cmd::Prompt { text, bound, reply } => {
                let text = match prefix.take() {
                    Some(p) => format!("{p}\n\n{text}"),
                    None => text,
                };
                let out = run_turn(
                    &cx,
                    &session_id,
                    &text,
                    bound,
                    &mut updates,
                    &mut transcript,
                )
                .await;
                let lost = out
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.kind == AcpErrorKind::TransportLost);
                let _ = reply.send(out);
                if lost {
                    break;
                }
            }
        }
    }
}

async fn handshake(
    cx: &ConnectionTo<Agent>,
    cfg: SessionConfig,
) -> Result<
    (
        SessionInfo,
        agent_client_protocol::v1::schema::SessionId,
        Option<String>,
    ),
    AcpError,
> {
    let init = cx
        .send_request(
            InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(
                    ClientCapabilities::new().fs(FileSystemCapabilities::new()
                        .read_text_file(true)
                        .write_text_file(true)),
                    // `terminal/*` is NOT advertised: the agent uses its own
                    // shell tool.
                )
                .client_info(Implementation::new(
                    "qontinui-runner",
                    env!("CARGO_PKG_VERSION"),
                )),
        )
        .block_task()
        .await
        .map_err(|e| classify(&e, "initialize"))?;
    if init.protocol_version != ProtocolVersion::V1 {
        return Err(AcpError::new(
            AcpErrorKind::Protocol,
            format!(
                "initialize: agent speaks protocol {:?}, this client speaks v1 only",
                init.protocol_version
            ),
        ));
    }
    let caps = AgentCaps {
        system_prompt: init
            .agent_capabilities
            .session_capabilities
            .system_prompt
            .is_some(),
        mcp_http: init.agent_capabilities.mcp_capabilities.http,
        load_session: init.agent_capabilities.load_session,
    };

    let mcp = cfg.mcp.and_then(|choose| choose(&caps));
    let mcp_delivered = mcp.is_some();
    let (briefing_delivery, system_prompt, prefix) = match cfg.briefing {
        None => (BriefingDelivery::None, None, None),
        Some(b) if caps.system_prompt => (
            BriefingDelivery::SystemPrompt,
            Some(SystemPrompt::new(vec![text_block(b)])),
            None,
        ),
        Some(b) => (BriefingDelivery::PromptPrefix, None, Some(b)),
    };
    let mut req = NewSessionRequest::new(cfg.cwd.clone())
        .mcp_servers(mcp.iter().map(to_mcp_server).collect());
    if let Some(sp) = system_prompt {
        req = req.system_prompt(sp);
    }
    let resp = cx
        .send_request(req)
        .block_task()
        .await
        .map_err(|e| classify(&e, "session/new"))?;
    let info = SessionInfo {
        agent_session_id: resp.session_id.to_string(),
        caps,
        briefing_delivery,
        mcp_delivered,
    };
    Ok((info, resp.session_id, prefix))
}

async fn run_turn(
    cx: &ConnectionTo<Agent>,
    session_id: &agent_client_protocol::v1::schema::SessionId,
    text: &str,
    bound: Duration,
    updates: &mut mpsc::UnboundedReceiver<(String, Value)>,
    transcript: &mut Transcript,
) -> Result<PromptOutcome, AcpError> {
    let mut fut = Box::pin(
        cx.send_request(PromptRequest::new(
            session_id.clone(),
            vec![text_block(text)],
        ))
        .block_task(),
    );
    let mut stats = TurnStats::default();
    let mut deadline = tokio::time::Instant::now() + bound;
    let mut cancelled = false;
    let response = loop {
        tokio::select! {
            biased;
            r = &mut fut => break r,
            u = updates.recv() => {
                if let Some((method, params)) = u {
                    stats.record(&method, &params, transcript);
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                if cancelled {
                    return Err(AcpError::new(
                        AcpErrorKind::TimedOut,
                        format!("no stop reason within {}s of cancelling the turn", CANCEL_GRACE.as_secs()),
                    ));
                }
                cancelled = true;
                deadline = tokio::time::Instant::now() + CANCEL_GRACE;
                // Best effort: if this fails the connection is down and the
                // pending request fails next poll.
                let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
            }
        }
    };
    // Notifications that preceded the response were dispatched before it was
    // routed, so they are all queued by now.
    while let Ok((method, params)) = updates.try_recv() {
        stats.record(&method, &params, transcript);
    }
    let response = response.map_err(|e| classify(&e, "session/prompt"))?;
    use agent_client_protocol::v1::schema::StopReason;
    let stop_reason = match response.stop_reason {
        StopReason::EndTurn => AcpStopReason::EndTurn,
        StopReason::MaxTokens => AcpStopReason::MaxTokens,
        StopReason::MaxTurnRequests => AcpStopReason::MaxTurnRequests,
        StopReason::Refusal => AcpStopReason::Refusal,
        StopReason::Cancelled => AcpStopReason::Cancelled,
        other => AcpStopReason::Other(format!("{other:?}")),
    };
    Ok(PromptOutcome {
        stop_reason,
        update_count: stats.count,
        unknown_updates: stats.unknown,
        last_usage: stats.usage,
    })
}

#[derive(Default)]
struct TurnStats {
    count: u32,
    unknown: u32,
    usage: Option<AcpUsage>,
}

impl TurnStats {
    fn record(&mut self, method: &str, params: &Value, transcript: &mut Transcript) {
        let mapped = map_notification(method, params);
        self.count += 1;
        if mapped.unknown {
            self.unknown += 1;
        }
        if let Some(u) = mapped.usage {
            self.usage = Some(u);
        }
        transcript.append(&mapped.line);
    }
}

/// Append-only JSONL transcript. A failed write is logged once and the
/// transcript stops: it is evidence, and must never take the session down.
struct Transcript {
    file: Option<std::fs::File>,
}

impl Transcript {
    fn open(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self { file: None };
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => Self { file: Some(f) },
            Err(e) => {
                tracing::warn!("acp: transcript {} not writable: {e}", path.display());
                Self { file: None }
            }
        }
    }

    fn append(&mut self, line: &Value) {
        use std::io::Write;
        let Some(f) = self.file.as_mut() else { return };
        if let Err(e) = writeln!(f, "{line}") {
            tracing::warn!("acp: transcript write failed ({e}); transcript stops here");
            self.file = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Client-side requests: permission and confined filesystem access.
// ---------------------------------------------------------------------------

/// Answer `session/request_permission` from the tenant policy. Picks by option
/// `kind`, never by position or name; allow prefers the narrower `allow_once`.
/// An answer the agent offered no option for is `Cancelled`.
pub(crate) fn permission_response(
    req: &RequestPermissionRequest,
    denies: bool,
) -> RequestPermissionResponse {
    let wanted: [PermissionOptionKind; 2] = if denies {
        [
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ]
    } else {
        [
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        ]
    };
    let chosen = wanted.iter().find_map(|kind| {
        req.options
            .iter()
            .find(|o| o.kind == *kind)
            .map(|o| o.option_id.clone())
    });
    RequestPermissionResponse::new(match chosen {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        None => RequestPermissionOutcome::Cancelled,
    })
}

fn fs_refusal(message: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data(Value::String(message.into()))
}

/// Resolve `requested` inside `root`, or refuse. The path must be absolute;
/// symlinks and `..` are resolved before the containment test, and for a path
/// that does not exist yet the PARENT is the one resolved.
fn confine(root: &Path, requested: &Path) -> Result<PathBuf, agent_client_protocol::Error> {
    if !requested.is_absolute() {
        return Err(fs_refusal("path must be absolute"));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|e| fs_refusal(format!("session cwd is not resolvable: {e}")))?;
    let resolved = match std::fs::canonicalize(requested) {
        Ok(p) => p,
        Err(_) => {
            let name = requested
                .file_name()
                .ok_or_else(|| fs_refusal("path has no file name"))?;
            let parent = requested
                .parent()
                .ok_or_else(|| fs_refusal("path has no parent"))?;
            std::fs::canonicalize(parent)
                .map_err(|e| fs_refusal(format!("parent directory is not resolvable: {e}")))?
                .join(name)
        }
    };
    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(fs_refusal("path is outside the session working directory"))
    }
}

pub(super) fn read_confined(
    root: &Path,
    req: &ReadTextFileRequest,
) -> Result<String, agent_client_protocol::Error> {
    let path = confine(root, &req.path)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| fs_refusal(format!("cannot read {}: {e}", req.path.display())))?;
    // `line` is 1-based; `limit` is a line count.
    let start = req.line.map_or(0, |l| l.saturating_sub(1) as usize);
    if start == 0 && req.limit.is_none() {
        return Ok(text);
    }
    let lines = text.split_inclusive('\n').skip(start);
    Ok(match req.limit {
        Some(n) => lines.take(n as usize).collect(),
        None => lines.collect(),
    })
}

pub(super) fn write_confined(
    root: &Path,
    req: &WriteTextFileRequest,
) -> Result<(), agent_client_protocol::Error> {
    let path = confine(root, &req.path)?;
    std::fs::write(&path, &req.content)
        .map_err(|e| fs_refusal(format!("cannot write {}: {e}", req.path.display())))
}
