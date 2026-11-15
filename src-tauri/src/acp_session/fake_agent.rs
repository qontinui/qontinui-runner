//! In-process fake ACP agent for tests: a real `Agent.v1()` connection over an
//! in-memory duplex pair, so the client under test speaks genuine ACP JSON-RPC
//! and the only fake part is the model behind the agent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    AgentCapabilities, ContentBlock, InitializeRequest, InitializeResponse, McpCapabilities,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionCapabilities,
    SessionSystemPromptCapabilities, StopReason, TextContent, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectTo, UntypedMessage};
use serde_json::Value;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// What the fake agent does on `session/prompt`.
#[derive(Clone)]
pub(crate) enum OnPrompt {
    /// Stream `updates` (raw notification params) then stop with the reason.
    Stop(StopReason, Vec<Value>),
    /// Ask permission with the offered kinds, record the outcome, `end_turn`.
    AskPermission(Vec<PermissionOptionKind>),
    /// Answer the n-th prompt of the session with the n-th stop reason (the
    /// last one repeats): a work turn that ends normally followed by a nudge
    /// turn that does not.
    Sequence(Vec<StopReason>),
    /// Never answer: the test kills the agent while the prompt is pending.
    Hang,
}

#[derive(Clone)]
pub(crate) struct FakeCfg {
    pub(crate) system_prompt: bool,
    pub(crate) mcp_http: bool,
    pub(crate) on_prompt: OnPrompt,
}

impl FakeCfg {
    pub(crate) fn new(on_prompt: OnPrompt) -> Self {
        Self {
            system_prompt: true,
            mcp_http: false,
            on_prompt,
        }
    }
}

/// What the fake agent observed, for assertions.
#[derive(Default)]
pub(crate) struct Seen {
    pub(crate) new_session: Option<NewSessionRequest>,
    pub(crate) prompts: Vec<String>,
    pub(crate) permission_outcome: Option<RequestPermissionOutcome>,
    pub(crate) prompt_started: Option<tokio::sync::oneshot::Sender<()>>,
}

pub(crate) type SeenRef = Arc<Mutex<Seen>>;

pub(crate) fn text(t: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(t))
}

/// Spawn the fake agent and return the client-side transport plus the agent's
/// task handle (abort it to simulate the child dying).
pub(crate) fn spawn_fake_agent(
    cfg: FakeCfg,
    seen: SeenRef,
) -> (
    impl ConnectTo<Client> + 'static,
    tokio::task::JoinHandle<()>,
) {
    let (agent_w, client_r) = tokio::io::duplex(64 * 1024);
    let (client_w, agent_r) = tokio::io::duplex(64 * 1024);
    let cfg_init = cfg.clone();
    let seen_new = seen.clone();
    let seen_prompt = seen.clone();
    let on_prompt = cfg.on_prompt.clone();
    let agent = Agent
        .v1()
        .on_receive_request(
            async move |_req: InitializeRequest, responder, _cx| {
                let mut session = SessionCapabilities::new();
                if cfg_init.system_prompt {
                    session = session.system_prompt(SessionSystemPromptCapabilities::new());
                }
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(
                        AgentCapabilities::new()
                            .session_capabilities(session)
                            .mcp_capabilities(McpCapabilities::new().http(cfg_init.mcp_http)),
                    ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, _cx| {
                seen_new.lock().unwrap().new_session = Some(req);
                responder.respond(NewSessionResponse::new("sess-1"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, cx| {
                let first = match req.prompt.first() {
                    Some(ContentBlock::Text(t)) => t.text.clone(),
                    _ => String::new(),
                };
                {
                    let mut s = seen_prompt.lock().unwrap();
                    s.prompts.push(first);
                    if let Some(tx) = s.prompt_started.take() {
                        let _ = tx.send(());
                    }
                }
                match on_prompt.clone() {
                    OnPrompt::Stop(reason, updates) => {
                        for params in updates {
                            cx.send_notification(
                                UntypedMessage::new("session/update", params).unwrap(),
                            )?;
                        }
                        responder.respond(PromptResponse::new(reason))
                    }
                    OnPrompt::AskPermission(kinds) => {
                        let seen = seen_prompt.clone();
                        let cx2 = cx.clone();
                        // Spawned: awaiting the client's answer inside the
                        // handler would hold the dispatch loop it arrives on.
                        cx.spawn(async move {
                            let options = kinds
                                .iter()
                                .enumerate()
                                .map(|(i, k)| {
                                    PermissionOption::new(format!("opt-{i}"), format!("o{i}"), *k)
                                })
                                .collect();
                            let resp = cx2
                                .send_request(RequestPermissionRequest::new(
                                    "sess-1",
                                    ToolCallUpdate::new("tc-1", ToolCallUpdateFields::new()),
                                    options,
                                ))
                                .block_task()
                                .await?;
                            seen.lock().unwrap().permission_outcome = Some(resp.outcome);
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        })
                    }
                    OnPrompt::Sequence(reasons) => {
                        let n = seen_prompt.lock().unwrap().prompts.len().saturating_sub(1);
                        let reason = reasons
                            .get(n)
                            .or(reasons.last())
                            .cloned()
                            .unwrap_or(StopReason::EndTurn);
                        responder.respond(PromptResponse::new(reason))
                    }
                    OnPrompt::Hang => {
                        // Park forever; the test aborts the whole agent task.
                        std::future::pending::<()>().await;
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        );
    let task = tokio::spawn(async move {
        let _ = agent
            .connect_to(ByteStreams::new(agent_w.compat_write(), agent_r.compat()))
            .await;
    });
    (
        ByteStreams::new(client_w.compat_write(), client_r.compat()),
        task,
    )
}

/// A session config for the fake agent: briefing `BRIEFING`, a transcript under
/// `dir`, and a fixed permission policy.
pub(crate) fn base_cfg(dir: &std::path::Path, denies: bool) -> super::SessionConfig {
    super::SessionConfig {
        cwd: dir.to_path_buf(),
        briefing: Some("BRIEFING".to_string()),
        mcp: None,
        transcript: Some(dir.join("transcript.jsonl")),
        permission: Arc::new(move || denies),
        handshake_timeout: Duration::from_secs(20),
    }
}
