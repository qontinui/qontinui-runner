//! ACP client lane: drive an ACP-native agent CLI (Gemini CLI, OpenCode,
//! Codex via `codex-acp`, ...) headlessly through the Agent Client Protocol.
//!
//! Plan `2026-11-13-acp-client-lane-for-acp-native-clis`, Phase 1: the client
//! core only. Nothing in the runner calls this module yet — Phase 2 wires it
//! into the headless continuation arm (`HeadlessHarness::Acp` in
//! `agent_runtime.rs`). The bin crate's root allows dead code crate-wide, so
//! no module-level allow is needed meanwhile.
//!
//! - [`client`]: spawn the agent as our own child (credential scrub and git
//!   posture identical to the headless Claude child), `initialize`,
//!   `session/new`, `session/prompt`, `close`; the client-side handlers
//!   (permission policy, `fs/*` confined to the session cwd; `terminal/*` is
//!   not advertised).
//! - [`map`]: `session/update` → transcript lines and usage events; stop
//!   reasons.

pub(crate) mod client;
pub(crate) mod map;

pub(crate) use client::{
    build_acp_command, spawn_acp_agent, spawn_acp_agent_with_policy, tenant_permission_policy,
    AcpError, AcpLaunch, AcpSession, AcpSessionRequest, AgentAdvertised, BriefingDelivery,
    ChildExit, CloseOutcome, PermissionPolicy, PromptOutcome, StartedSession,
};
pub(crate) use map::{AcpCost, AcpStopReason, AcpUsage, PermissionDecision, TranscriptLine};

/// End-to-end tests against an in-process fake agent built with the crate's
/// own `Agent.v1()` builder, connected over an in-memory duplex.
#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::{
        AgentCapabilities, CloseSessionRequest, CloseSessionResponse, Implementation,
        InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
        PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse, ReadTextFileRequest,
        RequestPermissionOutcome, RequestPermissionRequest, SessionCapabilities,
        SessionCloseCapabilities, SessionSystemPromptCapabilities, StopReason, ToolCallUpdate,
        ToolCallUpdateFields, WriteTextFileRequest,
    };
    use agent_client_protocol::{Agent, ByteStreams, UntypedMessage};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    /// What the fake agent does when it receives `session/prompt`.
    #[derive(Clone)]
    enum Script {
        /// Send each notification params as a `session/update`, then answer
        /// with `stop` (a raw wire value).
        Reply {
            updates: Vec<Value>,
            stop: &'static str,
        },
        /// Ask the client for permission, then end the turn.
        AskPermission,
        /// Ask the client to read `path`, then end the turn.
        ReadFile(PathBuf),
        /// Ask the client to write `path`, then end the turn.
        WriteFile(PathBuf),
        /// Send one update, then signal the test to kill the transport, never
        /// answering the prompt.
        DieMidPrompt,
    }

    #[derive(Default)]
    struct Recorded {
        new_session: Option<NewSessionRequest>,
        prompts: Vec<String>,
        permission: Option<RequestPermissionOutcome>,
        client_reply: Option<Result<String, String>>,
        closed: bool,
    }

    struct FakeAgent {
        recorded: Arc<Mutex<Recorded>>,
        handle: tokio::task::JoinHandle<()>,
        die: Option<oneshot::Receiver<()>>,
    }

    struct Caps {
        system_prompt: bool,
        session_close: bool,
    }

    fn duplex() -> (
        ByteStreams<
            tokio_util::compat::Compat<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            tokio_util::compat::Compat<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        >,
        ByteStreams<
            tokio_util::compat::Compat<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
            tokio_util::compat::Compat<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        >,
    ) {
        let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (agent_read, agent_write) = tokio::io::split(agent_io);
        (
            ByteStreams::new(client_write.compat_write(), client_read.compat()),
            ByteStreams::new(agent_write.compat_write(), agent_read.compat()),
        )
    }

    fn prompt_text(request: &PromptRequest) -> String {
        request
            .prompt
            .iter()
            .filter_map(|block| match block {
                agent_client_protocol::v1::schema::ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn start_fake_agent(
        caps: Caps,
        script: Script,
    ) -> (
        FakeAgent,
        impl agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
    ) {
        let (client_side, agent_side) = duplex();
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let (die_tx, die_rx) = oneshot::channel::<()>();
        let die_tx = Arc::new(Mutex::new(Some(die_tx)));

        let rec_new = Arc::clone(&recorded);
        let rec_prompt = Arc::clone(&recorded);
        let rec_close = Arc::clone(&recorded);
        let builder = Agent
            .v1()
            .name("fake-agent")
            .on_receive_request(
                async move |request: InitializeRequest, responder, _cx| {
                    let mut session = SessionCapabilities::new();
                    if caps.system_prompt {
                        session = session.system_prompt(SessionSystemPromptCapabilities::new());
                    }
                    if caps.session_close {
                        session = session.close(SessionCloseCapabilities::new());
                    }
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(
                                AgentCapabilities::new().session_capabilities(session),
                            )
                            .agent_info(Implementation::new("fake-agent", "0.1.0")),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest, responder, _cx| {
                    rec_new.lock().unwrap().new_session = Some(request);
                    responder.respond(NewSessionResponse::new("agent-session-1"))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx| {
                    rec_prompt
                        .lock()
                        .unwrap()
                        .prompts
                        .push(prompt_text(&request));
                    let session_id = request.session_id.clone();
                    let recorded = Arc::clone(&rec_prompt);
                    let die_tx = Arc::clone(&die_tx);
                    let script = script.clone();
                    // Round trips to the client run in a spawned task: blocking
                    // the dispatch loop on them would deadlock it.
                    let task_cx = cx.clone();
                    cx.spawn(async move {
                        match script {
                            Script::Reply { updates, stop } => {
                                for update in updates {
                                    task_cx.send_notification(UntypedMessage::new(
                                        "session/update",
                                        json!({ "sessionId": session_id.0, "update": update }),
                                    )?)?;
                                }
                                let reason = serde_json::from_value::<StopReason>(json!(stop))
                                    .expect("typed fake agent sends only v1 stop reasons");
                                responder.respond(PromptResponse::new(reason))
                            }
                            Script::AskPermission => {
                                let request = RequestPermissionRequest::new(
                                    session_id,
                                    ToolCallUpdate::new(
                                        "t-1",
                                        ToolCallUpdateFields::new().title("rm -rf build"),
                                    ),
                                    vec![
                                        PermissionOption::new(
                                            "allow",
                                            "Allow",
                                            PermissionOptionKind::AllowOnce,
                                        ),
                                        PermissionOption::new(
                                            "reject",
                                            "Reject",
                                            PermissionOptionKind::RejectOnce,
                                        ),
                                    ],
                                );
                                let answer = task_cx.send_request(request).block_task().await?;
                                recorded.lock().unwrap().permission = Some(answer.outcome);
                                responder.respond(PromptResponse::new(StopReason::EndTurn))
                            }
                            Script::ReadFile(path) => {
                                let reply = task_cx
                                    .send_request(ReadTextFileRequest::new(session_id, path))
                                    .block_task()
                                    .await
                                    .map(|r| r.content)
                                    .map_err(|e| e.to_string());
                                recorded.lock().unwrap().client_reply = Some(reply);
                                responder.respond(PromptResponse::new(StopReason::EndTurn))
                            }
                            Script::WriteFile(path) => {
                                let reply = task_cx
                                    .send_request(WriteTextFileRequest::new(
                                        session_id, path, "written",
                                    ))
                                    .block_task()
                                    .await
                                    .map(|_| String::new())
                                    .map_err(|e| e.to_string());
                                recorded.lock().unwrap().client_reply = Some(reply);
                                responder.respond(PromptResponse::new(StopReason::EndTurn))
                            }
                            Script::DieMidPrompt => {
                                task_cx.send_notification(UntypedMessage::new(
                                    "session/update",
                                    json!({
                                        "sessionId": session_id.0,
                                        "update": {
                                            "sessionUpdate": "agent_message_chunk",
                                            "content": { "type": "text", "text": "partial" }
                                        }
                                    }),
                                )?)?;
                                // Let the update reach the client before the
                                // transport goes away.
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                if let Some(tx) = die_tx.lock().unwrap().take() {
                                    let _ = tx.send(());
                                }
                                // Hold the responder: dropping it would answer.
                                let _held = responder;
                                std::future::pending::<()>().await;
                                Ok(())
                            }
                        }
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: CloseSessionRequest, responder, _cx| {
                    rec_close.lock().unwrap().closed = true;
                    responder.respond(CloseSessionResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            );
        let handle = tokio::spawn(async move {
            let _ = builder.connect_to(agent_side).await;
        });
        (
            FakeAgent {
                recorded,
                handle,
                die: Some(die_rx),
            },
            client_side,
        )
    }

    fn policy(denies: bool) -> PermissionPolicy {
        Arc::new(move || denies)
    }

    async fn connected(
        caps: Caps,
        script: Script,
        cwd: &Path,
        denies: bool,
    ) -> (FakeAgent, AcpSession) {
        let (agent, transport) = start_fake_agent(caps, script);
        let session = AcpSession::connect(transport, cwd, policy(denies))
            .await
            .expect("initialize against the fake agent");
        (agent, session)
    }

    fn end_turn() -> Script {
        Script::Reply {
            updates: vec![],
            stop: "end_turn",
        }
    }

    fn plain() -> Caps {
        Caps {
            system_prompt: false,
            session_close: false,
        }
    }

    #[tokio::test]
    async fn acp_session_handshake_records_capabilities_and_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, mut session) = connected(
            Caps {
                system_prompt: true,
                session_close: true,
            },
            end_turn(),
            dir.path(),
            false,
        )
        .await;
        let advertised = session.advertised().clone();
        assert_eq!(advertised.protocol_version, 1);
        assert_eq!(advertised.agent_name.as_deref(), Some("fake-agent"));
        assert_eq!(advertised.agent_version.as_deref(), Some("0.1.0"));
        assert!(advertised.system_prompt);
        assert!(advertised.session_close);
        assert!(!advertised.mcp_http);
        assert!(!advertised.load_session);

        let started = session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        assert_eq!(started.agent_session_id, "agent-session-1");
        assert_eq!(started.briefing_delivery, BriefingDelivery::None);
        assert_eq!(session.agent_session_id(), Some("agent-session-1"));

        let recorded_cwd = agent
            .recorded
            .lock()
            .unwrap()
            .new_session
            .as_ref()
            .unwrap()
            .cwd
            .clone();
        assert_eq!(recorded_cwd, std::fs::canonicalize(dir.path()).unwrap());

        // A second session on the same connection is refused.
        assert!(matches!(
            session.new_session(AcpSessionRequest::default()).await,
            Err(AcpError::SessionExists)
        ));

        let outcome = session.close().await;
        assert!(outcome.child.is_none(), "in-process transport has no child");
        assert!(
            agent.recorded.lock().unwrap().closed,
            "session/close is sent when the agent advertises it"
        );
    }

    #[tokio::test]
    async fn acp_session_prompt_before_new_session_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (_agent, mut session) = connected(plain(), end_turn(), dir.path(), false).await;
        assert!(matches!(
            session.prompt("hi").await,
            Err(AcpError::NoSession)
        ));
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_system_prompt_sent_when_advertised() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, mut session) = connected(
            Caps {
                system_prompt: true,
                session_close: false,
            },
            end_turn(),
            dir.path(),
            false,
        )
        .await;
        let started = session
            .new_session(AcpSessionRequest {
                mcp_servers: vec![],
                briefing: Some("BRIEFING".into()),
            })
            .await
            .unwrap();
        assert_eq!(started.briefing_delivery, BriefingDelivery::SystemPrompt);
        session.prompt("do the work").await.unwrap();
        {
            let recorded = agent.recorded.lock().unwrap();
            let system_prompt = recorded
                .new_session
                .as_ref()
                .unwrap()
                .system_prompt
                .as_ref()
                .expect("systemPrompt sent");
            assert_eq!(
                serde_json::to_value(system_prompt).unwrap(),
                json!({ "mode": "append", "content": [{ "type": "text", "text": "BRIEFING" }] })
            );
            assert_eq!(recorded.prompts, vec!["do the work".to_string()]);
        }
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_briefing_prefixes_first_prompt_when_not_advertised() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, mut session) = connected(plain(), end_turn(), dir.path(), false).await;
        let started = session
            .new_session(AcpSessionRequest {
                mcp_servers: vec![],
                briefing: Some("BRIEFING".into()),
            })
            .await
            .unwrap();
        assert_eq!(started.briefing_delivery, BriefingDelivery::PromptPrefix);
        session.prompt("first").await.unwrap();
        session.prompt("second").await.unwrap();
        {
            let recorded = agent.recorded.lock().unwrap();
            assert!(
                recorded
                    .new_session
                    .as_ref()
                    .unwrap()
                    .system_prompt
                    .is_none(),
                "systemPrompt must not be sent to an agent that does not advertise it"
            );
            assert_eq!(
                recorded.prompts,
                vec!["BRIEFING\n\nfirst".to_string(), "second".to_string()]
            );
        }
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_http_mcp_refused_when_not_advertised() {
        use agent_client_protocol::v1::schema::{McpServer, McpServerHttp};
        let dir = tempfile::tempdir().unwrap();
        let (agent, mut session) = connected(plain(), end_turn(), dir.path(), false).await;
        let result = session
            .new_session(AcpSessionRequest {
                mcp_servers: vec![McpServer::Http(McpServerHttp::new(
                    "coord",
                    "http://127.0.0.1:9876/coord-mcp",
                ))],
                briefing: None,
            })
            .await;
        assert!(matches!(
            result,
            Err(AcpError::Unsupported("mcpCapabilities.http"))
        ));
        assert!(agent.recorded.lock().unwrap().new_session.is_none());
        session.close().await;
    }

    async fn run_permission(denies: bool) -> (Option<RequestPermissionOutcome>, PromptOutcome) {
        let dir = tempfile::tempdir().unwrap();
        let (agent, mut session) =
            connected(plain(), Script::AskPermission, dir.path(), denies).await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        let outcome = session.prompt("go").await.unwrap();
        session.close().await;
        let permission = agent.recorded.lock().unwrap().permission.clone();
        (permission, outcome)
    }

    #[tokio::test]
    async fn acp_session_permission_allowed_by_policy() {
        let (permission, outcome) = run_permission(false).await;
        match permission {
            Some(RequestPermissionOutcome::Selected(s)) => assert_eq!(&*s.option_id.0, "allow"),
            other => panic!("expected allow, got {other:?}"),
        }
        assert_eq!(outcome.stop_reason, AcpStopReason::EndTurn);
        assert!(outcome.updates.iter().any(|line| matches!(
            line,
            TranscriptLine::Permission { decision: PermissionDecision::Allow, option_id: Some(id), title: Some(title), .. }
                if id == "allow" && title == "rm -rf build"
        )));
    }

    #[tokio::test]
    async fn acp_session_permission_denied_by_policy() {
        let (permission, outcome) = run_permission(true).await;
        match permission {
            Some(RequestPermissionOutcome::Selected(s)) => assert_eq!(&*s.option_id.0, "reject"),
            other => panic!("expected reject, got {other:?}"),
        }
        assert!(outcome.updates.iter().any(|line| matches!(
            line,
            TranscriptLine::Permission {
                decision: PermissionDecision::Deny,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn acp_session_stop_reason_mapping_end_to_end() {
        for (wire, expected) in [
            ("end_turn", AcpStopReason::EndTurn),
            ("max_tokens", AcpStopReason::MaxTokens),
            ("max_turn_requests", AcpStopReason::MaxTurnRequests),
            ("refusal", AcpStopReason::Refusal),
            ("cancelled", AcpStopReason::Cancelled),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (_agent, mut session) = connected(
                plain(),
                Script::Reply {
                    updates: vec![],
                    stop: wire,
                },
                dir.path(),
                false,
            )
            .await;
            session
                .new_session(AcpSessionRequest::default())
                .await
                .unwrap();
            let outcome = session.prompt("go").await.unwrap();
            assert_eq!(outcome.stop_reason, expected, "wire stopReason {wire}");
            session.close().await;
        }
    }

    #[tokio::test]
    async fn acp_session_updates_mapped_and_unknown_update_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let unknown = json!({ "sessionUpdate": "hologram_update", "beam": [1, 2, 3] });
        let (_agent, mut session) = connected(
            plain(),
            Script::Reply {
                updates: vec![
                    json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": "hello" }
                    }),
                    unknown.clone(),
                    json!({ "sessionUpdate": "usage_update", "used": 10, "size": 100 }),
                ],
                stop: "end_turn",
            },
            dir.path(),
            false,
        )
        .await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        let outcome = session.prompt("go").await.unwrap();
        assert_eq!(outcome.updates.len(), 3, "{:?}", outcome.updates);
        assert!(matches!(
            &outcome.updates[0],
            TranscriptLine::AgentMessageChunk { text: Some(t), .. } if t == "hello"
        ));
        assert_eq!(
            outcome.updates[1],
            TranscriptLine::UnknownUpdate {
                method: "session/update".into(),
                raw: json!({ "sessionId": "agent-session-1", "update": unknown }),
            }
        );
        assert_eq!(
            outcome.usage,
            vec![AcpUsage {
                used: 10,
                size: 100,
                cost: None
            }]
        );
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_fs_read_inside_cwd_and_refused_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("notes.txt"), "inside").unwrap();

        let (agent, mut session) = connected(
            plain(),
            Script::ReadFile(root.join("notes.txt")),
            &root,
            false,
        )
        .await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        session.prompt("read").await.unwrap();
        assert_eq!(
            agent.recorded.lock().unwrap().client_reply,
            Some(Ok("inside".to_string()))
        );
        session.close().await;

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        let (agent, mut session) = connected(
            plain(),
            Script::ReadFile(outside.path().join("secret")),
            &root,
            false,
        )
        .await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        session.prompt("read").await.unwrap();
        assert!(matches!(
            agent.recorded.lock().unwrap().client_reply,
            Some(Err(_))
        ));
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_fs_write_confined_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (agent, mut session) = connected(
            plain(),
            Script::WriteFile(root.join("out/new.txt")),
            &root,
            false,
        )
        .await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        session.prompt("write").await.unwrap();
        assert_eq!(
            agent.recorded.lock().unwrap().client_reply,
            Some(Ok(String::new()))
        );
        assert_eq!(
            std::fs::read_to_string(root.join("out/new.txt")).unwrap(),
            "written"
        );
        session.close().await;

        let (agent, mut session) = connected(
            plain(),
            Script::WriteFile(root.join("../escape.txt")),
            &root,
            false,
        )
        .await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        session.prompt("write").await.unwrap();
        assert!(matches!(
            agent.recorded.lock().unwrap().client_reply,
            Some(Err(_))
        ));
        assert!(!root.parent().unwrap().join("escape.txt").exists());
        session.close().await;
    }

    #[tokio::test]
    async fn acp_session_transport_lost_mid_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let (mut agent, mut session) =
            connected(plain(), Script::DieMidPrompt, dir.path(), false).await;
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        let die = agent.die.take().unwrap();
        let handle = agent.handle;
        // The "child exit": once the agent has sent its partial update, its
        // whole side of the transport goes away.
        tokio::spawn(async move {
            let _ = die.await;
            handle.abort();
        });
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(30), session.prompt("go"))
                .await
                .expect("a lost transport must end the prompt, not hang it")
                .unwrap();
        assert_eq!(outcome.stop_reason, AcpStopReason::TransportLost);
        assert!(
            outcome.updates.iter().any(|line| matches!(
                line,
                TranscriptLine::AgentMessageChunk { text: Some(t), .. } if t == "partial"
            )),
            "updates received before the loss are kept: {:?}",
            outcome.updates
        );
        session.close().await;
    }

    /// A real child that exits during the handshake: the error names the exit
    /// code and the child's last stderr line, and the child is reaped.
    #[cfg(unix)]
    #[tokio::test]
    async fn acp_session_real_child_exit_during_initialize_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let launch = AcpLaunch {
            program: PathBuf::from("sh"),
            args: vec!["-c".into(), "echo 'not logged in' >&2; exit 3".into()],
            env: vec![],
            cwd: dir.path().to_path_buf(),
        };
        let result = spawn_acp_agent_with_policy(
            &launch,
            crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
            &crate::served_corpus::ServedCorpus::unknown("test"),
            policy(false),
        )
        .await;
        match result {
            Err(AcpError::AgentExited { during, exit }) => {
                assert_eq!(during, "initialize");
                assert_eq!(exit.code, Some(3));
                assert!(!exit.killed);
                assert_eq!(exit.stderr_tail, vec!["not logged in".to_string()]);
            }
            Err(other) => panic!("expected AgentExited, got {other}"),
            Ok(_) => panic!("a child that exits cannot complete initialize"),
        }
    }

    /// A raw JSON-RPC peer, for wire shapes the crate's typed `Agent` builder
    /// refuses to send: a `stopReason` outside ACP v1.
    async fn raw_agent(agent_io: tokio::io::DuplexStream, stop_reason: &'static str) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (read, mut write) = tokio::io::split(agent_io);
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            let result = match message["method"].as_str() {
                Some("initialize") => json!({ "protocolVersion": 1, "agentCapabilities": {} }),
                Some("session/new") => json!({ "sessionId": "raw-1" }),
                Some("session/prompt") => json!({ "stopReason": stop_reason }),
                _ => json!({}),
            };
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let mut bytes = serde_json::to_vec(&reply).unwrap();
            bytes.push(b'\n');
            if write.write_all(&bytes).await.is_err() {
                return;
            }
        }
    }

    #[tokio::test]
    async fn acp_session_unrecognised_stop_reason_is_kept_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(raw_agent(agent_io, "snoozed"));
        let (client_read, client_write) = tokio::io::split(client_io);
        let transport = ByteStreams::new(client_write.compat_write(), client_read.compat());
        let mut session = AcpSession::connect(transport, dir.path(), policy(false))
            .await
            .unwrap();
        session
            .new_session(AcpSessionRequest::default())
            .await
            .unwrap();
        let outcome = session.prompt("go").await.unwrap();
        assert_eq!(
            outcome.stop_reason,
            AcpStopReason::Unrecognised("snoozed".into())
        );
        assert_eq!(outcome.stop_reason.as_token(), "snoozed");
        session.close().await;
    }
}
