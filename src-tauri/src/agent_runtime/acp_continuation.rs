//! The ACP arm of the headless gate continuation (plan
//! `2026-11-13-acp-client-lane-for-acp-native-clis`, Phases 2 and 3).
//!
//! A continuation that carries `harness: {"acp": "<id>"}` runs its headless arm
//! under an ACP-native agent CLI instead of `claude -p`. Everything the Claude
//! path gets from its Stop hook, its `--session-id` pin and its slash-command
//! resolution is rebuilt here from what ACP does carry:
//!
//! * **Request parsing** — [`HarnessRequest`], lenient in the one direction that
//!   cannot run the wrong harness: an unreadable `harness` is REFUSED, never
//!   dropped to Claude.
//! * **Refusal / selection** — [`resolve_harness_request`] (early, before any
//!   worktree) and [`select_headless_harness`] (late, once the workdir exists).
//! * **Skill delivery** — [`inline_skill`]: ACP has no `run /<skill>`, so the
//!   SKILL.md body from the served corpus is inlined into the first prompt.
//! * **Verdict** — [`drive_verdict`]: Design decision 1. The runner never writes
//!   `work_completed` / `work_abandoned`; it reads the gate's standing outcome,
//!   nudges ONCE if the session did not report, and otherwise posts only
//!   `work_unreported[: detail]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{
    first_line, headless_continuation_bound_port, is_session_work_outcome, post_spawn_outcome,
    post_work_unreported_fallback, remove_unspawned_continuation, report_spawn_complete,
    report_spawn_failed, report_spawn_failed_in_phase, AnchorReservation, ContinuationOutcome,
    HeadlessExit, HeadlessHarness, Presentation, SpawnPhase,
};
use crate::acp_session::{
    spawn_acp_agent, AcpError, AcpErrorKind, AcpLaunch, AcpSession, AcpStopReason,
};
use crate::commands::terminal::{GateIdentity, GATE_DEVICE_ID_ENV, GATE_ID_ENV};
use crate::settings::AcpAgentSettings;

/// Wall-clock bound of the work turn. Generous: a continuation is a whole
/// session's work. Past it the turn is cancelled and then reported timed out.
const WORK_TURN_BOUND: Duration = Duration::from_secs(4 * 60 * 60);

/// Wall-clock bound of the one "report your outcome" nudge turn.
const NUDGE_TURN_BOUND: Duration = Duration::from_secs(15 * 60);

/// The one bounded nudge sent when a turn ended without the session reporting
/// its outcome. It names the mechanism (the two env vars read BY NAME, the
/// device-keyed continuation route) rather than a skill, because the skill that
/// was running already says how.
const NUDGE_PROMPT: &str = "You stopped without reporting your outcome for this coord gate. \
Report it now, exactly as the skill you were running specifies: read QONTINUI_GATE_ID and \
QONTINUI_GATE_DEVICE_ID BY NAME (printenv, never an environment dump) and POST the outcome to \
coord's continuation-consumed route -- `work_completed` only if the work is done and verified, \
otherwise `work_abandoned` with a one-line detail saying what remains. Then stop. Do not start \
new work.";

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

/// The `harness` key of a continuation frame / agent-spawn payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HarnessRequest {
    /// `{"acp": "<agent id>"}`.
    Acp(String),
    /// A `harness` this build cannot read. Carried, not dropped: reading it as
    /// "no harness" would run Claude where an ACP agent was asked for.
    Unreadable(String),
}

/// Parse a `harness` JSON value. `null` is absence; every other shape that is
/// not exactly `{"acp": "<non-empty string>"}` is [`HarnessRequest::Unreadable`].
pub(crate) fn parse_harness(value: &Value) -> Option<HarnessRequest> {
    match value {
        Value::Null => None,
        Value::Object(map) => match map.get("acp") {
            Some(Value::String(id)) if !id.trim().is_empty() && map.len() == 1 => {
                Some(HarnessRequest::Acp(id.trim().to_string()))
            }
            Some(_) if map.len() == 1 => Some(HarnessRequest::Unreadable(
                "harness.acp must be a non-empty string".to_string(),
            )),
            _ => Some(HarnessRequest::Unreadable(format!(
                "unsupported harness keys: {}",
                map.keys().cloned().collect::<Vec<_>>().join(",")
            ))),
        },
        _ => Some(HarnessRequest::Unreadable(
            "harness must be an object".to_string(),
        )),
    }
}

/// serde adapter for the optional `harness` field: never fails the frame (a
/// frame the runner cannot parse is a dropped continuation), never drops an
/// unreadable value (see [`HarnessRequest::Unreadable`]).
pub(crate) fn deserialize_lenient_harness<'de, D>(d: D) -> Result<Option<HarnessRequest>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let value = Value::deserialize(d)?;
    Ok(parse_harness(&value))
}

// ---------------------------------------------------------------------------
// Refusal and selection
// ---------------------------------------------------------------------------

/// Why a requested harness cannot run on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HarnessRefusal {
    /// No `acp_agents` entry under this id.
    Absent(String),
    /// An entry exists but its program does not resolve to a launchable file.
    Unlaunchable { id: String, program: String },
    /// The `harness` value itself could not be read.
    Unreadable(String),
    /// An ACP harness asked for a terminal presentation.
    RequiresHeadless(String),
}

impl HarnessRefusal {
    /// The `spawn_failed` detail. `acp_agent_absent:<id>` is the plan's
    /// contract for an unknown id.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Absent(id) => format!("acp_agent_absent:{id}"),
            Self::Unlaunchable { id, program } => {
                format!("acp_agent_unlaunchable:{id} (program {program} does not resolve)")
            }
            Self::Unreadable(why) => format!("harness_unreadable: {why}"),
            Self::RequiresHeadless(id) => format!("acp_requires_headless:{id}"),
        }
    }
}

/// An `acp_agents` entry whose program resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedAcpAgent {
    pub id: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env_account_var: Option<String>,
}

/// Decide, from the request, the presentation and this device's `acp_agents`,
/// whether the harness can run. PURE over its inputs (the settings map and the
/// program resolver are injected) so every arm is unit-testable.
///
/// `Ok(None)` is "no harness asked for" — today's Claude path, byte for byte.
pub(crate) fn resolve_harness_request(
    req: Option<&HarnessRequest>,
    presentation: Presentation,
    agents: &BTreeMap<String, AcpAgentSettings>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Option<ResolvedAcpAgent>, HarnessRefusal> {
    let id = match req {
        None => return Ok(None),
        Some(HarnessRequest::Unreadable(why)) => {
            return Err(HarnessRefusal::Unreadable(why.clone()))
        }
        Some(HarnessRequest::Acp(id)) => id,
    };
    if presentation == Presentation::Terminal {
        return Err(HarnessRefusal::RequiresHeadless(id.clone()));
    }
    let Some(cfg) = agents.get(id) else {
        return Err(HarnessRefusal::Absent(id.clone()));
    };
    let Some(program) = resolve(&cfg.program) else {
        return Err(HarnessRefusal::Unlaunchable {
            id: id.clone(),
            program: cfg.program.clone(),
        });
    };
    Ok(Some(ResolvedAcpAgent {
        id: id.clone(),
        program,
        args: cfg.args.clone(),
        env_account_var: cfg.env_account_var.clone().filter(|v| !v.trim().is_empty()),
    }))
}

/// The env a continuation's ACP child gets BEFORE the runner's finalize step:
/// the autonomous git identity (commit attribution parity with the Claude
/// arm), the gate identity the session reports its outcome against
/// (`/dispatch-canary` reads both variables), and the per-agent account
/// variable. The gate pair travels together or not at all
/// ([`GateIdentity`]).
pub(crate) fn acp_child_env(
    gate: Option<GateIdentity>,
    account: Option<(&str, &str)>,
    git_identity: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut env = git_identity;
    if let Some(g) = gate {
        env.push((GATE_ID_ENV.to_string(), g.gate_id.to_string()));
        env.push((
            GATE_DEVICE_ID_ENV.to_string(),
            g.consuming_device_id.to_string(),
        ));
    }
    if let Some((var, dir)) = account {
        env.push((var.to_string(), dir.to_string()));
    }
    env
}

/// The launch env of `agent`'s child: [`acp_child_env`] over the live gate
/// identity, the autonomous git identity and, when the agent's settings name an
/// `env_account_var`, the config dir of the account the runner's selector picks
/// for this spawn (the same selection `claude -p` gets, exported under the
/// per-agent variable instead of `CLAUDE_CONFIG_DIR`).
pub(crate) async fn acp_launch_env(
    agent: &ResolvedAcpAgent,
    gate: Option<GateIdentity>,
) -> Vec<(String, String)> {
    let account = match &agent.env_account_var {
        None => None,
        Some(var) => {
            let _ = super::spawn_blocking_tracked(crate::ai_provider::pick_best_account).await;
            let ai = crate::settings::get_ai_settings();
            crate::ai_provider::get_effective_config_dir_with_override(&ai.claude_cli, None)
                .0
                .map(|dir| (var.clone(), dir))
        }
    };
    acp_child_env(
        gate,
        account.as_ref().map(|(v, d)| (v.as_str(), d.as_str())),
        super::agent_git_identity_env(),
    )
}

/// Pick the headless harness, once the workdir exists. `None` is Claude.
pub(crate) fn select_headless_harness(
    agent: Option<&ResolvedAcpAgent>,
    workdir: &str,
    env: Vec<(String, String)>,
) -> HeadlessHarness {
    match agent {
        None => HeadlessHarness::ClaudeCli,
        Some(a) => HeadlessHarness::Acp(AcpLaunch {
            program: a.program.clone(),
            args: a.args.clone(),
            env,
            cwd: PathBuf::from(workdir),
        }),
    }
}

// ---------------------------------------------------------------------------
// Skill delivery
// ---------------------------------------------------------------------------

/// What [`inline_skill`] found in a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkillInlining {
    /// The prompt is not a `run /<skill> ...` prompt: sent as is.
    NotASkillPrompt,
    /// The skill body was expanded into the prompt (`skill_delivery: inlined`).
    Inlined { skill: String, prompt: String },
    /// A `run /<skill>` prompt whose skill is in neither served directory.
    Missing { skill: String },
}

/// Expand `run /<skill> <args>` into the skill's own text. ACP has no slash
/// commands: an agent told `run /dispatch-canary` has nothing to run it with, so
/// the SKILL.md body from the served corpus rides the first prompt instead. Looks
/// in `<workdir>/.claude/skills/<skill>/SKILL.md`, then
/// `<workdir>/.claude/commands/<skill>.md`; the name is restricted to
/// `[A-Za-z0-9_-]` so it cannot traverse. Text after the first line (coord's
/// appended brief) is kept after the skill. `$ARGUMENTS` in the body is replaced
/// with the arguments, as Claude Code does for a command.
pub(crate) fn inline_skill(prompt: &str, workdir: &Path) -> SkillInlining {
    let (first, rest) = prompt.split_once('\n').unwrap_or((prompt, ""));
    let Some(after) = first.trim_start().strip_prefix("run /") else {
        return SkillInlining::NotASkillPrompt;
    };
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return SkillInlining::NotASkillPrompt;
    }
    let args = after
        .strip_prefix(name.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let claude = workdir.join(".claude");
    let body = [
        claude.join("skills").join(&name).join("SKILL.md"),
        claude.join("commands").join(format!("{name}.md")),
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok());
    let Some(body) = body else {
        return SkillInlining::Missing { skill: name };
    };
    let body = strip_frontmatter(&body).replace("$ARGUMENTS", &args);
    let mut out = format!(
        "You were asked to run the skill `/{name}`{}. This agent has no slash-command support, \
         so the skill's instructions follow verbatim between the markers; carry them out now, \
         exactly as written.\n\n<<< SKILL /{name} >>>\n{}\n<<< END SKILL /{name} >>>",
        if args.is_empty() {
            String::new()
        } else {
            format!(" with arguments `{args}`")
        },
        body.trim_end()
    );
    let rest = rest.trim_start_matches('\n');
    if !rest.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(rest);
    }
    SkillInlining::Inlined {
        skill: name,
        prompt: out,
    }
}

/// Drop a leading `---` YAML frontmatter block, if present.
fn strip_frontmatter(body: &str) -> &str {
    let Some(rest) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    else {
        return body;
    };
    match rest.find("\n---").and_then(|end| rest.get(end + 4..)) {
        Some(after) => after
            .strip_prefix('\r')
            .unwrap_or(after)
            .trim_start_matches('\n'),
        None => body,
    }
}

// ---------------------------------------------------------------------------
// The continuation verdict (Design decision 1)
// ---------------------------------------------------------------------------

/// What the gate's standing outcome reads right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateRead {
    /// coord answered: this is the recorded `continuation_consumed_outcome`
    /// (`None` = the column is still null).
    Standing(Option<String>),
    /// No answer. UNKNOWN is never read as "reported" nor as "not reported" by
    /// anything but the nudge, which is harmless either way: coord refuses the
    /// runner's `work_unreported` over a session's own `work_*`.
    Unknown(String),
}

impl GateRead {
    /// Whether the SESSION reported its own work outcome.
    fn self_reported(&self) -> bool {
        matches!(self, Self::Standing(Some(s)) if is_session_work_outcome(s))
    }
}

/// The two gate operations the verdict needs, behind a seam so the verdict is
/// tested without coord.
#[async_trait::async_trait]
pub(crate) trait GateBackend: Send + Sync {
    /// Post `spawned` (the session exists). Called once, BEFORE any turn:
    /// coord takes `spawned -> work_unreported` but refuses `spawned` over a
    /// `work_unreported` that raced ahead of it.
    async fn post_spawned(&self);
    async fn read_outcome(&self) -> GateRead;
    /// Post `work_unreported: <detail>`. Idempotent against a session's own
    /// report (coord refuses `work_* -> work_*`).
    async fn post_work_unreported(&self, detail: String);
}

/// The production backend over coord's device-keyed routes.
pub(crate) struct CoordGateBackend {
    pub gate: GateIdentity,
    pub label: String,
}

#[async_trait::async_trait]
impl GateBackend for CoordGateBackend {
    async fn post_spawned(&self) {
        post_spawn_outcome(
            self.gate.gate_id,
            self.gate.consuming_device_id,
            ContinuationOutcome::Spawned,
            None,
            None,
        )
        .await;
    }

    async fn read_outcome(&self) -> GateRead {
        read_gate_outcome(self.gate.gate_id).await
    }

    async fn post_work_unreported(&self, detail: String) {
        post_work_unreported_fallback(
            self.gate.gate_id,
            self.gate.consuming_device_id,
            &self.label,
            detail,
        )
        .await;
    }
}

/// `GET /coord/agent-gates?gate_id=<id>`: the device-JWT by-id read of one gate.
async fn read_gate_outcome(gate_id: uuid::Uuid) -> GateRead {
    let Some(base) = super::connected_coord_base() else {
        return GateRead::Unknown("no connected coord base".to_string());
    };
    let Some(client) = crate::coord_http::coord_client() else {
        return GateRead::Unknown("no shared coord client".to_string());
    };
    let url = format!("{base}/coord/agent-gates?gate_id={gate_id}&limit=1");
    // The gate row HAS an owning tenant that this call cannot name (the
    // continuation payload carries the gate id, not its tenant), so state
    // `Unresolved` rather than assert `Device`: a single-bound device presents
    // its only credential as before, a multi-bound one sends none, and the read
    // then degrades to UNKNOWN (`work_unreported`), never another tenant's view.
    let resp = match crate::auth::attach_device_auth_for(
        client.get(&url),
        crate::auth::TenantScope::Unresolved,
    )
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return GateRead::Unknown(format!("gate read failed: {e}")),
    };
    if !resp.status().is_success() {
        return GateRead::Unknown(format!("gate read answered {}", resp.status()));
    }
    match resp.json::<Value>().await {
        Ok(body) => parse_gate_read(&body, gate_id),
        Err(e) => GateRead::Unknown(format!("gate read body unreadable: {e}")),
    }
}

/// PURE: the standing outcome of `gate_id` in a `GET /coord/agent-gates` body.
/// The row is matched BY ID, so a coord that predates the `gate_id` filter and
/// answers an unfiltered page reads UNKNOWN rather than another gate's outcome.
pub(crate) fn parse_gate_read(body: &Value, gate_id: uuid::Uuid) -> GateRead {
    let Some(rows) = body.get("gates").and_then(Value::as_array) else {
        return GateRead::Unknown("gate read has no `gates` array".to_string());
    };
    let wanted = gate_id.to_string();
    match rows
        .iter()
        .find(|r| r.get("gate_id").and_then(Value::as_str) == Some(wanted.as_str()))
    {
        None => GateRead::Unknown("gate read page does not contain the gate".to_string()),
        Some(row) => GateRead::Standing(
            row.get("continuation_consumed_outcome")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
    }
}

/// How the ACP run ended, from the runner's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpVerdict {
    /// The run ended its turn and, where a gate exists, the SESSION reported its
    /// own outcome (or there is no gate to report to).
    Reported,
    /// The run ended without a session-reported outcome (or abnormally); the
    /// runner posted `work_unreported` with this detail.
    Unreported { detail: String, failed: bool },
}

/// Turn bounds, injectable so the timeout arm is testable.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnBounds {
    pub work: Duration,
    pub nudge: Duration,
}

impl Default for TurnBounds {
    fn default() -> Self {
        Self {
            work: WORK_TURN_BOUND,
            nudge: NUDGE_TURN_BOUND,
        }
    }
}

/// `exit=<class> code=<n|none> after=<secs>s acp=<id> <reason>`: the
/// `work_unreported` detail grammar of the PTY producers (class FIRST, which is
/// all coord's classifier reads), so an ACP end is classified by the same
/// rule: only `exit=crashed` is ever re-delivered.
fn unreported_detail(exit: &str, code: &str, after: Duration, id: &str, reason: &str) -> String {
    format!(
        "exit={exit} code={code} after={}s acp={id} {reason}",
        after.as_secs()
    )
}

/// The detail for an abnormal stop reason. Spelled as the plan fixes it
/// (`acp_stop_reason=<reason>`); it does not lead with `exit=`, so coord reads
/// the class as unknown and never re-delivers a refusal or a cap.
fn stop_reason_detail(reason: &AcpStopReason) -> String {
    format!("acp_stop_reason={}", reason.as_str())
}

/// Design decision 1, as code. Runs the work turn, then:
///
/// * `end_turn` and the session self-reported (`work_completed` /
///   `work_abandoned`) -> done;
/// * `end_turn` without a self-report -> ONE bounded nudge, re-read, and if it
///   still did not report, `work_unreported`;
/// * `refusal` / `max_tokens` / `max_turn_requests` / `cancelled` ->
///   `work_unreported: acp_stop_reason=<reason>`;
/// * a transport loss or protocol error -> `work_unreported` leading
///   `exit=crashed` (a dead agent is a crash coord may re-deliver).
///
/// Never writes `work_completed` / `work_abandoned`, and never reads success
/// from the absence of a signal. With no gate (a work-unit dispatch or an
/// agent-spawn) there is nothing to report to, so a clean `end_turn` is done and
/// every other end is a failure.
pub(crate) async fn drive_verdict(
    session: &mut AcpSession,
    gate: Option<&dyn GateBackend>,
    prompt: &str,
    acp_id: &str,
    bounds: TurnBounds,
) -> AcpVerdict {
    let started = std::time::Instant::now();
    /// Post the detail (when there is a gate) and settle as a failed end.
    async fn unreported(gate: Option<&dyn GateBackend>, detail: String) -> AcpVerdict {
        if let Some(g) = gate {
            g.post_work_unreported(detail.clone()).await;
        }
        AcpVerdict::Unreported {
            detail,
            failed: true,
        }
    }
    let error_detail = |e: &AcpError, started: std::time::Instant| {
        // A dead agent is a crash coord may re-deliver; every other lane error
        // is a deliberate-looking end it must not.
        let class = if e.kind == AcpErrorKind::TransportLost {
            "crashed"
        } else {
            "unknown"
        };
        unreported_detail(
            class,
            "none",
            started.elapsed(),
            acp_id,
            &format!("reason={}", e.token()),
        )
    };

    /// A turn ended abnormally. If the SESSION already reported its own outcome
    /// (it can report and then be cut off or hit a cap), that report stands and
    /// nothing is posted over it; otherwise post the detail.
    async fn settle_abnormal(gate: Option<&dyn GateBackend>, detail: String) -> AcpVerdict {
        if let Some(g) = gate {
            if g.read_outcome().await.self_reported() {
                return AcpVerdict::Reported;
            }
        }
        unreported(gate, detail).await
    }
    /// The gate could not be read, so "reported" and "not reported" are both
    /// unknown: do not nudge on a guess. Post `work_unreported` (coord refuses it
    /// over a session's own `work_*`, so it is safe either way) and say why.
    async fn settle_unknown(
        gate: &dyn GateBackend,
        started: std::time::Instant,
        acp_id: &str,
        why: &str,
    ) -> AcpVerdict {
        let detail = unreported_detail(
            "clean",
            "0",
            started.elapsed(),
            acp_id,
            &format!("reason=gate_outcome_unknown {}", first_line(why)),
        );
        gate.post_work_unreported(detail.clone()).await;
        AcpVerdict::Unreported {
            detail,
            failed: false,
        }
    }

    let out = match session.prompt(prompt, bounds.work).await {
        Ok(o) => o,
        Err(e) => return unreported(gate, error_detail(&e, started)).await,
    };
    if out.stop_reason != AcpStopReason::EndTurn {
        return settle_abnormal(gate, stop_reason_detail(&out.stop_reason)).await;
    }
    let Some(gate) = gate else {
        return AcpVerdict::Reported;
    };
    match gate.read_outcome().await {
        r if r.self_reported() => return AcpVerdict::Reported,
        GateRead::Unknown(why) => return settle_unknown(gate, started, acp_id, &why).await,
        GateRead::Standing(_) => {}
    }
    // One bounded nudge, then settle.
    let nudge = match session.prompt(NUDGE_PROMPT, bounds.nudge).await {
        Ok(o) => o,
        Err(e) => return unreported(Some(gate), error_detail(&e, started)).await,
    };
    if nudge.stop_reason != AcpStopReason::EndTurn {
        return settle_abnormal(Some(gate), stop_reason_detail(&nudge.stop_reason)).await;
    }
    match gate.read_outcome().await {
        r if r.self_reported() => return AcpVerdict::Reported,
        GateRead::Unknown(why) => return settle_unknown(gate, started, acp_id, &why).await,
        GateRead::Standing(_) => {}
    }
    let detail = unreported_detail(
        "clean",
        "0",
        started.elapsed(),
        acp_id,
        "reason=no_outcome_reported nudged=1",
    );
    gate.post_work_unreported(detail.clone()).await;
    AcpVerdict::Unreported {
        detail,
        failed: false,
    }
}

// ---------------------------------------------------------------------------
// The arm itself
// ---------------------------------------------------------------------------

/// The post-spawn half of the arm, over an already-open session: post `spawned`
/// (when there is a gate), drive the verdict, then close the session and reap
/// the agent. Always closes — whatever the verdict, the session is ended and
/// its child reaped, because an ACP agent does not exit at `end_turn`.
pub(crate) async fn finish_session(
    mut session: AcpSession,
    gate: Option<&dyn GateBackend>,
    prompt: &str,
    acp_id: &str,
    bounds: TurnBounds,
) -> AcpVerdict {
    if let Some(g) = gate {
        g.post_spawned().await;
    }
    let verdict = drive_verdict(&mut session, gate, prompt, acp_id, bounds).await;
    session.close().await;
    verdict
}

/// Everything the ACP arm needs beyond what `run_gate_continuation_inner` holds
/// in locals.
pub(crate) struct AcpArm {
    pub agent_id: uuid::Uuid,
    pub workdir: String,
    /// The composed prompt (`run /<skill> ...` plus coord's brief).
    pub prompt: String,
    pub acp_id: String,
    pub launch: AcpLaunch,
    /// `Some` only for a real gate continuation (not a work-unit dispatch).
    pub gate: Option<GateIdentity>,
}

/// Run one continuation under an ACP agent: spawn, post `spawned`, drive the
/// verdict, close and reap, post the lifecycle result.
///
/// There is no operator-stop path here because the Claude headless arm it
/// mirrors has none (`run_continuation_headless` runs its child to exit); the
/// agent-spawn door, which does have a stop token, sends `session/cancel` before
/// closing ([`run_agent_spawn_acp`]).
///
/// `Err` means NOTHING ran (the caller posts `spawn_failed`); once the session
/// exists every outcome is `Ok` — `spawned` has been posted by this arm, and a
/// bad end is recorded as `work_unreported`, which is what it is.
pub(crate) async fn run_continuation_acp(
    arm: AcpArm,
    reservation: AnchorReservation,
    ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
) -> anyhow::Result<HeadlessExit> {
    let AcpArm {
        agent_id,
        workdir,
        prompt,
        acp_id,
        launch,
        gate,
    } = arm;

    // Pre-session refusals and spawn failures share one tail: the tree no child
    // ever ran in is removed, and the one lifecycle report is posted.
    macro_rules! fail_before_session {
        ($reason:expr, $child_started:expr) => {{
            let reason: String = $reason;
            remove_unspawned_continuation(ctx, $child_started, &reason).await;
            report_spawn_failed_in_phase(agent_id, &reason, None, 0, None, SpawnPhase::Exited)
                .await;
            return Err(anyhow::anyhow!(reason));
        }};
    }

    // The same provisioning the terminal arm does, so the served corpus the
    // skill is inlined from exists in this workdir.
    crate::fleet_commands::provision_fleet_commands_for_session(&workdir);
    crate::fleet_skills::provision_fleet_skills_for_session(&workdir);

    let prompt = match inline_skill(&prompt, Path::new(&workdir)) {
        SkillInlining::NotASkillPrompt => prompt,
        SkillInlining::Inlined { skill, prompt } => {
            tracing::info!(
                "agent_runtime: acp continuation agent_id={agent_id} skill_delivery=inlined \
                 skill={skill}"
            );
            prompt
        }
        SkillInlining::Missing { skill } => {
            fail_before_session!(format!("acp_skill_unavailable:{skill}"), false)
        }
    };

    let bound_port = headless_continuation_bound_port(crate::coord_mcp::resolve_bound_api_port);
    let coord_mcp = crate::coord_mcp::provision_coord_mcp_for_session(&workdir, bound_port, None);
    let served = crate::served_corpus::probe_async(&workdir).await;
    let briefing =
        crate::terminal::runner_context(crate::terminal::spawn_seam_api_port(), coord_mcp, &served);
    let mcp_workdir = workdir.clone();
    let cfg = crate::acp_session::SessionConfig {
        cwd: PathBuf::from(&workdir),
        briefing: Some(briefing),
        mcp: Some(Box::new(move |caps| {
            crate::coord_mcp::acp_mcp_entry(
                &mcp_workdir,
                caps.mcp_http,
                crate::coord_mcp::ProxyConfigIdentity::device(&mcp_workdir, None),
            )
        })),
        transcript: super::agent_logs_root().map(|d| d.join(format!("{agent_id}.acp.jsonl"))),
        permission: std::sync::Arc::new(crate::session::permission_accept::stream_json_denies),
        handshake_timeout: crate::acp_session::HANDSHAKE_TIMEOUT,
    };

    let mut session = match spawn_acp_agent(launch, coord_mcp, &served, cfg).await {
        Ok(s) => s,
        Err(e) => {
            let started = !matches!(e.kind, AcpErrorKind::Spawn);
            fail_before_session!(e.to_string(), started)
        }
    };
    // The agent exists and nothing will register it in the live registry: give
    // the anchor back now, as the Claude headless arm does. The worktree
    // context (claim heartbeat) stays bound until this fn returns, matching the
    // Claude arm's heartbeat-then-release lifecycle.
    reservation.release();
    let _worktree_context = ctx;
    let info = session.info().clone();
    tracing::info!(
        "agent_runtime: acp continuation agent_id={agent_id} acp={acp_id} \
         agent_session_id={} briefing_delivery={} mcp_delivered={} identity=read_back",
        info.agent_session_id,
        info.briefing_delivery.as_str(),
        info.mcp_delivered
    );
    let pid = session.pid().map(i64::from);
    report_spawn_complete(
        agent_id,
        pid,
        Some(&format!("gate continuation (acp:{acp_id})")),
        None,
    )
    .await;
    // `spawned` is posted at SPAWN, not at exit (see `finish_session`).
    let backend = gate.map(|g| CoordGateBackend {
        gate: g,
        label: format!("acp:{acp_id}"),
    });
    let verdict = finish_session(
        session,
        backend.as_ref().map(|b| b as &dyn GateBackend),
        &prompt,
        &acp_id,
        TurnBounds::default(),
    )
    .await;

    match verdict {
        AcpVerdict::Reported => {
            tracing::info!(
                "agent_runtime: acp continuation agent_id={agent_id} acp={acp_id} ended cleanly"
            );
        }
        AcpVerdict::Unreported { detail, failed } => {
            if failed {
                report_spawn_failed(agent_id, &first_line(&detail), None, 0, None).await;
            }
        }
    }
    Ok(HeadlessExit::Acp)
}

// ---------------------------------------------------------------------------
// The agent-spawn door (`/agents/spawn` -> `events.agent.spawn_requested`)
// ---------------------------------------------------------------------------

/// How an ACP agent-spawn run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpSpawnEnd {
    /// The agent ended its turn cleanly.
    Clean,
    /// An operator stop arrived first; the session was closed and reaped.
    Stopped,
    /// The run failed. `child_started` says whether an agent process ran (its
    /// tree may then hold the agent's writes and must be kept).
    Failed { reason: String, child_started: bool },
}

/// Run a coord-dispatched agent spawn under an ACP agent. Unlike a gate
/// continuation there is no gate to report to, so a clean `end_turn` IS the end
/// and every other stop reason is a failure; and it is run ONCE, never
/// restarted — a partially-run agent session is not safely replayable, which is
/// why the restart loop of the Claude arm does not apply.
pub(crate) async fn run_agent_spawn_acp(
    agent_id: uuid::Uuid,
    workdir: &str,
    prompt: &str,
    agent: &ResolvedAcpAgent,
    coord_mcp: crate::coord_mcp::CoordMcpDelivery,
    push_ref: Option<&str>,
    stop: &tokio_util::sync::CancellationToken,
) -> AcpSpawnEnd {
    let prompt = match inline_skill(prompt, Path::new(workdir)) {
        SkillInlining::NotASkillPrompt => prompt.to_string(),
        SkillInlining::Inlined { prompt, .. } => prompt,
        SkillInlining::Missing { skill } => {
            return AcpSpawnEnd::Failed {
                reason: format!("acp_skill_unavailable:{skill}"),
                child_started: false,
            }
        }
    };
    let served = crate::served_corpus::probe_async(workdir).await;
    let briefing =
        crate::terminal::runner_context(crate::terminal::spawn_seam_api_port(), coord_mcp, &served);
    let launch =
        match select_headless_harness(Some(agent), workdir, acp_launch_env(agent, None).await) {
            HeadlessHarness::Acp(l) => l,
            // `Some(agent)` always selects ACP.
            HeadlessHarness::ClaudeCli => {
                return AcpSpawnEnd::Failed {
                    reason: "acp harness selection returned the Claude harness".to_string(),
                    child_started: false,
                }
            }
        };
    let mcp_workdir = workdir.to_string();
    let cfg = crate::acp_session::SessionConfig {
        cwd: PathBuf::from(workdir),
        briefing: Some(briefing),
        mcp: Some(Box::new(move |caps| {
            crate::coord_mcp::acp_mcp_entry(
                &mcp_workdir,
                caps.mcp_http,
                crate::coord_mcp::ProxyConfigIdentity::agent(&mcp_workdir, agent_id),
            )
        })),
        transcript: super::agent_logs_root().map(|d| d.join(format!("{agent_id}.acp.jsonl"))),
        permission: std::sync::Arc::new(crate::session::permission_accept::stream_json_denies),
        handshake_timeout: crate::acp_session::HANDSHAKE_TIMEOUT,
    };
    let mut session = match spawn_acp_agent(launch, coord_mcp, &served, cfg).await {
        Ok(s) => s,
        Err(e) => {
            return AcpSpawnEnd::Failed {
                child_started: !matches!(e.kind, AcpErrorKind::Spawn),
                reason: e.to_string(),
            }
        }
    };
    report_spawn_complete(
        agent_id,
        session.pid().map(i64::from),
        Some(&format!("acp:{}", agent.id)),
        push_ref,
    )
    .await;
    let end = tokio::select! {
        biased;
        _ = stop.cancelled() => {
            // Ask the agent to wind the turn down before the session is closed,
            // rather than cutting it off mid tool call.
            session.cancel();
            AcpSpawnEnd::Stopped
        },
        v = drive_verdict(&mut session, None, &prompt, &agent.id, TurnBounds::default()) => match v {
            AcpVerdict::Reported => AcpSpawnEnd::Clean,
            AcpVerdict::Unreported { detail, .. } => AcpSpawnEnd::Failed {
                reason: first_line(&detail),
                child_started: true,
            },
        },
    };
    session.close().await;
    end
}

#[cfg(test)]
mod tests;
