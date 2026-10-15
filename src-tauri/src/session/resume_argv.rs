//! The `--resume` relaunch flags of a provider session, derived ONCE from its
//! launch argv — with the allowlist and shell-inert gate a relaunch must pass.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 6, D11. When a pane's PTY holder itself dies (tier 1 lost), the
//! session in it is relaunched in a fresh pane as `claude <flags> --resume
//! <id>`, TYPED INTO A SHELL by the webview's restore. The flags are the
//! harmless ones the session was launched with (`--model`, `--add-dir`, …), so
//! a relaunched session runs the way its operator started it.
//!
//! # Where the argv comes from, and what is kept of it
//!
//! The identity shim (`resources/intercept/identity_shim.bash`, and the native
//! `qontinui-shim` on Windows) sees the argv a `claude` launch received and
//! exports it to that `claude` as [`LAUNCH_ARGV_ENV`]. The bundled
//! `SessionStart` hook — a child of that `claude` — sends it in the body of
//! its LOOPBACK `POST /control/session-open`, never over the PTY: the PTY
//! stream is relayed, kept in rings and shipped to coord, and an argv can hold
//! an initial prompt or an inline secret (review r1, H1). On receipt the
//! runner derives the flags with [`derive_resume_flags`] and persists ONLY
//! that result ([`ResumeFlags`]: the allowlisted flags, or a refusal) — never
//! the raw argv, the prompt, or the value of any flag not on the allowlist.
//!
//! # The allowlist: flags that load no code or config
//!
//! Only `--model`, `--fallback-model`, `--permission-mode`, `--add-dir`,
//! `--verbose`, `--ide`, `--chrome` / `--no-chrome` and `--debug` are replayed
//! (review r1, M2). Every other flag is DROPPED with its value: flags such as
//! `--settings`, `--mcp-config` or `--plugin-dir` load hooks, servers or code,
//! and a relaunch must not run something the operator does not see. Unknown
//! flags are dropped too. The arity table still names the dropped flags this
//! runner knows, so their values are skipped rather than mistaken for prompts.
//!
//! A relative `--add-dir` is resolved against the launch's working directory
//! (the hook's `cwd`); when that is unknown or not absolute the replay is
//! refused, because the relaunch's pane starts in the RECORD's directory,
//! which need not be where `claude` was launched.
//!
//! # The gate: reject the whole replay, never escape it
//!
//! Adopted from tty7 (`cli_agent.rs`, idea only): every token that SURVIVES
//! the filter must be shell-inert ([`is_shell_inert`]) or the REPLAY is
//! refused outright. Escaping is not attempted: the command is typed into
//! whichever shell the pane runs (bash, zsh, PowerShell), and a quoting scheme
//! correct for one is an injection for another.
//!
//! A refused replay does NOT mean no relaunch (coordinator decision,
//! 2026-10-15): the session is still resumed with the DEFAULT resume command —
//! exactly the pre-Phase-6 behaviour — and only the extra flags are dropped
//! ([`Relaunch::Fallback`]). The refusal is logged at warn naming the flag the
//! offending token belongs to, never its value. Only a missing or unsafe
//! session id means no relaunch at all.
//!
//! Print mode (`-p`/`--print`) and combined short flags (`-pc`, `-rID`: a
//! single-dash token longer than two characters) are refused, since either
//! can smuggle a second mode or session selector past the filter.

use crate::session::session_id::is_valid_session_id;

/// The environment variable the identity shim sets on the `claude` it launches:
/// a JSON array of the argv that launch received (minus the runner plumbing
/// the shim strips first), every non-ASCII character `\u`-escaped. Read by the
/// bundled `SessionStart` hook, which sends it in its loopback POST.
pub const LAUNCH_ARGV_ENV: &str = "QONTINUI_LAUNCH_ARGV";

/// Most tokens an argv may carry for a replay to be derived from it.
pub const MAX_ARGV_TOKENS: usize = 64;

/// Longest single argv token accepted, in BYTES (the same unit the shims and
/// the hook count in). A longer one makes the whole argv unknown — never a
/// truncated token.
pub const MAX_ARGV_TOKEN_BYTES: usize = 1024;

/// Longest token a REPLAY may type, in bytes. A replayed token is a flag or a
/// short value; anything longer is not something an operator typed by hand.
pub const MAX_REPLAY_TOKEN_BYTES: usize = 256;

/// What a session's relaunch replays, as derived once from its launch argv and
/// persisted on its lifecycle record. Never the raw argv.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResumeFlags {
    /// Type these allowlisted, shell-inert flags before `--resume <id>`.
    Replay { flags: Vec<String> },
    /// The argv must not be replayed: the relaunch uses the default resume
    /// command. `reason` names a position and a flag, never a value.
    Refused { reason: String },
}

/// Why a launch argv cannot be replayed. Typed so the webview can say why its
/// flags were dropped, and so no variant carries a token's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayRefusal {
    /// The session id is not shell-safe (`session_id::is_valid_session_id`).
    InvalidSessionId,
    /// Token `index` survives the filter but is not shell-inert. `flag` is the
    /// flag it belongs to (itself, or the flag whose value it is), named only
    /// when that name is itself inert — never the token's value.
    NotShellInert { index: usize, flag: Option<String> },
    /// A relative `--add-dir` value at `index`, and no absolute launch
    /// directory to resolve it against.
    RelativePath { index: usize },
    /// A single-dash token longer than two characters at `index` (`-pc`,
    /// `-rID`): combined short flags the filter cannot read safely.
    CombinedShortFlags { index: usize },
    /// The allowlisted value-taking flag at `index` has no value, or its value
    /// starts with `-` — replayed bare, `claude --model --resume <id>` would
    /// take `--resume` as the model and the id as a new prompt (review r2, N6).
    MissingValue { index: usize, flag: Option<String> },
    /// A `--permission-mode` at `index` names none of the four known modes.
    UnknownPermissionMode { index: usize },
    /// A relative `--add-dir` on Windows whose launch directory is an MSYS
    /// path (`/c/...`): a PowerShell pane cannot resolve it (review r2, N5).
    MsysLaunchDirectory { index: usize },
    /// The session was launched in print mode (`-p`/`--print`).
    PrintMode,
    /// More than [`MAX_ARGV_TOKENS`] tokens.
    TooLong,
}

impl std::fmt::Display for ReplayRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayRefusal::InvalidSessionId => {
                write!(f, "the session id is not shell-safe")
            }
            ReplayRefusal::NotShellInert { index, flag } => write!(
                f,
                "launch argument {index} (flag {}) is not shell-inert; its replay is refused \
                 rather than escaped",
                flag.as_deref().unwrap_or("<unnamed>")
            ),
            ReplayRefusal::RelativePath { index } => write!(
                f,
                "launch argument {index} (flag --add-dir) is a relative path and the launch \
                 directory is unknown"
            ),
            ReplayRefusal::CombinedShortFlags { index } => write!(
                f,
                "launch argument {index} combines short flags, which the replay does not read"
            ),
            ReplayRefusal::MissingValue { index, flag } => write!(
                f,
                "launch argument {index} (flag {}) has no value, or a value that starts with `-`",
                flag.as_deref().unwrap_or("<unnamed>")
            ),
            ReplayRefusal::UnknownPermissionMode { index } => write!(
                f,
                "launch argument {index} (flag --permission-mode) names no known permission mode"
            ),
            ReplayRefusal::MsysLaunchDirectory { index } => write!(
                f,
                "launch argument {index} (flag --add-dir) is relative and the launch directory is \
                 an MSYS path"
            ),
            ReplayRefusal::PrintMode => {
                write!(f, "the session was launched in print mode (-p/--print)")
            }
            ReplayRefusal::TooLong => {
                write!(
                    f,
                    "the launch argv has more than {MAX_ARGV_TOKENS} arguments"
                )
            }
        }
    }
}

/// The one shell-inert character set: `[A-Za-z0-9_./:=+-]`.
///
/// Inert in bash, zsh, POSIX sh AND PowerShell, which is what a typed command
/// needs: no quoting, globbing, expansion, splatting (`@`), array (`,`),
/// variable (`$`, `%`), redirection, separator or escape character, and no
/// whitespace.
pub fn is_inert_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '=' | '+' | '-')
}

/// Whether `token` may be typed into a shell as one argument as-is: inert
/// characters only, at most [`MAX_REPLAY_TOKEN_BYTES`], and a first character
/// that is alphanumeric, `-`, `/` or `.` — a leading `=` is zsh's EQUALS
/// expansion (`=foo` → the path of `foo`).
pub fn is_shell_inert(token: &str) -> bool {
    let Some(first) = token.chars().next() else {
        return false;
    };
    (first.is_ascii_alphanumeric() || matches!(first, '-' | '/' | '.'))
        && token.len() <= MAX_REPLAY_TOKEN_BYTES
        && token.chars().all(is_inert_char)
}

/// How a known flag consumes the tokens after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// Takes no value.
    Bool,
    /// Takes exactly one value (the next token, or `--flag=value`).
    One,
    /// Takes every following non-flag token (commander's `<values...>`).
    Many,
    /// Takes the next token only when it is not a flag (`--resume [id]`).
    Optional,
}

/// What the replay does with a known flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// On the allowlist: replayed.
    Keep,
    /// Known, and dropped with its value.
    Drop,
    /// Print mode: the replay is refused.
    RefusePrint,
}

/// The replay ALLOWLIST: flags that load no code or config, and how each
/// consumes the tokens after it. The ONE list — the identity shims carry a
/// copy (they export only these flags), pinned equal to this by
/// `tests::the_shim_allowlists_match_the_runners` (review r2, N1).
pub const REPLAY_ALLOWLIST_ONE: &[&str] = &["--model", "--fallback-model", "--permission-mode"];
/// See [`REPLAY_ALLOWLIST_ONE`]: flags taking every following plain token.
pub const REPLAY_ALLOWLIST_MANY: &[&str] = &["--add-dir"];
/// See [`REPLAY_ALLOWLIST_ONE`]: flags taking the next token unless it is a flag.
pub const REPLAY_ALLOWLIST_OPTIONAL: &[&str] = &["--debug"];
/// See [`REPLAY_ALLOWLIST_ONE`]: flags taking no value.
pub const REPLAY_ALLOWLIST_BOOL: &[&str] = &["--verbose", "--ide", "--chrome", "--no-chrome"];

/// The permission modes a replay may name. Any other value refuses the
/// replay (review r2, permission rule).
pub const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "plan", "bypassPermissions"];

/// The `claude` flags this runner knows. `Keep` is the allowlist (module
/// docs); `Drop` entries exist so their VALUES are consumed rather than read
/// as positionals. Anything else is unknown and dropped.
fn flag_arity(flag: &str) -> Option<(Arity, Disposition)> {
    use Arity::*;
    use Disposition::*;
    if REPLAY_ALLOWLIST_ONE.contains(&flag) {
        return Some((One, Keep));
    }
    if REPLAY_ALLOWLIST_MANY.contains(&flag) {
        return Some((Many, Keep));
    }
    if REPLAY_ALLOWLIST_OPTIONAL.contains(&flag) {
        return Some((Optional, Keep));
    }
    if REPLAY_ALLOWLIST_BOOL.contains(&flag) {
        return Some((Bool, Keep));
    }
    Some(match flag {
        // Print mode — not a conversation a pane can resume.
        "--print" | "-p" => (Bool, RefusePrint),
        // Session selection — the relaunch supplies its own.
        "--resume" | "-r" | "--from-pr" => (Optional, Drop),
        "--continue" | "-c" | "--fork-session" => (Bool, Drop),
        "--session-id" => (One, Drop),
        // Runner plumbing — re-added by the shell integration in the new pane.
        "--append-system-prompt" | "--append-system-prompt-file" => (One, Drop),
        // Known, and NOT replayed: they load code or config, or change what
        // the session may do, out of the operator's sight.
        "--agent"
        | "--agents"
        | "--settings"
        | "--setting-sources"
        | "--system-prompt"
        | "--system-prompt-file"
        | "--plugin-dir"
        | "--output-format"
        | "--input-format"
        | "--max-turns"
        | "--permission-prompt-tool" => (One, Drop),
        "--allowedTools" | "--allowed-tools" | "--disallowedTools" | "--disallowed-tools"
        | "--mcp-config" | "--betas" => (Many, Drop),
        "--dangerously-skip-permissions"
        | "--allow-dangerously-skip-permissions"
        | "--strict-mcp-config"
        | "--include-partial-messages"
        | "--replay-user-messages" => (Bool, Drop),
        _ => return None,
    })
}

fn is_flag(token: &str) -> bool {
    token.len() > 1 && token.starts_with('-')
}

/// A flag name fit for a message: only when it is itself shell-inert (a flag
/// token can be anything an operator typed).
fn inert_name(name: &str) -> Option<String> {
    is_shell_inert(name).then(|| name.to_string())
}

/// The flags to type between `claude` and `--resume <session_id>` when
/// relaunching a session launched with `launch_argv` from `launch_cwd` — or why
/// they must not be replayed. `Ok(vec![])` is a launch with no replayable
/// flags (a bare `claude`).
pub fn replay_args(
    launch_argv: &[String],
    session_id: &str,
    launch_cwd: Option<&str>,
) -> Result<Vec<String>, ReplayRefusal> {
    replay_args_on(launch_argv, session_id, launch_cwd, cfg!(windows))
}

/// One kept allowlisted flag and its values, as `(position, token)`.
struct KeptFlag<'a> {
    name: &'a str,
    tokens: Vec<(usize, String)>,
}

/// [`replay_args`] for an explicit platform (`windows`), so both arms of the
/// Windows path rules are tested on any host.
pub fn replay_args_on(
    launch_argv: &[String],
    session_id: &str,
    launch_cwd: Option<&str>,
    windows: bool,
) -> Result<Vec<String>, ReplayRefusal> {
    if !is_valid_session_id(session_id) {
        return Err(ReplayRefusal::InvalidSessionId);
    }
    if launch_argv.len() > MAX_ARGV_TOKENS {
        return Err(ReplayRefusal::TooLong);
    }
    let mut kept: Vec<KeptFlag> = Vec::new();
    let mut i = 0;
    while i < launch_argv.len() {
        let token = &launch_argv[i];
        // `--` ends the flags: everything after it is positional (the prompt).
        if token == "--" {
            break;
        }
        if !is_flag(token) {
            // A positional: the initial prompt. Never replayed.
            i += 1;
            continue;
        }
        if !token.starts_with("--") && token.chars().count() > 2 {
            return Err(ReplayRefusal::CombinedShortFlags { index: i });
        }
        let (name, inline) = match token.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (token.as_str(), None),
        };
        // Unknown flags are dropped. Their value, if any, is a plain token and
        // is dropped as a positional on the next turn.
        let Some((arity, disposition)) = flag_arity(name) else {
            i += 1;
            continue;
        };
        if disposition == Disposition::RefusePrint {
            return Err(ReplayRefusal::PrintMode);
        }
        let keep = disposition == Disposition::Keep;
        let flag_index = i;
        let missing = || ReplayRefusal::MissingValue {
            index: flag_index,
            flag: inert_name(name),
        };
        i += 1;
        let mut group = KeptFlag {
            name,
            tokens: vec![(flag_index, token.clone())],
        };
        if let Some(v) = inline {
            if keep && arity != Arity::Bool && (v.is_empty() || v.starts_with('-')) {
                return Err(missing());
            }
        } else {
            let takes = |t: &String| !is_flag(t) && t != "--";
            let mut values: Vec<usize> = Vec::new();
            match arity {
                Arity::Bool => {}
                Arity::One => {
                    // `claude` takes a required value greedily, flag-shaped
                    // or not; a replay refuses one that is absent or a flag.
                    if launch_argv.get(i).is_some() {
                        values.push(i);
                    }
                }
                Arity::Optional => {
                    if launch_argv.get(i).is_some_and(takes) {
                        values.push(i);
                    }
                }
                Arity::Many => {
                    let mut j = i;
                    while launch_argv.get(j).is_some_and(takes) {
                        values.push(j);
                        j += 1;
                    }
                }
            }
            if keep
                && matches!(arity, Arity::One | Arity::Many)
                && (values.is_empty() || values.iter().any(|&v| launch_argv[v].starts_with('-')))
            {
                return Err(missing());
            }
            i += values.len();
            group
                .tokens
                .extend(values.into_iter().map(|v| (v, launch_argv[v].clone())));
        }
        if keep {
            kept.push(group);
        }
    }
    // The permission rule (review r2): every mode must be a known one, and
    // only the LAST `--permission-mode` is replayed, so the typed command
    // carries exactly one permission choice.
    for g in kept.iter().filter(|g| g.name == "--permission-mode") {
        let (index, value) = match g.tokens.as_slice() {
            [(i, t)] => (*i, t.split_once('=').map(|(_, v)| v).unwrap_or("")),
            [_, (i, v)] => (*i, v.as_str()),
            _ => (g.tokens[0].0, ""),
        };
        if !PERMISSION_MODES.contains(&value) {
            return Err(ReplayRefusal::UnknownPermissionMode { index });
        }
    }
    if let Some(last) = kept.iter().rposition(|g| g.name == "--permission-mode") {
        let mut n = 0;
        kept.retain(|g| {
            let keep = g.name != "--permission-mode" || n == last;
            n += 1;
            keep
        });
    }
    let mut out = Vec::new();
    for g in kept {
        for (index, token) in g.tokens {
            let token = if g.name == "--add-dir" && token != "--add-dir" {
                resolve_add_dir(&token, index, launch_cwd, windows)?
            } else {
                token
            };
            if !is_shell_inert(&token) {
                return Err(ReplayRefusal::NotShellInert {
                    index,
                    flag: inert_name(g.name),
                });
            }
            out.push(token);
        }
    }
    Ok(out)
}

/// Whether `dir` is a Windows drive-letter path (`C:/x`, `C:\x`, `C:`).
fn is_drive_path(dir: &str) -> bool {
    let b = dir.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
}

/// Whether `cwd` is an MSYS path (`/c/...`, `/c`): Git Bash's spelling of a
/// Windows directory, which a PowerShell pane cannot use.
fn is_msys_path(cwd: &str) -> bool {
    let b = cwd.as_bytes();
    b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/')
}

/// An `--add-dir` value (bare, or the whole `--add-dir=<dir>` token) with a
/// relative directory resolved against the absolute launch directory. On
/// Windows a drive-letter path is absolute, and a relative one under an MSYS
/// launch directory is refused (review r2, N5).
fn resolve_add_dir(
    token: &str,
    index: usize,
    launch_cwd: Option<&str>,
    windows: bool,
) -> Result<String, ReplayRefusal> {
    let (prefix, dir) = match token.strip_prefix("--add-dir=") {
        Some(dir) => ("--add-dir=", dir),
        None => ("", token),
    };
    if dir.starts_with('/') || (windows && is_drive_path(dir)) {
        return Ok(token.to_string());
    }
    let Some(cwd) = launch_cwd else {
        return Err(ReplayRefusal::RelativePath { index });
    };
    if windows {
        if is_msys_path(cwd) {
            return Err(ReplayRefusal::MsysLaunchDirectory { index });
        }
        if is_drive_path(cwd) {
            let cwd = cwd.replace('\\', "/");
            return Ok(format!("{prefix}{}/{dir}", cwd.trim_end_matches('/')));
        }
        return Err(ReplayRefusal::RelativePath { index });
    }
    if cwd.starts_with('/') {
        Ok(format!("{prefix}{}/{dir}", cwd.trim_end_matches('/')))
    } else {
        Err(ReplayRefusal::RelativePath { index })
    }
}

/// What to persist for a session launched with `launch_argv` from
/// `launch_cwd`: the replay flags, or a refusal (the relaunch then falls back
/// to the default command). `None` when the session id itself is unsafe —
/// such a session is never relaunched, so there is nothing to persist.
pub fn derive_resume_flags(
    launch_argv: &[String],
    session_id: &str,
    launch_cwd: Option<&str>,
) -> Option<ResumeFlags> {
    match replay_args(launch_argv, session_id, launch_cwd) {
        Ok(flags) => Some(ResumeFlags::Replay { flags }),
        Err(ReplayRefusal::InvalidSessionId) => None,
        Err(why) => Some(ResumeFlags::Refused {
            reason: why.to_string(),
        }),
    }
}

/// How a session is relaunched — the two derived fields the restore set
/// carries (`terminal_session_list_open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaunch {
    /// No flags were ever derived: the default resume command, exactly as
    /// before Phase 6.
    Default,
    /// Replay these flags before `--resume <id>`.
    Replay(Vec<String>),
    /// The argv was refused: the DEFAULT resume command is used instead, and
    /// only the extra flags are dropped. Carries the refusal's message.
    Fallback(String),
}

impl Relaunch {
    /// Decide for one record from its persisted [`ResumeFlags`]. Every
    /// replayed flag is re-checked shell-inert here, so a record file edited
    /// by hand cannot get a token typed.
    pub fn for_record(flags: Option<&ResumeFlags>) -> Self {
        match flags {
            None => Relaunch::Default,
            Some(ResumeFlags::Replay { flags }) => {
                if flags.iter().all(|f| is_shell_inert(f)) {
                    Relaunch::Replay(flags.clone())
                } else {
                    Relaunch::Fallback("a stored relaunch flag is not shell-inert".into())
                }
            }
            Some(ResumeFlags::Refused { reason }) => Relaunch::Fallback(reason.clone()),
        }
    }

    /// `(resumeArgs, resumeRefusal)` for the wire: both absent for
    /// [`Relaunch::Default`], so a webview that predates them — and a record
    /// that predates Phase 6 — behaves exactly as before. A
    /// [`Relaunch::Fallback`] carries NO args (the webview then types the
    /// default resume) plus the refusal, for its log line.
    pub fn wire_fields(&self) -> (Option<Vec<String>>, Option<String>) {
        match self {
            Relaunch::Default => (None, None),
            Relaunch::Replay(flags) => (Some(flags.clone()), None),
            Relaunch::Fallback(why) => (None, Some(why.clone())),
        }
    }
}

/// Parse the JSON array [`LAUNCH_ARGV_ENV`] carries into a bounded argv:
/// `None` unless it is an array of at most [`MAX_ARGV_TOKENS`] strings of at
/// most [`MAX_ARGV_TOKEN_BYTES`] bytes each. Never a partial argv.
pub fn parse_argv_value(value: &serde_json::Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    if items.len() > MAX_ARGV_TOKENS {
        return None;
    }
    items
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= MAX_ARGV_TOKEN_BYTES)
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";
    const CWD: Option<&str> = Some("/home/op/proj");

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    fn replay(tokens: &[&str]) -> Result<Vec<String>, ReplayRefusal> {
        replay_args(&argv(tokens), SID, CWD)
    }

    #[test]
    fn resume_argv_keeps_only_the_allowlist_and_drops_selection_plumbing_and_prompt() {
        assert_eq!(
            replay(&[
                "--model",
                "opus",
                "--resume",
                "old-id",
                "--append-system-prompt-file",
                "/tmp/ctx.md",
                "--add-dir",
                "/srv/a",
                "/srv/b",
                "--verbose",
                "--session-id",
                "pinned",
                "-c",
                "fix the build",
            ])
            .unwrap(),
            argv(&[
                "--model",
                "opus",
                "--add-dir",
                "/srv/a",
                "/srv/b",
                "--verbose"
            ])
        );
    }

    /// Review r1, M2: flags that load code or config are never replayed, with
    /// their values; unknown flags are dropped too.
    #[test]
    fn resume_argv_drops_flags_that_load_code_or_config_and_unknown_flags() {
        assert_eq!(
            replay(&[
                "--settings",
                "/tmp/s.json",
                "--mcp-config",
                "/tmp/a.json",
                "/tmp/b.json",
                "--plugin-dir",
                "/tmp/p",
                "--system-prompt-file",
                "/tmp/sp.md",
                "--agents",
                "x",
                "--permission-prompt-tool",
                "t",
                "--dangerously-skip-permissions",
                "--future-flag",
                "value",
                "--other=v",
                "--model",
                "opus",
            ])
            .unwrap(),
            argv(&["--model", "opus"])
        );
    }

    #[test]
    fn resume_argv_a_bare_launch_relaunches_with_no_flags() {
        assert_eq!(replay(&[]).unwrap(), Vec::<String>::new());
        // An initial prompt alone is dropped, not replayed.
        assert_eq!(
            replay(&["write a poem; rm -rf ~"]).unwrap(),
            Vec::<String>::new()
        );
        // Everything after `--` is positional.
        assert_eq!(
            replay(&["--verbose", "--", "--model", "x"]).unwrap(),
            argv(&["--verbose"])
        );
    }

    #[test]
    fn resume_argv_inline_values_and_optional_resume() {
        assert_eq!(
            replay(&["--model=sonnet", "--resume=abc", "-r", "--verbose"]).unwrap(),
            argv(&["--model=sonnet", "--verbose"])
        );
        // `-r` followed by a plain token takes it as the id (dropped with it).
        assert_eq!(replay(&["-r", "abc", "--ide"]).unwrap(), argv(&["--ide"]));
    }

    /// A variadic flag takes every following plain token, as `claude`'s own
    /// parser does — so a prompt typed AFTER `--add-dir` is one more directory,
    /// and one with a space refuses the replay rather than being guessed away.
    #[test]
    fn resume_argv_a_variadic_flag_reads_like_claude_does() {
        assert_eq!(
            replay(&["--add-dir", "/a", "/b", "--verbose"]).unwrap(),
            argv(&["--add-dir", "/a", "/b", "--verbose"])
        );
        assert_eq!(
            replay(&["--add-dir", "/a", "fix the build"]),
            Err(ReplayRefusal::NotShellInert {
                index: 2,
                flag: Some("--add-dir".into())
            })
        );
    }

    /// Review r1, nit: a relative `--add-dir` resolves against the LAUNCH
    /// directory; without an absolute one the replay is refused.
    #[test]
    fn resume_argv_relative_add_dir_resolves_against_the_launch_directory() {
        assert_eq!(
            replay(&["--add-dir", "../lib", "--add-dir=docs"]).unwrap(),
            argv(&[
                "--add-dir",
                "/home/op/proj/../lib",
                "--add-dir=/home/op/proj/docs"
            ])
        );
        for cwd in [None, Some("relative/cwd")] {
            assert_eq!(
                replay_args(&argv(&["--add-dir", "lib"]), SID, cwd),
                Err(ReplayRefusal::RelativePath { index: 1 })
            );
        }
        // An absolute one needs no launch directory.
        assert_eq!(
            replay_args(&argv(&["--add-dir", "/abs"]), SID, None).unwrap(),
            argv(&["--add-dir", "/abs"])
        );
    }

    /// D11's gate: a token that would be TYPED and is not shell-inert refuses
    /// the whole replay — it is never escaped, and never silently dropped.
    #[test]
    fn resume_argv_an_unsafe_surviving_token_refuses_the_whole_replay() {
        for bad in [
            "/srv/my project",
            "/$(touch /tmp/pwned)",
            "/a;b",
            "/`id`",
            "/x|y",
            "~/dir",
            "@splat",
            "/a,b",
            "/50%",
            "/a\nb",
            "/q\"uote",
        ] {
            assert!(
                matches!(
                    replay(&["--verbose", "--add-dir", bad]),
                    Err(ReplayRefusal::NotShellInert { index: 2, .. })
                        | Err(ReplayRefusal::RelativePath { index: 2 })
                ),
                "{bad:?} must refuse the replay"
            );
        }
        assert_eq!(
            replay(&["--model", "=opus"]),
            Err(ReplayRefusal::NotShellInert {
                index: 1,
                flag: Some("--model".into())
            }),
            "a leading `=` is zsh's EQUALS expansion"
        );
        let long = "a".repeat(MAX_REPLAY_TOKEN_BYTES + 1);
        assert_eq!(
            replay(&["--model", &long]),
            Err(ReplayRefusal::NotShellInert {
                index: 1,
                flag: Some("--model".into())
            })
        );
        // An empty value is not inert either.
        assert!(matches!(
            replay(&["--model", ""]),
            Err(ReplayRefusal::NotShellInert { index: 1, .. })
        ));
    }

    #[test]
    fn resume_argv_is_shell_inert_requires_a_safe_first_character() {
        for ok in ["opus", "-x", "/srv", "./x", "--model=a"] {
            assert!(is_shell_inert(ok), "{ok}");
        }
        for bad in ["", "=x", ":x", "+x", "_x", "a b"] {
            assert!(!is_shell_inert(bad), "{bad:?}");
        }
    }

    /// A non-inert token that is NOT typed — the prompt, a plumbing value, an
    /// old session id, a dropped flag's value — does not block the relaunch.
    #[test]
    fn resume_argv_unsafe_tokens_that_are_dropped_do_not_refuse() {
        assert_eq!(
            replay(&[
                "--append-system-prompt",
                "You are inside the runner.\nBe careful; really.",
                "--resume",
                "../$(bad)",
                "do the thing && more",
                "--settings",
                "{\"env\":{\"K\":\"v\"}}",
                "--model",
                "opus",
            ])
            .unwrap(),
            argv(&["--model", "opus"])
        );
    }

    #[test]
    fn resume_argv_a_missing_or_unsafe_session_id_refuses() {
        for bad in ["", "abc def", "a;b", "$(id)", "x\n"] {
            assert_eq!(
                replay_args(&[], bad, CWD),
                Err(ReplayRefusal::InvalidSessionId),
                "{bad:?}"
            );
            assert_eq!(
                derive_resume_flags(&[], bad, CWD),
                None,
                "nothing to persist"
            );
        }
    }

    /// Review r1, nit: `-pc` / `-rID` would let print mode or a second session
    /// selector through as an "unknown flag".
    #[test]
    fn resume_argv_combined_short_flags_print_mode_and_oversize_refuse() {
        for combined in ["-pc", "-rabc", "-vd"] {
            assert_eq!(
                replay(&["--model", "x", combined]),
                Err(ReplayRefusal::CombinedShortFlags { index: 2 }),
                "{combined}"
            );
        }
        assert_eq!(replay(&["-p", "hello"]), Err(ReplayRefusal::PrintMode));
        assert_eq!(replay(&["--print"]), Err(ReplayRefusal::PrintMode));
        let many: Vec<String> = (0..=MAX_ARGV_TOKENS).map(|_| "--verbose".into()).collect();
        assert_eq!(replay_args(&many, SID, CWD), Err(ReplayRefusal::TooLong));
    }

    #[test]
    fn resume_argv_refusals_never_carry_token_text() {
        let err = replay(&["--model", "sk-secret-token value"]).unwrap_err();
        assert!(!err.to_string().contains("sk-secret"), "{err}");
        assert!(err.to_string().contains("--model"), "names the flag: {err}");
        let err = replay(&["--add-dir", "/a/$(id)"]).unwrap_err();
        assert!(!err.to_string().contains("$(id)"), "{err}");
    }

    #[test]
    fn resume_argv_derive_persists_flags_or_a_refusal_never_the_argv() {
        assert_eq!(
            derive_resume_flags(&argv(&["--model", "opus", "secret prompt"]), SID, CWD),
            Some(ResumeFlags::Replay {
                flags: argv(&["--model", "opus"])
            })
        );
        let refused = derive_resume_flags(&argv(&["--model", "a b"]), SID, CWD).unwrap();
        let ResumeFlags::Refused { reason } = &refused else {
            panic!("{refused:?}")
        };
        assert!(!reason.contains("a b"), "{reason}");
        let json = serde_json::to_string(&refused).unwrap();
        assert!(!json.contains("a b"), "{json}");
    }

    #[test]
    fn resume_argv_relaunch_wire_fields_three_ways() {
        assert_eq!(Relaunch::for_record(None).wire_fields(), (None, None));
        let replay = ResumeFlags::Replay {
            flags: argv(&["--model", "opus"]),
        };
        assert_eq!(
            Relaunch::for_record(Some(&replay)).wire_fields(),
            (Some(argv(&["--model", "opus"])), None)
        );
        // A refusal falls back to the DEFAULT resume: no args, the refusal
        // for the warn line.
        let refused = ResumeFlags::Refused {
            reason: "launch argument 2 (flag --add-dir) is not shell-inert".into(),
        };
        let (args, refusal) = Relaunch::for_record(Some(&refused)).wire_fields();
        assert_eq!(args, None, "the extra flags are dropped, the resume is not");
        assert!(refusal.unwrap().contains("--add-dir"));
        // A hand-edited record cannot get a token typed.
        let tampered = ResumeFlags::Replay {
            flags: argv(&["--model", "$(id)"]),
        };
        assert!(matches!(
            Relaunch::for_record(Some(&tampered)),
            Relaunch::Fallback(_)
        ));
        // An empty replay still relaunches — with no flags.
        let empty = ResumeFlags::Replay { flags: vec![] };
        assert_eq!(
            Relaunch::for_record(Some(&empty)).wire_fields(),
            (Some(vec![]), None)
        );
    }

    /// Review r2, N6: a value-taking allowlisted flag with no value, or with
    /// a flag-shaped value, is refused — never replayed bare.
    #[test]
    fn resume_argv_a_value_flag_without_a_value_is_refused() {
        for bad in [
            &["--model"][..],
            &["--model", "--resume"][..],
            &["--model="][..],
            &["--model=-x"][..],
            &["--add-dir"][..],
            &["--add-dir", "--verbose"][..],
            &["--fallback-model", "-c"][..],
        ] {
            assert!(
                matches!(
                    replay(bad),
                    Err(ReplayRefusal::MissingValue { index: 0, .. })
                ),
                "{bad:?}: {:?}",
                replay(bad)
            );
        }
        // An optional value may be absent.
        assert_eq!(
            replay(&["--debug", "--verbose"]).unwrap(),
            argv(&["--debug", "--verbose"])
        );
    }

    /// Review r2, permission rule: one known mode, the LAST one; never the
    /// dangerous-skip flags.
    #[test]
    fn resume_argv_replays_exactly_one_known_permission_mode() {
        assert_eq!(
            replay(&[
                "--permission-mode",
                "plan",
                "--model",
                "m",
                "--permission-mode=acceptEdits"
            ])
            .unwrap(),
            argv(&["--model", "m", "--permission-mode=acceptEdits"])
        );
        for mode in PERMISSION_MODES {
            assert_eq!(
                replay(&["--permission-mode", mode]).unwrap(),
                argv(&["--permission-mode", mode])
            );
        }
        for bad in ["dontAsk", "BYPASS", "auto"] {
            assert_eq!(
                replay(&["--permission-mode", bad]),
                Err(ReplayRefusal::UnknownPermissionMode { index: 1 })
            );
        }
        // An unknown EARLIER mode refuses too: every mode must be known.
        assert!(replay(&["--permission-mode", "weird", "--permission-mode", "plan"]).is_err());
        assert_eq!(
            replay(&[
                "--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions"
            ])
            .unwrap(),
            Vec::<String>::new(),
            "never replayed"
        );
    }

    /// Review r2, N5: on Windows a drive-letter `--add-dir` is absolute, and a
    /// relative one under an MSYS launch directory is refused.
    #[test]
    fn resume_argv_windows_add_dir_paths() {
        let w = |a: &[&str], cwd: Option<&str>| replay_args_on(&argv(a), SID, cwd, true);
        assert_eq!(
            w(&["--add-dir", "C:/work", "--add-dir=D:/x"], None).unwrap(),
            argv(&["--add-dir", "C:/work", "--add-dir=D:/x"])
        );
        assert_eq!(
            w(&["--add-dir", "lib"], Some("C:/proj")).unwrap(),
            argv(&["--add-dir", "C:/proj/lib"])
        );
        assert_eq!(
            w(&["--add-dir", "lib"], Some("/c/proj")),
            Err(ReplayRefusal::MsysLaunchDirectory { index: 1 })
        );
        assert_eq!(
            w(&["--add-dir", "lib"], None),
            Err(ReplayRefusal::RelativePath { index: 1 })
        );
        // Off Windows a drive letter is just a relative name.
        assert_eq!(
            replay_args_on(
                &argv(&["--add-dir", "C:/work"]),
                SID,
                Some("/home/p"),
                false
            )
            .unwrap(),
            argv(&["--add-dir", "/home/p/C:/work"])
        );
    }

    /// Review r2, N1: the identity shims export only the allowlisted flags,
    /// from a copy of this module's allowlist. Pin the copies equal to it.
    #[test]
    fn the_shim_allowlists_match_the_runners() {
        const BASH: &str = include_str!("../../resources/intercept/identity_shim.bash");
        const NATIVE: &str = include_str!("../bin/qontinui_shim.rs");
        let bash_list = |var: &str| -> Vec<String> {
            let line = BASH
                .lines()
                .find(|l| l.starts_with(&format!("{var}=\"")))
                .unwrap_or_else(|| panic!("{var} is not in the bash shim"));
            let inner = line.split('"').nth(1).unwrap();
            inner.split_whitespace().map(str::to_string).collect()
        };
        let native_list = |name: &str| -> Vec<String> {
            let (_, rest) = NATIVE
                .split_once(&format!("const {name}: &[&str] = &["))
                .unwrap_or_else(|| panic!("{name} is not in the native shim"));
            let (body, _) = rest.split_once("];").unwrap();
            body.split(',')
                .map(|t| t.trim().trim_matches('"').to_string())
                .filter(|t| !t.is_empty())
                .collect()
        };
        for (suffix, runner) in [
            ("ONE", REPLAY_ALLOWLIST_ONE),
            ("MANY", REPLAY_ALLOWLIST_MANY),
            ("OPTIONAL", REPLAY_ALLOWLIST_OPTIONAL),
            ("BOOL", REPLAY_ALLOWLIST_BOOL),
        ] {
            let runner: Vec<String> = runner.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                bash_list(&format!("QS_REPLAY_{suffix}")),
                runner,
                "bash {suffix}"
            );
            assert_eq!(
                native_list(&format!("REPLAY_ALLOWLIST_{suffix}")),
                runner,
                "native {suffix}"
            );
        }
    }

    #[test]
    fn resume_argv_parse_value_is_bounded_and_never_partial() {
        assert_eq!(
            parse_argv_value(&serde_json::json!(["--model", "opus"])),
            Some(argv(&["--model", "opus"]))
        );
        assert_eq!(parse_argv_value(&serde_json::json!([])), Some(vec![]));
        assert_eq!(parse_argv_value(&serde_json::json!(["a", 1])), None);
        assert_eq!(parse_argv_value(&serde_json::json!("--model")), None);
        let too_many: Vec<&str> = vec!["a"; MAX_ARGV_TOKENS + 1];
        assert_eq!(parse_argv_value(&serde_json::json!(too_many)), None);
        let too_long = "a".repeat(MAX_ARGV_TOKEN_BYTES + 1);
        assert_eq!(parse_argv_value(&serde_json::json!([too_long])), None);
        // Bytes, not characters.
        let multibyte = "é".repeat(MAX_ARGV_TOKEN_BYTES / 2 + 1);
        assert_eq!(parse_argv_value(&serde_json::json!([multibyte])), None);
    }
}
