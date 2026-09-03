//! `ProjectSnapshot` — the joined dashboard view.
//!
//! Everything on the Projects grid and detail page is a projection of one
//! struct, computed here in a single call so the page never fans out.
//!
//! # The join key is the project root path
//!
//! Six sources all carry a filesystem path, and none of them agree on how to
//! spell it. [`crate::commands::saved_projects::normalize_path`] canonicalizes
//! (trailing separator, slash direction, case, MSYS `/d/…`, `subst` alias) and
//! [`crate::commands::saved_projects::is_under`] supplies the prefix boundary.
//!
//! | Source | Path field |
//! |---|---|
//! | `ProcessConfig` (settings) | `cwd` |
//! | Terminal manager | `TerminalInfo.working_dir` |
//! | AI sessions | `project.session_touched_files.file_path` |
//! | `deferred_questions` | (via its `task_run_id`'s touched files) |
//! | `phase_token_usage` | (via its `task_run_id`'s touched files) |
//! | git | the root itself |
//!
//! # Three things the Phase-0 probe forced (plan §6.1)
//!
//! 1. The table is **`project.session_touched_files`**, not `public.`, and
//!    `recorded_at` is **on the row** — no `task_runs` join is needed for
//!    recency, only for a session's name and status.
//! 2. **Backslash is Postgres's `LIKE` escape character**, so
//!    `file_path LIKE 'D:\qontinui-root\%'` silently mis-matches and produces
//!    an always-empty dashboard. Every prefix test here uses
//!    `starts_with(lower(file_path), $n)`.
//! 3. **`coord.agent_worktrees` is empty**, so worktree-resident sessions
//!    (22% of all sessions) cannot be mapped through it. Both live worktree
//!    path shapes encode the repo name in the path, so
//!    [`map_worktree_path`] parses it out instead.
//!
//! # Attribution is longest-prefix-wins
//!
//! `D:\work` and `D:\work\pizzeria` can both be saved projects; a file under
//! the latter belongs to the latter. The winner is resolved once in Rust over
//! the whole project list ([`attribute`]), never per-query.

use std::path::Path;
use std::sync::Arc;

use qontinui_types::process_management::{ProcessState, ProcessStatus};
use qontinui_types::projects::{
    GitCommitLite, GitLite, HealthLevel, HealthLite, PendingQuestion, ProcessStatusLite,
    ProjectSnapshot, SavedProject, SessionLite, SessionSource,
};
use tracing::{debug, warn};

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

use crate::commands::saved_projects::{is_under, normalize_path};
use crate::database::pg::PgDb;
use crate::terminal::types::TerminalInfo;

/// How far back the session join looks. Bounds the scan without truncating
/// anything the dashboard shows — "worked on N days ago" stops being useful
/// long before this.
const SESSION_WINDOW_DAYS: i64 = 90;

/// How many recent sessions the detail view renders.
const RECENT_SESSION_LIMIT: usize = 10;

/// How many commits `GitLite::last_commits` carries.
const GIT_LOG_LIMIT: usize = 5;

/// Rolling window for `spend_7d_usd`.
const SPEND_WINDOW_DAYS: i64 = 7;

/// Hard cap on rows pulled back for Rust-side attribution.
///
/// Aggregation cannot run in SQL: the uuid-scoped worktree shape puts the repo
/// name *after* the uuid, so it is not prefix-filterable, and longest-prefix
/// attribution needs the whole project list. Rows come back newest-first, so
/// hitting this cap drops the oldest — the least interesting end for a
/// "what happened recently" view. The measured whole-workspace population is
/// ~1.7k rows, so this is headroom, not a working limit.
const ROW_SCAN_LIMIT: usize = 20_000;

// ============================================================================
// Path attribution
// ============================================================================

/// Agent scratch directories. ~30% of `session_touched_files` rows live here
/// and belong to no project — the agent's own memory and temp files.
const NOISE_PREFIXES: [&str; 1] = ["c:\\claude\\"];
const NOISE_FRAGMENTS: [&str; 1] = ["\\appdata\\local\\temp\\claude\\"];

/// Directory name the agent-worktree allocator uses for uuid-scoped trees.
const WORKTREE_DIR: &str = "qontinui-worktrees";

/// Marker that separates a repo name from a slug in the sibling worktree
/// shape: `<repo>-wt-<slug>`.
const WORKTREE_SUFFIX_MARKER: &str = "-wt-";

/// True when a **normalized** path is agent scratch rather than project work.
pub(crate) fn is_noise_path(normalized: &str) -> bool {
    NOISE_PREFIXES.iter().any(|p| normalized.starts_with(p))
        || NOISE_FRAGMENTS.iter().any(|f| normalized.contains(f))
}

/// Render a compile-time constant as a Postgres string literal.
///
/// Only used for the `NOISE_*` constants above — never for user input, which
/// always travels as a bound parameter. Backslashes need no escaping because
/// `standard_conforming_strings` has defaulted to `on` since PostgreSQL 9.1,
/// which is what makes a Windows path safe to inline at all; doubling single
/// quotes is the one escape that still applies, and it is here so a future
/// edit to those constants cannot introduce a syntax error.
fn sql_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Rewrite a **normalized** path that lives inside an agent worktree into the
/// equivalent path inside the canonical checkout, so worktree-resident
/// sessions attribute to the project they are actually working on.
///
/// Two shapes exist in live data, and both encode the repo name in the path
/// itself (which is why this does not depend on `coord.agent_worktrees` —
/// that table is empty):
///
/// - `<ws>\qontinui-worktrees\<uuid>\<repo>\<rest>` → `<ws>\<repo>\<rest>`
/// - `<ws>\<repo>-wt-<slug>\<rest>`                → `<ws>\<repo>\<rest>`
///
/// Returns `None` when the path is not worktree-resident.
pub(crate) fn map_worktree_path(normalized: &str) -> Option<String> {
    let sep = std::path::MAIN_SEPARATOR;
    let segments: Vec<&str> = normalized.split(sep).collect();

    // Shape 1: `…\qontinui-worktrees\<uuid>\<repo>\…`
    if let Some(idx) = segments.iter().position(|s| *s == WORKTREE_DIR) {
        // Need at least `<uuid>` and `<repo>` after the marker.
        if segments.len() > idx + 2 {
            let mut rebuilt: Vec<&str> = segments[..idx].to_vec();
            rebuilt.extend_from_slice(&segments[idx + 2..]);
            return Some(rebuilt.join(&sep.to_string()));
        }
        return None;
    }

    // Shape 2: `…\<repo>-wt-<slug>\…`
    for (idx, segment) in segments.iter().enumerate() {
        let Some(marker_at) = segment.find(WORKTREE_SUFFIX_MARKER) else {
            continue;
        };
        // A segment that *starts* with the marker has no repo name in front
        // of it — not this shape.
        if marker_at == 0 {
            continue;
        }
        let repo = &segment[..marker_at];
        let mut rebuilt: Vec<String> = segments[..idx].iter().map(|s| s.to_string()).collect();
        rebuilt.push(repo.to_string());
        rebuilt.extend(segments[idx + 1..].iter().map(|s| s.to_string()));
        return Some(rebuilt.join(&sep.to_string()));
    }

    None
}

/// A saved project reduced to what attribution needs: its id and its
/// normalized root.
#[derive(Debug, Clone)]
pub(crate) struct ProjectRoot {
    pub id: String,
    pub root: String,
}

/// Build the attribution table, **sorted longest-root-first** so the first
/// [`is_under`] hit is the longest prefix.
pub(crate) fn build_roots(projects: &[SavedProject]) -> Vec<ProjectRoot> {
    let mut roots: Vec<ProjectRoot> = projects
        .iter()
        .filter(|p| !p.path.trim().is_empty())
        .map(|p| ProjectRoot {
            id: p.id.clone(),
            root: normalize_path(&p.path),
        })
        .filter(|r| !r.root.is_empty())
        .collect();
    roots.sort_by(|a, b| b.root.len().cmp(&a.root.len()));
    roots
}

/// Attribute one **raw** path to a project id, longest-prefix wins.
///
/// Tries the path as given, then — if that matched nothing — its
/// worktree-mapped equivalent, so a session working in
/// `…\qontinui-runner-wt-foo\` lands on the `…\qontinui-runner` project.
pub(crate) fn attribute(roots: &[ProjectRoot], raw_path: &str) -> Option<String> {
    let normalized = normalize_path(raw_path);
    if normalized.is_empty() || is_noise_path(&normalized) {
        return None;
    }
    if let Some(hit) = roots.iter().find(|r| is_under(&r.root, &normalized)) {
        return Some(hit.id.clone());
    }
    let mapped = map_worktree_path(&normalized)?;
    roots
        .iter()
        .find(|r| is_under(&r.root, &mapped))
        .map(|r| r.id.clone())
}

// ============================================================================
// SQL prefix spellings
// ============================================================================

/// Every lowercase spelling of `normalized_root` that could appear in a
/// stored `file_path`, each already terminated with a separator so it is a
/// true directory prefix.
///
/// Stored paths are raw — the dispatcher writes whatever the agent's tool
/// passed — so the read side must ask for all of them. The Phase-0 probe
/// found four spellings of one workspace root in live data:
/// `D:\…`, `D:/…`, `/d/…` (MSYS) and the subst target `C:\Users\…\Documents\…`.
///
/// Normalization is not reproducible in SQL (it resolves the machine's live
/// `subst` table), so this enumerates the inverse instead.
fn prefix_spellings(normalized_root: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // The normalized (physical, subst-resolved, lowercased) form.
    let physical = normalized_root.trim_end_matches(['\\', '/']).to_string();
    if physical.is_empty() {
        return out;
    }

    fn push(out: &mut Vec<String>, value: String) {
        if !out.contains(&value) {
            out.push(value);
        }
    }

    let mut bases: Vec<String> = vec![physical.clone()];

    // Every alias that maps ONTO the physical form gives another spelling.
    for (alias, target) in crate::commands::saved_projects::subst_alias_pairs() {
        let target_norm = target.trim_end_matches(['\\', '/']).replace('/', "\\");
        let target_norm = target_norm.to_lowercase();
        let alias_norm = alias.to_lowercase();
        if physical == target_norm {
            bases.push(alias_norm);
        } else if let Some(rest) = physical.strip_prefix(&target_norm) {
            if rest.starts_with('\\') {
                bases.push(format!("{alias_norm}{rest}"));
            }
        }
    }

    for base in bases {
        // Backslash form: `d:\qontinui-root\qontinui-runner\`
        push(&mut out, format!("{base}\\"));
        // Forward-slash form: `d:/qontinui-root/qontinui-runner/`
        push(&mut out, format!("{}/", base.replace('\\', "/")));
        // MSYS form: `/d/qontinui-root/qontinui-runner/`
        let bytes = base.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let drive = bytes[0] as char;
            let rest = base[2..].replace('\\', "/");
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() {
                push(&mut out, format!("/{drive}/"));
            } else {
                push(&mut out, format!("/{drive}/{rest}/"));
            }
        }
    }

    out
}

/// Prefixes to hand the SQL coarse filter for one project.
///
/// Three families, all of which Rust re-checks exactly afterwards:
/// 1. the project root itself,
/// 2. the sibling-worktree shape `<ws>\<repo>-wt-` (a genuine prefix, so it
///    catches every slug),
/// 3. the uuid-scoped shape `<ws>\qontinui-worktrees\` (over-broad — the repo
///    name sits *after* the uuid, so it cannot be prefix-filtered; the
///    `strpos` narrowing in the query and [`attribute`] in Rust do the rest).
fn project_scan_prefixes(normalized_root: &str) -> Vec<String> {
    let mut prefixes = prefix_spellings(normalized_root);

    let root_path = Path::new(normalized_root);
    let repo = root_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let workspace = root_path
        .parent()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    if !repo.is_empty() && !workspace.is_empty() {
        // `<ws>\<repo>-wt-` — deliberately NOT separator-terminated: it is a
        // prefix of every `<repo>-wt-<slug>` sibling.
        let wt_sibling = format!("{workspace}\\{repo}{WORKTREE_SUFFIX_MARKER}");
        for spelling in prefix_spellings(&wt_sibling) {
            // `prefix_spellings` appended a separator; drop it so the marker
            // stays open-ended.
            let trimmed = spelling.trim_end_matches(['\\', '/']).to_string();
            if !prefixes.contains(&trimmed) {
                prefixes.push(trimmed);
            }
        }

        let wt_dir = format!("{workspace}\\{WORKTREE_DIR}");
        for spelling in prefix_spellings(&wt_dir) {
            if !prefixes.contains(&spelling) {
                prefixes.push(spelling);
            }
        }
    }

    prefixes
}

// ============================================================================
// Session join
// ============================================================================

/// One `(task_run_id, file_path, recorded_at_ms)` row from the coarse SQL
/// filter, before Rust attribution.
struct TouchedRow {
    task_run_id: String,
    file_path: String,
    recorded_at_ms: i64,
}

/// Run the coarse prefix filter against `project.session_touched_files`.
///
/// The `WHERE` is an OR-chain of `starts_with(lower(file_path), $n)` — never
/// `LIKE`, because a Windows path's backslashes are `LIKE` escape characters
/// and would silently mis-match (plan §6.1 finding 2). Agent-scratch buckets
/// are excluded with `strpos`, which has no escape semantics either.
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
async fn fetch_touched_rows(
    pg: &Arc<PgDb>,
    prefixes: &[String],
) -> Result<Vec<TouchedRow>, String> {
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }

    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {e}"))?;

    let prefix_clause = (1..=prefixes.len())
        .map(|i| format!("starts_with(lower(stf.file_path), ${i})"))
        .collect::<Vec<_>>()
        .join(" OR ");

    // The noise filter is applied HERE as well as in `is_noise_path` — not
    // redundantly. Scratch rows are ~30% of the table, so excluding them
    // before `LIMIT` is what stops the scan window from being eaten by paths
    // that belong to no project. It is derived from the same constants rather
    // than re-typed, so the two sides cannot drift apart.
    let noise_clause = NOISE_PREFIXES
        .iter()
        .map(|p| format!("NOT starts_with(lower(stf.file_path), {})", sql_literal(p)))
        .chain(
            NOISE_FRAGMENTS
                .iter()
                .map(|f| format!("strpos(lower(stf.file_path), {}) = 0", sql_literal(f))),
        )
        .collect::<Vec<_>>()
        .join("\n             AND ");

    let window_param = prefixes.len() + 1;
    let sql = format!(
        r#"SELECT stf.task_run_id,
                  stf.file_path,
                  (extract(epoch from stf.recorded_at) * 1000)::bigint AS recorded_at_ms
           FROM project.session_touched_files stf
           WHERE ({prefix_clause})
             AND stf.recorded_at >= NOW() - make_interval(days => ${window_param}::int)
             AND {noise_clause}
           ORDER BY stf.recorded_at DESC
           LIMIT {ROW_SCAN_LIMIT}"#
    );

    let window_days = SESSION_WINDOW_DAYS as i32;
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        Vec::with_capacity(prefixes.len() + 1);
    for prefix in prefixes {
        params.push(prefix);
    }
    params.push(&window_days);

    let rows = conn
        .query(sql.as_str(), &params)
        .await
        .map_err(|e| format!("PG session_touched_files prefix scan: {e}"))?;

    Ok(rows
        .iter()
        .map(|r| TouchedRow {
            task_run_id: r.get(0),
            file_path: r.get(1),
            recorded_at_ms: r.get(2),
        })
        .collect())
}

/// Per-session aggregate after Rust attribution.
struct SessionAgg {
    task_run_id: String,
    last_touch_ms: i64,
    files_touched: i64,
}

/// Group attributed rows by session: newest touch, distinct files touched.
fn aggregate_sessions(
    rows: Vec<TouchedRow>,
    roots: &[ProjectRoot],
    project_id: &str,
) -> Vec<SessionAgg> {
    use std::collections::{HashMap, HashSet};

    let mut per_session: HashMap<String, (i64, HashSet<String>)> = HashMap::new();
    for row in rows {
        if attribute(roots, &row.file_path).as_deref() != Some(project_id) {
            continue;
        }
        let recorded_at_ms = row.recorded_at_ms;
        let normalized_file = normalize_path(&row.file_path);
        let entry = per_session
            .entry(row.task_run_id)
            .or_insert_with(|| (recorded_at_ms, HashSet::new()));
        entry.0 = entry.0.max(recorded_at_ms);
        entry.1.insert(normalized_file);
    }

    let mut aggs: Vec<SessionAgg> = per_session
        .into_iter()
        .map(|(task_run_id, (last_touch_ms, files))| SessionAgg {
            task_run_id,
            last_touch_ms,
            files_touched: files.len() as i64,
        })
        .collect();
    aggs.sort_by(|a, b| b.last_touch_ms.cmp(&a.last_touch_ms));
    aggs
}

/// Second query of the pair: names and statuses for the attributed sessions.
/// `recorded_at` already answered recency, so this is the *only* reason
/// `task_runs` is joined at all.
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
async fn fetch_session_meta(
    pg: &Arc<PgDb>,
    task_run_ids: &[String],
) -> Result<std::collections::HashMap<String, (Option<String>, Option<String>)>, String> {
    use std::collections::HashMap;

    if task_run_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {e}"))?;
    let rows = conn
        .query(
            "SELECT id, task_name, status FROM task_runs WHERE id = ANY($1)",
            &[&task_run_ids],
        )
        .await
        .map_err(|e| format!("PG task_runs meta: {e}"))?;

    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<_, String>(0),
                (r.get::<_, Option<String>>(1), r.get::<_, Option<String>>(2)),
            )
        })
        .collect())
}

/// Sum `phase_token_usage.cost_cents` over the attributed sessions in the
/// spend window. `None` when there is no cost data at all — distinct from
/// `Some(0.0)`, which means "measured, and it was free".
#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
async fn fetch_spend_usd(pg: &Arc<PgDb>, task_run_ids: &[String]) -> Result<Option<f64>, String> {
    if task_run_ids.is_empty() {
        return Ok(None);
    }
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {e}"))?;
    let window_days = SPEND_WINDOW_DAYS as i32;
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&task_run_ids, &window_days];
    let row = conn
        .query_one(
            r#"SELECT COALESCE(SUM(cost_cents), 0)::float8 / 100.0, COUNT(*)::bigint
               FROM phase_token_usage
               WHERE task_run_id = ANY($1)
                 AND created_at >= NOW() - make_interval(days => $2::int)"#,
            &params,
        )
        .await
        .map_err(|e| format!("PG phase_token_usage spend: {e}"))?;

    let total: f64 = row.get(0);
    let observations: i64 = row.get(1);
    Ok(if observations == 0 { None } else { Some(total) })
}

// ============================================================================
// Git
// ============================================================================

/// Budget for one local git probe in this module.
///
/// Four of these run sequentially in [`collect_git`], which is driven by the
/// `project_snapshot` Tauri command that the project page re-invokes freely.
/// A healthy call is milliseconds; the hang class is a contended `index.lock`.
const SNAPSHOT_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run a git command in `dir`, returning trimmed stdout on success.
fn git_capture(dir: &str, args: &[&str]) -> Option<String> {
    match git_capture_outcome(dir, args) {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => {
            Some(String::from_utf8_lossy(&stdout).trim().to_string())
        }
        crate::process_helpers::ProbeOutcome::Degraded(_) => None,
    }
}

/// [`git_capture`] without the two-state collapse — for the one caller whose
/// answer would otherwise become a confident number it did not measure.
fn git_capture_outcome(dir: &str, args: &[&str]) -> crate::process_helpers::ProbeOutcome {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["-C", dir]).args(args);
    crate::process_helpers::run_probe(cmd, SNAPSHOT_GIT_TIMEOUT, "projects::snapshot: git")
}

/// Branch, dirty count and recent commits for the project root. `None` when
/// the root is not a git working tree (or git is unavailable).
fn collect_git(project_path: &str) -> Option<GitLite> {
    // Cheapest possible "is this a work tree" probe; everything else is
    // pointless if it fails.
    if git_capture(project_path, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true")
    {
        return None;
    }

    let branch =
        git_capture(project_path, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty());

    // `dirty_count` is a plain `u32` in the shared wire type
    // (`qontinui_types::projects::GitLite`, which lives in the qontinui-schemas
    // repo) — there is no "unknown" to put in it. `.unwrap_or(0)` therefore
    // rendered a killed, over-budget `git status` as a confident
    // "0 uncommitted changes" on the project page, which is the one number a
    // reader would act on before switching branches.
    //
    // Widening the field would mean a cross-repo schema change plus a frontend
    // change; the proportionate fix is to degrade the way this function ALREADY
    // degrades when the `--is-inside-work-tree` probe fails: report no git
    // panel at all. `git status` exiting non-zero inside a work tree is
    // vanishingly rare, so in practice this arm is the timeout arm.
    let dirty_count = match git_capture_outcome(project_path, &["status", "--porcelain"]) {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => String::from_utf8_lossy(&stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count() as u32,
        crate::process_helpers::ProbeOutcome::Degraded(reason) => {
            warn!(
                "project_snapshot: `git status` in {project_path} degraded ({reason:?}) \
                 — reporting NO git state rather than a fabricated dirty_count of 0"
            );
            return None;
        }
    };

    let log_arg = format!("-{GIT_LOG_LIMIT}");
    let last_commits = git_capture(
        project_path,
        &["log", log_arg.as_str(), "--format=%h%x1f%ct%x1f%s"],
    )
    .map(|raw| {
        raw.lines()
            .filter_map(|line| {
                let mut parts = line.split('\u{1f}');
                let sha = parts.next()?.to_string();
                let committed_secs: i64 = parts.next()?.parse().ok()?;
                let subject = parts.next().unwrap_or_default().to_string();
                Some(GitCommitLite {
                    sha,
                    subject,
                    committed_ms: committed_secs.saturating_mul(1000),
                })
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();

    Some(GitLite {
        branch,
        dirty_count,
        last_commits,
    })
}

// ============================================================================
// Health
// ============================================================================

/// One green/amber/red verdict, in words a non-developer can read.
pub(crate) fn compute_health(processes: &[ProcessStatusLite]) -> HealthLite {
    if processes.is_empty() {
        return HealthLite {
            level: HealthLevel::Unknown,
            reason: "No services are set up for this project yet.".to_string(),
            failing_processes: Vec::new(),
        };
    }

    let failed: Vec<String> = processes
        .iter()
        .filter(|p| p.state == ProcessState::Failed)
        .map(|p| p.name.clone())
        .collect();
    if !failed.is_empty() {
        return HealthLite {
            reason: format!("{} stopped unexpectedly.", join_names(&failed)),
            level: HealthLevel::Red,
            failing_processes: failed,
        };
    }

    let starting: Vec<String> = processes
        .iter()
        .filter(|p| {
            matches!(
                p.state,
                ProcessState::Starting | ProcessState::Building | ProcessState::Stopping
            )
        })
        .map(|p| p.name.clone())
        .collect();
    if !starting.is_empty() {
        return HealthLite {
            reason: format!("{} is still starting up.", join_names(&starting)),
            level: HealthLevel::Amber,
            failing_processes: starting,
        };
    }

    let unresponsive: Vec<String> = processes
        .iter()
        .filter(|p| {
            matches!(p.state, ProcessState::Running | ProcessState::Healthy)
                && p.port_healthy == Some(false)
        })
        .map(|p| p.name.clone())
        .collect();
    if !unresponsive.is_empty() {
        return HealthLite {
            reason: format!(
                "{} is running but not answering.",
                join_names(&unresponsive)
            ),
            level: HealthLevel::Amber,
            failing_processes: unresponsive,
        };
    }

    let running = processes.iter().any(|p| {
        matches!(
            p.state,
            ProcessState::Running | ProcessState::Healthy | ProcessState::ExternallyOwned
        )
    });
    if running {
        HealthLite {
            level: HealthLevel::Green,
            reason: "Everything is running fine.".to_string(),
            failing_processes: Vec::new(),
        }
    } else {
        HealthLite {
            level: HealthLevel::Unknown,
            reason: "Nothing is running right now.".to_string(),
            failing_processes: Vec::new(),
        }
    }
}

/// "Website", "Website and API", "Website, API and Worker".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

// ============================================================================
// Entry point
// ============================================================================

/// Build the joined snapshot for the project with `project_id`.
///
/// `projects` is the full registry (needed for longest-prefix attribution —
/// a nested project must not have its sessions swallowed by its parent),
/// `terminals` is the live terminal list, and `process_statuses` is the
/// process manager's current view (passed in rather than fetched here so this
/// function stays free of Tauri state and stays unit-testable).
///
/// Every remote source degrades independently: no Postgres means empty
/// session/question/spend sections, not an error. A missing *project*, by
/// contrast, is a genuine caller bug and does error.
pub async fn build_snapshot(
    projects: &[SavedProject],
    project_id: &str,
    terminals: &[TerminalInfo],
    process_statuses: &[ProcessStatus],
) -> Result<ProjectSnapshot, String> {
    let project = projects
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .ok_or_else(|| format!("No saved project with id {project_id}"))?;

    let roots = build_roots(projects);
    let normalized_root = normalize_path(&project.path);

    // ---- processes -------------------------------------------------------
    let processes = collect_processes(&project, &roots, process_statuses);

    // ---- live terminal sessions -----------------------------------------
    let live_sessions: Vec<SessionLite> = terminals
        .iter()
        .filter(|t| t.is_alive)
        .filter(|t| attribute(&roots, &t.working_dir).as_deref() == Some(project_id))
        .map(|t| SessionLite {
            id: t.id.clone(),
            name: Some(t.title.clone()),
            status: None,
            source: SessionSource::Terminal,
            last_activity_ms: Some(t.created_at as i64),
            files_touched: 0,
            working_dir: Some(t.working_dir.clone()),
        })
        .collect();

    // ---- AI sessions, questions, spend (all Postgres-backed) -------------
    let mut recent_sessions: Vec<SessionLite> = Vec::new();
    let mut questions: Vec<PendingQuestion> = Vec::new();
    let mut spend_7d_usd: Option<f64> = None;

    if let Some(pg) = PgDb::try_global() {
        let prefixes = project_scan_prefixes(&normalized_root);
        debug!(
            "project_snapshot {}: scanning session_touched_files with {} prefix spelling(s)",
            project_id,
            prefixes.len()
        );

        match fetch_touched_rows(&pg, &prefixes).await {
            Ok(rows) => {
                let aggs = aggregate_sessions(rows, &roots, project_id);
                let all_ids: Vec<String> = aggs.iter().map(|a| a.task_run_id.clone()).collect();

                let meta = fetch_session_meta(&pg, &all_ids).await.unwrap_or_else(|e| {
                    warn!("project_snapshot: task_runs meta lookup failed: {e}");
                    Default::default()
                });

                recent_sessions = aggs
                    .iter()
                    .take(RECENT_SESSION_LIMIT)
                    .map(|agg| {
                        let (name, status) =
                            meta.get(&agg.task_run_id).cloned().unwrap_or((None, None));
                        SessionLite {
                            id: agg.task_run_id.clone(),
                            name,
                            status,
                            source: SessionSource::TouchedFiles,
                            last_activity_ms: Some(agg.last_touch_ms),
                            files_touched: agg.files_touched,
                            working_dir: None,
                        }
                    })
                    .collect();

                questions = collect_questions(&pg, &all_ids).await;

                spend_7d_usd = fetch_spend_usd(&pg, &all_ids).await.unwrap_or_else(|e| {
                    warn!("project_snapshot: spend lookup failed: {e}");
                    None
                });
            }
            Err(e) => {
                // Degrade, don't fail: an unreachable coord schema must not
                // blank the whole page.
                warn!("project_snapshot: session join failed: {e}");
            }
        }
    }

    // ---- git -------------------------------------------------------------
    // Four sequential bounded git children (up to 4 x SNAPSHOT_GIT_TIMEOUT).
    // `build_snapshot` is an `async fn`, so running them inline parked a
    // REACTOR worker for the whole run — and the project page re-invokes this
    // freely, so a few open tabs on a repo with a contended `index.lock` is
    // enough to starve the worker threads. Blocking work belongs on the
    // blocking pool.
    let git_path = project.path.clone();
    let git = spawn_blocking_tracked(move || collect_git(&git_path))
        .await
        .unwrap_or_else(|e| {
            warn!("project_snapshot: git collection task failed: {e}");
            None
        });

    // ---- health + last activity -----------------------------------------
    let health = compute_health(&processes);

    let last_activity_ms = [
        recent_sessions
            .iter()
            .filter_map(|s| s.last_activity_ms)
            .max(),
        live_sessions
            .iter()
            .filter_map(|s| s.last_activity_ms)
            .max(),
        git.as_ref()
            .and_then(|g| g.last_commits.first().map(|c| c.committed_ms)),
    ]
    .into_iter()
    .flatten()
    .max();

    Ok(ProjectSnapshot {
        project,
        processes,
        live_sessions,
        recent_sessions,
        git,
        questions,
        health,
        spend_7d_usd,
        last_activity_ms,
    })
}

/// Managed processes whose `cwd` attributes to this project — either because
/// the project explicitly binds them by id, or because their working
/// directory falls under the root.
fn collect_processes(
    project: &SavedProject,
    roots: &[ProjectRoot],
    statuses: &[ProcessStatus],
) -> Vec<ProcessStatusLite> {
    let configs = crate::settings::load_settings().managed_processes;

    configs
        .into_iter()
        .filter(|cfg| {
            project.process_ids.contains(&cfg.id)
                || attribute(roots, &cfg.cwd).as_deref() == Some(project.id.as_str())
        })
        .map(|cfg| {
            let status = statuses.iter().find(|s| s.id == cfg.id);
            ProcessStatusLite {
                id: cfg.id.clone(),
                name: cfg.name.clone(),
                state: status.map(|s| s.state).unwrap_or(ProcessState::Stopped),
                health_port: cfg.health_port,
                port_healthy: status.and_then(|s| s.port_healthy),
                cwd: cfg.cwd.clone(),
            }
        })
        .collect()
}

/// Pending `deferred_questions` raised by sessions attributed to this
/// project.
async fn collect_questions(pg: &Arc<PgDb>, task_run_ids: &[String]) -> Vec<PendingQuestion> {
    if task_run_ids.is_empty() {
        return Vec::new();
    }
    let rows = match pg.get_all_pending_questions().await {
        Ok(rows) => rows,
        Err(e) => {
            warn!("project_snapshot: pending-question lookup failed: {e}");
            return Vec::new();
        }
    };

    rows.into_iter()
        .filter_map(|row| {
            let task_run_id = row.get("task_run_id")?.as_str()?.to_string();
            if !task_run_ids.contains(&task_run_id) {
                return None;
            }
            Some(PendingQuestion {
                id: row
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                task_run_id,
                question: row
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                risk_level: row
                    .get("risk_level")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                created_at_ms: row
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.timestamp_millis()),
            })
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, path: &str) -> SavedProject {
        SavedProject {
            id: id.to_string(),
            path: path.to_string(),
            name: id.to_string(),
            project_type: "node".to_string(),
            manifest: "package.json".to_string(),
            description: None,
            emoji: None,
            color: None,
            front_page_url: None,
            process_ids: Vec::new(),
            terminal_page_id: None,
            zone_profile: None,
            repo_slug: None,
            notes: None,
            pinned: false,
            last_opened_ms: None,
        }
    }

    /// Platform-appropriate absolute path from segments, so the same test
    /// asserts on Windows (`E:\a\b`) and Linux (`/a/b`).
    fn tp(segments: &[&str]) -> String {
        let sep = std::path::MAIN_SEPARATOR;
        #[cfg(windows)]
        let mut s = String::from("E:");
        #[cfg(not(windows))]
        let mut s = String::new();
        for segment in segments {
            s.push(sep);
            s.push_str(segment);
        }
        s
    }

    // ---- noise -----------------------------------------------------------

    #[test]
    #[cfg(windows)]
    fn noise_buckets_are_recognised() {
        assert!(is_noise_path(&normalize_path(
            "C:\\claude\\memory-canonical\\notes.md"
        )));
        assert!(is_noise_path(&normalize_path(
            "C:\\Users\\jspin\\AppData\\Local\\Temp\\claude\\scratch\\x.txt"
        )));
        assert!(!is_noise_path(&normalize_path(
            "E:\\work\\pizzeria\\src\\menu.tsx"
        )));
    }

    // ---- worktree mapping ------------------------------------------------

    #[test]
    fn uuid_scoped_worktree_maps_to_canonical_repo() {
        let wt = normalize_path(&tp(&[
            "qontinui-root",
            "qontinui-worktrees",
            "0f6c-uuid",
            "qontinui-runner",
            "src",
            "main.rs",
        ]));
        let expected = normalize_path(&tp(&["qontinui-root", "qontinui-runner", "src", "main.rs"]));
        assert_eq!(map_worktree_path(&wt).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn sibling_worktree_maps_to_canonical_repo() {
        let wt = normalize_path(&tp(&[
            "qontinui-root",
            "qontinui-runner-wt-projects-dash",
            "src",
            "main.rs",
        ]));
        let expected = normalize_path(&tp(&["qontinui-root", "qontinui-runner", "src", "main.rs"]));
        assert_eq!(map_worktree_path(&wt).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn non_worktree_path_maps_to_nothing() {
        let plain = normalize_path(&tp(&["qontinui-root", "qontinui-runner", "src", "main.rs"]));
        assert_eq!(map_worktree_path(&plain), None);
    }

    #[test]
    fn worktree_marker_at_segment_start_is_not_a_repo() {
        // `-wt-foo` has no repo name in front of the marker.
        let path = normalize_path(&tp(&["qontinui-root", "-wt-foo", "src", "main.rs"]));
        assert_eq!(map_worktree_path(&path), None);
    }

    // ---- attribution -----------------------------------------------------

    #[test]
    fn attribution_prefers_the_longest_matching_root() {
        let projects = vec![
            project("outer", &tp(&["work"])),
            project("inner", &tp(&["work", "pizzeria"])),
        ];
        let roots = build_roots(&projects);
        assert_eq!(
            attribute(&roots, &tp(&["work", "pizzeria", "src", "menu.tsx"])).as_deref(),
            Some("inner")
        );
        assert_eq!(
            attribute(&roots, &tp(&["work", "other", "x.rs"])).as_deref(),
            Some("outer")
        );
    }

    #[test]
    fn attribution_respects_the_prefix_boundary() {
        let projects = vec![project("foo", &tp(&["work", "foo"]))];
        let roots = build_roots(&projects);
        assert_eq!(
            attribute(&roots, &tp(&["work", "foobar", "x.rs"])),
            None,
            "`work/foo` must not swallow `work/foobar`"
        );
    }

    #[test]
    fn attribution_falls_back_to_the_worktree_mapping() {
        let projects = vec![project(
            "runner",
            &tp(&["qontinui-root", "qontinui-runner"]),
        )];
        let roots = build_roots(&projects);
        assert_eq!(
            attribute(
                &roots,
                &tp(&[
                    "qontinui-root",
                    "qontinui-runner-wt-projects-dash",
                    "src",
                    "main.rs"
                ])
            )
            .as_deref(),
            Some("runner")
        );
        assert_eq!(
            attribute(
                &roots,
                &tp(&[
                    "qontinui-root",
                    "qontinui-worktrees",
                    "abc-uuid",
                    "qontinui-runner",
                    "src",
                    "main.rs"
                ])
            )
            .as_deref(),
            Some("runner")
        );
    }

    #[test]
    fn attribution_does_not_leak_a_different_repos_worktree() {
        let projects = vec![project(
            "runner",
            &tp(&["qontinui-root", "qontinui-runner"]),
        )];
        let roots = build_roots(&projects);
        assert_eq!(
            attribute(
                &roots,
                &tp(&[
                    "qontinui-root",
                    "qontinui-worktrees",
                    "abc-uuid",
                    "qontinui-web",
                    "app.py"
                ])
            ),
            None
        );
    }

    #[test]
    #[cfg(windows)]
    fn attribution_ignores_agent_scratch() {
        let projects = vec![project("claude", "C:\\claude")];
        let roots = build_roots(&projects);
        assert_eq!(
            attribute(&roots, "C:\\claude\\memory-canonical\\a.md"),
            None,
            "scratch is excluded even when a project root would match it"
        );
    }

    #[test]
    fn attribution_skips_projects_with_an_empty_path() {
        let projects = vec![project("blank", "  ")];
        let roots = build_roots(&projects);
        assert!(roots.is_empty());
        assert_eq!(attribute(&roots, &tp(&["work", "x.rs"])), None);
    }

    // ---- SQL prefix spellings -------------------------------------------

    #[test]
    #[cfg(windows)]
    fn prefix_spellings_cover_backslash_forward_and_msys_forms() {
        let spellings = prefix_spellings("e:\\work\\pizzeria");
        assert!(spellings.contains(&"e:\\work\\pizzeria\\".to_string()));
        assert!(spellings.contains(&"e:/work/pizzeria/".to_string()));
        assert!(spellings.contains(&"/e/work/pizzeria/".to_string()));
    }

    #[test]
    #[cfg(windows)]
    fn prefix_spellings_are_separator_terminated() {
        // Without the trailing separator, `e:\work\foo` would also match
        // `e:\work\foobar\…` in SQL — the same boundary defect `is_under`
        // fixes on the Rust side.
        for spelling in prefix_spellings("e:\\work\\foo") {
            assert!(
                spelling.ends_with('\\') || spelling.ends_with('/'),
                "{spelling} is not separator-terminated"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn scan_prefixes_include_both_worktree_shapes() {
        let prefixes = project_scan_prefixes("e:\\qontinui-root\\qontinui-runner");
        assert!(
            prefixes
                .iter()
                .any(|p| p == "e:\\qontinui-root\\qontinui-runner-wt-"),
            "sibling-worktree prefix missing from {prefixes:?}"
        );
        assert!(
            prefixes
                .iter()
                .any(|p| p == "e:\\qontinui-root\\qontinui-worktrees\\"),
            "uuid-scoped worktree prefix missing from {prefixes:?}"
        );
    }

    // ---- session aggregation --------------------------------------------

    #[test]
    fn aggregate_counts_distinct_files_and_keeps_newest_touch() {
        let projects = vec![project("p", &tp(&["work", "pizzeria"]))];
        let roots = build_roots(&projects);
        let rows = vec![
            TouchedRow {
                task_run_id: "s1".to_string(),
                file_path: tp(&["work", "pizzeria", "a.tsx"]),
                recorded_at_ms: 1_000,
            },
            TouchedRow {
                task_run_id: "s1".to_string(),
                file_path: tp(&["work", "pizzeria", "a.tsx"]),
                recorded_at_ms: 3_000,
            },
            TouchedRow {
                task_run_id: "s1".to_string(),
                file_path: tp(&["work", "pizzeria", "b.tsx"]),
                recorded_at_ms: 2_000,
            },
            TouchedRow {
                task_run_id: "s2".to_string(),
                file_path: tp(&["work", "pizzeria", "c.tsx"]),
                recorded_at_ms: 500,
            },
            // Attributed elsewhere — must be dropped.
            TouchedRow {
                task_run_id: "s3".to_string(),
                file_path: tp(&["work", "other", "d.tsx"]),
                recorded_at_ms: 9_000,
            },
        ];

        let aggs = aggregate_sessions(rows, &roots, "p");
        assert_eq!(aggs.len(), 2, "s3 belongs to no project here");
        assert_eq!(aggs[0].task_run_id, "s1", "newest-first ordering");
        assert_eq!(aggs[0].last_touch_ms, 3_000);
        assert_eq!(aggs[0].files_touched, 2, "distinct files, not raw rows");
        assert_eq!(aggs[1].task_run_id, "s2");
    }

    // ---- health ----------------------------------------------------------

    fn proc(name: &str, state: ProcessState, port_healthy: Option<bool>) -> ProcessStatusLite {
        ProcessStatusLite {
            id: name.to_string(),
            name: name.to_string(),
            state,
            health_port: Some(3000),
            port_healthy,
            cwd: tp(&["work", "pizzeria"]),
        }
    }

    #[test]
    fn health_is_unknown_without_processes() {
        assert_eq!(compute_health(&[]).level, HealthLevel::Unknown);
    }

    #[test]
    fn health_is_red_when_a_process_failed() {
        let health = compute_health(&[
            proc("Website", ProcessState::Healthy, Some(true)),
            proc("API", ProcessState::Failed, None),
        ]);
        assert_eq!(health.level, HealthLevel::Red);
        assert_eq!(health.failing_processes, vec!["API".to_string()]);
    }

    #[test]
    fn health_is_amber_while_starting_and_when_a_port_is_dead() {
        assert_eq!(
            compute_health(&[proc("Website", ProcessState::Starting, None)]).level,
            HealthLevel::Amber
        );
        assert_eq!(
            compute_health(&[proc("Website", ProcessState::Running, Some(false))]).level,
            HealthLevel::Amber
        );
    }

    #[test]
    fn health_is_green_when_something_runs_and_answers() {
        let health = compute_health(&[
            proc("Website", ProcessState::Healthy, Some(true)),
            proc("Worker", ProcessState::Running, None),
        ]);
        assert_eq!(health.level, HealthLevel::Green);
        assert!(health.failing_processes.is_empty());
    }

    #[test]
    fn health_is_unknown_when_everything_is_stopped() {
        assert_eq!(
            compute_health(&[proc("Website", ProcessState::Stopped, None)]).level,
            HealthLevel::Unknown
        );
    }

    #[test]
    fn join_names_reads_as_a_sentence() {
        assert_eq!(join_names(&["A".to_string()]), "A");
        assert_eq!(join_names(&["A".to_string(), "B".to_string()]), "A and B");
        assert_eq!(
            join_names(&["A".to_string(), "B".to_string(), "C".to_string()]),
            "A, B and C"
        );
    }
}
