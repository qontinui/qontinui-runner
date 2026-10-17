//! The per-CLI profile manifest: one [`CliProfile`] per AI CLI the runner can
//! host, and the lookups every consumer goes through.
//!
//! Plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 4. Before this module the runner answered "what does this AI CLI do?"
//! by testing the literal program name at scattered sites, and the frontend
//! kept its own copy of Claude's resume markers. The profiles here are the one
//! source: Rust consumers call [`profile_for`] / [`profile_for_program`], and
//! the frontend reads the same data over the Tauri command
//! `terminal_cli_profiles` or `GET /terminals/cli-profiles`.
//!
//! **An unknown provider has no profile.** [`profile_for`] answers `None` for
//! it, never a default CLI's profile — a record whose provider the runner
//! does not know is restored terminal-only with its provider named, never by
//! typing another CLI's resume command at it.
//!
//! Lives in the LIB crate so the dependency-free shim bin and `qontinui-pr`
//! can read it too (`terminal` and `session` are bin-only).
//!
//! # Handshake regex dialect
//!
//! [`HandshakePatterns`](qontinui_types::cli_session::HandshakePatterns) regex
//! sources are compiled by two engines: the Rust `regex` crate here and
//! JavaScript `RegExp` in the frontend's resume verification. A source must
//! therefore stay in the syntax both accept:
//!
//! - **Always case-insensitive.** Both engines compile every source with their
//!   case-insensitive switch (`RegexBuilder::case_insensitive(true)`, the JS
//!   `"i"` flag), matching the substring lists, which are case-insensitive
//!   too. Inline flag groups such as `(?i)` are not allowed: JS rejects them.
//! - Only `(?:…)` groups — no look-around, no named groups, no backreferences.
//!
//! `every_handshake_regex_is_in_the_shared_dialect` enforces both rules, and
//! the shared screen fixtures under `src-tauri/tests/fixtures/cli_screens/`
//! prove the two engines classify the same screens identically (this module's
//! tests and `src/components/terminal/cliScreens.test.ts` read the same files).

pub mod claude;
pub mod codex;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use qontinui_types::cli_session::{
    AccountIsolation, AutoApprove, CliProfile, GracefulExit, IdentitySource, RestoreTier,
    ResumeSpec,
};

/// The placeholder a [`ResumeSpec::ByIdArgv`] template carries where the
/// session id goes.
pub const ID_PLACEHOLDER: &str = "{id}";

static PROFILES: LazyLock<Vec<CliProfile>> =
    LazyLock::new(|| vec![claude::PROFILE.clone(), codex::PROFILE.clone()]);

/// Every profile the runner knows, in a stable order. This is exactly what the
/// served routes return.
pub fn all() -> &'static [CliProfile] {
    &PROFILES
}

/// The profile whose [`CliProfile::id`] is `id`, or `None` for a provider the
/// runner does not know.
pub fn profile_for(id: &str) -> Option<&'static CliProfile> {
    all().iter().find(|p| p.id == id)
}

/// The profile whose [`CliProfile::programs`] contains `program`'s stem — the
/// path's last component, stripped of surrounding quotes and of a Windows
/// launcher suffix (`.exe` / `.cmd` / `.bat` / `.ps1`), compared
/// ASCII-case-insensitively (`claude`, `claude.exe`, `/usr/bin/claude`,
/// `C:\bin\Claude.cmd`, `"claude"`). `None` when no profile claims it.
pub fn profile_for_program(program: &str) -> Option<&'static CliProfile> {
    let stem = program_stem(program.trim_matches(|c| c == '"' || c == '\''));
    all()
        .iter()
        .find(|p| p.programs.iter().any(|s| s.eq_ignore_ascii_case(stem)))
}

/// Every program stem any profile claims, in profile order — the process-image
/// names an AI-CLI census matches.
pub fn all_programs() -> impl Iterator<Item = &'static str> {
    all()
        .iter()
        .flat_map(|p| p.programs.iter().map(String::as_str))
}

/// `program` without its directory and without a trailing launcher suffix.
fn program_stem(program: &str) -> &str {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    for suffix in [".exe", ".cmd", ".bat", ".ps1"] {
        let Some(cut) = base.len().checked_sub(suffix.len()) else {
            continue;
        };
        // `get` rather than slicing: `cut` may fall inside a multi-byte char.
        if let (Some(head), Some(tail)) = (base.get(..cut), base.get(cut..)) {
            if !head.is_empty() && tail.eq_ignore_ascii_case(suffix) {
                return head;
            }
        }
    }
    base
}

/// The flag that carries the id in `profile`'s by-id resume template — the
/// token right before [`ID_PLACEHOLDER`] (`--resume` for
/// `["claude", "--resume", "{id}"]`). `None` when the profile declares no by-id
/// resume, or when its template takes the id positionally (`codex resume
/// {id}`, where the token before it is a subcommand, not a flag).
pub fn resume_flag(profile: &CliProfile) -> Option<&str> {
    let ResumeSpec::ByIdArgv { template } = &profile.resume else {
        return None;
    };
    let at = template.iter().position(|arg| arg == ID_PLACEHOLDER)?;
    let flag = template.get(at.checked_sub(1)?)?;
    flag.starts_with('-').then_some(flag.as_str())
}

/// The flag with which the runner pins `profile`'s session id at launch
/// (`--session-id`), when its identity is [`IdentitySource::Pinned`].
pub fn pin_flag(profile: &CliProfile) -> Option<&str> {
    match &profile.identity {
        IdentitySource::Pinned { flag } => Some(flag.as_str()),
        IdentitySource::ReadBack { .. } | IdentitySource::Unknown => None,
    }
}

/// Every flag that names a session id on one of `profile`'s launches, each with
/// whether it RESUMES that id (`true`) or pins a fresh one (`false`): the pin
/// flag, the resume flag, then the resume flag's aliases. A launch carrying
/// both a resume and a pin runs under the resumed id.
pub fn id_flags(profile: &CliProfile) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    if let Some(flag) = pin_flag(profile) {
        out.push((flag, false));
    }
    if let Some(flag) = resume_flag(profile) {
        out.push((flag, true));
    }
    out.extend(
        profile
            .resume_flag_aliases
            .iter()
            .map(|alias| (alias.as_str(), true)),
    );
    out
}

/// Whether `command` (a whole command line, or an argv joined with spaces)
/// already runs `profile` without approval prompts: it contains one of the
/// profile's [`AutoApprove::Flags`] `detect` spellings. `false` for a profile
/// that declares no auto-approval (or none known).
pub fn implies_auto_approve(profile: &CliProfile, command: &str) -> bool {
    match &profile.auto_approve {
        AutoApprove::Flags { detect, .. } => detect
            .iter()
            .any(|token| !token.is_empty() && command.contains(token.as_str())),
        AutoApprove::None | AutoApprove::Unknown => false,
    }
}

/// [`implies_auto_approve`] against every known profile. For a command whose
/// CLI is not known in advance (a spawn argv, possibly wrapped in a shell
/// string), where any profile's auto-approve spelling means the same thing.
pub fn command_implies_auto_approve(argv: &[String]) -> bool {
    let joined = argv.join(" ");
    all().iter().any(|p| implies_auto_approve(p, &joined))
}

/// The text the runner types to exit a live session of `profile` cleanly, or
/// why it must type nothing. Only [`GracefulExit::TypedCommand`] yields text: a
/// [`GracefulExit::Signal`] exit is not something a typed-input exit can
/// deliver, and [`GracefulExit::Unknown`] means nobody has verified what exits
/// this CLI — typing a guess into a live session is never safe.
pub fn graceful_exit_text(profile: &CliProfile) -> Result<&str, String> {
    match &profile.graceful_exit {
        GracefulExit::TypedCommand { text } if !text.is_empty() => Ok(text.as_str()),
        GracefulExit::TypedCommand { .. } => Err(format!(
            "the {} profile declares an empty exit command",
            profile.display_name
        )),
        GracefulExit::Signal => Err(format!(
            "the {} profile exits by signal, which a typed graceful exit cannot send",
            profile.display_name
        )),
        GracefulExit::Unknown => Err(format!(
            "the {} profile has no verified graceful exit (graceful_exit: unknown)",
            profile.display_name
        )),
    }
}

/// The environment variable that selects `profile`'s account, when its
/// isolation is [`AccountIsolation::EnvVar`] (`CLAUDE_CONFIG_DIR`).
pub fn account_env_var(profile: &CliProfile) -> Option<&str> {
    match &profile.account_isolation {
        AccountIsolation::EnvVar { name } => Some(name.as_str()),
        AccountIsolation::HomeDir | AccountIsolation::None | AccountIsolation::Unknown => None,
    }
}

/// Whether `args` show the user choosing `profile`'s session themselves — any
/// id flag ([`id_flags`]) or [`CliProfile::session_choice_args`] token, as a
/// whole argv entry compared case-insensitively; a `--long` token also matches
/// its `--long=…` spelling. A launch that does is never pinned to an id of the
/// runner's own.
pub fn user_chose_session(profile: &CliProfile, args: &[String]) -> bool {
    let tokens: Vec<&str> = id_flags(profile)
        .into_iter()
        .map(|(flag, _)| flag)
        .chain(profile.session_choice_args.iter().map(String::as_str))
        .collect();
    args_name_any(args, &tokens)
}

/// Whether `args` take up an EXISTING session of `profile` — a resuming id
/// flag ([`id_flags`] with `true`) or a [`CliProfile::session_choice_args`]
/// token (`--continue`, `resume`) — as opposed to starting a fresh one. A
/// launch that does and then exits unsuccessfully failed to get that
/// conversation back.
pub fn resumes_existing_session(profile: &CliProfile, args: &[String]) -> bool {
    let tokens: Vec<&str> = id_flags(profile)
        .into_iter()
        .filter(|(_, resumes)| *resumes)
        .map(|(flag, _)| flag)
        .chain(profile.session_choice_args.iter().map(String::as_str))
        .collect();
    args_name_any(args, &tokens)
}

/// Whether any of `args` is one of `tokens` — a whole argv entry compared
/// case-insensitively, a `--long` token also matching its `--long=…` form.
fn args_name_any(args: &[String], tokens: &[&str]) -> bool {
    args.iter().any(|arg| {
        let name = match arg.split_once('=') {
            Some((name, _)) if name.starts_with("--") => name,
            _ => arg.as_str(),
        };
        tokens.iter().any(|t| t.eq_ignore_ascii_case(name))
    })
}

/// Longest session id [`is_valid_session_id`] accepts.
pub const MAX_SESSION_ID_LEN: usize = 128;

/// Whether `id` is safe to interpolate into a command line: 1 to
/// [`MAX_SESSION_ID_LEN`] ASCII letters, digits, `-` and `_`, not starting
/// with `-` (which a CLI would read as a flag). Every id a supported CLI mints
/// (Claude's UUIDv4, Codex's UUIDv7) passes; anything carrying a shell
/// metacharacter, whitespace or a quote does not. A resume line is typed into
/// a shell (the PTY restore paths) or joined into a launch command string
/// (the restore record), so an id from a record, a transcript or a request is
/// never trusted to be inert.
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The argv that resumes `session_id` under `profile`. `None` when the
/// profile declares no by-id resume, or when `session_id` fails
/// [`is_valid_session_id`] — an id that could not be typed safely is never
/// spliced into a command.
pub fn resume_argv(profile: &CliProfile, session_id: &str) -> Option<Vec<String>> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    match &profile.resume {
        ResumeSpec::ByIdArgv { template } => Some(
            template
                .iter()
                .map(|arg| arg.replace(ID_PLACEHOLDER, session_id))
                .collect(),
        ),
        ResumeSpec::None | ResumeSpec::Unknown => None,
    }
}

/// What a restore of `profile`'s sessions can honestly bring back:
/// [`RestoreTier::Full`] only when the profile declares it AND declares a
/// by-id resume to deliver it with. A profile claiming `Full` with no resume
/// argv cannot keep that promise, so it reads `TerminalOnly`.
pub fn restore_tier(profile: &CliProfile) -> RestoreTier {
    match (&profile.restore_tier, &profile.resume) {
        (RestoreTier::Full, ResumeSpec::ByIdArgv { .. }) => RestoreTier::Full,
        _ => RestoreTier::TerminalOnly,
    }
}

/// The environment that pins a session of `profile` to the account whose
/// resolved config dir is `account`. Empty when `account` is `None` (the
/// spawn path's process-global resolution applies) or when the profile's
/// isolation is not an environment variable.
pub fn account_isolation_env(
    profile: &CliProfile,
    account: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    if let (AccountIsolation::EnvVar { name }, Some(dir)) = (&profile.account_isolation, account) {
        env.insert(name.clone(), dir.to_string());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::{Regex, RegexBuilder};
    use serde::Deserialize;
    use std::path::PathBuf;

    #[test]
    fn claude_resolves_by_id_and_unknown_providers_resolve_to_none() {
        let p = profile_for("claude").expect("claude profile");
        assert_eq!(p.id, claude::ID);
        assert_eq!(profile_for("codex").map(|p| p.id.as_str()), Some(codex::ID));
        assert!(profile_for("gemini").is_none());
        assert!(profile_for("totally-new").is_none());
        assert!(profile_for("").is_none());
        // Ids are exact: a program spelling is not an id.
        assert!(profile_for("Claude").is_none());
    }

    #[test]
    fn program_lookup_matches_stems_across_paths_and_suffixes() {
        for program in [
            "claude",
            "claude.exe",
            "Claude.EXE",
            "claude.cmd",
            "/home/u/.nvm/versions/node/v24/bin/claude",
            r"C:\Users\u\AppData\Roaming\npm\claude.cmd",
        ] {
            assert_eq!(
                profile_for_program(program).map(|p| p.id.as_str()),
                Some("claude"),
                "{program}"
            );
        }
        for program in [
            "codex",
            "codex.exe",
            "/usr/local/bin/codex",
            r"C:\Users\u\AppData\Roaming\npm\codex.cmd",
        ] {
            assert_eq!(
                profile_for_program(program).map(|p| p.id.as_str()),
                Some("codex"),
                "{program}"
            );
        }
        for program in ["gemini", "claude-code", "bash", "", ".exe", "/usr/bin/"] {
            assert!(profile_for_program(program).is_none(), "{program}");
        }
    }

    #[test]
    fn program_lookup_strips_quotes_and_every_windows_launcher_suffix() {
        for program in ["\"claude\"", "'claude'", "claude.bat", "Claude.PS1"] {
            assert_eq!(
                profile_for_program(program).map(|p| p.id.as_str()),
                Some("claude"),
                "{program}"
            );
        }
        assert!(all_programs().any(|p| p == "claude"));
    }

    #[test]
    fn id_flags_come_from_the_identity_resume_and_alias_facts() {
        let p = profile_for("claude").unwrap();
        assert_eq!(pin_flag(p), Some("--session-id"));
        assert_eq!(resume_flag(p), Some("--resume"));
        assert_eq!(
            id_flags(p),
            vec![("--session-id", false), ("--resume", true), ("-r", true)]
        );
        // A positional id (`codex resume {id}`) has no resume FLAG.
        let mut positional = p.clone();
        positional.resume = ResumeSpec::ByIdArgv {
            template: vec!["codex".into(), "resume".into(), "{id}".into()],
        };
        positional.identity = IdentitySource::Unknown;
        positional.resume_flag_aliases.clear();
        assert_eq!(resume_flag(&positional), None);
        assert!(id_flags(&positional).is_empty());
    }

    /// The Codex profile through every lookup: read-back identity (no pin, no
    /// resume flag), a positional resume, terminal-only restore while resume
    /// continuity is unknown, and no typed exit to send.
    #[test]
    fn codex_profile_lookups_follow_its_read_back_facts() {
        let p = profile_for(codex::ID).unwrap();
        assert_eq!(pin_flag(p), None);
        assert_eq!(resume_flag(p), None);
        assert!(id_flags(p).is_empty());
        assert!(matches!(
            &p.identity,
            IdentitySource::ReadBack { capture } if capture == codex::CAPTURE_SESSION_FILE
        ));
        // Ids are UUIDv7 — the template takes whatever id it is handed.
        let v7 = "01a0ef49-1234-7abc-8def-0123456789ab";
        assert_eq!(resume_argv(p, v7).unwrap(), vec!["codex", "resume", v7]);
        assert_eq!(restore_tier(p), RestoreTier::TerminalOnly);
        assert_eq!(account_env_var(p), Some("CODEX_HOME"));
        assert!(graceful_exit_text(p).is_err());
        assert!(implies_auto_approve(
            p,
            "codex --dangerously-bypass-approvals-and-sandbox"
        ));
        assert!(implies_auto_approve(
            p,
            "codex resume x --ask-for-approval never"
        ));
        assert!(!implies_auto_approve(
            p,
            "codex --ask-for-approval on-request"
        ));
        let args = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(user_chose_session(p, &args(&["resume", "--last"])));
        assert!(!user_chose_session(p, &args(&["--model", "o3"])));
        // No handshake markers are claimed for an unauthenticated probe.
        let hp = &p.handshake;
        assert!(hp.success.is_empty() && hp.failure.is_empty());
        assert!(hp.success_regex.is_empty() && hp.failure_regex.is_empty());
    }

    #[test]
    fn auto_approve_detection_reads_the_profile_spellings() {
        let p = profile_for("claude").unwrap();
        assert!(implies_auto_approve(
            p,
            "claude --dangerously-skip-permissions"
        ));
        assert!(implies_auto_approve(
            p,
            "claude --permission-mode bypassPermissions --resume x"
        ));
        assert!(!implies_auto_approve(p, "claude --permission-mode default"));
        let argv = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(command_implies_auto_approve(&argv(&[
            "claude",
            "--permission-mode",
            "bypassPermissions",
        ])));
        assert!(!command_implies_auto_approve(&argv(&["bash", "-l"])));
        let mut unknown = p.clone();
        unknown.auto_approve = AutoApprove::Unknown;
        assert!(!implies_auto_approve(
            &unknown,
            "claude --dangerously-skip-permissions"
        ));
    }

    #[test]
    fn graceful_exit_text_types_only_a_declared_command() {
        let p = profile_for("claude").unwrap();
        assert_eq!(graceful_exit_text(p), Ok("/exit"));
        let mut other = p.clone();
        for exit in [GracefulExit::Unknown, GracefulExit::Signal] {
            other.graceful_exit = exit;
            assert!(graceful_exit_text(&other).is_err());
        }
        other.graceful_exit = GracefulExit::TypedCommand {
            text: String::new(),
        };
        assert!(graceful_exit_text(&other).is_err());
    }

    #[test]
    fn user_chose_session_matches_whole_tokens_only() {
        let p = profile_for("claude").unwrap();
        let args = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        for tok in [
            "--session-id",
            "--SESSION-ID",
            "--resume",
            "-r",
            "-R",
            "--continue",
            "-c",
            "resume",
            "--resume=abc",
            "--session-id=abc",
        ] {
            assert!(user_chose_session(p, &args(&["-p", "hi", tok])), "{tok}");
        }
        for tok in [
            "--session-id-ish",
            "--continued",
            "-cc",
            "resume the work",
            "-c=1",
        ] {
            assert!(!user_chose_session(p, &args(&[tok])), "{tok}");
        }
        assert_eq!(account_env_var(p), Some("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn resume_and_tier_are_lookups_on_the_profile() {
        let p = profile_for("claude").unwrap();
        assert_eq!(
            resume_argv(p, "sess-1").unwrap(),
            vec!["claude", "--resume", "sess-1"]
        );
        assert_eq!(restore_tier(p), RestoreTier::Full);

        // A Full claim with no resume argv cannot be honoured.
        let mut no_resume = p.clone();
        no_resume.resume = ResumeSpec::Unknown;
        assert_eq!(resume_argv(&no_resume, "sess-1"), None);
        assert_eq!(restore_tier(&no_resume), RestoreTier::TerminalOnly);
    }

    /// A resume (or continue) takes up an existing session; a pin starts one.
    #[test]
    fn resumes_existing_session_tells_a_resume_from_a_pin() {
        let p = profile_for("claude").unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for resuming in [
            &["--resume", "abc"][..],
            &["--resume=abc"],
            &["-r", "abc"],
            &["--continue"],
        ] {
            assert!(resumes_existing_session(p, &args(resuming)), "{resuming:?}");
        }
        for fresh in [&["--session-id", "abc"][..], &["-p", "hi"], &[]] {
            assert!(!resumes_existing_session(p, &args(fresh)), "{fresh:?}");
        }
    }

    /// A session id is spliced into a resume command only when it is in the
    /// strict id charset: no shell metacharacter, quote, space or leading
    /// dash reaches a command line.
    #[test]
    fn resume_argv_refuses_an_id_outside_the_strict_charset() {
        let p = profile_for("claude").unwrap();
        for ok in [
            "0b7e2a3c-1d4f-4a5b-9c8d-7e6f5a4b3c2d",
            "01a0ef49-1234-7abc-8def-0123456789ab",
            "sess_1",
        ] {
            assert!(is_valid_session_id(ok), "{ok}");
            assert!(resume_argv(p, ok).is_some(), "{ok}");
        }
        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        for bad in [
            "",
            "-rf",
            "a b",
            "x;rm -rf ~",
            "$(id)",
            "`id`",
            "a'b",
            "a\"b",
            "a&b",
            "a|b",
            "a\nb",
            too_long.as_str(),
        ] {
            assert!(!is_valid_session_id(bad), "{bad:?}");
            assert_eq!(resume_argv(p, bad), None, "{bad:?}");
        }
    }

    #[test]
    fn account_isolation_names_the_env_var_only_for_a_resolved_account() {
        let p = profile_for("claude").unwrap();
        assert!(account_isolation_env(p, None).is_empty());
        assert_eq!(
            account_isolation_env(p, Some("/cfg"))
                .get("CLAUDE_CONFIG_DIR")
                .map(String::as_str),
            Some("/cfg")
        );
        let mut unknown = p.clone();
        unknown.account_isolation = AccountIsolation::Unknown;
        assert!(account_isolation_env(&unknown, Some("/cfg")).is_empty());
    }

    /// The dialect contract in the module docs, mechanically: every handshake
    /// regex compiles case-insensitively here and carries no construct
    /// JavaScript rejects or reads differently.
    #[test]
    fn every_handshake_regex_is_in_the_shared_dialect() {
        for p in all() {
            for src in p
                .handshake
                .success_regex
                .iter()
                .chain(&p.handshake.failure_regex)
                .chain(&p.handshake.title_regex)
            {
                compile(src);
                assert!(
                    src.split("(?").skip(1).all(|after| after.starts_with(':')),
                    "{}: {src:?} uses a `(?` group other than `(?:` — inline flags, \
                     look-around and named groups are outside the shared dialect",
                    p.id
                );
                assert!(
                    !(1..=9).any(|n| src.contains(&format!("\\{n}"))),
                    "{}: {src:?} uses a backreference",
                    p.id
                );
            }
        }
    }

    /// Claude's window-title marker is anchored to Claude's own launch title:
    /// a shell title naming a `Claude Code` path must not verify a resume.
    #[test]
    fn claude_title_marker_matches_only_claudes_own_title() {
        let p = profile_for("claude").unwrap();
        let hit = |title: &str| {
            p.handshake
                .title_regex
                .iter()
                .any(|r| compile(r).is_match(title))
        };
        assert!(hit("\u{2733} Claude Code"));
        assert!(hit("\u{00b7} Claude Code"));
        assert!(!hit("~/Claude Code"));
        assert!(!hit("user@host: ~/Projects/Claude Code demo"));
        assert!(!hit("\u{2733} session title"));
    }

    fn compile(src: &str) -> Regex {
        RegexBuilder::new(src)
            .case_insensitive(true)
            .build()
            .unwrap_or_else(|e| panic!("{src:?} does not compile: {e}"))
    }

    // -- the shared screen fixtures (the cross-language drift guard) ---------

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli_screens")
    }

    /// The served profiles, as the frontend's tests read them. Rewrite after an
    /// intentional profile change with
    /// `UPDATE_CLI_PROFILE_SNAPSHOT=1 cargo test --lib -- cli_profile`.
    #[test]
    fn served_profiles_snapshot_matches_the_live_manifest() {
        let path = fixtures_dir().join("served_profiles.json");
        let live = serde_json::to_value(all()).unwrap();
        if std::env::var_os("UPDATE_CLI_PROFILE_SNAPSHOT").is_some() {
            let mut text = serde_json::to_string_pretty(&live).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let snapshot: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .expect("served_profiles.json parses");
        assert_eq!(
            snapshot, live,
            "served_profiles.json is stale — the frontend's tests would classify against an \
             old manifest. Regenerate with UPDATE_CLI_PROFILE_SNAPSHOT=1."
        );
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ExpectedScreen {
        provider: String,
        file: String,
        resume: String,
        usage_limit: bool,
    }

    #[derive(Deserialize)]
    struct Expected {
        screens: Vec<ExpectedScreen>,
    }

    /// Mirrors `detectResumeFailure` / `detectClaudeHandshake` in
    /// `resumeVerification.ts`: failure first (failure screens are CLI UI
    /// too), each side the union of case-insensitive substrings and regexes.
    fn classify_resume(p: &CliProfile, screen: &str) -> &'static str {
        let hp = &p.handshake;
        let hit = |subs: &[String], regexes: &[String]| {
            let lower = screen.to_lowercase();
            subs.iter()
                .any(|s| !s.is_empty() && lower.contains(&s.to_lowercase()))
                || regexes.iter().any(|r| compile(r).is_match(screen))
        };
        if hit(&hp.failure, &hp.failure_regex) {
            "failed"
        } else if hit(&hp.success, &hp.success_regex) {
            "verified"
        } else {
            "none"
        }
    }

    /// Mirrors `terminal/usage_limit.rs`: lowercase, whitespace runs collapsed
    /// to one space, then a substring match of any phrase.
    fn classify_usage_limit(p: &CliProfile, screen: &str) -> bool {
        let normalized = screen
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        p.usage_limit_phrases
            .iter()
            .any(|phrase| normalized.contains(phrase.as_str()))
    }

    #[test]
    fn shared_screen_fixtures_classify_as_expected_through_the_manifest() {
        let dir = fixtures_dir();
        let expected: Expected =
            serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap())
                .expect("expected.json parses");
        assert!(!expected.screens.is_empty());
        for s in &expected.screens {
            let p = profile_for(&s.provider)
                .unwrap_or_else(|| panic!("{}: no profile {:?}", s.file, s.provider));
            let screen = std::fs::read_to_string(dir.join(&s.file))
                .unwrap_or_else(|e| panic!("{}: {e}", s.file));
            assert_eq!(classify_resume(p, &screen), s.resume, "{} resume", s.file);
            assert_eq!(
                classify_usage_limit(p, &screen),
                s.usage_limit,
                "{} usageLimit",
                s.file
            );
        }
    }
}
