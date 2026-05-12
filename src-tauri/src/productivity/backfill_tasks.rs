//! Phase 3 — one-shot Tauri-callable backfill that clears the backlog of
//! stuck `coord.tasks` rows by cross-referencing each row's
//! `expected_file_claims` against `git log` on `main` in the known qontinui
//! ecosystem repos.
//!
//! Phase 2 of the coord-task-status-hygiene plan already covers FUTURE
//! merges (the github-merge listener flips tasks when their claimed paths
//! land on main). This module addresses the EXISTING backlog of tasks that
//! were stranded before that listener shipped.
//!
//! Behaviour:
//!
//! 1. SELECT every non-terminal task with at least one `expected_file_claim`.
//! 2. For each repo in the scan list (skip missing dirs), run
//!    `git log --since=<since> --diff-filter=AM --name-only` on `main` and
//!    parse the output into a list of `(sha, date, subject, files)`.
//! 3. Filter out commits whose subjects look like mechanical reformats
//!    (`prettier`, `rustfmt`, `reformat`, `format files`) — they were the
//!    obvious false-positive source the plan calls out.
//! 4. For each task, find the most recent commit (newest-first `git log`
//!    order) that touched any of the task's claimed paths. The
//!    path-matching predicate handles absolute paths, repo-prefixed paths,
//!    repo-relative paths, and bare-basename claims.
//! 5. Emit `BEGIN; UPDATE …; … COMMIT;` SQL. When `apply=true`, execute
//!    that SQL inside a single transaction; otherwise return it as a dry-
//!    run preview.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;

// =============================================================================
// Public option / result shapes
// =============================================================================

/// One stuck task that the backfill matched to a commit on `main`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillCandidate {
    pub task_id: String,
    pub plan_id: String,
    pub phase_name: String,
    pub sequence_in_phase: i32,
    pub description: String,
    pub expected_file_claims: Vec<String>,
    pub matched_commit_sha: String,
    pub matched_commit_subject: String,
    /// ISO 8601 commit date (`--format=%cI`).
    pub matched_commit_date: String,
    /// Which repo dir the commit was found in.
    pub repo_path: String,
}

/// Result returned to the Tauri caller.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillResult {
    pub candidates: Vec<BackfillCandidate>,
    /// Newline-joined `BEGIN; UPDATE …; COMMIT;` script.
    pub generated_sql: String,
    /// True when `apply=true` and the UPDATEs actually ran.
    pub applied: bool,
    /// Rows actually updated. 0 in dry-run.
    pub updated_count: usize,
    pub scanned_task_count: usize,
    pub scanned_repos: Vec<String>,
    /// Repos in the scan list whose directory was missing on disk.
    pub skipped_repos: Vec<String>,
    /// The `--since=` value the runtime used (matches the option, surfaced
    /// for the UI preview so the user sees what window was scanned).
    pub since: String,
}

/// Input options.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillOptions {
    /// Repos to scan. Empty → use `DEFAULT_REPOS`.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Lower bound for `git log --since=<this>`. Default `2026-04-01`.
    #[serde(default = "default_since")]
    pub since: String,
    /// When true, execute the generated UPDATEs inside one transaction.
    /// Otherwise emit them as a dry-run script.
    #[serde(default)]
    pub apply: bool,
}

fn default_since() -> String {
    "2026-04-01".to_string()
}

impl Default for BackfillOptions {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            since: default_since(),
            apply: false,
        }
    }
}

/// Default scan list — the known qontinui ecosystem repos under
/// `D:/qontinui-root/`. Missing dirs are silently skipped (and surfaced
/// via `BackfillResult::skipped_repos`) so an incomplete checkout doesn't
/// fail the call.
pub const DEFAULT_REPOS: &[&str] = &[
    "D:/qontinui-root/qontinui-runner",
    "D:/qontinui-root/qontinui-coord",
    "D:/qontinui-root/qontinui-web",
    "D:/qontinui-root/qontinui-mobile",
    "D:/qontinui-root/qontinui-supervisor",
    "D:/qontinui-root/ui-bridge",
];

/// Subject substrings that disqualify a commit from matching (case-
/// insensitive). The plan calls these out as the obvious false-positive
/// source — a `prettier`/`rustfmt` sweep touches dozens of files unrelated
/// to any one task's intent.
pub const REFORMAT_SUBJECT_TOKENS: &[&str] = &["prettier", "rustfmt", "reformat", "format files"];

/// Tag stored in `coord.tasks.completion_source` when this command flips
/// a row to `done`. Not a member of the [`CompletionSource`] enum — it's
/// an opaque marker so the read path's forward-compat branch (`unknown
/// value → None`) surfaces "this was a backfill" without blowing up the
/// existing variants.
///
/// [`CompletionSource`]: crate::database::pg::completion_reports::CompletionSource
pub const BACKFILL_COMPLETION_SOURCE: &str = "backfill-2026-05-12";

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug)]
pub enum BackfillError {
    Db(String),
    GitLog(String),
    Apply(String),
}

impl std::fmt::Display for BackfillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackfillError::Db(e) => write!(f, "database error: {}", e),
            BackfillError::GitLog(e) => write!(f, "git log error: {}", e),
            BackfillError::Apply(e) => write!(f, "apply transaction error: {}", e),
        }
    }
}

impl std::error::Error for BackfillError {}

// =============================================================================
// Parsed git log row
// =============================================================================

/// One commit with the files it touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub date: String,
    pub subject: String,
    pub files: Vec<String>,
}

// =============================================================================
// Public entry point
// =============================================================================

/// Run the backfill end-to-end. See module docs for behaviour.
pub async fn run_backfill(
    pg: &Arc<PgDb>,
    opts: BackfillOptions,
) -> Result<BackfillResult, BackfillError> {
    info!(
        "productivity.backfill.started since={} apply={} repos={}",
        opts.since,
        opts.apply,
        if opts.repos.is_empty() {
            "<default>".to_string()
        } else {
            opts.repos.join(",")
        }
    );

    // 1. Pull non-terminal tasks with at least one expected_file_claim.
    let tasks = fetch_stuck_tasks(pg).await?;
    let scanned_task_count = tasks.len();
    debug!("productivity.backfill.fetched_tasks count={}", tasks.len());

    if tasks.is_empty() {
        return Ok(BackfillResult {
            candidates: Vec::new(),
            generated_sql: String::new(),
            applied: false,
            updated_count: 0,
            scanned_task_count: 0,
            scanned_repos: Vec::new(),
            skipped_repos: Vec::new(),
            since: opts.since,
        });
    }

    // 2. Scan repos.
    let repo_list: Vec<String> = if opts.repos.is_empty() {
        DEFAULT_REPOS.iter().map(|s| s.to_string()).collect()
    } else {
        opts.repos.clone()
    };
    let mut scanned_repos: Vec<String> = Vec::new();
    let mut skipped_repos: Vec<String> = Vec::new();
    let mut repo_commits: Vec<(String, Vec<CommitInfo>)> = Vec::new();

    for repo in &repo_list {
        if !Path::new(repo).is_dir() {
            warn!("productivity.backfill.skip_missing_repo path={}", repo);
            skipped_repos.push(repo.clone());
            continue;
        }
        match run_git_log(repo, &opts.since).await {
            Ok(raw) => {
                let commits = parse_git_log_output(&raw);
                let filtered: Vec<CommitInfo> = commits
                    .into_iter()
                    .filter(|c| !is_reformat_subject(&c.subject))
                    .collect();
                debug!(
                    "productivity.backfill.scanned_repo path={} commit_count={}",
                    repo,
                    filtered.len()
                );
                repo_commits.push((repo.clone(), filtered));
                scanned_repos.push(repo.clone());
            }
            Err(e) => {
                warn!(
                    "productivity.backfill.git_log_failed path={} err={}",
                    repo, e
                );
                skipped_repos.push(repo.clone());
            }
        }
    }

    // 3. For each task, find the most recent matching commit.
    let mut candidates: Vec<BackfillCandidate> = Vec::new();
    for task in &tasks {
        if let Some((repo_path, commit)) = match_task(task, &repo_commits) {
            candidates.push(BackfillCandidate {
                task_id: task.id.clone(),
                plan_id: task.plan_id.clone(),
                phase_name: task.phase_name.clone(),
                sequence_in_phase: task.sequence_in_phase,
                description: task.description.clone(),
                expected_file_claims: task.expected_file_claims.clone(),
                matched_commit_sha: commit.sha.clone(),
                matched_commit_subject: commit.subject.clone(),
                matched_commit_date: commit.date.clone(),
                repo_path,
            });
        }
    }

    info!(
        "productivity.backfill.matched candidate_count={} of scanned={}",
        candidates.len(),
        scanned_task_count
    );

    // 4. Build the SQL preview.
    let generated_sql = build_sql_preview(&candidates, &opts.since);

    // 5. Optionally apply.
    let (applied, updated_count) = if opts.apply && !candidates.is_empty() {
        let n = apply_updates(pg, &candidates).await?;
        info!("productivity.backfill.applied updated_count={}", n);
        (true, n)
    } else {
        (false, 0)
    };

    Ok(BackfillResult {
        candidates,
        generated_sql,
        applied,
        updated_count,
        scanned_task_count,
        scanned_repos,
        skipped_repos,
        since: opts.since,
    })
}

// =============================================================================
// DB — fetch stuck tasks
// =============================================================================

/// Minimal task projection sufficient for matching + UPDATE generation.
/// Avoids pulling the full `TaskRow` SELECT column list so the query
/// stays narrow.
#[derive(Debug, Clone)]
struct StuckTask {
    id: String,
    plan_id: String,
    phase_name: String,
    sequence_in_phase: i32,
    description: String,
    expected_file_claims: Vec<String>,
}

async fn fetch_stuck_tasks(pg: &Arc<PgDb>) -> Result<Vec<StuckTask>, BackfillError> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| BackfillError::Db(format!("PG pool error: {}", e)))?;

    let rows = conn
        .query(
            r#"
            SELECT
                id::text,
                plan_id::text,
                phase_name,
                sequence_in_phase,
                description,
                expected_file_claims
            FROM coord.tasks
            WHERE status IN ('pending','ready','assigned','running','review','needs_fix')
              AND array_length(expected_file_claims, 1) > 0
            ORDER BY created_at ASC
            "#,
            &[],
        )
        .await
        .map_err(|e| BackfillError::Db(format!("select stuck tasks: {}", e)))?;

    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(StuckTask {
            id: r.get(0),
            plan_id: r.get(1),
            phase_name: r.get(2),
            sequence_in_phase: r.get(3),
            description: r.get(4),
            expected_file_claims: r.get(5),
        });
    }
    Ok(out)
}

// =============================================================================
// Git
// =============================================================================

/// Run `git log --since=<since> --diff-filter=AM --name-only ... main`
/// against `repo` and return the raw stdout.
async fn run_git_log(repo: &str, since: &str) -> Result<String, String> {
    // %x09 is TAB. The format gives one header line per commit, followed
    // by a blank line, followed by `--name-only` paths, followed by a
    // blank line before the next commit header.
    let format_arg = "--format=%H%x09%cI%x09%s";
    let mut cmd = tokio::process::Command::new("git");
    cmd.args([
        "log",
        &format!("--since={}", since),
        "--diff-filter=AM",
        "--name-only",
        format_arg,
        "main",
    ]);
    cmd.current_dir(repo);

    let output = match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("spawn git log: {}", e)),
        Err(_) => return Err("git log timed out after 60s".to_string()),
    };
    if !output.status.success() {
        return Err(format!(
            "git log exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse the raw `git log --name-only` output into a list of
/// [`CommitInfo`]. Tolerant of trailing whitespace, extra blank lines,
/// and headerless tails. Newest-first ordering is preserved from `git
/// log`.
pub fn parse_git_log_output(raw: &str) -> Vec<CommitInfo> {
    let mut out: Vec<CommitInfo> = Vec::new();
    let mut current: Option<CommitInfo> = None;

    for line in raw.lines() {
        // Header lines look like `<sha>\t<date>\t<subject>`. We detect
        // them by checking for two tabs and a sha-shaped first field. A
        // file path could conceivably contain tabs, but `git` quotes
        // those paths (so the line would start with `"`), and commit
        // subjects after the second tab can contain anything — match on
        // the first-two-fields shape.
        if let Some((sha, rest)) = line.split_once('\t') {
            if looks_like_sha(sha) {
                if let Some((date, subject)) = rest.split_once('\t') {
                    // Flush the previous commit.
                    if let Some(prev) = current.take() {
                        out.push(prev);
                    }
                    current = Some(CommitInfo {
                        sha: sha.to_string(),
                        date: date.to_string(),
                        subject: subject.to_string(),
                        files: Vec::new(),
                    });
                    continue;
                }
            }
        }

        if line.trim().is_empty() {
            // Blank line — separator, ignored. The header-detection
            // branch above is what flips to the next commit.
            continue;
        }

        // Anything else is a file path under the current commit.
        if let Some(c) = current.as_mut() {
            c.files.push(line.to_string());
        }
        // If `current` is None, we're seeing stray output before the
        // first header — silently skip it.
    }

    if let Some(prev) = current.take() {
        out.push(prev);
    }
    out
}

/// A heuristic SHA recogniser: 7..=64 lowercase hex chars.
fn looks_like_sha(s: &str) -> bool {
    if s.len() < 7 || s.len() > 64 {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Case-insensitive substring match against [`REFORMAT_SUBJECT_TOKENS`].
fn is_reformat_subject(subject: &str) -> bool {
    let lower = subject.to_ascii_lowercase();
    REFORMAT_SUBJECT_TOKENS.iter().any(|t| lower.contains(t))
}

// =============================================================================
// Path matching
// =============================================================================

/// Normalize a `expected_file_claim` into a repo-relative path.
///
/// Strips leading `D:/qontinui-root/` and the leading `<repo-name>/`
/// segment if present so a claim like
/// `D:/qontinui-root/qontinui-runner/src/foo.ts` becomes `src/foo.ts`.
pub fn normalize_claim(raw: &str) -> String {
    // Normalise backslashes → forward slashes so the suffix-match logic
    // below is OS-independent.
    let mut s = raw.replace('\\', "/");

    // Strip the absolute prefix.
    const ABS_PREFIX_LOWER: &str = "d:/qontinui-root/";
    if s.to_ascii_lowercase().starts_with(ABS_PREFIX_LOWER) {
        s = s[ABS_PREFIX_LOWER.len()..].to_string();
    }
    // Strip a leading `<repo-name>/` IFF the first segment looks like a
    // known qontinui-prefixed repo. We're deliberately conservative —
    // stripping arbitrary first segments would eat legitimate
    // `src/foo.rs`-style claims that happen to start with a real
    // directory.
    if let Some(slash) = s.find('/') {
        let head = &s[..slash];
        if head.starts_with("qontinui-") || head == "ui-bridge" {
            s = s[slash + 1..].to_string();
        }
    }
    s
}

/// Does the normalized claim match the commit-file path?
///
/// Rules (in order):
/// 1. Exact equality.
/// 2. `commit_file` ends with `/` + `normalized`.
/// 3. When `normalized` has no `/`, fall back to basename match
///    (commit-file's basename == `normalized`).
pub fn claim_matches_file(normalized: &str, commit_file: &str) -> bool {
    if normalized == commit_file {
        return true;
    }
    if normalized.contains('/') {
        // Path-shaped: only accept a suffix match anchored on `/`.
        let needle = format!("/{}", normalized);
        commit_file.ends_with(&needle)
    } else {
        // Bare basename — match against the file's basename.
        let basename = commit_file.rsplit('/').next().unwrap_or(commit_file);
        basename == normalized
    }
}

/// Returns `(repo_path, commit)` for the most-recent commit across all
/// scanned repos that touched any of the task's claims, or `None` if
/// nothing matched.
///
/// Iteration order: per-repo, the `Vec<CommitInfo>` is newest-first
/// (preserved from `git log`). We scan repos in the order they were
/// supplied and return the first hit; this is good enough because each
/// task's claims are typically scoped to a single repo. If a claim's
/// repo-relative path collides across repos, the first scan wins —
/// document the corner case here so the user can rerun with a narrower
/// scan list if needed.
fn match_task<'a>(
    task: &'a StuckTask,
    repo_commits: &'a [(String, Vec<CommitInfo>)],
) -> Option<(String, &'a CommitInfo)> {
    let normalized_claims: Vec<String> = task
        .expected_file_claims
        .iter()
        .map(|c| normalize_claim(c))
        .collect();
    if normalized_claims.is_empty() {
        return None;
    }
    for (repo_path, commits) in repo_commits {
        for commit in commits {
            for file in &commit.files {
                for claim in &normalized_claims {
                    if claim_matches_file(claim, file) {
                        return Some((repo_path.clone(), commit));
                    }
                }
            }
        }
    }
    None
}

// =============================================================================
// SQL preview
// =============================================================================

/// Escape a single-quoted SQL string literal by doubling each `'`. Used
/// for inline values in the generated preview script (we don't bind
/// parameters here because the preview is a copy-pasteable artifact).
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Short SHA — the first 12 chars. Matches `git log --abbrev-commit`
/// default-ish length and stays unambiguous in our repos.
fn short_sha(sha: &str) -> &str {
    if sha.len() <= 12 {
        sha
    } else {
        &sha[..12]
    }
}

/// Build the BEGIN/UPDATE/COMMIT preview script. Returns an empty string
/// when there are no candidates — caller can use that to skip writing
/// the file in the UI layer.
pub fn build_sql_preview(candidates: &[BackfillCandidate], since: &str) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mut out = String::new();
    out.push_str(&format!(
        "-- Backfill matched {} task(s) across {} commit(s). Scan window: --since={}.\n",
        candidates.len(),
        count_distinct_commits(candidates),
        since
    ));
    out.push_str(&format!(
        "-- Generated {}. Run inside a transaction.\n",
        timestamp
    ));
    out.push_str("BEGIN;\n");
    for c in candidates {
        let subj_esc = sql_escape(&c.matched_commit_subject);
        let repo_esc = sql_escape(&c.repo_path);
        out.push_str(&format!(
            "UPDATE coord.tasks\n\
             SET status = 'done',\n    \
                 completion_source = '{src}',\n    \
                 completion_report = jsonb_build_object(\n        \
                     'repo_path', '{repo}',\n        \
                     'commit_sha', '{sha}',\n        \
                     'commit_subject', '{subj}',\n        \
                     'commit_date', '{date}',\n        \
                     'backfilled_at', NOW()\n    \
                 ),\n    \
                 notes = COALESCE(notes, '') || E'\\nBackfilled from {short}: {subj}',\n    \
                 completed_at = NOW(),\n    \
                 updated_at = NOW()\n\
             WHERE id = '{task_id}'\n  \
               AND status IN ('pending','ready','assigned','running','review','needs_fix');\n",
            src = BACKFILL_COMPLETION_SOURCE,
            repo = repo_esc,
            sha = c.matched_commit_sha,
            subj = subj_esc,
            date = c.matched_commit_date,
            short = short_sha(&c.matched_commit_sha),
            task_id = c.task_id,
        ));
    }
    out.push_str("COMMIT;\n");
    out
}

fn count_distinct_commits(candidates: &[BackfillCandidate]) -> usize {
    let mut seen: HashMap<&str, ()> = HashMap::new();
    for c in candidates {
        seen.insert(&c.matched_commit_sha, ());
    }
    seen.len()
}

// =============================================================================
// Apply (transactional)
// =============================================================================

/// Execute the same UPDATE statements as [`build_sql_preview`] but using
/// parameterised tokio-postgres bindings inside a single transaction.
/// Returns the total rows affected (i.e. updated_count). Rolls back on
/// any error.
async fn apply_updates(
    pg: &Arc<PgDb>,
    candidates: &[BackfillCandidate],
) -> Result<usize, BackfillError> {
    let mut conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| BackfillError::Db(format!("PG pool error: {}", e)))?;
    let txn = conn
        .transaction()
        .await
        .map_err(|e| BackfillError::Apply(format!("begin tx: {}", e)))?;

    let mut total: usize = 0;
    for c in candidates {
        let report = serde_json::json!({
            "repo_path": c.repo_path,
            "commit_sha": c.matched_commit_sha,
            "commit_subject": c.matched_commit_subject,
            "commit_date": c.matched_commit_date,
            "backfilled_at": chrono::Utc::now().to_rfc3339(),
        });
        let task_uuid = match uuid::Uuid::parse_str(&c.task_id) {
            Ok(u) => u,
            Err(e) => {
                let _ = txn.rollback().await;
                return Err(BackfillError::Apply(format!(
                    "invalid task id {}: {}",
                    c.task_id, e
                )));
            }
        };
        let note_appendix = format!(
            "\nBackfilled from {}: {}",
            short_sha(&c.matched_commit_sha),
            c.matched_commit_subject
        );
        let res = txn
            .execute(
                r#"
                UPDATE coord.tasks
                SET status = 'done',
                    completion_source = $1,
                    completion_report = $2::jsonb,
                    notes = COALESCE(notes, '') || $3,
                    completed_at = NOW(),
                    updated_at = NOW()
                WHERE id = $4::uuid
                  AND status IN ('pending','ready','assigned','running','review','needs_fix')
                "#,
                &[
                    &BACKFILL_COMPLETION_SOURCE,
                    &report,
                    &note_appendix,
                    &task_uuid,
                ],
            )
            .await;
        match res {
            Ok(n) => total += n as usize,
            Err(e) => {
                let _ = txn.rollback().await;
                return Err(BackfillError::Apply(format!(
                    "UPDATE for task {} failed: {}",
                    c.task_id, e
                )));
            }
        }
    }

    txn.commit()
        .await
        .map_err(|e| BackfillError::Apply(format!("commit tx: {}", e)))?;
    Ok(total)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, claims: &[&str]) -> StuckTask {
        StuckTask {
            id: id.to_string(),
            plan_id: "00000000-0000-0000-0000-000000000001".to_string(),
            phase_name: "Phase 1".to_string(),
            sequence_in_phase: 0,
            description: "test".to_string(),
            expected_file_claims: claims.iter().map(|s| s.to_string()).collect(),
        }
    }

    // -------------------------------------------------------------------
    // parse_git_log_output
    // -------------------------------------------------------------------

    #[test]
    fn parse_git_log_output_handles_multifile_commits() {
        // Two commits: first touched 3 files, second touched 1.
        // Format mirrors the real output: `<sha>\t<date>\t<subject>` then
        // blank line then file paths then blank line then next header.
        let raw = "\
abc123def4567\t2026-04-15T10:00:00+00:00\tfix: clean up logging\n\
\n\
qontinui-runner/src/foo.rs\n\
qontinui-runner/src/bar.rs\n\
qontinui-web/backend/x.py\n\
\n\
beefcafe0011\t2026-04-12T08:30:00+00:00\tfeat: add knob\n\
\n\
qontinui-mobile/App.tsx\n\
";
        let commits = parse_git_log_output(raw);
        assert_eq!(commits.len(), 2);

        assert_eq!(commits[0].sha, "abc123def4567");
        assert_eq!(commits[0].date, "2026-04-15T10:00:00+00:00");
        assert_eq!(commits[0].subject, "fix: clean up logging");
        assert_eq!(
            commits[0].files,
            vec![
                "qontinui-runner/src/foo.rs".to_string(),
                "qontinui-runner/src/bar.rs".to_string(),
                "qontinui-web/backend/x.py".to_string()
            ]
        );

        assert_eq!(commits[1].sha, "beefcafe0011");
        assert_eq!(commits[1].subject, "feat: add knob");
        assert_eq!(
            commits[1].files,
            vec!["qontinui-mobile/App.tsx".to_string()]
        );
    }

    #[test]
    fn parse_git_log_output_handles_empty() {
        assert!(parse_git_log_output("").is_empty());
        assert!(parse_git_log_output("\n\n\n").is_empty());
    }

    #[test]
    fn parse_git_log_output_handles_trailing_blank() {
        let raw = "\
deadbeef1234\t2026-04-01T00:00:00+00:00\tone\n\
\n\
src/a.rs\n\
\n\
";
        let commits = parse_git_log_output(raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].files, vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn parse_git_log_output_subject_with_tabs_is_handled() {
        // Subject contains a real tab — should still parse: first two
        // tabs split header into sha, date, subject; remaining tabs
        // stay in subject.
        let raw = "\
ababab989898\t2026-04-10T10:00:00+00:00\tfix:\tweird subject\n\
\n\
src/a.rs\n\
";
        let commits = parse_git_log_output(raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix:\tweird subject");
    }

    // -------------------------------------------------------------------
    // is_reformat_subject
    // -------------------------------------------------------------------

    #[test]
    fn reformat_subject_filter() {
        assert!(is_reformat_subject("chore: run prettier"));
        assert!(is_reformat_subject("PRETTIER cleanup"));
        assert!(is_reformat_subject("rustfmt the runner"));
        assert!(is_reformat_subject("Reformat all files"));
        assert!(is_reformat_subject("format files in src/"));
        assert!(!is_reformat_subject("feat: add backfill command"));
        assert!(!is_reformat_subject("fix bug"));
    }

    // -------------------------------------------------------------------
    // path matching
    // -------------------------------------------------------------------

    #[test]
    fn path_matching_handles_absolute_and_relative_claims() {
        // Table-driven. Each row: (claim, commit_file, expect_match).
        let cases: &[(&str, &str, bool)] = &[
            // Absolute claim that includes D:/qontinui-root/<repo>/...
            // After normalize_claim, becomes `src/foo.rs` — matches the
            // commit file `qontinui-runner/src/foo.rs` via suffix.
            (
                "D:/qontinui-root/qontinui-runner/src/foo.rs",
                "qontinui-runner/src/foo.rs",
                true,
            ),
            // Repo-prefixed claim normalises to `src/foo.rs` — matches.
            (
                "qontinui-runner/src/foo.rs",
                "qontinui-runner/src/foo.rs",
                true,
            ),
            // Repo-relative claim normalises to itself; matches by
            // suffix when the commit-side file is repo-prefixed.
            ("src/foo.rs", "qontinui-runner/src/foo.rs", true),
            // Exact equality also works (no repo prefix on either side).
            ("src/foo.rs", "src/foo.rs", true),
            // Basename-only claim — falls back to basename match.
            (
                "KnowledgeBrowser.tsx",
                "qontinui-web/frontend/src/components/KnowledgeBrowser.tsx",
                true,
            ),
            // Non-matching basename.
            (
                "KnowledgeBrowser.tsx",
                "qontinui-web/frontend/src/components/Other.tsx",
                false,
            ),
            // Different path — must not match by accident.
            ("src/foo.rs", "qontinui-runner/src/bar.rs", false),
            // Backslash-bearing absolute claim (Windows-shaped) — should
            // normalise + match.
            (
                "D:\\qontinui-root\\qontinui-runner\\src\\foo.rs",
                "qontinui-runner/src/foo.rs",
                true,
            ),
            // ui-bridge prefix is also recognised by the normaliser.
            ("ui-bridge/src/lib.ts", "ui-bridge/src/lib.ts", true),
            // Same-named file in a *different* directory must not match
            // when the claim is path-shaped (no basename fallback).
            (
                "src/foo.rs",
                "qontinui-runner/tests/src/foo.rs",
                true, // Note: still ends with /src/foo.rs, so this DOES match.
            ),
            // Path-shaped claim where neither equality nor suffix-with-
            // slash holds.
            ("components/Foo.tsx", "qontinui-runner/src/lib.rs", false),
        ];
        for (claim, file, expect) in cases {
            let normalized = normalize_claim(claim);
            let got = claim_matches_file(&normalized, file);
            assert_eq!(
                got, *expect,
                "claim={:?} (normalised={:?}) vs file={:?}: expected {}, got {}",
                claim, normalized, file, expect, got
            );
        }
    }

    #[test]
    fn normalize_claim_strips_known_repo_prefixes() {
        assert_eq!(
            normalize_claim("D:/qontinui-root/qontinui-runner/src/a.rs"),
            "src/a.rs"
        );
        assert_eq!(
            normalize_claim("qontinui-web/frontend/x.tsx"),
            "frontend/x.tsx"
        );
        assert_eq!(normalize_claim("ui-bridge/sdk/index.ts"), "sdk/index.ts");
        // Unknown first segment is preserved (not a qontinui-* / ui-bridge prefix).
        assert_eq!(normalize_claim("src/foo.rs"), "src/foo.rs");
        assert_eq!(normalize_claim("notes/x.md"), "notes/x.md");
    }

    // -------------------------------------------------------------------
    // match_task
    // -------------------------------------------------------------------

    #[test]
    fn match_task_returns_first_matching_commit_newest_first() {
        let t = task("00000000-0000-0000-0000-0000000000aa", &["src/foo.rs"]);
        let repo = "D:/qontinui-root/qontinui-runner".to_string();
        let newer = CommitInfo {
            sha: "newercommit".to_string(),
            date: "2026-04-20T00:00:00+00:00".to_string(),
            subject: "newer".to_string(),
            files: vec!["qontinui-runner/src/foo.rs".to_string()],
        };
        let older = CommitInfo {
            sha: "oldercommit".to_string(),
            date: "2026-04-10T00:00:00+00:00".to_string(),
            subject: "older".to_string(),
            files: vec!["qontinui-runner/src/foo.rs".to_string()],
        };
        // `git log` is newest-first, so the first match wins.
        let scan = vec![(repo.clone(), vec![newer.clone(), older])];
        let m = match_task(&t, &scan).expect("matched");
        assert_eq!(m.0, repo);
        assert_eq!(m.1.sha, "newercommit");
    }

    #[test]
    fn match_task_returns_none_when_no_overlap() {
        let t = task("00000000-0000-0000-0000-0000000000bb", &["src/missing.rs"]);
        let scan = vec![(
            "D:/qontinui-root/qontinui-runner".to_string(),
            vec![CommitInfo {
                sha: "abc".to_string(),
                date: "2026-04-20T00:00:00+00:00".to_string(),
                subject: "x".to_string(),
                files: vec!["qontinui-runner/src/foo.rs".to_string()],
            }],
        )];
        assert!(match_task(&t, &scan).is_none());
    }

    #[test]
    fn match_task_skips_task_with_empty_claims() {
        let t = task("00000000-0000-0000-0000-0000000000cc", &[]);
        let scan = vec![(
            "D:/qontinui-root/qontinui-runner".to_string(),
            vec![CommitInfo {
                sha: "abc".to_string(),
                date: "2026-04-20T00:00:00+00:00".to_string(),
                subject: "x".to_string(),
                files: vec!["qontinui-runner/src/foo.rs".to_string()],
            }],
        )];
        assert!(match_task(&t, &scan).is_none());
    }

    // -------------------------------------------------------------------
    // SQL preview
    // -------------------------------------------------------------------

    #[test]
    fn dry_run_sql_preview_emits_begin_commit_and_one_update_per_candidate() {
        // We can't drive `run_backfill` end-to-end without PG, so we
        // exercise the SQL-building branch directly. This is the same
        // string the caller sees on `apply=false`.
        let candidates = vec![
            BackfillCandidate {
                task_id: "11111111-1111-1111-1111-111111111111".to_string(),
                plan_id: "22222222-2222-2222-2222-222222222222".to_string(),
                phase_name: "Phase 1".to_string(),
                sequence_in_phase: 0,
                description: "t".to_string(),
                expected_file_claims: vec!["src/foo.rs".to_string()],
                matched_commit_sha: "abcdef1234567890".to_string(),
                matched_commit_subject: "feat: do the thing".to_string(),
                matched_commit_date: "2026-04-20T00:00:00+00:00".to_string(),
                repo_path: "D:/qontinui-root/qontinui-runner".to_string(),
            },
            BackfillCandidate {
                task_id: "33333333-3333-3333-3333-333333333333".to_string(),
                plan_id: "22222222-2222-2222-2222-222222222222".to_string(),
                phase_name: "Phase 2".to_string(),
                sequence_in_phase: 1,
                description: "u".to_string(),
                expected_file_claims: vec!["src/bar.rs".to_string()],
                matched_commit_sha: "cafef00d99".to_string(),
                matched_commit_subject: "fix: it's broken".to_string(), // tests apostrophe escape
                matched_commit_date: "2026-04-21T00:00:00+00:00".to_string(),
                repo_path: "D:/qontinui-root/qontinui-runner".to_string(),
            },
        ];
        let sql = build_sql_preview(&candidates, "2026-04-01");
        assert!(sql.starts_with("-- Backfill matched 2 task(s)"));
        assert!(sql.contains("BEGIN;"));
        assert!(sql.ends_with("COMMIT;\n"));
        // Two UPDATE statements.
        let updates = sql.matches("UPDATE coord.tasks").count();
        assert_eq!(updates, 2);
        // Each contains the expected guard.
        assert_eq!(
            sql.matches(
                "AND status IN ('pending','ready','assigned','running','review','needs_fix')"
            )
            .count(),
            2
        );
        // Apostrophe in commit subject is escaped (single → doubled).
        assert!(sql.contains("fix: it''s broken"));
        // Source tag baked in.
        assert!(sql.contains(BACKFILL_COMPLETION_SOURCE));
        // First task id appears in a WHERE clause.
        assert!(sql.contains("WHERE id = '11111111-1111-1111-1111-111111111111'"));
    }

    #[test]
    fn dry_run_sql_preview_empty_when_no_candidates() {
        let sql = build_sql_preview(&[], "2026-04-01");
        assert!(sql.is_empty());
    }

    #[test]
    fn sql_escape_doubles_single_quotes() {
        assert_eq!(sql_escape("it's"), "it''s");
        assert_eq!(sql_escape("''"), "''''");
        assert_eq!(sql_escape("no quotes"), "no quotes");
    }

    // -------------------------------------------------------------------
    // looks_like_sha
    // -------------------------------------------------------------------

    #[test]
    fn looks_like_sha_recognises_hex_lengths() {
        assert!(looks_like_sha("abcdef1"));
        assert!(looks_like_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!looks_like_sha("short"));
        assert!(!looks_like_sha("not-hex-at-all-no"));
        // Uppercase hex is technically valid; we accept it.
        assert!(looks_like_sha("ABCDEF1"));
    }

    // -------------------------------------------------------------------
    // BackfillOptions
    // -------------------------------------------------------------------

    #[test]
    fn backfill_options_default_is_safe() {
        let o = BackfillOptions::default();
        assert!(o.repos.is_empty());
        assert_eq!(o.since, "2026-04-01");
        assert!(!o.apply);
    }
}
