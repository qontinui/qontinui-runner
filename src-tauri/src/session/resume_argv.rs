//! The persisted launch argv of a provider session, and the `--resume`
//! relaunch built from it — with the shell-inert gate the relaunch must pass.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 6, D11. When a pane's PTY holder itself dies (tier 1 lost), the
//! session in it is relaunched in a fresh pane as `claude <flags> --resume
//! <id>`, TYPED INTO A SHELL by the webview's restore. The flags are the ones
//! the session was launched with (`--model`, `--add-dir`, …), so a relaunched
//! session runs the way its operator started it rather than under a default.
//!
//! # Where the argv comes from
//!
//! The identity shim (`resources/intercept/identity_shim.bash`, and the native
//! `qontinui-shim` on Windows) sees the argv a `claude` launch received and
//! exports it to that `claude` as [`LAUNCH_ARGV_ENV`]; the bundled
//! `SessionStart` hook — a child of that `claude` — puts it in its OSC 9999
//! `session_start` report (`terminal::session_start_sideband`); the runner
//! persists it on the session's lifecycle record
//! (`SessionLifecycleStore::set_launch_argv`).
//!
//! # The gate: reject the whole replay, never escape it
//!
//! Adopted from tty7 (`cli_agent.rs`, idea only): every token that SURVIVES
//! the filter below must be shell-inert — characters from [`is_inert_char`]
//! only — or the REPLAY is refused outright. Escaping is not attempted: the
//! command is typed into whichever shell the pane runs (bash, zsh, PowerShell),
//! and a quoting scheme correct for one is an injection for another.
//!
//! A refused replay does NOT mean no relaunch (coordinator decision,
//! 2026-10-15): the session is still resumed with the DEFAULT resume command —
//! exactly the pre-Phase-6 behaviour — and only the extra flags are dropped
//! ([`Relaunch::Fallback`]). The refusal is logged at warn naming the flag the
//! offending token belongs to, never its value. Only a missing or unsafe
//! session id means no relaunch at all.
//!
//! # What survives, and what is dropped
//!
//! - **Session selection** (`--resume`/`-r`, `--continue`/`-c`,
//!   `--session-id`, `--fork-session`) — dropped: the relaunch adds its own
//!   `--resume <id>`, and a second selector would fight it.
//! - **Runner plumbing** (`--append-system-prompt`,
//!   `--append-system-prompt-file`) — dropped: the runner's shell integration
//!   adds the session context again in the new pane, and its inline form is a
//!   multi-line value no shell-inert gate would pass.
//! - **Positional arguments** — dropped: `claude`'s positional is the INITIAL
//!   PROMPT, and replaying it into a resumed conversation would send it again.
//! - **Known flags** — kept with their values (see [`flag_arity`]).
//! - **Unknown flags** — kept only when unambiguous: a `--flag=value` token, or
//!   a bare flag followed by another flag or by nothing. A bare unknown flag
//!   followed by a plain token cannot be split into "flag value" and "initial
//!   prompt" without knowing the flag, so the relaunch is refused rather than
//!   guessed ([`ReplayRefusal::AmbiguousFlag`]).
//! - **Print mode** (`-p`/`--print`) — refused: a non-interactive run has no
//!   conversation to come back to in a pane.
//!
//! Refusals never carry a token's text: an argv is operator input and can
//! hold anything; the refusal names a position and, when it is itself inert,
//! the FLAG the token belongs to.

use crate::session::session_id::is_valid_session_id;

/// The environment variable the identity shim sets on the `claude` it launches:
/// a JSON array of the argv that launch received (minus the runner plumbing
/// the shim strips first). Read by the bundled `SessionStart` hook.
pub const LAUNCH_ARGV_ENV: &str = "QONTINUI_LAUNCH_ARGV";

/// Most tokens a persisted argv may carry. A longer one is not stored (the
/// report's `argv` is dropped whole, never truncated — a truncated argv is a
/// different argv).
pub const MAX_ARGV_TOKENS: usize = 64;

/// Longest single token a persisted argv may carry, in characters.
pub const MAX_ARGV_TOKEN_CHARS: usize = 1024;

/// Longest token a REPLAY may type, in bytes. A replayed token is a flag or a
/// short value; anything longer is not something an operator typed by hand.
pub const MAX_REPLAY_TOKEN_BYTES: usize = 256;

/// Why a persisted argv cannot be replayed. Typed so the webview can say why a
/// session was not relaunched, and so no variant carries a token's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayRefusal {
    /// The session id is not shell-safe (`session_id::is_valid_session_id`).
    InvalidSessionId,
    /// Token `index` survives the filter but is not shell-inert. `flag` is the
    /// flag it belongs to (itself, or the flag whose value it is), named only
    /// when that name is itself inert — never the token's value.
    NotShellInert { index: usize, flag: Option<String> },
    /// Bare unknown flag at `index` is followed by a plain token: its value
    /// and the initial prompt cannot be told apart.
    AmbiguousFlag { index: usize, flag: Option<String> },
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
            ReplayRefusal::AmbiguousFlag { index, flag } => write!(
                f,
                "launch argument {index} (flag {}) is a flag this runner does not know, \
                 followed by a value it cannot tell from a prompt",
                flag.as_deref().unwrap_or("<unnamed>")
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

/// Whether `token` may be typed into a shell as one argument as-is.
pub fn is_shell_inert(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_REPLAY_TOKEN_BYTES && token.chars().all(is_inert_char)
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
    Keep,
    Drop,
    RefusePrint,
}

/// The `claude` flags this runner knows. Anything else is "unknown" and is
/// kept only when unambiguous (module docs).
fn flag_arity(flag: &str) -> Option<(Arity, Disposition)> {
    use Arity::*;
    use Disposition::*;
    Some(match flag {
        // Session selection — the relaunch supplies its own.
        "--resume" | "-r" => (Optional, Drop),
        "--continue" | "-c" | "--fork-session" => (Bool, Drop),
        "--session-id" => (One, Drop),
        // Runner plumbing — re-added by the shell integration in the new pane.
        "--append-system-prompt" | "--append-system-prompt-file" => (One, Drop),
        // Print mode — not a conversation a pane can resume.
        "--print" | "-p" => (Bool, RefusePrint),
        // Value flags kept as the operator gave them.
        "--model"
        | "--fallback-model"
        | "--permission-mode"
        | "--agent"
        | "--agents"
        | "--settings"
        | "--setting-sources"
        | "--system-prompt"
        | "--system-prompt-file"
        | "--plugin-dir"
        | "--output-format"
        | "--input-format"
        | "--max-turns"
        | "--permission-prompt-tool" => (One, Keep),
        "--add-dir" | "--allowedTools" | "--allowed-tools" | "--disallowedTools"
        | "--disallowed-tools" | "--mcp-config" | "--betas" => (Many, Keep),
        "--debug" => (Optional, Keep),
        // Boolean flags kept as given.
        "--dangerously-skip-permissions"
        | "--allow-dangerously-skip-permissions"
        | "--verbose"
        | "--ide"
        | "--strict-mcp-config"
        | "--chrome"
        | "--no-chrome"
        | "--include-partial-messages"
        | "--replay-user-messages" => (Bool, Keep),
        _ => return None,
    })
}

fn is_flag(token: &str) -> bool {
    token.len() > 1 && token.starts_with('-')
}

/// The flags to type between `claude` and `--resume <session_id>` when
/// relaunching a session launched with `launch_argv` — or why it must not be
/// relaunched. `Ok(vec![])` is a launch with no replayable flags (a bare
/// `claude`), which still relaunches.
pub fn replay_args(launch_argv: &[String], session_id: &str) -> Result<Vec<String>, ReplayRefusal> {
    if !is_valid_session_id(session_id) {
        return Err(ReplayRefusal::InvalidSessionId);
    }
    if launch_argv.len() > MAX_ARGV_TOKENS {
        return Err(ReplayRefusal::TooLong);
    }
    // (position, token, the flag it belongs to — for the refusal's message).
    let mut kept: Vec<(usize, String, &str)> = Vec::new();
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
        let (name, inline_value) = match token.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (token.as_str(), false),
        };
        match flag_arity(name) {
            Some((_, Disposition::RefusePrint)) => return Err(ReplayRefusal::PrintMode),
            Some((arity, disposition)) => {
                let keep = disposition == Disposition::Keep;
                if keep {
                    kept.push((i, token.clone(), name));
                }
                i += 1;
                if inline_value {
                    continue;
                }
                let takes = |t: &String| !is_flag(t) && t != "--";
                match arity {
                    Arity::Bool => {}
                    Arity::One => {
                        if let Some(v) = launch_argv.get(i) {
                            if keep {
                                kept.push((i, v.clone(), name));
                            }
                            i += 1;
                        }
                    }
                    Arity::Optional => {
                        if launch_argv.get(i).is_some_and(takes) {
                            if keep {
                                kept.push((i, launch_argv[i].clone(), name));
                            }
                            i += 1;
                        }
                    }
                    Arity::Many => {
                        while launch_argv.get(i).is_some_and(takes) {
                            if keep {
                                kept.push((i, launch_argv[i].clone(), name));
                            }
                            i += 1;
                        }
                    }
                }
            }
            None => {
                // Unknown flag: unambiguous only as `--flag=value`, or bare
                // when the next token is another flag or nothing.
                if !inline_value
                    && launch_argv
                        .get(i + 1)
                        .is_some_and(|t| !is_flag(t) && t != "--")
                {
                    return Err(ReplayRefusal::AmbiguousFlag {
                        index: i,
                        flag: inert_name(name),
                    });
                }
                kept.push((i, token.clone(), name));
                i += 1;
            }
        }
    }
    for (index, token, flag) in &kept {
        if !is_shell_inert(token) {
            return Err(ReplayRefusal::NotShellInert {
                index: *index,
                flag: inert_name(flag),
            });
        }
    }
    Ok(kept.into_iter().map(|(_, t, _)| t).collect())
}

/// A flag name fit for a log line: only when it is itself shell-inert (a flag
/// token can be anything an operator typed).
fn inert_name(name: &str) -> Option<String> {
    is_shell_inert(name).then(|| name.to_string())
}

/// How a session with persisted argv `launch_argv` is relaunched — the two
/// derived fields the restore set carries (`terminal_session_list_open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaunch {
    /// No argv was ever reported: the default resume command, exactly as
    /// before argv persistence existed.
    Default,
    /// Replay these flags before `--resume <id>`.
    Replay(Vec<String>),
    /// An argv was reported and must not be replayed: the DEFAULT resume
    /// command is used instead, and only the extra flags are dropped.
    Fallback(ReplayRefusal),
    /// The session id is not shell-safe: NO relaunch.
    NoRelaunch,
}

impl Relaunch {
    /// Decide for one record.
    pub fn for_record(launch_argv: Option<&[String]>, session_id: &str) -> Self {
        match launch_argv {
            None => Relaunch::Default,
            Some(argv) => match replay_args(argv, session_id) {
                Ok(flags) => Relaunch::Replay(flags),
                Err(ReplayRefusal::InvalidSessionId) => Relaunch::NoRelaunch,
                Err(why) => Relaunch::Fallback(why),
            },
        }
    }

    /// `(resumeArgs, resumeRefusal)` for the wire: both absent for
    /// [`Relaunch::Default`], so a webview that predates them — and a record
    /// that predates argv persistence — behaves exactly as before. A
    /// [`Relaunch::Fallback`] carries NO args (the webview then types the
    /// default resume) plus the refusal, for its log line. [`Relaunch::NoRelaunch`]
    /// carries neither: the webview's own session-id gate (`skip-invalid`)
    /// already refuses that record.
    pub fn wire_fields(&self) -> (Option<Vec<String>>, Option<String>) {
        match self {
            Relaunch::Default | Relaunch::NoRelaunch => (None, None),
            Relaunch::Replay(flags) => (Some(flags.clone()), None),
            Relaunch::Fallback(why) => (None, Some(why.to_string())),
        }
    }
}

/// Parse the JSON array [`LAUNCH_ARGV_ENV`] carries into a bounded argv:
/// `None` unless it is an array of at most [`MAX_ARGV_TOKENS`] strings of at
/// most [`MAX_ARGV_TOKEN_CHARS`] characters each. Never a partial argv.
pub fn parse_argv_value(value: &serde_json::Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    if items.len() > MAX_ARGV_TOKENS {
        return None;
    }
    items
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| s.chars().count() <= MAX_ARGV_TOKEN_CHARS)
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn resume_argv_keeps_known_flags_and_drops_selection_plumbing_and_prompt() {
        let launch = argv(&[
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
        ]);
        assert_eq!(
            replay_args(&launch, SID).unwrap(),
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

    /// A variadic flag takes every following plain token, as `claude`'s own
    /// parser does — so a prompt typed AFTER `--add-dir` is one more directory,
    /// and one with a space refuses the replay rather than being guessed away.
    #[test]
    fn resume_argv_a_variadic_flag_reads_like_claude_does() {
        assert_eq!(
            replay_args(&argv(&["--add-dir", "/a", "/b", "--verbose"]), SID).unwrap(),
            argv(&["--add-dir", "/a", "/b", "--verbose"])
        );
        assert_eq!(
            replay_args(&argv(&["--add-dir", "/a", "fix the build"]), SID),
            Err(ReplayRefusal::NotShellInert {
                index: 2,
                flag: Some("--add-dir".into())
            })
        );
    }

    #[test]
    fn resume_argv_a_bare_launch_relaunches_with_no_flags() {
        assert_eq!(replay_args(&[], SID).unwrap(), Vec::<String>::new());
        // An initial prompt alone is dropped, not replayed.
        assert_eq!(
            replay_args(&argv(&["write a poem; rm -rf ~"]), SID).unwrap(),
            Vec::<String>::new()
        );
        // Everything after `--` is positional.
        assert_eq!(
            replay_args(&argv(&["--verbose", "--", "--model", "x"]), SID).unwrap(),
            argv(&["--verbose"])
        );
    }

    #[test]
    fn resume_argv_inline_values_and_optional_resume() {
        assert_eq!(
            replay_args(
                &argv(&["--model=sonnet", "--resume=abc", "-r", "--verbose"]),
                SID
            )
            .unwrap(),
            argv(&["--model=sonnet", "--verbose"])
        );
        // `-r` followed by a plain token takes it as the id (dropped with it).
        assert_eq!(
            replay_args(&argv(&["-r", "abc", "--ide"]), SID).unwrap(),
            argv(&["--ide"])
        );
    }

    /// D11's gate: a token that would be TYPED and is not shell-inert refuses
    /// the whole relaunch — it is never escaped, and never silently dropped.
    #[test]
    fn resume_argv_an_unsafe_surviving_token_refuses_the_whole_replay() {
        for bad in [
            "/srv/my project",
            "$(touch /tmp/pwned)",
            "a;b",
            "`id`",
            "x|y",
            "~/dir",
            "@splat",
            "a,b",
            "50%",
            "a\nb",
            "q\"uote",
            "",
        ] {
            let launch = argv(&["--verbose", "--add-dir", bad]);
            assert_eq!(
                replay_args(&launch, SID),
                Err(ReplayRefusal::NotShellInert {
                    index: 2,
                    flag: Some("--add-dir".into())
                }),
                "{bad:?} must refuse the replay"
            );
        }
        // Over the length bound.
        let long = "a".repeat(MAX_REPLAY_TOKEN_BYTES + 1);
        assert_eq!(
            replay_args(&argv(&["--model", &long]), SID),
            Err(ReplayRefusal::NotShellInert {
                index: 1,
                flag: Some("--model".into())
            })
        );
    }

    /// A non-inert token that is NOT typed — the prompt, a plumbing value, an
    /// old session id — does not block the relaunch.
    #[test]
    fn resume_argv_unsafe_tokens_that_are_dropped_do_not_refuse() {
        let launch = argv(&[
            "--append-system-prompt",
            "You are inside the runner.\nBe careful; really.",
            "--resume",
            "../$(bad)",
            "do the thing && more",
            "--model",
            "opus",
        ]);
        assert_eq!(
            replay_args(&launch, SID).unwrap(),
            argv(&["--model", "opus"])
        );
    }

    #[test]
    fn resume_argv_a_missing_or_unsafe_session_id_refuses() {
        for bad in ["", "abc def", "a;b", "$(id)", "x\n"] {
            assert_eq!(
                replay_args(&[], bad),
                Err(ReplayRefusal::InvalidSessionId),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn resume_argv_unknown_flags_are_kept_only_when_unambiguous() {
        assert_eq!(
            replay_args(&argv(&["--future-flag", "--verbose", "--other=v"]), SID).unwrap(),
            argv(&["--future-flag", "--verbose", "--other=v"])
        );
        assert_eq!(
            replay_args(&argv(&["--future-flag"]), SID).unwrap(),
            argv(&["--future-flag"])
        );
        assert_eq!(
            replay_args(&argv(&["--model", "x", "--future-flag", "value"]), SID),
            Err(ReplayRefusal::AmbiguousFlag {
                index: 2,
                flag: Some("--future-flag".into())
            })
        );
    }

    #[test]
    fn resume_argv_print_mode_and_oversize_refuse() {
        assert_eq!(
            replay_args(&argv(&["-p", "hello"]), SID),
            Err(ReplayRefusal::PrintMode)
        );
        assert_eq!(
            replay_args(&argv(&["--print"]), SID),
            Err(ReplayRefusal::PrintMode)
        );
        let many: Vec<String> = (0..=MAX_ARGV_TOKENS).map(|_| "--verbose".into()).collect();
        assert_eq!(replay_args(&many, SID), Err(ReplayRefusal::TooLong));
    }

    #[test]
    fn resume_argv_refusals_never_carry_token_text() {
        let secret = "sk-secret-token value";
        let err = replay_args(&argv(&["--model", secret]), SID).unwrap_err();
        assert!(!err.to_string().contains("sk-secret"), "{err}");
        // A flag NAME that is not inert is not named either.
        let err = replay_args(&argv(&["--x$(id)=v"]), SID).unwrap_err();
        assert!(!err.to_string().contains("$(id)"), "{err}");
    }

    #[test]
    fn resume_argv_relaunch_wire_fields_three_ways() {
        assert_eq!(Relaunch::for_record(None, SID).wire_fields(), (None, None));
        assert_eq!(
            Relaunch::for_record(Some(&argv(&["--model", "opus", "hi"])), SID).wire_fields(),
            (Some(argv(&["--model", "opus"])), None)
        );
        // An unsafe argv falls back to the DEFAULT resume: no args (the
        // webview types its default command), and the refusal — naming the
        // flag, never the value — for the warn line.
        let relaunch = Relaunch::for_record(Some(&argv(&["--add-dir", "a b"])), SID);
        assert!(matches!(relaunch, Relaunch::Fallback(_)), "{relaunch:?}");
        let (args, refusal) = relaunch.wire_fields();
        assert_eq!(args, None, "the extra flags are dropped, the resume is not");
        let refusal = refusal.unwrap();
        assert!(refusal.contains("--add-dir"), "{refusal}");
        assert!(!refusal.contains("a b"), "{refusal}");
        // A missing or unsafe session id: no relaunch at all.
        assert_eq!(
            Relaunch::for_record(Some(&argv(&["--model", "opus"])), "a b"),
            Relaunch::NoRelaunch
        );
        assert_eq!(
            Relaunch::for_record(Some(&argv(&["--model", "opus"])), ""),
            Relaunch::NoRelaunch
        );
        // A persisted argv with no flags still relaunches — with none.
        assert_eq!(
            Relaunch::for_record(Some(&[]), SID).wire_fields(),
            (Some(vec![]), None)
        );
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
        let too_long = "a".repeat(MAX_ARGV_TOKEN_CHARS + 1);
        assert_eq!(parse_argv_value(&serde_json::json!([too_long])), None);
    }
}
