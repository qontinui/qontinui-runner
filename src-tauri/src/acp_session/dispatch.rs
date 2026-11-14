//! The headless ACP dispatch lane: run a coord continuation (or a coord agent
//! launch) under an ACP-native agent CLI instead of `claude`.
//!
//! Plan `2026-11-13-acp-client-lane-for-acp-native-clis`, Phases 2 and 3 (the
//! runner half). `agent_runtime.rs` chooses the harness
//! ([`crate::agent_runtime::HeadlessHarness`]) and calls into this module; every
//! ACP-specific decision lives here so the dispatch point stays a narrow match.
//!
//! What one run does, in order:
//!
//! 1. **Harness selection** ([`select_headless_harness`]): the frame's
//!    `"harness": {"acp": "<id>"}` is looked up in the `acp_agents` setting.
//!    Every miss is a typed refusal ([`HarnessRefusal`]) the caller posts as
//!    `spawn_failed: <detail>` — never a silent fall back to Claude.
//! 2. **Skill delivery** ([`inline_skill`]): `run /<skill> <args>` resolves only
//!    inside Claude Code, so the `SKILL.md` body is read from the served
//!    `.claude/` corpus and prefixed to the first prompt (`skill_delivery:
//!    inlined`). A skill that cannot be found refuses the run.
//! 3. **Spawn** through [`super::spawn_acp_agent`] — our own child, through
//!    the same headless env finalizer as the Claude child, plus the gate env
//!    (`QONTINUI_GATE_ID` / `QONTINUI_GATE_DEVICE_ID`) the terminal path sets.
//! 4. **coord-mcp** ([`coord_mcp_servers`]): the `McpServer` handed to
//!    `session/new` is read back from the `.mcp.json` the runner's one renderer
//!    just wrote (or the workdir already declared), so the agent and a Claude
//!    session in the same workdir are given the same door.
//! 5. **Verdict** (Design decision 1): the runner never writes `work_completed`
//!    or `work_abandoned`. After `end_turn` it reads the gate's standing outcome;
//!    if the session did not report, ONE nudge prompt, then `work_unreported`.
//!    Every other stop posts `work_unreported: acp_stop_reason=<token>`.
//! 6. **Close and reap** on every path; the transcript is a JSONL file beside
//!    the headless run's log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerStdio,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::{
    AcpError, AcpLaunch, AcpSession, AcpSessionRequest, AcpStopReason, AcpUsage, AgentAdvertised,
    BriefingDelivery, ChildExit, TranscriptLine,
};
use crate::agent_runtime::{HeadlessHarness, Presentation};
use crate::coord_mcp::CoordMcpDelivery;
use crate::settings::AcpAgentSettings;

/// Prefix of the host capability token a runner advertises per usable ACP
/// agent (Design decision 3): `acp:<id>`. Coord places a continuation carrying
/// `harness: {"acp": "<id>"}` only on a device advertising that token, so the
/// spelling is a wire contract with coord's frame builder.
pub(crate) const ACP_CAPABILITY_PREFIX: &str = "acp:";

/// The name the coord-mcp server is given in `session/new` — the same key it
/// has in `.mcp.json`.
const COORD_MCP_SERVER_NAME: &str = "coord-mcp";

/// Upper bound on the one nudge turn (it asks for a single report, not work).
/// Capped further by the agent's own turn budget.
const NUDGE_TURN_BUDGET: Duration = Duration::from_secs(15 * 60);

/// Whether `id` matches coord's agent-id grammar `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub(crate) fn valid_agent_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    let lead = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    lead(first)
        && rest.len() <= 63
        && rest
            .iter()
            .all(|b| lead(b) || matches!(b, b'.' | b'_' | b'-'))
}

/// The frame's `harness` key. Coord sends `{"acp": "<id>"}`; anything else
/// is kept as [`Self::Unrecognised`] so it is REFUSED rather than read as "no
/// harness" — a harness this runner cannot read must never become a Claude
/// spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessSpec {
    Acp(String),
    Unrecognised(Value),
}

impl<'de> Deserialize<'de> for HarnessSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        let acp = value
            .as_object()
            .filter(|o| o.len() == 1)
            .and_then(|o| o.get("acp"))
            .and_then(Value::as_str);
        Ok(match acp {
            Some(id) => Self::Acp(id.to_string()),
            None => Self::Unrecognised(value),
        })
    }
}

/// A selected ACP agent: its id (for logs, details and the run record) and
/// the launch, whose `cwd` is bound when the run starts (selection happens
/// before the worktree exists).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpHarness {
    pub agent: String,
    pub launch: AcpLaunch,
    /// Wall-clock bound on the first prompt turn (the agent's
    /// `timeout_seconds`).
    pub turn_budget: Duration,
}

/// Why an ACP harness request was refused. `Display` is the one-token detail
/// the caller posts as `spawn_failed: <detail>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HarnessRefusal {
    /// No `acp_agents` entry for the id. Coord's capability placement should
    /// make this unreachable; a runner that receives it anyway refuses.
    #[error("acp_agent_absent:{0}")]
    AgentAbsent(String),
    /// The id fails coord's agent-id grammar.
    #[error("acp_agent_id_invalid")]
    InvalidId,
    /// The `harness` value is not a shape this runner reads.
    #[error("harness_unrecognised")]
    Unrecognised,
    /// The entry's program does not resolve to an executable on this host.
    #[error("acp_agent_unresolvable:{0}")]
    ProgramUnresolvable(String),
    /// The ACP lane is headless; a visible-terminal presentation cannot carry it.
    #[error("acp_requires_headless:{0}")]
    TerminalPresentation(String),
    /// The launch pins an account but the agent declares no `env_account_var`
    /// to carry it; a pin silently ignored is indistinguishable from one
    /// honoured.
    #[error("acp_account_pin_unsupported:{0}")]
    AccountPinUnsupported(String),
}

/// Resolve an ACP agent's `program` to the executable a spawn would run: an
/// absolute path must be an executable file; a bare name is looked up on the
/// runner's inherited `PATH`. A relative path with directories is refused — it
/// would resolve against the runner's own cwd, not the session's.
pub(crate) fn resolve_acp_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        return is_executable_file(path).then(|| path.to_path_buf());
    }
    if path.components().count() != 1 {
        return None;
    }
    crate::automation_stack::which(program)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// The `acp:<id>` capability tokens for every configured agent whose id is
/// valid and whose program resolves (Design decision 3). PURE over `resolve`.
pub(crate) fn acp_capability_tokens(
    agents: &BTreeMap<String, AcpAgentSettings>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Vec<String> {
    agents
        .iter()
        .filter(|(id, agent)| valid_agent_id(id) && resolve(&agent.program).is_some())
        .map(|(id, _)| format!("{ACP_CAPABILITY_PREFIX}{id}"))
        .collect()
}

/// Choose the headless harness for one spawn. No `harness` key is
/// [`HeadlessHarness::ClaudeCli`] (today's behaviour, unchanged); an `acp`
/// harness must name a configured agent whose program resolves, on a headless
/// presentation. `account` is the launch's per-spawn account pin, carried to
/// the agent through its `env_account_var`.
pub(crate) fn select_headless_harness(
    harness: Option<&HarnessSpec>,
    presentation: Presentation,
    account: Option<&str>,
    agents: &BTreeMap<String, AcpAgentSettings>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Result<HeadlessHarness, HarnessRefusal> {
    let id = match harness {
        None => return Ok(HeadlessHarness::ClaudeCli),
        Some(HarnessSpec::Unrecognised(_)) => return Err(HarnessRefusal::Unrecognised),
        Some(HarnessSpec::Acp(id)) => id,
    };
    if !valid_agent_id(id) {
        return Err(HarnessRefusal::InvalidId);
    }
    if presentation == Presentation::Terminal {
        return Err(HarnessRefusal::TerminalPresentation(id.clone()));
    }
    let agent = agents
        .get(id)
        .ok_or_else(|| HarnessRefusal::AgentAbsent(id.clone()))?;
    let program =
        resolve(&agent.program).ok_or_else(|| HarnessRefusal::ProgramUnresolvable(id.clone()))?;
    let mut env = Vec::new();
    if let Some(account) = account {
        let var = agent
            .env_account_var
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| HarnessRefusal::AccountPinUnsupported(id.clone()))?;
        env.push((var.to_string(), account.to_string()));
    }
    Ok(HeadlessHarness::Acp(AcpHarness {
        agent: id.clone(),
        launch: AcpLaunch {
            program,
            args: agent.args.clone(),
            env,
            cwd: PathBuf::new(),
        },
        turn_budget: Duration::from_secs(agent.timeout_seconds.max(1)),
    }))
}

/// [`select_headless_harness`] against this machine's settings and `PATH`.
pub(crate) fn select_headless_harness_here(
    harness: Option<&HarnessSpec>,
    presentation: Presentation,
    account: Option<&str>,
) -> Result<HeadlessHarness, HarnessRefusal> {
    if harness.is_none() {
        return Ok(HeadlessHarness::ClaudeCli);
    }
    let agents = crate::settings::get_ai_settings().acp_agents;
    select_headless_harness(harness, presentation, account, &agents, resolve_acp_program)
}

/// The gate a continuation reports to and the device that claimed it — the
/// pair the terminal path exports as `QONTINUI_GATE_ID` /
/// `QONTINUI_GATE_DEVICE_ID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GateIdentity {
    pub gate_id: uuid::Uuid,
    pub device_id: uuid::Uuid,
}

/// The launch for one run: the selected agent bound to `workdir`, plus the
/// gate env. Applied before the headless env finalizer, so the credential
/// scrub still wins over anything here.
pub(crate) fn launch_for_run(
    harness: &AcpHarness,
    workdir: &Path,
    gate: Option<GateIdentity>,
) -> AcpLaunch {
    let mut launch = harness.launch.clone();
    launch.cwd = workdir.to_path_buf();
    if let Some(gate) = gate {
        launch.env.push((
            crate::commands::terminal::GATE_ID_ENV.to_string(),
            gate.gate_id.to_string(),
        ));
        launch.env.push((
            crate::commands::terminal::GATE_DEVICE_ID_ENV.to_string(),
            gate.device_id.to_string(),
        ));
    }
    launch
}

// =============================================================================
// Skill delivery
// =============================================================================

/// How the prompt's skill reached the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "skill_delivery", rename_all = "snake_case")]
pub(crate) enum SkillDelivery {
    /// The prompt does not invoke a skill.
    None,
    /// The `SKILL.md` (or command) body was prefixed to the first prompt.
    Inlined { skill: String, source: PathBuf },
}

/// `run /<name> <args>` on the prompt's first line → `(name, args)`.
pub(crate) fn parse_skill_invocation(prompt: &str) -> Option<(String, String)> {
    let first = prompt.lines().next()?.trim();
    let rest = first.strip_prefix("run /")?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    let mut chars = name.chars();
    let lead = chars.next()?;
    let valid = (lead.is_ascii_alphanumeric() || lead == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then(|| (name.to_string(), args.to_string()))
}

/// The nearest `.claude/skills/<name>/SKILL.md` (else `.claude/commands/<name>.md`)
/// at or above `workdir` — the served corpus, then the workspace roots above
/// it, in the order Claude Code itself resolves a project slash command.
pub(crate) async fn find_skill_body(workdir: &Path, name: &str) -> Option<(PathBuf, String)> {
    for dir in workdir.ancestors() {
        let claude = dir.join(".claude");
        for candidate in [
            claude.join("skills").join(name).join("SKILL.md"),
            claude.join("commands").join(format!("{name}.md")),
        ] {
            if let Ok(body) = tokio::fs::read_to_string(&candidate).await {
                return Some((candidate, body));
            }
        }
    }
    None
}

/// `body` without a leading YAML frontmatter block.
fn strip_frontmatter(body: &str) -> &str {
    let Some(rest) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    else {
        return body;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == "---" {
            return rest[offset..].trim_start_matches(['\r', '\n']);
        }
    }
    body
}

/// The first prompt with the skill inlined. A prompt that invokes no skill is
/// returned unchanged; one whose skill cannot be found is refused.
pub(crate) async fn inline_skill(
    prompt: &str,
    workdir: &Path,
) -> Result<(String, SkillDelivery), AcpDispatchError> {
    let Some((name, args)) = parse_skill_invocation(prompt) else {
        return Ok((prompt.to_string(), SkillDelivery::None));
    };
    let (source, body) = find_skill_body(workdir, &name)
        .await
        .ok_or_else(|| AcpDispatchError::SkillAbsent(name.clone()))?;
    let mut instructions = strip_frontmatter(&body).trim_end().to_string();
    if instructions.contains("$ARGUMENTS") {
        instructions = instructions.replace("$ARGUMENTS", &args);
    } else if !args.is_empty() {
        instructions.push_str(&format!("\n\nARGUMENTS: {args}"));
    }
    let text = format!(
        "The task below invokes the skill `/{name}`, which this agent cannot resolve by \
         itself. Its full instructions follow; carry them out as if `/{name}` had been run \
         with the arguments `{args}`.\n\n<skill name=\"{name}\" source=\"{source}\">\n\
         {instructions}\n</skill>\n\n{prompt}",
        source = source.display(),
    );
    Ok((text, SkillDelivery::Inlined { skill: name, source }))
}

// =============================================================================
// coord-mcp delivery
// =============================================================================

/// How coord-mcp reached the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub(crate) enum CoordMcpForAgent {
    /// The fleet stdio shim, exactly as `.mcp.json` declares it.
    Stdio,
    /// The loopback HTTP proxy with its nonce headers.
    Http,
    /// Nothing was handed to the agent, and why.
    NotDelivered { reason: String },
}

/// The `McpServer`s for `session/new`, read from `<workdir>/.mcp.json`'s
/// `coord-mcp` entry — the document the runner's single renderer
/// (`coord_mcp::coord_mcp_proxy_config_json`) wrote on `Provisioned`, or the
/// one the workdir already declared on `WorkdirDeclared`. Following the
/// document's transport keeps the two consumers on one door: a stdio entry is
/// handed over as stdio (mandatory for every ACP agent, and it survives nonce
/// rotation because the shim re-reads its credential file); an HTTP entry only
/// when the agent advertises HTTP MCP.
pub(crate) fn coord_mcp_servers(
    workdir: &Path,
    delivery: CoordMcpDelivery,
    agent_http: bool,
) -> (Vec<McpServer>, CoordMcpForAgent) {
    let not_delivered = |reason: &str| {
        (
            Vec::new(),
            CoordMcpForAgent::NotDelivered {
                reason: reason.to_string(),
            },
        )
    };
    if delivery == CoordMcpDelivery::Unprovisioned {
        return not_delivered("coord-mcp was not provisioned for this session");
    }
    let doc = std::fs::read_to_string(workdir.join(".mcp.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let Some(entry) = doc
        .as_ref()
        .and_then(crate::coord_mcp_config::coord_mcp_entry)
    else {
        return not_delivered("the workdir .mcp.json declares no readable coord-mcp entry");
    };
    match mcp_server_from_entry(entry, agent_http) {
        Ok((server, transport)) => (vec![server], transport),
        Err(reason) => not_delivered(&reason),
    }
}

/// One `.mcp.json` server entry → an ACP `McpServer`. PURE.
fn mcp_server_from_entry(
    entry: &Value,
    agent_http: bool,
) -> Result<(McpServer, CoordMcpForAgent), String> {
    let str_map = |key: &str| -> Vec<(String, String)> {
        entry
            .get(key)
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    match entry.get("type").and_then(Value::as_str) {
        Some("stdio") | None if entry.get("command").is_some() => {
            let command = entry
                .get("command")
                .and_then(Value::as_str)
                .ok_or("the stdio coord-mcp entry has no command")?;
            let args = entry
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let env = str_map("env")
                .into_iter()
                .map(|(k, v)| EnvVariable::new(k, v))
                .collect();
            Ok((
                McpServer::Stdio(
                    McpServerStdio::new(COORD_MCP_SERVER_NAME, command)
                        .args(args)
                        .env(env),
                ),
                CoordMcpForAgent::Stdio,
            ))
        }
        Some("http") => {
            if !agent_http {
                return Err("coord-mcp is HTTP-only here and the agent does not advertise \
                            mcpCapabilities.http"
                    .to_string());
            }
            let url = entry
                .get("url")
                .and_then(Value::as_str)
                .ok_or("the http coord-mcp entry has no url")?;
            let headers = str_map("headers")
                .into_iter()
                .map(|(k, v)| HttpHeader::new(k, v))
                .collect();
            Ok((
                McpServer::Http(McpServerHttp::new(COORD_MCP_SERVER_NAME, url).headers(headers)),
                CoordMcpForAgent::Http,
            ))
        }
        other => Err(format!(
            "coord-mcp entry transport {other:?} is not deliverable over ACP"
        )),
    }
}

// =============================================================================
// The run
// =============================================================================

/// What the gate's `continuation_consumed_outcome` reads, as far as the runner
/// can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// The value coord says it holds now.
    Value(String),
    /// No verdict (no coord, a transport failure, an unreadable answer) —
    /// UNKNOWN, never "not reported".
    Unknown,
}

impl Standing {
    /// The session's own report, when the standing value is one.
    fn self_report(&self) -> Option<&str> {
        match self {
            Self::Value(v) if crate::agent_runtime::is_session_work_outcome(v) => Some(v),
            _ => None,
        }
    }

    fn value(&self) -> Option<String> {
        match self {
            Self::Value(v) => Some(v.clone()),
            Self::Unknown => None,
        }
    }
}

/// Where a run's lifecycle facts go. The gate-continuation implementation
/// posts to coord's `continuation-consumed` route; the agent-launch one to the
/// agent lifecycle routes. There is deliberately no way to post
/// `work_completed` / `work_abandoned`: that vocabulary is the session's.
#[async_trait::async_trait]
pub(crate) trait AcpRunReporter: Send + Sync {
    /// The gate this run reports its outcome against; `None` = nothing to
    /// report to (an agent launch, a work-unit dispatch).
    fn gate_id(&self) -> Option<uuid::Uuid>;
    /// `session/new` answered: the agent is running.
    async fn session_started(&self, agent: &str, agent_session_id: &str);
    /// The gate's standing outcome. Only called when [`Self::gate_id`] is `Some`.
    async fn read_standing(&self) -> Standing;
    /// Post `work_unreported: <detail>`; returns what stands afterwards. Only
    /// called when [`Self::gate_id`] is `Some`.
    async fn post_work_unreported(&self, detail: &str) -> Standing;
    /// The session is closed and the child reaped.
    async fn run_finished(&self, report: &AcpRunReport);
}

/// How one prompt turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnEnd {
    /// The agent answered with a stop reason (or its transport closed).
    Stopped(AcpStopReason),
    /// The turn outran its wall-clock budget.
    BudgetExhausted,
    /// The runner was told to stop (an operator stop on an agent launch).
    RunnerCancelled,
    /// The prompt request itself failed.
    Failed(String),
}

impl TurnEnd {
    /// One token for the `acp_stop_reason=` detail.
    pub(crate) fn token(&self) -> &str {
        match self {
            Self::Stopped(reason) => reason.as_token(),
            Self::BudgetExhausted => "budget_exhausted",
            Self::RunnerCancelled => "runner_cancelled",
            Self::Failed(_) => "prompt_failed",
        }
    }

    fn is_end_turn(&self) -> bool {
        matches!(self, Self::Stopped(AcpStopReason::EndTurn))
    }
}

/// The `work_unreported` detail for a run whose session never reported.
/// `nudge` is the nudge turn's end, present only after an `end_turn`. PURE.
pub(crate) fn unreported_detail(first: &TurnEnd, nudge: Option<&TurnEnd>) -> String {
    match nudge {
        Some(nudge) => format!(
            "acp_stop_reason={} nudge_stop_reason={}",
            first.token(),
            nudge.token()
        ),
        None => format!("acp_stop_reason={}", first.token()),
    }
}

/// The one nudge prompt sent after an `end_turn` with no reported outcome.
pub(crate) fn nudge_prompt(gate_id: uuid::Uuid) -> String {
    format!(
        "Your turn ended without reporting this continuation's outcome to coord. Report \
         your outcome to coord for gate {gate_id} now: work_completed if the work is done, \
         or work_abandoned with a one-line reason if it is not. The gate id is in \
         $QONTINUI_GATE_ID and the consuming device in $QONTINUI_GATE_DEVICE_ID. Report and \
         do nothing else."
    )
}

/// The run's verdict (Design decision 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub(crate) enum AcpVerdict {
    /// The session reported its own outcome; the runner posted none.
    SelfReported { outcome: String, nudged: bool },
    /// The runner posted `work_unreported: <detail>`; `standing` is what coord
    /// says holds afterwards (`None` = no verdict, UNKNOWN).
    WorkUnreported {
        detail: String,
        standing: Option<String>,
    },
    /// No gate to report to: the first turn's stop reason is the outcome.
    NoGate { stop_reason: String },
}

/// One prompt turn, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TurnRecord {
    pub turn: &'static str,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Usage as far as the runner can account for it. No runner sink takes an ACP
/// `usage_update` (`ai_pricing` prices input/output tokens per model id, and
/// `ai_provider::account_usage` is Claude account rotation; an update carries
/// neither), so accounting is recorded as UNKNOWN beside the last report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct UsageRecord {
    pub accounting: &'static str,
    pub last: Option<AcpUsage>,
}

/// Everything one run produced, written as the transcript's last line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AcpRunReport {
    pub agent: String,
    pub agent_session_id: String,
    pub advertised: AgentAdvertised,
    pub briefing_delivery: BriefingDelivery,
    #[serde(flatten)]
    pub skill_delivery: SkillDelivery,
    pub coord_mcp: CoordMcpForAgent,
    pub turns: Vec<TurnRecord>,
    pub verdict: AcpVerdict,
    /// `kind: unknown` failure marker for every first turn that did not end
    /// with `end_turn` (the parent plan's failure taxonomy has not landed).
    pub failure_kind: Option<&'static str>,
    pub usage: UsageRecord,
    pub child: Option<ChildExit>,
}

impl AcpRunReport {
    /// The first turn ended with `end_turn`.
    pub(crate) fn clean(&self) -> bool {
        self.failure_kind.is_none()
    }

    /// The first turn's stop token.
    pub(crate) fn first_stop_reason(&self) -> &str {
        self.turns
            .first()
            .map_or("none", |t| t.stop_reason.as_str())
    }
}

/// Why a run did not start. `Display`'s first line is the `spawn_failed`
/// detail.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpDispatchError {
    #[error("acp_skill_absent:{0}")]
    SkillAbsent(String),
    #[error("acp_spawn_failed: {0}")]
    Spawn(#[source] AcpError),
    #[error("acp_session_new_failed: {source}")]
    NewSession {
        #[source]
        source: AcpError,
        child: Option<ChildExit>,
    },
}

/// The JSONL transcript. A write failure is logged once and the run goes on:
/// the transcript is evidence, not a reason to fail the work.
pub(crate) struct TranscriptSink {
    path: Option<PathBuf>,
    file: Option<tokio::fs::File>,
}

/// Runner-side transcript lines, beside the agent's [`TranscriptLine`]s.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RunnerLine<'a> {
    RunStart {
        agent: &'a str,
        program: String,
        args: &'a [String],
        cwd: String,
        gate_id: Option<uuid::Uuid>,
    },
    Initialized {
        advertised: &'a AgentAdvertised,
    },
    PromptSent {
        turn: &'static str,
        chars: usize,
    },
    TurnEnded {
        turn: &'static str,
        stop_reason: &'a str,
    },
    RunSummary {
        report: &'a AcpRunReport,
    },
}

impl TranscriptSink {
    pub(crate) async fn open(path: Option<PathBuf>) -> Self {
        let mut file = None;
        if let Some(path) = &path {
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
            {
                Ok(f) => file = Some(f),
                Err(e) => tracing::warn!(
                    "acp_session: transcript {} unwritable ({e}) — run continues unrecorded",
                    path.display()
                ),
            }
        }
        Self { path, file }
    }

    async fn write<T: Serialize>(&mut self, line: &T) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let mut bytes = match serde_json::to_vec(line) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("acp_session: transcript line unserializable: {e}");
                return;
            }
        };
        bytes.push(b'\n');
        if let Err(e) = file.write_all(&bytes).await {
            tracing::warn!(
                "acp_session: transcript {} write failed ({e}) — further lines dropped",
                self.path.as_deref().unwrap_or(Path::new("?")).display()
            );
            self.file = None;
        }
    }

    async fn write_updates(&mut self, lines: &[TranscriptLine]) {
        for line in lines {
            self.write(line).await;
        }
    }
}

/// Everything a run needs besides the harness.
pub(crate) struct AcpRunContext<'a> {
    pub workdir: &'a str,
    /// The composed continuation / launch prompt.
    pub prompt: &'a str,
    pub gate: Option<GateIdentity>,
    /// The per-session coord-mcp verdict the caller's provisioning produced.
    pub coord_mcp: CoordMcpDelivery,
    pub transcript_path: Option<PathBuf>,
    /// An operator stop (agent launches); `None` for continuations.
    pub cancel: Option<CancellationToken>,
}

/// A gate continuation (or work-unit dispatch) under `harness`: provision what
/// the headless Claude continuation provisions — coord-mcp for the workdir on
/// the device arm, and the fleet commands and skills the prompt may name, as
/// the terminal continuation does — then [`run_acp`].
pub(crate) async fn run_acp_continuation<R: AcpRunReporter + ?Sized>(
    harness: &AcpHarness,
    workdir: &str,
    prompt: &str,
    gate: Option<GateIdentity>,
    transcript_path: Option<PathBuf>,
    reporter: &R,
) -> Result<AcpRunReport, AcpDispatchError> {
    let coord_mcp = crate::coord_mcp::provision_coord_mcp_for_session(
        workdir,
        crate::coord_mcp::resolve_bound_api_port(),
        None,
    );
    crate::fleet_commands::provision_fleet_commands_for_session(workdir);
    crate::fleet_skills::provision_fleet_skills_for_session(workdir);
    run_acp(
        harness,
        AcpRunContext {
            workdir,
            prompt,
            gate,
            coord_mcp,
            transcript_path,
            cancel: None,
        },
        reporter,
    )
    .await
}

/// Run one headless continuation or launch under `harness`, end to end. The
/// caller has already provisioned the workdir (coord-mcp, commands, skills).
///
/// `Err` means nothing ran past `session/new` — the caller posts
/// `spawn_failed`. `Ok` means the session started (`reporter.session_started`
/// was called) and the verdict has been reached and reported; the session is
/// closed and the child reaped either way.
pub(crate) async fn run_acp<R: AcpRunReporter + ?Sized>(
    harness: &AcpHarness,
    ctx: AcpRunContext<'_>,
    reporter: &R,
) -> Result<AcpRunReport, AcpDispatchError> {
    let workdir = Path::new(ctx.workdir);
    let (first_prompt, skill_delivery) = inline_skill(ctx.prompt, workdir).await?;

    let launch = launch_for_run(harness, workdir, ctx.gate);
    let mut transcript = TranscriptSink::open(ctx.transcript_path).await;
    transcript
        .write(&RunnerLine::RunStart {
            agent: &harness.agent,
            program: launch.program.display().to_string(),
            args: &launch.args,
            cwd: launch.cwd.display().to_string(),
            gate_id: ctx.gate.map(|g| g.gate_id),
        })
        .await;
    let served = crate::served_corpus::probe_async(workdir).await;
    let session = super::spawn_acp_agent(&launch, ctx.coord_mcp, &served)
        .await
        .map_err(AcpDispatchError::Spawn)?;

    let (mcp_servers, coord_mcp) =
        coord_mcp_servers(workdir, ctx.coord_mcp, session.advertised().mcp_http);
    // The briefing the agent is given (systemPrompt or prompt prefix) names the
    // coord-mcp the AGENT actually received; the env copy rendered at spawn,
    // before `initialize`, could only name what the workdir was provisioned.
    let effective = if mcp_servers.is_empty() {
        CoordMcpDelivery::Unprovisioned
    } else {
        ctx.coord_mcp
    };
    let briefing = crate::terminal::runner_context(
        crate::terminal::spawn_seam_api_port(),
        effective,
        &served,
    );
    drive(
        session,
        DriveInput {
            agent: harness.agent.clone(),
            first_prompt,
            skill_delivery,
            mcp_servers,
            coord_mcp,
            briefing: Some(briefing),
            turn_budget: harness.turn_budget,
            cancel: ctx.cancel,
        },
        transcript,
        reporter,
    )
    .await
}

/// A connected session's run, after spawn.
struct DriveInput {
    agent: String,
    first_prompt: String,
    skill_delivery: SkillDelivery,
    mcp_servers: Vec<McpServer>,
    coord_mcp: CoordMcpForAgent,
    briefing: Option<String>,
    turn_budget: Duration,
    cancel: Option<CancellationToken>,
}

async fn cancelled(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// One bounded prompt turn; every update lands in the transcript.
async fn run_turn(
    session: &mut AcpSession,
    turn: &'static str,
    text: &str,
    budget: Duration,
    cancel: Option<&CancellationToken>,
    transcript: &mut TranscriptSink,
    last_usage: &mut Option<AcpUsage>,
) -> TurnEnd {
    transcript
        .write(&RunnerLine::PromptSent {
            turn,
            chars: text.chars().count(),
        })
        .await;
    let end = tokio::select! {
        result = tokio::time::timeout(budget, session.prompt(text)) => match result {
            Err(_) => TurnEnd::BudgetExhausted,
            Ok(Err(e)) => TurnEnd::Failed(e.to_string()),
            Ok(Ok(outcome)) => {
                transcript.write_updates(&outcome.updates).await;
                if let Some(usage) = outcome.usage.last() {
                    *last_usage = Some(usage.clone());
                }
                TurnEnd::Stopped(outcome.stop_reason)
            }
        },
        () = cancelled(cancel) => TurnEnd::RunnerCancelled,
    };
    // A turn cut short leaves its updates queued; keep them.
    transcript
        .write_updates(&session.take_pending_updates())
        .await;
    transcript
        .write(&RunnerLine::TurnEnded {
            turn,
            stop_reason: end.token(),
        })
        .await;
    end
}

fn record(turn: &'static str, end: &TurnEnd) -> TurnRecord {
    TurnRecord {
        turn,
        stop_reason: end.token().to_string(),
        error: match end {
            TurnEnd::Failed(message) => Some(message.clone()),
            _ => None,
        },
    }
}

/// Drive a connected session to its verdict, then close it. Always closes.
async fn drive<R: AcpRunReporter + ?Sized>(
    mut session: AcpSession,
    input: DriveInput,
    mut transcript: TranscriptSink,
    reporter: &R,
) -> Result<AcpRunReport, AcpDispatchError> {
    let advertised = session.advertised().clone();
    transcript
        .write(&RunnerLine::Initialized {
            advertised: &advertised,
        })
        .await;
    let started = match session
        .new_session(AcpSessionRequest {
            mcp_servers: input.mcp_servers,
            briefing: input.briefing,
        })
        .await
    {
        Ok(started) => started,
        Err(source) => {
            let closed = session.close().await;
            return Err(AcpDispatchError::NewSession {
                source,
                child: closed.child,
            });
        }
    };
    reporter
        .session_started(&input.agent, &started.agent_session_id)
        .await;

    let cancel = input.cancel.as_ref();
    let mut last_usage = None;
    let first = run_turn(
        &mut session,
        "first",
        &input.first_prompt,
        input.turn_budget,
        cancel,
        &mut transcript,
        &mut last_usage,
    )
    .await;
    let mut turns = vec![record("first", &first)];

    let verdict = match reporter.gate_id() {
        None => AcpVerdict::NoGate {
            stop_reason: first.token().to_string(),
        },
        Some(gate_id) => {
            let mut nudge = None;
            let mut verdict = None;
            if let Some(outcome) = reporter.read_standing().await.self_report() {
                verdict = Some(AcpVerdict::SelfReported {
                    outcome: outcome.to_string(),
                    nudged: false,
                });
            } else if first.is_end_turn() {
                let end = run_turn(
                    &mut session,
                    "nudge",
                    &nudge_prompt(gate_id),
                    input.turn_budget.min(NUDGE_TURN_BUDGET),
                    cancel,
                    &mut transcript,
                    &mut last_usage,
                )
                .await;
                turns.push(record("nudge", &end));
                nudge = Some(end);
                if let Some(outcome) = reporter.read_standing().await.self_report() {
                    verdict = Some(AcpVerdict::SelfReported {
                        outcome: outcome.to_string(),
                        nudged: true,
                    });
                }
            }
            match verdict {
                Some(v) => v,
                None => {
                    let detail = unreported_detail(&first, nudge.as_ref());
                    let standing = reporter.post_work_unreported(&detail).await;
                    // The session may have reported in the meantime; coord
                    // refuses the runner's write then, and the session's
                    // answer is what stands.
                    match standing.self_report() {
                        Some(outcome) => AcpVerdict::SelfReported {
                            outcome: outcome.to_string(),
                            nudged: nudge.is_some(),
                        },
                        None => AcpVerdict::WorkUnreported {
                            detail,
                            standing: standing.value(),
                        },
                    }
                }
            }
        }
    };

    transcript
        .write_updates(&session.take_pending_updates())
        .await;
    let closed = session.close().await;
    let report = AcpRunReport {
        agent: input.agent,
        agent_session_id: started.agent_session_id,
        advertised,
        briefing_delivery: started.briefing_delivery,
        skill_delivery: input.skill_delivery,
        coord_mcp: input.coord_mcp,
        turns,
        verdict,
        failure_kind: (!first.is_end_turn()).then_some("unknown"),
        usage: UsageRecord {
            accounting: "unknown",
            last: last_usage,
        },
        child: closed.child,
    };
    transcript
        .write(&RunnerLine::RunSummary { report: &report })
        .await;
    reporter.run_finished(&report).await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_session::PermissionPolicy;
    use agent_client_protocol::v1::schema::{
        AgentCapabilities, Implementation, InitializeRequest, InitializeResponse,
        McpCapabilities, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
        StopReason,
    };
    use agent_client_protocol::{Agent, ByteStreams};
    use std::sync::{Arc, Mutex};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // ---------------------------------------------------------------------
    // Fake agent and reporter
    // ---------------------------------------------------------------------

    #[derive(Default)]
    struct AgentLog {
        new_session: Option<NewSessionRequest>,
        prompts: Vec<String>,
    }

    /// A fake ACP agent answering each prompt with the next scripted stop
    /// reason (the last one repeats).
    fn start_fake_agent(
        stops: Vec<StopReason>,
        mcp_http: bool,
    ) -> (
        Arc<Mutex<AgentLog>>,
        impl agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static,
    ) {
        let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (agent_read, agent_write) = tokio::io::split(agent_io);
        let client_side = ByteStreams::new(client_write.compat_write(), client_read.compat());
        let agent_side = ByteStreams::new(agent_write.compat_write(), agent_read.compat());

        let log = Arc::new(Mutex::new(AgentLog::default()));
        let log_new = Arc::clone(&log);
        let log_prompt = Arc::clone(&log);
        let builder = Agent
            .v1()
            .name("fake-agent")
            .on_receive_request(
                async move |request: InitializeRequest, responder, _cx| {
                    responder.respond(
                        InitializeResponse::new(request.protocol_version)
                            .agent_capabilities(
                                AgentCapabilities::new()
                                    .mcp_capabilities(McpCapabilities::new().http(mcp_http)),
                            )
                            .agent_info(Implementation::new("fake-agent", "0.1.0")),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest, responder, _cx| {
                    log_new.lock().unwrap().new_session = Some(request);
                    responder.respond(NewSessionResponse::new("agent-session-1"))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, _cx| {
                    let text: String = request
                        .prompt
                        .iter()
                        .filter_map(|b| match b {
                            agent_client_protocol::v1::schema::ContentBlock::Text(t) => {
                                Some(t.text.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    let n = {
                        let mut log = log_prompt.lock().unwrap();
                        log.prompts.push(text);
                        log.prompts.len()
                    };
                    let stop = stops[(n - 1).min(stops.len() - 1)];
                    responder.respond(PromptResponse::new(stop))
                },
                agent_client_protocol::on_receive_request!(),
            );
        tokio::spawn(async move {
            let _ = builder.connect_to(agent_side).await;
        });
        (log, client_side)
    }

    /// Records every reporter call; `standings` are answered in order by
    /// `read_standing` (the last repeats).
    struct FakeReporter {
        gate: Option<uuid::Uuid>,
        standings: Vec<Standing>,
        reads: Mutex<usize>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeReporter {
        fn new(gate: Option<uuid::Uuid>, standings: Vec<Standing>) -> Self {
            Self {
                gate,
                standings,
                reads: Mutex::new(0),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AcpRunReporter for FakeReporter {
        fn gate_id(&self) -> Option<uuid::Uuid> {
            self.gate
        }
        async fn session_started(&self, agent: &str, agent_session_id: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("started {agent} {agent_session_id}"));
        }
        async fn read_standing(&self) -> Standing {
            let mut reads = self.reads.lock().unwrap();
            let s = self.standings[(*reads).min(self.standings.len() - 1)].clone();
            *reads += 1;
            self.calls.lock().unwrap().push("read".to_string());
            s
        }
        async fn post_work_unreported(&self, detail: &str) -> Standing {
            let posted = format!("work_unreported: {detail}");
            self.calls.lock().unwrap().push(format!("post {posted}"));
            Standing::Value(posted)
        }
        async fn run_finished(&self, report: &AcpRunReport) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("finished clean={}", report.clean()));
        }
    }

    fn allow() -> PermissionPolicy {
        Arc::new(|| false)
    }

    fn gate() -> uuid::Uuid {
        uuid::Uuid::parse_str("11111111-2222-4333-8444-555555555555").unwrap()
    }

    fn input(prompt: &str) -> DriveInput {
        DriveInput {
            agent: "gemini".into(),
            first_prompt: prompt.into(),
            skill_delivery: SkillDelivery::None,
            mcp_servers: Vec::new(),
            coord_mcp: CoordMcpForAgent::NotDelivered {
                reason: "test".into(),
            },
            briefing: None,
            turn_budget: Duration::from_secs(30),
            cancel: None,
        }
    }

    async fn run_fake(
        stops: Vec<StopReason>,
        reporter: &FakeReporter,
        transcript: Option<PathBuf>,
        prompt: &str,
    ) -> (Arc<Mutex<AgentLog>>, AcpRunReport) {
        let dir = tempfile::tempdir().unwrap();
        let (log, transport) = start_fake_agent(stops, false);
        let session = AcpSession::connect(transport, dir.path(), allow())
            .await
            .expect("initialize");
        let report = drive(
            session,
            input(prompt),
            TranscriptSink::open(transcript).await,
            reporter,
        )
        .await
        .expect("drive");
        (log, report)
    }

    fn assert_runner_never_claims_work(calls: &[String]) {
        for call in calls {
            assert!(
                !call.contains("work_completed") && !call.contains("work_abandoned"),
                "the runner posted a session-only outcome: {call}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Dispatch end to end (spawn → prompt → verdict → close)
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn acp_dispatch_end_turn_self_reported_needs_no_nudge() {
        let out = tempfile::tempdir().unwrap();
        let transcript = out.path().join("run.acp.jsonl");
        let reporter = FakeReporter::new(
            Some(gate()),
            vec![Standing::Value("work_completed".into())],
        );
        let (log, report) = run_fake(
            vec![StopReason::EndTurn],
            &reporter,
            Some(transcript.clone()),
            "do the work",
        )
        .await;

        assert_eq!(
            report.verdict,
            AcpVerdict::SelfReported {
                outcome: "work_completed".into(),
                nudged: false
            }
        );
        assert!(report.clean());
        assert_eq!(log.lock().unwrap().prompts, vec!["do the work".to_string()]);
        let calls = reporter.calls();
        assert_eq!(
            calls,
            vec![
                "started gemini agent-session-1".to_string(),
                "read".to_string(),
                "finished clean=true".to_string(),
            ]
        );
        assert!(report.child.is_none(), "in-process transport has no child");

        // The transcript is JSONL ending in the run summary.
        let body = std::fs::read_to_string(&transcript).unwrap();
        let lines: Vec<Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.first().unwrap()["type"], "initialized");
        let summary = lines.last().unwrap();
        assert_eq!(summary["type"], "run_summary");
        assert_eq!(summary["report"]["verdict"]["verdict"], "self_reported");
        assert_eq!(summary["report"]["usage"]["accounting"], "unknown");
        assert_eq!(summary["report"]["skill_delivery"], "none");
    }

    #[tokio::test]
    async fn acp_dispatch_end_turn_unreported_nudges_once_then_posts_work_unreported() {
        let reporter = FakeReporter::new(Some(gate()), vec![Standing::Value("spawned".into())]);
        let (log, report) =
            run_fake(vec![StopReason::EndTurn], &reporter, None, "do the work").await;

        let prompts = log.lock().unwrap().prompts.clone();
        assert_eq!(prompts.len(), 2, "exactly one nudge: {prompts:?}");
        assert!(prompts[1].contains(&gate().to_string()));
        let detail = "acp_stop_reason=end_turn nudge_stop_reason=end_turn".to_string();
        assert_eq!(
            report.verdict,
            AcpVerdict::WorkUnreported {
                detail: detail.clone(),
                standing: Some(format!("work_unreported: {detail}")),
            }
        );
        let calls = reporter.calls();
        assert_eq!(
            calls,
            vec![
                "started gemini agent-session-1".to_string(),
                "read".to_string(),
                "read".to_string(),
                format!("post work_unreported: {detail}"),
                "finished clean=true".to_string(),
            ]
        );
        assert_runner_never_claims_work(&calls);
        assert_eq!(report.turns.len(), 2);
    }

    #[tokio::test]
    async fn acp_dispatch_nudge_answered_by_a_self_report_posts_nothing() {
        let reporter = FakeReporter::new(
            Some(gate()),
            vec![
                Standing::Value("spawned".into()),
                Standing::Value("work_abandoned: blocked on review".into()),
            ],
        );
        let (_, report) = run_fake(vec![StopReason::EndTurn], &reporter, None, "go").await;
        assert_eq!(
            report.verdict,
            AcpVerdict::SelfReported {
                outcome: "work_abandoned: blocked on review".into(),
                nudged: true
            }
        );
        assert!(!reporter.calls().iter().any(|c| c.starts_with("post")));
    }

    #[tokio::test]
    async fn acp_dispatch_every_other_stop_reason_posts_work_unreported_without_a_nudge() {
        for (stop, token) in [
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::MaxTurnRequests, "max_turn_requests"),
            (StopReason::Refusal, "refusal"),
            (StopReason::Cancelled, "cancelled"),
        ] {
            let reporter =
                FakeReporter::new(Some(gate()), vec![Standing::Value("spawned".into())]);
            let (log, report) = run_fake(vec![stop], &reporter, None, "go").await;
            assert_eq!(log.lock().unwrap().prompts.len(), 1, "{token}: no nudge");
            let detail = format!("acp_stop_reason={token}");
            assert_eq!(
                report.verdict,
                AcpVerdict::WorkUnreported {
                    detail: detail.clone(),
                    standing: Some(format!("work_unreported: {detail}")),
                },
                "{token}"
            );
            assert_eq!(report.failure_kind, Some("unknown"), "{token}");
            let calls = reporter.calls();
            assert!(calls.contains(&format!("post work_unreported: {detail}")));
            assert_eq!(calls.last().unwrap(), "finished clean=false");
            assert_runner_never_claims_work(&calls);
        }
    }

    #[tokio::test]
    async fn acp_dispatch_without_a_gate_reads_and_posts_nothing() {
        let reporter = FakeReporter::new(None, vec![Standing::Unknown]);
        let (log, report) = run_fake(vec![StopReason::EndTurn], &reporter, None, "go").await;
        assert_eq!(
            report.verdict,
            AcpVerdict::NoGate {
                stop_reason: "end_turn".into()
            }
        );
        assert_eq!(log.lock().unwrap().prompts.len(), 1);
        assert_eq!(
            reporter.calls(),
            vec![
                "started gemini agent-session-1".to_string(),
                "finished clean=true".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn acp_dispatch_unknown_standing_still_nudges_and_reports() {
        // An unreadable gate is UNKNOWN, never "not reported" and never
        // "reported": the nudge is harmless, and coord refuses the runner's
        // write if the session did report.
        let reporter = FakeReporter::new(Some(gate()), vec![Standing::Unknown]);
        let (log, report) = run_fake(vec![StopReason::EndTurn], &reporter, None, "go").await;
        assert_eq!(log.lock().unwrap().prompts.len(), 2);
        assert!(matches!(report.verdict, AcpVerdict::WorkUnreported { .. }));
    }

    #[tokio::test]
    async fn acp_dispatch_spawn_failure_never_reports_a_started_session() {
        let dir = tempfile::tempdir().unwrap();
        let harness = AcpHarness {
            agent: "ghost".into(),
            launch: AcpLaunch {
                program: dir.path().join("no-such-agent"),
                args: vec![],
                env: vec![],
                cwd: PathBuf::new(),
            },
            turn_budget: Duration::from_secs(5),
        };
        let reporter = FakeReporter::new(Some(gate()), vec![Standing::Unknown]);
        let workdir = dir.path().to_string_lossy().to_string();
        let result = run_acp(
            &harness,
            AcpRunContext {
                workdir: &workdir,
                prompt: "go",
                gate: Some(GateIdentity {
                    gate_id: gate(),
                    device_id: gate(),
                }),
                coord_mcp: CoordMcpDelivery::Unprovisioned,
                transcript_path: None,
                cancel: None,
            },
            &reporter,
        )
        .await;
        let err = result.expect_err("a missing program cannot start");
        assert!(matches!(err, AcpDispatchError::Spawn(_)), "{err}");
        assert!(err.to_string().starts_with("acp_spawn_failed: "));
        assert!(reporter.calls().is_empty());
    }

    #[test]
    fn acp_dispatch_unreported_detail_maps_every_turn_end() {
        let stop = |r| TurnEnd::Stopped(r);
        assert_eq!(
            unreported_detail(&stop(AcpStopReason::TransportLost), None),
            "acp_stop_reason=transport_lost"
        );
        assert_eq!(
            unreported_detail(&stop(AcpStopReason::Unrecognised("paused".into())), None),
            "acp_stop_reason=paused"
        );
        assert_eq!(
            unreported_detail(&TurnEnd::BudgetExhausted, None),
            "acp_stop_reason=budget_exhausted"
        );
        assert_eq!(
            unreported_detail(&TurnEnd::RunnerCancelled, None),
            "acp_stop_reason=runner_cancelled"
        );
        assert_eq!(
            unreported_detail(&TurnEnd::Failed("x".into()), None),
            "acp_stop_reason=prompt_failed"
        );
        assert_eq!(
            unreported_detail(
                &stop(AcpStopReason::EndTurn),
                Some(&stop(AcpStopReason::TransportLost))
            ),
            "acp_stop_reason=end_turn nudge_stop_reason=transport_lost"
        );
    }

    // ---------------------------------------------------------------------
    // Harness selection
    // ---------------------------------------------------------------------

    fn agents() -> BTreeMap<String, AcpAgentSettings> {
        let mut agents = BTreeMap::new();
        agents.insert(
            "gemini".to_string(),
            AcpAgentSettings {
                program: "gemini".into(),
                args: vec!["--experimental-acp".into()],
                env_account_var: Some("GEMINI_CLI_HOME".into()),
                timeout_seconds: 120,
            },
        );
        agents.insert(
            "opencode".to_string(),
            AcpAgentSettings {
                program: "/nowhere/opencode".into(),
                args: vec!["acp".into()],
                env_account_var: None,
                timeout_seconds: 120,
            },
        );
        agents
    }

    fn resolve_gemini_only(program: &str) -> Option<PathBuf> {
        (program == "gemini").then(|| PathBuf::from("/usr/bin/gemini"))
    }

    fn select(
        harness: Option<HarnessSpec>,
        presentation: Presentation,
        account: Option<&str>,
    ) -> Result<HeadlessHarness, HarnessRefusal> {
        select_headless_harness(
            harness.as_ref(),
            presentation,
            account,
            &agents(),
            resolve_gemini_only,
        )
    }

    #[test]
    fn acp_dispatch_no_harness_is_the_claude_cli() {
        assert_eq!(
            select(None, Presentation::Headless, None),
            Ok(HeadlessHarness::ClaudeCli)
        );
        assert_eq!(
            select(None, Presentation::Terminal, Some("acct")),
            Ok(HeadlessHarness::ClaudeCli)
        );
    }

    #[test]
    fn acp_dispatch_selects_a_configured_resolvable_agent() {
        let Ok(HeadlessHarness::Acp(harness)) = select(
            Some(HarnessSpec::Acp("gemini".into())),
            Presentation::Headless,
            None,
        ) else {
            panic!("gemini must select");
        };
        assert_eq!(harness.agent, "gemini");
        assert_eq!(harness.launch.program, PathBuf::from("/usr/bin/gemini"));
        assert_eq!(harness.launch.args, vec!["--experimental-acp".to_string()]);
        assert!(harness.launch.env.is_empty());
        assert_eq!(harness.turn_budget, Duration::from_secs(120));
    }

    #[test]
    fn acp_dispatch_refuses_an_unknown_agent_id() {
        let refusal = select(
            Some(HarnessSpec::Acp("goose".into())),
            Presentation::Headless,
            None,
        )
        .unwrap_err();
        assert_eq!(refusal, HarnessRefusal::AgentAbsent("goose".into()));
        assert_eq!(refusal.to_string(), "acp_agent_absent:goose");
    }

    #[test]
    fn acp_dispatch_refuses_acp_on_a_terminal_presentation() {
        let refusal = select(
            Some(HarnessSpec::Acp("gemini".into())),
            Presentation::Terminal,
            None,
        )
        .unwrap_err();
        assert_eq!(refusal.to_string(), "acp_requires_headless:gemini");
    }

    #[test]
    fn acp_dispatch_refuses_invalid_unrecognised_and_unresolvable() {
        assert_eq!(
            select(
                Some(HarnessSpec::Acp("../x".into())),
                Presentation::Headless,
                None
            ),
            Err(HarnessRefusal::InvalidId)
        );
        assert_eq!(
            select(
                Some(HarnessSpec::Unrecognised(serde_json::json!({"pi": "x"}))),
                Presentation::Headless,
                None
            ),
            Err(HarnessRefusal::Unrecognised)
        );
        assert_eq!(
            select(
                Some(HarnessSpec::Acp("opencode".into())),
                Presentation::Headless,
                None
            ),
            Err(HarnessRefusal::ProgramUnresolvable("opencode".into()))
        );
    }

    #[test]
    fn acp_dispatch_account_pin_rides_the_agents_env_var_or_is_refused() {
        let Ok(HeadlessHarness::Acp(harness)) = select(
            Some(HarnessSpec::Acp("gemini".into())),
            Presentation::Headless,
            Some("/home/u/.gemini-work"),
        ) else {
            panic!("gemini must select");
        };
        assert_eq!(
            harness.launch.env,
            vec![(
                "GEMINI_CLI_HOME".to_string(),
                "/home/u/.gemini-work".to_string()
            )]
        );

        let mut no_var = agents();
        no_var.get_mut("gemini").unwrap().env_account_var = None;
        let refusal = select_headless_harness(
            Some(&HarnessSpec::Acp("gemini".into())),
            Presentation::Headless,
            Some("acct"),
            &no_var,
            resolve_gemini_only,
        )
        .unwrap_err();
        assert_eq!(refusal.to_string(), "acp_account_pin_unsupported:gemini");
    }

    #[test]
    fn acp_dispatch_agent_id_grammar() {
        for ok in ["gemini", "codex-acp", "a", "q2.5_x-y", &"a".repeat(64)] {
            assert!(valid_agent_id(ok), "{ok}");
        }
        for bad in ["", "-x", ".x", "Gemini", "a/b", "a b", &"a".repeat(65)] {
            assert!(!valid_agent_id(bad), "{bad}");
        }
    }

    // ---------------------------------------------------------------------
    // Frame deserialization (Phase 3)
    // ---------------------------------------------------------------------

    fn continuation_frame(extra: Value) -> Value {
        let mut frame = serde_json::json!({
            "target_device_id": "0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70",
            "initial_prompt": "run /dispatch-canary",
            "source": "gate_continuation",
            "presentation": "headless",
        });
        frame
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        frame
    }

    #[test]
    fn acp_dispatch_continuation_frame_with_and_without_harness() {
        let without: crate::agent_runtime::GateContinuationPayload =
            serde_json::from_value(continuation_frame(serde_json::json!({}))).unwrap();
        assert_eq!(without.harness, None);

        let with: crate::agent_runtime::GateContinuationPayload =
            serde_json::from_value(continuation_frame(serde_json::json!({
                "harness": {"acp": "gemini"},
                "required_capabilities": ["acp:gemini"],
            })))
            .unwrap();
        assert_eq!(with.harness, Some(HarnessSpec::Acp("gemini".into())));
        assert_eq!(with.presentation, Presentation::Headless);

        let null: crate::agent_runtime::GateContinuationPayload =
            serde_json::from_value(continuation_frame(serde_json::json!({"harness": null})))
                .unwrap();
        assert_eq!(null.harness, None);

        let odd: crate::agent_runtime::GateContinuationPayload =
            serde_json::from_value(continuation_frame(serde_json::json!({
                "harness": {"acp": "gemini", "mode": "x"}
            })))
            .unwrap();
        assert!(matches!(odd.harness, Some(HarnessSpec::Unrecognised(_))));
    }

    #[test]
    fn acp_dispatch_frame_naming_an_unconfigured_agent_is_refused_typed() {
        let frame: crate::agent_runtime::GateContinuationPayload =
            serde_json::from_value(continuation_frame(serde_json::json!({
                "harness": {"acp": "qwen"}
            })))
            .unwrap();
        let refusal = select_headless_harness(
            frame.harness.as_ref(),
            frame.presentation,
            None,
            &agents(),
            resolve_gemini_only,
        )
        .unwrap_err();
        assert_eq!(refusal.to_string(), "acp_agent_absent:qwen");
    }

    #[test]
    fn acp_dispatch_launch_payload_with_and_without_harness() {
        let base = serde_json::json!({
            "agent_id": "0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f70",
            "target_device_id": "0190000a-9b6c-7d3e-8f1a-2b3c4d5e6f71",
            "worktrees": [],
            "initial_prompt": "run /babysit-prs",
            "claim_token": "t",
        });
        let without: crate::agent_runtime::LaunchPayload =
            serde_json::from_value(base.clone()).unwrap();
        assert_eq!(without.harness, None);
        assert_eq!(without.presentation, None);

        let mut with = base;
        with.as_object_mut().unwrap().extend(
            serde_json::json!({"harness": {"acp": "gemini"}, "presentation": "headless"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let with: crate::agent_runtime::LaunchPayload = serde_json::from_value(with).unwrap();
        assert_eq!(with.harness, Some(HarnessSpec::Acp("gemini".into())));
        assert_eq!(with.presentation, Some(Presentation::Headless));
    }

    // ---------------------------------------------------------------------
    // Capability tokens and program resolution
    // ---------------------------------------------------------------------

    #[test]
    fn acp_dispatch_capability_token_only_for_a_resolvable_program() {
        let mut configured = agents();
        configured.insert(
            "Bad_Id".to_string(),
            AcpAgentSettings {
                program: "gemini".into(),
                args: vec![],
                env_account_var: None,
                timeout_seconds: 1,
            },
        );
        assert_eq!(
            acp_capability_tokens(&configured, resolve_gemini_only),
            vec!["acp:gemini".to_string()]
        );
        assert!(acp_capability_tokens(&BTreeMap::new(), resolve_gemini_only).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn acp_dispatch_resolves_only_executable_absolute_programs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("agent");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "x").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            resolve_acp_program(exe.to_str().unwrap()),
            Some(exe.clone())
        );
        assert_eq!(resolve_acp_program(plain.to_str().unwrap()), None);
        assert_eq!(
            resolve_acp_program(dir.path().join("absent").to_str().unwrap()),
            None
        );
        assert_eq!(resolve_acp_program("bin/agent"), None);
        assert_eq!(resolve_acp_program("qontinui-no-such-acp-agent-xyz"), None);
    }

    // ---------------------------------------------------------------------
    // The child's environment
    // ---------------------------------------------------------------------

    fn gemini_harness(env: Vec<(String, String)>) -> AcpHarness {
        AcpHarness {
            agent: "gemini".into(),
            launch: AcpLaunch {
                program: PathBuf::from("gemini"),
                args: vec!["--experimental-acp".into()],
                env,
                cwd: PathBuf::new(),
            },
            turn_budget: Duration::from_secs(60),
        }
    }

    fn env_value(cmd: &tokio::process::Command, key: &str) -> Option<String> {
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()))
    }

    #[test]
    fn acp_dispatch_child_carries_the_gate_env() {
        let workdir = PathBuf::from("/tmp/session");
        let identity = GateIdentity {
            gate_id: gate(),
            device_id: uuid::Uuid::parse_str("99999999-9999-4999-8999-999999999999").unwrap(),
        };
        let launch = launch_for_run(&gemini_harness(vec![]), &workdir, Some(identity));
        assert_eq!(launch.cwd, workdir);
        let cmd = crate::acp_session::build_acp_command(
            &launch,
            CoordMcpDelivery::Unprovisioned,
            &crate::served_corpus::ServedCorpus::unknown("test"),
        );
        assert_eq!(
            env_value(&cmd, crate::commands::terminal::GATE_ID_ENV),
            Some(gate().to_string())
        );
        assert_eq!(
            env_value(&cmd, crate::commands::terminal::GATE_DEVICE_ID_ENV),
            Some(identity.device_id.to_string())
        );

        let no_gate = launch_for_run(&gemini_harness(vec![]), &workdir, None);
        let cmd = crate::acp_session::build_acp_command(
            &no_gate,
            CoordMcpDelivery::Unprovisioned,
            &crate::served_corpus::ServedCorpus::unknown("test"),
        );
        assert_eq!(env_value(&cmd, crate::commands::terminal::GATE_ID_ENV), None);
    }

    #[test]
    fn acp_dispatch_child_env_scrub_matches_the_claude_arm() {
        // Every credential the scrub covers, carried in through the harness's
        // own env (the account-pin channel): the dispatch launch goes through
        // the same finalizer, so none survives — the assertion
        // `finalize_headless_child_env`'s tests make for the Claude child.
        let env = crate::terminal::CREDENTIAL_VALUE_ENV_VARS
            .iter()
            .map(|name| (name.to_string(), "hunter2".to_string()))
            .collect();
        let launch = launch_for_run(
            &gemini_harness(env),
            Path::new("."),
            Some(GateIdentity {
                gate_id: gate(),
                device_id: gate(),
            }),
        );
        let cmd = crate::acp_session::build_acp_command(
            &launch,
            CoordMcpDelivery::Unprovisioned,
            &crate::served_corpus::ServedCorpus::unknown("test"),
        );
        crate::terminal::assert_credentials_scrubbed_tokio(&cmd, "acp dispatch launch");
        crate::credential_helper::assert_non_interactive_git_posture_tokio(
            &cmd,
            "acp dispatch launch",
        );
    }

    // ---------------------------------------------------------------------
    // Skill delivery
    // ---------------------------------------------------------------------

    #[test]
    fn acp_dispatch_parses_only_a_run_skill_first_line() {
        assert_eq!(
            parse_skill_invocation("run /babysit-prs\n\nhint"),
            Some(("babysit-prs".into(), String::new()))
        );
        assert_eq!(
            parse_skill_invocation("run /expedite-review abc 123"),
            Some(("expedite-review".into(), "abc 123".into()))
        );
        assert_eq!(
            parse_skill_invocation("run /_detector-reach-fence"),
            Some(("_detector-reach-fence".into(), String::new()))
        );
        for no in ["fix the bug", "run /", "run /../x", "run /a/b", "please run /x"] {
            assert_eq!(parse_skill_invocation(no), None, "{no}");
        }
    }

    #[tokio::test]
    async fn acp_dispatch_inlines_the_nearest_skill_body() {
        let root = tempfile::tempdir().unwrap();
        let skill_dir = root.path().join(".claude/skills/dispatch-canary");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: dispatch-canary\ndescription: d\n---\n# Canary\nReport for $ARGUMENTS.\n",
        )
        .unwrap();
        let workdir = root.path().join("agent-worktrees/a/repo");
        std::fs::create_dir_all(&workdir).unwrap();

        let (text, delivery) = inline_skill("run /dispatch-canary g-1\n\nhint", &workdir)
            .await
            .unwrap();
        assert!(text.contains("# Canary\nReport for g-1."), "{text}");
        assert!(!text.contains("description: d"), "frontmatter stripped: {text}");
        assert!(text.ends_with("run /dispatch-canary g-1\n\nhint"));
        assert_eq!(
            delivery,
            SkillDelivery::Inlined {
                skill: "dispatch-canary".into(),
                source: skill_dir.join("SKILL.md"),
            }
        );

        // A command body is found too, and arguments without a placeholder
        // are appended.
        let cmd_dir = workdir.join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("vet-imp.md"), "Vet it.\n").unwrap();
        let (text, _) = inline_skill("run /vet-imp plans/x.md", &workdir)
            .await
            .unwrap();
        assert!(text.contains("Vet it.\n\nARGUMENTS: plans/x.md"), "{text}");
    }

    #[tokio::test]
    async fn acp_dispatch_refuses_a_skill_it_cannot_find_and_passes_plain_prompts() {
        let root = tempfile::tempdir().unwrap();
        let err = inline_skill("run /no-such-skill-xyz", root.path())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "acp_skill_absent:no-such-skill-xyz");

        let (text, delivery) = inline_skill("fix the bug", root.path()).await.unwrap();
        assert_eq!(text, "fix the bug");
        assert_eq!(delivery, SkillDelivery::None);
    }

    // ---------------------------------------------------------------------
    // coord-mcp delivery
    // ---------------------------------------------------------------------

    fn write_mcp_json(dir: &Path, entry: Value) {
        std::fs::write(
            dir.join(".mcp.json"),
            serde_json::json!({"mcpServers": {"coord-mcp": entry}}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn acp_dispatch_coord_mcp_follows_the_rendered_document() {
        let dir = tempfile::tempdir().unwrap();
        write_mcp_json(
            dir.path(),
            serde_json::json!({
                "type": "stdio",
                "command": "/usr/bin/python3",
                "args": ["/x/coord-mcp-shim.py", "--credential", "/c/cred.json"],
            }),
        );
        let (servers, transport) =
            coord_mcp_servers(dir.path(), CoordMcpDelivery::Provisioned, false);
        assert_eq!(transport, CoordMcpForAgent::Stdio);
        let McpServer::Stdio(stdio) = &servers[0] else {
            panic!("stdio expected: {servers:?}");
        };
        assert_eq!(stdio.name, "coord-mcp");
        assert_eq!(stdio.command, PathBuf::from("/usr/bin/python3"));
        assert_eq!(stdio.args[1], "--credential");

        // The http document: delivered only to an agent advertising HTTP MCP.
        let http = crate::coord_mcp::http_proxy_config_json(4567, "nonce-1", false);
        std::fs::write(dir.path().join(".mcp.json"), http.to_string()).unwrap();
        let (servers, transport) =
            coord_mcp_servers(dir.path(), CoordMcpDelivery::WorkdirDeclared, true);
        assert_eq!(transport, CoordMcpForAgent::Http);
        let McpServer::Http(server) = &servers[0] else {
            panic!("http expected: {servers:?}");
        };
        assert_eq!(server.url, "http://127.0.0.1:4567/coord-mcp");
        assert!(server
            .headers
            .iter()
            .any(|h| h.name == "Authorization" && h.value == "Bearer nonce-1"));

        let (servers, transport) =
            coord_mcp_servers(dir.path(), CoordMcpDelivery::Provisioned, false);
        assert!(servers.is_empty());
        assert!(matches!(transport, CoordMcpForAgent::NotDelivered { .. }));

        let (servers, _) = coord_mcp_servers(dir.path(), CoordMcpDelivery::Unprovisioned, true);
        assert!(servers.is_empty(), "an unprovisioned session is handed nothing");
    }
}
