//! Account-home discovery and account labelling for the session archive.
//!
//! A "Claude Code account home" is a `CLAUDE_CONFIG_DIR` — a directory holding
//! a `projects/` subtree of `<encoded-cwd>/<session-id>.jsonl` transcripts.
//! This fleet runs several side by side (measured 2026-08-26: **8,308 `.jsonl`
//! across 7 homes** — six `C:/claude/.claude-*` roots plus
//! `%USERPROFILE%/.claude`).
//!
//! **The roster is discovered, never hard-coded.** A machine with a different
//! set of accounts is the normal case, not the exception, and a literal list
//! here would silently archive a subset of the corpus while reporting success.
//!
//! ## Why this lives in the lib crate
//!
//! `terminal::transcript::find_claude_config_dirs` and
//! `session::past_sessions::account_from_config_dir` already implemented these
//! two rules — but both live in the **binary** crate, and `qontinui-pr` is a
//! separate crate root that can only reach `qontinui_runner_lib`. Rather than
//! grow a second copy ("kept in sync by shape, not by import" is the failure
//! mode this tree argues against everywhere), the implementations moved HERE
//! and those two functions now delegate. One rule, two callers.
//!
//! The one thing that could not move is the settings roster: reading it needs
//! `crate::settings`, which is bin-side. So the roster is an **injected
//! argument** ([`discover_account_homes`]'s `configured`), exactly the shape
//! `plan_workunit_adapter::body_push::resolve_backend_base` uses for the same
//! reason. The runner passes `settings::get_claude_config_dirs()`; the CLI
//! passes [`roster_config_dirs`], which reads the same machine-global
//! `claude-accounts.json` the settings loader overlays.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One discovered account home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountHome {
    /// The config dir itself (the parent of `projects/`).
    pub config_dir: PathBuf,
    /// The account label this home's sessions are filed under — half of the
    /// session-repository identity key. See [`account_label_for`].
    pub label: String,
    /// The CLI wrapper that launches this account (`clg`, `clp`, …), or the
    /// generic `claude`. Recorded so a relaunch command can name the account.
    pub wrapper: String,
}

/// The `projects/` subdirectory name — a directory is only an account home if
/// it has one, which is what distinguishes a real config dir from any other
/// dot-directory under `C:/claude`.
const PROJECTS_SUBDIR: &str = "projects";

/// Map a config dir to its account label + CLI wrapper.
///
/// The mapping mirrors this fleet's account roster (`gmail→clg`, `hotmail→clh`,
/// `paktis→clp`, `qontinui→clq`, `tiohorst→clt`); an unrecognized
/// `.claude-<x>` keeps `<x>` as the label with the generic `claude` wrapper,
/// and a config dir with no `.claude-<x>` suffix at all (the plain
/// `%USERPROFILE%/.claude`) is `unknown`/`claude`.
///
/// **`unknown` is a real, stable label here, not a failure.** It is what the
/// runner's own session registry stores for those sessions
/// (`TerminalSessionRecord::account_label`), and the session-repository
/// identity key is `(claude_session_id, account_label)` — so labelling the
/// default home anything prettier would fork the identity of every session in
/// it away from the row the web archiver's metadata promotion converges on.
pub fn account_label_for(config_dir: Option<&str>) -> (String, String) {
    let acct = |label: &str, wrapper: &str| (label.to_string(), wrapper.to_string());
    let suffix = config_dir.and_then(|cd| {
        Path::new(cd)
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|name| name.strip_prefix(".claude-"))
            .map(|s| s.to_string())
    });
    match suffix.as_deref() {
        Some("gmail") => acct("gmail", "clg"),
        Some("hotmail") => acct("hotmail", "clh"),
        Some("paktis") => acct("paktis", "clp"),
        Some("qontinui") => acct("qontinui", "clq"),
        Some("tiohorst") => acct("tiohorst", "clt"),
        Some(other) => acct(other, "claude"),
        None => acct("unknown", "claude"),
    }
}

/// Discover every Claude Code account home on this machine, in precedence
/// order: `CLAUDE_CONFIG_DIR` → the injected `configured` roster → a scan of
/// `C:/claude/.claude-*` → `%USERPROFILE%/.claude` (or `$HOME/.claude`).
///
/// Every candidate must carry a `projects/` subdirectory to count, and the
/// result is de-duplicated by path in first-seen order.
///
/// `env_config_dir` is passed in rather than read here so the precedence is a
/// unit test rather than a process-env dance.
pub fn discover_account_homes(
    env_config_dir: Option<String>,
    configured: &[String],
    home_dir: Option<PathBuf>,
    claude_root: Option<PathBuf>,
) -> Vec<AccountHome> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dirs: &mut Vec<PathBuf>, p: PathBuf| {
        if p.join(PROJECTS_SUBDIR).exists() && !dirs.iter().any(|d| d == &p) {
            dirs.push(p);
        }
    };

    if let Some(env_dir) = env_config_dir.filter(|s| !s.trim().is_empty()) {
        push(&mut dirs, PathBuf::from(env_dir.trim()));
    }
    for dir in configured.iter().filter(|s| !s.trim().is_empty()) {
        push(&mut dirs, PathBuf::from(dir.trim()));
    }
    if let Some(root) = claude_root {
        if root.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&root) {
                // `read_dir` order is filesystem-defined, so sort for a
                // deterministic scan order — a backfill that reports per-home
                // counts should report them the same way twice.
                let mut found: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_dir()
                            && p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.starts_with(".claude-"))
                    })
                    .collect();
                found.sort();
                for p in found {
                    push(&mut dirs, p);
                }
            }
        }
    }
    if let Some(home) = home_dir {
        push(&mut dirs, home.join(".claude"));
    }

    dirs.into_iter()
        .map(|config_dir| {
            let (label, wrapper) = account_label_for(config_dir.to_str());
            AccountHome {
                config_dir,
                label,
                wrapper,
            }
        })
        .collect()
}

/// [`discover_account_homes`] reading the ambient machine itself.
///
/// `C:/claude` is probed on every platform rather than gated behind
/// `cfg(windows)`: the check is `is_dir()`, which is false everywhere else, and
/// a `cfg` here would make the scan untestable on the machine that runs CI.
pub fn discover_account_homes_from_env(configured: &[String]) -> Vec<AccountHome> {
    discover_account_homes(
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        configured,
        // `USERPROFILE` on Windows, `HOME` elsewhere — `dirs` already resolves
        // both, and the runner's own device-identity code uses the same door.
        dirs::home_dir(),
        Some(PathBuf::from("C:\\claude")),
    )
}

/// The machine-global Claude account roster's `claude_config_dirs`, or an
/// empty vector.
///
/// Reads exactly one field out of `claude-accounts.json`, whose canonical
/// unscoped path is `<config_root>/com.qontinui.runner/claude-accounts.json`
/// (`crate::claude_accounts` is the WRITER and the full model — this is a
/// read-only projection of one field for the CLI, which cannot reach that
/// bin-crate module). Fail-open on every error, exactly like the writer's own
/// loader: a missing or corrupt roster must degrade to the directory scan
/// rather than abort a backfill.
pub fn roster_config_dirs() -> Vec<String> {
    let Some(path) =
        dirs::config_dir().map(|d| d.join("com.qontinui.runner").join("claude-accounts.json"))
    else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    value
        .get("claude_config_dirs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Directory Claude Code writes a session's SUBAGENT side-transcripts into —
/// `projects/<encoded-cwd>/<session-id>/subagents/agent-<hex>.jsonl`.
///
/// Deliberately NOT part of the session corpus; see [`transcripts_in`].
const SUBAGENT_SUBDIR: &str = "subagents";

/// Every `<config_dir>/projects/<encoded-cwd>/*.jsonl` **session** transcript
/// under one account home.
///
/// ## Why only that one level — and the 8,308 vs 6,407 discrepancy
///
/// Claude Code also writes `projects/<encoded-cwd>/<session-id>/subagents/
/// agent-<hex>.jsonl` for every subagent a session spawns. Measured on the
/// operator box 2026-08-27, those are **1,896 of the 8,303 `.jsonl` files** in
/// these homes — which is where the plan's headline "8,308 transcripts" figure
/// comes from. They are not sessions: their stem is `agent-<hex>` rather than a
/// Claude session id, nothing can `claude --resume` one, and filing them as
/// head rows would put ~1,900 unresumable entries into a corpus whose whole
/// purpose is `GET /unfinished`.
///
/// So this returns **6,407** paths on that machine, not 8,303, and the run
/// report states the skipped count out loud
/// ([`count_subagent_transcripts`]) rather than leaving the gap to be
/// discovered as a shortfall against Phase 6's "within 5% of the on-disk file
/// count" criterion.
///
/// Returned sorted by path so a run's ordering — and therefore its `--limit`
/// slice — is reproducible. Unreadable project directories are skipped rather
/// than aborting the home: one bad directory must not cost the other 1,000
/// transcripts.
pub fn transcripts_in(home: &AccountHome) -> Vec<PathBuf> {
    let projects = home.config_dir.join(PROJECTS_SUBDIR);
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for project in entries.flatten() {
        let dir = project.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// How many subagent side-transcripts [`transcripts_in`] deliberately left out
/// of one account home.
///
/// Reported by the backfill so the difference between the file count on disk
/// and the row count in the corpus is a stated number rather than an
/// unexplained shortfall.
pub fn count_subagent_transcripts(home: &AccountHome) -> usize {
    let projects = home.config_dir.join(PROJECTS_SUBDIR);
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return 0;
    };
    let mut count = 0usize;
    for project in entries.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let dir = session.path().join(SUBAGENT_SUBDIR);
            if !dir.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&dir) {
                count += files
                    .flatten()
                    .filter(|f| f.path().extension().and_then(|e| e.to_str()) == Some("jsonl"))
                    .count();
            }
        }
    }
    count
}

/// Index the discovered homes by label, for joining a registry record's
/// `account_label` back to its home.
pub fn by_label(homes: &[AccountHome]) -> HashMap<String, &AccountHome> {
    homes.iter().map(|h| (h.label.clone(), h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_home(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(PROJECTS_SUBDIR)).unwrap();
        dir
    }

    #[test]
    fn account_mapping_covers_the_roster_and_both_fallbacks() {
        assert_eq!(
            account_label_for(Some("C:/claude/.claude-gmail")),
            ("gmail".into(), "clg".into())
        );
        assert_eq!(
            account_label_for(Some("C:/claude/.claude-tiohorst")).0,
            "tiohorst"
        );
        // An unknown suffix keeps its label but gets the generic wrapper.
        assert_eq!(
            account_label_for(Some("C:/claude/.claude-weird")),
            ("weird".into(), "claude".into())
        );
        // The plain default home has no `.claude-<x>` suffix.
        assert_eq!(
            account_label_for(Some("C:/Users/jspin/.claude")).0,
            "unknown"
        );
        assert_eq!(account_label_for(None).0, "unknown");
    }

    #[test]
    fn discovery_follows_the_documented_precedence_and_dedupes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let env_home = make_home(root, ".claude-envpin");
        let configured = make_home(root, ".claude-configured");
        let scanned = make_home(root, ".claude-scanned");
        let default_home = make_home(root, ".claude");

        let homes = discover_account_homes(
            Some(env_home.to_string_lossy().to_string()),
            // The env pin is repeated in the roster on purpose: the result
            // must not carry it twice.
            &[
                env_home.to_string_lossy().to_string(),
                configured.to_string_lossy().to_string(),
            ],
            Some(root.to_path_buf()),
            Some(root.to_path_buf()),
        );

        // env pin first, then the roster's new entry, then the only dir the
        // scan adds that was not already seen, then the default home. The
        // scan re-offers `.claude-envpin` and `.claude-configured`; dedup
        // keeps the first sighting, so neither appears twice.
        let labels: Vec<&str> = homes.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, vec!["envpin", "configured", "scanned", "unknown"]);
        let _ = (scanned, default_home);
    }

    #[test]
    fn a_directory_without_projects_is_not_an_account_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".claude-empty")).unwrap();
        make_home(root, ".claude-real");

        let homes = discover_account_homes(None, &[], None, Some(root.to_path_buf()));
        let labels: Vec<&str> = homes.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, vec!["real"]);
    }

    #[test]
    fn transcripts_are_listed_across_projects_in_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home_dir = make_home(tmp.path(), ".claude-gmail");
        let projects = home_dir.join(PROJECTS_SUBDIR);
        std::fs::create_dir_all(projects.join("b-proj")).unwrap();
        std::fs::create_dir_all(projects.join("a-proj")).unwrap();
        std::fs::write(projects.join("b-proj").join("s2.jsonl"), b"{}").unwrap();
        std::fs::write(projects.join("a-proj").join("s1.jsonl"), b"{}").unwrap();
        // Not a transcript — must be ignored.
        std::fs::write(projects.join("a-proj").join("notes.md"), b"#").unwrap();

        let home = AccountHome {
            config_dir: home_dir,
            label: "gmail".into(),
            wrapper: "clg".into(),
        };
        let found = transcripts_in(&home);
        let stems: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(stems, vec!["s1.jsonl", "s2.jsonl"]);
    }

    #[test]
    fn subagent_side_transcripts_are_excluded_and_counted() {
        // 1,896 of the 8,303 `.jsonl` files on the operator box are these.
        // They are not sessions and cannot be resumed, so they must not become
        // head rows — but the difference has to be a stated number rather than
        // an unexplained shortfall against the on-disk count.
        let tmp = tempfile::tempdir().unwrap();
        let home_dir = make_home(tmp.path(), ".claude-gmail");
        let project = home_dir.join(PROJECTS_SUBDIR).join("D--qontinui-root");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("real-session.jsonl"), b"{}").unwrap();
        let subagents = project.join("real-session").join(SUBAGENT_SUBDIR);
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent-a28e7300dafe7f389.jsonl"), b"{}").unwrap();
        std::fs::write(subagents.join("agent-a8f9dbf6080f6c721.jsonl"), b"{}").unwrap();

        let home = AccountHome {
            config_dir: home_dir,
            label: "gmail".into(),
            wrapper: "clg".into(),
        };
        let found = transcripts_in(&home);
        assert_eq!(found.len(), 1, "only the session transcript is a session");
        assert!(found[0].ends_with("real-session.jsonl"));
        assert_eq!(count_subagent_transcripts(&home), 2);
    }
}
