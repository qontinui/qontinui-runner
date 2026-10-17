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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
}

impl PermissionMode {
    /// Whether tool approvals are the runner's to answer (the CLI asks), as
    /// opposed to bypassed at launch.
    pub fn prompts(self) -> bool {
        self == PermissionMode::Prompt
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
}

impl LaunchConfig {
    /// Convenience constructor reading the live settings for `config_dir`.
    ///
    /// Consumed by the Phase 2/3 call-site migrations (each spawn site resolves
    /// its account's config dir and hands it here).
    pub fn from_settings(config_dir: Option<&str>) -> Self {
        let default_template = crate::settings::get_claude_default_launch_command();
        let account_command = config_dir.and_then(|dir| {
            crate::settings::get_claude_account_launch_commands()
                .get(dir)
                .cloned()
        });
        Self {
            default_template,
            account_command,
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

/// Make `raw` safe to hand to `claude --name`: control characters and newlines
/// become spaces, whitespace runs collapse to one space, leading `-` is stripped
/// (a value starting with `-` could be parsed as a flag), and the result is
/// capped at [`MAX_SESSION_NAME_CHARS`] chars. `None` when nothing is left.
pub fn sanitize_session_name(raw: &str) -> Option<String> {
    let spaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_start_matches(|c: char| c == '-' || c.is_whitespace());
    let capped: String = trimmed.chars().take(MAX_SESSION_NAME_CHARS).collect();
    let capped = capped.trim_end().to_string();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
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
    if let Some(alias) = pty_verbatim_alias(spec, cfg) {
        return alias;
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

    // 1. Permission — caller-authoritative, always applied.
    out.extend(permission_flags(profile, spec.permission));

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
    let provided = provided_flag_names(&spec.extra_required);
    let caller_owns_append_prompt = provides_append_prompt_flag(&spec.extra_required);
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
                        flag = %name,
                        "launch template flag dropped: the caller's system-prompt carrier owns \
                         the append slot and Claude Code refuses both append flags together"
                    );
                }
                // Any other operator flag is layered in, unless the caller's
                // extra_required already provides that flag (spec wins).
                // A prompted launch layers in only the template flags known
                // not to touch permissions — fail closed: a flag this list
                // does not name (`--settings` carrying a `defaultMode`,
                // `--allowedTools`, a bypass spelling a newer CLI adds) could
                // stop the session asking.
                name if spec.permission.prompts() && !prompt_mode_admits(name) => {
                    tracing::warn!(
                        flag = %name,
                        "launch template flag dropped: a prompted launch layers in only flags \
                         that cannot change its permission posture"
                    );
                }
                _ => {
                    if provided.contains(&unit.name) {
                        tracing::warn!(
                            flag = %unit.name,
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
    out.extend(spec.extra_required.iter().cloned());

    out
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
fn permission_flags(profile: &CliProfile, mode: PermissionMode) -> Vec<String> {
    let bypass = match &profile.auto_approve {
        AutoApprove::Flags { argv, detect } => Some((argv, detect)),
        _ => None,
    };
    match (mode, bypass) {
        (PermissionMode::Prompt, _) => profile.permission_prompt_args.clone(),
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
fn shell_quote(tok: &str, is_windows: bool) -> String {
    let safe = !tok.is_empty()
        && tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c));
    if safe {
        return tok.to_string();
    }
    if is_windows {
        // PowerShell single-quote literal: embedded ' is doubled.
        format!("'{}'", tok.replace('\'', "''"))
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
            permission_flags(claude, PermissionMode::BypassPermissions),
            vec!["--permission-mode", "bypassPermissions"]
        );
        assert_eq!(
            permission_flags(claude, PermissionMode::DangerouslySkip),
            vec!["--dangerously-skip-permissions"]
        );
        assert_eq!(
            permission_flags(codex(), PermissionMode::DangerouslySkip),
            vec!["--dangerously-bypass-approvals-and-sandbox"]
        );
        // A profile that declares no auto-approval renders no permission flag.
        let mut none = codex().clone();
        none.auto_approve = AutoApprove::Unknown;
        assert!(permission_flags(&none, PermissionMode::BypassPermissions).is_empty());
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
        assert!(permission_flags(codex(), PermissionMode::Prompt).is_empty());
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
