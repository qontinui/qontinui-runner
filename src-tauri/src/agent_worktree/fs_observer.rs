//! Runner-side **Ξ_FS observer producer** — the push hook that feeds coord's
//! `POST /coord/fs/observations` ingest.
//!
//! Plan reference: `plans/2026-05-31-edit-action-effect-signatures-plan.md`
//! §6.2 (deferred producer for Phases 1+2, which shipped as coord PR #252 +
//! web migration #386). Coord's ingest side is live on `coord` main
//! (`src/edit_effects.rs::post_fs_observations` /
//! `FsObservationsRequest` / `FsObservationItem`).
//!
//! ## What it does
//!
//! When an agent's per-repo worktree mutates tracked files, this module
//! diffs the worktree against its base commit (`parent_sha`), enumerates the
//! changed paths, computes the pre/post git **blob** shas per path, and POSTs
//! the batch to coord under one `correlation_id` so a multi-file logical edit
//! is one push (matching coord's `predict`/`verify` correlation model).
//!
//! ## Why diff-based capture (not per-write hooks)
//!
//! Diffing the worktree against `parent_sha` gives us, for free:
//!   - `.gitignore` filtering — `git status --porcelain` never lists ignored
//!     paths, so scratch/build/IDE-temp files are silently dropped.
//!   - natural batching — one logical edit (multiple files) becomes one push.
//!   - robust pre/post pairing — the pre-image is the blob at `parent_sha`,
//!     the post-image is the current working-tree content. No need to
//!     intercept every filesystem write.
//!
//! ## Lifecycle / integration point
//!
//! The producer is invoked from [`super::isolated_edit::IsolatedEditContext`]'s
//! `Drop` (the agent's edit session ending) — the same best-effort,
//! fire-and-forget pattern the claim-release uses. It models its lifecycle on
//! the sibling Ξ_Git watcher (`trigger_system/watchers/git_watcher.rs`), which
//! likewise uses `git2` to enrich a change set.
//!
//! ## Gating
//!
//! Behind env flag [`FS_OBSERVER_FLAG_ENV`] (`QONTINUI_FS_OBSERVER_ENABLED`),
//! **default off** — mirrors how `QONTINUI_AGENT_WORKTREE_MODE` gates the
//! worktree spawn path. Off ⇒ [`push_worktree_observations`] is a no-op.
//!
//! ## Degrade honestly
//!
//! Every failure path here logs at `debug`/`warn` and returns — coord being
//! unreachable, a malformed repo, a blob-sha computation error, a non-2xx
//! response: none of them ever crash or block the agent's edit. Coord writes
//! are best-effort by contract (coord returns `200 { accepted, persisted }`
//! even on a DB failure).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};

/// Env flag that turns the Ξ_FS observer producer on. Default off. Accepts
/// the usual truthy values; anything else (including unset) is false. Matches
/// the truthy-set used by [`super::worktree_mode_enabled`].
pub const FS_OBSERVER_FLAG_ENV: &str = "QONTINUI_FS_OBSERVER_ENABLED";

/// Per-runner allowlist override env var. A comma-separated list of path
/// prefixes (worktree-relative, `/`-normalized). When set + non-empty, ONLY
/// paths under one of these prefixes are reported. When unset, every tracked
/// changed path that survives the always-drop denylist is reported.
///
/// Example: `QONTINUI_FS_OBSERVER_ALLOWLIST=src/,crates/` reports only edits
/// under `src/` or `crates/`.
pub const FS_OBSERVER_ALLOWLIST_ENV: &str = "QONTINUI_FS_OBSERVER_ALLOWLIST";

/// Cap on observations per push so a giant generated-file churn can't produce
/// a multi-megabyte body. Excess paths are dropped (logged at debug). Coord's
/// ingest is per-row INSERTs so a huge batch would also be slow on its side.
const MAX_OBSERVATIONS_PER_PUSH: usize = 500;

/// Per-runner denylist of path *segments* / suffixes that are dropped even if
/// they slipped past `.gitignore` (e.g. a committed-then-locally-churned lock
/// file, or an IDE temp not in this repo's ignore). `.gitignore` already
/// handles the common cases via `git status`; this is defense-in-depth for
/// scratch/build/IDE-temp noise.
fn is_always_dropped(rel_path: &str) -> bool {
    let p = rel_path.replace('\\', "/");
    // Drop anything under a VCS / build / IDE scratch directory, and common
    // editor temp suffixes.
    const DROP_DIR_SEGMENTS: &[&str] = &[
        "/.git/",
        "/node_modules/",
        "/target/",
        "/dist/",
        "/.idea/",
        "/.vscode/",
        "/__pycache__/",
        "/.pytest_cache/",
        "/.worktrees/",
    ];
    let padded = format!("/{p}/");
    if DROP_DIR_SEGMENTS.iter().any(|seg| padded.contains(seg)) {
        return true;
    }
    const DROP_SUFFIXES: &[&str] = &[".tmp", ".swp", ".swo", ".orig", ".rej", "~", ".pyc", ".log"];
    if DROP_SUFFIXES.iter().any(|sfx| p.ends_with(sfx)) {
        return true;
    }
    // Drop OS junk.
    matches!(p.rsplit('/').next(), Some(".DS_Store") | Some("Thumbs.db"))
}

/// Parse the optional allowlist env var into a normalized prefix set. Empty
/// (unset or all-blank) means "no allowlist — report everything that survives
/// the denylist".
fn allowlist_prefixes() -> Vec<String> {
    std::env::var(FS_OBSERVER_ALLOWLIST_ENV)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().replace('\\', "/"))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Apply the allowlist + always-drop denylist to a worktree-relative path.
/// `allowlist` is the parsed prefix set (empty ⇒ allow-all-but-denylist).
fn path_is_reportable(rel_path: &str, allowlist: &[String]) -> bool {
    if is_always_dropped(rel_path) {
        return false;
    }
    if allowlist.is_empty() {
        return true;
    }
    let p = rel_path.replace('\\', "/");
    allowlist.iter().any(|prefix| p.starts_with(prefix))
}

/// Returns true iff the Ξ_FS observer producer is enabled. Default off.
pub fn fs_observer_enabled() -> bool {
    matches!(
        std::env::var(FS_OBSERVER_FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// One observed file-system edit — mirrors coord's `FsObservationItem`
/// (`qontinui-coord/src/edit_effects.rs`). `hunks` is an opaque jsonb
/// passthrough; v1 emits a minimal `[{old_start,old_lines,new_start,new_lines}]`
/// list derived from the libgit2 diff, or `[]` when unavailable.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FsObservation {
    pub path: String,
    /// Pre-image blob sha. `None` for a newly-created (untracked-at-base) file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_sha: Option<String>,
    pub post_sha: String,
    pub hunks: serde_json::Value,
}

/// Body of `POST /coord/fs/observations` — mirrors coord's
/// `FsObservationsRequest`. Field names are load-bearing (coord deserializes
/// exactly these keys).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FsObservationsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<uuid::Uuid>,
    pub observations: Vec<FsObservation>,
}

/// A minimal unified-diff hunk header — the cheap subset of coord's opaque
/// `hunks` jsonb. Derived from libgit2's per-hunk callback.
#[derive(Debug, Clone, Serialize, PartialEq)]
struct HunkHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

/// Compute the git blob OID (sha) for arbitrary content using
/// `git hash-object` semantics — i.e. `sha1("blob <len>\0" + content)`.
/// libgit2's `Repository::blob` (or the free `Odb::hash_object_data`) gives
/// the identical OID without writing to the object store. We use the hashing
/// path so it's a pure function of the bytes (testable, no repo needed).
pub fn blob_sha_for_bytes(content: &[u8]) -> Result<String, String> {
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, content)
        .map_err(|e| format!("hash_object: {e}"))?;
    Ok(oid.to_string())
}

/// Read a file and compute its blob sha. `Ok(None)` if the file doesn't exist
/// (deleted in the worktree) — the caller decides how to treat a deletion.
///
/// `pub(crate)` so the install-effects producer reuses this exact pre/post
/// blob-sha capture for its manifest/lockfile FS observations (Phase 3) instead
/// of copy-pasting the `git hash-object` semantics.
pub(crate) fn blob_sha_for_file(abs: &Path) -> Result<Option<String>, String> {
    match std::fs::read(abs) {
        Ok(bytes) => Ok(Some(blob_sha_for_bytes(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", abs.display())),
    }
}

/// Enumerate the worktree's edits against `parent_sha` and build the
/// per-path observation list.
///
/// Strategy:
///   1. Open the worktree as a git2 repo and resolve `parent_sha`'s tree
///      (the base the worktree branched from).
///   2. Diff that tree against the **working directory** (includes untracked,
///      respects `.gitignore`) to enumerate changed paths + per-path hunks.
///   3. For each path that survives filtering:
///        - `pre_sha`  = blob OID in the base tree (None if path absent there)
///        - `post_sha` = blob OID of the current working-tree content
///   4. Deletions (post content gone) are skipped — coord requires a
///      `post_sha`, and a "non-materialised edit" isn't an Ξ_FS observation.
///
/// Returns the (possibly empty) observation list. Any git2 error degrades to
/// `Ok(vec![])` for the affected step rather than failing the whole push —
/// callers treat an empty list as "nothing to report".
pub fn collect_observations(
    worktree_path: &Path,
    parent_sha: &str,
) -> Result<Vec<FsObservation>, String> {
    let repo = git2::Repository::open(worktree_path)
        .map_err(|e| format!("open worktree {}: {e}", worktree_path.display()))?;

    // Resolve the base tree from parent_sha.
    let base_oid = git2::Oid::from_str(parent_sha)
        .map_err(|e| format!("parse parent_sha {parent_sha:?}: {e}"))?;
    let base_commit = repo
        .find_commit(base_oid)
        .map_err(|e| format!("find base commit {parent_sha}: {e}"))?;
    let base_tree = base_commit
        .tree()
        .map_err(|e| format!("base tree for {parent_sha}: {e}"))?;

    // Diff base tree -> working directory. `include_untracked` surfaces
    // newly-created files; libgit2 honours `.gitignore` so ignored files
    // never appear. `recurse_untracked_dirs` makes whole new dirs enumerate.
    let mut diff_opts = git2::DiffOptions::new();
    diff_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let diff = repo
        .diff_tree_to_workdir_with_index(Some(&base_tree), Some(&mut diff_opts))
        .map_err(|e| format!("diff base..workdir: {e}"))?;

    // Collect per-path hunk headers in a first pass over the diff. The
    // `foreach` callback fires per-hunk; we key by the delta's new path.
    let mut hunks_by_path: std::collections::HashMap<String, Vec<HunkHeader>> =
        std::collections::HashMap::new();
    let _ = diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |delta, hunk| {
            if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                let key = p.to_string_lossy().replace('\\', "/");
                hunks_by_path.entry(key).or_default().push(HunkHeader {
                    old_start: hunk.old_start(),
                    old_lines: hunk.old_lines(),
                    new_start: hunk.new_start(),
                    new_lines: hunk.new_lines(),
                });
            }
            true
        }),
        None,
    );

    let allowlist = allowlist_prefixes();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<FsObservation> = Vec::new();

    for delta in diff.deltas() {
        // Prefer the new (post) path; fall back to old for pure deletions.
        let rel = match delta.new_file().path().or_else(|| delta.old_file().path()) {
            Some(p) => p.to_string_lossy().replace('\\', "/"),
            None => continue,
        };

        if !path_is_reportable(&rel, &allowlist) {
            debug!(path = %rel, "fs_observer: dropped by filter");
            continue;
        }
        if !seen.insert(rel.clone()) {
            continue; // dedupe (e.g. a rename surfaces twice)
        }

        // post_sha from the current working-tree content. A deletion has no
        // post content — skip it (coord requires post_sha; a removed file is
        // not an Ξ_FS materialised edit).
        let abs = worktree_path.join(&rel);
        let post_sha = match blob_sha_for_file(&abs) {
            Ok(Some(sha)) => sha,
            Ok(None) => {
                debug!(path = %rel, "fs_observer: skip (no post content / deletion)");
                continue;
            }
            Err(e) => {
                debug!(path = %rel, error = %e, "fs_observer: post_sha failed — skip path");
                continue;
            }
        };

        // pre_sha = blob OID of this path in the base tree. Absent ⇒ new file.
        let pre_sha = base_tree
            .get_path(Path::new(&rel))
            .ok()
            .map(|entry| entry.id().to_string());

        let hunks = hunks_by_path
            .get(&rel)
            .map(|h| serde_json::to_value(h).unwrap_or_else(|_| serde_json::json!([])))
            .unwrap_or_else(|| serde_json::json!([]));

        out.push(FsObservation {
            path: rel,
            pre_sha,
            post_sha,
            hunks,
        });

        if out.len() >= MAX_OBSERVATIONS_PER_PUSH {
            debug!(
                cap = MAX_OBSERVATIONS_PER_PUSH,
                "fs_observer: observation cap reached — truncating batch"
            );
            break;
        }
    }

    Ok(out)
}

/// Phase 5 (fs_observer backstop) — collect the uncommitted edits of an
/// arbitrary checkout against its OWN current `HEAD`.
///
/// Unlike [`collect_observations`], whose `parent_sha` is the base commit an
/// agent worktree branched from, this resolves the checkout's `HEAD` via
/// libgit2 (the same `repo.head()?.target()` pattern the verify side uses in
/// `edit_effect_loop::worktree_head_sha`) and delegates to
/// [`collect_observations`] so it reuses ALL of the gitignore / denylist /
/// blob-sha / hunk / cap logic — there is no second diff implementation.
///
/// The fs_backstop poller uses this to surface the working-tree drift of a
/// SHARED canonical checkout that is dirty AND not covered by any live
/// `kind=worktree` claim (an edit that leaked outside a session worktree).
///
/// Errors (not a git repo, unborn/detached HEAD we can't resolve, libgit2
/// failure) are returned as `Err` so the caller degrades honestly — it never
/// alarms on a checkout it couldn't read.
pub fn collect_uncommitted_observations(
    checkout_path: &Path,
) -> Result<Vec<FsObservation>, String> {
    let repo = git2::Repository::open(checkout_path)
        .map_err(|e| format!("open checkout {}: {e}", checkout_path.display()))?;
    let head = repo
        .head()
        .map_err(|e| format!("resolve HEAD of {}: {e}", checkout_path.display()))?;
    let head_sha = head
        .target()
        .ok_or_else(|| format!("HEAD of {} has no target oid", checkout_path.display()))?
        .to_string();
    collect_observations(checkout_path, &head_sha)
}

/// Derive a repo basename from a worktree path or coord repo slug. Coord
/// stores `repo` as an optional basename; we send the leaf dir name of the
/// canonical repo (e.g. `owner/qontinui-runner` ⇒ `qontinui-runner`).
fn repo_basename(repo_slug: &str) -> Option<String> {
    let leaf = repo_slug
        .replace('\\', "/")
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string());
    leaf.filter(|s| !s.is_empty())
}

/// POST a pre-built [`FsObservationsRequest`] to coord. Best-effort: any
/// transport error or non-2xx logs at `debug`/`warn` and returns `Ok(())` —
/// the producer NEVER surfaces a coord failure to the caller.
async fn post_observations(coord_http_base: &str, body: &FsObservationsRequest) {
    if body.observations.is_empty() {
        return;
    }
    let url = format!(
        "{}/coord/fs/observations",
        coord_http_base.trim_end_matches('/')
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("fs_observer: build http client failed: {e}");
            return;
        }
    };
    // Attach the device-JWT bearer when one is available (never fatal —
    // unpaired / empty keychain collapses to the anonymous send coord accepts
    // today). Same write-path attach the credential-helper uses, and it feeds
    // the data-plane auth-coverage metric ahead of multiuser Phase-2
    // enforcement on `/coord/fs/observations`. Must be `crate::auth`, not
    // `qontinui_runner_lib::auth` — this module compiles into the bin target,
    // and the lib path would bump the lib crate's separate counter statics,
    // invisible to the bin's `DATA_PLANE_TOTAL/AUTHED` coverage readout.
    let req = crate::auth::attach_device_auth(client.post(&url));
    match req.json(body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                info!(
                    url = %url,
                    n = body.observations.len(),
                    status = status.as_u16(),
                    "fs_observer: pushed observations"
                );
            } else {
                let txt = resp.text().await.unwrap_or_default();
                warn!(
                    url = %url,
                    status = status.as_u16(),
                    body = %txt,
                    "fs_observer: coord returned non-2xx (soft failure)"
                );
            }
        }
        Err(e) => {
            // Coord unreachable is the expected degraded case — debug, not warn.
            debug!(url = %url, error = %e, "fs_observer: push failed (coord unreachable)");
        }
    }
}

/// Top-level producer entry point: diff one materialized worktree against its
/// base and push the resulting observations to coord. Best-effort end to end.
///
/// No-op when the producer is disabled ([`fs_observer_enabled`] false). Called
/// from [`super::isolated_edit::IsolatedEditContext`]'s `Drop` — once per
/// per-repo worktree the agent edited.
///
/// `agent_id` / `correlation_id` are resolved from the session/worktree
/// context by the caller; `None` is fine (coord's fields are all optional).
pub async fn push_worktree_observations(
    coord_http_base: &str,
    worktree_path: &Path,
    parent_sha: &str,
    repo_slug: &str,
    agent_id: Option<String>,
    correlation_id: Option<uuid::Uuid>,
    tenant_id: Option<uuid::Uuid>,
) {
    if !fs_observer_enabled() {
        return;
    }

    let observations = match collect_observations(worktree_path, parent_sha) {
        Ok(o) => o,
        Err(e) => {
            // A repo we can't diff is a degraded case, not a crash.
            debug!(
                worktree = %worktree_path.display(),
                error = %e,
                "fs_observer: collect failed — nothing pushed"
            );
            return;
        }
    };

    if observations.is_empty() {
        debug!(
            worktree = %worktree_path.display(),
            "fs_observer: no reportable edits"
        );
        return;
    }

    let body = FsObservationsRequest {
        agent_id,
        correlation_id,
        repo: repo_basename(repo_slug),
        tenant_id,
        observations,
    };

    post_observations(coord_http_base, &body).await;
}

/// Convenience used by the `Drop` hook: spawn the best-effort push on the
/// current Tokio runtime. Captures owned values so the future is `'static`.
/// A failure to spawn (no runtime) is swallowed — the producer is optional.
#[allow(clippy::too_many_arguments)]
pub fn spawn_push_worktree_observations(
    coord_http_base: String,
    worktree_path: PathBuf,
    parent_sha: String,
    repo_slug: String,
    agent_id: Option<String>,
    correlation_id: Option<uuid::Uuid>,
    tenant_id: Option<uuid::Uuid>,
) {
    // Only spawn if there's a runtime; `Handle::try_current` avoids a panic in
    // non-async drop contexts (e.g. unit tests dropping the context).
    if tokio::runtime::Handle::try_current().is_err() {
        debug!("fs_observer: no tokio runtime in drop context — skipping push");
        return;
    }
    tokio::spawn(async move {
        push_worktree_observations(
            &coord_http_base,
            &worktree_path,
            &parent_sha,
            &repo_slug,
            agent_id,
            correlation_id,
            tenant_id,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    // ---- pure helpers -----------------------------------------------------

    #[test]
    fn blob_sha_matches_git_hash_object() {
        // `printf 'hello\n' | git hash-object --stdin` == this well-known OID.
        let sha = blob_sha_for_bytes(b"hello\n").expect("hash");
        assert_eq!(sha, "ce013625030ba8dba906f756967f9e9ca394464a");
        // Empty blob OID is also a git constant.
        let empty = blob_sha_for_bytes(b"").expect("hash empty");
        assert_eq!(empty, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn enabled_is_false_by_default() {
        // Holds as long as no concurrent test set the flag (env is global;
        // see memory feedback_env_var_tests_serialize). We don't mutate env
        // here to stay parallel-safe.
        if std::env::var(FS_OBSERVER_FLAG_ENV).is_err() {
            assert!(!fs_observer_enabled());
        }
    }

    #[test]
    fn denylist_drops_scratch_and_build() {
        assert!(is_always_dropped("target/debug/foo"));
        assert!(is_always_dropped("node_modules/x/index.js"));
        assert!(is_always_dropped("dist/bundle.js"));
        assert!(is_always_dropped(".idea/workspace.xml"));
        assert!(is_always_dropped("src/foo.rs.orig"));
        assert!(is_always_dropped("src/foo.swp"));
        assert!(is_always_dropped("notes.tmp"));
        assert!(is_always_dropped("a/b/.DS_Store"));
        // Real source survives.
        assert!(!is_always_dropped("src/agent_worktree/fs_observer.rs"));
        assert!(!is_always_dropped("Cargo.toml"));
    }

    #[test]
    fn allowlist_when_set_restricts_to_prefixes() {
        let allow = vec!["src/".to_string(), "crates/".to_string()];
        assert!(path_is_reportable("src/main.rs", &allow));
        assert!(path_is_reportable("crates/x/lib.rs", &allow));
        assert!(!path_is_reportable("docs/readme.md", &allow));
        // Denylist still wins inside an allowed prefix.
        assert!(!path_is_reportable("src/foo.rs.orig", &allow));
    }

    #[test]
    fn allowlist_empty_allows_all_but_denylist() {
        let allow: Vec<String> = vec![];
        assert!(path_is_reportable("docs/readme.md", &allow));
        assert!(path_is_reportable("anywhere/thing.rs", &allow));
        assert!(!path_is_reportable("target/x", &allow));
    }

    #[test]
    fn repo_basename_strips_owner() {
        assert_eq!(
            repo_basename("qontinui/qontinui-runner").as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(
            repo_basename("qontinui-runner").as_deref(),
            Some("qontinui-runner")
        );
        assert_eq!(repo_basename("").as_deref(), None);
    }

    #[test]
    fn request_serializes_to_coord_contract() {
        // The field names + null handling must match coord's
        // FsObservationsRequest / FsObservationItem exactly.
        let req = FsObservationsRequest {
            agent_id: Some("agent-1".into()),
            correlation_id: None,
            repo: Some("qontinui-runner".into()),
            tenant_id: None,
            observations: vec![
                FsObservation {
                    path: "src/a.rs".into(),
                    pre_sha: Some("abc".into()),
                    post_sha: "def".into(),
                    hunks: serde_json::json!([]),
                },
                FsObservation {
                    // new file: pre_sha omitted
                    path: "src/new.rs".into(),
                    pre_sha: None,
                    post_sha: "999".into(),
                    hunks: serde_json::json!([{"old_start":0,"old_lines":0,"new_start":1,"new_lines":3}]),
                },
            ],
        };
        let v = serde_json::to_value(&req).expect("serialize");
        assert_eq!(v["agent_id"], "agent-1");
        assert_eq!(v["repo"], "qontinui-runner");
        // Omitted optionals must be ABSENT (not null) — coord uses
        // #[serde(default)] so absent is fine; we skip to keep bodies lean.
        assert!(v.get("correlation_id").is_none());
        assert!(v.get("tenant_id").is_none());
        let obs = v["observations"].as_array().unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(obs[0]["path"], "src/a.rs");
        assert_eq!(obs[0]["pre_sha"], "abc");
        assert_eq!(obs[0]["post_sha"], "def");
        assert!(obs[0]["hunks"].is_array());
        // New file: pre_sha key absent.
        assert!(obs[1].get("pre_sha").is_none());
        assert_eq!(obs[1]["post_sha"], "999");
    }

    // ---- end-to-end diff over a real temp git repo ------------------------

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("git {:?}: {}", args, e));
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn init_repo() -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["config", "user.email", "t@e.com"]);
        git(&path, &["config", "user.name", "T"]);
        git(&path, &["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("kept.rs"), "// v1\n").unwrap();
        std::fs::write(path.join(".gitignore"), "ignored.txt\ntarget/\n").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "init"]);
        let head = git(&path, &["rev-parse", "HEAD"]).trim().to_string();
        (dir, path, head)
    }

    #[test]
    fn collect_observes_modified_and_new_skips_ignored_and_deleted() {
        let (_dir, repo, parent_sha) = init_repo();

        // Modify a tracked file.
        std::fs::write(repo.join("kept.rs"), "// v2 modified\n").unwrap();
        // Add a brand-new untracked (tracked-eligible) file.
        std::fs::write(repo.join("added.rs"), "// brand new\n").unwrap();
        // Create an ignored file — must NOT be reported.
        std::fs::write(repo.join("ignored.txt"), "secret scratch\n").unwrap();
        // Create a build-dir file (ignored by .gitignore target/).
        std::fs::create_dir_all(repo.join("target")).unwrap();
        std::fs::write(repo.join("target/out.bin"), "binary\n").unwrap();

        let obs = collect_observations(&repo, &parent_sha).expect("collect");
        let paths: HashSet<&str> = obs.iter().map(|o| o.path.as_str()).collect();

        assert!(paths.contains("kept.rs"), "modified tracked file reported");
        assert!(paths.contains("added.rs"), "new file reported");
        assert!(
            !paths.contains("ignored.txt"),
            "gitignored file must be dropped: {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|p| p.contains("target/")),
            "build dir must be dropped: {:?}",
            paths
        );

        // Modified file: pre_sha present (base blob), post_sha is current blob.
        let kept = obs.iter().find(|o| o.path == "kept.rs").unwrap();
        assert!(kept.pre_sha.is_some(), "modified file has a pre_sha");
        let expected_post = blob_sha_for_bytes(b"// v2 modified\n").unwrap();
        assert_eq!(kept.post_sha, expected_post);
        let expected_pre = blob_sha_for_bytes(b"// v1\n").unwrap();
        assert_eq!(kept.pre_sha.as_deref(), Some(expected_pre.as_str()));

        // New file: pre_sha None.
        let added = obs.iter().find(|o| o.path == "added.rs").unwrap();
        assert!(added.pre_sha.is_none(), "new file has no pre_sha");
        assert_eq!(
            added.post_sha,
            blob_sha_for_bytes(b"// brand new\n").unwrap()
        );
    }

    #[test]
    fn collect_skips_pure_deletion() {
        let (_dir, repo, parent_sha) = init_repo();
        // Delete a tracked file in the worktree.
        std::fs::remove_file(repo.join("kept.rs")).unwrap();
        let obs = collect_observations(&repo, &parent_sha).expect("collect");
        // A deletion has no post content -> not an Ξ_FS observation.
        assert!(
            !obs.iter().any(|o| o.path == "kept.rs"),
            "deletion must be skipped (no post_sha): {:?}",
            obs
        );
    }

    #[test]
    fn collect_empty_when_clean() {
        let (_dir, repo, parent_sha) = init_repo();
        let obs = collect_observations(&repo, &parent_sha).expect("collect");
        assert!(obs.is_empty(), "clean worktree yields no observations");
    }

    // ---- Phase 5: collect_uncommitted_observations (HEAD-relative) --------

    #[test]
    fn collect_uncommitted_reports_modified_and_new_skips_ignored() {
        let (_dir, repo, _head) = init_repo();
        // Modify a tracked file + add an untracked one.
        std::fs::write(repo.join("kept.rs"), "// v2 modified\n").unwrap();
        std::fs::write(repo.join("added.rs"), "// brand new\n").unwrap();
        // gitignored file must NOT appear.
        std::fs::write(repo.join("ignored.txt"), "scratch\n").unwrap();

        let obs = collect_uncommitted_observations(&repo).expect("collect uncommitted");
        let paths: HashSet<&str> = obs.iter().map(|o| o.path.as_str()).collect();
        assert!(paths.contains("kept.rs"), "modified tracked file reported");
        assert!(paths.contains("added.rs"), "new file reported");
        assert!(
            !paths.contains("ignored.txt"),
            "gitignored file must be dropped: {:?}",
            paths
        );

        // Resolves HEAD itself — pre_sha of a modified file is the HEAD blob.
        let kept = obs.iter().find(|o| o.path == "kept.rs").unwrap();
        assert_eq!(
            kept.pre_sha.as_deref(),
            Some(blob_sha_for_bytes(b"// v1\n").unwrap().as_str())
        );
    }

    #[test]
    fn collect_uncommitted_empty_when_clean() {
        let (_dir, repo, _head) = init_repo();
        let obs = collect_uncommitted_observations(&repo).expect("collect uncommitted");
        assert!(obs.is_empty(), "clean checkout yields no observations");
    }
}
