//! Shared Claude-launch-command builder (#548/#779 seam, Phase 1).
//!
//! One flag model, two renderers. The runner spawns `claude` two ways and both
//! must be driven from a single source of truth so the operator's optional
//! launch flags (and, critically, the caller's non-negotiable *required* flags)
//! can never drift between them:
//!
//! - **Modality A — argv vector** ([`render_argv`]): a `Vec<String>` handed to a
//!   child `Command` / terminal backend (see `session.rs`, `runner.rs`,
//!   `agent_runtime.rs`). The head is the resolved `claude` binary path; the
//!   caller sets `CLAUDE_CONFIG_DIR` on the child env itself, so argv never
//!   carries the env prefix.
//! - **Modality B — PTY-typed shell command** ([`render_pty_command`]): a shell
//!   string typed into a pseudo-terminal (see `aiLaunchCommand.ts`). The head is
//!   a bare `claude` (PATH/shim resolves it) and the string is prefixed with the
//!   `CLAUDE_CONFIG_DIR` assignment.
//!
//! Both share [`compose_flags`] so the composed flag set is identical by
//! construction — the parity the tests pin.
//!
//! ## Precedence (load-bearing)
//!
//! Per flag: **caller REQUIRED (`LaunchSpec` fields) > per-account override >
//! global template > CLI default.** The permission flag is *always*
//! caller-authoritative — an operator template that carries a conflicting
//! permission flag has that flag dropped, never applied. This invariant protects
//! autonomous spawns from being silently downgraded out of bypass mode.

use std::collections::HashSet;

/// `{sessionId}` placeholder an operator may embed in a launch template; when
/// present the pinned id is substituted in place instead of a flag being
/// appended (mirrors the #779 frontend behavior in `aiLaunchCommand.ts`).
const SESSION_ID_PLACEHOLDER: &str = "{sessionId}";

/// Caller-authoritative permission posture. Always wins over any permission flag
/// found in an operator template. Defaults to `BypassPermissions` — every
/// current spawn site is autonomous and must never stall on a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// `--permission-mode bypassPermissions` (interactive/stream-json sites).
    #[default]
    BypassPermissions,
    /// `--dangerously-skip-permissions` (autonomous `agent_runtime` sites).
    DangerouslySkip,
}

/// Operator-configured optional launch templates, resolved for one account.
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
#[derive(Debug, Clone, Default)]
pub struct LaunchSpec {
    /// `CLAUDE_CONFIG_DIR`. Used as the PTY env prefix; argv callers set the env
    /// on the child `Command` themselves. `None` ⇒ no prefix.
    pub config_dir: Option<String>,
    /// Caller-authoritative permission mode. Always applied; never overridden by
    /// the template. Defaults to `BypassPermissions`.
    pub permission: PermissionMode,
    /// New-pin id ⇒ `--session-id <id>`. Ignored when `resume_id` is set.
    pub session_id: Option<String>,
    /// Resume id ⇒ `--resume <id>`. Takes precedence over `session_id`.
    pub resume_id: Option<String>,
    /// Caller-provided model ⇒ `--model <m>`; subsumes model overrides and any
    /// failover sniff. When set, drops any `--model` from the template.
    pub model: Option<String>,
    /// Other non-negotiable trailing args, verbatim and in order — carries the
    /// `--append-system-prompt <s>`, `--add-dir <d>`, and the trailing
    /// `-- <prompt>` positional. Never reordered or deduped within.
    pub extra_required: Vec<String>,
}

/// A parsed template flag and the value tokens that follow it (arity inferred:
/// a value is any following token that does not itself start with `-`). A bare
/// positional token is stored with an empty `values` list.
struct FlagUnit {
    name: String,
    values: Vec<String>,
}

/// Render the argv vector: `[claude_bin, <composed flags…>]`. No env prefix and
/// no shell wrapper — both are the caller's concern.
pub fn render_argv(spec: &LaunchSpec, cfg: &LaunchConfig, claude_bin: &str) -> Vec<String> {
    let mut argv = Vec::with_capacity(1 + spec.extra_required.len() + 6);
    argv.push(claude_bin.to_string());
    argv.extend(compose_flags(spec, cfg));
    argv
}

/// Render the PTY-typed shell command string.
///
/// If the per-account override is an opaque alias (not a recognizable `claude …`
/// invocation), it is returned verbatim — the runner cannot introspect it, so it
/// applies no pin/permission/env (the #779 escape hatch). Otherwise the composed
/// flags are joined onto a bare `claude` head and prefixed with the
/// `CLAUDE_CONFIG_DIR` assignment (omitted when `config_dir` is `None`).
pub fn render_pty_command(spec: &LaunchSpec, cfg: &LaunchConfig, is_windows: bool) -> String {
    if let Some(alias) = pty_verbatim_alias(cfg) {
        return alias;
    }

    let flags = compose_flags(spec, cfg);
    let mut parts = Vec::with_capacity(flags.len() + 1);
    parts.push("claude".to_string());
    for flag in &flags {
        parts.push(shell_quote(flag, is_windows));
    }
    let body = parts.join(" ");

    match &spec.config_dir {
        Some(dir) if is_windows => format!("$env:CLAUDE_CONFIG_DIR=\"{}\"; {}", dir, body),
        Some(dir) => format!("CLAUDE_CONFIG_DIR=\"{}\" {}", dir, body),
        None => body,
    }
}

/// The shared flag-composition core. Both renderers build on this so the
/// composed flag set can never diverge between argv and PTY.
///
/// Order: `[permission] [model] [session] [other template flags…] [extra_required…]`.
/// Precedence per flag: spec field > per-account claude template > global
/// template > CLI default.
fn compose_flags(spec: &LaunchSpec, cfg: &LaunchConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // 1. Permission — caller-authoritative, always applied.
    match spec.permission {
        PermissionMode::BypassPermissions => {
            out.push("--permission-mode".to_string());
            out.push("bypassPermissions".to_string());
        }
        PermissionMode::DangerouslySkip => {
            out.push("--dangerously-skip-permissions".to_string());
        }
    }

    // Resolve the claude template to layer in (per-account claude command wins
    // over the global default; an opaque account alias falls through to the
    // global default for the argv/compose path).
    let template = claude_template(cfg);
    let pin = spec.resume_id.as_deref().or(spec.session_id.as_deref());
    let provided = provided_flag_names(&spec.extra_required);

    let mut template_model: Option<String> = None;
    let mut other_units: Vec<FlagUnit> = Vec::new();
    let mut session_via_placeholder = false;

    if let Some(t) = &template {
        let had_placeholder = t.contains(SESSION_ID_PLACEHOLDER);
        session_via_placeholder = had_placeholder;
        let substituted = t.replace(SESSION_ID_PLACEHOLDER, pin.unwrap_or(""));
        let tokens = shell_tokenize(&substituted);
        // Drop the `claude` head token (guaranteed present — `claude_template`
        // only returns recognizable claude commands).
        let body = tokens.get(1..).unwrap_or(&[]);
        for unit in parse_units(body) {
            match unit.name.as_str() {
                // Permission is spec-owned: drop any template permission flag so
                // it can never override the caller.
                "--permission-mode" | "--dangerously-skip-permissions" => {}
                // Model is decided below (spec beats template).
                "--model" => template_model = unit.values.first().cloned(),
                // Session flags: when the operator positioned the id via the
                // `{sessionId}` placeholder, keep the flag in place; otherwise the
                // spec owns session and we drop the template's.
                "--session-id" | "--resume" => {
                    if had_placeholder {
                        other_units.push(unit);
                    }
                }
                // Any other operator flag is layered in, unless the caller's
                // extra_required already provides that flag (spec wins).
                _ => {
                    if !provided.contains(&unit.name) {
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
    //    placeholder. resume takes precedence over session-id.
    if !session_via_placeholder {
        if let Some(resume) = &spec.resume_id {
            out.push("--resume".to_string());
            out.push(resume.clone());
        } else if let Some(session) = &spec.session_id {
            out.push("--session-id".to_string());
            out.push(session.clone());
        }
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

/// The claude template to parse for flag composition, honoring precedence:
/// a per-account *claude* command wins; an opaque account alias is skipped
/// (handled verbatim for PTY, ignored for argv) and the global default is used.
fn claude_template(cfg: &LaunchConfig) -> Option<String> {
    if let Some(acct) = &cfg.account_command {
        if is_claude_command(acct) {
            return Some(acct.clone());
        }
        // Opaque alias → fall through to the global default for compose.
    }
    if let Some(def) = &cfg.default_template {
        let trimmed = def.trim();
        if !trimmed.is_empty() && is_claude_command(trimmed) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// The per-account override when it is an opaque alias (PTY returns it verbatim).
fn pty_verbatim_alias(cfg: &LaunchConfig) -> Option<String> {
    cfg.account_command
        .as_ref()
        .filter(|a| !is_claude_command(a))
        .cloned()
}

/// Whether `s` is a recognizable `claude …` invocation (vs. an opaque alias).
/// Matches on the head token's basename, case-insensitively, `.exe`-stripped.
fn is_claude_command(s: &str) -> bool {
    let tokens = shell_tokenize(s.trim());
    let Some(first) = tokens.first() else {
        return false;
    };
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base);
    base.eq_ignore_ascii_case("claude")
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
        // PTY head is bare `claude`; the remaining space-split tokens must equal
        // argv[1..] (tokens here are space-free, so split round-trips).
        let pty_tokens: Vec<String> = pty.split(' ').map(String::from).collect();
        assert_eq!(pty_tokens[0], "claude");
        assert_eq!(&pty_tokens[1..], &argv[1..]);
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
             claude --permission-mode bypassPermissions --model opus --session-id abc"
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
             claude --permission-mode bypassPermissions --session-id abc"
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
             claude --permission-mode bypassPermissions --session-id abc --continue"
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

    #[test]
    fn from_settings_is_reachable_api() {
        // Forward API for Phase 2/3 call sites — reference it so the seam's
        // public constructor is exercised at the type level without touching
        // global settings state.
        let _ctor: fn(Option<&str>) -> LaunchConfig = LaunchConfig::from_settings;
    }
}
