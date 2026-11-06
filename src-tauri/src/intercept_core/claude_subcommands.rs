//! Which `claude` invocations are NOT sessions, so the identity shim must leave
//! their argv alone.
//!
//! The session-restore identity shim appends `--settings`, `--mcp-config` and
//! `--session-id` to a `claude` argv. Those are options of the ROOT command —
//! the one that starts a session. A subcommand (`claude plugin eval …`,
//! `claude mcp list`, `claude doctor`, …) is parsed by its own option table, so
//! the appended flags are rejected with exit status 1 (`error: unknown option
//! '--settings'`) or, where tolerated, hand a non-session tool a live coord-mcp
//! config it has no use for (coord finding 877edd19).
//!
//! ## Why a list of PASSTHROUGH subcommands rather than an allowlist
//! The invocations that should be injected are a bare `claude`, flags only, or
//! flags plus a prompt — and a prompt is a positional, exactly like a
//! subcommand name. `claude "fix the bug"` and `claude plugin` differ only in
//! the WORD, so there is no allowlist to write: the only test that can tell them
//! apart is "is the first positional a subcommand claude knows". Claude's own
//! parser makes the same decision the same way — a first operand matching a
//! command name dispatches to it — so this list mirrors `claude --help`'s
//! `Commands:` section (2.1.318), plus `help` (the parser's implicit command)
//! and `config` / `migrate-installer` (commands of earlier releases still
//! installed on some boxes). A subcommand claude adds later and this list lacks
//! is injected as before, which is today's behaviour, not a regression.
//!
//! ## Option values
//! A root option that takes a value (`--add-dir config`, `--agent doctor`,
//! `--model x`) consumes the next token, so that token is not a subcommand —
//! reading it as one would pass a SESSION through unpinned. The scan therefore
//! skips the token after any option in [`CLAUDE_VALUE_OPTIONS`] (from the same
//! `claude --help`, required- and optional-value options alike, plus the
//! undocumented `--append-system-prompt-file` / `--system-prompt-file`), unless
//! that token is itself `-`-prefixed — as claude's parser does for an
//! optional value. A variadic option (`--add-dir a b`) is skipped one value
//! deep, so `claude --add-dir a mcp` still reads `mcp` as the subcommand, as
//! claude itself would not; and an option claude adds later is not in the
//! table. Both err toward an un-injected argv, never a broken launch.
//!
//! Twins: `resources/intercept/identity_shim.bash` and `identity_shim.cmd`
//! carry the same list as a literal; a test in the shim materializer holds them
//! equal to [`CLAUDE_NON_SESSION_SUBCOMMANDS`] and [`CLAUDE_VALUE_OPTIONS`].

/// `claude` subcommands that are not sessions. Case-sensitive, as claude's
/// parser is.
pub const CLAUDE_NON_SESSION_SUBCOMMANDS: &[&str] = &[
    "agents",
    "attach",
    "auth",
    "auto-mode",
    "config",
    "doctor",
    "gateway",
    "help",
    "import",
    "install",
    "kill",
    "logs",
    "mcp",
    "migrate-installer",
    "plugin",
    "plugins",
    "purge",
    "respawn",
    "rm",
    "setup-token",
    "stop",
    "ultrareview",
    "update",
    "upgrade",
];

/// Root `claude` options that take a value (required or optional), whose
/// value token the first-positional scan skips. Exact match: the inline
/// `--opt=value` form is one `-`-prefixed token and needs no skip.
pub const CLAUDE_VALUE_OPTIONS: &[&str] = &[
    "--add-dir",
    "--agent",
    "--agents",
    "--allowedTools",
    "--allowed-tools",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--autocompact",
    "--betas",
    "--cloud",
    "-d",
    "--debug",
    "--debug-file",
    "--disallowedTools",
    "--disallowed-tools",
    "--effort",
    "--environment",
    "--fallback-model",
    "--file",
    "--from-pr",
    "--input-format",
    "--json-schema",
    "--max-budget-usd",
    "--mcp-config",
    "--model",
    "-n",
    "--name",
    "--output-format",
    "--permission-mode",
    "--permission-prompts",
    "--plugin-dir",
    "--plugin-url",
    "--prompt-suggestions",
    "--remote-control",
    "--remote-control-session-name-prefix",
    "-r",
    "--resume",
    "--session-id",
    "--setting-sources",
    "--settings",
    "--system-prompt",
    "--system-prompt-file",
    "--system-prompt-snapshot",
    "--teleport",
    "--tools",
    "-w",
    "--worktree",
];

/// The first positional of `args`: the first token that is neither
/// `-`-prefixed nor the value of a preceding [`CLAUDE_VALUE_OPTIONS`] option,
/// scanning only ahead of a `--` (everything after it is a prompt).
pub fn first_positional(args: &[String]) -> Option<&str> {
    let mut skip_value = false;
    for a in args.iter().take_while(|a| *a != "--") {
        if a.starts_with('-') {
            skip_value = CLAUDE_VALUE_OPTIONS.contains(&a.as_str());
        } else if skip_value {
            skip_value = false;
        } else {
            return Some(a);
        }
    }
    None
}

/// Is this `claude` argv a non-session subcommand, which the identity shim
/// must exec with its argv untouched?
pub fn is_non_session_subcommand(args: &[String]) -> bool {
    first_positional(args).is_some_and(|t| CLAUDE_NON_SESSION_SUBCOMMANDS.contains(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn subcommands_pass_through() {
        for args in [
            vec!["plugin", "eval", "suite.json"],
            vec!["plugins", "list"],
            vec!["mcp", "list"],
            vec!["--verbose", "doctor"],
            vec!["-p", "update"],
            vec!["setup-token"],
            vec!["help"],
            // After a value option's value, or a value option whose optional
            // value is absent (the next token is a flag).
            vec!["--model", "opus", "mcp", "list"],
            vec!["--add-dir=config", "plugin"],
            vec!["--debug", "--verbose", "doctor"],
        ] {
            assert!(is_non_session_subcommand(&strs(&args)), "{args:?}");
        }
    }

    #[test]
    fn sessions_are_not_subcommands() {
        for args in [
            vec![],
            vec!["--verbose"],
            vec!["-p", "fix the bug"],
            vec!["fix the plugin"],
            // A prompt word that only LOOKS like a subcommand.
            vec!["--", "plugin"],
            vec!["-p", "--", "mcp"],
            // Case-sensitive, as claude's parser is.
            vec!["Plugin"],
            vec!["--session-id", "abc"],
            // An unknown first word is a prompt.
            vec!["refactor", "mcp"],
            // An option VALUE that looks like a subcommand.
            vec!["--add-dir", "config"],
            vec!["--agent", "doctor", "-p", "hi"],
            vec!["-d", "mcp"],
            vec!["--model", "opus", "--add-dir", "logs"],
        ] {
            assert!(!is_non_session_subcommand(&strs(&args)), "{args:?}");
        }
    }

    #[test]
    fn first_positional_stops_at_the_terminator() {
        assert_eq!(first_positional(&strs(&["-c", "--", "x"])), None);
        assert_eq!(first_positional(&strs(&["-c", "y", "--", "x"])), Some("y"));
    }
}
