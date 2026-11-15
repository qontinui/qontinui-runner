//! Fake-agent suite for the ACP client lane (plan
//! `2026-11-13-acp-client-lane-for-acp-native-clis`, Phase 1).
//!
//! The agent is a real `Agent.v1()` connection over an in-memory duplex pair
//! (`ByteStreams`), so the client under test speaks genuine ACP JSON-RPC and the
//! only fake part is the model behind the agent. Coverage: handshake,
//! permission allow/deny, `systemPrompt` advertised vs not, `stopReason`
//! mapping, unknown-update preservation, and the child going away mid-prompt
//! (`transport_lost`).

use std::time::Duration;

use agent_client_protocol::v1::schema::{
    ContentChunk, PermissionOption, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, SessionNotification, SessionUpdate, StopReason, ToolCallUpdate,
    ToolCallUpdateFields,
};
use serde_json::{json, Value};

use super::client::{permission_response, start_acp_session_over};
use super::fake_agent::{base_cfg, spawn_fake_agent, text, FakeCfg, OnPrompt, SeenRef};
use super::*;

fn transcript_lines(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("transcript.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const BOUND: Duration = Duration::from_secs(20);

#[tokio::test]
async fn handshake_records_the_agent_minted_id_and_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, _agent) = spawn_fake_agent(
        FakeCfg {
            system_prompt: true,
            mcp_http: true,
            on_prompt: OnPrompt::Stop(StopReason::EndTurn, vec![]),
        },
        seen.clone(),
    );
    let session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .expect("handshake");
    let info = session.info();
    assert_eq!(info.agent_session_id, "sess-1", "the AGENT mints the id");
    assert!(info.caps.system_prompt);
    assert!(info.caps.mcp_http);
    assert!(!info.caps.load_session);
    let req = seen.lock().unwrap().new_session.clone().unwrap();
    assert_eq!(req.cwd, dir.path());
    session.close().await;
}

#[tokio::test]
async fn an_advertised_system_prompt_carries_the_briefing_and_leaves_the_prompt_alone() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, _agent) = spawn_fake_agent(
        FakeCfg::new(OnPrompt::Stop(StopReason::EndTurn, vec![])),
        seen.clone(),
    );
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    assert_eq!(
        session.info().briefing_delivery,
        BriefingDelivery::SystemPrompt
    );
    session.prompt("do the work", BOUND).await.unwrap();
    let s = seen.lock().unwrap();
    let sp = s
        .new_session
        .as_ref()
        .and_then(|r| r.system_prompt.as_ref())
        .expect("systemPrompt rides session/new");
    assert_eq!(sp.content, vec![text("BRIEFING")]);
    assert_eq!(s.prompts, vec!["do the work".to_string()]);
    drop(s);
    session.close().await;
}

#[tokio::test]
async fn an_agent_without_system_prompt_gets_the_briefing_prefixed_to_the_first_prompt_only() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, _agent) = spawn_fake_agent(
        FakeCfg {
            system_prompt: false,
            mcp_http: false,
            on_prompt: OnPrompt::Stop(StopReason::EndTurn, vec![]),
        },
        seen.clone(),
    );
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    assert_eq!(
        session.info().briefing_delivery,
        BriefingDelivery::PromptPrefix
    );
    session.prompt("first", BOUND).await.unwrap();
    session.prompt("second", BOUND).await.unwrap();
    let s = seen.lock().unwrap();
    assert!(
        s.new_session.as_ref().unwrap().system_prompt.is_none(),
        "no systemPrompt may be sent to an agent that did not advertise it"
    );
    assert_eq!(s.prompts[0], "BRIEFING\n\nfirst");
    assert_eq!(s.prompts[1], "second", "the prefix is owed once");
    drop(s);
    session.close().await;
}

#[tokio::test]
async fn the_mcp_chooser_sees_the_advertised_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, _agent) = spawn_fake_agent(
        FakeCfg {
            system_prompt: true,
            mcp_http: false,
            on_prompt: OnPrompt::Stop(StopReason::EndTurn, vec![]),
        },
        seen.clone(),
    );
    let mut cfg = base_cfg(dir.path(), false);
    cfg.mcp = Some(Box::new(|caps| {
        // Stdio is mandatory; HTTP only when advertised.
        assert!(!caps.mcp_http);
        Some(McpEntry::Stdio {
            name: "coord-mcp".into(),
            command: "/usr/bin/python3".into(),
            args: vec!["shim.py".into(), "--credential".into(), "/c".into()],
            env: vec![],
        })
    }));
    let session = start_acp_session_over(transport, cfg).await.unwrap();
    assert!(session.info().mcp_delivered);
    let req = seen.lock().unwrap().new_session.clone().unwrap();
    assert_eq!(req.mcp_servers.len(), 1);
    session.close().await;
}

async fn permission_outcome_for(
    kinds: Vec<PermissionOptionKind>,
    denies: bool,
) -> Option<RequestPermissionOutcome> {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, _agent) =
        spawn_fake_agent(FakeCfg::new(OnPrompt::AskPermission(kinds)), seen.clone());
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), denies))
        .await
        .unwrap();
    let out = session.prompt("go", BOUND).await.unwrap();
    assert_eq!(out.stop_reason, AcpStopReason::EndTurn);
    session.close().await;
    let outcome = seen.lock().unwrap().permission_outcome.clone();
    outcome
}

fn selected_id(o: &RequestPermissionOutcome) -> String {
    match o {
        RequestPermissionOutcome::Selected(s) => s.option_id.to_string(),
        other => panic!("expected a selected option, got {other:?}"),
    }
}

#[tokio::test]
async fn a_permission_request_is_allowed_by_kind_preferring_allow_once() {
    // Order is deliberately hostile: the broad allow and a reject come first.
    let o = permission_outcome_for(
        vec![
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::AllowAlways,
            PermissionOptionKind::AllowOnce,
        ],
        false,
    )
    .await
    .expect("the agent saw an outcome");
    assert_eq!(selected_id(&o), "opt-2", "allow_once by kind, not position");
}

#[tokio::test]
async fn a_permission_request_is_denied_when_the_tenant_dial_denies() {
    let o = permission_outcome_for(
        vec![
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::RejectAlways,
            PermissionOptionKind::RejectOnce,
        ],
        true,
    )
    .await
    .expect("the agent saw an outcome");
    assert_eq!(
        selected_id(&o),
        "opt-2",
        "reject_once by kind, not position"
    );
}

#[tokio::test]
async fn a_request_with_no_matching_option_is_cancelled_never_guessed() {
    let o = permission_outcome_for(vec![PermissionOptionKind::AllowOnce], true)
        .await
        .expect("the agent saw an outcome");
    assert!(
        matches!(o, RequestPermissionOutcome::Cancelled),
        "a deny with only allow options on offer must cancel, got {o:?}"
    );
}

#[test]
fn permission_response_is_pure_over_the_policy() {
    let req = RequestPermissionRequest::new(
        "s",
        ToolCallUpdate::new("t", ToolCallUpdateFields::new()),
        vec![
            PermissionOption::new("a", "allow", PermissionOptionKind::AllowAlways),
            PermissionOption::new("r", "reject", PermissionOptionKind::RejectAlways),
        ],
    );
    assert_eq!(
        selected_id(&permission_response(&req, false).outcome),
        "a",
        "falls back to allow_always when no allow_once is offered"
    );
    assert_eq!(selected_id(&permission_response(&req, true).outcome), "r");
}

#[tokio::test]
async fn every_stop_reason_maps_to_its_own_token() {
    for (wire, token) in [
        (StopReason::EndTurn, "end_turn"),
        (StopReason::MaxTokens, "max_tokens"),
        (StopReason::MaxTurnRequests, "max_turn_requests"),
        (StopReason::Refusal, "refusal"),
        (StopReason::Cancelled, "cancelled"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (transport, _agent) = spawn_fake_agent(
            FakeCfg::new(OnPrompt::Stop(wire, vec![])),
            SeenRef::default(),
        );
        let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
            .await
            .unwrap();
        let out = session.prompt("go", BOUND).await.unwrap();
        assert_eq!(out.stop_reason.as_str(), token);
        session.close().await;
    }
}

#[tokio::test]
async fn an_unknown_update_is_preserved_verbatim_and_known_ones_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let unknown = json!({
        "sessionId": "sess-1",
        "update": {"sessionUpdate": "quantum_flux", "payload": {"a": [1, 2]}}
    });
    let known = serde_json::to_value(SessionNotification::new(
        "sess-1",
        SessionUpdate::AgentMessageChunk(ContentChunk::new(text("hello"))),
    ))
    .unwrap();
    let usage = json!({
        "sessionId": "sess-1",
        "update": {"sessionUpdate": "usage_update", "used": 10, "size": 100}
    });
    let (transport, _agent) = spawn_fake_agent(
        FakeCfg::new(OnPrompt::Stop(
            StopReason::EndTurn,
            vec![known, unknown.clone(), usage],
        )),
        SeenRef::default(),
    );
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    let out = session.prompt("go", BOUND).await.unwrap();
    assert_eq!(out.update_count, 3);
    assert_eq!(out.unknown_updates, 1);
    assert_eq!(
        out.last_usage,
        Some(AcpUsage {
            used: 10,
            size: 100,
            cost: None
        })
    );
    session.close().await;
    let lines = transcript_lines(dir.path());
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["type"], "acp_update");
    assert_eq!(lines[0]["kind"], "agent_message_chunk");
    assert_eq!(lines[1]["type"], "unknown_update");
    assert_eq!(lines[1]["raw"], unknown, "preserved verbatim, not dropped");
    assert_eq!(lines[2]["kind"], "usage_update");
}

#[tokio::test]
async fn the_agent_dying_mid_prompt_is_transport_lost() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    seen.lock().unwrap().prompt_started = Some(started_tx);
    let (transport, agent) = spawn_fake_agent(FakeCfg::new(OnPrompt::Hang), seen.clone());
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    let killer = tokio::spawn(async move {
        started_rx.await.unwrap();
        agent.abort();
    });
    let err = session
        .prompt("go", BOUND)
        .await
        .expect_err("a dead agent cannot answer");
    killer.await.unwrap();
    assert_eq!(err.kind, AcpErrorKind::TransportLost, "got {err}");
    assert_eq!(err.token(), "transport_lost");
    // `close` on a lost session must still return (and reap).
    session.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_turn_past_its_bound_is_cancelled_then_reported_timed_out() {
    let dir = tempfile::tempdir().unwrap();
    // The agent never answers the prompt and ignores the cancel. The clock is
    // paused, so the runtime auto-advances through the bound and the grace.
    let (transport, _agent) = spawn_fake_agent(FakeCfg::new(OnPrompt::Hang), SeenRef::default());
    let mut session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    let err = session
        .prompt("go", Duration::from_secs(1))
        .await
        .expect_err("must time out");
    assert_eq!(err.kind, AcpErrorKind::TimedOut, "got {err}");
}

// ---- confined filesystem + child env parity (no agent needed) -------------

#[test]
fn fs_access_is_confined_to_the_session_cwd() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let inside_file = root.path().join("a.txt");
    std::fs::write(&inside_file, "l1\nl2\nl3\n").unwrap();
    std::fs::write(outside.path().join("secret.txt"), "nope").unwrap();

    let read = |p: &std::path::Path, line, limit| {
        let mut r = agent_client_protocol::v1::schema::ReadTextFileRequest::new("s", p);
        r.line = line;
        r.limit = limit;
        super::client::read_confined(root.path(), &r)
    };
    assert_eq!(read(&inside_file, None, None).unwrap(), "l1\nl2\nl3\n");
    assert_eq!(read(&inside_file, Some(2), Some(1)).unwrap(), "l2\n");
    assert!(read(&outside.path().join("secret.txt"), None, None).is_err());
    // `..` traversal out of the root is resolved before the containment test.
    let traversal = root
        .path()
        .join("..")
        .join(outside.path().file_name().unwrap())
        .join("secret.txt");
    assert!(read(&traversal, None, None).is_err());
    // A relative path is refused.
    assert!(read(std::path::Path::new("a.txt"), None, None).is_err());

    let write = |p: &std::path::Path| {
        let r = agent_client_protocol::v1::schema::WriteTextFileRequest::new("s", p, "data");
        super::client::write_confined(root.path(), &r)
    };
    let new_file = root.path().join("new.txt");
    write(&new_file).unwrap();
    assert_eq!(std::fs::read_to_string(&new_file).unwrap(), "data");
    assert!(write(&outside.path().join("w.txt")).is_err());
    assert!(!outside.path().join("w.txt").exists());
}

#[test]
fn the_agent_child_env_is_finalized_exactly_like_the_claude_headless_path() {
    let launch = AcpLaunch {
        program: "dummy".into(),
        args: vec!["--acp".into()],
        env: vec![("QONTINUI_GATE_ID".into(), "gate-1".into())],
        cwd: std::env::temp_dir(),
    };
    // Plant every credential variable the scrub is meant to remove, the way the
    // runner's inherited process env would supply them, by way of `launch.env`.
    let mut launch_with_secrets = launch.clone();
    for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
        launch_with_secrets
            .env
            .push((name.to_string(), "hunter2".to_string()));
    }
    let cmd = super::client::build_acp_command(
        &launch_with_secrets,
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &crate::served_corpus::ServedCorpus::unknown("test"),
    );
    crate::terminal::assert_credentials_scrubbed_tokio(&cmd, "build_acp_command");
    crate::credential_helper::assert_non_interactive_git_posture_tokio(&cmd, "build_acp_command");
    let envs: Vec<(String, Option<String>)> = cmd
        .as_std()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.map(|v| v.to_string_lossy().to_string()),
            )
        })
        .collect();
    assert!(
        envs.iter()
            .any(|(k, v)| k == "QONTINUI_GATE_ID" && v.as_deref() == Some("gate-1")),
        "the gate env the caller set must reach the child"
    );
    assert!(
        envs.iter()
            .any(|(k, v)| k == "QONTINUI_RUNNER_CONTEXT" && v.is_some()),
        "the runner-context briefing env must reach the child"
    );
    assert_eq!(cmd.as_std().get_args().count(), 1);
}
