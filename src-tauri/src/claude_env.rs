//! Claude Code process-topology env markers, and the strip rule for them.
//!
//! Claude Code sets a small number of env vars that describe a process's place
//! in a session tree. They are inherited by the entire process tree and nothing
//! clears them, so a long-lived daemon that happens to have been launched from
//! a Claude Code session passes them down forever — and every `claude` it
//! eventually spawns claims to be a nested session of something that exited
//! months ago.
//!
//! The rule, per `qontinui-runner/CLAUDE.md` "Claude CLI Spawning": **every
//! spawn site strips every marker in [`INHERITED_SESSION_MARKERS`]**. This
//! module exists so the spawn-site strips, the startup warning
//! ([`inherited_session_markers`]) and the `coord doctor` check share one
//! spelling of each name — a typo in any one of them is silent, removing
//! nothing while appearing to.
//!
//! ## The markers
//!
//! - [`CLAUDECODE_ENV`] — "you are running inside Claude Code". Long-standing;
//!   stripped at eleven spawn sites before this module existed.
//! - [`CLAUDE_CHILD_SESSION_ENV`] — "you are a child/nested session". Added by
//!   plan `2026-07-28-runner-transcript-persistence-env-leak`, which found it
//!   leaking into every fleet session.
//!
//! ## What the child-session marker does NOT do
//!
//! It was believed to disable transcript persistence. Vetting on 2026-08-03
//! **refuted** that by direct observation: a session with the marker set was
//! writing its JSONL transcript incrementally. So this strip is env hygiene —
//! removing a lie about process topology that the CLI is entitled to act on
//! however it likes — and NOT a fix for lost transcripts. Do not re-justify it
//! as data recovery; see §0 of that plan.

/// "You are running inside Claude Code."
pub const CLAUDECODE_ENV: &str = "CLAUDECODE";

/// "You are a child/nested Claude Code session."
pub const CLAUDE_CHILD_SESSION_ENV: &str = "CLAUDE_CODE_CHILD_SESSION";

/// Every marker a spawn site must strip, and the startup check must report.
///
/// Adding a marker here does NOT automatically strip it at spawn sites — those
/// call `env_remove` directly on their own `Command`/`CommandBuilder` types,
/// which share no trait. This slice is the canonical list the startup warning
/// and `coord doctor` iterate; keep new spawn-site strips in step with it.
pub const INHERITED_SESSION_MARKERS: &[&str] = &[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV];

/// Which of [`INHERITED_SESSION_MARKERS`] this process inherited, in order.
///
/// A marker counts as inherited whenever it is **present**, including when set
/// to the empty string — `env_remove` is what clears one, not assigning `""`.
pub fn inherited_session_markers() -> Vec<&'static str> {
    INHERITED_SESSION_MARKERS
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some())
        .collect()
}

/// One-line, operator-readable summary of an inherited-marker set.
///
/// Kept next to the detection so the runner startup warning and the
/// `coord doctor` check render the same sentence.
pub fn inherited_markers_detail(markers: &[&str]) -> String {
    format!(
        "inherited Claude Code session marker(s): {} — this process is mislabelled as a nested \
         session. They are stripped from every terminal/CLI spawn, so panes are unaffected; the \
         markers reach this process from whatever launched it (usually the supervisor, which \
         inherits them from a Claude Code session).",
        markers.join(", ")
    )
}

/// Did a `claude` argv (everything after the program) already choose a
/// session? Then nothing may add a second one. Exact token match,
/// case-insensitive: `--session-id`, `--resume`, `-r`, the `resume`
/// subcommand, `--continue`, `-c`, plus the `--session-id=…` / `--resume=…`
/// inline forms. A merely-prefixed token (`--session-id-ish`) does NOT match,
/// and neither does anything after a `--`, which is positional. Nor does the
/// VALUE of a Claude Code option that takes one ([`CLAUDE_VALUE_FLAGS`],
/// [`CLAUDE_VARIADIC_FLAGS`]): in `--name resume` or `--model -c` the token
/// names a session title or a model, not a session choice, and reading it as
/// one would leave the pane unpinned.
///
/// The Rust spelling of that set, shared by every Rust reader: the native
/// identity shim (`bin/qontinui_shim.rs`) decides from it whether to append
/// `--session-id $QONTINUI_PINNED_SESSION_ID`, the runner's terminal seam
/// decides from it whether a pane started without that shim lost its pin, and
/// the launch-template composer (`claude_session::launch_spec`) drops a
/// template unit it matches when the spec pins the session (plan
/// `2026-11-21-a-live-claude-session-loses-its-sessions-pid-json-record`,
/// Phase 2). It is NOT yet the only spelling: the `.bash` and `.cmd` script
/// shims (`resources/intercept/identity_shim.bash` / `.cmd`) carry their own
/// copies of the list — a recorded follow-up, plan-library edge
/// `2ce2a5a6-da43-4585-a372-d533fb50b7e7`.
pub fn claude_argv_chooses_session<S: AsRef<str>>(args: &[S]) -> bool {
    let mut tokens = args.iter().map(AsRef::as_ref).peekable();
    while let Some(a) = tokens.next() {
        // `--` ends the options, so nothing after it is a session choice. A
        // `--` that is an option's VALUE (`--name -- -c`) never reaches here:
        // it is consumed below, as commander consumes it, and the scan goes on.
        if a == "--" {
            return false;
        }
        let t = a.to_ascii_lowercase();
        if matches!(
            t.as_str(),
            "--session-id" | "--resume" | "-r" | "resume" | "--continue" | "-c"
        ) || t.starts_with("--session-id=")
            || t.starts_with("--resume=")
        {
            return true;
        }
        // An option's value is never a session choice. Inline `--flag=value`
        // carries its value in the same token, so only the bare spelling
        // consumes what follows.
        if CLAUDE_VALUE_FLAGS.contains(&a) {
            tokens.next();
        } else if CLAUDE_VARIADIC_FLAGS.contains(&a) {
            // Variadic options take every following token up to the next
            // option, i.e. one longer than `-` that starts with it
            // (commander's `maybeOption`), so a `resume` or a lone `-` in that
            // run is a value. A `--` is an option by that test, so it ends the
            // run and is then read as the terminator.
            while tokens
                .next_if(|v| !(v.len() > 1 && v.starts_with('-')))
                .is_some()
            {}
        }
    }
    false
}

/// Claude Code options that take exactly ONE value, which is consumed even
/// when it starts with `-`, `--` included (commander's rule for a required
/// option-argument). The script identity shims (`resources/intercept/
/// identity_shim.bash` / `.cmd`) carry the same two lists; a test pins them.
/// Case-sensitive, as the CLI is. A flag missing from this list only costs
/// the old behaviour for its value (it is scanned as a token), so the list
/// errs toward the options a runner or an operator template actually sends.
pub const CLAUDE_VALUE_FLAGS: &[&str] = &[
    "--name",
    "-n",
    "--model",
    "--fallback-model",
    "--permission-mode",
    "--permission-prompt-tool",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--system-prompt",
    "--system-prompt-file",
    "--output-format",
    "--input-format",
    "--settings",
    "--setting-sources",
    "--agent",
    "--agents",
    "--effort",
    "--max-turns",
    "--max-budget-usd",
    "--json-schema",
    "--plugin-dir",
];

/// Claude Code options that take one or more values: everything up to the
/// next `-`-prefixed token.
pub const CLAUDE_VARIADIC_FLAGS: &[&str] = &[
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--mcp-config",
    "--tools",
    "--betas",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_names_are_the_cli_spellings() {
        // A typo here is silent: a spawn would keep leaking the real marker
        // while `env_remove` deleted a variable nobody sets.
        assert_eq!(CLAUDECODE_ENV, "CLAUDECODE");
        assert_eq!(CLAUDE_CHILD_SESSION_ENV, "CLAUDE_CODE_CHILD_SESSION");
    }

    #[test]
    fn every_marker_is_listed_for_detection() {
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDECODE_ENV));
        assert!(INHERITED_SESSION_MARKERS.contains(&CLAUDE_CHILD_SESSION_ENV));
        assert_eq!(INHERITED_SESSION_MARKERS.len(), 2);
    }

    #[test]
    fn detection_reports_present_markers_including_empty_values() {
        // `std::env` is process-global — hold the shared lock and restore on
        // drop so a sibling test can neither race this nor inherit its writes.
        let _g = crate::test_env::env_lock();
        let _restore =
            crate::test_env::EnvVarRestore::capture(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);

        for name in INHERITED_SESSION_MARKERS {
            std::env::remove_var(name);
        }
        assert!(
            inherited_session_markers().is_empty(),
            "no markers set → nothing inherited"
        );

        std::env::set_var(CLAUDE_CHILD_SESSION_ENV, "1");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDE_CHILD_SESSION_ENV],
            "a set marker is detected"
        );

        // Empty-but-set is still inherited — this is the case a naive
        // `var() == Ok(non_empty)` check would wrongly clear.
        std::env::set_var(CLAUDECODE_ENV, "");
        assert_eq!(
            inherited_session_markers(),
            vec![CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV],
            "empty-but-set counts, and order follows INHERITED_SESSION_MARKERS"
        );
    }

    #[test]
    fn detail_names_every_marker_it_was_given() {
        let detail = inherited_markers_detail(&[CLAUDECODE_ENV, CLAUDE_CHILD_SESSION_ENV]);
        assert!(detail.contains(CLAUDECODE_ENV));
        assert!(detail.contains(CLAUDE_CHILD_SESSION_ENV));
    }

    #[test]
    fn session_choice_tokens_are_recognised() {
        for argv in [
            &["--session-id", "x"][..],
            &["--resume"],
            &["-r", "x"],
            &["resume"],
            &["--continue"],
            &["-c"],
            &["--Session-Id=x"],
            &["--resume=x"],
            &["--verbose", "resume"],
            &["--model", "opus", "-c"],
            &["--add-dir", "a", "b", "--continue"],
            &["--name", "--", "-c"],
        ] {
            assert!(
                claude_argv_chooses_session(argv),
                "{argv:?} chooses a session"
            );
        }
    }

    #[test]
    fn an_option_value_is_not_a_session_choice() {
        for argv in [
            &["--name", "resume"][..],
            &["-n", "-c"],
            &["--model", "-r"],
            &["--name", "resume", "--verbose"],
            &["--add-dir", "a", "resume"],
            &["--allowedTools", "Bash", "resume", "--", "-c"],
            &["--session-id-ish"],
            &["--", "resume"],
            &["--name=resume"],
            &["--add-dir", "a", "--", "-c"],
            &["--plugin-dir", "p", "--verbose"],
            &["--add-dir", "a", "-", "resume"],
        ] {
            assert!(
                !claude_argv_chooses_session(argv),
                "{argv:?} chooses no session"
            );
        }
    }

    /// One argv table every session-choice scan is held to: the Rust scan
    /// here, and the bash / .cmd identity shim loops below. `true` = the argv
    /// already chooses a session, so the shim must not pin.
    const SCAN_CASES: &[(&[&str], bool)] = &[
        (&["--session-id", "x"], true),
        (&["--resume"], true),
        (&["-r", "x"], true),
        (&["resume"], true),
        (&["--continue"], true),
        (&["-c"], true),
        (&["--verbose", "resume"], true),
        (&["--model", "opus", "-c"], true),
        (&["--add-dir", "a", "b", "--continue"], true),
        (&["--name", "--", "-c"], true),
        (&["--name", "", "-c"], true),
        (&["--name", "resume"], false),
        (&["-n", "-c"], false),
        (&["--model", "-r"], false),
        (&["--add-dir", "a", "resume"], false),
        (&["--add-dir", "", "resume"], false),
        (&["--allowedTools", "Bash", "resume", "--", "-c"], false),
        (&["--", "resume"], false),
        (&["--add-dir", "a", "--", "-c"], false),
        (&["--plugin-dir", "p", "--verbose"], false),
        (&["--add-dir", "a", "-", "resume"], false),
        (&["-p", "hi"], false),
        (&["--name", "Why?", "-c"], true),
        (&["--name", "x*", "-c"], true),
    ];

    #[test]
    fn the_rust_scan_matches_the_shared_table() {
        for (argv, want) in SCAN_CASES {
            assert_eq!(claude_argv_chooses_session(argv), *want, "{argv:?}");
        }
    }

    /// Run `script` (one shim's scan, extracted verbatim, plus a line printing
    /// its verdict) over every row of [`SCAN_CASES`] and compare what it prints
    /// with the table. A failure names the exit status and stderr, so a script
    /// that errored reads as an error, not as a bare 0-vs-1 mismatch.
    ///
    /// The `cmd /c` line holds more than two quotes on the empty-argument
    /// rows, so a temp dir whose path contains a space would break it (cmd
    /// strips the first and last quote); CI and default Windows temp paths
    /// have none.
    fn assert_script_scan_matches(program: &str, pre_args: &[&str], file_name: &str, script: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file_name);
        std::fs::write(&path, script).unwrap();
        for (argv, want) in SCAN_CASES {
            let out = std::process::Command::new(program)
                .args(pre_args)
                .arg(&path)
                .args(argv.iter())
                .output()
                .unwrap_or_else(|e| panic!("{program} did not start: {e}"));
            let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let want = if *want { "1" } else { "0" };
            assert_eq!(
                got,
                want,
                "{program} scan of {argv:?} (status {:?}, stderr {:?})",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_bash_shim_scan_matches_the_shared_table() {
        const BASH: &str = include_str!("../resources/intercept/identity_shim.bash");
        let start = BASH
            .find("user_chose_session=0\n")
            .expect("bash shim has the scan");
        let end = BASH
            .get(start..)
            .and_then(|rest| rest.find("\ndone\n"))
            .expect("bash scan loop closes")
            + start
            + "\ndone\n".len();
        let body = BASH.get(start..end).expect("scan slice");
        let script = format!("{body}echo \"$user_chose_session\"\n");
        assert_script_scan_matches("bash", &[], "scan.sh", &script);
    }

    #[cfg(windows)]
    #[test]
    fn the_cmd_shim_scan_matches_the_shared_table() {
        const CMD: &str = include_str!("../resources/intercept/identity_shim.cmd");
        let start = CMD
            .find("set \"USER_CHOSE=0\"")
            .expect("cmd shim has the scan");
        // Anchor on the LABEL line: the text `:scan_args_done` first appears
        // in the loop's own `goto :scan_args_done`, and a slice ending there
        // would drop the loop body (review round 3, A1).
        const LABEL: &str = "\n:scan_args_done";
        let end = CMD
            .get(start..)
            .and_then(|rest| rest.find(LABEL))
            .expect("cmd scan loop closes")
            + start
            + LABEL.len();
        let body = CMD.get(start..end).expect("scan slice");
        let script = format!(
            "@echo off\r\nsetlocal EnableDelayedExpansion\r\n{}\r\necho !USER_CHOSE!\r\n",
            body.replace("\r\n", "\n").replace('\n', "\r\n")
        );
        assert_script_scan_matches("cmd.exe", &["/d", "/c"], "scan.cmd", &script);
    }

    /// The script shims carry their own copies of the two option lists. Every
    /// option the Rust scan skips must be skipped by both scripts too, or a
    /// script-shim pane is left unpinned where the runner predicts a pin
    /// (review round 5, A1).
    #[test]
    fn script_shims_carry_the_same_option_lists() {
        const BASH: &str = include_str!("../resources/intercept/identity_shim.bash");
        const CMD: &str = include_str!("../resources/intercept/identity_shim.cmd");
        // The `case` pattern line that starts with `first|` (the whole pattern,
        // first flag included).
        let bash_list = |first: &str| -> Vec<String> {
            let marker = format!("{first}|");
            let line = BASH
                .lines()
                .find(|l| l.trim_start().starts_with(&marker))
                .unwrap_or_else(|| panic!("bash shim has no `{marker}` pattern line"));
            line.trim()
                .trim_end_matches(')')
                .split('|')
                .map(|f| f.trim().to_string())
                .collect()
        };
        let cmd_list = |var: &str| -> Vec<String> {
            let prefix = format!("set \"{var}= ");
            let line = CMD
                .lines()
                .find(|l| l.trim_start().starts_with(&prefix))
                .unwrap_or_else(|| panic!("cmd shim has no `{var}` list"));
            line.trim_start()
                .trim_start_matches(&prefix)
                .trim_end_matches(" \"")
                .split(' ')
                .map(str::to_string)
                .collect()
        };
        let mut want_value: Vec<String> =
            CLAUDE_VALUE_FLAGS.iter().map(|s| s.to_string()).collect();
        let mut want_variadic: Vec<String> = CLAUDE_VARIADIC_FLAGS
            .iter()
            .map(|s| s.to_string())
            .collect();
        want_value.sort();
        want_variadic.sort();
        for (got, want, which) in [
            (bash_list("--name"), &want_value, "bash value"),
            (bash_list("--add-dir"), &want_variadic, "bash variadic"),
            (cmd_list("VALUE_FLAGS"), &want_value, "cmd value"),
            (cmd_list("VARIADIC_FLAGS"), &want_variadic, "cmd variadic"),
        ] {
            let mut got = got;
            got.sort();
            assert_eq!(&got, want, "{which} list drifted from the Rust list");
        }
    }
}
