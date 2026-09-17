//! Phase 2 of `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`:
//! take the scan's BYTES from a ref, not from whatever branch the checkout is
//! parked on.
//!
//! ## Why a ref
//!
//! The corpus every reader sees was being written from a shared working tree.
//! A checkout parked on a feature branch published that branch's private state
//! as the fleet's plan corpus, and a `SUPERSEDED` stamp on `origin/main` never
//! reached it. Reading `origin/<default-branch>` makes the scan deterministic
//! and reproducible from any device, and independent of what any session
//! happens to have checked out.
//!
//! ## What this module deliberately does NOT do
//!
//! **It does not fall back to the tree when the ref cannot be read.** A failed
//! fetch yields [`ScanSource::Unavailable`], and the caller's contract for that
//! arm is to publish NOTHING and leave the previous corpus standing. Falling
//! back would reintroduce the defect precisely when the ref is least
//! trustworthy [policy: `unknown-must-not-render-as-a-default`].
//!
//! **It does not widen the walk.** The listing is depth 1, matching
//! [`super::trigger::read_plan_dir`]'s documented flat contract and coord's
//! `walk_root`. Making the scan recursive here would add every subdirectory
//! plan to the corpus as a silent side effect of a scan-SOURCE change.
//!
//! **It does not change what an un-pushed plan means.** A plan that exists only
//! in someone's working tree is not discoverable work, which is what the
//! authority model already says: `2026-08-16-plan-corpus-authority-and-run-provenance`
//! makes the filesystem an authoring surface and the DB authoritative for
//! discovery. `/vet-imp` commits and pushes an authored plan before vetting, so
//! the supported authoring path is unaffected.

use std::path::{Path, PathBuf};

use super::trigger::GitRefReader;

/// Where one scan cycle should take its bytes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSource {
    /// Read `rel_dir` out of `ref_name` (already fetched) in `repo_root`.
    Ref {
        repo_root: PathBuf,
        /// e.g. `origin/main` — the repo's OWN default, never a hardcoded guess.
        ref_name: String,
        /// The plans dir relative to `repo_root`, e.g. `plans`.
        rel_dir: String,
    },
    /// The plans dir is not inside a git work tree. A SUPPORTED configuration —
    /// the plan-corpus contract says a tenant may author into a plain directory
    /// — so the working tree is the only source there is, and reading it is
    /// correct rather than a degradation.
    WorkTree,
    /// The ref could not be established or refreshed. The caller publishes
    /// NOTHING this cycle: an unreadable ref is UNKNOWN, and a tree read here
    /// would be the very substitution this plan exists to remove.
    Unavailable { reason: String },
}

/// Decide this cycle's scan source, fetching the default branch first.
///
/// Ordered so each failure is attributed to the probe that produced it rather
/// than collapsing into one "could not scan": the work-tree question, the
/// default-branch question and the fetch are three different answers.
pub fn resolve_scan_source(git: &dyn GitRefReader, plans_dir: &Path) -> ScanSource {
    let repo_root = match git.work_tree_root(plans_dir) {
        // Not in a repo at all — a real answer, and a supported layout.
        Ok(None) => return ScanSource::WorkTree,
        Ok(Some(root)) => root,
        Err(e) => {
            return ScanSource::Unavailable {
                reason: format!("could not establish whether the plans dir is in a work tree: {e}"),
            }
        }
    };
    let ref_name = match git.default_ref(&repo_root) {
        Ok(r) => r,
        Err(e) => {
            return ScanSource::Unavailable {
                reason: format!("could not resolve the repo's default branch: {e}"),
            }
        }
    };
    let rel_dir = match relative_dir(&repo_root, plans_dir) {
        Some(d) => d,
        None => {
            return ScanSource::Unavailable {
                reason: format!(
                    "the plans dir {} is not under its own work-tree root {}",
                    plans_dir.display(),
                    repo_root.display()
                ),
            }
        }
    };
    // Fetch LAST, so a fetch failure is never reported for a repo whose ref
    // could not have been named anyway.
    if let Err(e) = git.fetch_default(&repo_root, &ref_name) {
        return ScanSource::Unavailable {
            reason: format!("could not refresh {ref_name} before scanning: {e}"),
        };
    }
    ScanSource::Ref {
        repo_root,
        ref_name,
        rel_dir,
    }
}

/// `plans_dir` expressed relative to `repo_root`, with `/` separators.
///
/// `None` when it is not under the root at all, which is a contradiction worth
/// reporting rather than papering over with an empty path that would scan the
/// whole repo.
fn relative_dir(repo_root: &Path, plans_dir: &Path) -> Option<String> {
    // Canonicalized on BOTH sides before the strip. `rev-parse --show-toplevel`
    // returns a realpath with every symlink resolved, while the configured
    // plans dir need not be one — so on a box whose workspace root is reached
    // through a symlink a purely lexical strip fails and the scan goes
    // permanently dark for a layout that is otherwise fine. A path that cannot
    // be canonicalized (it does not exist yet) falls back to itself, which is
    // the old lexical behaviour.
    let root = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let dir = std::fs::canonicalize(plans_dir).unwrap_or_else(|_| plans_dir.to_path_buf());
    let rel = dir.strip_prefix(&root).ok()?;
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/");
    if s.is_empty() {
        // The plans dir IS the repo root; `<ref>:` addresses the root tree.
        Some(String::new())
    } else {
        Some(s)
    }
}

/// One plan file as it exists at the ref: its bare name and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefPlanFile {
    pub name: String,
    pub body: String,
}

/// What one ref listing produced: the files whose bytes were read, and the
/// CENSUS of the listing itself.
///
/// The two are deliberately different sets. `files` is what the scan could
/// USE; `names` is what the listing SAW — including an entry whose blob would
/// not read, which is skipped below with a warning. The census is the
/// denominator of a coverage question, so it has to be the second: a file the
/// scan chokes on must not vanish from both sides of the set difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefListing {
    /// Files whose blobs read, sorted by name.
    pub files: Vec<RefPlanFile>,
    /// Every depth-1 `*.md` name at the ref, sorted — blob-readable or not.
    pub names: Vec<String>,
    /// What `ref_name` resolved to when this listing was taken. `None` when
    /// the rev would not resolve: UNKNOWN, and the census still stands —
    /// the stems WERE listed, only the sha they were listed at is missing.
    pub ref_sha: Option<String>,
}

/// Read every depth-1 `*.md` of `rel_dir` at `ref_name`.
///
/// Two `git` invocations for the whole directory — one listing, one batched
/// blob read — rather than one per file. The active plans dir is ~1,100 files
/// and the reconcile loop's own comment measures its first cycle in minutes;
/// a spawn per entry would add that cost to a loop that already dilates a
/// single-worker runtime's time driver.
///
/// A file whose blob will not read is SKIPPED with a warning, exactly as the
/// working-tree walk skips an unreadable file: one bad entry must not discard
/// the other 1,099.
pub fn read_ref_dir(
    git: &dyn GitRefReader,
    repo_root: &Path,
    ref_name: &str,
    rel_dir: &str,
) -> Result<RefListing, String> {
    let entries = git.list_ref_dir(repo_root, ref_name, rel_dir)?;
    let wanted: Vec<_> = entries
        .into_iter()
        .filter(|e| is_plan_file(&e.name))
        .collect();
    // The census is taken from the LISTING, before a single blob is read, so
    // it names what the ref side holds rather than what this cycle managed to
    // read out of it.
    let mut names: Vec<String> = wanted.iter().map(|e| e.name.clone()).collect();
    names.sort();
    // Resolved HERE, at the listing, so the census carries the sha its stems
    // were listed AT — not one read at some other moment of the cycle. A rev
    // that will not resolve leaves it UNKNOWN rather than failing the listing:
    // the stems are the reading, the sha only qualifies it.
    let ref_sha = match git.rev_parse(repo_root, ref_name) {
        Ok(sha) => Some(sha),
        Err(e) => {
            tracing::debug!(
                ref_name = %ref_name,
                error = %e,
                "plan adapter: listed the ref but could not resolve it to an object id; the \
                 slug census carries ref_sha UNKNOWN"
            );
            None
        }
    };
    if wanted.is_empty() {
        return Ok(RefListing {
            files: Vec::new(),
            names,
            ref_sha,
        });
    }
    let asked = wanted.len();
    let ids: Vec<String> = wanted.iter().map(|e| e.id.clone()).collect();
    let bodies = git.read_blobs(repo_root, &ids);
    let mut out = Vec::with_capacity(wanted.len());
    for (entry, body) in wanted.into_iter().zip(bodies) {
        match body {
            Ok(b) => out.push(RefPlanFile {
                name: entry.name,
                body: b,
            }),
            Err(e) => {
                tracing::warn!(
                    name = %entry.name,
                    id = %entry.id,
                    error = %e,
                    "plan adapter: skipping a plan whose blob could not be read at the ref"
                );
            }
        }
    }
    if out.is_empty() {
        // EVERY blob failed. Per file that is a skip; all of them at once is
        // not a directory of 1,100 broken files, it is ONE broken read — a
        // `git` that would not spawn, a batch killed by its watchdog, a severed
        // pipe — and `Ok(Vec::new())` would hand reconcile an empty corpus and
        // let it treat every plan as disappeared. That publish is the exact
        // thing the no-fallback contract exists to prevent, so the whole-batch
        // case is an error even though its parts are not
        // [policy: `unknown-must-not-render-as-a-default`].
        return Err(format!(
            "none of the {asked} plan blobs at `{ref_name}:{rel_dir}` could be read"
        ));
    }
    // `ls-tree` already emits in tree order, which is byte order on the name —
    // sorted anyway so a dry-run report is reproducible across git versions.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(RefListing {
        files: out,
        names,
        ref_sha,
    })
}

/// The ref arm's `*.md` predicate.
///
/// `extension() == Some("md")`, matching [`super::trigger::read_plan_dir`]
/// rather than a bare `ends_with(".md")`: the two disagree on a file named
/// exactly `.md`, where `extension()` is `None` and the stem is the whole
/// name. A one-file difference, but the claim this phase makes is that the two
/// arms scan the SAME set, and an unexamined difference is how that claim
/// stops being true.
fn is_plan_file(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()) == Some("md")
}
