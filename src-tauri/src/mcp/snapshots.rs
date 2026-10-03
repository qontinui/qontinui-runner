//! `/sessions/<id>/snapshots` and `/sessions/<id>/rewind` HTTP endpoints
//! — Phase 4 of the productivity stack. Backs the `/rewind-session` slash
//! command (see `D:/qontinui-root/.claude/commands/rewind-session.md`).
//!
//! `GET /sessions/<id>/snapshots` lists every `session_file_snapshots`
//! row for the given session — exposed so an external caller can inspect
//! the rollback set before triggering a restore.
//!
//! `GET /sessions/<id>/file-changes` pairs every file the session touched
//! with its pre-edit snapshot text and the file's CURRENT text, so a reader
//! (the Terminal grid's `WorkerSessionCell`) can render a true
//! snapshot-vs-now diff whatever tool made the edit. The diff itself is
//! computed client-side; this route only reads and reports, and it reports a
//! read failure per file rather than dropping the file. It is BOUNDED on three
//! axes — [`FILE_CHANGE_TEXT_CAP_BYTES`] per side (structurally, through a
//! `take`-bounded read rather than a stat that is stale by the time it is
//! used), [`FILE_CHANGE_MAX_FILES`] paths per report, and
//! [`FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES`] across the whole response — so an
//! unauthenticated loopback caller cannot make the runner allocate a worker's
//! whole touched set. The first two bound each file and the file count; the
//! third bounds their PRODUCT, which is the number that actually reaches the
//! allocator.
//!
//! A touched path with NO pre-edit snapshot — every path a PTY-hosted `claude`
//! touches, since only the stream-json dispatcher captures snapshots — takes
//! its "before" side from git instead: the path's blob at a resolved base
//! commit, read in-process by [`BaseBlobs`]. Each entry says which source its
//! "before" came from (`beforeSource`), and a git base that cannot be resolved
//! is reported as `unreadable` rather than letting the path read as `created`.
//!
//! `POST /sessions/<id>/rewind` performs the actual restore: for each
//! pre-edit snapshot, verify the on-disk blob's sha256 matches the
//! recorded `blob_sha256`, then copy the blob over the original
//! `file_path`. This avoids the slash command needing to orchestrate
//! `cp` calls inside the LLM tool-call context.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::session_file_snapshots::SnapshotRow;
use crate::mcp::types::ApiState;

/// Per-side byte cap for the text returned by `GET /sessions/<id>/file-changes`.
/// A side above the cap is reported by size only (`truncated: true`, text
/// `None`) — a diff of a truncated file would be a lie, so none is offered.
///
/// The cap is STRUCTURAL, not advisory: [`FileProbe`] offers no unbounded read
/// at all, so a side is read through `File::take(cap + 1)` and a result of
/// `cap + 1` bytes IS the over-cap verdict. A stat-then-read would have decided
/// on a length that is stale the moment it is returned — a worker actively
/// appending to a generated artifact or a log between the two syscalls gets the
/// whole thing read in — and the runner is a tier-0 process whose loss destroys
/// every live session on the box, over a route reachable by anything on
/// loopback (the `:9876` router has permissive CORS and no auth layer). So a
/// worker that touched a multi-GB generated artifact must not be able to make
/// it allocate one, whatever that file is doing while we look at it.
pub const FILE_CHANGE_TEXT_CAP_BYTES: usize = 256 * 1024;

/// Aggregate byte budget for ALL the text one `GET /sessions/<id>/file-changes`
/// response holds.
///
/// [`FILE_CHANGE_TEXT_CAP_BYTES`] bounds one side and [`FILE_CHANGE_MAX_FILES`]
/// bounds the count, but until this budget existed nothing bounded their
/// PRODUCT: 400 files × 2 sides × 256 KiB is ~200 MiB resident, which `Json(…)`
/// then serialises into a second buffer of comparable size — ~400 MiB peak for
/// one request, with no concurrency limit in front of it. The realistic case is
/// worse than the adversarial one is rare: a worker that touched 400 files
/// averaging 50 KiB is ~80 MiB per request, and the page issues one per visible
/// cell per `commit-state-changed` burst.
///
/// Once the budget is spent, the remaining candidates are still REPORTED — with
/// sizes, digests, status and `truncated: true` — so the cut is visible rather
/// than silent. A `detail` naming the budget distinguishes it from a
/// genuinely over-cap file, which the UI renders instead of "too large to
/// diff" (`noDiffReason` in `workerFileChanges.ts`).
pub const FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Maximum number of candidate paths one `GET /sessions/<id>/file-changes`
/// examines. A session that touched more has the remainder reported as
/// `omittedFiles` with `filesTruncated: true` rather than silently cut — and,
/// more importantly, the route's cost is bounded by this constant rather than
/// by how many files a worker happened to touch.
pub const FILE_CHANGE_MAX_FILES: usize = 400;

/// Buffer size for the streaming digest of an over-cap side.
const SHA_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Largest git base blob [`BaseBlobs`] will load to digest.
///
/// A blob at or under the per-side cap is held as text like any other side. A
/// blob OVER it is loaded only to compute its sha256 and then dropped — libgit2
/// offers no streaming read for a packed object, so the digest needs the whole
/// blob for an instant. The blob's size is read from the object header FIRST,
/// and a git object is immutable, so this decision cannot be raced the way a
/// stat on a working-tree file can. Above this ceiling the side is reported
/// `unreadable` with its size in `detail` rather than pulled into a tier-0
/// process.
pub const BASE_BLOB_DIGEST_CEILING_BYTES: usize = 32 * 1024 * 1024;

/// Where an entry's "before" side came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeforeSource {
    /// The session's own pre-edit snapshot (`capture_pre_edit_snapshot`).
    Snapshot,
    /// The path's blob at a resolved git base commit — see [`BaseKind`].
    GitBase,
    /// No "before" side could be established; the entry is `unreadable` and
    /// `detail` names why. Never rendered as a creation.
    None,
}

/// Which rung of the base resolution a `git_base` "before" side came from.
///
/// First match wins: the session's coord-allocated worktree's recorded
/// `parent_sha`; else, in a linked worktree that is not on the default branch,
/// `merge-base(HEAD, origin/<default>)`; else `HEAD`. The first two exist
/// because committing clears a session's touched set, so a `HEAD` base would
/// empty the review the moment the session commits — and `head` says so to
/// the reader rather than hiding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseKind {
    ParentSha,
    MergeBase,
    Head,
}

/// The commit a git-base "before" side was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBase {
    pub kind: BaseKind,
    pub sha: String,
}

/// One path's blob at a resolved base, bounded like any other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseBlob {
    /// The base tree POSITIVELY has no entry at this path — the only evidence
    /// that licenses `created`.
    Absent,
    /// The blob, at or under the cap it was asked for.
    Text(Vec<u8>),
    /// Over the cap: size and sha256 only, never kept.
    Oversize { bytes: usize, sha256: String },
}

/// Why [`BaseProbe::base_blob`] produced no "before" side — and whether the
/// base nonetheless POSITIVELY has an entry at the path, which decides whether
/// a path that is gone now is still reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseError {
    /// No base could be established for this path — not a repository, an
    /// unopenable one, an unresolvable base, a failed tree lookup. Nothing
    /// says the path ever existed, so a path that is gone now is omitted.
    NoBase(String),
    /// The base was resolved and HAS an entry at this path, but it is not read:
    /// a symlink, a directory or submodule, a blob over the digest ceiling, a
    /// failed object read, a filter that cannot be applied in-process. The
    /// entry is `unreadable` whether or not the file still exists — the base
    /// positively had something there, so dropping it would hide a change.
    EntryUnread { base: ResolvedBase, why: String },
}

/// The git-base source for a snapshot-less path's "before" side, narrow enough
/// that a test can supply an in-memory one.
pub trait BaseProbe {
    /// The blob of `path` at its repository's resolved base, in the form a
    /// checkout would write it to the working tree (eol conversion, `ident`),
    /// bounded at `cap` exactly as [`FileProbe::read_capped`] is, together with
    /// which base it came from. `Err` says why there is no "before" side and
    /// whether the base has an entry here — see [`BaseError`]; the entry is
    /// then `unreadable` (or omitted, for [`BaseError::NoBase`] on a path that
    /// is gone), never `created`.
    fn base_blob(&mut self, path: &str, cap: usize) -> Result<(ResolvedBase, BaseBlob), BaseError>;
}

/// One repository's resolved base, opened once per report.
struct RepoBase {
    repo: git2::Repository,
    tree: git2::Oid,
    base: ResolvedBase,
    /// `core.ignorecase`: the checkout lives on a case-insensitive
    /// filesystem, so a touched path's recorded case need not match the
    /// tree's.
    ignorecase: bool,
    /// `core.autocrlf=true`: a checkout converts LF to CRLF for every path
    /// git judges to be text, with no attribute naming it.
    autocrlf_on_checkout: bool,
}

/// The real [`BaseProbe`]: blobs read in-process with `git2`.
///
/// The base tree is resolved ONCE per distinct repository per report and every
/// path in that repository is then a `tree.get_path(rel)` — the
/// `agent_worktree::fs_observer` pattern — so a report's git cost is one repo
/// open and one base resolution per repository, not one process per path.
pub struct BaseBlobs {
    /// Canonical worktree path → the `parent_sha` coord recorded when it
    /// allocated that worktree, for every allocation a live session holds.
    allocated: HashMap<PathBuf, String>,
    /// Canonical directory → the canonical workdir of the repository holding
    /// it, so a directory is discovered once however many paths share it.
    workdir_for_dir: HashMap<PathBuf, Result<PathBuf, String>>,
    /// Canonical workdir → its resolved base.
    bases: HashMap<PathBuf, Result<RepoBase, String>>,
}

impl BaseBlobs {
    /// `allocated` pairs each coord-allocated worktree a live session holds
    /// with its recorded `parent_sha`; paths need not be canonical.
    pub fn new(allocated: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        Self {
            allocated: allocated
                .into_iter()
                .map(|(path, sha)| (std::fs::canonicalize(&path).unwrap_or(path), sha))
                .collect(),
            workdir_for_dir: HashMap::new(),
            bases: HashMap::new(),
        }
    }

    /// How many repositories this reader has resolved a base for. One per
    /// distinct repository, however many paths it was asked about.
    #[cfg(test)]
    pub(crate) fn resolved_repositories(&self) -> usize {
        self.bases.len()
    }

    /// The canonical workdir holding `path`, and `path` relative to it as a
    /// `/`-separated string (the form libgit2's tree lookups take).
    fn locate(&mut self, path: &str) -> Result<(PathBuf, String), String> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err(format!("touched path is not absolute: {}", path.display()));
        }
        // When the file itself exists (and is not a link, whose own name is
        // what the tree records) the WHOLE path is canonicalised, so on a
        // filesystem that folds case the leaf takes its on-disk spelling
        // rather than whatever case the touch was recorded in.
        let is_plain_file = std::fs::symlink_metadata(path)
            .map(|m| !m.file_type().is_symlink())
            .unwrap_or(false);
        let (canon_dir, rest) = match is_plain_file
            .then(|| std::fs::canonicalize(path).ok())
            .flatten()
            .and_then(|full| {
                let name = full.file_name()?.to_os_string();
                Some((full.parent()?.to_path_buf(), PathBuf::from(name)))
            }) {
            Some(split) => split,
            None => {
                // The file — and its directory — may be gone (a session that
                // deleted them), so discovery starts from the nearest
                // directory that exists.
                let mut dir = path.parent();
                while let Some(d) = dir {
                    if d.is_dir() {
                        break;
                    }
                    dir = d.parent();
                }
                let dir =
                    dir.ok_or_else(|| format!("no existing ancestor of {}", path.display()))?;
                let rest = path
                    .strip_prefix(dir)
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .to_path_buf();
                let canon_dir = std::fs::canonicalize(dir)
                    .map_err(|e| format!("canonicalize {}: {e}", dir.display()))?;
                (canon_dir, rest)
            }
        };

        let workdir = self
            .workdir_for_dir
            .entry(canon_dir.clone())
            .or_insert_with(|| discover_workdir(&canon_dir))
            .clone()?;
        let rel = canon_dir
            .join(rest)
            .strip_prefix(&workdir)
            .map_err(|_| {
                format!(
                    "{} is outside its repository's workdir {}",
                    path.display(),
                    workdir.display()
                )
            })?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        Ok((workdir, rel))
    }

    fn base_for(&mut self, workdir: &Path) -> Result<&RepoBase, String> {
        if !self.bases.contains_key(workdir) {
            let resolved = open_repo_base(workdir, self.allocated.get(workdir).map(String::as_str));
            self.bases.insert(workdir.to_path_buf(), resolved);
        }
        match self.bases.get(workdir) {
            Some(Ok(base)) => Ok(base),
            Some(Err(why)) => Err(why.clone()),
            None => Err(format!("no base recorded for {}", workdir.display())),
        }
    }
}

impl BaseProbe for BaseBlobs {
    fn base_blob(&mut self, path: &str, cap: usize) -> Result<(ResolvedBase, BaseBlob), BaseError> {
        let (workdir, rel) = self.locate(path).map_err(BaseError::NoBase)?;
        let rb = self.base_for(&workdir).map_err(BaseError::NoBase)?;
        let tree = rb
            .repo
            .find_tree(rb.tree)
            .map_err(|e| BaseError::NoBase(format!("base tree {}: {e}", rb.base.sha)))?;
        let (entry, rel) = match lookup_entry(&rb.repo, &tree, &rel, rb.ignorecase) {
            Ok(Some(found)) => found,
            Ok(None) => return Ok((rb.base.clone(), BaseBlob::Absent)),
            Err(e) => {
                return Err(BaseError::NoBase(format!(
                    "look up {rel} at base {}: {e}",
                    rb.base.sha
                )))
            }
        };
        // From here on the base positively HAS an entry at this path: every
        // refusal below is "present but not read", never "no base".
        let unread = |why: String| BaseError::EntryUnread {
            base: rb.base.clone(),
            why,
        };
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return Err(unread(format!(
                "{rel} is not a file at base {} (a directory or submodule there)",
                rb.base.sha
            )));
        }
        // A symlink's blob is its target string, while the current side is
        // read THROUGH the link — comparing the two would invent a change.
        if entry.filemode() == 0o120_000 {
            return Err(unread(format!(
                "{rel} is a symlink at base {}",
                rb.base.sha
            )));
        }
        let odb = rb
            .repo
            .odb()
            .map_err(|e| unread(format!("open object db: {e}")))?;
        let (size, _) = odb
            .read_header(entry.id())
            .map_err(|e| unread(format!("read base blob header for {rel}: {e}")))?;
        if size > BASE_BLOB_DIGEST_CEILING_BYTES {
            return Err(unread(format!(
                "base blob for {rel} is {size} bytes, above the {BASE_BLOB_DIGEST_CEILING_BYTES}-byte ceiling this route loads to digest"
            )));
        }
        let content = worktree_form(rb, &rel, entry.id()).map_err(unread)?;
        let side = if content.len() <= cap {
            BaseBlob::Text(content)
        } else {
            BaseBlob::Oversize {
                bytes: content.len(),
                sha256: sha256_hex(&content),
            }
        };
        Ok((rb.base.clone(), side))
    }
}

/// The tree entry at `rel`, with the path as the TREE spells it.
///
/// `tree.get_path` is case-sensitive; on a checkout with `core.ignorecase` a
/// touched path recorded as `README.md` names the same file as a tracked
/// `Readme.md`, and reading that as "the base lacks it" would report a
/// modification as a creation. So when the exact lookup misses and the
/// repository folds case, each component is matched case-insensitively —
/// preferring an exact match at every level — before concluding absence.
fn lookup_entry(
    repo: &git2::Repository,
    tree: &git2::Tree<'_>,
    rel: &str,
    ignorecase: bool,
) -> Result<Option<(git2::TreeEntry<'static>, String)>, git2::Error> {
    match tree.get_path(Path::new(rel)) {
        Ok(entry) => return Ok(Some((entry, rel.to_string()))),
        Err(e) if e.code() == git2::ErrorCode::NotFound => {}
        Err(e) => return Err(e),
    }
    if !ignorecase {
        return Ok(None);
    }
    let components: Vec<&str> = rel.split('/').collect();
    let mut current = repo.find_tree(tree.id())?;
    let mut spelled: Vec<String> = Vec::with_capacity(components.len());
    for (i, want) in components.iter().enumerate() {
        let found = current
            .iter()
            .find(|e| e.name_bytes() == want.as_bytes())
            .or_else(|| {
                current.iter().find(|e| {
                    e.name()
                        .is_some_and(|name| name.to_lowercase() == want.to_lowercase())
                })
            })
            .map(|e| e.to_owned());
        let Some(entry) = found else {
            return Ok(None);
        };
        spelled.push(String::from_utf8_lossy(entry.name_bytes()).into_owned());
        if i + 1 == components.len() {
            return Ok(Some((entry, spelled.join("/"))));
        }
        if entry.kind() != Some(git2::ObjectType::Tree) {
            return Ok(None);
        }
        current = repo.find_tree(entry.id())?;
    }
    Ok(None)
}

/// A path's attribute, owned (`git2::AttrValue` borrows the repository).
#[derive(Debug, PartialEq, Eq)]
enum Attr {
    Unspecified,
    True,
    False,
    Value(String),
}

fn attr_of(repo: &git2::Repository, rel: &str, name: &str) -> Result<Attr, String> {
    let raw = repo
        .get_attr_bytes(Path::new(rel), name, git2::AttrCheckFlags::FILE_THEN_INDEX)
        .map_err(|e| format!("read the `{name}` attribute of {rel}: {e}"))?;
    Ok(match git2::AttrValue::from_bytes(raw) {
        git2::AttrValue::Unspecified => Attr::Unspecified,
        git2::AttrValue::True => Attr::True,
        git2::AttrValue::False => Attr::False,
        git2::AttrValue::String(value) => Attr::Value(value.to_string()),
        git2::AttrValue::Bytes(value) => Attr::Value(String::from_utf8_lossy(value).into_owned()),
    })
}

/// The blob `id` at `rel` as a checkout would write it to the working tree.
///
/// The "after" side is the file on disk, which went through git's checkout
/// filters — `core.autocrlf`, a `.gitattributes` `eol=crlf` / `text`, `ident`
/// — while the object store holds the CLEAN form. Comparing a raw blob to a
/// smudged file reports every line of a CRLF checkout as changed, so the base
/// side is smudged first.
///
/// The common case (no attribute or config that could convert anything) is the
/// raw blob, unread twice. Otherwise libgit2's own checkout writes this one path
/// into a scratch directory, which runs exactly the filters a real checkout
/// would, with the repository's attributes and configuration. A path carrying a
/// `filter=<driver>` attribute (Git LFS, a custom clean/smudge pair) needs an
/// external program libgit2 does not run, so it is refused rather than compared
/// in its clean form.
fn worktree_form(rb: &RepoBase, rel: &str, id: git2::Oid) -> Result<Vec<u8>, String> {
    let attr = |name: &str| attr_of(&rb.repo, rel, name);
    if let Attr::Value(driver) = attr("filter")? {
        return Err(format!(
            "{rel} carries `filter={driver}`, whose smudge program the runner does not run in-process"
        ));
    }
    let text = attr("text")?;
    let untouched = attr("ident")? != Attr::True
        && attr("eol")? == Attr::Unspecified
        && attr("crlf")? == Attr::Unspecified
        && (text == Attr::False || (text == Attr::Unspecified && !rb.autocrlf_on_checkout));
    if untouched {
        return rb
            .repo
            .find_blob(id)
            .map(|blob| blob.content().to_vec())
            .map_err(|e| format!("read base blob for {rel}: {e}"));
    }

    let scratch = tempfile::tempdir().map_err(|e| format!("scratch dir to smudge {rel}: {e}"))?;
    let tree = rb
        .repo
        .find_object(rb.tree, Some(git2::ObjectType::Tree))
        .map_err(|e| format!("base tree {}: {e}", rb.base.sha))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout
        .target_dir(scratch.path())
        .force()
        .recreate_missing(true)
        .update_index(false)
        .disable_pathspec_match(true)
        .path(rel);
    rb.repo
        .checkout_tree(&tree, Some(&mut checkout))
        .map_err(|e| format!("apply checkout filters to {rel}: {e}"))?;
    std::fs::read(scratch.path().join(rel))
        .map_err(|e| format!("read {rel} as checked out with filters: {e}"))
}

/// The canonical workdir of the repository holding `dir`.
fn discover_workdir(dir: &Path) -> Result<PathBuf, String> {
    let repo = git2::Repository::discover(dir)
        .map_err(|e| format!("not in an openable git repository ({}): {e}", dir.display()))?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| format!("{} is in a bare repository", dir.display()))?;
    std::fs::canonicalize(workdir).map_err(|e| format!("canonicalize {}: {e}", workdir.display()))
}

/// Open `workdir` and resolve its base by Decision 1's three rungs.
fn open_repo_base(workdir: &Path, allocated_parent_sha: Option<&str>) -> Result<RepoBase, String> {
    let repo = git2::Repository::open(workdir)
        .map_err(|e| format!("open repository {}: {e}", workdir.display()))?;
    let (tree, base) = resolve_base(&repo, allocated_parent_sha)?;
    let config = repo
        .config()
        .map_err(|e| format!("read config of {}: {e}", workdir.display()))?;
    let ignorecase = config.get_bool("core.ignorecase").unwrap_or(false);
    // `core.autocrlf` is `true`, `input` or `false`; only `true` converts on
    // checkout (`input` converts on commit only).
    let autocrlf_on_checkout = config.get_bool("core.autocrlf").unwrap_or(false);
    Ok(RepoBase {
        repo,
        tree,
        base,
        ignorecase,
        autocrlf_on_checkout,
    })
}

/// Rung 1: the allocation's recorded `parent_sha`. Rung 2: a linked worktree
/// not on the default branch → `merge-base(HEAD, origin/<default>)`. Rung 3:
/// `HEAD`.
fn resolve_base(
    repo: &git2::Repository,
    allocated_parent_sha: Option<&str>,
) -> Result<(git2::Oid, ResolvedBase), String> {
    if let Some(sha) = allocated_parent_sha {
        // Resolved exactly as the Ξ_FS observer resolves its own pre-images.
        match crate::agent_worktree::fs_observer::commit_tree(repo, sha) {
            Ok(tree) => {
                return Ok((
                    tree.id(),
                    ResolvedBase {
                        kind: BaseKind::ParentSha,
                        sha: sha.to_string(),
                    },
                ))
            }
            // Honest to fall through: the rung that answers is named in
            // `baseKind`, so a reader is never told this was the allocation base.
            Err(e) => warn!("file-changes: allocated parent_sha unusable, falling back: {e}"),
        }
    }

    let head = repo
        .head()
        .map_err(|e| format!("HEAD does not resolve: {e}"))?;
    let head_commit = head
        .peel_to_commit()
        .map_err(|e| format!("HEAD is not a commit: {e}"))?;

    if repo.is_worktree() {
        let default = default_remote_branch(repo);
        let on_default = matches!(
            (&default, head.is_branch().then(|| head.shorthand()).flatten()),
            (Some((name, _)), Some(branch)) if name == branch
        );
        if !on_default {
            // A HEAD base here would hide everything the session committed,
            // so an unresolvable default is UNKNOWN — never a quiet `HEAD`.
            let (name, tip) = default.ok_or_else(|| {
                "linked worktree off the default branch, and origin's default branch does not resolve"
                    .to_string()
            })?;
            let mb = repo
                .merge_base(head_commit.id(), tip)
                .map_err(|e| format!("merge-base(HEAD, origin/{name}): {e}"))?;
            let tree = repo
                .find_commit(mb)
                .and_then(|c| c.tree())
                .map_err(|e| format!("tree of merge-base {mb}: {e}"))?;
            return Ok((
                tree.id(),
                ResolvedBase {
                    kind: BaseKind::MergeBase,
                    sha: mb.to_string(),
                },
            ));
        }
    }

    let tree = head_commit
        .tree()
        .map_err(|e| format!("tree of HEAD: {e}"))?;
    Ok((
        tree.id(),
        ResolvedBase {
            kind: BaseKind::Head,
            sha: head_commit.id().to_string(),
        },
    ))
}

/// origin's default branch as `(name, tip)`: `refs/remotes/origin/HEAD`'s
/// target when it is set, else `origin/main`, else `origin/master`.
fn default_remote_branch(repo: &git2::Repository) -> Option<(String, git2::Oid)> {
    const PREFIX: &str = "refs/remotes/origin/";
    let symbolic = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|r| r.symbolic_target().map(str::to_string));
    let candidates = symbolic
        .into_iter()
        .chain(["main", "master"].map(|b| format!("{PREFIX}{b}")));
    for refname in candidates {
        if let (Some(name), Ok(oid)) = (refname.strip_prefix(PREFIX), repo.refname_to_id(&refname))
        {
            return Some((name.to_string(), oid));
        }
    }
    None
}

/// One file a session touched, paired with what it looked like BEFORE the
/// session's first edit and what it looks like NOW.
///
/// The "before" side is the session's pre-edit snapshot when one exists, and
/// otherwise the path's blob at a resolved git base (`before_source` says
/// which). `status` is one of:
/// - `modified` — the before and current text differ;
/// - `unchanged` — same sha on both sides;
/// - `deleted` — a before side exists but the file is gone;
/// - `created` — the git base POSITIVELY lacks the path and it exists now
///   (asserted on no weaker evidence: a path whose base could not be read is
///   `unreadable`, never `created`);
/// - `binary` — at least one side is not valid UTF-8;
/// - `unreadable` — a side could not be read, or no before side could be
///   established; `detail` names why.
///
/// Both text sides are `None` whenever they cannot honestly be diffed
/// (`binary`, `unreadable`, or a side over [`FILE_CHANGE_TEXT_CAP_BYTES`]).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChange {
    pub file_path: String,
    pub status: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_bytes: Option<usize>,
    pub after_bytes: Option<usize>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub truncated: bool,
    /// `taken_at` of the pre-edit snapshot; `None` unless `before_source` is
    /// `snapshot`.
    pub taken_at: Option<String>,
    pub detail: Option<String>,
    /// Where the "before" side came from.
    pub before_source: BeforeSource,
    /// The base rung a `git_base` before side was read from; `None` otherwise.
    pub base_kind: Option<BaseKind>,
    /// The commit a `git_base` before side was read from; `None` otherwise.
    pub base_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChangesResponse {
    pub session_id: String,
    pub files: Vec<SessionFileChange>,
    /// True when the session touched more paths than [`FILE_CHANGE_MAX_FILES`]
    /// and the list below is therefore a prefix — the UI says so rather than
    /// presenting a cut list as the whole truth.
    pub files_truncated: bool,
    /// How many candidate paths were dropped by that cap (`0` when none were).
    pub omitted_files: usize,
    /// The base rung every `git_base` entry was read from, when they all share
    /// ONE base. `None` when no entry is `git_base`, or when the session's
    /// paths span repositories with different bases — each entry then carries
    /// its own `baseKind` / `baseSha`.
    pub base_kind: Option<BaseKind>,
    /// The commit paired with [`Self::base_kind`].
    pub base_sha: Option<String>,
    /// Epoch millis the report was assembled, so a reader can label its age.
    pub read_at_ms: i64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// sha256 AND byte length of a reader's remaining bytes, computed through a
/// fixed-size buffer so the contents are never held in memory.
///
/// The length comes from the same pass as the digest rather than from a
/// separate `metadata()` call, so the two describe the same bytes even if the
/// file is being written while we read it.
fn sha256_stream(mut reader: impl Read) -> std::io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; SHA_STREAM_CHUNK_BYTES];
    let mut len: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        len += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((format!("{:x}", hasher.finalize()), len))
}

/// Filesystem access for [`assemble_file_changes`], narrow enough that a test
/// can supply an in-memory tree.
///
/// **There is deliberately no unbounded read here.** The trait offers exactly
/// two operations, and neither can pull an arbitrary file into memory:
/// [`FileProbe::read_capped`] stops after `cap + 1` bytes, and
/// [`FileProbe::digest`] streams through a fixed buffer. An earlier shape had a
/// `size` stat plus an unbounded `read`, which made the cap a decision taken on
/// a length that the very next syscall could invalidate (TOCTOU); the bound is
/// now a property of the read itself.
pub trait FileProbe {
    /// Read at most `cap + 1` bytes of `path`.
    ///
    /// The extra byte is the verdict: a result of exactly `cap + 1` bytes means
    /// the file is OVER the cap and must not be diffed. Anything shorter is the
    /// whole file.
    fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>>;
    /// sha256 AND byte length of `path`, computed in ONE streaming pass so the
    /// contents are never held and the two agree with each other.
    fn digest(&self, path: &str) -> std::io::Result<(String, u64)>;
}

/// The real filesystem.
pub struct DiskFiles;

impl FileProbe for DiskFiles {
    fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
        // Saturating, not `+ 1`: `FileProbe` is public and the tests already
        // hand it `usize::MAX`. A wrapping `cap + 1` there is a debug panic,
        // and in release a `take(0)` — which reports the file as EMPTY TEXT,
        // the one answer that is both wrong and confident.
        let limit = (cap as u64).saturating_add(1);
        let mut buf = Vec::new();
        std::fs::File::open(path)?
            .take(limit)
            .read_to_end(&mut buf)?;
        Ok(buf)
    }
    fn digest(&self, path: &str) -> std::io::Result<(String, u64)> {
        sha256_stream(std::fs::File::open(path)?)
    }
}

/// One side of a file change as observed on disk.
enum Side {
    Missing,
    Unreadable(String),
    /// At or below the cap, so the bytes are held and can be diffed.
    Text(Vec<u8>),
    /// Over the cap: size and digest only, never buffered.
    Oversize {
        bytes: usize,
        sha256: String,
        /// The cap this side's read was ACTUALLY handed — recorded at the read
        /// that refused it, so the attribution below cannot be moved by a
        /// concurrent writer. `bytes` comes from the second (digest) pass and
        /// is therefore a different observation of the same file: a file that
        /// SHRINKS between the two passes lands under the per-side cap and
        /// would otherwise be attributed to the report's budget when the
        /// budget was never touched.
        handed_cap: usize,
    },
}

impl Side {
    /// Bytes this side is holding resident. `0` for every variant that is not
    /// buffered text — which is the point of the other variants.
    fn buffered_len(&self) -> usize {
        match self {
            Side::Text(bytes) => bytes.len(),
            _ => 0,
        }
    }
}

/// Read one side, bounded at `cap` bytes.
///
/// The read itself carries the bound (`take(cap + 1)`), so nothing decided here
/// can be invalidated by a concurrent writer: a file that grows past `cap`
/// between two syscalls simply comes back as `cap + 1` bytes and is classified
/// [`Side::Oversize`]. That is the whole TOCTOU fix — there is no stat to race.
///
/// `cap` is the smaller of [`FILE_CHANGE_TEXT_CAP_BYTES`] and whatever is left
/// of the report's aggregate budget, so a `0` cap (budget spent) makes every
/// non-empty file oversize, which is exactly the "sizes and digests only"
/// behaviour that budget wants.
fn read_side(files: &dyn FileProbe, path: &str, cap: usize) -> Side {
    let bytes = match files.read_capped(path, cap) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Side::Missing,
        Err(e) => return Side::Unreadable(e.to_string()),
    };
    if bytes.len() <= cap {
        return Side::Text(bytes);
    }
    // Over the cap. Drop the probe bytes before the streaming pass so the two
    // are never resident together, then describe the file by size and digest.
    drop(bytes);
    match files.digest(path) {
        Ok((sha256, len)) => Side::Oversize {
            bytes: usize::try_from(len).unwrap_or(usize::MAX),
            sha256,
            handed_cap: cap,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Side::Missing,
        Err(e) => Side::Unreadable(e.to_string()),
    }
}

/// Running aggregate cap on the text one report holds
/// ([`FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES`]).
///
/// It works by SHRINKING the per-side cap handed to [`read_side`], so an
/// over-budget side is never read into memory in the first place — the budget
/// is enforced at the same place and in the same way as the per-side cap,
/// rather than by trimming an already-allocated list afterwards.
struct TextBudget {
    total: usize,
    remaining: usize,
}

impl TextBudget {
    fn new(total: usize) -> Self {
        Self {
            total,
            remaining: total,
        }
    }
    /// The cap for the next side: the per-side cap, or what is left of the
    /// aggregate budget, whichever is smaller.
    fn cap(&self) -> usize {
        FILE_CHANGE_TEXT_CAP_BYTES.min(self.remaining)
    }
    /// The cap an UNSPENT budget hands out. A side handed less than this was
    /// refused because EARLIER reads had spent the budget down — which is the
    /// exact claim the truncation `detail` makes, and the only reading under
    /// which it is true for a total budget smaller than the per-side cap (a
    /// configuration the route never uses, but `assemble_file_changes` allows
    /// and the tests exercise).
    fn unspent_cap(&self) -> usize {
        FILE_CHANGE_TEXT_CAP_BYTES.min(self.total)
    }
    fn spend(&mut self, n: usize) {
        self.remaining = self.remaining.saturating_sub(n);
    }
    /// Give back bytes that were read but NOT kept in the response (a side
    /// dropped as binary, or discarded because its partner was oversize). They
    /// were resident for one iteration, but they are not in `out`.
    fn refund(&mut self, n: usize) {
        self.remaining = self.remaining.saturating_add(n);
    }
}

/// What a side contributes once its size, digest and (maybe) bytes are known.
struct SideFacts {
    bytes: usize,
    sha256: String,
    /// `None` for an over-cap side — present but not diffable.
    text: Option<Vec<u8>>,
    /// `Some(cap)` iff the read refused to buffer this side, carrying the cap
    /// it was handed. `None` for a side that fits. See [`Side::Oversize`].
    handed_cap: Option<usize>,
}

enum SideOutcome {
    Absent,
    Failed(String),
    Present(SideFacts),
}

fn classify(side: Side) -> SideOutcome {
    match side {
        Side::Missing => SideOutcome::Absent,
        Side::Unreadable(why) => SideOutcome::Failed(why),
        Side::Text(bytes) => SideOutcome::Present(SideFacts {
            bytes: bytes.len(),
            sha256: sha256_hex(&bytes),
            text: Some(bytes),
            handed_cap: None,
        }),
        Side::Oversize {
            bytes,
            sha256,
            handed_cap,
        } => SideOutcome::Present(SideFacts {
            bytes,
            sha256,
            text: None,
            handed_cap: Some(handed_cap),
        }),
    }
}

/// The bounded result of [`assemble_file_changes`].
pub struct AssembledFileChanges {
    pub files: Vec<SessionFileChange>,
    /// Candidate paths dropped because the report hit its `max_files` bound.
    pub omitted_files: usize,
}

impl AssembledFileChanges {
    /// The one base every `git_base` entry shares, or `None` when there is no
    /// such entry or they disagree. See [`SessionFileChangesResponse::base_kind`].
    pub fn shared_base(&self) -> Option<ResolvedBase> {
        let mut bases = self
            .files
            .iter()
            .filter_map(|c| match (c.base_kind, &c.base_sha) {
                (Some(kind), Some(sha)) => Some(ResolvedBase {
                    kind,
                    sha: sha.clone(),
                }),
                _ => None,
            });
        let first = bases.next()?;
        bases.all(|b| b == first).then_some(first)
    }
}

/// Where the "before" side of one candidate was established from.
enum BeforeOrigin<'a> {
    Snapshot {
        taken_at: &'a str,
        recorded_sha: &'a str,
    },
    GitBase(ResolvedBase),
}

impl BaseBlob {
    /// This blob as a [`Side`], recording the cap its read was handed so the
    /// truncation attribution works exactly as it does for a disk read.
    fn into_side(self, handed_cap: usize) -> Side {
        match self {
            BaseBlob::Absent => Side::Missing,
            BaseBlob::Text(bytes) => Side::Text(bytes),
            BaseBlob::Oversize { bytes, sha256 } => Side::Oversize {
                bytes,
                sha256,
                handed_cap,
            },
        }
    }
}

/// The entry for a snapshot-less path that exists now but whose git base could
/// not be established. `unreadable`, with the reason — the one thing it must
/// not be is `created`, which is what this path read as before the git base
/// existed.
fn no_base_entry(file_path: &str, why: &str) -> SessionFileChange {
    SessionFileChange {
        file_path: file_path.to_string(),
        status: "unreadable".to_string(),
        before: None,
        after: None,
        before_bytes: None,
        after_bytes: None,
        before_sha256: None,
        after_sha256: None,
        truncated: false,
        taken_at: None,
        detail: Some(format!("no pre-edit snapshot and no git base: {why}")),
        before_source: BeforeSource::None,
        base_kind: None,
        base_sha: None,
    }
}

/// The entry for a snapshot-less path whose base HAS an entry that was not
/// read (a symlink, a directory, an over-ceiling blob, a filter driver). Its
/// base is named, since it was resolved; the entry is `unreadable` even when
/// the file is gone now, because the base positively had something there.
fn unread_base_entry(file_path: &str, base: ResolvedBase, why: &str) -> SessionFileChange {
    SessionFileChange {
        file_path: file_path.to_string(),
        status: "unreadable".to_string(),
        before: None,
        after: None,
        before_bytes: None,
        after_bytes: None,
        before_sha256: None,
        after_sha256: None,
        truncated: false,
        taken_at: None,
        detail: Some(format!("git base entry not read: {why}")),
        before_source: BeforeSource::GitBase,
        base_kind: Some(base.kind),
        base_sha: Some(base.sha),
    }
}

/// Build the change list for a session from its snapshot rows and touched
/// paths. Pure over `files` so the pairing/status logic is unit-testable
/// without a filesystem.
///
/// - The FIRST `captured_before` snapshot per path is the "before" side
///   (the same rule `rewind_session_handler` applies); later rows are
///   ignored.
/// - A touched path with no snapshot takes its "before" side from `base`
///   (the path's blob at a resolved git base). It is `created` only when the
///   base positively lacks the path, `deleted` when the base has it and the
///   file is gone, and omitted when neither the base nor the disk has it
///   (the session never left a file there). When no base can be established
///   it is `unreadable` with the reason — or omitted, if it does not exist
///   now either, since nothing then positively says there was ever a file.
///   When the base HAS an entry it will not read ([`BaseError::EntryUnread`])
///   the path is `unreadable` whether or not it exists now.
/// - A base side is read under the same per-side cap and spends the same
///   aggregate budget as a snapshot side. Presence at the base is decided by
///   the tree lookup, not by the bytes, so a spent budget truncates a base
///   side's TEXT but never turns a modification into a creation.
/// - Order: snapshot rows in `taken_at` order, then snapshot-less touched
///   paths in touch order.
/// - At most `max_files` CANDIDATE paths are examined, in that order. The
///   bound is on candidates rather than emitted rows so it also bounds the
///   number of read syscalls; a candidate that turns out not to be a change
///   still consumes its slot. Whatever is left over is counted into
///   [`AssembledFileChanges::omitted_files`] and never silently dropped.
/// - At most `total_text_budget` bytes of TEXT are held across the whole
///   report. Once it is spent every remaining candidate is still emitted —
///   status, sizes, digests, `truncated: true` and a `detail` naming the
///   budget — so the operator sees which files changed and only loses the
///   ability to diff them inline. Files are not dropped to stay in budget;
///   their bodies are.
pub fn assemble_file_changes(
    snapshots: &[SnapshotRow],
    touched: &[String],
    files: &dyn FileProbe,
    base: &mut dyn BaseProbe,
    max_files: usize,
    total_text_budget: usize,
) -> AssembledFileChanges {
    // Resolve the candidate set FIRST, so the bound is applied before any
    // filesystem work rather than after it.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut candidates: Vec<(&str, Option<&SnapshotRow>)> = Vec::new();
    for snap in snapshots.iter().filter(|s| s.captured_before) {
        if seen.insert(snap.file_path.as_str()) {
            candidates.push((snap.file_path.as_str(), Some(snap)));
        }
    }
    for path in touched {
        if seen.insert(path.as_str()) {
            candidates.push((path.as_str(), None));
        }
    }
    let omitted_files = candidates.len().saturating_sub(max_files);
    candidates.truncate(max_files);

    let mut out: Vec<SessionFileChange> = Vec::with_capacity(candidates.len());
    let mut budget = TextBudget::new(total_text_budget);
    for (path, snapshot) in candidates {
        let (origin, before) = match snapshot {
            Some(snap) => (
                BeforeOrigin::Snapshot {
                    taken_at: &snap.taken_at,
                    recorded_sha: &snap.blob_sha256,
                },
                read_side(files, &snap.snapshot_blob_path, budget.cap()),
            ),
            None => {
                let cap = budget.cap();
                match base.base_blob(path, cap) {
                    Ok((resolved, blob)) => (BeforeOrigin::GitBase(resolved), blob.into_side(cap)),
                    Err(BaseError::NoBase(why)) => {
                        // A zero-byte probe: all this needs is whether the path
                        // exists now, not its contents.
                        match files.read_capped(path, 0) {
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            _ => out.push(no_base_entry(path, &why)),
                        }
                        continue;
                    }
                    Err(BaseError::EntryUnread { base, why }) => {
                        // The base positively had an entry here, so the path
                        // is reported whether or not it still exists.
                        out.push(unread_base_entry(path, base, &why));
                        continue;
                    }
                }
            }
        };
        // Sequential caps, not one cap used twice: the second side's bound
        // already accounts for what the first side took, so a single pair can
        // never hold 2× the remaining budget.
        budget.spend(before.buffered_len());
        let after = read_side(files, path, budget.cap());
        budget.spend(after.buffered_len());
        if matches!(origin, BeforeOrigin::GitBase(_))
            && matches!(before, Side::Missing)
            && matches!(after, Side::Missing)
        {
            // Neither the base nor the disk has it: the session never left a
            // file there. Nothing was buffered, so nothing to refund.
            continue;
        }
        let spent = before.buffered_len() + after.buffered_len();
        // Sampled from the budget's STARTING total, not its remainder, so it is
        // the same value for every entry — the per-side `handed_cap` recorded
        // at each read is what varies, and comparing the two is what decides
        // which bound refused a side.
        let unspent_cap = budget.unspent_cap();
        let change = pair_sides(path, origin, before, after, unspent_cap);
        // Bytes read but not kept (a binary side, or one discarded because its
        // partner was oversize) were resident for this iteration only — they
        // are not in `out`, so they do not count against the report's budget.
        let kept = change.before.as_ref().map_or(0, |s| s.len())
            + change.after.as_ref().map_or(0, |s| s.len());
        budget.refund(spent.saturating_sub(kept));
        out.push(change);
    }

    AssembledFileChanges {
        files: out,
        omitted_files,
    }
}

/// Pair the two sides into one reported entry.
fn pair_sides(
    file_path: &str,
    origin: BeforeOrigin<'_>,
    before: Side,
    after: Side,
    unspent_cap: usize,
) -> SessionFileChange {
    let mut change = SessionFileChange {
        file_path: file_path.to_string(),
        status: String::new(),
        before: None,
        after: None,
        before_bytes: None,
        after_bytes: None,
        before_sha256: None,
        after_sha256: None,
        truncated: false,
        taken_at: None,
        detail: None,
        before_source: BeforeSource::Snapshot,
        base_kind: None,
        base_sha: None,
    };
    let recorded_before_sha = match origin {
        BeforeOrigin::Snapshot {
            taken_at,
            recorded_sha,
        } => {
            change.taken_at = Some(taken_at.to_string());
            Some(recorded_sha)
        }
        BeforeOrigin::GitBase(resolved) => {
            change.before_source = BeforeSource::GitBase;
            change.base_kind = Some(resolved.kind);
            change.base_sha = Some(resolved.sha);
            None
        }
    };
    let had_snapshot = recorded_before_sha.is_some();

    let before = match classify(before) {
        SideOutcome::Failed(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("pre-edit snapshot unreadable: {why}"));
            return change;
        }
        SideOutcome::Absent if had_snapshot => {
            change.status = "unreadable".to_string();
            change.detail = Some("pre-edit snapshot blob is missing on disk".to_string());
            return change;
        }
        SideOutcome::Absent => None,
        SideOutcome::Present(facts) => Some(facts),
    };
    if let Some(facts) = &before {
        if let Some(recorded) = recorded_before_sha {
            if recorded != facts.sha256 {
                change.status = "unreadable".to_string();
                change.detail = Some(format!(
                    "pre-edit snapshot blob sha256 mismatch: recorded={recorded}, actual={}",
                    facts.sha256
                ));
                return change;
            }
        }
        change.before_bytes = Some(facts.bytes);
        change.before_sha256 = Some(facts.sha256.clone());
    }

    let after = match classify(after) {
        SideOutcome::Failed(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("current file unreadable: {why}"));
            return change;
        }
        SideOutcome::Absent => None,
        SideOutcome::Present(facts) => Some(facts),
    };
    if let Some(facts) = &after {
        change.after_bytes = Some(facts.bytes);
        change.after_sha256 = Some(facts.sha256.clone());
    }

    change.status = match (&before, &after) {
        (Some(_), None) => "deleted",
        // `before` is `None` only for a git base that POSITIVELY lacks the
        // path: a snapshot with no blob returned above, and a path with no
        // resolvable base never reaches `pair_sides` (`no_base_entry`).
        (None, Some(_)) => "created",
        (Some(_), Some(_)) if change.before_sha256 == change.after_sha256 => "unchanged",
        (Some(_), Some(_)) => "modified",
        (None, None) => "unreadable",
    }
    .to_string();
    if change.status == "unreadable" {
        change.detail = Some("neither side exists".to_string());
        return change;
    }

    let before_text = before
        .as_ref()
        .and_then(|f| f.text.as_deref())
        .map(std::str::from_utf8);
    let after_text = after
        .as_ref()
        .and_then(|f| f.text.as_deref())
        .map(std::str::from_utf8);
    if matches!(before_text, Some(Err(_))) || matches!(after_text, Some(Err(_))) {
        change.status = "binary".to_string();
        return change;
    }
    // A present side with no bytes is one `read_side` refused to buffer. WHICH
    // bound refused it is decided PER SIDE from the cap that side's read was
    // actually handed, recorded at the read itself — not from any flag sampled
    // before the reads (the aggregate budget shrinks between the two sides of
    // one entry, so such a flag is wrong for the second side exactly at the
    // boundary), and not from the side's reported length either. That length
    // comes from the digest pass, a SECOND observation of the same file: a file
    // that shrinks between the two passes reports a length under the per-side
    // cap and would be blamed on a budget that was never touched. The handed
    // cap is immune in both directions because it is not a property of the file
    // at all.
    let budget_limited_sides: Vec<bool> = [before.as_ref(), after.as_ref()]
        .into_iter()
        .flatten()
        .filter(|f| f.text.is_none())
        .map(|f| f.handed_cap.is_some_and(|cap| cap < unspent_cap))
        .collect();
    if !budget_limited_sides.is_empty() {
        change.truncated = true;
        // A side handed less than an unspent budget's cap can only have been
        // refused by what earlier reads had already spent. Honesty: a 2 KiB
        // file whose neighbours ate the report's budget is not "too large to
        // diff", which is what the UI says for a plain over-cap entry, so name
        // the bound that actually applied. If EITHER side was handed the full
        // cap and still refused, "too large" is the true statement about this
        // entry and the UI's default says it.
        if budget_limited_sides.iter().all(|by_budget| *by_budget) {
            change.detail = Some(
                "the report's text budget was spent on earlier files — size and digest only"
                    .to_string(),
            );
        }
        return change;
    }
    change.before = before_text.and_then(|r| r.ok()).map(str::to_string);
    change.after = after_text.and_then(|r| r.ok()).map(str::to_string);
    change
}

async fn file_changes_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<SessionFileChangesResponse>, (StatusCode, String)> {
    let pg = &state.app_state.pg_db;
    let snapshots = pg
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let touched = pg
        .get_files_touched(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let allocated = live_allocation_bases(&state);

    // Disk and git reads are blocking; keep them off the async executor.
    let assembled = tokio::task::spawn_blocking(move || {
        assemble_file_changes(
            &snapshots,
            &touched,
            &DiskFiles,
            &mut BaseBlobs::new(allocated),
            FILE_CHANGE_MAX_FILES,
            FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("file-changes assembly panicked: {e}"),
        )
    })?;

    if assembled.omitted_files > 0 {
        warn!(
            "file-changes: session={} touched more than {} paths; {} omitted from the report",
            session_id, FILE_CHANGE_MAX_FILES, assembled.omitted_files
        );
    }

    let shared_base = assembled.shared_base();
    Ok(Json(SessionFileChangesResponse {
        session_id,
        files: assembled.files,
        files_truncated: assembled.omitted_files > 0,
        omitted_files: assembled.omitted_files,
        base_kind: shared_base.as_ref().map(|b| b.kind),
        base_sha: shared_base.map(|b| b.sha),
        read_at_ms: chrono::Utc::now().timestamp_millis(),
    }))
}

/// `(worktree_path, parent_sha)` for every coord-allocated worktree a live
/// session holds — PTY terminals and stream-json sessions alike — so a
/// touched path inside one reads its "before" side from the allocation base
/// (rung 1 of [`resolve_base`]).
///
/// Keyed by worktree rather than by the requested session id: an allocated
/// worktree belongs to exactly one agent, and the route's id (a `claude`
/// session id for a PTY tab, a task-run id for a worker) is not the key either
/// session type parks its context under. A session that has ended no longer
/// holds its allocation, and its paths fall to the next rung — which
/// `baseKind` names.
fn live_allocation_bases(state: &ApiState) -> Vec<(PathBuf, String)> {
    use tauri::Manager;
    let terminals = crate::mcp::terminals::get_terminal_manager(state)
        .sessions_snapshot()
        .into_iter()
        .flat_map(|(_, session)| session.allocation_bases());
    let claude_sessions = state
        .app_handle
        .try_state::<Arc<crate::claude_session::manager::SessionManager>>()
        .map(|sm| sm.active_claude_sessions())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(_, session)| session.allocation_bases());
    terminals.chain(claude_sessions).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshotsResponse {
    pub session_id: String,
    pub snapshots: Vec<SnapshotRow>,
}

async fn get_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<GetSnapshotsResponse>, (StatusCode, String)> {
    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(GetSnapshotsResponse {
        session_id,
        snapshots,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RewindSessionRequest {
    /// Reserved for future use (e.g. dry-run, scope-by-path). The body
    /// is currently empty `{}` per the slash command's contract; we
    /// accept extra fields liberally.
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindError {
    pub file_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindSessionResponse {
    pub session_id: String,
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<RewindError>,
}

/// Compute the sha256 of `path`'s contents as a lowercase hex string.
/// Returns `None` if the file cannot be read. Streamed, so a large snapshot
/// blob costs a fixed buffer rather than its own size.
fn sha256_of_file(path: &Path) -> Option<String> {
    // `sha256_stream` also reports the length it hashed; the rewind path only
    // verifies the digest, so the length is dropped here.
    sha256_stream(std::fs::File::open(path).ok()?)
        .ok()
        .map(|(sha, _len)| sha)
}

async fn rewind_session_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<RewindSessionResponse>, (StatusCode, String)> {
    let _ = body; // body fields are reserved for future use

    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut files_restored = 0usize;
    let mut files_skipped = 0usize;
    let mut errors: Vec<RewindError> = Vec::new();

    // Only the FIRST captured-before snapshot per file_path is the
    // rollback target. Subsequent rows are informational. Walk in
    // taken_at ASC order (the SELECT already does this) and skip a
    // file_path once we've handled it.
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for snap in &snapshots {
        if !snap.captured_before {
            continue;
        }
        if !handled.insert(snap.file_path.clone()) {
            // already restored from the first snapshot for this path
            files_skipped += 1;
            continue;
        }

        let blob_path = Path::new(&snap.snapshot_blob_path);
        if !blob_path.exists() {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!("blob missing: {}", snap.snapshot_blob_path),
            });
            continue;
        }

        let actual_sha = match sha256_of_file(blob_path) {
            Some(h) => h,
            None => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("blob unreadable: {}", snap.snapshot_blob_path),
                });
                continue;
            }
        };
        if actual_sha != snap.blob_sha256 {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!(
                    "blob sha256 mismatch: stored={}, actual={}",
                    snap.blob_sha256, actual_sha
                ),
            });
            continue;
        }

        // Ensure the destination's parent dir exists (the file may
        // have been deleted by the failed worker).
        if let Some(parent) = Path::new(&snap.file_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("create parent dir failed: {}", e),
                });
                continue;
            }
        }

        match std::fs::copy(blob_path, Path::new(&snap.file_path)) {
            Ok(_) => {
                files_restored += 1;
                info!(
                    "rewind_session: restored {} from blob {} (session={})",
                    snap.file_path, snap.snapshot_blob_path, session_id
                );
            }
            Err(e) => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("copy failed: {}", e),
                });
            }
        }
    }

    if !errors.is_empty() {
        warn!(
            "rewind_session: {} restored, {} errored, session={}",
            files_restored,
            errors.len(),
            session_id
        );
    }

    Ok(Json(RewindSessionResponse {
        session_id,
        files_restored,
        files_skipped,
        errors,
    }))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Avoid the v0.7-syntax false positive on `{name}` captures, mirroring
        // the workaround in `mcp/ai_session.rs::routes`.
        .without_v07_checks()
        .route(
            "/sessions/{session_id}/snapshots",
            get(get_snapshots_handler),
        )
        .route(
            "/sessions/{session_id}/rewind",
            post(rewind_session_handler),
        )
        .route(
            "/sessions/{session_id}/file-changes",
            get(file_changes_handler),
        )
}

#[cfg(test)]
mod file_changes_tests {
    use super::*;
    use std::collections::HashMap;

    fn snap(path: &str, blob: &str, sha: &str, before: bool) -> SnapshotRow {
        SnapshotRow {
            id: format!("id-{path}"),
            session_id: "s1".to_string(),
            file_path: path.to_string(),
            snapshot_blob_path: blob.to_string(),
            blob_sha256: sha.to_string(),
            captured_before: before,
            taken_at: "2026-09-15T00:00:00Z".to_string(),
        }
    }

    /// In-memory tree that RECORDS every bounded read — the path, the cap it
    /// was asked for, and how many bytes it handed back — so a test can assert
    /// what the production code actually pulled into memory rather than trust
    /// it. `read_capped` honours the cap exactly as `DiskFiles` does.
    struct FakeFs {
        files: HashMap<String, Vec<u8>>,
        reads: std::cell::RefCell<Vec<(String, usize, usize)>>,
    }

    impl FakeFs {
        fn new(entries: &[(&str, &[u8])]) -> Self {
            Self {
                files: entries
                    .iter()
                    .map(|(p, b)| (p.to_string(), b.to_vec()))
                    .collect(),
                reads: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn get(&self, path: &str) -> std::io::Result<&Vec<u8>> {
            match self.files.get(path) {
                Some(b) => Ok(b),
                None if path.starts_with("EIO:") => Err(std::io::Error::other("disk on fire")),
                None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            }
        }
        /// Paths whose read came back WITHIN its cap — the only case the
        /// production code buffers as text.
        ///
        /// Deliberately not "n == the file's length": a file of exactly
        /// `cap + 1` bytes returns its whole length and is still over the cap,
        /// so length equality would call the very file the bound exists to stop
        /// "fully read".
        fn fully_buffered(&self) -> Vec<String> {
            self.reads
                .borrow()
                .iter()
                .filter(|(_, cap, n)| n <= cap)
                .map(|(p, _, _)| p.clone())
                .collect()
        }
        /// The largest number of bytes any single read handed back. The whole
        /// point of the `take`-bounded read is that this stays tiny however
        /// large the tree is.
        fn largest_read(&self) -> usize {
            self.reads
                .borrow()
                .iter()
                .map(|(_, _, n)| *n)
                .max()
                .unwrap_or(0)
        }
        /// Every read honoured its cap: no call returned more than `cap + 1`.
        fn every_read_respected_its_cap(&self) -> bool {
            self.reads.borrow().iter().all(|(_, cap, n)| *n <= cap + 1)
        }
    }

    impl FileProbe for FakeFs {
        fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
            let body = self.get(path)?;
            let take = body.len().min(cap.saturating_add(1));
            let bytes = body[..take].to_vec();
            self.reads
                .borrow_mut()
                .push((path.to_string(), cap, bytes.len()));
            Ok(bytes)
        }
        fn digest(&self, path: &str) -> std::io::Result<(String, u64)> {
            sha256_stream(self.get(path)?.as_slice())
        }
    }

    /// In-memory git base: `blobs` are the paths the base HAS, every other
    /// path is positively absent, and `unavailable` paths have no resolvable
    /// base at all. Records every cap it is handed.
    struct FakeBase {
        blobs: HashMap<String, Vec<u8>>,
        unavailable: HashMap<String, String>,
        /// Paths the base HAS but will not read.
        unread: HashMap<String, String>,
        handed_caps: Vec<usize>,
    }

    impl FakeBase {
        fn empty() -> Self {
            Self::with(&[])
        }
        fn with(blobs: &[(&str, &[u8])]) -> Self {
            Self {
                blobs: blobs
                    .iter()
                    .map(|(p, b)| (p.to_string(), b.to_vec()))
                    .collect(),
                unavailable: HashMap::new(),
                unread: HashMap::new(),
                handed_caps: Vec::new(),
            }
        }
        fn base() -> ResolvedBase {
            ResolvedBase {
                kind: BaseKind::Head,
                sha: "b".repeat(40),
            }
        }
    }

    impl BaseProbe for FakeBase {
        fn base_blob(
            &mut self,
            path: &str,
            cap: usize,
        ) -> Result<(ResolvedBase, BaseBlob), BaseError> {
            self.handed_caps.push(cap);
            if let Some(why) = self.unavailable.get(path) {
                return Err(BaseError::NoBase(why.clone()));
            }
            if let Some(why) = self.unread.get(path) {
                return Err(BaseError::EntryUnread {
                    base: Self::base(),
                    why: why.clone(),
                });
            }
            let blob = match self.blobs.get(path) {
                None => BaseBlob::Absent,
                Some(b) if b.len() <= cap => BaseBlob::Text(b.clone()),
                Some(b) => BaseBlob::Oversize {
                    bytes: b.len(),
                    sha256: sha256_hex(b),
                },
            };
            Ok((Self::base(), blob))
        }
    }

    fn fs(entries: &[(&str, &[u8])]) -> FakeFs {
        FakeFs::new(entries)
    }

    /// The bounds are exercised by their own tests; everywhere else they must
    /// not interfere.
    const NO_FILE_CAP: usize = usize::MAX;
    const NO_TEXT_BUDGET: usize = usize::MAX;

    /// `assemble_file_changes` with both bounds wide open.
    fn assemble(
        snapshots: &[SnapshotRow],
        touched: &[String],
        files: &dyn FileProbe,
    ) -> AssembledFileChanges {
        assemble_file_changes(
            snapshots,
            touched,
            files,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            NO_TEXT_BUDGET,
        )
    }

    #[test]
    fn modified_file_carries_both_sides_and_shas() {
        let before = b"a\nb\n";
        let sha = sha256_hex(before);
        let read = fs(&[("/blob/1", before), ("/src/x.rs", b"a\nc\n")]);
        let out = assemble(&[snap("/src/x.rs", "/blob/1", &sha, true)], &[], &read).files;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, "modified");
        assert_eq!(out[0].before.as_deref(), Some("a\nb\n"));
        assert_eq!(out[0].after.as_deref(), Some("a\nc\n"));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha.as_str()));
        assert!(!out[0].truncated);
        assert_eq!(out[0].taken_at.as_deref(), Some("2026-09-15T00:00:00Z"));
    }

    #[test]
    fn unchanged_deleted_and_created_are_told_apart() {
        let text = b"same\n";
        let sha = sha256_hex(text);
        let read = fs(&[
            ("/blob/same", text),
            ("/src/same.rs", text),
            ("/blob/gone", text),
            ("/src/new.rs", b"fresh\n"),
        ]);
        let snaps = [
            snap("/src/same.rs", "/blob/same", &sha, true),
            snap("/src/gone.rs", "/blob/gone", &sha, true),
        ];
        let touched = [
            "/src/same.rs".to_string(),
            "/src/new.rs".to_string(),
            "/src/never-landed.rs".to_string(),
        ];
        let out = assemble(&snaps, &touched, &read).files;
        let by_path: HashMap<_, _> = out.iter().map(|c| (c.file_path.as_str(), c)).collect();
        assert_eq!(by_path["/src/same.rs"].status, "unchanged");
        assert_eq!(by_path["/src/gone.rs"].status, "deleted");
        assert_eq!(by_path["/src/gone.rs"].after, None);
        assert_eq!(by_path["/src/new.rs"].status, "created");
        assert_eq!(by_path["/src/new.rs"].before, None);
        assert_eq!(by_path["/src/new.rs"].after.as_deref(), Some("fresh\n"));
        // A touched path that exists on neither side is not a change.
        assert!(!by_path.contains_key("/src/never-landed.rs"));
        // Snapshot rows come first, in row order; touched-only paths follow.
        assert_eq!(out[0].file_path, "/src/same.rs");
        assert_eq!(out[1].file_path, "/src/gone.rs");
        assert_eq!(out[2].file_path, "/src/new.rs");
    }

    #[test]
    fn a_failed_read_is_reported_not_dropped() {
        let sha = sha256_hex(b"x");
        let read = fs(&[("/blob/ok", b"x"), ("/blob/x", b"x")]);
        let snaps = [
            // blob missing on disk
            snap("/src/a.rs", "/blob/missing", &sha, true),
            // current file unreadable (not ENOENT)
            snap("EIO:/src/b.rs", "/blob/ok", &sha, true),
            // recorded sha disagrees with the blob's bytes
            snap("/src/c.rs", "/blob/x", "deadbeef", true),
        ];
        let out = assemble(&snaps, &[], &read).files;
        assert_eq!(out.len(), 3);
        for c in &out {
            assert_eq!(c.status, "unreadable", "{c:?}");
            assert!(c.detail.is_some(), "{c:?}");
            assert_eq!(c.before, None);
            assert_eq!(c.after, None);
        }
        assert!(out[0].detail.as_deref().unwrap().contains("missing"));
        assert!(out[1].detail.as_deref().unwrap().contains("disk on fire"));
        assert!(out[2].detail.as_deref().unwrap().contains("mismatch"));
    }

    #[test]
    fn binary_and_oversized_sides_carry_no_text() {
        let bin = [0xff_u8, 0xfe, 0x00];
        let sha_bin = sha256_hex(&bin);
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let sha_big = sha256_hex(&big);
        let read = fs(&[
            ("/blob/bin", &bin),
            ("/src/bin", b"text now"),
            ("/blob/big", &big),
            ("/src/big", b"small now"),
        ]);
        let snaps = [
            snap("/src/bin", "/blob/bin", &sha_bin, true),
            snap("/src/big", "/blob/big", &sha_big, true),
        ];
        let out = assemble(&snaps, &[], &read).files;
        assert_eq!(out[0].status, "binary");
        assert_eq!(out[0].before, None);
        assert_eq!(out[1].status, "modified");
        assert!(out[1].truncated);
        assert_eq!(out[1].before, None);
        assert_eq!(out[1].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES + 1));
    }

    /// The availability fix: an over-cap side is never pulled into memory.
    /// Before this, every side was read whole and the cap only suppressed the
    /// text afterwards — so one multi-GB generated artifact in a worker's
    /// touched set could OOM the runner, a tier-0 process, through an
    /// unauthenticated loopback route.
    #[test]
    fn an_over_cap_side_is_never_read_into_memory() {
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let sha_big = sha256_hex(&big);
        let small = b"small now";
        let read = fs(&[("/blob/big", &big), ("/src/big", small)]);
        let out = assemble(&[snap("/src/big", "/blob/big", &sha_big, true)], &[], &read).files;

        // Only the small side came back WHOLE; the oversize blob was probed to
        // `cap + 1` bytes and no further.
        assert_eq!(read.fully_buffered(), vec!["/src/big".to_string()]);
        assert!(read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1);
        // It is still fully described: size, digest, and an honest `truncated`.
        assert_eq!(out[0].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES + 1));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha_big.as_str()));
        assert_eq!(out[0].after_bytes, Some(small.len()));
        assert!(out[0].truncated);
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].after, None);
        assert_eq!(out[0].status, "modified");
        // Cut by its own size, so no budget claim.
        assert_eq!(out[0].detail, None);
    }

    /// The TOCTOU fix: the cap is enforced by the READ, not by a stat taken
    /// before it.
    ///
    /// The old `read_side` stat'd, decided the file was under the cap, then
    /// called `std::fs::read` — the whole file, at whatever size it had by
    /// then. A worker appending to a generated artifact or a log between those
    /// two syscalls got that file read whole into a tier-0 process. Here the
    /// tree holds a body 16× the cap: whatever any stat might have said, the
    /// bound is what `read_capped` hands back, so the entry is `truncated`
    /// with a streamed digest and NOTHING near the body's size is ever
    /// resident.
    ///
    /// Note the bound is now structural as well as tested: [`FileProbe`] has no
    /// unbounded read to call, so restoring the old stat-then-read shape does
    /// not fail this assertion — it fails to compile.
    #[test]
    fn a_side_far_over_the_cap_is_bounded_by_the_read_itself() {
        let huge = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES * 16];
        let sha_huge = sha256_hex(&huge);
        let read = fs(&[("/blob/huge", &huge), ("/src/huge", b"now\n")]);
        let out = assemble(
            &[snap("/src/huge", "/blob/huge", &sha_huge, true)],
            &[],
            &read,
        )
        .files;

        assert!(read.every_read_respected_its_cap());
        assert!(
            read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1,
            "largest read was {} bytes for a {}-byte file",
            read.largest_read(),
            huge.len()
        );
        assert!(out[0].truncated);
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].before_bytes, Some(huge.len()));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha_huge.as_str()));
    }

    /// The streamed digest of an over-cap side is still checked against the
    /// recorded one, so a corrupt huge blob is `unreadable`, not `modified`.
    #[test]
    fn an_over_cap_snapshot_blob_still_fails_its_sha_check() {
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let read = fs(&[("/blob/big", &big), ("/src/big", b"now")]);
        let out = assemble(
            &[snap("/src/big", "/blob/big", "deadbeef", true)],
            &[],
            &read,
        )
        .files;
        assert_eq!(out[0].status, "unreadable");
        assert!(out[0].detail.as_deref().unwrap().contains("mismatch"));
        // The digest that failed the check was STREAMED: the huge blob's body
        // was never pulled in whole. (The under-cap current file is.)
        assert!(!read.fully_buffered().contains(&"/blob/big".to_string()));
        assert!(read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1);
    }

    /// The file-count bound: the report stops at `max_files` candidates, says
    /// how many it dropped, and does no filesystem work for them at all.
    #[test]
    fn the_file_count_is_capped_and_the_cut_is_reported() {
        let entries: Vec<(String, Vec<u8>)> = (0..10)
            .map(|i| (format!("/src/f{i}"), b"body".to_vec()))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = entries.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(
            &[],
            &touched,
            &read,
            &mut FakeBase::empty(),
            3,
            NO_TEXT_BUDGET,
        );
        assert_eq!(assembled.files.len(), 3);
        assert_eq!(assembled.omitted_files, 7);
        // Order is preserved: the cut is a suffix, not an arbitrary subset.
        assert_eq!(assembled.files[0].file_path, "/src/f0");
        assert_eq!(assembled.files[2].file_path, "/src/f2");
        // Nothing beyond the bound was even opened.
        assert_eq!(read.fully_buffered().len(), 3);

        // Under the bound, nothing is reported as omitted.
        let all = assemble_file_changes(
            &[],
            &touched,
            &fs(&borrowed),
            &mut FakeBase::empty(),
            10,
            NO_TEXT_BUDGET,
        );
        assert_eq!(all.files.len(), 10);
        assert_eq!(all.omitted_files, 0);
    }

    /// The aggregate-bytes fix. The per-side cap and the file-count cap each
    /// bound one axis; nothing bounded their PRODUCT, so 400 files just under
    /// the per-side cap was ~200 MiB resident plus a comparable serialisation
    /// buffer — for one unauthenticated loopback request, with no concurrency
    /// limit. The realistic shape is the one tested here: many ordinary files,
    /// each individually fine.
    ///
    /// The budget does not DROP files. Every candidate is still reported, with
    /// status, sizes and digests; only the bodies stop.
    #[test]
    fn the_total_text_budget_bounds_the_whole_report() {
        // 20 files of 1 KiB each = 20 KiB of text, against a 4 KiB budget.
        let bodies: Vec<(String, Vec<u8>)> = (0..20)
            .map(|i| (format!("/src/f{i:02}"), vec![b'x'; 1024]))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = bodies
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = bodies.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(
            &[],
            &touched,
            &read,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            4 * 1024,
        );

        // Nothing was dropped: the file-count bound is a different bound.
        assert_eq!(assembled.files.len(), 20);
        assert_eq!(assembled.omitted_files, 0);

        // The text the response holds is inside the budget.
        let held: usize = assembled
            .files
            .iter()
            .map(|c| {
                c.before.as_ref().map_or(0, |s| s.len()) + c.after.as_ref().map_or(0, |s| s.len())
            })
            .sum();
        assert!(
            held <= 4 * 1024,
            "held {held} bytes against a 4096-byte budget"
        );

        // The first few carry text; the rest are truncated but fully described.
        assert!(assembled.files[0].after.is_some());
        assert!(!assembled.files[0].truncated);
        let tail = &assembled.files[19];
        assert!(tail.truncated);
        assert_eq!(tail.after, None);
        assert_eq!(tail.after_bytes, Some(1024));
        assert!(tail.after_sha256.is_some());
        assert_eq!(tail.status, "created");
        // Honesty: a 1 KiB file is not "too large to diff". The entry names the
        // bound that actually applied, and the UI renders that `detail`.
        assert!(
            tail.detail.as_deref().unwrap().contains("budget"),
            "{:?}",
            tail.detail
        );
        // And it is the BUDGET, not the per-side cap, so the per-file detail
        // must not appear on an entry read while the budget was still wide.
        assert_eq!(assembled.files[0].detail, None);
    }

    /// The budget shrinks the cap handed to the read, so an over-budget side is
    /// never buffered in the first place — the bound is not a post-hoc trim of
    /// an already-allocated list.
    #[test]
    fn an_over_budget_side_is_not_read_into_memory_and_then_discarded() {
        let bodies: Vec<(String, Vec<u8>)> = (0..6)
            .map(|i| (format!("/src/g{i}"), vec![b'y'; 1024]))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = bodies
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = bodies.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(
            &[],
            &touched,
            &read,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            2048,
        );

        // Two files fit; the remaining four were probed to their (zero) cap and
        // no further, so only two whole bodies were ever resident.
        assert_eq!(read.fully_buffered().len(), 2);
        assert!(read.every_read_respected_its_cap());
        assert_eq!(assembled.files.iter().filter(|c| c.truncated).count(), 4);
    }

    /// The truncation REASON is decided per side from that side's own length,
    /// not from a flag sampled before the entry's reads.
    ///
    /// The budget shrinks between the two sides of one entry, so a per-entry
    /// flag is wrong exactly at the boundary — which is the entry most likely
    /// to be truncated. Here the `before` side (256 KiB, within its own cap)
    /// leaves too little budget for a 60 KiB `after`: the UI must be told the
    /// BUDGET cut it, or `noDiffReason` renders "too large to diff" about a
    /// 60 KiB file against a 256 KiB cap.
    #[test]
    fn a_side_cut_at_the_budget_boundary_names_the_budget_not_its_size() {
        let before = vec![b'b'; FILE_CHANGE_TEXT_CAP_BYTES];
        let after = vec![b'a'; 60 * 1024];
        let sha_before = sha256_hex(&before);
        let read = fs(&[("/blob/x", &before), ("/src/x", &after)]);

        let out = assemble_file_changes(
            &[snap("/src/x", "/blob/x", &sha_before, true)],
            &[],
            &read,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            300 * 1024, // wide open at the top of the loop, spent by the before side
        )
        .files;

        assert!(out[0].truncated);
        assert_eq!(out[0].after_bytes, Some(60 * 1024));
        assert!(
            out[0].detail.as_deref().unwrap_or("").contains("budget"),
            "a 60 KiB side cut by the budget claimed the per-side cap: {:?}",
            out[0].detail
        );
    }

    /// The converse: a genuinely over-cap side says nothing about the budget,
    /// even when the budget happens to be low. "Too large to diff" is the true
    /// statement about that entry and the UI supplies it.
    #[test]
    fn a_genuinely_over_cap_side_never_blames_the_budget() {
        let huge = vec![b'h'; FILE_CHANGE_TEXT_CAP_BYTES * 4];
        let sha_huge = sha256_hex(&huge);
        let read = fs(&[("/blob/h", &huge), ("/src/h", b"now\n")]);

        // Budget deliberately smaller than the per-side cap, which is the case
        // an entry-level flag got wrong on the very FIRST candidate.
        let out = assemble_file_changes(
            &[snap("/src/h", "/blob/h", &sha_huge, true)],
            &[],
            &read,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            4 * 1024,
        )
        .files;

        assert!(out[0].truncated);
        assert_eq!(out[0].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES * 4));
        assert_eq!(
            out[0].detail, None,
            "blamed the budget for an over-cap side"
        );
    }

    /// A probe whose file is OVER the cap at the bounded read and SMALLER at
    /// the digest pass — the concurrent-writer case in the shrinking
    /// direction. It also records every cap it was handed.
    struct ShrinkingFile {
        handed_caps: std::cell::RefCell<Vec<usize>>,
        shrunk_to: Vec<u8>,
    }

    impl FileProbe for ShrinkingFile {
        fn read_capped(&self, _path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
            self.handed_caps.borrow_mut().push(cap);
            // Always over whatever cap it was handed: the read is what decides.
            Ok(vec![b'w'; cap.saturating_add(1)])
        }
        fn digest(&self, _path: &str) -> std::io::Result<(String, u64)> {
            Ok((sha256_hex(&self.shrunk_to), self.shrunk_to.len() as u64))
        }
    }

    /// Attribution reads the cap handed to the READ, not the length reported by
    /// the digest pass.
    ///
    /// The two passes are separate observations of the same file. A file that
    /// shrinks between them comes back under the per-side cap, so a rule keyed
    /// on that length blames the report's aggregate budget — here on the FIRST
    /// candidate, with the budget wide open and nothing yet spent. The `detail`
    /// exists to stop false statements; that would be one.
    #[test]
    fn a_side_that_shrinks_between_the_two_passes_does_not_blame_an_untouched_budget() {
        let probe = ShrinkingFile {
            handed_caps: std::cell::RefCell::new(Vec::new()),
            shrunk_to: vec![b'w'; 2048],
        };

        let out = assemble_file_changes(
            &[],
            &["/src/shrinks".to_string()],
            &probe,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            NO_TEXT_BUDGET,
        )
        .files;

        // The read really was handed the full per-side cap: nothing had been
        // spent, so the budget cannot be what refused it.
        assert_eq!(
            probe.handed_caps.borrow().as_slice(),
            &[FILE_CHANGE_TEXT_CAP_BYTES]
        );
        assert!(out[0].truncated);
        assert_eq!(out[0].after_bytes, Some(2048));
        assert_eq!(
            out[0].detail, None,
            "blamed an untouched budget for a file that shrank between the bounded read and the digest pass"
        );
    }

    /// Every other test in this module drives [`FakeFs`], whose `read_capped`
    /// honours the cap BY CONSTRUCTION — so they pin what
    /// `assemble_file_changes` asks for, not what the production probe does.
    /// Dropping `.take(..)` from [`DiskFiles::read_capped`] compiles, satisfies
    /// the trait and fails none of them. These three exercise the real
    /// filesystem, which is where the bound actually has to hold.
    #[test]
    fn disk_files_read_capped_stops_at_the_cap_on_a_real_filesystem() {
        const CAP: usize = 64;
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |name: &str, len: usize| -> String {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![b'q'; len]).expect("write");
            path.to_string_lossy().into_owned()
        };

        // Under the cap and exactly AT it are whole files: the returned length
        // is the file's own, and `read_side` classifies them as text.
        assert_eq!(
            DiskFiles
                .read_capped(&write("under", CAP - 1), CAP)
                .unwrap()
                .len(),
            CAP - 1
        );
        assert_eq!(
            DiskFiles
                .read_capped(&write("exact", CAP), CAP)
                .unwrap()
                .len(),
            CAP
        );
        // `cap + 1` is the VERDICT, and it is the ONLY over-cap signal — which
        // is why the assertion is on the returned LENGTH. A file one byte over
        // and a file forty times the cap must be indistinguishable here, or the
        // bound is not a property of the read.
        assert_eq!(
            DiskFiles
                .read_capped(&write("over", CAP + 1), CAP)
                .unwrap()
                .len(),
            CAP + 1
        );
        assert_eq!(
            DiskFiles
                .read_capped(&write("huge", CAP * 40), CAP)
                .unwrap()
                .len(),
            CAP + 1
        );
        // A zero cap (aggregate budget spent) still probes exactly one byte, so
        // a non-empty file is oversize and an empty one is text.
        assert_eq!(DiskFiles.read_capped(&write("z", 10), 0).unwrap().len(), 1);
        assert_eq!(
            DiskFiles.read_capped(&write("empty", 0), 0).unwrap().len(),
            0
        );
        // The error kind `read_side` branches on to report `Missing`.
        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(
            DiskFiles.read_capped(&missing, CAP).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn disk_files_digest_reports_the_files_own_length_and_sha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = vec![b'd'; 5000];
        let path = dir.path().join("body");
        std::fs::write(&path, &body).expect("write");
        let path = path.to_string_lossy().into_owned();

        // The two halves must agree with each other AND with the bytes on
        // disk: `Side::Oversize` reports a file it never buffered, so this pair
        // is the only description the operator gets of it.
        let (sha, len) = DiskFiles.digest(&path).expect("digest");
        assert_eq!(len, body.len() as u64);
        assert_eq!(sha, sha256_hex(&body));

        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").expect("write");
        let (sha, len) = DiskFiles.digest(&empty.to_string_lossy()).expect("digest");
        assert_eq!(len, 0);
        assert_eq!(sha, sha256_hex(b""));

        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(
            DiskFiles.digest(&missing).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    /// `FileProbe` is public and `usize::MAX` is a cap a caller can hand it —
    /// the tests above pass it as a budget. A wrapping `cap as u64 + 1` is a
    /// debug panic, and in release a `take(0)`: the file comes back empty and
    /// `bytes.len() <= cap` classifies it as EMPTY TEXT. Confidently wrong is
    /// the one answer this route must not give.
    #[test]
    fn disk_files_read_capped_survives_a_usize_max_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("whole");
        std::fs::write(&path, b"whole file\n").expect("write");
        let got = DiskFiles
            .read_capped(&path.to_string_lossy(), usize::MAX)
            .expect("read");
        assert_eq!(got, b"whole file\n");
    }

    /// Bytes read but NOT kept are refunded: a binary file's bytes are dropped
    /// by `pair_sides`, so they must not eat the budget the text files need.
    #[test]
    fn bytes_that_never_reach_the_response_do_not_spend_the_budget() {
        let bin = vec![0xff_u8; 1024];
        let text = vec![b'z'; 1024];
        let read = fs(&[("/src/a.bin", &bin), ("/src/b.txt", &text)]);
        let touched = ["/src/a.bin".to_string(), "/src/b.txt".to_string()];

        // 1200 bytes: enough for ONE 1 KiB body. The binary one is read first
        // and discarded, so the text one must still fit.
        let assembled = assemble_file_changes(
            &[],
            &touched,
            &read,
            &mut FakeBase::empty(),
            NO_FILE_CAP,
            1200,
        );
        assert_eq!(assembled.files[0].status, "binary");
        assert_eq!(assembled.files[0].after, None);
        assert_eq!(assembled.files[1].status, "created");
        assert!(
            assembled.files[1].after.is_some(),
            "the binary file's discarded bytes spent the budget: {:?}",
            assembled.files[1]
        );
    }

    #[test]
    fn only_the_first_pre_edit_snapshot_per_path_counts() {
        let first = b"first\n";
        let second = b"second\n";
        let read = fs(&[
            ("/blob/first", first),
            ("/blob/second", second),
            ("/src/x", b"now\n"),
        ]);
        let snaps = [
            snap("/src/x", "/blob/first", &sha256_hex(first), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), false),
        ];
        let out = assemble(&snaps, &["/src/x".to_string()], &read).files;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].before.as_deref(), Some("first\n"));
    }

    // ── git-base "before" side (snapshot-less paths) ─────────────────────────

    /// A snapshot-less path the base HAS is a modification against the base,
    /// and one the base has but the disk no longer does is a deletion — the
    /// two cases that used to read `created` and vanish respectively.
    #[test]
    fn a_snapshot_less_path_diffs_against_the_git_base() {
        let read = fs(&[("/src/m.rs", b"new\n"), ("/src/c.rs", b"fresh\n")]);
        let mut base = FakeBase::with(&[("/src/m.rs", b"old\n"), ("/src/d.rs", b"gone\n")]);
        let touched = [
            "/src/m.rs".to_string(),
            "/src/d.rs".to_string(),
            "/src/c.rs".to_string(),
            "/src/never.rs".to_string(),
        ];
        let out =
            assemble_file_changes(&[], &touched, &read, &mut base, NO_FILE_CAP, NO_TEXT_BUDGET);
        let by_path: HashMap<_, _> = out
            .files
            .iter()
            .map(|c| (c.file_path.as_str(), c))
            .collect();

        let m = by_path["/src/m.rs"];
        assert_eq!(m.status, "modified");
        assert_eq!(m.before_source, BeforeSource::GitBase);
        assert_eq!(m.base_kind, Some(BaseKind::Head));
        assert_eq!(m.base_sha.as_deref(), Some(FakeBase::base().sha.as_str()));
        assert_eq!(m.before.as_deref(), Some("old\n"));
        assert_eq!(m.after.as_deref(), Some("new\n"));
        assert_eq!(m.taken_at, None);

        assert_eq!(by_path["/src/d.rs"].status, "deleted");
        assert_eq!(by_path["/src/d.rs"].before.as_deref(), Some("gone\n"));
        // Positively absent at the base: the one case that is `created`.
        assert_eq!(by_path["/src/c.rs"].status, "created");
        assert_eq!(by_path["/src/c.rs"].before_source, BeforeSource::GitBase);
        // On neither side: not a change.
        assert!(!by_path.contains_key("/src/never.rs"));
        assert_eq!(out.shared_base(), Some(FakeBase::base()));
    }

    /// No resolvable base is UNKNOWN, never a creation. A path that does not
    /// exist now either is omitted: nothing positively says it ever did.
    #[test]
    fn a_path_with_no_git_base_is_unreadable_never_created() {
        let read = fs(&[("/src/x.rs", b"now\n")]);
        let mut base = FakeBase::empty();
        base.unavailable.insert(
            "/src/x.rs".to_string(),
            "not in an openable git repository".to_string(),
        );
        base.unavailable.insert(
            "/src/gone.rs".to_string(),
            "not in an openable git repository".to_string(),
        );
        let touched = ["/src/x.rs".to_string(), "/src/gone.rs".to_string()];
        let out =
            assemble_file_changes(&[], &touched, &read, &mut base, NO_FILE_CAP, NO_TEXT_BUDGET);

        assert_eq!(out.files.len(), 1, "{:?}", out.files);
        let x = &out.files[0];
        assert_eq!(x.status, "unreadable");
        assert_eq!(x.before_source, BeforeSource::None);
        assert_eq!(x.base_kind, None);
        assert_eq!((x.before.as_deref(), x.after.as_deref()), (None, None));
        assert!(
            x.detail
                .as_deref()
                .unwrap()
                .contains("openable git repository"),
            "{:?}",
            x.detail
        );
        assert_eq!(out.shared_base(), None);
    }

    /// "Base has an entry I won't read" is not "no base": a gone file is still
    /// reported, where a gone file with no base at all is omitted.
    #[test]
    fn an_unread_base_entry_survives_the_file_being_gone() {
        let read = fs(&[]);
        let mut base = FakeBase::empty();
        base.unread
            .insert("/src/gone.rs".to_string(), "a directory there".to_string());
        base.unavailable
            .insert("/src/never.rs".to_string(), "no repository".to_string());
        let touched = ["/src/gone.rs".to_string(), "/src/never.rs".to_string()];
        let out =
            assemble_file_changes(&[], &touched, &read, &mut base, NO_FILE_CAP, NO_TEXT_BUDGET);

        assert_eq!(out.files.len(), 1, "{:?}", out.files);
        let c = &out.files[0];
        assert_eq!(c.file_path, "/src/gone.rs");
        assert_eq!(c.status, "unreadable");
        assert_eq!(c.before_source, BeforeSource::GitBase);
        assert_eq!(c.base_kind, Some(BaseKind::Head));
        assert!(c.detail.as_deref().unwrap().contains("a directory there"));
    }

    /// A snapshot still wins over the git base, and says so.
    #[test]
    fn a_snapshot_is_preferred_over_the_git_base() {
        let snap_text = b"snapshot\n";
        let read = fs(&[("/blob/s", snap_text), ("/src/s.rs", b"now\n")]);
        let mut base = FakeBase::with(&[("/src/s.rs", b"base\n")]);
        let out = assemble_file_changes(
            &[snap("/src/s.rs", "/blob/s", &sha256_hex(snap_text), true)],
            &["/src/s.rs".to_string()],
            &read,
            &mut base,
            NO_FILE_CAP,
            NO_TEXT_BUDGET,
        );
        assert_eq!(out.files[0].before_source, BeforeSource::Snapshot);
        assert_eq!(out.files[0].before.as_deref(), Some("snapshot\n"));
        assert_eq!(out.files[0].base_kind, None);
        assert!(
            base.handed_caps.is_empty(),
            "the base was consulted for a snapshotted path"
        );
        assert_eq!(out.shared_base(), None);
    }

    /// Base reads spend the SAME aggregate budget as every other side: the cap
    /// handed to the base shrinks as the report fills, the held text stays
    /// inside the budget, and an entry past it is truncated with the budget
    /// named — still `modified`, because presence at the base is decided by
    /// the tree lookup, not by the bytes.
    #[test]
    fn base_reads_are_counted_in_the_text_budget() {
        let paths: Vec<String> = (0..4).map(|i| format!("/src/b{i}")).collect();
        let before = vec![b'o'; 1024];
        let after = vec![b'n'; 1024];
        let disk: Vec<(&str, &[u8])> = paths
            .iter()
            .map(|p| (p.as_str(), after.as_slice()))
            .collect();
        let base_blobs: Vec<(&str, &[u8])> = paths
            .iter()
            .map(|p| (p.as_str(), before.as_slice()))
            .collect();
        let read = fs(&disk);
        let mut base = FakeBase::with(&base_blobs);

        let out = assemble_file_changes(&[], &paths, &read, &mut base, NO_FILE_CAP, 3 * 1024);

        let held: usize = out
            .files
            .iter()
            .map(|c| {
                c.before.as_ref().map_or(0, |s| s.len()) + c.after.as_ref().map_or(0, |s| s.len())
            })
            .sum();
        assert!(
            held <= 3 * 1024,
            "held {held} bytes against a 3072-byte budget"
        );
        // The first pair fit whole (1 KiB + 1 KiB), so the second base read was
        // handed only what was left.
        assert_eq!(base.handed_caps[0], 3 * 1024);
        assert_eq!(base.handed_caps[1], 1024);
        assert!(base.handed_caps.windows(2).all(|w| w[1] <= w[0]));
        let last = &out.files[3];
        assert!(last.truncated);
        assert_eq!(last.status, "modified");
        assert_eq!(last.before_source, BeforeSource::GitBase);
        assert_eq!(last.before_bytes, Some(1024));
        assert!(
            last.detail.as_deref().unwrap().contains("budget"),
            "{:?}",
            last.detail
        );
    }

    /// A base blob over the per-side cap is described by size and digest and
    /// never kept — the same honesty as an over-cap snapshot.
    #[test]
    fn an_over_cap_base_blob_is_reported_by_size_not_text() {
        let big = vec![b'g'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let read = fs(&[("/src/big", b"small now")]);
        let mut base = FakeBase::with(&[("/src/big", &big)]);
        let out = assemble_file_changes(
            &[],
            &["/src/big".to_string()],
            &read,
            &mut base,
            NO_FILE_CAP,
            NO_TEXT_BUDGET,
        );
        let c = &out.files[0];
        assert_eq!(c.status, "modified");
        assert!(c.truncated);
        assert_eq!(c.before, None);
        assert_eq!(c.before_bytes, Some(big.len()));
        assert_eq!(c.before_sha256.as_deref(), Some(sha256_hex(&big).as_str()));
        assert_eq!(
            c.detail, None,
            "a genuinely over-cap base blob blamed the budget"
        );
    }

    // ── BaseBlobs against real repositories ──────────────────────────────────

    fn sig() -> git2::Signature<'static> {
        git2::Signature::now("t", "t@example.invalid").expect("signature")
    }

    /// A repository whose initial branch is `main`.
    fn init_repo(dir: &Path) -> git2::Repository {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).expect("init")
    }

    fn write(root: &Path, rel: &str, body: &[u8]) -> String {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, body).expect("write");
        path.to_string_lossy().into_owned()
    }

    /// Stage everything in the workdir and commit it onto HEAD.
    fn commit_all(repo: &git2::Repository, msg: &str) -> git2::Oid {
        let mut index = repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add_all");
        index.write().expect("index write");
        let tree = repo
            .find_tree(index.write_tree().expect("write_tree"))
            .expect("tree");
        let parents: Vec<git2::Commit<'_>> = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig(), &sig(), msg, &tree, &parent_refs)
            .expect("commit")
    }

    fn assemble_real(touched: &[String], base: &mut BaseBlobs) -> AssembledFileChanges {
        assemble_file_changes(
            &[],
            touched,
            &DiskFiles,
            base,
            FILE_CHANGE_MAX_FILES,
            FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES,
        )
    }

    #[test]
    fn base_blobs_report_a_modified_file_against_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        let path = write(dir.path(), "src/a.txt", b"one\n");
        let head = commit_all(&repo, "c1");
        write(dir.path(), "src/a.txt", b"two\n");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        let c = &out.files[0];
        assert_eq!(c.status, "modified", "{c:?}");
        assert_eq!(c.before_source, BeforeSource::GitBase);
        assert_eq!(c.base_kind, Some(BaseKind::Head));
        assert_eq!(c.base_sha.as_deref(), Some(head.to_string().as_str()));
        assert_eq!(c.before.as_deref(), Some("one\n"));
        assert_eq!(c.after.as_deref(), Some("two\n"));
    }

    #[test]
    fn base_blobs_report_a_created_file_only_when_the_base_lacks_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        let tracked = write(dir.path(), "kept.txt", b"same\n");
        commit_all(&repo, "c1");
        let fresh = write(dir.path(), "new/fresh.txt", b"hello\n");

        let out = assemble_real(&[tracked, fresh], &mut BaseBlobs::new([]));
        assert_eq!(out.files[0].status, "unchanged");
        assert_eq!(out.files[0].before_source, BeforeSource::GitBase);
        assert_eq!(out.files[1].status, "created");
        assert_eq!(out.files[1].before_source, BeforeSource::GitBase);
        assert_eq!(out.files[1].before, None);
        assert_eq!(out.files[1].after.as_deref(), Some("hello\n"));
    }

    /// A checkout with `eol=crlf` holds CRLF on disk while the object store
    /// holds LF. The base side is smudged first, so an untouched file reads
    /// `unchanged` and an edit diffs one line — not the whole file.
    #[test]
    fn a_crlf_checkout_compares_the_smudged_base_not_the_raw_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        write(dir.path(), ".gitattributes", b"*.txt eol=crlf\n");
        let same = write(dir.path(), "same.txt", b"one\ntwo\n");
        let edited = write(dir.path(), "edited.txt", b"one\ntwo\n");
        commit_all(&repo, "c1");
        // What a checkout of that commit leaves on disk.
        write(dir.path(), "same.txt", b"one\r\ntwo\r\n");
        write(dir.path(), "edited.txt", b"one\r\nTWO\r\n");

        let out = assemble_real(&[same, edited], &mut BaseBlobs::new([]));
        assert_eq!(out.files[0].status, "unchanged", "{:?}", out.files[0]);
        let c = &out.files[1];
        assert_eq!(c.status, "modified", "{c:?}");
        assert_eq!(c.before.as_deref(), Some("one\r\ntwo\r\n"));
        assert_eq!(c.after.as_deref(), Some("one\r\nTWO\r\n"));
    }

    /// `core.autocrlf=true` converts with no attribute naming the path.
    #[test]
    fn autocrlf_true_smudges_the_base_side() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        let path = write(dir.path(), "a.txt", b"x\ny\n");
        commit_all(&repo, "c1");
        repo.config()
            .expect("config")
            .set_str("core.autocrlf", "true")
            .expect("set autocrlf");
        write(dir.path(), "a.txt", b"x\r\ny\r\n");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        assert_eq!(out.files[0].status, "unchanged", "{:?}", out.files[0]);
    }

    /// A `filter=<driver>` path (LFS) cannot be smudged in-process, so it is
    /// `unreadable` rather than a pointer file diffed against real content.
    #[test]
    fn a_filter_driver_path_is_unreadable_not_diffed_in_its_clean_form() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        write(dir.path(), ".gitattributes", b"*.bin filter=lfs\n");
        let path = write(
            dir.path(),
            "big.bin",
            b"version https://git-lfs.github.com/spec/v1\n",
        );
        commit_all(&repo, "c1");
        write(dir.path(), "big.bin", b"the real content");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        let c = &out.files[0];
        assert_eq!(c.status, "unreadable", "{c:?}");
        assert_eq!(c.before_source, BeforeSource::GitBase);
        assert!(c.detail.as_deref().unwrap().contains("filter=lfs"), "{c:?}");
    }

    /// On a case-folding checkout a touch recorded as `README.md` names the
    /// tracked `Readme.md`: it must not read as a creation.
    #[test]
    fn an_ignorecase_checkout_finds_the_base_entry_in_any_case() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        write(dir.path(), "Docs/Readme.md", b"hello\n");
        commit_all(&repo, "c1");
        repo.config()
            .expect("config")
            .set_bool("core.ignorecase", true)
            .expect("set ignorecase");
        // This filesystem is case-sensitive, so the recorded spelling is
        // made to exist on disk to stand in for a folding one.
        std::fs::rename(dir.path().join("Docs"), dir.path().join("docs")).expect("rename dir");
        std::fs::rename(
            dir.path().join("docs/Readme.md"),
            dir.path().join("docs/README.md"),
        )
        .expect("rename file");
        let touched = dir
            .path()
            .join("docs/README.md")
            .to_string_lossy()
            .into_owned();

        let out = assemble_real(std::slice::from_ref(&touched), &mut BaseBlobs::new([]));
        assert_eq!(out.files[0].status, "unchanged", "{:?}", out.files[0]);

        // Without `core.ignorecase` the same lookup is case-sensitive.
        repo.config()
            .expect("config")
            .set_bool("core.ignorecase", false)
            .expect("unset ignorecase");
        let out = assemble_real(&[touched], &mut BaseBlobs::new([]));
        assert_eq!(out.files[0].status, "created", "{:?}", out.files[0]);
    }

    /// The base HAS a symlink at this path, and the session removed it: the
    /// entry is reported `unreadable` with its base, not dropped because the
    /// file is gone.
    #[cfg(unix)]
    #[test]
    fn a_base_entry_that_is_not_read_is_reported_even_when_the_file_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        write(dir.path(), "target.txt", b"t\n");
        std::os::unix::fs::symlink("target.txt", dir.path().join("link")).expect("symlink");
        let head = commit_all(&repo, "c1");
        std::fs::remove_file(dir.path().join("link")).expect("rm link");
        let link = dir.path().join("link").to_string_lossy().into_owned();

        let out = assemble_real(&[link], &mut BaseBlobs::new([]));
        assert_eq!(out.files.len(), 1, "{:?}", out.files);
        let c = &out.files[0];
        assert_eq!(c.status, "unreadable", "{c:?}");
        assert_eq!(c.before_source, BeforeSource::GitBase);
        assert_eq!(c.base_sha.as_deref(), Some(head.to_string().as_str()));
        assert!(c.detail.as_deref().unwrap().contains("symlink"), "{c:?}");
    }

    /// A `.git` that points nowhere is a repository git cannot open: the path
    /// is `unreadable` with the reason — it is not a creation.
    #[test]
    fn an_unopenable_repository_is_unreadable_not_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".git"),
            format!("gitdir: {}\n", dir.path().join("does-not-exist").display()),
        )
        .expect("write .git");
        let path = write(dir.path(), "x.txt", b"content\n");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        let c = &out.files[0];
        assert_eq!(c.status, "unreadable", "{c:?}");
        assert_eq!(c.before_source, BeforeSource::None);
        assert!(c.detail.is_some());
        assert_eq!(out.shared_base(), None);
    }

    /// A linked worktree on an agent branch: main at C1 (`origin/main` too),
    /// the allocation recorded at C2, the session committed C3 and has
    /// uncommitted work on disk. Rung 1 (the recorded `parent_sha`) beats
    /// rung 2 (merge-base), and without an allocation rung 2 answers — never
    /// `HEAD`, which would hide the committed C3 work.
    #[test]
    fn the_allocated_parent_sha_is_preferred_over_the_merge_base() {
        let root = tempfile::tempdir().expect("tempdir");
        let main_dir = root.path().join("main");
        std::fs::create_dir_all(&main_dir).expect("mkdir");
        let repo = init_repo(&main_dir);
        write(&main_dir, "f.txt", b"v1\n");
        let c1 = commit_all(&repo, "c1");
        repo.reference("refs/remotes/origin/main", c1, true, "test")
            .expect("origin/main");
        let branch = repo
            .branch("agent/x", &repo.find_commit(c1).expect("c1"), false)
            .expect("branch");

        let wt_dir = root.path().join("wt");
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        repo.worktree("wt", &wt_dir, Some(&opts))
            .expect("worktree add");
        let wt = git2::Repository::open(&wt_dir).expect("open worktree");
        write(&wt_dir, "f.txt", b"v2\n");
        let c2 = commit_all(&wt, "c2");
        write(&wt_dir, "f.txt", b"v3\n");
        commit_all(&wt, "c3");
        let path = write(&wt_dir, "f.txt", b"v4\n");

        let mut allocated = BaseBlobs::new([(wt_dir.clone(), c2.to_string())]);
        let out = assemble_real(std::slice::from_ref(&path), &mut allocated);
        let c = &out.files[0];
        assert_eq!(c.base_kind, Some(BaseKind::ParentSha), "{c:?}");
        assert_eq!(c.base_sha.as_deref(), Some(c2.to_string().as_str()));
        assert_eq!(c.before.as_deref(), Some("v2\n"));
        assert_eq!(c.status, "modified");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        let c = &out.files[0];
        assert_eq!(c.base_kind, Some(BaseKind::MergeBase), "{c:?}");
        assert_eq!(c.base_sha.as_deref(), Some(c1.to_string().as_str()));
        assert_eq!(c.before.as_deref(), Some("v1\n"));
    }

    /// Off the default branch in a linked worktree with no resolvable
    /// `origin/<default>`, a `HEAD` base would silently hide committed work —
    /// so the answer is UNKNOWN, not `HEAD`.
    #[test]
    fn a_linked_worktree_with_no_resolvable_default_is_unreadable() {
        let root = tempfile::tempdir().expect("tempdir");
        let main_dir = root.path().join("main");
        std::fs::create_dir_all(&main_dir).expect("mkdir");
        let repo = init_repo(&main_dir);
        write(&main_dir, "f.txt", b"v1\n");
        let c1 = commit_all(&repo, "c1");
        let branch = repo
            .branch("agent/y", &repo.find_commit(c1).expect("c1"), false)
            .expect("branch");
        let wt_dir = root.path().join("wt");
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        repo.worktree("wt", &wt_dir, Some(&opts))
            .expect("worktree add");
        let path = write(&wt_dir, "f.txt", b"v2\n");

        let out = assemble_real(&[path], &mut BaseBlobs::new([]));
        let c = &out.files[0];
        assert_eq!(c.status, "unreadable", "{c:?}");
        assert_eq!(c.before_source, BeforeSource::None);
        assert!(
            c.detail.as_deref().unwrap().contains("default branch"),
            "{:?}",
            c.detail
        );
    }

    /// The falsifier: a 50-path session in one repository resolves its base
    /// ONCE, reports every path against it, and stays inside the route's
    /// existing text budget.
    #[test]
    fn a_fifty_path_session_resolves_one_base_inside_the_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = init_repo(dir.path());
        let paths: Vec<String> = (0..50)
            .map(|i| write(dir.path(), &format!("src/f{i:02}.txt"), b"line\n"))
            .collect();
        commit_all(&repo, "c1");
        for p in &paths {
            std::fs::write(p, b"line\nmore\n").expect("rewrite");
        }

        let mut base = BaseBlobs::new([]);
        let out = assemble_real(&paths, &mut base);
        assert_eq!(out.files.len(), 50);
        assert!(out
            .files
            .iter()
            .all(|c| c.status == "modified" && c.before_source == BeforeSource::GitBase));
        assert_eq!(base.resolved_repositories(), 1);
        let held: usize = out
            .files
            .iter()
            .map(|c| {
                c.before.as_ref().map_or(0, |s| s.len()) + c.after.as_ref().map_or(0, |s| s.len())
            })
            .sum();
        assert!(held <= FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES);
        assert_eq!(out.shared_base().map(|b| b.kind), Some(BaseKind::Head));
    }

    /// The wire shape the frontend mirrors (`workerFileChanges.ts`).
    #[test]
    fn the_new_fields_serialise_in_the_routes_wire_vocabulary() {
        let mut c = no_base_entry("/x", "why");
        let v = serde_json::to_value(&c).expect("json");
        assert_eq!(v["beforeSource"], "none");
        assert!(v["baseKind"].is_null());
        c.before_source = BeforeSource::GitBase;
        c.base_kind = Some(BaseKind::MergeBase);
        let v = serde_json::to_value(&c).expect("json");
        assert_eq!(v["beforeSource"], "git_base");
        assert_eq!(v["baseKind"], "merge_base");
        assert_eq!(
            serde_json::to_value(BaseKind::ParentSha).expect("json"),
            "parent_sha"
        );
        assert_eq!(
            serde_json::to_value(BeforeSource::Snapshot).expect("json"),
            "snapshot"
        );
    }
}
