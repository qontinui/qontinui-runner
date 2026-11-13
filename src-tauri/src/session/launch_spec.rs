//! Shared AI-CLI launch-command builder (#548/#779 seam, Phase 1; made
//! profile-driven and moved out of `claude_session/` by plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 6).
//!
//! One flag model, two renderers. The runner launches an AI CLI two ways and
//! both must be driven from a single source of truth so the operator's optional
//! launch flags (and, critically, the caller's non-negotiable *required* flags)
//! can never drift between them:
//!
//! - **Modality A — argv vector** ([`render_argv`]): a `Vec<String>` handed to a
//!   child `Command` / terminal backend (see `claude_session/session.rs`,
//!   `runner.rs`, `agent_runtime.rs`). The head is the resolved binary path; the
//!   caller sets the account variable on the child env itself, so argv never
//!   carries the env prefix.
//! - **Modality B — PTY-typed shell command** ([`render_pty_command`]): a shell
//!   string typed into a pseudo-terminal (see `aiLaunchCommand.ts`). The head is
//!   the bare program (PATH/shim resolves it) and the string is prefixed with
//!   the account variable's assignment.
//!
//! Both share [`compose_flags`] so the composed flag set is identical by
//! construction — the parity the tests pin.
//!
//! ## Every CLI fact comes from the profile
//!
//! [`LaunchSpec::provider`] is a [`CliProfile`], and the renderers read the
//! program, the account variable, the id flags, the auto-approve flags and the
//! PTY-only args from it — Claude by default, Codex when asked. A profile whose
//! identity is read back (Codex) has no pin flag, so a requested pin renders as
//! nothing; a profile whose resume id is positional (`codex resume <id>`) puts
//! the resume subcommand right after the program.
//!
//! The operator's launch templates ([`LaunchConfig`]) are CLAUDE settings
//! (`claude_default_launch_command`, `claude_account_launch_commands`, keyed by
//! Claude config dir), so they apply to a Claude launch only.
//!
//! ## Precedence (load-bearing)
//!
//! Per flag: **caller REQUIRED (`LaunchSpec` fields) > per-account override >
//! global template > CLI default.** The permission flag is *always*
//! caller-authoritative — an operator template that carries a conflicting
//! permission flag has that flag dropped, never applied. This invariant protects
//! autonomous spawns from being silently downgraded out of bypass mode.
//!
//! The one thing that may replace the spec's permission is the operator's
//! explicit session posture ([`SessionPermissionSetting`], carried on
//! [`LaunchConfig`]): selecting `allow_list` swaps every site's bypass flag for
//! [`PermissionMode::AllowList`], for machines whose IT-managed settings forbid
//! bypass (plan `2026-10-09-spec-front-end-of-the-software-factory` D7). It is a
//! typed setting, never a template flag.

use std::collections::HashSet;

use qontinui_runner_lib::cli_profile::{self, claude};
use qontinui_types::cli_session::{AutoApprove, CliProfile, ResumeSpec};

/// `{sessionId}` placeholder an operator may embed in a launch template; when
/// present the pinned id is substituted in place instead of a flag being
/// appended (mirrors the #779 frontend behavior in `aiLaunchCommand.ts`).
const SESSION_ID_PLACEHOLDER: &str = "{sessionId}";

/// Caller-authoritative permission posture. Always wins over any permission flag
/// found in an operator template. Defaults to `BypassPermissions` — every
/// autonomous spawn site must never stall on a prompt. The bypass spellings
/// come from the profile's [`AutoApprove::Flags`], the prompt spelling from its
/// `permission_prompt_args`; a profile that declares neither renders no
/// permission flag at all ([`permission_flags`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// The profile's `auto_approve.argv` (Claude `--permission-mode
    /// bypassPermissions`, Codex `--dangerously-bypass-approvals-and-sandbox`)
    /// — interactive/stream-json sites.
    #[default]
    BypassPermissions,
    /// The profile's first `auto_approve.detect` spelling, its single-flag
    /// skip-everything form (Claude `--dangerously-skip-permissions`) — the
    /// autonomous `agent_runtime` sites.
    DangerouslySkip,
    /// No auto-approval: the profile's `permission_prompt_args` (Claude
    /// `--permission-prompt-tool stdio`) so each tool approval arrives on the
    /// structured lane as a typed request the runner answers (plan
    /// `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
    /// Phase 9). **Never a default and never an autonomous site's choice** —
    /// only an operator's explicit structured launch from an interactive
    /// surface asks for it, because a request nobody answers stalls the turn
    /// until the runner's fail-closed deny.
    Prompt,
    /// `--permission-mode dontAsk --allowedTools <tools…>`: autonomy without
    /// bypass (plan `2026-10-09-spec-front-end-of-the-software-factory` D7), for
    /// machines whose IT-managed Claude Code settings disable bypass mode.
    ///
    /// `dontAsk` is Claude Code's no-prompt mode at the same privilege level as
    /// `default`: a tool something has pre-approved runs, anything else is
    /// refused instead of raising a prompt nobody is there to answer. What THIS
    /// renderer guarantees is narrower than "only these tools": the runner never
    /// widens the list — a template's own tool list, settings file, prompt tool
    /// and MCP config are dropped, and a caller's `--allowedTools` is
    /// intersected with it. Settings layers the runner does not control (a
    /// repository's `.claude/settings.json`, managed settings) can still
    /// pre-approve more tools; govern those to bound a session fully. An empty
    /// list renders no `--allowedTools`, so the runner pre-approves nothing.
    AllowList { tools: Vec<String> },
}

impl PermissionMode {
    /// Render this mode's argv flags in Claude Code's spelling (the
    /// profile-aware renderer is [`permission_flags`]).
    pub fn render(&self) -> Vec<String> {
        match self {
            PermissionMode::BypassPermissions => {
                vec![
                    "--permission-mode".to_string(),
                    "bypassPermissions".to_string(),
                ]
            }
            PermissionMode::DangerouslySkip => vec!["--dangerously-skip-permissions".to_string()],
            PermissionMode::AllowList { tools } => {
                let mut out = vec!["--permission-mode".to_string(), "dontAsk".to_string()];
                out.extend(allowed_tools_args(tools));
                out
            }
            PermissionMode::Prompt => claude::PROFILE.permission_prompt_args.clone(),
        }
    }
}

/// `["--allowedTools", t1, t2, …]` for the non-blank entries of `tools`, or
/// nothing when none remain. The one renderer for the flag: the
/// [`PermissionMode::AllowList`] spec, the scheduler's RemoteAgent launch
/// (`scheduler_remote_agent::claude_args`) and a workflow node's tool policy
/// (`tool_policy_args::build_tool_policy_cli`) all build it here.
pub fn allowed_tools_args<S: AsRef<str>>(tools: &[S]) -> Vec<String> {
    let kept: Vec<String> = tools
        .iter()
        .map(|t| t.as_ref().trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if kept.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(kept.len() + 1);
    out.push("--allowedTools".to_string());
    out.extend(kept);
    out
}

/// Operator-selected permission posture for runner-spawned sessions
/// (`settings.json` key `claude_session_permission`).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SessionPermissionSetting {
    /// Each spawn site keeps its own bypass flag (the historical behavior).
    #[default]
    SiteDefault,
    /// Every spawn runs [`PermissionMode::AllowList`] with these tools.
    AllowList {
        #[serde(default)]
        tools: Vec<String>,
    },
}

impl SessionPermissionSetting {
    /// The mode a spawn uses under this setting.
    pub fn apply(&self, site_default: PermissionMode) -> PermissionMode {
        match self {
            SessionPermissionSetting::SiteDefault => site_default,
            // An operator's explicit prompted launch already asks before every
            // tool and uses no bypass flag, so the allow-list (a narrowing of
            // the autonomous modes) does not replace it.
            SessionPermissionSetting::AllowList { .. } if site_default.prompts() => site_default,
            SessionPermissionSetting::AllowList { tools } => PermissionMode::AllowList {
                tools: tools.clone(),
            },
        }
    }

    /// Trim, drop blanks and duplicates, and refuse a tool spec that would
    /// break argv rendering (a leading `-` reads as a flag).
    pub fn normalized(&self) -> Result<Self, String> {
        match self {
            SessionPermissionSetting::SiteDefault => Ok(SessionPermissionSetting::SiteDefault),
            SessionPermissionSetting::AllowList { tools } => {
                let mut seen = HashSet::new();
                let mut kept = Vec::new();
                for t in tools {
                    let t = t.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if t.starts_with('-') {
                        return Err(format!(
                            "allow-list entry '{t}' starts with '-' and would be read as a flag"
                        ));
                    }
                    if t.contains(['\r', '\n']) {
                        return Err(format!("allow-list entry '{t}' contains a line break"));
                    }
                    if seen.insert(t.to_string()) {
                        kept.push(t.to_string());
                    }
                }
                Ok(SessionPermissionSetting::AllowList { tools: kept })
            }
        }
    }
}

impl PermissionMode {
    /// Whether tool approvals are the runner's to answer (the CLI asks), as
    /// opposed to bypassed at launch.
    pub fn prompts(&self) -> bool {
        matches!(self, PermissionMode::Prompt)
    }
}

/// Operator-configured optional launch templates, resolved for one Claude
/// account. Consulted only for a Claude launch — see the module docs.
///
/// Populated either directly (tests, explicit callers) or via
/// [`LaunchConfig::from_settings`] which reads the live settings for a given
/// config dir.
#[derive(Debug, Clone, Default)]
pub struct LaunchConfig {
    /// `settings.claude_default_launch_command` — machine-global template applied
    /// to every account without a per-account override. `None`/blank ⇒ built-in
    /// default.
    pub default_template: Option<String>,
    /// `settings.claude_account_launch_commands[config_dir]`, if any — the
    /// per-account override. May be a real `claude …` template OR an opaque alias
    /// (e.g. `clg`) the runner cannot introspect.
    pub account_command: Option<String>,
    /// `settings.claude_session_permission` — the operator's session permission
    /// posture. When it selects `allow_list` it replaces the spec's site default
    /// (see [`SessionPermissionSetting::apply`]); a template never decides
    /// permission. `Default` (site default) keeps the spec's mode.
    pub session_permission: SessionPermissionSetting,
}

impl LaunchConfig {
    /// Convenience constructor reading the live settings for `config_dir`.
    ///
    /// Consumed by the Phase 2/3 call-site migrations (each spawn site resolves
    /// its account's config dir and hands it here).
    pub fn from_settings(config_dir: Option<&str>) -> Self {
        let default_template = crate::settings::get_claude_default_launch_command();
        // A per-account command belongs to a subscription account, and those
        // are off while a model gateway is declared (it may be an alias that
        // pins its own account dir). Only the machine-global template applies.
        let session_permission = crate::settings::get_claude_session_permission();
        if crate::model_gateway::gateway_declared() {
            return Self {
                default_template,
                account_command: None,
                session_permission,
            };
        }
        let account_command = config_dir.and_then(|dir| {
            crate::settings::get_claude_account_launch_commands()
                .get(dir)
                .cloned()
        });
        Self {
            default_template,
            account_command,
            session_permission,
        }
    }
}

/// The caller's REQUIRED launch parameters — the non-negotiable half of the
/// composition. Everything here wins over the operator template.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// The CLI being launched. Every CLI-specific spelling the renderers emit
    /// is read from it. Defaults to Claude.
    pub provider: &'static CliProfile,
    /// The account dir, set through the profile's account variable
    /// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`). Used as the PTY env prefix; argv
    /// callers set the env on the child `Command` themselves. `None` ⇒ no
    /// prefix.
    pub config_dir: Option<String>,
    /// Caller-authoritative permission mode. Always applied; never overridden by
    /// the template. Defaults to `BypassPermissions`.
    pub permission: PermissionMode,
    /// New-pin id ⇒ `<pin flag> <id>` (Claude `--session-id`). Renders nothing
    /// for a profile that reads its id back. Ignored when `resume_id` is set.
    pub session_id: Option<String>,
    /// Resume id ⇒ the profile's resume (`--resume <id>`, or the positional
    /// `resume <id>` subcommand). Takes precedence over `session_id`.
    pub resume_id: Option<String>,
    /// Caller-provided model ⇒ `--model <m>`; subsumes model overrides and any
    /// failover sniff. When set, drops any `--model` from the template.
    pub model: Option<String>,
    /// Display name ⇒ `--name <v>` (shown in the prompt box, `/resume` picker and
    /// terminal title). Passed through [`sanitize_session_name`]; a name that
    /// sanitises to nothing is omitted. When set, drops any `--name` / `-n` (and
    /// attached `--name=…`) from the template. Claude only: ignored for a
    /// profile with no `--name` flag.
    pub name: Option<String>,
    /// Other non-negotiable trailing args, verbatim and in order — carries the
    /// `--append-system-prompt <s>`, `--add-dir <d>`, and the trailing
    /// `-- <prompt>` positional. Never reordered or deduped within.
    pub extra_required: Vec<String>,
}

/// Maximum length (chars) of a `--name` value.
pub const MAX_SESSION_NAME_CHARS: usize = 40;

/// Bytes of SHA-256 an over-long name ends in, written as 8 lowercase hex chars.
const SESSION_NAME_DIGEST_BYTES: usize = 4;

/// Chars an over-long name keeps ahead of its `-<digest>`: 31 at a 40-char cap.
const SESSION_NAME_HEAD_CHARS: usize = MAX_SESSION_NAME_CHARS - 1 - 2 * SESSION_NAME_DIGEST_BYTES;

/// Make `raw` safe to hand to `claude --name`: control characters and newlines
/// become spaces, whitespace runs collapse to one space, and leading `-` is
/// stripped (a value starting with `-` could be parsed as a flag). `None` when
/// nothing is left.
///
/// A cleaned value of at most [`MAX_SESSION_NAME_CHARS`] chars is returned
/// unchanged, so every name coord authors passes through byte-identical. A
/// longer one is compacted, never cut silently: it becomes its longest run of
/// leading whole tokens (split on `-` or ` `) that fits in 31 chars — a first
/// token that alone overflows is cut by chars to 31 — followed by `-` and the
/// first 8 lowercase hex of the SHA-256 of the whole cleaned value. The result
/// fits the cap, stays distinct from a value that differs only past the cut,
/// and never marks the cut with `…`. It is the session's name on every surface
/// (`--name`, `~/.qontinui/session-names/<id>`, the tab), and
/// `continuation-session.py` will not use a name ending in `…`, which reads as
/// cut from a longer value that no surface holds. It also sanitises to itself,
/// which `compose_flags` relies on when it re-sanitises a call site's name. Plan
/// `2026-11-09-gate-continuations-get-a-display-truncated-name-so-the-visible-hand-off-refuses-them`
/// D5.
pub fn sanitize_session_name(raw: &str) -> Option<String> {
    let spaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = collapsed.trim_start_matches(|c: char| c == '-' || c.is_whitespace());
    if cleaned.is_empty() {
        None
    } else if cleaned.chars().count() <= MAX_SESSION_NAME_CHARS {
        Some(cleaned.to_string())
    } else {
        Some(compact_session_name(cleaned))
    }
}

/// [`sanitize_session_name`]'s form for a cleaned value over the cap:
/// `<whole leading tokens>-<digest>`.
fn compact_session_name(cleaned: &str) -> String {
    use sha2::{Digest, Sha256};
    let is_sep = |c: char| c == '-' || c == ' ';
    // Counted in chars throughout, never sliced by byte (`clippy::string_slice`).
    let window: String = cleaned.chars().take(SESSION_NAME_HEAD_CHARS).collect();
    let window_ends_a_token = cleaned
        .chars()
        .nth(SESSION_NAME_HEAD_CHARS)
        .is_some_and(is_sep);
    // Whole tokens only: a token the window ends inside is dropped, and so are
    // the separators ahead of it. `cleaned` never starts with a separator, so a
    // window holding none is a first token that alone overflows, cut by chars.
    let head = if window_ends_a_token {
        window.trim_end_matches(is_sep)
    } else {
        window
            .rsplit_once(is_sep)
            .map_or(window.as_str(), |(kept, _)| kept.trim_end_matches(is_sep))
    };
    let digest = Sha256::digest(cleaned.as_bytes());
    let digest = hex::encode(&digest[..SESSION_NAME_DIGEST_BYTES]);
    format!("{head}-{digest}")
}

impl Default for LaunchSpec {
    fn default() -> Self {
        Self {
            provider: &claude::PROFILE,
            config_dir: None,
            permission: PermissionMode::default(),
            session_id: None,
            resume_id: None,
            model: None,
            name: None,
            extra_required: Vec::new(),
        }
    }
}

impl LaunchSpec {
    /// Whether this spec pins its session id on the argv — the profile has a
    /// pin flag, `session_id` is set, and no resume overrides it. A caller that
    /// must report the pinned id (`build_ai_launch_command`) asks this rather
    /// than guessing from the provider.
    pub fn pins_session_id(&self) -> bool {
        self.resume_id.is_none()
            && self.session_id.is_some()
            && cli_profile::pin_flag(self.provider).is_some()
    }
}

/// A parsed template flag and the value tokens that follow it (arity inferred:
/// a value is any following token that does not itself start with `-`). A bare
/// positional token is stored with an empty `values` list.
struct FlagUnit {
    name: String,
    values: Vec<String>,
}

/// Render the argv vector: `[bin, <resume subcommand…>, <composed flags…>]`.
/// No env prefix and no shell wrapper — both are the caller's concern. `bin`
/// is the resolved binary of `spec.provider`.
pub fn render_argv(spec: &LaunchSpec, cfg: &LaunchConfig, bin: &str) -> Vec<String> {
    let mut argv = Vec::with_capacity(1 + spec.extra_required.len() + 6);
    argv.push(bin.to_string());
    argv.extend(positional_resume(spec));
    argv.extend(compose_flags(spec, cfg));
    argv
}

/// The program `spec.provider` is launched as — the first of its profile's
/// programs, which is a bare name PATH (or an identity shim) resolves.
fn program(spec: &LaunchSpec) -> &str {
    spec.provider
        .programs
        .first()
        .map_or(spec.provider.id.as_str(), String::as_str)
}

/// The resume tokens that follow the program when the profile takes the
/// resume id POSITIONALLY (`codex resume <id>`): the template after its
/// program, with the id substituted. Empty when there is no resume, or when
/// the id rides a flag ([`compose_flags`] emits that one).
fn positional_resume(spec: &LaunchSpec) -> Vec<String> {
    let Some(id) = &spec.resume_id else {
        return Vec::new();
    };
    if cli_profile::resume_flag(spec.provider).is_some() {
        return Vec::new();
    }
    match &spec.provider.resume {
        ResumeSpec::ByIdArgv { template } => template
            .iter()
            .skip(1)
            .map(|arg| arg.replace(cli_profile::ID_PLACEHOLDER, id))
            .collect(),
        ResumeSpec::None | ResumeSpec::Unknown => Vec::new(),
    }
}

/// Render the `(program, args)` pair for a DIRECT (non-PTY) `claude` spawn,
/// branched by platform. The single place that decides whether a `cmd.exe`
/// wrapper is involved.
///
/// **Windows keeps `cmd.exe /c claude …`, and that is not incidental.** npm on
/// Windows installs `claude.cmd`, a batch shim `CreateProcessW` cannot launch
/// directly, so the `/c` wrapper is what performs the `.cmd` resolution. The
/// same rationale is recorded at `commands/ai_settings.rs` ("use cmd.exe /c to
/// handle .cmd files from npm install"), `ai_provider/gemini_cli.rs`, and in
/// [`crate::agent_runtime::resolve_claude_bin`]'s doc comment.
///
/// **Everywhere else there is no shim**, and `cmd.exe` does not exist — spawning
/// it failed with `No such file or directory (os error 2)`, which is what made
/// `POST /sessions/spawn` unusable on Linux. So resolve `claude` to a real
/// executable and exec it directly, exactly as
/// `agent_runtime::build_continuation_claude_command` already does for PTY
/// continuations.
///
/// The non-Windows arm passes the resolved head **into** [`render_argv`] rather
/// than pairing a separate program with an argv that still begins with
/// `"claude"`. That ordering is load-bearing: the latter execs
/// `<resolved> claude --flags`, handing the CLI its own name as the first
/// positional (i.e. as a prompt). `argv_head_is_claude_bin` pins the contract
/// this relies on.
///
/// **Blocking:** the non-Windows arm walks `PATH` with `stat`s via
/// [`crate::agent_runtime::resolve_claude_bin`], whose doc requires callers on
/// an async task to use `spawn_blocking`. That resolution is what skips the
/// runner's own per-terminal identity/shim dirs, which `shim_materializer` also
/// prepends to `PATH` off Windows — so a bare `Command::new("claude")` could
/// resolve to a shim, and the walk is not optional.
///
/// This inherits, and does not widen, `ClaudeSession::spawn`'s existing
/// blocking contract: that function already spawns a process and runs the whole
/// stream-json init handshake synchronously. Two of its callers —
/// `mcp/task_runs.rs::create_ai_session` and the `tokio::spawn` in
/// `mcp/backend_relay.rs` — are on the executor and were already violating it
/// far more expensively than a `PATH` walk; `mcp/sessions.rs` and the
/// `runner.rs` inline path both go through `spawn_blocking` and are fine.
/// Fixing those two is a separate change, not a consequence of this one.
///
/// The binary resolvers are Claude's (`QONTINUI_CLAUDE_BIN`, the shim-skipping
/// PATH walk): the direct spawn serves the structured lane, which the runner
/// implements for Claude alone. Another profile's spec renders its bare
/// program, left to the OS to resolve.
pub fn render_program_and_argv(spec: &LaunchSpec, cfg: &LaunchConfig) -> (String, Vec<String>) {
    let is_claude = spec.provider.id == claude::ID;
    #[cfg(target_os = "windows")]
    {
        // `claude_bin_path()` (not the literal `"claude"`) so a
        // `QONTINUI_CLAUDE_BIN` override is honoured on BOTH platforms. It
        // returns `"claude"` when unset, so the default invocation is unchanged.
        // The full `resolve_claude_bin()` is deliberately NOT used here: cmd.exe
        // does its own `.cmd`/PATHEXT resolution, which is the entire reason
        // this arm exists.
        let bin = if is_claude {
            crate::agent_runtime::claude_bin_path()
        } else {
            program(spec).to_string()
        };
        let mut args = vec!["/c".to_string()];
        args.extend(render_argv(spec, cfg, &bin));
        ("cmd.exe".to_string(), args)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let bin = if is_claude {
            crate::agent_runtime::resolve_claude_bin()
        } else {
            program(spec).to_string()
        };
        let argv = render_argv(spec, cfg, &bin);
        // `render_argv` always pushes the head first, so this never panics —
        // `argv_head_is_claude_bin` is the test that keeps it true.
        let (program, args) = argv
            .split_first()
            .expect("render_argv always emits the claude_bin head");
        (program.clone(), args.to_vec())
    }
}

/// Render the PTY-typed shell command string.
///
/// If the per-account override is an opaque alias (not a recognizable command
/// of the launched CLI), it is returned verbatim — the runner cannot introspect
/// it, so it applies no pin/permission/env (the #779 escape hatch). Otherwise
/// the profile's program, its positional resume (if any), its PTY-only args
/// ([`pty_only_args`]) and the composed flags are joined and prefixed with the
/// profile's account-variable assignment (omitted when `config_dir` is `None`
/// or the profile isolates accounts some other way).
pub fn render_pty_command(spec: &LaunchSpec, cfg: &LaunchConfig, is_windows: bool) -> String {
    // An opaque alias applies no permission flag the runner can see, so under
    // an allow-list posture it is refused (review M1): the composed command,
    // which carries the allow-list, is typed instead.
    let allow_list = matches!(
        cfg.session_permission.apply(spec.permission.clone()),
        PermissionMode::AllowList { .. }
    );
    if let Some(alias) = pty_verbatim_alias(spec, cfg) {
        if !allow_list {
            return alias;
        }
        tracing::warn!(
            "account launch alias ignored: an allow-list permission posture cannot be applied \
             through an opaque alias"
        );
    }

    let flags = compose_flags(spec, cfg);
    let pty_args = pty_only_args(spec, &flags);
    let resume = positional_resume(spec);
    let mut parts = Vec::with_capacity(resume.len() + pty_args.len() + flags.len() + 1);
    parts.push(program(spec).to_string());
    for token in resume.iter().chain(&pty_args).chain(&flags) {
        parts.push(shell_quote(token, is_windows));
    }
    let body = parts.join(" ");

    match (
        &spec.config_dir,
        cli_profile::account_env_var(spec.provider),
    ) {
        (Some(dir), Some(var)) if is_windows => format!("$env:{var}=\"{dir}\"; {body}"),
        (Some(dir), Some(var)) => format!("{var}=\"{dir}\" {body}"),
        _ => body,
    }
}

/// The launched profile's [`pty_args`] for a PTY-typed launch, placed right
/// after the head — ahead of any `--` that ends option parsing — unless `flags`
/// already set the first of them (an operator template choosing its own
/// `--teammate-mode` keeps it). Only the TUI launch gets them: the argv
/// renderer also serves the stream-json lane, which has no terminal to split.
/// The profile records the CLI version they were verified against; a CLI that
/// dropped a hidden flag refuses the launch loudly rather than misbehaving.
///
/// [`pty_args`]: qontinui_types::cli_session::CliProfile::pty_args
fn pty_only_args(spec: &LaunchSpec, flags: &[String]) -> Vec<String> {
    let args = &spec.provider.pty_args;
    let Some(name) = args.first() else {
        return Vec::new();
    };
    let already_set = flags.iter().take_while(|f| f.as_str() != "--").any(|f| {
        f == name
            || f.strip_prefix(name.as_str())
                .is_some_and(|rest| rest.starts_with('='))
    });
    if already_set {
        Vec::new()
    } else {
        args.clone()
    }
}

/// The shared flag-composition core. Both renderers build on this so the
/// composed flag set can never diverge between argv and PTY.
///
/// Order: `[permission] [model] [session] [other template flags…] [extra_required…]`.
/// Precedence per flag: spec field > per-account template > global template >
/// CLI default.
fn compose_flags(spec: &LaunchSpec, cfg: &LaunchConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let profile = spec.provider;

    // 1. Permission — caller-authoritative, always applied. The operator's
    //    explicit session posture may replace the site default; a template
    //    never can.
    //    Under an allow-list, a caller-supplied `--allowedTools` (a workflow
    //    node's tool policy) NARROWS the list — it is intersected and removed
    //    from the trailing args, never appended (review M1).
    let mut permission = cfg.session_permission.apply(spec.permission.clone());
    let mut extra_required = spec.extra_required.clone();
    if let PermissionMode::AllowList { tools } = &permission {
        if let Some(caller_tools) = take_caller_allowed_tools(&mut extra_required) {
            permission = PermissionMode::AllowList {
                tools: intersect_tool_lists(tools, &caller_tools),
            };
        }
    }
    out.extend(permission_flags(profile, &permission));
    let allow_list_spec = matches!(permission, PermissionMode::AllowList { .. });

    // Resolve the template to layer in (per-account command wins over the
    // global default; an opaque account alias falls through to the global
    // default for the argv/compose path).
    let template = launch_template(spec, cfg);
    let permission_names = permission_flag_names(profile);
    let id_flag_names: Vec<&str> = cli_profile::id_flags(profile)
        .into_iter()
        .map(|(flag, _)| flag)
        .collect();
    let pin = spec.resume_id.as_deref().or(spec.session_id.as_deref());
    let provided = provided_flag_names(&extra_required);
    let caller_owns_append_prompt = provides_append_prompt_flag(&extra_required);
    // `--name` is a Claude Code flag; no other profile declares one, so a
    // display name is dropped rather than handed to a CLI that would refuse it.
    let name = (profile.id == claude::ID)
        .then(|| spec.name.as_deref().and_then(sanitize_session_name))
        .flatten();

    let mut template_model: Option<String> = None;
    let mut other_units: Vec<FlagUnit> = Vec::new();
    let mut session_via_placeholder = false;

    if let Some(t) = &template {
        let had_placeholder = t.contains(SESSION_ID_PLACEHOLDER);
        session_via_placeholder = had_placeholder;
        let substituted = t.replace(SESSION_ID_PLACEHOLDER, pin.unwrap_or(""));
        let tokens = shell_tokenize(&substituted);
        // Drop the program head token (guaranteed present — `launch_template`
        // only returns recognizable commands of the launched CLI).
        let body = tokens.get(1..).unwrap_or(&[]);
        for unit in parse_units(body) {
            match unit.name.as_str() {
                // Permission is spec-owned: drop any template permission flag so
                // it can never override the caller — in its `--flag=value`
                // spelling too, or a template could layer bypass into a
                // prompted launch.
                name if permission_names
                    .contains(&name.split_once('=').map_or(name, |(n, _)| n)) => {}
                // Under an allow-list spec the tool list is spec-owned too: a
                // template's own `--allowedTools` would widen it, and
                // `--allow-dangerously-skip-permissions` would re-open bypass.
                n if allow_list_spec && is_allow_list_owned_flag(n) => {
                    tracing::warn!(
                        flag = %flag_name_only(n),
                        "launch template flag dropped: the allow-list permission mode owns it"
                    );
                }
                // Model is decided below (spec beats template).
                "--model" => template_model = unit.values.first().cloned(),
                // Name is spec-owned when the caller supplies one: drop the
                // template's `--name` / `-n` (either spelling) so two never ship.
                "--name" | "-n" if name.is_some() => {}
                n if name.is_some() && n.starts_with("--name=") => {}
                // Session flags: when the operator positioned the id via the
                // `{sessionId}` placeholder, keep the flag in place; otherwise the
                // spec owns session and we drop the template's.
                name if id_flag_names.contains(&name) => {
                    if had_placeholder {
                        other_units.push(unit);
                    }
                }
                // The append prompt flags are one mutually-exclusive group: when
                // the caller supplies either (the runner's spawn-time carrier),
                // drop both from the template, whatever the spelling. Loudly —
                // an operator's configured prompt silently vanishing is exactly
                // what a reader of the launch would never guess.
                name if caller_owns_append_prompt && is_append_prompt_flag(name) => {
                    tracing::warn!(
                        flag = %flag_name_only(name),
                        "launch template flag dropped: the caller's system-prompt carrier owns \
                         the append slot and Claude Code refuses both append flags together"
                    );
                }
                // Under an allow-list ONLY a known-harmless template flag
                // survives (review N4): anything else — `--plugin-dir` (plugins
                // carry PreToolUse allow hooks), `--add-dir`, a flag this
                // runner does not know yet — could widen what runs.
                n if allow_list_spec && !is_permitted_under_allow_list(n) => {
                    tracing::warn!(
                        flag = %flag_name_only(n),
                        "launch template flag dropped: not permitted under the allow-list \
                         permission mode"
                    );
                }
                // Any other operator flag is layered in, unless the caller's
                // extra_required already provides that flag (spec wins).
                // A prompted launch layers in only the template flags known
                // not to touch permissions — fail closed: a flag this list
                // does not name (`--settings` carrying a `defaultMode`,
                // `--allowedTools`, a bypass spelling a newer CLI adds) could
                // stop the session asking.
                name if permission.prompts() && !prompt_mode_admits(name) => {
                    tracing::warn!(
                        flag = %name,
                        "launch template flag dropped: a prompted launch layers in only flags \
                         that cannot change its permission posture"
                    );
                }
                _ => {
                    if provided.contains(&unit.name) {
                        tracing::warn!(
                            flag = %flag_name_only(&unit.name),
                            "launch template flag dropped: the caller supplies the same flag"
                        );
                    } else {
                        other_units.push(unit);
                    }
                }
            }
        }
    }

    // 2. Model — spec beats template; else template; else CLI default (omit).
    if let Some(model) = &spec.model {
        out.push("--model".to_string());
        out.push(model.clone());
    } else if let Some(model) = template_model {
        out.push("--model".to_string());
        out.push(model);
    }

    // 3. Session — appended only when the template did not position it via
    //    placeholder. resume takes precedence over session-id. A positional
    //    resume is not a flag: the renderers emit it after the program
    //    ([`positional_resume`]). A profile with no pin flag pins nothing.
    if !session_via_placeholder {
        if let Some(resume) = &spec.resume_id {
            if let Some(flag) = cli_profile::resume_flag(profile) {
                out.push(flag.to_string());
                out.push(resume.clone());
            }
        } else if let (Some(session), Some(flag)) =
            (&spec.session_id, cli_profile::pin_flag(profile))
        {
            out.push(flag.to_string());
            out.push(session.clone());
        }
    }

    // 3b. Display name — a separate token pair, like `--session-id`.
    if let Some(n) = name {
        out.push("--name".to_string());
        out.push(n);
    }

    // 4. Remaining operator template flags, in template order (includes a
    //    placeholder-positioned session flag).
    for unit in other_units {
        out.push(unit.name);
        out.extend(unit.values);
    }

    // 5. Caller's required trailing args, verbatim and in order (carries the
    //    `-- <prompt>` positional). Never reordered or deduped.
    out.extend(extra_required);

    out
}

/// Remove every `--allowedTools` / `--allowed-tools` unit (spaced or
/// `=`-attached) ahead of the `--` terminator from `extra`, returning the tools
/// they named, or `None` when the caller supplied no such flag.
fn take_caller_allowed_tools(extra: &mut Vec<String>) -> Option<Vec<String>> {
    let mut found: Option<Vec<String>> = None;
    let mut kept = Vec::with_capacity(extra.len());
    let mut i = 0;
    while i < extra.len() {
        let tok = &extra[i];
        if tok == "--" {
            kept.extend(extra[i..].iter().cloned());
            break;
        }
        let (name, attached) = match tok.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (tok.as_str(), None),
        };
        if matches!(name, "--allowedTools" | "--allowed-tools") {
            let list = found.get_or_insert_with(Vec::new);
            if let Some(v) = attached {
                list.extend(split_tool_specs(v));
            }
            i += 1;
            while i < extra.len() && !extra[i].starts_with('-') {
                list.extend(split_tool_specs(&extra[i]));
                i += 1;
            }
            continue;
        }
        kept.push(tok.clone());
        i += 1;
    }
    *extra = kept;
    found
}

/// Split one `--allowedTools` value the way Claude Code reads it: comma or
/// whitespace separated, except inside a specifier's parentheses
/// (`Bash(git log, status)` is one tool).
fn split_tool_specs(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for ch in value.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && (ch == ',' || ch.is_whitespace()) {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// The tools in `allow` that `narrower` also names, in `allow`'s order. Never
/// adds a tool `allow` lacks — the allow-list can be narrowed, never widened.
pub fn intersect_tool_lists<A: AsRef<str>, B: AsRef<str>>(
    allow: &[A],
    narrower: &[B],
) -> Vec<String> {
    let wanted: HashSet<String> = narrower
        .iter()
        .flat_map(|t| split_tool_specs(t.as_ref()))
        .collect();
    let mut seen = HashSet::new();
    allow
        .iter()
        .flat_map(|t| split_tool_specs(t.as_ref()))
        .filter(|t| wanted.contains(t) && seen.insert(t.clone()))
        .collect()
}

/// The launch config that applies to `spec`: the Claude account templates
/// apply to a Claude launch only (module docs); any other CLI is launched
/// without operator templates.
fn applicable_config<'a>(spec: &LaunchSpec, cfg: &'a LaunchConfig) -> Option<&'a LaunchConfig> {
    (spec.provider.id == claude::ID).then_some(cfg)
}

/// The template to parse for flag composition, honoring precedence: a
/// per-account command of the launched CLI wins; an opaque account alias is
/// skipped (handled verbatim for PTY, ignored for argv) and the global default
/// is used.
fn launch_template(spec: &LaunchSpec, cfg: &LaunchConfig) -> Option<String> {
    let cfg = applicable_config(spec, cfg)?;
    if let Some(acct) = &cfg.account_command {
        if is_command_of(acct, spec.provider) {
            return Some(acct.clone());
        }
        // Opaque alias → fall through to the global default for compose.
    }
    if let Some(def) = &cfg.default_template {
        let trimmed = def.trim();
        if !trimmed.is_empty() && is_command_of(trimmed, spec.provider) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// The per-account override when it is an opaque alias (PTY returns it verbatim).
fn pty_verbatim_alias(spec: &LaunchSpec, cfg: &LaunchConfig) -> Option<String> {
    applicable_config(spec, cfg)?
        .account_command
        .as_ref()
        .filter(|a| !is_command_of(a, spec.provider))
        .cloned()
}

/// Whether `s` is a recognizable invocation of `profile`'s CLI (vs. an opaque
/// alias): its head token is one of the profile's programs
/// ([`cli_profile::profile_for_program`] — basename, case-insensitive, a
/// Windows launcher suffix tolerated).
fn is_command_of(s: &str, profile: &CliProfile) -> bool {
    shell_tokenize(s.trim())
        .first()
        .and_then(|head| cli_profile::profile_for_program(head))
        .is_some_and(|p| p.id == profile.id)
}

/// The operator-template flags a [`PermissionMode::Prompt`] launch layers in —
/// every one of them unable to change whether the CLI asks before a tool call.
/// Anything else in a template (`--settings`, `--allowedTools`,
/// `--allow-dangerously-skip-permissions`, a positional, a flag a newer CLI
/// adds) is dropped from a prompted launch. `--model` and the session flags are
/// decided before this list is consulted.
const PROMPT_MODE_TEMPLATE_FLAGS: &[&str] = &[
    "--add-dir",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--system-prompt",
    "--system-prompt-file",
    "--verbose",
    "--debug",
    "--fallback-model",
    "--max-turns",
    "--disallowedTools",
    "--disallowed-tools",
    "--teammate-mode",
    // A display name changes nothing about permission posture.
    "--name",
    "-n",
];

/// Whether a prompted launch keeps template flag `name` (`--flag` or
/// `--flag=value`).
fn prompt_mode_admits(name: &str) -> bool {
    let flag = name.split_once('=').map_or(name, |(n, _)| n);
    PROMPT_MODE_TEMPLATE_FLAGS.contains(&flag)
}

/// The caller-authoritative permission flags for `mode` under `profile`
/// ([`PermissionMode`]). Empty when the profile declares no auto-approval (a
/// bypass mode) or no prompt arguments ([`PermissionMode::Prompt`]). A prompted
/// launch never carries a bypass flag.
fn permission_flags(profile: &CliProfile, mode: &PermissionMode) -> Vec<String> {
    let bypass = match &profile.auto_approve {
        AutoApprove::Flags { argv, detect } => Some((argv, detect)),
        _ => None,
    };
    match (mode, bypass) {
        (PermissionMode::Prompt, _) => profile.permission_prompt_args.clone(),
        // `--permission-mode dontAsk --allowedTools …` is Claude Code's
        // spelling; no other profile declares an allow-list mode, so another
        // CLI gets no auto-approval at all — fail closed, never bypass.
        (PermissionMode::AllowList { .. }, _) if profile.id == claude::ID => mode.render(),
        (PermissionMode::AllowList { .. }, _) => Vec::new(),
        (_, None) => Vec::new(),
        (PermissionMode::BypassPermissions, Some((argv, _))) => argv.clone(),
        (PermissionMode::DangerouslySkip, Some((argv, detect))) => detect
            .first()
            .map(|spelling| shell_tokenize(spelling))
            .unwrap_or_else(|| argv.clone()),
    }
}

/// Every flag NAME `profile` spells auto-approval or prompt routing with — the
/// first token of its auto-approve argv and of each detect spelling, and every
/// flag in its `permission_prompt_args`, cut at `=`. A template flag with one of these
/// names is dropped: permission is caller-owned, so a template can neither
/// upgrade a prompted launch to bypass nor re-route its prompts.
fn permission_flag_names(profile: &CliProfile) -> Vec<&str> {
    let (argv, detect): (&[String], &[String]) = match &profile.auto_approve {
        AutoApprove::Flags { argv, detect } => (argv, detect),
        _ => (&[], &[]),
    };
    argv.first()
        .map(String::as_str)
        .into_iter()
        .chain(detect.iter().filter_map(|d| d.split_whitespace().next()))
        .chain(profile.permission_prompt_args.iter().map(String::as_str))
        .map(|token| token.split_once('=').map_or(token, |(name, _)| name))
        .filter(|name| name.starts_with('-'))
        .collect()
}

/// Claude Code's two APPEND system-prompt flags, which behave as ONE
/// mutually-exclusive group rather than two independent flags: the CLI refuses
/// `--append-system-prompt` beside `--append-system-prompt-file` (`Error: Cannot
/// use both …`, verified against the Claude Code CLI in use when this landed), so a template's inline prompt
/// layered next to a caller's composed-file carrier (plan
/// `2026-09-15-runner-policy-injection-off-sessionstart-hook-channel`) would stop
/// the spawn outright. Exact-name dedup cannot see that collision.
///
/// `--system-prompt` / `--system-prompt-file` are deliberately NOT members:
/// probed against the same CLI, either one starts fine beside either append
/// flag, so an operator's replacement prompt is layered in as before. (The
/// replacement pair refuses each other, but the runner's carrier is never a
/// replacement flag, so that pair cannot collide with it.)
const APPEND_PROMPT_FLAG_GROUP: [&str; 2] =
    ["--append-system-prompt", "--append-system-prompt-file"];

/// Is `token` one of [`APPEND_PROMPT_FLAG_GROUP`], in either the `--flag` or
/// the attached `--flag=value` spelling?
fn is_append_prompt_flag(token: &str) -> bool {
    let name = token.split_once('=').map_or(token, |(name, _)| name);
    APPEND_PROMPT_FLAG_GROUP.contains(&name)
}

/// Template flags permitted under an [`PermissionMode::AllowList`] spec
/// (spaced or `=`-attached). An allow-list of flags, not a deny-list, so a CLI
/// flag added later is dropped until someone decides it is harmless.
fn is_permitted_under_allow_list(token: &str) -> bool {
    let name = token.split_once('=').map_or(token, |(name, _)| name);
    matches!(
        name,
        "--model"
            | "--fallback-model"
            | "--effort"
            | "--name"
            | "-n"
            | "--verbose"
            | "--debug"
            | "-d"
            // `--debug-file` is NOT here (review L5): it writes to an
            // arbitrary path. `--ide` and the `*-system-prompt-file` flags
            // stay: they only READ a prompt file or attach to the IDE, and
            // grant no tool.
            | "--output-format"
            | "--input-format"
            | "--include-partial-messages"
            | "--replay-user-messages"
            | "--max-turns"
            | "--append-system-prompt"
            | "--append-system-prompt-file"
            | "--system-prompt"
            | "--system-prompt-file"
            | "--exclude-dynamic-system-prompt-sections"
            | "--ide"
    )
}

/// A template flag's NAME for a log line: the part before an attached `=value`
/// (review L2). A value can be a path or a secret, and never reaches a log.
fn flag_name_only(token: &str) -> &str {
    token.split_once('=').map_or(token, |(name, _)| name)
}

/// Template flags an [`PermissionMode::AllowList`] spec owns, in either the
/// spaced or the `=`-attached spelling.
fn is_allow_list_owned_flag(token: &str) -> bool {
    let name = token.split_once('=').map_or(token, |(name, _)| name);
    matches!(
        name,
        "--allowedTools"
            | "--allowed-tools"
            | "--allow-dangerously-skip-permissions"
            // Each of these can pre-approve or proxy tools past the list: a
            // settings layer's `permissions.allow`, a prompt tool that answers
            // "allow", an MCP config that adds tools.
            | "--settings"
            | "--permission-prompt-tool"
            | "--mcp-config"
            | "--strict-mcp-config"
    )
}

/// Does the caller's `extra_required` carry an append prompt flag ahead of its
/// `--` terminator? Tokens after the terminator are the positional prompt, and a
/// prompt that happens to begin with `--append-system-prompt` is not a flag.
fn provides_append_prompt_flag(extra: &[String]) -> bool {
    extra
        .iter()
        .take_while(|t| t.as_str() != "--")
        .any(|t| is_append_prompt_flag(t))
}

/// Flag names (`--foo`) present in the caller's `extra_required`, used to keep a
/// template from duplicating a flag the spec already provides.
fn provided_flag_names(extra: &[String]) -> HashSet<String> {
    extra
        .iter()
        .filter(|t| t.len() > 2 && t.starts_with("--"))
        .cloned()
        .collect()
}

/// Parse a token slice into flag units. A token starting with `-` opens a unit
/// and consumes following non-`-` tokens as its values; a leading non-flag token
/// is a value-only (positional) unit.
fn parse_units(tokens: &[String]) -> Vec<FlagUnit> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok.starts_with('-') {
            let mut unit = FlagUnit {
                name: tok.clone(),
                values: Vec::new(),
            };
            i += 1;
            while i < tokens.len() && !tokens[i].starts_with('-') {
                unit.values.push(tokens[i].clone());
                i += 1;
            }
            units.push(unit);
        } else {
            units.push(FlagUnit {
                name: tok.clone(),
                values: Vec::new(),
            });
            i += 1;
        }
    }
    units
}

/// Minimal POSIX-ish shell tokenizer: whitespace-splits, honoring single and
/// double quote grouping (quotes are removed; an empty quoted string yields an
/// empty token). Sufficient for operator launch templates.
fn shell_tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut in_single = false;
    let mut in_double = false;

    for c in s.chars() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
        } else if in_double {
            if c == '"' {
                in_double = false;
            } else {
                cur.push(c);
            }
        } else if c == '\'' {
            in_single = true;
            started = true;
        } else if c == '"' {
            in_double = true;
            started = true;
        } else if c.is_whitespace() {
            if started || !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                started = false;
            }
        } else {
            cur.push(c);
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Quote a token for re-injection into a PTY command string when it contains
/// characters the shell would otherwise split or interpret. Uses literal
/// single-quoting for both PowerShell and POSIX (differing only in the escape of
/// an embedded quote).
/// The one check for a path the runner interpolates inside double quotes in a
/// typed shell command (`CLAUDE_CONFIG_DIR="<dir>"`), used by both the launch
/// and the resume renderer (review N5). Refuses anything that could close the
/// quote or expand inside it on POSIX shells or PowerShell: `"`, backtick,
/// `$`, `!` (history expansion), the Unicode quotes PowerShell treats as
/// quotes (U+2018–U+201E), control characters, and a trailing backslash
/// (which would escape the closing quote).
pub fn check_shell_safe_path(path: &str) -> Result<(), String> {
    let bad = path.chars().find(|c| {
        matches!(c, '"' | '`' | '$' | '!')
            || ('\u{2018}'..='\u{201E}').contains(c)
            || c.is_control()
    });
    if let Some(c) = bad {
        return Err(format!(
            "refusing to type a command with an unsafe character ({c:?}) in the path {path:?}"
        ));
    }
    if path.ends_with('\\') {
        return Err(format!(
            "refusing to type a command with a trailing backslash in the path {path:?}"
        ));
    }
    Ok(())
}

/// Characters PowerShell reads as a single quote inside a single-quoted
/// literal: ASCII `'` and U+2018–U+201B. Each is escaped by doubling it.
const POWERSHELL_SINGLE_QUOTES: &[char] = &['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

fn shell_quote(tok: &str, is_windows: bool) -> String {
    let safe = !tok.is_empty()
        && tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c));
    if safe {
        return tok.to_string();
    }
    if is_windows {
        // PowerShell single-quote literal: every character PowerShell reads as
        // a single quote — ASCII and the Unicode ones — is doubled (review N5).
        let mut out = String::with_capacity(tok.len() + 2);
        out.push('\'');
        for c in tok.chars() {
            if POWERSHELL_SINGLE_QUOTES.contains(&c) {
                out.push(c);
            }
            out.push(c);
        }
        out.push('\'');
        out
    } else {
        // POSIX single-quote literal: embedded ' is closed, escaped, reopened.
        format!("'{}'", tok.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            permission: PermissionMode::BypassPermissions,
            ..Default::default()
        }
    }

    fn tmpl(t: &str) -> LaunchConfig {
        LaunchConfig {
            default_template: Some(t.to_string()),
            account_command: None,
            ..Default::default()
        }
    }

    /// Locate the value following `flag` in an argv slice.
    fn value_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn permission_caller_wins_over_conflicting_template() {
        // Template asks for acceptEdits; caller mandates bypass. Caller wins and
        // the template permission flag is dropped entirely.
        let argv = render_argv(
            &spec(),
            &tmpl("claude --permission-mode acceptEdits"),
            "claude",
        );
        assert_eq!(
            value_after(&argv, "--permission-mode"),
            Some("bypassPermissions")
        );
        assert!(!argv.iter().any(|a| a == "acceptEdits"));
        // Exactly one permission-mode flag.
        assert_eq!(argv.iter().filter(|a| *a == "--permission-mode").count(), 1);
    }

    fn allow_list(tools: &[&str]) -> PermissionMode {
        PermissionMode::AllowList {
            tools: tools.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn allow_list_renders_dont_ask_and_the_tool_list() {
        let mut s = spec();
        s.permission = allow_list(&["Read", " Bash(git status) ", ""]);
        s.extra_required = vec!["--".to_string(), "do it".to_string()];
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(
            argv,
            vec![
                "claude",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                "Read",
                "Bash(git status)",
                "--",
                "do it"
            ]
        );
        assert!(!argv
            .iter()
            .any(|a| a.contains("bypass") || a.contains("dangerously")));
    }

    /// Review M1: a caller-supplied `--allowedTools` (a workflow node's tool
    /// policy) is INTERSECTED with the allow-list, never appended to it.
    #[test]
    fn allow_list_intersects_caller_allowed_tools() {
        let mut s = spec();
        s.permission = allow_list(&["Read", "Edit"]);
        s.extra_required = vec![
            "--allowedTools".to_string(),
            "Read".to_string(),
            "Bash".to_string(),
            "--".to_string(),
            "go".to_string(),
        ];
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(
            argv.iter().filter(|a| *a == "--allowedTools").count(),
            1,
            "{argv:?}"
        );
        assert_eq!(value_after(&argv, "--allowedTools"), Some("Read"));
        assert!(!argv.iter().any(|a| a == "Bash" || a == "Edit"), "{argv:?}");
        assert_eq!(&argv[argv.len() - 2..], ["--", "go"]);
    }

    /// Review M1: a caller tool list disjoint from the allow-list leaves the
    /// session with NO pre-approved tools rather than the caller's.
    #[test]
    fn allow_list_disjoint_caller_tools_pre_approve_nothing() {
        let mut s = spec();
        s.permission = allow_list(&["Read"]);
        s.extra_required = vec!["--allowedTools".to_string(), "Bash".to_string()];
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(argv, vec!["claude", "--permission-mode", "dontAsk"]);
    }

    /// Review M1: under an allow-list, a template may not carry its own
    /// settings layer, permission-prompt tool or MCP config — each could
    /// pre-approve or proxy tools past the list.
    #[test]
    fn allow_list_drops_template_settings_prompt_tool_and_mcp_config() {
        let mut s = spec();
        s.permission = allow_list(&["Read"]);
        let argv = render_argv(
            &s,
            &tmpl(
                "claude --settings /t/s.json --permission-prompt-tool mcp__x__y \
                 --mcp-config /t/m.json --settings=/t/b.json --model opus",
            ),
            "claude",
        );
        for gone in [
            "--settings",
            "/t/s.json",
            "--permission-prompt-tool",
            "mcp__x__y",
            "--mcp-config",
            "/t/m.json",
            "--settings=/t/b.json",
        ] {
            assert!(!argv.iter().any(|a| a == gone), "{gone} leaked: {argv:?}");
        }
        assert_eq!(value_after(&argv, "--model"), Some("opus"));
    }

    /// Review L2: a dropped flag is logged by name only, never its value.
    #[test]
    fn flag_name_only_strips_an_attached_value() {
        assert_eq!(flag_name_only("--debug-file=/secret/path"), "--debug-file");
        assert_eq!(flag_name_only("--settings={\"k\":\"v\"}"), "--settings");
        assert_eq!(flag_name_only("--verbose"), "--verbose");
    }

    /// Review N4: under an allow-list only a known-harmless set of template
    /// flags survives — `--plugin-dir` (plugins carry PreToolUse allow hooks)
    /// and any flag the runner does not know are dropped.
    #[test]
    fn allow_list_keeps_only_permitted_template_flags() {
        let mut s = spec();
        s.permission = allow_list(&["Read"]);
        let argv = render_argv(
            &s,
            &tmpl(
                "claude --plugin-dir /p --brand-new-flag x --model opus --verbose \
                 --add-dir /elsewhere",
            ),
            "claude",
        );
        for gone in [
            "--plugin-dir",
            "/p",
            "--brand-new-flag",
            "x",
            "--add-dir",
            "/elsewhere",
        ] {
            assert!(!argv.iter().any(|a| a == gone), "{gone} leaked: {argv:?}");
        }
        assert_eq!(value_after(&argv, "--model"), Some("opus"));
        assert!(argv.iter().any(|a| a == "--verbose"));
        // Review L5: `--debug-file` writes to an arbitrary path, so it is not
        // harmless under an allow-list, in either spelling.
        let argv = render_argv(
            &s,
            &tmpl("claude --debug-file /tmp/d.log --debug-file=/tmp/e.log --verbose"),
            "claude",
        );
        assert!(
            !argv
                .iter()
                .any(|a| a.starts_with("--debug-file") || a.ends_with(".log")),
            "{argv:?}"
        );
        // Outside an allow-list the same template flags still layer in.
        let argv = render_argv(&spec(), &tmpl("claude --plugin-dir /p"), "claude");
        assert_eq!(value_after(&argv, "--plugin-dir"), Some("/p"));
    }

    /// Review N5: one shared refusal for paths interpolated into a typed
    /// command — smart quotes, `!`, a trailing backslash and the existing set.
    #[test]
    fn shell_safe_path_check_refuses_quote_breakers() {
        for bad in [
            "/x\u{2018}y",
            "/x\u{2019}y",
            "/x\u{201C}y",
            "/x\u{201D}y",
            "/x\u{201E}y",
            "/x!y",
            "C:\\claude\\",
            "/x\"y",
            "/x$y",
            "/x`y",
            "/x\ny",
        ] {
            assert!(check_shell_safe_path(bad).is_err(), "{bad:?} accepted");
        }
        for good in [
            "/home/u/.claude",
            "C:\\Users\\a b\\.claude-x",
            "C:/claude/acct (2)",
        ] {
            assert!(check_shell_safe_path(good).is_ok(), "{good:?} refused");
        }
    }

    /// Review N5: PowerShell treats U+2018-201B as single quotes too, so the
    /// single-quoted literal doubles them like `'`.
    #[test]
    fn powershell_quote_doubles_unicode_single_quotes() {
        assert_eq!(shell_quote("a\u{2019}b", true), "'a\u{2019}\u{2019}b'");
        assert_eq!(shell_quote("a\u{2018}b'c", true), "'a\u{2018}\u{2018}b''c'");
    }

    /// Review M1: an opaque account alias cannot be introspected, so under an
    /// allow-list it is never typed; the composed command is used instead.
    #[test]
    fn allow_list_refuses_an_opaque_account_alias() {
        let cfg = LaunchConfig {
            account_command: Some("clg".to_string()),
            session_permission: SessionPermissionSetting::AllowList {
                tools: vec!["Read".to_string()],
            },
            ..Default::default()
        };
        let cmd = render_pty_command(&spec(), &cfg, false);
        assert_ne!(cmd, "clg");
        assert!(
            cmd.starts_with(
                "claude --teammate-mode in-process --permission-mode dontAsk --allowedTools Read"
            ),
            "{cmd}"
        );
    }

    #[test]
    fn allow_list_with_no_tools_renders_no_allowed_tools_flag() {
        let mut s = spec();
        s.permission = allow_list(&[" ", ""]);
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(argv, vec!["claude", "--permission-mode", "dontAsk"]);
    }

    /// Rendered from the spec, never from a template: a template's permission
    /// flags, its own tool list and the bypass opt-in are all dropped.
    #[test]
    fn allow_list_drops_template_permission_and_tool_flags() {
        let mut s = spec();
        s.permission = allow_list(&["Read"]);
        let argv = render_argv(
            &s,
            &tmpl(
                "claude --permission-mode=bypassPermissions --allowedTools Bash Write \
                 --allow-dangerously-skip-permissions --allowed-tools=Edit --model opus",
            ),
            "claude",
        );
        assert_eq!(argv.iter().filter(|a| *a == "--permission-mode").count(), 1);
        assert_eq!(value_after(&argv, "--permission-mode"), Some("dontAsk"));
        assert_eq!(argv.iter().filter(|a| *a == "--allowedTools").count(), 1);
        for gone in [
            "Bash",
            "Write",
            "--allow-dangerously-skip-permissions",
            "--allowed-tools=Edit",
            "--permission-mode=bypassPermissions",
        ] {
            assert!(!argv.iter().any(|a| a == gone), "{gone} leaked: {argv:?}");
        }
        // Unrelated template flags still layer in.
        assert_eq!(value_after(&argv, "--model"), Some("opus"));
    }

    /// The same template flags are kept for a non-allow-list spec (only the
    /// permission flag itself is spec-owned there).
    #[test]
    fn template_tool_list_kept_under_bypass() {
        let argv = render_argv(&spec(), &tmpl("claude --allowedTools Bash"), "claude");
        assert_eq!(value_after(&argv, "--allowedTools"), Some("Bash"));
    }

    #[test]
    fn operator_allow_list_posture_replaces_every_site_default() {
        let cfg = LaunchConfig {
            session_permission: SessionPermissionSetting::AllowList {
                tools: vec!["Read".to_string()],
            },
            ..Default::default()
        };
        for site in [
            PermissionMode::BypassPermissions,
            PermissionMode::DangerouslySkip,
        ] {
            let mut s = spec();
            s.permission = site;
            let argv = render_argv(&s, &cfg, "claude");
            assert_eq!(
                &argv[1..5],
                ["--permission-mode", "dontAsk", "--allowedTools", "Read"]
            );
            assert!(!argv
                .iter()
                .any(|a| a.contains("dangerously") || a == "bypassPermissions"));
        }
        // The PTY renderer composes the same flags.
        let cmd = render_pty_command(&spec(), &cfg, false);
        assert!(
            cmd.starts_with(
                "claude --teammate-mode in-process --permission-mode dontAsk --allowedTools Read"
            ),
            "{cmd}"
        );
    }

    #[test]
    fn site_default_posture_keeps_the_spec_mode() {
        let mut s = spec();
        s.permission = PermissionMode::DangerouslySkip;
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(argv, vec!["claude", "--dangerously-skip-permissions"]);
    }

    #[test]
    fn session_permission_setting_serde_and_normalization() {
        let v: SessionPermissionSetting =
            serde_json::from_str(r#"{"mode":"site_default"}"#).unwrap();
        assert_eq!(v, SessionPermissionSetting::SiteDefault);
        let v: SessionPermissionSetting =
            serde_json::from_str(r#"{"mode":"allow_list","tools":[" Read ","","Read","Edit"]}"#)
                .unwrap();
        assert_eq!(
            v.normalized().unwrap(),
            SessionPermissionSetting::AllowList {
                tools: vec!["Read".to_string(), "Edit".to_string()]
            }
        );
        let bad = SessionPermissionSetting::AllowList {
            tools: vec!["--dangerously-skip-permissions".to_string()],
        };
        assert!(bad.normalized().is_err());
        assert_eq!(
            serde_json::to_string(&SessionPermissionSetting::default()).unwrap(),
            r#"{"mode":"site_default"}"#
        );
    }

    #[test]
    fn allowed_tools_args_is_the_shared_renderer() {
        assert!(allowed_tools_args::<&str>(&[]).is_empty());
        assert!(allowed_tools_args(&["  ", ""]).is_empty());
        assert_eq!(
            allowed_tools_args(&[" Bash ", "Read"]),
            vec!["--allowedTools", "Bash", "Read"]
        );
    }

    #[test]
    fn dangerously_skip_never_dropped_by_template() {
        let mut s = spec();
        s.permission = PermissionMode::DangerouslySkip;
        let argv = render_argv(&s, &tmpl("claude --permission-mode acceptEdits"), "claude");
        assert!(argv.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!argv.iter().any(|a| a == "--permission-mode"));
    }

    #[test]
    fn spec_model_beats_template_model() {
        let mut s = spec();
        s.model = Some("opus".to_string());
        let argv = render_argv(&s, &tmpl("claude --model sonnet"), "claude");
        assert_eq!(value_after(&argv, "--model"), Some("opus"));
        assert!(!argv.iter().any(|a| a == "sonnet"));
        assert_eq!(argv.iter().filter(|a| *a == "--model").count(), 1);
    }

    #[test]
    fn template_model_used_when_spec_none() {
        let argv = render_argv(&spec(), &tmpl("claude --model sonnet"), "claude");
        assert_eq!(value_after(&argv, "--model"), Some("sonnet"));
    }

    #[test]
    fn session_placeholder_substituted_not_appended() {
        let mut s = spec();
        s.session_id = Some("abc-123".to_string());
        let argv = render_argv(&s, &tmpl("claude --session-id {sessionId}"), "claude");
        assert_eq!(value_after(&argv, "--session-id"), Some("abc-123"));
        // Substituted in place, never doubled.
        assert_eq!(argv.iter().filter(|a| *a == "--session-id").count(), 1);
    }

    #[test]
    fn session_appended_when_no_placeholder() {
        let mut s = spec();
        s.session_id = Some("abc-123".to_string());
        let argv = render_argv(
            &s,
            &tmpl("claude --permission-mode bypassPermissions"),
            "claude",
        );
        assert_eq!(value_after(&argv, "--session-id"), Some("abc-123"));
    }

    #[test]
    fn name_absent_emits_no_name_flag() {
        let argv = render_argv(&spec(), &LaunchConfig::default(), "claude");
        assert!(!argv.iter().any(|a| a == "--name"));
    }

    #[test]
    fn name_rendered_as_separate_token_pair_before_extra_required() {
        let mut s = spec();
        s.session_id = Some("sid".to_string());
        s.name = Some("post-merge-runner#1863".to_string());
        s.extra_required = vec!["--".to_string(), "do it".to_string()];
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(value_after(&argv, "--name"), Some("post-merge-runner#1863"));
        let n = argv.iter().position(|a| a == "--name").unwrap();
        let dd = argv.iter().position(|a| a == "--").unwrap();
        assert!(n < dd, "--name must precede the `--` terminator: {argv:?}");
    }

    #[test]
    fn name_with_spaces_quotes_and_leading_dash_is_one_safe_token() {
        let mut s = spec();
        s.name = Some("  -\"--evil\"   it's\nfine ".to_string());
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        let v = value_after(&argv, "--name").unwrap();
        assert!(!v.starts_with('-'), "leading dash must be stripped: {v:?}");
        assert_eq!(v, "\"--evil\" it's fine");
        assert_eq!(argv.iter().filter(|a| *a == "--name").count(), 1);
    }

    #[test]
    fn name_with_interleaved_dashes_and_blanks_never_starts_with_a_dash() {
        for raw in ["- -x", "-\u{7}-x", "- - - y", "\t-  -\n-z"] {
            let got = sanitize_session_name(raw).unwrap();
            assert!(!got.starts_with('-'), "{raw:?} -> {got:?}");
            assert!(!got.starts_with(char::is_whitespace), "{raw:?} -> {got:?}");
        }
    }

    #[test]
    fn name_that_sanitises_to_nothing_is_omitted() {
        let mut s = spec();
        s.name = Some(" \n\t --- ".to_string());
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert!(!argv.iter().any(|a| a == "--name"));
    }

    #[test]
    fn spec_name_drops_template_name_in_every_spelling() {
        for t in [
            "claude --name old",
            "claude -n old",
            "claude --name=old",
            "claude --name old --model sonnet",
        ] {
            let mut s = spec();
            s.name = Some("new".to_string());
            let argv = render_argv(&s, &tmpl(t), "claude");
            assert_eq!(value_after(&argv, "--name"), Some("new"), "{t}");
            assert!(
                !argv.iter().any(|a| a.contains("old") || a == "-n"),
                "{t}: {argv:?}"
            );
        }
    }

    #[test]
    fn template_name_kept_when_spec_has_none() {
        let argv = render_argv(&spec(), &tmpl("claude --name old"), "claude");
        assert_eq!(value_after(&argv, "--name"), Some("old"));
    }

    #[test]
    fn prompted_launch_keeps_the_template_name() {
        let mut s = spec();
        s.permission = PermissionMode::Prompt;
        let argv = render_argv(&s, &tmpl("claude --name old"), "claude");
        assert_eq!(value_after(&argv, "--name"), Some("old"));
    }

    #[test]
    fn sanitize_session_name_strips_collapses_and_caps() {
        assert_eq!(
            sanitize_session_name("a\u{7}b\r\nc   d"),
            Some("a b c d".to_string())
        );
        assert_eq!(sanitize_session_name("---x"), Some("x".to_string()));
        assert_eq!(sanitize_session_name("   "), None);
        let long = "x".repeat(100);
        assert_eq!(
            sanitize_session_name(&long).unwrap().chars().count(),
            MAX_SESSION_NAME_CHARS
        );
        let multi = "é".repeat(100);
        assert_eq!(
            sanitize_session_name(&multi).unwrap().chars().count(),
            MAX_SESSION_NAME_CHARS
        );
    }

    // Plan `2026-11-09-gate-continuations-get-a-display-truncated-name-so-the-
    // visible-hand-off-refuses-them`, D5 / Phase 3. Every expected string below
    // was computed from an independent model of the rule, never from this
    // function's output.

    /// Every name coord authors fits the cap, so the runner hands it on byte for
    /// byte: `--name`, the session-names file and the tab carry coord's string.
    #[test]
    fn sanitize_session_name_passes_every_coord_spawn_name_through_byte_identical() {
        for name in [
            "resume-terminal-titlebar-shows-f5782ab5",
            "impl-terminal-titlebar-shows-694ae308",
            "post-merge-a-very-long-8295759e#12345",
            "fix-an-extraordinarily-ea25dfe5#123456",
            "shepherd-an-ea25dfe5#123456",
            "review-an-ea25dfe5#123456",
            "red-main-an-extraordinarily-ea25dfe5",
            "respond-red-main-an-ea25dfe5",
            "respond-stall-an-ea25dfe5",
            "resume-unit-9e9b1b7b-phase-3-5-c2abbf7c",
            "resume-finding-b31b377d",
            "resume-terminal-titlebar-shows-44b98580",
        ] {
            assert_eq!(sanitize_session_name(name).as_deref(), Some(name));
        }
    }

    /// Enters where every runner spawn does: a `LaunchSpec.name` turned into
    /// argv by `compose_flags`, the seam `render_argv` and `render_pty_command`
    /// share. A long runner-own name reaches `--name` compacted; a coord name
    /// reaches it byte-identical.
    #[test]
    fn compose_flags_compacts_an_overlong_session_name() {
        let mut s = spec();
        for (name, want) in [
            (
                "loop-nightly-merge-train-steward-watch-and-remediate",
                "loop-nightly-merge-train-2c0ac670",
            ),
            (
                "resume-unit-9e9b1b7b-phase-3-5-c2abbf7c",
                "resume-unit-9e9b1b7b-phase-3-5-c2abbf7c",
            ),
        ] {
            s.name = Some(name.to_string());
            let flags = compose_flags(&s, &LaunchConfig::default());
            assert_eq!(value_after(&flags, "--name"), Some(want), "{flags:?}");
            assert_eq!(flags.iter().filter(|a| *a == "--name").count(), 1);
        }
    }

    #[test]
    fn sanitize_session_name_compacts_an_overlong_value_with_a_digest() {
        let cases = [
            ("x".repeat(100), format!("{}-09ecb6eb", "x".repeat(31))),
            ("é".repeat(100), format!("{}-f42ec48e", "é".repeat(31))),
            // `loop-nightly-merge-train-steward` is 32 > 31 chars: `-steward` goes.
            (
                "loop-nightly-merge-train-steward-watch-and-remediate".to_string(),
                "loop-nightly-merge-train-2c0ac670".to_string(),
            ),
            // The cap is inclusive: 40 is kept whole, 41 is compacted.
            ("a".repeat(40), "a".repeat(40)),
            ("a".repeat(41), format!("{}-c0f8bd4d", "a".repeat(31))),
            // A first token that alone overflows is cut by chars to 31.
            (
                format!("{}-tail-tokens", "x".repeat(35)),
                format!("{}-15d13ceb", "x".repeat(31)),
            ),
            // Separators after the last kept token are dropped, not doubled.
            (
                format!("fix this - {}", "y".repeat(40)),
                "fix this-bd4a7d26".to_string(),
            ),
            // A space splits tokens too, and stays verbatim inside the head.
            (
                "check-login page renders the dashboard with all widgets".to_string(),
                "check-login page renders the-d7458ca1".to_string(),
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(sanitize_session_name(&raw), Some(want), "{raw:?}");
        }
    }

    /// The digest covers the CLEANED value: leading dashes, control chars and
    /// whitespace runs are normalised first, exactly as for a short name.
    #[test]
    fn sanitize_session_name_digests_the_cleaned_value() {
        assert_eq!(
            sanitize_session_name("  --loop-nightly-merge-train-steward-watch-and-remediate"),
            Some("loop-nightly-merge-train-2c0ac670".to_string())
        );
        assert_eq!(
            sanitize_session_name("loop-nightly\tmerge   train\u{7}steward watch-and-remediate"),
            Some("loop-nightly merge train-3a0b0f5d".to_string())
        );
    }

    /// The silent cut this replaces gave both of these
    /// `loop-nightly-merge-train-steward-watch-a`.
    #[test]
    fn sanitize_session_name_keeps_names_that_differ_past_the_cut_distinct() {
        let remediate =
            sanitize_session_name("loop-nightly-merge-train-steward-watch-and-remediate");
        let report = sanitize_session_name("loop-nightly-merge-train-steward-watch-and-report");
        assert_eq!(report.as_deref(), Some("loop-nightly-merge-train-51629b35"));
        assert_ne!(remediate, report);
    }

    /// Fixed-seed xorshift64, so a failing sweep input replays exactly.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// The first property the sweep's inputs violate, or `None`. A function of
    /// the sanitiser so the sweep can be shown to REJECT a `…`-marked cut.
    fn session_name_sweep_violation(
        sanitize: fn(&str) -> Option<String>,
        inputs: &[String],
    ) -> Option<String> {
        for raw in inputs {
            let Some(out) = sanitize(raw) else { continue };
            if out.contains('\u{2026}') {
                return Some(format!("{raw:?} -> {out:?} carries U+2026"));
            }
            if out.chars().count() > MAX_SESSION_NAME_CHARS {
                return Some(format!("{raw:?} -> {out:?} exceeds the cap"));
            }
            if out.starts_with('-') || out.starts_with(char::is_whitespace) {
                return Some(format!("{raw:?} -> {out:?} starts with a dash or blank"));
            }
            // `compose_flags` sanitises a call site's already-sanitised name
            // again, so `--name` matches the tab and the file only at a fixed point.
            if sanitize(&out).as_deref() != Some(out.as_str()) {
                return Some(format!("{raw:?} -> {out:?} is not a fixed point"));
            }
        }
        None
    }

    #[test]
    fn sanitize_session_name_never_emits_an_ellipsis() {
        // No piece carries `…`, so any in an output was introduced by the
        // function; the dirty pieces exercise the cleanup ahead of the cap.
        let long: &str = &"x".repeat(33);
        let mut pieces: Vec<&str> = "a bb unit 9e9b1b7b é 日本語 🦀 #12345".split(' ').collect();
        pieces.extend(["-", "--", " ", "   ", "\t", "\r\n", "\u{7}", long]);
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let inputs: Vec<String> = (0..500)
            .map(|_| {
                let len = 41 + (xorshift(&mut seed) % 160) as usize;
                let mut raw = String::new();
                while raw.chars().count() < len {
                    raw.push_str(pieces[(xorshift(&mut seed) % pieces.len() as u64) as usize]);
                }
                raw
            })
            .collect();
        assert_eq!(
            session_name_sweep_violation(sanitize_session_name, &inputs),
            None
        );
        // Negative control: the sweep catches the `…` cut coord used to author.
        fn cut_with_ellipsis(raw: &str) -> Option<String> {
            let kept: String = raw.chars().take(MAX_SESSION_NAME_CHARS - 1).collect();
            Some(format!("{kept}\u{2026}"))
        }
        let caught = session_name_sweep_violation(cut_with_ellipsis, &inputs).unwrap_or_default();
        assert!(caught.ends_with("carries U+2026"), "{caught:?}");
    }

    /// Clean long inputs (tokens joined by single `-`/` `), so the cleaned value
    /// IS the input and the compacted form can be checked exactly.
    #[test]
    fn sanitize_session_name_compacts_to_whole_leading_tokens_and_the_digest() {
        use sha2::{Digest, Sha256};
        let is_sep = |c: char| c == '-' || c == ' ';
        let long: &str = &"y".repeat(34);
        let mut tokens: Vec<&str> = "a bb loop nightly 9e9b1b7b é 日本語 🦀 #12345"
            .split(' ')
            .collect();
        tokens.push(long);
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..500 {
            let mut raw = String::new();
            loop {
                raw.push_str(tokens[(xorshift(&mut seed) % tokens.len() as u64) as usize]);
                if raw.chars().count() > MAX_SESSION_NAME_CHARS {
                    break;
                }
                raw.push(['-', ' '][(xorshift(&mut seed) % 2) as usize]);
            }
            let out = sanitize_session_name(&raw).unwrap();
            let (head, digest) = out.rsplit_once('-').unwrap();
            let want_digest = hex::encode(&Sha256::digest(raw.as_bytes())[..4]);
            assert_eq!(digest, want_digest, "{raw:?} -> {out:?}");
            let Some(rest) = raw.strip_prefix(head) else {
                panic!("{raw:?} -> {out:?}: the head is not a prefix of the input");
            };
            let kept = head.chars().count();
            if rest.starts_with(is_sep) {
                // Whole tokens up to a token end; the next whole token does not fit.
                let seps = rest.chars().take_while(|&c| is_sep(c)).count();
                let next = rest.chars().skip(seps).take_while(|&c| !is_sep(c)).count();
                assert!(
                    !head.ends_with(is_sep) && kept <= 31 && kept + seps + next > 31,
                    "{raw:?} -> {out:?}"
                );
            } else {
                // The first token alone overflows: cut by chars to 31.
                assert!(!head.contains(is_sep) && kept == 31, "{raw:?} -> {out:?}");
            }
        }
    }

    #[test]
    fn resume_takes_precedence_over_session_id() {
        let mut s = spec();
        s.session_id = Some("sid".to_string());
        s.resume_id = Some("rid".to_string());
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        assert_eq!(value_after(&argv, "--resume"), Some("rid"));
        assert!(!argv.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn blank_template_yields_builtin_default() {
        let argv = render_argv(&spec(), &LaunchConfig::default(), "claude");
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--permission-mode".to_string(),
                "bypassPermissions".to_string()
            ]
        );
    }

    #[test]
    fn whitespace_only_template_yields_builtin_default() {
        let argv = render_argv(&spec(), &tmpl("   "), "claude");
        assert_eq!(
            argv,
            vec![
                "claude".to_string(),
                "--permission-mode".to_string(),
                "bypassPermissions".to_string()
            ]
        );
    }

    #[test]
    fn config_dir_prefix_windows() {
        let mut s = spec();
        s.config_dir = Some("C:\\cfg\\gmail".to_string());
        let cmd = render_pty_command(&s, &LaunchConfig::default(), true);
        assert!(cmd.starts_with("$env:CLAUDE_CONFIG_DIR=\"C:\\cfg\\gmail\"; claude "));
    }

    #[test]
    fn config_dir_prefix_posix() {
        let mut s = spec();
        s.config_dir = Some("/home/x/.cfg".to_string());
        let cmd = render_pty_command(&s, &LaunchConfig::default(), false);
        assert!(cmd.starts_with("CLAUDE_CONFIG_DIR=\"/home/x/.cfg\" claude "));
    }

    #[test]
    fn config_dir_absent_no_prefix() {
        let cmd = render_pty_command(&spec(), &LaunchConfig::default(), false);
        assert!(cmd.starts_with("claude "));
        assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn argv_pty_flag_parity() {
        let mut s = spec();
        s.session_id = Some("sid".to_string());
        s.model = Some("opus".to_string());
        let cfg = tmpl("claude --output-format stream-json");
        let argv = render_argv(&s, &cfg, "claude");
        let pty = render_pty_command(&s, &cfg, false);
        // PTY head is bare `claude`, then the profile's PTY-only args; the
        // remaining space-split tokens must equal argv[1..] (tokens here are
        // space-free, so split round-trips).
        let pty_tokens: Vec<String> = pty.split(' ').map(String::from).collect();
        let pty_only = pty_only_args(&s, &[]);
        assert_eq!(pty_tokens[0], "claude");
        assert_eq!(&pty_tokens[1..=pty_only.len()], &pty_only[..]);
        assert_eq!(&pty_tokens[pty_only.len() + 1..], &argv[1..]);
    }

    #[test]
    fn extra_required_order_preserved() {
        let mut s = spec();
        s.extra_required = vec![
            "--append-system-prompt".to_string(),
            "briefing".to_string(),
            "--add-dir".to_string(),
            "/d".to_string(),
            "--".to_string(),
            "the prompt".to_string(),
        ];
        let argv = render_argv(&s, &LaunchConfig::default(), "claude");
        // extra_required is the verbatim, in-order tail.
        assert_eq!(&argv[argv.len() - 6..], &s.extra_required[..]);
    }

    #[test]
    fn extra_required_beats_duplicate_template_flag() {
        let mut s = spec();
        s.extra_required = vec!["--add-dir".to_string(), "/spec".to_string()];
        let argv = render_argv(&s, &tmpl("claude --add-dir /tpl"), "claude");
        assert_eq!(value_after(&argv, "--add-dir"), Some("/spec"));
        assert!(!argv.iter().any(|a| a == "/tpl"));
        assert_eq!(argv.iter().filter(|a| *a == "--add-dir").count(), 1);
    }

    /// The two APPEND flags are ONE group: a template's append prompt (either
    /// flag, either spelling) next to the caller's composed-file carrier would
    /// make Claude Code refuse to start, so both template members are dropped
    /// while the template's unrelated flags still layer in.
    #[test]
    fn caller_append_carrier_drops_every_template_append_flag() {
        let mut s = spec();
        s.extra_required = vec![
            "--append-system-prompt-file".to_string(),
            "/rt/spawn-prompts/spawn-1.md".to_string(),
            "--".to_string(),
            "the prompt".to_string(),
        ];
        for template in [
            "claude --append-system-prompt \"be terse\" --output-format stream-json",
            "claude --append-system-prompt=terse --output-format stream-json",
            "claude --append-system-prompt-file /t/other.md --output-format stream-json",
            "claude --append-system-prompt-file=/t/other.md --output-format stream-json",
        ] {
            for argv in [
                render_argv(&s, &tmpl(template), "claude"),
                shell_tokenize(&render_pty_command(&s, &tmpl(template), false)),
            ] {
                let append_flags: Vec<&String> = argv
                    .iter()
                    .take_while(|a| a.as_str() != "--")
                    .filter(|a| is_append_prompt_flag(a))
                    .collect();
                assert_eq!(
                    append_flags,
                    vec!["--append-system-prompt-file"],
                    "{template}: {argv:?}"
                );
                assert_eq!(
                    value_after(&argv, "--append-system-prompt-file"),
                    Some("/rt/spawn-prompts/spawn-1.md"),
                    "{template}"
                );
                assert!(!argv.iter().any(|a| a == "be terse" || a == "/t/other.md"));
                assert_eq!(
                    value_after(&argv, "--output-format"),
                    Some("stream-json"),
                    "{template}: unrelated template flags still layer in"
                );
            }
        }
    }

    /// A template REPLACEMENT prompt combines with the append carrier (probed
    /// against the Claude Code CLI in use when this landed), so it is no longer
    /// dropped.
    #[test]
    fn template_replacement_prompt_survives_a_caller_append_carrier() {
        let mut s = spec();
        s.extra_required = vec![
            "--append-system-prompt-file".to_string(),
            "/rt/spawn-prompts/spawn-1.md".to_string(),
        ];
        let argv = render_argv(&s, &tmpl("claude --system-prompt x"), "claude");
        assert_eq!(value_after(&argv, "--system-prompt"), Some("x"));
        let argv = render_argv(&s, &tmpl("claude --system-prompt-file=/t/sp.md"), "claude");
        assert!(
            argv.iter().any(|a| a == "--system-prompt-file=/t/sp.md"),
            "{argv:?}"
        );
        assert_eq!(
            value_after(&argv, "--append-system-prompt-file"),
            Some("/rt/spawn-prompts/spawn-1.md")
        );
    }

    /// With no caller carrier the template's own append prompt is untouched,
    /// and a positional prompt that merely LOOKS like the flag does not count.
    #[test]
    fn template_append_prompt_kept_without_a_caller_carrier() {
        let argv = render_argv(
            &spec(),
            &tmpl("claude --append-system-prompt terse"),
            "claude",
        );
        assert_eq!(value_after(&argv, "--append-system-prompt"), Some("terse"));

        let mut s = spec();
        s.extra_required = vec!["--".to_string(), "--append-system-prompt-file".to_string()];
        let argv = render_argv(&s, &tmpl("claude --append-system-prompt terse"), "claude");
        assert_eq!(value_after(&argv, "--append-system-prompt"), Some("terse"));
    }

    #[test]
    fn other_template_flags_layered_in() {
        let argv = render_argv(
            &spec(),
            &tmpl("claude --output-format stream-json"),
            "claude",
        );
        assert_eq!(value_after(&argv, "--output-format"), Some("stream-json"));
    }

    #[test]
    fn account_bare_alias_returned_verbatim_for_pty() {
        let cfg = LaunchConfig {
            default_template: Some("claude --permission-mode bypassPermissions".to_string()),
            account_command: Some("clg".to_string()),
            ..Default::default()
        };
        let mut s = spec();
        s.config_dir = Some("/home/x".to_string());
        // Opaque alias: verbatim, no env prefix, no pin.
        assert_eq!(render_pty_command(&s, &cfg, false), "clg");
    }

    #[test]
    fn account_claude_template_composed() {
        let cfg = LaunchConfig {
            default_template: Some("claude --model default".to_string()),
            account_command: Some("claude --model haiku".to_string()),
            ..Default::default()
        };
        let argv = render_argv(&spec(), &cfg, "claude");
        // Per-account claude command wins over the global default.
        assert_eq!(value_after(&argv, "--model"), Some("haiku"));
    }

    #[test]
    fn argv_bare_alias_account_falls_back_to_default() {
        let cfg = LaunchConfig {
            default_template: Some("claude --output-format stream-json".to_string()),
            account_command: Some("clg".to_string()),
            ..Default::default()
        };
        let argv = render_argv(&spec(), &cfg, "/abs/claude");
        assert_eq!(argv[0], "/abs/claude");
        // Alias ignored for argv; global default is layered in.
        assert_eq!(value_after(&argv, "--output-format"), Some("stream-json"));
    }

    #[test]
    fn argv_head_is_claude_bin() {
        let argv = render_argv(&spec(), &LaunchConfig::default(), "/opt/bin/claude");
        assert_eq!(argv[0], "/opt/bin/claude");
    }

    // ── render_program_and_argv: the cmd.exe platform branch ────────────────
    //
    // `/sessions/spawn` was unusable on Linux because the direct-spawn path
    // hardcoded `cmd.exe` (`No such file or directory (os error 2)`). These two
    // tests are the regression net: each asserts the arm for its own platform,
    // so neither can be satisfied by the other's behaviour.

    #[test]
    #[cfg(target_os = "windows")]
    fn program_and_argv_wraps_in_cmd_exe_on_windows() {
        let (program, args) = render_program_and_argv(&spec(), &LaunchConfig::default());
        assert_eq!(program, "cmd.exe");
        assert_eq!(args[0], "/c", "cmd.exe needs its /c switch first");
        assert_eq!(
            args[1], "claude",
            "the bare name is deliberate on Windows — cmd.exe resolves npm's claude.cmd shim"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn program_and_argv_execs_claude_directly_off_windows() {
        // Resolves PATH twice below; a test mutating PATH between them would
        // make the two answers differ.
        let _env_lock = crate::test_env::env_lock();
        let (program, args) = render_program_and_argv(&spec(), &LaunchConfig::default());

        assert_ne!(program, "cmd.exe", "cmd.exe does not exist off Windows");
        // Compare against the resolver itself rather than hardcoding a "claude"
        // suffix: `QONTINUI_CLAUDE_BIN` can legitimately point anywhere (a dev
        // box or CI runner may export it), and a literal suffix assertion would
        // fail there for reasons unrelated to the regression this guards.
        assert_eq!(
            program,
            crate::agent_runtime::resolve_claude_bin(),
            "program should be exactly the resolved claude binary"
        );
        assert!(
            !args.iter().any(|a| a == "/c"),
            "/c is a cmd.exe switch and must not survive off Windows: {args:?}"
        );
        // The doubling regression: pairing a resolved program with an argv that
        // still begins with "claude" execs `<resolved> claude --flags`, handing
        // the CLI its own name as a first positional (i.e. as a prompt). A
        // program-only assertion cannot see this.
        assert_ne!(
            args.first().map(String::as_str),
            Some("claude"),
            "argv[0] must be consumed as the program, not repeated as an arg: {args:?}"
        );
    }

    /// The Claude profile's PTY-only args (`--teammate-mode in-process`, Phase 2
    /// probe Q4) lead every PTY launch, never the argv one, and never twice.
    #[test]
    fn pty_launch_carries_the_profile_pty_args_once() {
        let pty = shell_tokenize(&render_pty_command(
            &spec(),
            &LaunchConfig::default(),
            false,
        ));
        assert_eq!(&pty[..3], ["claude", "--teammate-mode", "in-process"]);
        let argv = render_argv(&spec(), &LaunchConfig::default(), "claude");
        assert!(!argv.iter().any(|a| a == "--teammate-mode"));

        for template in ["claude --teammate-mode tmux", "claude --teammate-mode=tmux"] {
            let pty = shell_tokenize(&render_pty_command(&spec(), &tmpl(template), false));
            assert_eq!(
                pty.iter()
                    .filter(|a| a.starts_with("--teammate-mode"))
                    .count(),
                1,
                "an operator's own choice is kept, not doubled: {pty:?}"
            );
        }
    }

    #[test]
    fn pty_quotes_prompt_with_spaces() {
        let mut s = spec();
        s.extra_required = vec!["--".to_string(), "do the thing".to_string()];
        let cmd = render_pty_command(&s, &LaunchConfig::default(), false);
        // `--` is safe (unquoted); the multi-word prompt is single-quoted so it
        // re-parses as one token.
        assert!(cmd.contains(" -- 'do the thing'"), "got: {cmd}");
    }

    #[test]
    fn quoted_template_value_tokenized() {
        // A quoted value with a space stays one token through parse + re-render.
        let argv = render_argv(
            &spec(),
            &tmpl("claude --append-system-prompt \"be brief\""),
            "claude",
        );
        assert_eq!(
            value_after(&argv, "--append-system-prompt"),
            Some("be brief")
        );
    }

    // --- Migrated #779 aiLaunchCommand.ts coverage (now Rust-owned) ---
    //
    // These pin the PTY-typed spawn shape produced for `/spawn-ai` via the
    // `build_ai_launch_command` tauri command: a BypassPermissions spec with a
    // config-dir env prefix and a fresh pinned id. The permission flag is
    // caller-authoritative, so the layered output always carries
    // `--permission-mode bypassPermissions` (a hardening over the old TS, which
    // let a permission-less operator template silently drop bypass mode).

    #[test]
    fn pty_ai_launch_default_template_appends_session_windows() {
        // Default-template case: operator flags layer in, fresh id appended.
        let mut s = spec();
        s.config_dir = Some("C:\\claude\\.claude-hotmail".to_string());
        s.session_id = Some("abc".to_string());
        let cmd = render_pty_command(&s, &tmpl("claude --model opus"), true);
        assert_eq!(
            cmd,
            "$env:CLAUDE_CONFIG_DIR=\"C:\\claude\\.claude-hotmail\"; \
             claude --teammate-mode in-process \
             --permission-mode bypassPermissions --model opus --session-id abc"
        );
    }

    #[test]
    fn pty_ai_launch_blank_template_builtin_posix() {
        // Blank template → built-in default; id appended; POSIX env prefix.
        let mut s = spec();
        s.config_dir = Some("/h/.claude-x".to_string());
        s.session_id = Some("abc".to_string());
        let cmd = render_pty_command(&s, &tmpl("   "), false);
        assert_eq!(
            cmd,
            "CLAUDE_CONFIG_DIR=\"/h/.claude-x\" \
             claude --teammate-mode in-process \
             --permission-mode bypassPermissions --session-id abc"
        );
    }

    #[test]
    fn pty_ai_launch_session_placeholder_substituted() {
        // `{sessionId}` placeholder: id substituted in place, not appended.
        let mut s = spec();
        s.config_dir = Some("/h/.claude-x".to_string());
        s.session_id = Some("abc".to_string());
        let cmd = render_pty_command(
            &s,
            &tmpl("claude --session-id {sessionId} --continue"),
            false,
        );
        assert_eq!(
            cmd,
            "CLAUDE_CONFIG_DIR=\"/h/.claude-x\" \
             claude --teammate-mode in-process \
             --permission-mode bypassPermissions --session-id abc --continue"
        );
    }

    #[test]
    fn pty_ai_launch_account_alias_verbatim_no_pin() {
        // Per-account opaque alias wins over the default template and is typed
        // verbatim — no env prefix, no `--session-id` pin. The tauri command's
        // `command.contains(session_id)` check then reports `pinnedSessionId:
        // null`, routing the frontend to its mtime-capture fallback.
        let cfg = LaunchConfig {
            default_template: Some("claude --model opus".to_string()),
            account_command: Some("clh".to_string()),
            ..Default::default()
        };
        let mut s = spec();
        s.config_dir = Some("C:\\claude\\.claude-hotmail".to_string());
        s.session_id = Some("abc".to_string());
        let cmd = render_pty_command(&s, &cfg, true);
        assert_eq!(cmd, "clh");
        assert!(
            !cmd.contains("abc"),
            "verbatim alias must not carry the pin"
        );
    }

    // ── Profile-driven: a non-Claude CLI (Codex) ────────────────────────────

    fn codex() -> &'static CliProfile {
        cli_profile::profile_for(cli_profile::codex::ID).expect("codex profile")
    }

    /// A Codex launch reads every spelling from the Codex profile: its program,
    /// its auto-approve flag, `CODEX_HOME` as the account variable — and no
    /// pin, because Codex mints its own id.
    #[test]
    fn codex_pty_launch_reads_the_codex_profile_and_pins_nothing() {
        let s = LaunchSpec {
            provider: codex(),
            config_dir: Some("/h/.codex-work".to_string()),
            session_id: Some("abc".to_string()),
            ..Default::default()
        };
        assert!(!s.pins_session_id());
        assert_eq!(
            render_pty_command(&s, &LaunchConfig::default(), false),
            "CODEX_HOME=\"/h/.codex-work\" codex --dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(
            render_pty_command(&s, &LaunchConfig::default(), true),
            "$env:CODEX_HOME=\"/h/.codex-work\"; codex --dangerously-bypass-approvals-and-sandbox"
        );
    }

    /// The operator templates and account aliases are CLAUDE settings: a Codex
    /// launch neither types a Claude alias nor layers a Claude template in.
    #[test]
    fn claude_launch_settings_never_reach_a_codex_launch() {
        let cfg = LaunchConfig {
            default_template: Some("claude --model opus --permission-mode acceptEdits".to_string()),
            account_command: Some("clh".to_string()),
            ..Default::default()
        };
        let s = LaunchSpec {
            provider: codex(),
            ..Default::default()
        };
        let cmd = render_pty_command(&s, &cfg, false);
        assert_eq!(cmd, "codex --dangerously-bypass-approvals-and-sandbox");
        // …while the same config still drives a Claude launch.
        assert_eq!(render_pty_command(&spec(), &cfg, false), "clh");
    }

    /// `--name` is a Claude Code flag: a display name set on a Codex launch is
    /// dropped, never typed at a CLI that has no such flag.
    #[test]
    fn display_name_is_claude_only() {
        let s = LaunchSpec {
            provider: codex(),
            name: Some("worker-1".to_string()),
            ..Default::default()
        };
        let argv = render_argv(&s, &LaunchConfig::default(), "codex");
        assert!(!argv.iter().any(|a| a == "--name"), "{argv:?}");
    }

    /// Codex resumes positionally (`codex resume <id>`): the subcommand follows
    /// the program, ahead of every flag; nothing is emitted as `--resume`.
    #[test]
    fn codex_resume_is_a_positional_subcommand_after_the_program() {
        let id = "01a0ef49-1234-7abc-8def-0123456789ab";
        let s = LaunchSpec {
            provider: codex(),
            resume_id: Some(id.to_string()),
            model: Some("o3".to_string()),
            ..Default::default()
        };
        assert_eq!(
            render_argv(&s, &LaunchConfig::default(), "/usr/bin/codex"),
            vec![
                "/usr/bin/codex",
                "resume",
                id,
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "o3",
            ]
        );
        let pty = render_pty_command(&s, &LaunchConfig::default(), false);
        assert!(pty.starts_with(&format!("codex resume {id} ")), "{pty}");
        assert!(!pty.contains("--resume"));
    }

    /// Both permission modes come from the profile: Claude's two spellings are
    /// its auto-approve argv and its single-flag detect form.
    #[test]
    fn permission_modes_read_the_profile_spellings() {
        let claude = &*claude::PROFILE;
        assert_eq!(
            permission_flags(claude, &PermissionMode::BypassPermissions),
            vec!["--permission-mode", "bypassPermissions"]
        );
        assert_eq!(
            permission_flags(claude, &PermissionMode::DangerouslySkip),
            vec!["--dangerously-skip-permissions"]
        );
        assert_eq!(
            permission_flags(codex(), &PermissionMode::DangerouslySkip),
            vec!["--dangerously-bypass-approvals-and-sandbox"]
        );
        // A profile that declares no auto-approval renders no permission flag.
        let mut none = codex().clone();
        none.auto_approve = AutoApprove::Unknown;
        assert!(permission_flags(&none, &PermissionMode::BypassPermissions).is_empty());
        assert!(permission_flag_names(&none).is_empty());
        assert_eq!(
            permission_flag_names(claude),
            vec![
                "--permission-mode",
                "--dangerously-skip-permissions",
                "--permission-mode",
                "--permission-mode",
                "--permission-mode",
                "--permission-prompt-tool"
            ]
        );
    }

    /// `Prompt` renders the profile's prompt-routing args and NO bypass flag
    /// (Phase 9): Claude gets `--permission-mode default` (so an account's
    /// `settings.json` `defaultMode` cannot pre-empt the prompts) and
    /// `--permission-prompt-tool stdio`, the spelling the Phase 2 probe verified
    /// against 2.1.285.
    #[test]
    fn prompt_mode_renders_the_prompt_tool_and_no_bypass() {
        let mut s = spec();
        s.permission = PermissionMode::Prompt;
        s.session_id = Some("sid".to_string());
        s.extra_required = vec![
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];
        let argv = render_argv(&s, &LaunchConfig::default(), "/bin/claude");
        assert_eq!(
            argv,
            vec![
                "/bin/claude",
                "--permission-mode",
                "default",
                "--permission-prompt-tool",
                "stdio",
                "--session-id",
                "sid",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json",
                "--verbose",
            ]
        );
        for bypass in [
            "--dangerously-skip-permissions",
            "bypassPermissions",
            "acceptEdits",
        ] {
            assert!(!argv.iter().any(|a| a == bypass), "{bypass} in {argv:?}");
        }
        assert!(PermissionMode::Prompt.prompts());
        assert!(!PermissionMode::default().prompts());
        assert_eq!(PermissionMode::default(), PermissionMode::BypassPermissions);
    }

    /// The template-permission-flag-is-dropped invariant holds for `Prompt`
    /// too: an operator template carrying a bypass flag cannot upgrade a
    /// prompted launch to bypass, nor re-route its prompts elsewhere.
    #[test]
    fn prompt_mode_drops_a_template_bypass_or_prompt_route() {
        let mut s = spec();
        s.permission = PermissionMode::Prompt;
        for template in [
            "claude --dangerously-skip-permissions --model opus",
            "claude --permission-mode bypassPermissions --model opus",
            "claude --permission-mode=bypassPermissions --model opus",
            "claude --permission-mode acceptEdits --model opus",
            "claude --permission-prompt-tool mcp__evil__approve --model opus",
            r#"claude --settings '{"permissions":{"defaultMode":"bypassPermissions"}}' --model opus"#,
            "claude --allowedTools Bash,Write,Edit --model opus",
            "claude --allowed-tools=Bash --model opus",
            "claude --allow-dangerously-skip-permissions --model opus",
            "claude --some-future-flag x --model opus",
        ] {
            let argv = render_argv(&s, &tmpl(template), "claude");
            assert!(
                !argv.iter().any(|a| a.contains("bypassPermissions")),
                "{template}: {argv:?}"
            );
            assert!(
                !argv.iter().any(|a| a == "--dangerously-skip-permissions"),
                "{template}"
            );
            for dropped in [
                "--settings",
                "--allowedTools",
                "--allowed-tools=Bash",
                "--allow-dangerously-skip-permissions",
                "--some-future-flag",
                "acceptEdits",
            ] {
                assert!(!argv.iter().any(|a| a == dropped), "{template}: {argv:?}");
            }
            assert_eq!(
                argv.iter().filter(|a| *a == "--permission-mode").count(),
                1,
                "{template}: {argv:?}"
            );
            assert_eq!(
                value_after(&argv, "--permission-mode"),
                Some("default"),
                "{template}"
            );
            assert_eq!(
                argv.iter()
                    .filter(|a| *a == "--permission-prompt-tool")
                    .count(),
                1,
                "{template}: {argv:?}"
            );
            assert_eq!(
                value_after(&argv, "--permission-prompt-tool"),
                Some("stdio"),
                "{template}"
            );
            // The template's other flags still layer in.
            assert_eq!(value_after(&argv, "--model"), Some("opus"), "{template}");
        }
        // Flags that cannot change the posture do layer in.
        let argv = render_argv(
            &s,
            &tmpl("claude --add-dir /x --verbose --disallowedTools Bash"),
            "claude",
        );
        assert_eq!(value_after(&argv, "--add-dir"), Some("/x"));
        assert_eq!(value_after(&argv, "--disallowedTools"), Some("Bash"));
        // ...and a BYPASS launch is unaffected by the prompt-mode list.
        let bypass = render_argv(
            &spec(),
            &tmpl("claude --allowedTools Bash --model opus"),
            "claude",
        );
        assert_eq!(value_after(&bypass, "--allowedTools"), Some("Bash"));
    }

    /// A profile with no known prompt args renders no permission flag at all
    /// for a prompted launch — never a fallback to bypass.
    #[test]
    fn prompt_mode_on_a_profile_without_prompt_args_renders_nothing() {
        assert!(codex().permission_prompt_args.is_empty());
        assert!(permission_flags(codex(), &PermissionMode::Prompt).is_empty());
        let s = LaunchSpec {
            provider: codex(),
            permission: PermissionMode::Prompt,
            ..Default::default()
        };
        let argv = render_argv(&s, &LaunchConfig::default(), "codex");
        assert!(!argv.iter().any(|a| a.contains("bypass")), "{argv:?}");
    }

    #[test]
    fn a_claude_spec_pins_unless_it_resumes() {
        let mut s = spec();
        assert!(!s.pins_session_id());
        s.session_id = Some("sid".to_string());
        assert!(s.pins_session_id());
        s.resume_id = Some("rid".to_string());
        assert!(!s.pins_session_id());
    }

    #[test]
    fn from_settings_is_reachable_api() {
        // Forward API for Phase 2/3 call sites — reference it so the seam's
        // public constructor is exercised at the type level without touching
        // global settings state.
        let _ctor: fn(Option<&str>) -> LaunchConfig = LaunchConfig::from_settings;
    }
}
