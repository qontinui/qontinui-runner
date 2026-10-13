//! `session_info_list(ids?)` — the batched session-info read, shared by the
//! Tauri command and its HTTP twin `GET /control/sessions/info` (plan
//! `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
//! Phase 4).
//!
//! The Session Manager hydrates its query facts (repo, branch, PRs, account,
//! the finished marker) for every row on each refresh. One `session_info_get`
//! per row would be N IPC round-trips and N config-dir discoveries; this is
//! ONE call that discovers the config dirs and the transcript index once and
//! projects every requested session through the same
//! [`build_session_info`] the dropdown uses, so the two cannot disagree.
//!
//! ## Branch
//!
//! The per-session envelope carries no branch. The response adds `branches`,
//! one entry per DISTINCT working directory: a single `git rev-parse
//! --abbrev-ref HEAD`, deduped, bounded in concurrency
//! ([`BRANCH_PROBE_CONCURRENCY`]) and in time ([`BRANCH_PROBE_TIMEOUT`]),
//! capped in count ([`BRANCH_PROBE_MAX_DIRS`]) and cached briefly
//! ([`BRANCH_CACHE_TTL`]) so a polling panel does not fork git every refresh.
//! Every entry that could not be read says `unknown` with a reason — never an
//! absent branch that reads as "no branch".

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::session_info::{build_session_info, SessionInfoEnvelope, STATUS_OK, STATUS_UNAVAILABLE};
use crate::session::reconcile::DiskTranscriptIndex;
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};

/// `git rev-parse` processes allowed to run at once.
pub const BRANCH_PROBE_CONCURRENCY: usize = 4;
/// How long one `git rev-parse` may take before it is killed and the
/// directory reads `unknown`.
pub const BRANCH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Distinct working directories probed per call; the rest read `unknown`
/// (`probe_cap`), so a pathological listing cannot fork git hundreds of times.
pub const BRANCH_PROBE_MAX_DIRS: usize = 64;
/// How long a probed branch is reused before git is asked again.
pub const BRANCH_CACHE_TTL: Duration = Duration::from_secs(20);

/// `branches[].status`: git named a branch.
pub const BRANCH_OK: &str = "ok";
/// `branches[].status`: HEAD is detached (`rev-parse` printed `HEAD`).
pub const BRANCH_DETACHED: &str = "detached";
/// `branches[].status`: not established — see `reason`.
pub const BRANCH_UNKNOWN: &str = "unknown";

/// One working directory's branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingDirBranch {
    pub working_dir: String,
    /// [`BRANCH_OK`] | [`BRANCH_DETACHED`] | [`BRANCH_UNKNOWN`].
    pub status: String,
    /// Set only for [`BRANCH_OK`].
    pub branch: Option<String>,
    /// Set for [`BRANCH_UNKNOWN`]: `not_a_repo` | `git_failed` | `timeout` |
    /// `spawn_failed` | `probe_cap`.
    pub reason: Option<String>,
}

impl WorkingDirBranch {
    fn unknown(dir: &str, reason: &str) -> Self {
        Self {
            working_dir: dir.to_string(),
            status: BRANCH_UNKNOWN.to_string(),
            branch: None,
            reason: Some(reason.to_string()),
        }
    }
}

/// Response of `session_info_list` and `GET /control/sessions/info`.
///
/// `status: "unavailable"` always carries a `reason` and is never a bare
/// empty list — "this runner has zero sessions" and "the store is
/// unreachable" must not render the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsInfoListResponse {
    /// [`STATUS_OK`] | [`STATUS_UNAVAILABLE`].
    pub status: String,
    pub reason: Option<String>,
    /// Every requested session the store holds (all OPEN sessions when no ids
    /// were named), sorted by terminal id then session id.
    pub sessions: Vec<SessionInfoEnvelope>,
    /// Named ids the store has no record of.
    pub not_found: Vec<String>,
    /// One entry per distinct `placement.workingDir` among `sessions`.
    pub branches: Vec<WorkingDirBranch>,
}

impl SessionsInfoListResponse {
    pub fn unavailable(reason: &str) -> Self {
        Self {
            status: STATUS_UNAVAILABLE.to_string(),
            reason: Some(reason.to_string()),
            sessions: Vec::new(),
            not_found: Vec::new(),
            branches: Vec::new(),
        }
    }
}

/// The records a listing projects: the named ids (open or closed — a finished
/// or exited row still has facts), or every open record when none are named.
/// Pure over the store. Returns `(records, not_found)`.
pub fn select_records(
    store: &SessionLifecycleStore,
    ids: Option<&[String]>,
) -> (Vec<TerminalSessionRecord>, Vec<String>) {
    let (mut records, not_found) = match ids {
        None => (store.open_records(), Vec::new()),
        Some(ids) => {
            let wanted: BTreeSet<&str> = ids
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let mut found = Vec::new();
            let mut missing = Vec::new();
            for id in wanted {
                match store.get(id) {
                    Some(rec) => found.push(rec),
                    None => missing.push(id.to_string()),
                }
            }
            (found, missing)
        }
    };
    records.sort_by(|a, b| {
        a.terminal_id
            .cmp(&b.terminal_id)
            .then_with(|| a.claude_session_id.cmp(&b.claude_session_id))
    });
    (records, not_found)
}

/// The shared body of `session_info_list` and `GET /control/sessions/info`.
/// `store` is `None` when the lifecycle store is not in Tauri state.
pub async fn list_sessions_info(
    store: Option<Arc<SessionLifecycleStore>>,
    ids: Option<&[String]>,
) -> SessionsInfoListResponse {
    let Some(store) = store else {
        tracing::warn!(
            "session info list: lifecycle store not in Tauri state — reporting unavailable \
             WITH a reason (never a bare empty list)"
        );
        return SessionsInfoListResponse::unavailable("lifecycle_store_unavailable");
    };
    let (records, not_found) = select_records(&store, ids);
    // Discovered ONCE for the whole listing, not per session.
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let probe = DiskTranscriptIndex::discover();
    let mut sessions = Vec::with_capacity(records.len());
    for rec in &records {
        sessions.push(build_session_info(rec, &config_dirs, &probe).await);
    }
    let dirs: Vec<String> = records
        .iter()
        .filter_map(|r| r.working_dir.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(String::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let branches = probe_branches(&dirs, &GitRevParse).await;
    SessionsInfoListResponse {
        status: STATUS_OK.to_string(),
        reason: None,
        sessions,
        not_found,
        branches,
    }
}

/// `session_info_list(ids?)` — the Tauri twin of `GET /control/sessions/info`.
/// `ids` names Claude session ids (open or closed); absent lists every open
/// session.
#[tauri::command]
pub async fn session_info_list(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    ids: Option<Vec<String>>,
) -> Result<SessionsInfoListResponse, String> {
    Ok(list_sessions_info(Some(store.inner().clone()), ids.as_deref()).await)
}

// ── Branch probe ─────────────────────────────────────────────────────────────

/// What `git rev-parse --abbrev-ref HEAD` answered for one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevParse {
    /// Exit 0 with this stdout (trimmed).
    Printed(String),
    /// Non-zero exit; stderr says why.
    Failed(String),
    Timeout,
    SpawnFailed,
}

/// The process seam, so the dedup / cap / cache logic tests without git.
pub trait BranchProbe: Sync {
    fn rev_parse(&self, dir: &str) -> impl std::future::Future<Output = RevParse> + Send;
}

/// The real probe: one bounded, killed-on-timeout `git rev-parse`.
pub struct GitRevParse;

impl BranchProbe for GitRevParse {
    async fn rev_parse(&self, dir: &str) -> RevParse {
        let mut cmd = crate::process_helpers::tokio_no_window("git");
        cmd.args(["-C", dir, "rev-parse", "--abbrev-ref", "HEAD"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::select! {
            out = cmd.output() => out,
            _ = crate::off_runtime::deadline(BRANCH_PROBE_TIMEOUT) => return RevParse::Timeout,
        };
        match output {
            Err(_) => RevParse::SpawnFailed,
            Ok(o) if o.status.success() => {
                RevParse::Printed(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            Ok(o) => RevParse::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        }
    }
}

/// Classify one `rev-parse` answer. Pure.
pub fn classify_rev_parse(dir: &str, answer: RevParse) -> WorkingDirBranch {
    match answer {
        RevParse::Printed(s) if s == "HEAD" => WorkingDirBranch {
            working_dir: dir.to_string(),
            status: BRANCH_DETACHED.to_string(),
            branch: None,
            reason: None,
        },
        RevParse::Printed(s) if !s.is_empty() => WorkingDirBranch {
            working_dir: dir.to_string(),
            status: BRANCH_OK.to_string(),
            branch: Some(s),
            reason: None,
        },
        RevParse::Printed(_) => WorkingDirBranch::unknown(dir, "git_failed"),
        RevParse::Failed(stderr) if stderr.contains("not a git repository") => {
            WorkingDirBranch::unknown(dir, "not_a_repo")
        }
        RevParse::Failed(_) => WorkingDirBranch::unknown(dir, "git_failed"),
        RevParse::Timeout => WorkingDirBranch::unknown(dir, "timeout"),
        RevParse::SpawnFailed => WorkingDirBranch::unknown(dir, "spawn_failed"),
    }
}

type BranchCache = Mutex<HashMap<String, (Instant, WorkingDirBranch)>>;

fn branch_cache() -> &'static BranchCache {
    static CACHE: OnceLock<BranchCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe `dirs` (already distinct and sorted) through the global cache.
pub async fn probe_branches(dirs: &[String], probe: &impl BranchProbe) -> Vec<WorkingDirBranch> {
    probe_branches_with(dirs, probe, branch_cache(), Instant::now()).await
}

/// [`probe_branches`] over an explicit cache and clock — the tested core.
/// Distinct dirs past [`BRANCH_PROBE_MAX_DIRS`] read `unknown` (`probe_cap`);
/// a timed-out or unspawnable probe is not cached, so the next call retries.
pub async fn probe_branches_with(
    dirs: &[String],
    probe: &impl BranchProbe,
    cache: &BranchCache,
    now: Instant,
) -> Vec<WorkingDirBranch> {
    let mut out: Vec<Option<WorkingDirBranch>> = vec![None; dirs.len()];
    let mut to_probe = Vec::new();
    {
        let cached = cache.lock().ok();
        for (i, dir) in dirs.iter().enumerate() {
            if i >= BRANCH_PROBE_MAX_DIRS {
                out[i] = Some(WorkingDirBranch::unknown(dir, "probe_cap"));
                continue;
            }
            let fresh = cached
                .as_ref()
                .and_then(|c| c.get(dir))
                .filter(|(at, _)| now.saturating_duration_since(*at) < BRANCH_CACHE_TTL)
                .map(|(_, b)| b.clone());
            match fresh {
                Some(b) => out[i] = Some(b),
                None => to_probe.push(i),
            }
        }
    }
    let semaphore = tokio::sync::Semaphore::new(BRANCH_PROBE_CONCURRENCY);
    let probed = futures::future::join_all(to_probe.iter().map(|&i| {
        let semaphore = &semaphore;
        let dir = dirs[i].as_str();
        async move {
            let _permit = semaphore.acquire().await;
            let answer = probe.rev_parse(dir).await;
            let transient = matches!(answer, RevParse::Timeout | RevParse::SpawnFailed);
            (i, transient, classify_rev_parse(dir, answer))
        }
    }))
    .await;
    if let Ok(mut c) = cache.lock() {
        c.retain(|_, (at, _)| now.saturating_duration_since(*at) < BRANCH_CACHE_TTL);
        for (i, transient, branch) in &probed {
            if !transient {
                c.insert(dirs[*i].clone(), (now, branch.clone()));
            }
        }
    }
    for (i, _, branch) in probed {
        out[i] = Some(branch);
    }
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake git: answers by directory name and counts calls and the peak
    /// number in flight.
    struct FakeGit {
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }
        }
    }

    impl BranchProbe for FakeGit {
        async fn rev_parse(&self, dir: &str) -> RevParse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            match dir {
                "detached" => RevParse::Printed("HEAD".into()),
                "plain" => RevParse::Failed("fatal: not a git repository".into()),
                "slow" => RevParse::Timeout,
                d => RevParse::Printed(format!("branch-of-{d}")),
            }
        }
    }

    fn dirs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn every_dir_gets_one_classified_entry() {
        let git = FakeGit::new();
        let cache = Mutex::new(HashMap::new());
        let got = probe_branches_with(
            &dirs(&["a", "detached", "plain", "slow"]),
            &git,
            &cache,
            Instant::now(),
        )
        .await;
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].branch.as_deref(), Some("branch-of-a"));
        assert_eq!(got[0].status, BRANCH_OK);
        assert_eq!(got[1].status, BRANCH_DETACHED);
        assert_eq!(got[2].reason.as_deref(), Some("not_a_repo"));
        assert_eq!(got[3].status, BRANCH_UNKNOWN);
        assert_eq!(got[3].reason.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn concurrency_is_bounded() {
        let git = FakeGit::new();
        let cache = Mutex::new(HashMap::new());
        let many: Vec<String> = (0..20).map(|i| format!("d{i}")).collect();
        probe_branches_with(&many, &git, &cache, Instant::now()).await;
        assert_eq!(git.calls.load(Ordering::SeqCst), 20);
        assert!(git.peak.load(Ordering::SeqCst) <= BRANCH_PROBE_CONCURRENCY);
    }

    #[tokio::test]
    async fn a_fresh_cache_entry_is_reused_and_a_stale_one_reprobed() {
        let git = FakeGit::new();
        let cache = Mutex::new(HashMap::new());
        let t0 = Instant::now();
        probe_branches_with(&dirs(&["a", "slow"]), &git, &cache, t0).await;
        assert_eq!(git.calls.load(Ordering::SeqCst), 2);
        // Within the TTL: `a` is cached; `slow` timed out and is retried.
        probe_branches_with(
            &dirs(&["a", "slow"]),
            &git,
            &cache,
            t0 + Duration::from_secs(1),
        )
        .await;
        assert_eq!(git.calls.load(Ordering::SeqCst), 3);
        // Past the TTL: asked again.
        probe_branches_with(&dirs(&["a"]), &git, &cache, t0 + BRANCH_CACHE_TTL * 2).await;
        assert_eq!(git.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn dirs_past_the_cap_are_unknown_not_absent() {
        let git = FakeGit::new();
        let cache = Mutex::new(HashMap::new());
        let many: Vec<String> = (0..BRANCH_PROBE_MAX_DIRS + 3)
            .map(|i| format!("d{i:03}"))
            .collect();
        let got = probe_branches_with(&many, &git, &cache, Instant::now()).await;
        assert_eq!(got.len(), many.len());
        assert_eq!(git.calls.load(Ordering::SeqCst), BRANCH_PROBE_MAX_DIRS);
        assert!(got[BRANCH_PROBE_MAX_DIRS..]
            .iter()
            .all(|b| b.status == BRANCH_UNKNOWN && b.reason.as_deref() == Some("probe_cap")));
    }

    #[test]
    fn empty_stdout_is_unknown_not_a_blank_branch() {
        let b = classify_rev_parse("x", RevParse::Printed(String::new()));
        assert_eq!(b.status, BRANCH_UNKNOWN);
        assert!(b.branch.is_none());
    }

    #[tokio::test]
    async fn the_real_probe_reads_a_real_repo_and_refuses_a_plain_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q", "-b", "feature-x"])
            .current_dir(&repo)
            .status();
        let Ok(status) = ok else {
            eprintln!("git not on PATH — skipping the real-probe arm");
            return;
        };
        assert!(status.success());
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let cache = Mutex::new(HashMap::new());
        let got = probe_branches_with(
            &[
                repo.to_string_lossy().into_owned(),
                plain.to_string_lossy().into_owned(),
            ],
            &GitRevParse,
            &cache,
            Instant::now(),
        )
        .await;
        // A fresh repo has no commit, so `rev-parse --abbrev-ref HEAD` fails
        // on an unborn branch — UNKNOWN, never a guessed name.
        assert!(got[0].status == BRANCH_UNKNOWN || got[0].branch.as_deref() == Some("feature-x"));
        assert_eq!(got[1].status, BRANCH_UNKNOWN);
    }

    fn record(id: &str, terminal: &str, state: &str) -> TerminalSessionRecord {
        serde_json::from_value(serde_json::json!({
            "claudeSessionId": id,
            "configDir": null,
            "workingDir": null,
            "pageId": "default",
            "zoneIndex": 0,
            "title": null,
            "terminalId": terminal,
            "openedAt": 1_000,
            "lastSeenAt": 2_000,
            "state": state,
            "closedAt": null,
            "closeReason": null,
            "confirmedAt": 1_500,
        }))
        .expect("record fixture")
    }

    #[test]
    fn select_records_names_ids_open_or_closed_and_lists_the_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(tmp.path().join("s.json")).unwrap();
        store.record_open(record("a", "t2", "open"));
        store.record_open(record("b", "t1", "open"));
        store.record_close("b", "exited");

        let (open, missing) = select_records(&store, None);
        assert_eq!(
            open.iter()
                .map(|r| r.claude_session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
        assert!(missing.is_empty());

        let ids = vec![
            "a".to_string(),
            "b".to_string(),
            "ghost".to_string(),
            " ".to_string(),
        ];
        let (named, missing) = select_records(&store, Some(&ids));
        // Sorted by terminal id: b (t1) before a (t2); the closed one included.
        assert_eq!(
            named
                .iter()
                .map(|r| r.claude_session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(missing, vec!["ghost"]);
    }

    #[tokio::test]
    async fn no_store_is_unavailable_with_a_reason() {
        let r = list_sessions_info(None, None).await;
        assert_eq!(r.status, STATUS_UNAVAILABLE);
        assert_eq!(r.reason.as_deref(), Some("lifecycle_store_unavailable"));
    }

    #[tokio::test]
    async fn a_listing_projects_each_named_session_and_its_finished_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionLifecycleStore::open(tmp.path().join("s.json")).unwrap());
        let mut rec = record("11111111-1111-1111-1111-111111111111", "t1", "open");
        rec.working_dir = Some(tmp.path().to_string_lossy().into_owned());
        store.record_open(rec);
        let ids = vec![
            "11111111-1111-1111-1111-111111111111".to_string(),
            "ghost".to_string(),
        ];
        let r = list_sessions_info(Some(store), Some(&ids)).await;
        assert_eq!(r.status, STATUS_OK);
        assert_eq!(r.sessions.len(), 1);
        assert!(r.sessions[0].available);
        let body = r.sessions[0].body.as_ref().unwrap();
        assert_eq!(body.lifecycle.finished_at, None);
        assert_eq!(r.not_found, vec!["ghost"]);
        // One branch entry for the one distinct working dir.
        assert_eq!(r.branches.len(), 1);
        assert_eq!(r.branches[0].working_dir, tmp.path().to_string_lossy());
    }
}
