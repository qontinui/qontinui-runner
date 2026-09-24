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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::trigger::{GitRefReader, RefDirEntry};

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

/// ONE ref state per reconcile cycle, shared by both halves of that cycle.
///
/// Follow-up to `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`
/// (#1662 named it and deliberately did not do it). The work-unit half and the
/// document half each resolve a scan source, and the document half once per
/// root. Before this, each of those resolutions fetched (through a 30 s
/// process-global memo) and each LISTING called `rev_parse` on its own, so the
/// two halves were two reads of a MOVING target. A cycle slower than the memo's
/// TTL re-fetched mid-cycle — a cold first cycle runs minutes — and a peer's
/// `git fetch` in the same shared checkout, routine here, could advance
/// `origin/main` between the two `rev_parse` calls. The census the work-unit
/// half reported and the bodies the document half published could then come
/// from two different commits, with every field still looking well-formed.
///
/// A pin closes that by construction rather than by timing: the FIRST
/// resolution of a `(repo_root, ref_name)` in a cycle fetches once and resolves
/// the ref to an object id once, and every later resolution of that pair in the
/// same cycle reuses the answer — the sha, or the failure. Listings are then
/// addressed by that object id, and an object id names one tree forever, so
/// both halves read byte-identical listings however long the cycle runs and
/// whoever fetches meanwhile.
///
/// What it deliberately does NOT share is the listing itself. Holding the
/// read bodies (~54 MB on the measured corpus) across the reconcile's network
/// phase would buy nothing for coherence — the sha already fixes the bytes —
/// and the byte-bounded blob cache already serves the second read.
///
/// Scope: one pin per cycle, dropped with it. A pin that outlived its cycle
/// would freeze the corpus at one ref state, so nothing stores one —
/// `a_pin_does_not_outlive_its_cycle` pins that.
///
/// What it does NOT cover: the scan-divergence probe. That runs earlier in the
/// tick, fetches nothing, and resolves the ref for itself, so on a cycle whose
/// fetch advances `origin/main` the scan-root report can carry the probe's
/// `ref_sha` (pre-fetch) beside the census's (post-fetch). That predates this
/// type and is not made worse by it; routing the probe through the pin would
/// change what the probe measures, and is its own change.
///
/// A failed fetch is one answer for the whole repo for the whole cycle: every
/// root in that repo is `Unavailable` together, and the next cycle retries.
#[derive(Debug, Default)]
pub struct CycleRefPin {
    /// `(repo_root, ref_name)` -> this cycle's answer. `Ok(Some(sha))`: fetched
    /// and resolved. `Ok(None)`: fetched, but the ref would not resolve to an
    /// object id — listings fall back to the ref NAME and carry `ref_sha`
    /// UNKNOWN, exactly as an unpinned listing does; that is the one arm in
    /// which the halves are not pinned, and it is reported rather than hidden.
    /// `Err`: the fetch failed, and every consumer of the pair this cycle sees
    /// the SAME `Unavailable` rather than retrying into a different answer.
    resolved: std::sync::Mutex<HashMap<(PathBuf, String), Result<Option<String>, String>>>,
}

impl CycleRefPin {
    /// Decide a scan source, fetching the default branch at most once per
    /// `(repo_root, ref_name)` for the life of this pin.
    ///
    /// Ordered so each failure is attributed to the probe that produced it
    /// rather than collapsing into one "could not scan": the work-tree
    /// question, the default-branch question and the fetch are three different
    /// answers.
    pub fn resolve_source(&self, git: &dyn GitRefReader, plans_dir: &Path) -> ScanSource {
        let source = resolve_ref_listing_source(git, plans_dir);
        // Fetch LAST, so a fetch failure is never reported for a repo whose ref
        // could not have been named anyway.
        if let ScanSource::Ref {
            repo_root,
            ref_name,
            ..
        } = &source
        {
            if let Err(reason) = self.resolve_ref(git, repo_root, ref_name) {
                return ScanSource::Unavailable { reason };
            }
        }
        source
    }

    /// The object id this cycle pinned `ref_name` to, resolving it (fetch, then
    /// `rev_parse`) on first use. `Ok(None)` is a fetched ref that would not
    /// resolve — UNKNOWN, see [`Self::resolved`].
    pub fn resolve_ref(
        &self,
        git: &dyn GitRefReader,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<Option<String>, String> {
        let key = (repo_root.to_path_buf(), ref_name.to_string());
        // Held across the fetch on purpose: a second consumer of the same pair
        // must WAIT for the first one's answer rather than race it with a
        // fetch of its own, which is the incoherence this type removes. Only
        // one cycle's consumers ever contend here.
        let mut resolved = self.resolved.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(answer) = resolved.get(&key) {
            return answer.clone();
        }
        let answer = match git.fetch_default(repo_root, ref_name) {
            Err(e) => Err(format!("could not refresh {ref_name} before scanning: {e}")),
            Ok(()) => Ok(resolve_ref_sha(git, repo_root, ref_name)),
        };
        resolved.insert(key, answer.clone());
        answer
    }
}

/// `rev_parse` with its failure demoted to UNKNOWN: the stems a listing takes
/// are the reading, and the sha only qualifies it.
fn resolve_ref_sha(git: &dyn GitRefReader, repo_root: &Path, ref_name: &str) -> Option<String> {
    match git.rev_parse(repo_root, ref_name) {
        Ok(sha) => Some(sha),
        Err(e) => {
            tracing::debug!(
                ref_name = %ref_name,
                error = %e,
                "plan adapter: could not resolve the ref to an object id before listing it; the \
                 slug census carries ref_sha UNKNOWN and the listing is taken at the ref name"
            );
            None
        }
    }
}

/// [`CycleRefPin::resolve_source`] WITHOUT the fetch: name the ref this clone already
/// holds and ask nothing of the network.
///
/// The split exists for the census-only read
/// ([`super::trigger::ref_census_only`], which a withheld work-unit posture
/// takes). A stem LISTING is not a scan: it reads no blob, parses nothing and
/// publishes no corpus, so it neither needs nor deserves a fetch. Its claim is
/// "these stems exist at this object id", which is true of the tracking ref
/// whatever its age — and the scan-root report the census travels in carries
/// that age (`behind`, `ahead`, `ref_age_secs`) right beside it, so a stale
/// ref is qualified rather than mistaken for a fresh one.
pub fn resolve_ref_listing_source(git: &dyn GitRefReader, plans_dir: &Path) -> ScanSource {
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
    /// `false` when at least one listed `*.md` blob was skipped (unreadable,
    /// or not UTF-8). A short `files` is then NOT evidence that the skipped
    /// plans are absent from the ref — see
    /// [`super::trigger::CycleScan::complete`], which this feeds on the
    /// work-unit half. On the document half, `scan_roots_at_source` turns it
    /// into one `unreadable_file` skip per missing stem (or one root-level
    /// `unreadable_entry` if no stem is missing), so the catch-up dry run
    /// reports the gap instead of a short count with no explanation.
    ///
    /// It qualifies `files` ONLY. `names` is taken from the listing before a
    /// single blob is read, so the census stays whole across exactly the
    /// failure this flag reports — which is the point of keeping both.
    pub complete: bool,
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
/// the other 1,099. The skip clears [`RefListing::complete`], because it is
/// the same partial-read shape the tree walk reports: the returned set is
/// short, and the plans it lacks were NOT shown absent from the ref.
pub fn read_ref_dir(
    git: &dyn GitRefReader,
    repo_root: &Path,
    ref_name: &str,
    rel_dir: &str,
) -> Result<RefListing, String> {
    let ref_sha = resolve_ref_sha(git, repo_root, ref_name);
    read_ref_dir_at(git, repo_root, ref_name, ref_sha, rel_dir)
}

/// [`read_ref_dir`] at an object id the CALLER already resolved — the door a
/// [`CycleRefPin`] reads through, so every consumer of one cycle lists the
/// same tree. `ref_sha: None` is a ref that would not resolve: the listing is
/// taken at `ref_name` and carries the sha UNKNOWN.
pub fn read_ref_dir_at(
    git: &dyn GitRefReader,
    repo_root: &Path,
    ref_name: &str,
    ref_sha: Option<String>,
    rel_dir: &str,
) -> Result<RefListing, String> {
    // Listed by [`list_ref_entries`] — which is also what the census-only door
    // takes, so neither can drift on the predicate or on the object id the
    // stems were listed at.
    let RefEntryListing {
        entries: wanted,
        ref_sha,
    } = list_ref_entries(git, repo_root, ref_name, ref_sha, rel_dir)?;
    // The census is taken from the LISTING, before a single blob is read, so
    // it names what the ref side holds rather than what this cycle managed to
    // read out of it.
    let names: Vec<String> = wanted.iter().map(|e| e.name.clone()).collect();
    if wanted.is_empty() {
        // A listing with no `*.md` in it: genuinely empty, and complete.
        return Ok(RefListing {
            files: Vec::new(),
            names,
            ref_sha,
            complete: true,
        });
    }
    let asked = wanted.len();
    let ids: Vec<String> = wanted.iter().map(|e| e.id.clone()).collect();
    let bodies = git.read_blobs(repo_root, &ids);
    let mut out = Vec::with_capacity(wanted.len());
    // `read_blobs` answers one slot per id asked; a short answer is itself a
    // gap (the `zip` below would silently drop the unanswered tail).
    let mut complete = bodies.len() == asked;
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
                    "plan adapter: skipping a plan whose blob could not be read at the ref; \
                     scan is PARTIAL"
                );
                complete = false;
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
        complete,
    })
}

/// The stem-bearing entries of `<ref>:<rel_dir>`, sorted by name, and the
/// object id they were listed at.
struct RefEntryListing {
    entries: Vec<RefDirEntry>,
    ref_sha: Option<String>,
}

/// The listing half of [`read_ref_dir`] — one `git ls-tree`, no blob read.
///
/// Shared with [`list_ref_plan_names`] so the two doors cannot drift into two
/// different answers about which entries are plans, or about which object id
/// they were listed at.
fn list_ref_entries(
    git: &dyn GitRefReader,
    repo_root: &Path,
    ref_name: &str,
    ref_sha: Option<String>,
    rel_dir: &str,
) -> Result<RefEntryListing, String> {
    // LISTED AT THE RESOLVED OBJECT ID, which the caller resolved FIRST — so
    // the census carries the sha its stems were actually listed at.
    //
    // Naming `ref_name` twice would be two reads of a MOVING target: these are
    // separate `git` processes, and a concurrent `git fetch` in the same clone
    // (the norm on a shared, hot checkout) advances `origin/main` between them.
    // The census would then assert stems listed at A under a sha of B — a set
    // difference computed against the wrong side, and invisible, because every
    // field would look well-formed. Addressing the listing by object id makes
    // the pair atomic by construction rather than by luck.
    //
    // A rev that would not resolve (`None`) leaves the sha UNKNOWN and the
    // listing is taken at the ref name, rather than failing it: the stems are
    // the reading, the sha only qualifies it.
    let listed_at = ref_sha.as_deref().unwrap_or(ref_name);
    let mut entries: Vec<RefDirEntry> = git
        .list_ref_dir(repo_root, listed_at, rel_dir)?
        .into_iter()
        .filter(|e| is_plan_file(&e.name))
        .collect();
    // `ls-tree` already emits in tree order, which is byte order on the name —
    // sorted anyway so both doors are reproducible across git versions.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(RefEntryListing { entries, ref_sha })
}

/// What a LISTING-ONLY read of the ref saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefNameListing {
    /// Every depth-1 `*.md` name at the ref, sorted.
    pub names: Vec<String>,
    /// What the ref resolved to when this listing was taken; `None` is
    /// UNKNOWN, and the stems still stand.
    pub ref_sha: Option<String>,
}

/// LIST the ref's plan names without reading a single blob.
///
/// The census-only door, for a cycle that is not going to publish a corpus and
/// so must not pay ~1,100 blob reads to discard them — the withheld work-unit
/// posture. It is the same listing [`read_ref_dir`] takes its own census from,
/// by construction: both go through [`list_ref_entries`].
pub fn list_ref_plan_names(
    git: &dyn GitRefReader,
    repo_root: &Path,
    ref_name: &str,
    rel_dir: &str,
) -> Result<RefNameListing, String> {
    let ref_sha = resolve_ref_sha(git, repo_root, ref_name);
    let listing = list_ref_entries(git, repo_root, ref_name, ref_sha, rel_dir)?;
    Ok(RefNameListing {
        names: listing.entries.into_iter().map(|e| e.name).collect(),
        ref_sha: listing.ref_sha,
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
