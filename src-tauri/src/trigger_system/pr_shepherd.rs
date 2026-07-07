//! PR Shepherd — device-local, coord-independent PR watch substrate.
//!
//! Phases 1+2 of plan `2026-07-04-runner-pr-shepherd.md`:
//!
//! - **Phase 1 (seeding):** the transcript watcher detects `gh pr create`
//!   invocations in interactive session transcripts
//!   ([`crate::terminal::pr_open_report`]) and enqueues a [`PendingPrSeed`]
//!   here. The PR watcher ([`super::watchers::pr_watcher`]) drains the queue
//!   on its next poll, resolves branch-only seeds to a PR number via
//!   `GET /pulls?head=<owner>:<branch>`, and upserts a `pr_watch_state` row —
//!   so PRs opened by *interactive* sessions get watched, not just task runs.
//! - **Phase 2 (green-but-unmerged):** [`green_threshold`] feeds the
//!   `PrAction::GreenButUnmerged` detection in `determine_pr_action` — a PR
//!   fully green on an unchanged head for longer than the threshold while
//!   still open. Deliberately tighter (45m) than coord's operator-facing
//!   `pr_merge_green_unlanded` alert (2h dwell) and computed from GitHub data
//!   only, so it fires even when coord itself is down or wedged.
//!
//! ## Config gates
//!
//! - `QONTINUI_PR_SHEPHERD` — master flag, default **OFF**. Only a truthy
//!   value (`1`/`true`/`on`/`yes`, case-insensitive) enables any shepherd
//!   behavior (mirrors `QONTINUI_AGENT_LOGS_FROM_SESSIONS`' strict posture).
//! - `QONTINUI_PR_SHEPHERD_AUTOSEED` — gates Phase-1 seeding specifically.
//!   Default **ON when the master flag is on**; `0`/`false`/`off` disables.
//! - `QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD` — green-but-unmerged dwell.
//!   Default 45m. Accepts a bare integer (minutes) or an `s`/`m`/`h` suffix
//!   (`2700s`, `45m`, `2h`).
//! - Per-repo allowlist: the `shepherd_repos` field on
//!   [`super::types::TriggerConfig::PrWatch`] — looked up at the same site as
//!   the trigger's `github_token` (`service.rs`) and enforced at seed time.
//!   Empty = all repos allowed.
//!
//! ## Seed queue
//!
//! The queue is a process-global, capped, in-memory `VecDeque` (mirrors
//! `commit_report`'s `LAST_REPORTED` idiom). Seeds observed while no PR-watch
//! trigger is running — or lost to a runner restart — are dropped; that is
//! acceptable because a seeded row is only ever *acted on* by the PR watcher,
//! so a seed without a running watcher is inert anyway, and the upsert is
//! idempotent when the same PR is observed again.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;

/// Default green-but-unmerged dwell before `PrAction::GreenButUnmerged` fires.
pub const DEFAULT_GREEN_THRESHOLD: Duration = Duration::from_secs(45 * 60);

/// Upper bound on buffered seeds. Oldest drop first — the queue only ever
/// holds seeds between transcript observation and the next PR-watcher poll
/// (≤ ~30s in a healthy runner), so 64 is generous.
const SEED_QUEUE_CAP: usize = 64;

/// Master gate. Default **OFF**; only `1`/`true`/`on`/`yes` (case-insensitive)
/// enables — anything else, including unset, leaves every shepherd surface a
/// strict no-op.
pub fn shepherd_enabled() -> bool {
    matches!(
        std::env::var("QONTINUI_PR_SHEPHERD").map(|v| v.trim().to_ascii_lowercase()),
        Ok(ref v) if matches!(v.as_str(), "1" | "true" | "on" | "yes")
    )
}

/// Phase-1 seeding gate. Requires the master flag; then default **ON** unless
/// `QONTINUI_PR_SHEPHERD_AUTOSEED` is `0`/`false`/`off` (case-insensitive) —
/// the same default-on shape as `QONTINUI_COMMIT_LINEAGE_REPORT`.
pub fn autoseed_enabled() -> bool {
    if !shepherd_enabled() {
        return false;
    }
    match std::env::var("QONTINUI_PR_SHEPHERD_AUTOSEED") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// Phase-3 diagnosis gate. Requires the master flag; then default **ON** unless
/// `QONTINUI_PR_SHEPHERD_DIAGNOSE` is `0`/`false`/`off` (case-insensitive).
/// Gates the coord verdict/events/graph/queue/health calls specifically, so
/// Phase 3 can run log-only with diagnosis ON while later action phases stay
/// OFF — the same default-on shape as [`autoseed_enabled`].
pub fn diagnose_enabled() -> bool {
    if !shepherd_enabled() {
        return false;
    }
    match std::env::var("QONTINUI_PR_SHEPHERD_DIAGNOSE") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// Green-but-unmerged dwell threshold. Reads
/// `QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD` (bare minutes or `s`/`m`/`h`
/// suffix); an unset or unparseable value falls back to
/// [`DEFAULT_GREEN_THRESHOLD`] (45m).
pub fn green_threshold() -> Duration {
    std::env::var("QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD")
        .ok()
        .and_then(|v| parse_threshold(&v))
        .unwrap_or(DEFAULT_GREEN_THRESHOLD)
}

/// Parse a threshold string: `"45"` → 45 minutes, `"90s"` → 90 seconds,
/// `"45m"` → 45 minutes, `"2h"` → 2 hours. Returns `None` for anything else
/// (including zero — a zero threshold would fire on every green poll).
pub fn parse_threshold(raw: &str) -> Option<Duration> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let (num, unit_secs): (&str, u64) = match s.as_bytes()[s.len() - 1] {
        b's' => (&s[..s.len() - 1], 1),
        b'm' => (&s[..s.len() - 1], 60),
        b'h' => (&s[..s.len() - 1], 3600),
        _ => (s.as_str(), 60), // bare integer = minutes
    };
    let n: u64 = num.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(Duration::from_secs(n * unit_secs))
}

/// Per-repo allowlist check. An empty allowlist allows all repos; otherwise a
/// token must match either the `owner/repo` full name or the bare repo name,
/// case-insensitively (the same repo-token shape the PrWatch trigger config
/// deals in).
pub fn repo_allowed(repo_full_name: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let full = repo_full_name.trim().to_ascii_lowercase();
    let short = full.rsplit('/').next().unwrap_or(&full).to_string();
    allowlist.iter().any(|t| {
        let t = t.trim().to_ascii_lowercase();
        !t.is_empty() && (t == full || t == short)
    })
}

/// Who authored the PR — the identity a seeded `pr_watch_state` row is keyed
/// on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedIdentity {
    /// The transcript session IS a task run: key the row on `task_run_id`
    /// (the existing `(task_run_id, pr_number)` upsert path), carrying the
    /// coord session id alongside when the registrar resolved one.
    TaskRun {
        task_run_id: String,
        authoring_session_id: Option<String>,
    },
    /// Pure interactive session: no task run to attribute — key the row on
    /// the nullable `authoring_session_id` column instead.
    Session { authoring_session_id: String },
}

/// What the transcript observation resolved the PR to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedTarget {
    /// The `gh pr create` tool_result carried the PR URL — fully resolved.
    Pr {
        repo_full_name: String,
        pr_number: u64,
    },
    /// No URL was available; the PR watcher resolves the number via
    /// `GET /pulls?head=<owner>:<branch>` on its next poll.
    Branch {
        repo_full_name: String,
        branch: String,
    },
}

/// One pending PR-watch seed, produced by the transcript watcher and consumed
/// by the PR watcher's poll loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPrSeed {
    pub identity: SeedIdentity,
    pub target: SeedTarget,
}

/// Process-global pending-seed queue (transcript watcher → PR watcher).
static PENDING_SEEDS: Lazy<Mutex<VecDeque<PendingPrSeed>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));

/// Enqueue a seed for the PR watcher's next poll. Duplicate (identity, target)
/// pairs already pending are dropped; on overflow the oldest seed drops first.
pub fn enqueue_seed(seed: PendingPrSeed) {
    let mut q = PENDING_SEEDS.lock().unwrap_or_else(|e| e.into_inner());
    if q.iter().any(|s| *s == seed) {
        return;
    }
    if q.len() >= SEED_QUEUE_CAP {
        q.pop_front();
    }
    q.push_back(seed);
}

/// Drain all pending seeds (FIFO). Called by the PR watcher once per poll.
pub fn drain_seeds() -> Vec<PendingPrSeed> {
    let mut q = PENDING_SEEDS.lock().unwrap_or_else(|e| e.into_inner());
    q.drain(..).collect()
}

/// Test-only: clear the seed queue so tests don't bleed into each other.
#[cfg(test)]
pub fn reset_seeds_for_test() {
    PENDING_SEEDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars are process-global mutable state; tests that set/remove them
    /// race under the parallel test harness. Every env-touching test holds
    /// this lock for its full body (same idiom as `coord_register`'s tests).
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn seed(n: u64) -> PendingPrSeed {
        PendingPrSeed {
            identity: SeedIdentity::Session {
                authoring_session_id: "sess-1".to_string(),
            },
            target: SeedTarget::Pr {
                repo_full_name: "qontinui/qontinui-runner".to_string(),
                pr_number: n,
            },
        }
    }

    #[test]
    fn master_gate_defaults_off_and_only_truthy_enables() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_PR_SHEPHERD");
        assert!(!shepherd_enabled(), "unset must be OFF");

        for off in ["0", "false", "off", "", "no", "garbage"] {
            std::env::set_var("QONTINUI_PR_SHEPHERD", off);
            assert!(!shepherd_enabled(), "value {off:?} must NOT enable");
        }
        for on in ["1", "true", "on", "yes", "TRUE", "On"] {
            std::env::set_var("QONTINUI_PR_SHEPHERD", on);
            assert!(shepherd_enabled(), "value {on:?} must enable");
        }
        std::env::remove_var("QONTINUI_PR_SHEPHERD");
    }

    #[test]
    fn autoseed_requires_master_and_defaults_on_under_it() {
        let _env = env_lock();
        // Master off → autoseed off regardless of its own var.
        std::env::remove_var("QONTINUI_PR_SHEPHERD");
        std::env::set_var("QONTINUI_PR_SHEPHERD_AUTOSEED", "1");
        assert!(!autoseed_enabled(), "autoseed must be gated on the master flag");

        // Master on, autoseed unset → ON (default).
        std::env::set_var("QONTINUI_PR_SHEPHERD", "1");
        std::env::remove_var("QONTINUI_PR_SHEPHERD_AUTOSEED");
        assert!(autoseed_enabled(), "autoseed defaults ON under the master flag");

        // Master on, autoseed explicitly off.
        std::env::set_var("QONTINUI_PR_SHEPHERD_AUTOSEED", "off");
        assert!(!autoseed_enabled());

        std::env::remove_var("QONTINUI_PR_SHEPHERD");
        std::env::remove_var("QONTINUI_PR_SHEPHERD_AUTOSEED");
    }

    #[test]
    fn diagnose_requires_master_and_defaults_on_under_it() {
        let _env = env_lock();
        // Master off → diagnose off regardless of its own var.
        std::env::remove_var("QONTINUI_PR_SHEPHERD");
        std::env::set_var("QONTINUI_PR_SHEPHERD_DIAGNOSE", "1");
        assert!(!diagnose_enabled(), "diagnose must be gated on the master flag");

        // Master on, diagnose unset → ON (default).
        std::env::set_var("QONTINUI_PR_SHEPHERD", "1");
        std::env::remove_var("QONTINUI_PR_SHEPHERD_DIAGNOSE");
        assert!(diagnose_enabled(), "diagnose defaults ON under the master flag");

        // Master on, diagnose explicitly off.
        std::env::set_var("QONTINUI_PR_SHEPHERD_DIAGNOSE", "off");
        assert!(!diagnose_enabled());

        std::env::remove_var("QONTINUI_PR_SHEPHERD");
        std::env::remove_var("QONTINUI_PR_SHEPHERD_DIAGNOSE");
    }

    #[test]
    fn threshold_parses_bare_minutes_and_suffixes() {
        assert_eq!(parse_threshold("45"), Some(Duration::from_secs(45 * 60)));
        assert_eq!(parse_threshold("45m"), Some(Duration::from_secs(45 * 60)));
        assert_eq!(parse_threshold("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_threshold("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_threshold(" 10 m "), Some(Duration::from_secs(600)));
        assert_eq!(parse_threshold("0"), None, "zero would fire every poll");
        assert_eq!(parse_threshold("garbage"), None);
        assert_eq!(parse_threshold(""), None);
    }

    #[test]
    fn green_threshold_falls_back_to_default() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD");
        assert_eq!(green_threshold(), DEFAULT_GREEN_THRESHOLD);
        std::env::set_var("QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD", "not-a-duration");
        assert_eq!(green_threshold(), DEFAULT_GREEN_THRESHOLD);
        std::env::set_var("QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD", "30m");
        assert_eq!(green_threshold(), Duration::from_secs(1800));
        std::env::remove_var("QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD");
    }

    #[test]
    fn repo_allowlist_empty_allows_all_and_matches_full_or_short() {
        assert!(repo_allowed("qontinui/qontinui-runner", &[]));

        let list = vec!["qontinui/qontinui-runner".to_string(), "ui-bridge".to_string()];
        assert!(repo_allowed("qontinui/qontinui-runner", &list));
        assert!(repo_allowed("QONTINUI/QONTINUI-RUNNER", &list), "case-insensitive");
        assert!(repo_allowed("qontinui/ui-bridge", &list), "short-name token matches");
        assert!(!repo_allowed("qontinui/qontinui-web", &list));
    }

    #[test]
    fn seed_queue_dedups_and_caps() {
        reset_seeds_for_test();
        enqueue_seed(seed(1));
        enqueue_seed(seed(1)); // duplicate — dropped
        enqueue_seed(seed(2));
        let drained = drain_seeds();
        assert_eq!(drained.len(), 2);
        assert_eq!(
            drained
                .iter()
                .map(|s| match &s.target {
                    SeedTarget::Pr { pr_number, .. } => *pr_number,
                    SeedTarget::Branch { .. } => 0,
                })
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        // Queue is empty after a drain.
        assert!(drain_seeds().is_empty());

        // Cap: oldest drops first.
        for n in 0..(SEED_QUEUE_CAP as u64 + 5) {
            enqueue_seed(seed(n));
        }
        let drained = drain_seeds();
        assert_eq!(drained.len(), SEED_QUEUE_CAP);
        match &drained[0].target {
            SeedTarget::Pr { pr_number, .. } => assert_eq!(*pr_number, 5, "oldest 5 dropped"),
            SeedTarget::Branch { .. } => panic!("unexpected branch target"),
        }
    }
}
