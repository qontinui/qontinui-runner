//! ACP dispatch-arm tests (plan `2026-11-13-acp-client-lane-for-acp-native-clis`,
//! Phases 2 and 3): the request parse and refusal, the selection and its child
//! env, skill inlining, the continuation verdict end to end against a fake
//! agent, and the payload binding.

use std::sync::Mutex;

use serde_json::json;

use super::super::GateContinuationPayload;
use super::*;
use crate::acp_session::fake_agent::{base_cfg, spawn_fake_agent, FakeCfg, OnPrompt, SeenRef};
use crate::acp_session::start_acp_session_over;
use agent_client_protocol::v1::schema::StopReason;

fn agent_settings(program: &str) -> AcpAgentSettings {
    AcpAgentSettings {
        program: program.to_string(),
        args: vec!["--experimental-acp".to_string()],
        env_account_var: None,
    }
}

fn agents(entries: &[(&str, &str)]) -> BTreeMap<String, AcpAgentSettings> {
    entries
        .iter()
        .map(|(id, program)| (id.to_string(), agent_settings(program)))
        .collect()
}

fn acp(id: &str) -> HarnessRequest {
    HarnessRequest::Acp(id.to_string())
}

// ---- request parsing -------------------------------------------------------

#[test]
fn parse_harness_reads_exactly_the_acp_shape() {
    assert_eq!(parse_harness(&json!(null)), None);
    assert_eq!(
        parse_harness(&json!({"acp": "gemini"})),
        Some(acp("gemini"))
    );
    assert_eq!(
        parse_harness(&json!({"acp": "  gemini "})),
        Some(acp("gemini")),
        "surrounding whitespace is not part of the id"
    );
}

#[test]
fn an_unreadable_harness_is_carried_never_dropped_to_claude() {
    for bad in [
        json!({"acp": ""}),
        json!({"acp": 7}),
        json!({"acp": "g", "extra": 1}),
        json!({"pi": "x"}),
        json!({}),
        json!("gemini"),
        json!([1]),
    ] {
        assert!(
            matches!(parse_harness(&bad), Some(HarnessRequest::Unreadable(_))),
            "{bad} must be Unreadable, not absent"
        );
    }
}

fn payload_json(extra: serde_json::Value) -> serde_json::Value {
    let mut v = json!({
        "target_device_id": uuid::Uuid::nil(),
        "initial_prompt": "run /dispatch-canary",
        "presentation": "headless",
        "source": "gate_continuation",
    });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

#[test]
fn the_continuation_payload_binds_the_frames_harness() {
    let p: GateContinuationPayload =
        serde_json::from_value(payload_json(json!({"harness": {"acp": "gemini"}}))).unwrap();
    assert_eq!(p.harness, Some(acp("gemini")));
    let p: GateContinuationPayload = serde_json::from_value(payload_json(json!({}))).unwrap();
    assert_eq!(p.harness, None, "an older coord sends no harness: Claude");
    let p: GateContinuationPayload =
        serde_json::from_value(payload_json(json!({"harness": null}))).unwrap();
    assert_eq!(p.harness, None);
    // An unreadable harness does NOT fail the frame (that would drop the
    // continuation) and is NOT read as absent (that would run Claude).
    let p: GateContinuationPayload =
        serde_json::from_value(payload_json(json!({"harness": {"acp": 3}}))).unwrap();
    assert!(matches!(p.harness, Some(HarnessRequest::Unreadable(_))));
}

#[test]
fn the_agent_spawn_payload_binds_the_harness_too() {
    let body = json!({
        "agent_id": uuid::Uuid::nil(),
        "target_device_id": uuid::Uuid::nil(),
        "worktrees": [],
        "initial_prompt": "go",
        "claim_token": "agent:x",
        "harness": {"acp": "opencode"},
    });
    let p: super::super::LaunchPayload = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(p.harness, Some(acp("opencode")));
    let mut without = body;
    without.as_object_mut().unwrap().remove("harness");
    let p: super::super::LaunchPayload = serde_json::from_value(without).unwrap();
    assert_eq!(p.harness, None);
}

// ---- refusal ---------------------------------------------------------------

fn resolve_all(program: &str) -> Option<PathBuf> {
    Some(PathBuf::from(format!("/resolved/{program}")))
}

#[test]
fn no_harness_means_no_acp_and_touches_nothing() {
    let r = resolve_harness_request(None, Presentation::Headless, &agents(&[]), |_| {
        panic!("a request with no harness must not resolve any program")
    });
    assert_eq!(r, Ok(None));
}

#[test]
fn an_unknown_agent_id_is_refused_with_the_typed_detail() {
    let err = resolve_harness_request(
        Some(&acp("ghost")),
        Presentation::Headless,
        &agents(&[("gemini", "gemini")]),
        resolve_all,
    )
    .unwrap_err();
    assert_eq!(err, HarnessRefusal::Absent("ghost".to_string()));
    assert_eq!(err.detail(), "acp_agent_absent:ghost");
}

#[test]
fn an_empty_acp_agents_setting_refuses_every_acp_request() {
    // Absent means no ACP agents; the request is refused, never a silent
    // Claude fallback.
    let err = resolve_harness_request(
        Some(&acp("gemini")),
        Presentation::Headless,
        &agents(&[]),
        resolve_all,
    )
    .unwrap_err();
    assert_eq!(err.detail(), "acp_agent_absent:gemini");
}

#[test]
fn an_acp_harness_on_a_terminal_presentation_is_refused() {
    let err = resolve_harness_request(
        Some(&acp("gemini")),
        Presentation::Terminal,
        &agents(&[("gemini", "gemini")]),
        resolve_all,
    )
    .unwrap_err();
    assert_eq!(err.detail(), "acp_requires_headless:gemini");
}

#[test]
fn a_configured_agent_whose_program_does_not_resolve_is_refused() {
    let err = resolve_harness_request(
        Some(&acp("gemini")),
        Presentation::Headless,
        &agents(&[("gemini", "gemini-gone")]),
        |_| None,
    )
    .unwrap_err();
    assert!(err.detail().starts_with("acp_agent_unlaunchable:gemini"));
}

#[test]
fn an_unreadable_harness_is_refused_by_name() {
    let req = HarnessRequest::Unreadable("harness must be an object".into());
    let err = resolve_harness_request(
        Some(&req),
        Presentation::Headless,
        &agents(&[("gemini", "gemini")]),
        resolve_all,
    )
    .unwrap_err();
    assert_eq!(
        err.detail(),
        "harness_unreadable: harness must be an object"
    );
}

#[test]
fn a_resolvable_agent_is_selected_with_its_launch_shape() {
    let mut a = agents(&[("gemini", "gemini")]);
    a.get_mut("gemini").unwrap().env_account_var = Some("GEMINI_HOME".into());
    let resolved = resolve_harness_request(
        Some(&acp("gemini")),
        Presentation::Headless,
        &a,
        resolve_all,
    )
    .unwrap()
    .expect("selected");
    assert_eq!(resolved.program, PathBuf::from("/resolved/gemini"));
    assert_eq!(resolved.args, vec!["--experimental-acp".to_string()]);
    assert_eq!(resolved.env_account_var.as_deref(), Some("GEMINI_HOME"));
    match select_headless_harness(Some(&resolved), "/w/repo", vec![("K".into(), "V".into())]) {
        HeadlessHarness::Acp(l) => {
            assert_eq!(l.cwd, PathBuf::from("/w/repo"));
            assert_eq!(l.args, resolved.args);
            assert_eq!(l.env, vec![("K".to_string(), "V".to_string())]);
        }
        HeadlessHarness::ClaudeCli => panic!("a resolved agent must select ACP"),
    }
    assert!(matches!(
        select_headless_harness(None, "/w", vec![]),
        HeadlessHarness::ClaudeCli
    ));
}

// ---- the child's env -------------------------------------------------------

fn child_envs(cmd: &tokio::process::Command) -> Vec<(String, Option<String>)> {
    cmd.as_std()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.map(|v| v.to_string_lossy().to_string()),
            )
        })
        .collect()
}

#[test]
fn the_gate_env_reaches_the_child_and_the_scrub_still_holds() {
    let gate = GateIdentity {
        gate_id: uuid::Uuid::from_u128(0x1111),
        consuming_device_id: uuid::Uuid::from_u128(0x2222),
    };
    let mut env = acp_child_env(
        Some(gate),
        Some(("GEMINI_HOME", "/accts/a")),
        vec![("GIT_AUTHOR_NAME".into(), "agent".into())],
    );
    // As the inherited process env would have supplied them.
    for name in crate::terminal::CREDENTIAL_VALUE_ENV_VARS {
        env.push((name.to_string(), "hunter2".to_string()));
    }
    let HeadlessHarness::Acp(launch) = select_headless_harness(
        Some(&ResolvedAcpAgent {
            id: "gemini".into(),
            program: "dummy".into(),
            args: vec![],
            env_account_var: Some("GEMINI_HOME".into()),
        }),
        &std::env::temp_dir().to_string_lossy(),
        env,
    ) else {
        panic!("expected ACP");
    };
    let cmd = crate::acp_session::build_acp_command(
        &launch,
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &crate::served_corpus::ServedCorpus::unknown("test"),
    );
    // Parity with the Claude arm: the same finalize step ran last.
    crate::terminal::assert_credentials_scrubbed_tokio(&cmd, "the ACP child");
    let envs = child_envs(&cmd);
    let get = |k: &str| {
        envs.iter()
            .find(|(n, _)| n == k)
            .and_then(|(_, v)| v.clone())
    };
    assert_eq!(get(GATE_ID_ENV), Some(gate.gate_id.to_string()));
    assert_eq!(
        get(GATE_DEVICE_ID_ENV),
        Some(gate.consuming_device_id.to_string()),
        "/dispatch-canary reads both variables"
    );
    assert_eq!(get("GEMINI_HOME").as_deref(), Some("/accts/a"));
    assert_eq!(get("GIT_AUTHOR_NAME").as_deref(), Some("agent"));
    assert!(get("QONTINUI_RUNNER_CONTEXT").is_some());
}

#[test]
fn no_gate_means_no_gate_env() {
    let env = acp_child_env(None, None, vec![]);
    assert!(env.is_empty(), "an absent variable is the 'no gate' signal");
}

// ---- skill inlining --------------------------------------------------------

fn corpus(skills: &[(&str, &str)], commands: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (name, body) in skills {
        let dir = d.path().join(".claude/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }
    for (name, body) in commands {
        let dir = d.path().join(".claude/commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
    }
    d
}

#[test]
fn a_skill_prompt_is_expanded_into_the_skill_body() {
    let d = corpus(
        &[(
            "dispatch-canary",
            "---\nname: dispatch-canary\ndescription: x\n---\n# Canary\nDo only this.\n",
        )],
        &[],
    );
    let SkillInlining::Inlined { skill, prompt } = inline_skill("run /dispatch-canary", d.path())
    else {
        panic!("expected the skill to be inlined");
    };
    assert_eq!(skill, "dispatch-canary");
    assert!(prompt.contains("# Canary\nDo only this."), "{prompt}");
    assert!(!prompt.contains("description: x"), "frontmatter is dropped");
    assert!(prompt.contains("<<< SKILL /dispatch-canary >>>"));
}

#[test]
fn arguments_substitute_and_the_trailing_brief_is_kept() {
    let d = corpus(&[("implement-plan", "Plan stem: $ARGUMENTS\n")], &[]);
    let SkillInlining::Inlined { prompt, .. } = inline_skill(
        "run /implement-plan my-plan-stem\n\n[coord brief] PR #12 is red",
        d.path(),
    ) else {
        panic!("expected inlined");
    };
    assert!(prompt.contains("Plan stem: my-plan-stem"), "{prompt}");
    assert!(prompt.contains("with arguments `my-plan-stem`"));
    assert!(
        prompt.ends_with("[coord brief] PR #12 is red"),
        "the brief rides after the skill: {prompt}"
    );
}

#[test]
fn a_command_file_is_the_fallback_to_a_skill() {
    let d = corpus(&[], &[("gate", "Gate command body")]);
    let SkillInlining::Inlined { prompt, .. } = inline_skill("run /gate", d.path()) else {
        panic!("expected the command body to be inlined");
    };
    assert!(prompt.contains("Gate command body"));
}

#[test]
fn a_missing_skill_and_a_non_skill_prompt_are_told_apart() {
    let d = corpus(&[], &[]);
    assert_eq!(
        inline_skill("run /nope", d.path()),
        SkillInlining::Missing {
            skill: "nope".into()
        }
    );
    assert_eq!(
        inline_skill("fix the build please", d.path()),
        SkillInlining::NotASkillPrompt
    );
    assert_eq!(
        inline_skill("run / nothing", d.path()),
        SkillInlining::NotASkillPrompt
    );
}

#[test]
fn a_skill_name_cannot_traverse_out_of_the_served_corpus() {
    let d = corpus(&[("a", "x")], &[]);
    std::fs::write(d.path().join("secret.md"), "SECRET").unwrap();
    // The name stops at the first character outside [A-Za-z0-9_-], so the
    // traversal never reaches the filesystem.
    match inline_skill("run /../../secret", d.path()) {
        SkillInlining::NotASkillPrompt => {}
        other => panic!("a traversing name must not resolve: {other:?}"),
    }
}

// ---- reading the gate ------------------------------------------------------

#[test]
fn the_gate_read_is_matched_by_id_and_unknown_is_never_a_default() {
    let g = uuid::Uuid::from_u128(7);
    let body = json!({"gates": [
        {"gate_id": uuid::Uuid::from_u128(8).to_string(), "continuation_consumed_outcome": "work_completed"},
        {"gate_id": g.to_string(), "continuation_consumed_outcome": "spawned"},
    ]});
    assert_eq!(
        parse_gate_read(&body, g),
        GateRead::Standing(Some("spawned".to_string()))
    );
    assert!(!parse_gate_read(&body, g).self_reported());
    // Another gate's outcome is never read as ours (a coord that ignores the
    // `gate_id` filter answers an unfiltered page).
    let other_only = json!({"gates": [
        {"gate_id": uuid::Uuid::from_u128(8).to_string(), "continuation_consumed_outcome": "work_completed"}
    ]});
    assert!(matches!(
        parse_gate_read(&other_only, g),
        GateRead::Unknown(_)
    ));
    assert!(matches!(
        parse_gate_read(&json!({}), g),
        GateRead::Unknown(_)
    ));
    // A null column is a real answer: nothing reported yet.
    let nulled =
        json!({"gates": [{"gate_id": g.to_string(), "continuation_consumed_outcome": null}]});
    assert_eq!(parse_gate_read(&nulled, g), GateRead::Standing(None));
    // The session's own outcomes count; the runner's own do not.
    for (rec, want) in [
        ("work_completed", true),
        ("work_abandoned", true),
        ("work_abandoned: gave up", true),
        ("work_unreported: exit=clean", false),
        ("spawned", false),
        ("spawn_failed", false),
    ] {
        assert_eq!(
            GateRead::Standing(Some(rec.to_string())).self_reported(),
            want,
            "{rec}"
        );
    }
}

// ---- the verdict, end to end against a fake agent --------------------------

/// A scripted gate: `reads` is the sequence of standing outcomes successive
/// reads return (the last one repeats), and every call is recorded.
struct FakeGate {
    reads: Mutex<Vec<GateRead>>,
    calls: Mutex<Vec<String>>,
}

impl FakeGate {
    fn new(reads: Vec<GateRead>) -> Self {
        Self {
            reads: Mutex::new(reads),
            calls: Mutex::new(vec![]),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl GateBackend for FakeGate {
    async fn post_spawned(&self) {
        self.calls.lock().unwrap().push("spawned".into());
    }
    async fn read_outcome(&self) -> GateRead {
        self.calls.lock().unwrap().push("read".into());
        let mut r = self.reads.lock().unwrap();
        if r.len() > 1 {
            r.remove(0)
        } else {
            r[0].clone()
        }
    }
    async fn post_work_unreported(&self, detail: String) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("work_unreported: {detail}"));
    }
}

fn nothing() -> GateRead {
    GateRead::Standing(Some("spawned".to_string()))
}
fn reported() -> GateRead {
    GateRead::Standing(Some("work_completed".to_string()))
}

const BOUNDS: TurnBounds = TurnBounds {
    work: Duration::from_secs(20),
    nudge: Duration::from_secs(20),
};

struct Run {
    verdict: AcpVerdict,
    gate: FakeGate,
    seen: SeenRef,
    agent_ended: bool,
}

async fn run_with(on_prompt: OnPrompt, reads: Vec<GateRead>, with_gate: bool) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (transport, agent) = spawn_fake_agent(FakeCfg::new(on_prompt), seen.clone());
    let session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    let gate = FakeGate::new(reads);
    let verdict = finish_session(
        session,
        with_gate.then_some(&gate as &dyn GateBackend),
        "run the thing",
        "gemini",
        BOUNDS,
    )
    .await;
    // `finish_session` closed the session, which drops the transport and ends
    // the agent's connection: the spawn -> prompt -> verdict -> close/reap path.
    let agent_ended = tokio::time::timeout(Duration::from_secs(10), agent)
        .await
        .is_ok();
    Run {
        verdict,
        gate,
        seen,
        agent_ended,
    }
}

#[tokio::test]
async fn a_session_that_reported_its_own_outcome_is_done_with_no_nudge() {
    let r = run_with(
        OnPrompt::Stop(StopReason::EndTurn, vec![]),
        vec![reported()],
        true,
    )
    .await;
    assert_eq!(r.verdict, AcpVerdict::Reported);
    assert_eq!(r.gate.calls(), vec!["spawned", "read"]);
    assert_eq!(r.seen.lock().unwrap().prompts.len(), 1, "no nudge sent");
    assert!(r.agent_ended, "the session was closed and the agent reaped");
}

#[tokio::test]
async fn end_turn_without_a_report_gets_exactly_one_nudge_then_settles_on_what_it_reads() {
    let r = run_with(
        OnPrompt::Stop(StopReason::EndTurn, vec![]),
        // first read: nothing; after the nudge: the session reported.
        vec![nothing(), reported()],
        true,
    )
    .await;
    assert_eq!(r.verdict, AcpVerdict::Reported);
    let s = r.seen.lock().unwrap();
    assert_eq!(s.prompts.len(), 2, "one work turn and ONE nudge");
    assert!(s.prompts[1].contains("without reporting your outcome"));
    assert_eq!(r.gate.calls(), vec!["spawned", "read", "read"]);
}

#[tokio::test]
async fn end_turn_that_never_reports_is_work_unreported_and_never_a_success() {
    let r = run_with(
        OnPrompt::Stop(StopReason::EndTurn, vec![]),
        vec![nothing()],
        true,
    )
    .await;
    let AcpVerdict::Unreported { detail, failed } = r.verdict else {
        panic!("the absence of a report must not read as success");
    };
    assert!(!failed, "a clean end that said nothing is not a failure");
    assert!(detail.starts_with("exit=clean code=0 after="), "{detail}");
    assert!(detail.contains("acp=gemini") && detail.contains("no_outcome_reported"));
    assert_eq!(r.seen.lock().unwrap().prompts.len(), 2, "exactly one nudge");
    let calls = r.gate.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("work_unreported"))
            .count(),
        1,
        "{calls:?}"
    );
    // The runner's vocabulary is spawned / spawn_failed / work_unreported only:
    // the backend has no way to write work_completed or work_abandoned, and the
    // detail says nothing of the kind.
    assert!(!calls.iter().any(|c| c.contains("work_completed")));
    assert!(r.agent_ended);
}

#[tokio::test]
async fn abnormal_stop_reasons_post_work_unreported_with_the_reason_and_no_nudge() {
    for (wire, token) in [
        (StopReason::Refusal, "refusal"),
        (StopReason::MaxTokens, "max_tokens"),
        (StopReason::MaxTurnRequests, "max_turn_requests"),
        (StopReason::Cancelled, "cancelled"),
    ] {
        let r = run_with(OnPrompt::Stop(wire, vec![]), vec![nothing()], true).await;
        let AcpVerdict::Unreported { detail, failed } = r.verdict else {
            panic!("{token} is not a success");
        };
        assert!(failed);
        assert_eq!(detail, format!("acp_stop_reason={token}"));
        assert_eq!(
            r.seen.lock().unwrap().prompts.len(),
            1,
            "no nudge on {token}"
        );
        assert_eq!(
            r.gate.calls(),
            // The gate is read BEFORE posting: a session that already
            // reported keeps its own answer (see the next test).
            vec![
                "spawned".to_string(),
                "read".to_string(),
                format!("work_unreported: {detail}")
            ]
        );
    }
}

#[tokio::test]
async fn a_session_that_reported_then_stopped_abnormally_keeps_its_own_report() {
    let r = run_with(
        OnPrompt::Stop(StopReason::MaxTokens, vec![]),
        vec![reported()],
        true,
    )
    .await;
    assert_eq!(r.verdict, AcpVerdict::Reported, "honour the self-report");
    assert_eq!(
        r.gate.calls(),
        vec!["spawned", "read"],
        "nothing posted over it"
    );
}

#[tokio::test]
async fn an_unreadable_gate_gets_no_nudge_and_a_work_unreported_with_the_reason() {
    let r = run_with(
        OnPrompt::Stop(StopReason::EndTurn, vec![]),
        vec![GateRead::Unknown("coord unreachable".to_string())],
        true,
    )
    .await;
    let AcpVerdict::Unreported { detail, failed } = r.verdict else {
        panic!("an unknown gate is not a success");
    };
    assert!(!failed);
    assert!(detail.contains("gate_outcome_unknown"), "{detail}");
    assert_eq!(
        r.seen.lock().unwrap().prompts.len(),
        1,
        "no nudge on a guess"
    );
    assert_eq!(r.gate.calls().len(), 3, "{:?}", r.gate.calls());
}

#[tokio::test]
async fn an_abnormal_stop_on_the_nudge_turn_is_reported_with_its_reason() {
    // The work turn ends normally without a report; the nudge turn then hits the
    // token cap. That is `work_unreported: acp_stop_reason=max_tokens`, not the
    // "no outcome reported" end.
    let r = run_with(
        OnPrompt::Sequence(vec![StopReason::EndTurn, StopReason::MaxTokens]),
        vec![nothing()],
        true,
    )
    .await;
    let AcpVerdict::Unreported { detail, failed } = r.verdict else {
        panic!("a capped nudge is not a success");
    };
    assert!(failed);
    assert_eq!(detail, "acp_stop_reason=max_tokens");
    assert_eq!(r.seen.lock().unwrap().prompts.len(), 2);
    assert!(NUDGE_PROMPT.contains("QONTINUI_GATE_ID") && NUDGE_PROMPT.contains("BY NAME"));
}

#[tokio::test]
async fn the_agent_dying_mid_run_is_a_crash_coord_may_redeliver() {
    let dir = tempfile::tempdir().unwrap();
    let seen = SeenRef::default();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    seen.lock().unwrap().prompt_started = Some(started_tx);
    let (transport, agent) = spawn_fake_agent(FakeCfg::new(OnPrompt::Hang), seen.clone());
    let session = start_acp_session_over(transport, base_cfg(dir.path(), false))
        .await
        .unwrap();
    tokio::spawn(async move {
        started_rx.await.unwrap();
        agent.abort();
    });
    let gate = FakeGate::new(vec![nothing()]);
    let v = finish_session(session, Some(&gate), "go", "gemini", BOUNDS).await;
    let AcpVerdict::Unreported { detail, failed } = v else {
        panic!("a dead agent is not a success");
    };
    assert!(failed);
    assert!(detail.starts_with("exit=crashed code=none"), "{detail}");
    assert!(detail.contains("transport_lost"));
    assert!(gate
        .calls()
        .iter()
        .any(|c| c.starts_with("work_unreported: exit=crashed")));
}

#[tokio::test]
async fn with_no_gate_a_clean_end_turn_is_done_and_anything_else_is_a_failure() {
    let ok = run_with(
        OnPrompt::Stop(StopReason::EndTurn, vec![]),
        vec![nothing()],
        false,
    )
    .await;
    assert_eq!(ok.verdict, AcpVerdict::Reported);
    assert_eq!(
        ok.seen.lock().unwrap().prompts.len(),
        1,
        "no gate, no nudge"
    );
    assert!(ok.gate.calls().is_empty(), "no gate, nothing posted");

    let bad = run_with(
        OnPrompt::Stop(StopReason::Refusal, vec![]),
        vec![nothing()],
        false,
    )
    .await;
    assert!(matches!(
        bad.verdict,
        AcpVerdict::Unreported { failed: true, .. }
    ));
    assert!(bad.gate.calls().is_empty());
}

#[test]
fn the_stop_reason_and_error_details_have_the_documented_grammar() {
    assert_eq!(
        stop_reason_detail(&AcpStopReason::MaxTokens),
        "acp_stop_reason=max_tokens"
    );
    let d = unreported_detail("crashed", "none", Duration::from_secs(12), "g", "reason=x");
    assert_eq!(d, "exit=crashed code=none after=12s acp=g reason=x");
    // Single line by construction: coord keeps the first line only.
    assert!(!d.contains('\n'));
}

// ---- a REAL child over stdio: spawn -> prompt -> close/reap ----------------

/// A scripted ACP agent in python (line-delimited JSON-RPC over stdio), so the
/// REAL spawn path — `build_acp_command`, the piped stdio, the handshake, the
/// close and the reap of an actual process — runs, not only the in-memory
/// transport. Skipped (loudly) where python3 is absent.
const PY_AGENT: &str = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    meth, mid = m.get("method"), m.get("id")
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {}}})
    elif meth == "session/new":
        send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "py-1"}})
    elif meth == "session/prompt":
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "py-1",
              "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}}}})
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
"#;

#[cfg(unix)]
#[tokio::test]
async fn a_real_child_runs_spawn_prompt_verdict_close_and_is_reaped() {
    use std::os::unix::fs::PermissionsExt;
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIPPED: python3 is not available to script the ACP child");
        return;
    }
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.py");
    std::fs::write(&script, format!("#!/usr/bin/env python3\n{PY_AGENT}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let launch = AcpLaunch {
        program: script,
        args: vec![],
        env: vec![],
        cwd: dir.path().to_path_buf(),
    };
    let served = crate::served_corpus::ServedCorpus::unknown("test");
    let session = crate::acp_session::spawn_acp_agent(
        launch,
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &served,
        base_cfg(dir.path(), false),
    )
    .await
    .expect("the real child completes the handshake");
    let pid = session.pid().expect("a live child has a pid");
    assert_eq!(session.info().agent_session_id, "py-1");
    // The agent advertised nothing: the briefing is prefixed, not a systemPrompt.
    assert_eq!(
        session.info().briefing_delivery,
        crate::acp_session::BriefingDelivery::PromptPrefix
    );
    let gate = FakeGate::new(vec![reported()]);
    let verdict = finish_session(session, Some(&gate), "go", "py", BOUNDS).await;
    assert_eq!(verdict, AcpVerdict::Reported);
    assert_eq!(gate.calls(), vec!["spawned", "read"]);
    // Reaped: the pid no longer names a live process (signal 0 probes it).
    let alive = process_is_alive(pid);
    assert!(!alive, "the agent child {pid} must be gone after close");
    let lines = std::fs::read_to_string(dir.path().join("transcript.jsonl")).unwrap();
    assert!(lines.contains("agent_message_chunk"), "{lines}");
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `kill -0 <pid>` through the shell avoids a libc dependency in a test.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---- reaping: escalation, drop, pre-handshake exit, handshake timeout -------

/// Tolerates a zombie: an exited-but-unreaped child still answers `kill -0`, and
/// what matters is that it no longer RUNS.
#[cfg(unix)]
fn process_runs(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => process_is_alive(pid),
    }
}

#[cfg(unix)]
async fn wait_gone(pid: u32) -> bool {
    for _ in 0..100 {
        if !process_runs(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[cfg(unix)]
fn py_launch(dir: &std::path::Path, body: &str) -> AcpLaunch {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("agent.py");
    std::fs::write(&script, format!("#!/usr/bin/env python3\n{body}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    AcpLaunch {
        program: script,
        args: vec![],
        env: vec![],
        cwd: dir.to_path_buf(),
    }
}

#[cfg(unix)]
fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

/// Speaks ACP, then on stdin EOF ignores it and sleeps: only a kill ends it.
#[cfg(unix)]
const PY_STUBBORN: &str = r#"
import sys, json, time
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    meth, mid = m.get("method"), m.get("id")
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {}}})
    elif meth == "session/new":
        send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "py-1"}})
while True:
    time.sleep(1000)
"#;

#[cfg(unix)]
#[tokio::test]
async fn close_escalates_to_a_kill_for_a_child_that_ignores_stdin_eof() {
    if !have_python() {
        eprintln!("SKIPPED: python3 is not available");
        return;
    }
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempfile::tempdir().unwrap();
    let served = crate::served_corpus::ServedCorpus::unknown("test");
    let session = crate::acp_session::spawn_acp_agent(
        py_launch(dir.path(), PY_STUBBORN),
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &served,
        base_cfg(dir.path(), false),
    )
    .await
    .expect("handshake");
    let pid = session.pid().unwrap();
    assert!(process_runs(pid));
    session.close().await;
    assert!(
        wait_gone(pid).await,
        "the stubborn child {pid} must be killed"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_a_session_without_close_still_reaps_the_child() {
    if !have_python() {
        eprintln!("SKIPPED: python3 is not available");
        return;
    }
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempfile::tempdir().unwrap();
    let served = crate::served_corpus::ServedCorpus::unknown("test");
    let session = crate::acp_session::spawn_acp_agent(
        py_launch(dir.path(), PY_STUBBORN),
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &served,
        base_cfg(dir.path(), false),
    )
    .await
    .expect("handshake");
    let pid = session.pid().unwrap();
    drop(session);
    assert!(wait_gone(pid).await, "a dropped session leaked child {pid}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_that_exits_before_the_handshake_is_reaped_and_reported() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let launch = AcpLaunch {
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), format!("echo $$ > {}", pidfile.display())],
        env: vec![],
        cwd: dir.path().to_path_buf(),
    };
    let served = crate::served_corpus::ServedCorpus::unknown("test");
    let err = match crate::acp_session::spawn_acp_agent(
        launch,
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &served,
        base_cfg(dir.path(), false),
    )
    .await
    {
        Ok(_) => panic!("a child that never speaks ACP is not a session"),
        Err(e) => e,
    };
    assert!(matches!(
        err.kind,
        AcpErrorKind::TransportLost | AcpErrorKind::Protocol
    ));
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_gone(pid).await,
        "the pre-handshake child {pid} must be reaped"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_that_never_answers_the_handshake_is_timed_out_and_killed() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let launch = AcpLaunch {
        program: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".into(),
            format!("echo $$ > {}; exec sleep 1000", pidfile.display()),
        ],
        env: vec![],
        cwd: dir.path().to_path_buf(),
    };
    let served = crate::served_corpus::ServedCorpus::unknown("test");
    let mut cfg = base_cfg(dir.path(), false);
    cfg.handshake_timeout = Duration::from_millis(500);
    let err = match crate::acp_session::spawn_acp_agent(
        launch,
        crate::coord_mcp::CoordMcpDelivery::Unprovisioned,
        &served,
        cfg,
    )
    .await
    {
        Ok(_) => panic!("a silent child is not a session"),
        Err(e) => e,
    };
    assert_eq!(err.kind, AcpErrorKind::TimedOut, "got {err}");
    let pid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_gone(pid).await,
        "the silent child {pid} must be killed"
    );
}
