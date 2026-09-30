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

use std::collections::BTreeMap;
use std::sync::LazyLock;

use qontinui_types::cli_session::{AccountIsolation, CliProfile, RestoreTier, ResumeSpec};

/// The placeholder a [`ResumeSpec::ByIdArgv`] template carries where the
/// session id goes.
pub const ID_PLACEHOLDER: &str = "{id}";

static PROFILES: LazyLock<Vec<CliProfile>> = LazyLock::new(|| vec![claude::profile()]);

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
/// path's last component with a Windows `.exe` / `.cmd` suffix removed, compared
/// ASCII-case-insensitively (`claude`, `claude.exe`, `/usr/bin/claude`,
/// `C:\bin\Claude.cmd`). `None` when no profile claims it.
pub fn profile_for_program(program: &str) -> Option<&'static CliProfile> {
    let stem = program_stem(program);
    all()
        .iter()
        .find(|p| p.programs.iter().any(|s| s.eq_ignore_ascii_case(stem)))
}

/// `program` without its directory and without a trailing `.exe` / `.cmd`.
fn program_stem(program: &str) -> &str {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    for suffix in [".exe", ".cmd"] {
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

/// The argv that resumes `session_id` under `profile`, or `None` when the
/// profile declares no by-id resume.
pub fn resume_argv(profile: &CliProfile, session_id: &str) -> Option<Vec<String>> {
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
            "gemini",
            "claude-code",
            "bash",
            "",
            ".exe",
            "/usr/bin/",
        ] {
            assert!(profile_for_program(program).is_none(), "{program}");
        }
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
