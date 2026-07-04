//! Probe executor — the runner half of PR 5 of
//! `plans/2026-07-03-subagent-stall-watchdog.md` (§3 D3).
//!
//! A device-JWT-authed pull-loop (the `maintenance_executor` /
//! `session_message_poller` armed-instruction pattern): `GET
//! /coord/probes/pending/{device_id}` → execute each CLOSED, typed,
//! **READ-ONLY** probe → `POST /coord/probes/{id}/result`. The supervisor
//! (coord, leader-gated) only ever enqueues a probe for an expectation that
//! has already escalated to rung 2; this loop answers with typed evidence so
//! the supervisor can tell "still working" from "finished but uncollected"
//! from "dead".
//!
//! ## Hard safety rules (plan §0/§7)
//!
//! - **Closed typed catalog** — four kinds only; there is NO arbitrary-command
//!   channel. An unrecognized kind returns a typed error, never executes.
//! - **Read-only** — every probe only READS (fs metadata, `git` plumbing on
//!   the read allowlist [`READ_ONLY_GIT`], a process-table snapshot). No probe
//!   writes, spawns work, or mutates a worktree.
//! - **Worktree-scoped, no requester path** — the probe carries at most a
//!   coord-*server-resolved* `worktree_path`; before ANY fs/git access the
//!   runner RE-VALIDATES that the path canonicalizes to a real directory
//!   **contained under this device's `qontinui_root`** ([`is_scoped`]). A path
//!   that escapes the root is rejected — the probe cannot read outside the
//!   device's own worktrees (the RCE guard).
//! - **Fail-open** — any resolution failure yields a typed `error` result;
//!   coord interprets that as inconclusive and leaves the work alone.
//!
//! ## Arming
//!
//! Behind `RUNNER_PROBES_ENABLED` (default OFF — this is a NEW active pull-loop
//! and the plan's "arm one flag at a time" ramp applies; coord's supervisor
//! tri-state is the primary enqueue gate, so nothing is lost while dark). When
//! disabled the loop never spawns.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The arming flag (default OFF — see module docs).
const RUNNER_PROBES_FLAG: &str = "RUNNER_PROBES_ENABLED";

/// Poll cadence (seconds); env `RUNNER_PROBE_INTERVAL_SECS`, floored at 15.
const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Bound the newest-mtime walk so a huge worktree cannot stall a tick.
const MTIME_WALK_MAX_DEPTH: usize = 4;
const MTIME_WALK_MAX_ENTRIES: usize = 20_000;

/// Directory names skipped by the mtime walk (VCS/build noise that would
/// dominate freshness and blow the entry budget).
const MTIME_WALK_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "dist"];

/// The ONLY git subcommands a probe may run — all read-only plumbing. A
/// test pins this list so a future edit can't smuggle in a mutating verb.
pub const READ_ONLY_GIT: &[&str] = &["rev-parse", "rev-list", "symbolic-ref", "status", "cherry"];

/// Is `RUNNER_PROBES_ENABLED` truthy? Default OFF (the `fs_backstop_enabled`
/// shape).
pub fn probes_enabled() -> bool {
    matches!(
        std::env::var(RUNNER_PROBES_FLAG).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") | Some("on")
    )
}

fn interval() -> Duration {
    let secs = std::env::var("RUNNER_PROBE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(15);
    Duration::from_secs(secs)
}

// ---------------------------------------------------------------------------
// Wire types (mirror coord's `expectation_probes`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PendingProbesResponse {
    #[serde(default)]
    probes: Vec<PendingProbe>,
}

#[derive(Debug, Deserialize)]
struct PendingProbe {
    id: String,
    #[serde(default)]
    probe_kind: String,
    #[serde(default)]
    probe_args: Value,
}

/// The typed result the runner POSTs back — a superset whose fields coord's
/// `ProbeResult` reads by kind. All optional (fail-open).
#[derive(Debug, Default, Serialize)]
pub struct ProbeResultBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_mtime_secs_ago: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_unpushed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProbeResultBody {
    fn err(msg: impl Into<String>) -> Self {
        ProbeResultBody {
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// The RCE guard — worktree containment (pure, unit-tested)
// ---------------------------------------------------------------------------

/// `true` iff `candidate` is contained under `root` (both expected absolute /
/// canonical). This is the path-scoping guard: a probe may only touch a
/// worktree under the device's own `qontinui_root`.
pub fn is_scoped(candidate: &Path, root: &Path) -> bool {
    // `starts_with` is component-wise (not a string prefix), so
    // `/root-evil` is NOT under `/root`. A `..` that climbs out fails once
    // canonicalized (canonicalize resolves `..`); we additionally reject any
    // literal `..` component defensively for the non-canonicalizable path.
    if candidate.components().any(|c| c.as_os_str() == "..") {
        return false;
    }
    candidate.starts_with(root)
}

/// Resolve the coord-supplied `worktree_path` to a REAL directory contained
/// under this device's `qontinui_root`. Returns the canonical path or a typed
/// error string (fail-open). The requester never supplies a path coord did not
/// server-resolve, and this re-validation means even a malicious instruction
/// cannot escape the device's worktrees.
fn resolve_scoped_worktree(args: &Value) -> Result<PathBuf, String> {
    let raw = args
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "no worktree_path in probe args".to_string())?;
    let root = crate::agent_worktree::census::qontinui_root()
        .ok_or_else(|| "cannot resolve qontinui_root on this device".to_string())?;
    let root_c =
        std::fs::canonicalize(&root).map_err(|e| format!("canonicalize root failed: {e}"))?;
    let cand =
        std::fs::canonicalize(raw).map_err(|e| format!("worktree_path does not resolve: {e}"))?;
    if !cand.is_dir() {
        return Err("worktree_path is not a directory".to_string());
    }
    if !is_scoped(&cand, &root_c) {
        return Err("worktree_path escapes the device worktree root (rejected)".to_string());
    }
    Ok(cand)
}

// ---------------------------------------------------------------------------
// Read-only collectors (reuse the runner's existing idioms)
// ---------------------------------------------------------------------------

/// Read-only `git -C <worktree> <args>` → trimmed stdout on success. Refuses
/// any subcommand not on [`READ_ONLY_GIT`] (defense in depth — the callers
/// already only pass read verbs).
fn git_read(worktree: &Path, args: &[&str]) -> Option<String> {
    match args.first() {
        Some(sub) if READ_ONLY_GIT.contains(sub) => {}
        _ => return None,
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Newest mtime under `root` (bounded walk), as seconds before `now`. `None`
/// when nothing readable. Pure over the filesystem + a clock (read-only).
pub fn newest_mtime_secs_ago(root: &Path, now: SystemTime) -> Option<i64> {
    let mut newest: Option<SystemTime> = None;
    let mut budget = MTIME_WALK_MAX_ENTRIES;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if budget == 0 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if MTIME_WALK_SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
                    continue;
                }
                if depth < MTIME_WALK_MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if let Ok(m) = entry.metadata() {
                if let Ok(modified) = m.modified() {
                    if newest.map(|n| modified > n).unwrap_or(true) {
                        newest = Some(modified);
                    }
                }
            }
        }
    }
    newest.map(|t| {
        now.duration_since(t)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    })
}

/// Runner-wide claude liveness: does this runner's own process subtree contain
/// a live `claude*` process? Coarse-but-honest (never falsely dooms a specific
/// session — only reports dead when the WHOLE device is claude-free). PR 6
/// tightens this to a session-scoped root pid.
async fn runner_claude_alive() -> Option<bool> {
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let runner_pid = std::process::id();
    // reference=0 disables the PID-reuse skew guard (we check the live runner
    // itself, whose pid cannot have been reused under it).
    Some(
        crate::process_capture::process_tree::claude_present_in_inclusive_subtree(
            runner_pid, &snapshot, 0,
        ),
    )
}

// ---------------------------------------------------------------------------
// The typed probes
// ---------------------------------------------------------------------------

async fn probe_path_activity(args: &Value) -> ProbeResultBody {
    let process_alive = runner_claude_alive().await;
    match resolve_scoped_worktree(args) {
        Ok(wt) => {
            let secs = newest_mtime_secs_ago(&wt, SystemTime::now());
            ProbeResultBody {
                process_alive,
                newest_mtime_secs_ago: secs,
                ..Default::default()
            }
        }
        // No worktree, but liveness is still a usable signal (fail-open).
        Err(e) => ProbeResultBody {
            process_alive,
            error: Some(e),
            ..Default::default()
        },
    }
}

fn probe_git_branch_state(args: &Value) -> ProbeResultBody {
    let wt = match resolve_scoped_worktree(args) {
        Ok(w) => w,
        Err(e) => return ProbeResultBody::err(e),
    };
    let head_sha = git_read(&wt, &["rev-parse", "HEAD"]);
    let branch = args.get("branch").and_then(Value::as_str);
    let branch_exists = match branch {
        Some(b) => git_read(&wt, &["rev-parse", "--verify", &format!("refs/heads/{b}")]).is_some(),
        None => head_sha.is_some(),
    };
    // ahead/behind vs the upstream base (origin/main), read-only.
    let (ahead, behind) = git_read(
        &wt,
        &["rev-list", "--count", "--left-right", "origin/main...HEAD"],
    )
    .and_then(|s| {
        let mut it = s.split_whitespace();
        let behind = it.next()?.parse::<i64>().ok()?;
        let ahead = it.next()?.parse::<i64>().ok()?;
        Some((Some(ahead), Some(behind)))
    })
    .unwrap_or((None, None));
    // Unpushed = local commits not on origin/main (the PR-6 reclaim SAFETY
    // predicate). `git cherry` lists `+` for unpushed; count them.
    let has_unpushed = git_read(&wt, &["cherry", "origin/main"])
        .map(|s| s.lines().any(|l| l.trim_start().starts_with('+')));
    ProbeResultBody {
        branch_exists: Some(branch_exists),
        head_sha,
        ahead,
        behind,
        // `cherry` failing (no origin/main) → fall back to ahead>0.
        has_unpushed: has_unpushed.or_else(|| ahead.map(|a| a > 0)),
        ..Default::default()
    }
}

async fn probe_session_process_alive(_args: &Value) -> ProbeResultBody {
    ProbeResultBody {
        process_alive: runner_claude_alive().await,
        ..Default::default()
    }
}

fn probe_artifact_present(args: &Value) -> ProbeResultBody {
    // Typed existence: the session's worktree resolves to a real, scoped
    // directory. (Kind-specific artifact checks layer on the same guard.)
    match resolve_scoped_worktree(args) {
        Ok(_) => ProbeResultBody {
            present: Some(true),
            ..Default::default()
        },
        Err(e) => ProbeResultBody {
            present: Some(false),
            error: Some(e),
            ..Default::default()
        },
    }
}

/// Execute one typed probe. Unknown kinds return a typed error — the closed
/// catalog is enforced here.
async fn execute_probe(kind: &str, args: &Value) -> ProbeResultBody {
    match kind {
        "path_activity" => probe_path_activity(args).await,
        "git_branch_state" => probe_git_branch_state(args),
        "session_process_alive" => probe_session_process_alive(args).await,
        "artifact_present" => probe_artifact_present(args),
        other => ProbeResultBody::err(format!("unknown probe kind: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Pull-loop
// ---------------------------------------------------------------------------

/// One poll: GET this device's pending probes, execute each read-only, POST
/// the typed result. Returns `Err` only on a transport/parse failure (the tick
/// loop just logs + retries next interval).
async fn tick_once() -> Result<usize, String> {
    let device_id = match crate::agent_worktree::census::load_device_id_pub() {
        Some(d) => d,
        None => return Ok(0), // unpaired — nothing to do
    };
    let base = match crate::agent_worktree::census::coord_http_base_pub() {
        Some(b) => b,
        None => return Ok(0),
    };
    let base = base.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let pending_url = format!("{base}/coord/probes/pending/{device_id}");
    let resp = crate::coord_http::coord_get(&client, &pending_url)
        .send()
        .await
        .map_err(|e| format!("GET pending: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET pending → {}", resp.status()));
    }
    let pull: PendingProbesResponse = resp
        .json()
        .await
        .map_err(|e| format!("decode pending: {e}"))?;

    let mut done = 0usize;
    for probe in &pull.probes {
        let result = execute_probe(&probe.probe_kind, &probe.probe_args).await;
        let result_url = format!("{base}/coord/probes/{}/result", probe.id);
        match qontinui_runner_lib::auth::attach_device_auth(client.post(&result_url))
            .json(&result)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => done += 1,
            Ok(r) => tracing::warn!(
                probe_id = %probe.id,
                "probe_executor: POST result → {}",
                r.status()
            ),
            Err(e) => tracing::warn!(probe_id = %probe.id, "probe_executor: POST result: {e}"),
        }
    }
    Ok(done)
}

/// Spawn the probe executor pull-loop. No-op when `RUNNER_PROBES_ENABLED` is
/// unset/false (default). Idempotent to spawn (harmless if called twice).
pub fn spawn_probe_executor() {
    if !probes_enabled() {
        tracing::info!("probe_executor: disabled ({RUNNER_PROBES_FLAG} not set) — not spawning");
        return;
    }
    let period = interval();
    tracing::info!(
        "probe_executor: starting (interval={}s, read-only worktree-scoped probes)",
        period.as_secs()
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match tick_once().await {
                Ok(n) if n > 0 => {
                    tracing::info!("probe_executor: answered {n} probe(s) this tick")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("probe_executor: tick failed: {e}"),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests — the REQUIRED read-only + path-scoping guarantees
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn flag_defaults_off() {
        // With the env unset, the executor stays dark.
        std::env::remove_var(RUNNER_PROBES_FLAG);
        assert!(!probes_enabled());
    }

    #[test]
    fn git_command_allowlist_is_read_only() {
        // The catalog of git verbs a probe may run contains NO mutating verb —
        // a probe can never write to a worktree via git.
        const MUTATING: &[&str] = &[
            "commit",
            "push",
            "pull",
            "fetch",
            "merge",
            "rebase",
            "reset",
            "checkout",
            "clean",
            "add",
            "rm",
            "branch",
            "tag",
            "stash",
            "apply",
            "cherry-pick",
            "worktree",
            "gc",
        ];
        for verb in READ_ONLY_GIT {
            assert!(
                !MUTATING.contains(verb),
                "read allowlist leaked a mutator: {verb}"
            );
        }
        // And `git_read` refuses anything off the allowlist regardless of caller.
        let tmp = std::env::temp_dir();
        assert!(git_read(&tmp, &["push", "origin", "main"]).is_none());
        assert!(git_read(&tmp, &["reset", "--hard"]).is_none());
    }

    #[test]
    fn is_scoped_rejects_escape_and_traversal() {
        let root = PathBuf::from("/qontinui-root");
        // Contained.
        assert!(is_scoped(
            &PathBuf::from("/qontinui-root/qontinui-coord-wt-x"),
            &root
        ));
        assert!(is_scoped(&root, &root));
        // Sibling that merely shares a string prefix is NOT contained
        // (component-wise starts_with).
        assert!(!is_scoped(&PathBuf::from("/qontinui-root-evil/x"), &root));
        // Fully outside.
        assert!(!is_scoped(&PathBuf::from("/etc/passwd"), &root));
        assert!(!is_scoped(&PathBuf::from("/windows/system32"), &root));
        // Any traversal component is rejected outright.
        assert!(!is_scoped(
            &PathBuf::from("/qontinui-root/../etc/passwd"),
            &root
        ));
    }

    #[test]
    fn resolve_scoped_worktree_rejects_missing_and_unscoped() {
        // No worktree_path → typed error, no fs access.
        assert!(resolve_scoped_worktree(&serde_json::json!({})).is_err());
        // A path that exists but is (almost certainly) outside qontinui_root:
        // the system temp dir. Either qontinui_root is unresolvable (Err) or
        // the temp dir is not contained (Err) — never Ok.
        let tmp = std::env::temp_dir();
        let args = serde_json::json!({ "worktree_path": tmp.to_string_lossy() });
        assert!(
            resolve_scoped_worktree(&args).is_err(),
            "the system temp dir must never resolve as a scoped worktree"
        );
    }

    #[test]
    fn newest_mtime_is_read_only_and_finds_recent_writes() {
        // A read-only scan over a temp dir: it must observe a just-written file
        // and must NOT alter the directory (read-only guarantee).
        let dir = std::env::temp_dir().join(format!("probe-mtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("out.txt");
        std::fs::write(&f, b"built").unwrap();
        let before: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();

        let secs = newest_mtime_secs_ago(&dir, SystemTime::now());
        assert!(secs.is_some(), "a freshly written file has a mtime");
        assert!(secs.unwrap() < 3600, "recent write is fresh");

        // The scan created/removed nothing.
        let after: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(before, after, "the mtime scan must not mutate the worktree");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn unknown_probe_kind_is_a_typed_error_never_executes() {
        let r = execute_probe("rm_-rf_slash", &serde_json::json!({})).await;
        assert!(r.error.is_some());
        assert!(r.error.unwrap().contains("unknown probe kind"));
        // A typed-error result mutates nothing and carries no evidence.
        assert!(r.process_alive.is_none());
        assert!(r.present.is_none());
    }
}
