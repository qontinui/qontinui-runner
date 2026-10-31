//! Which `.claude/` tree a spawned session is served, how stale it is, and
//! whether its files are THIS build's bundle.
//!
//! Plan `2026-09-03-served-corpus-provenance-at-spawn`. A session is served a
//! slash-command body, a skill or a hook out of `<workdir>/.claude/`, and on
//! this fleet that directory is often a symlink into a shared, tracked
//! `qontinui-claude-config` checkout. That checkout drifts from `origin/main`,
//! and a runner build that predates the tracked-destination guard can overwrite
//! it with the binary's embedded bundle. Neither leaves a mark a session can
//! read without a `git` call of its own, and the harness expands a command body
//! before the session's first turn, so no reader-side rule can reach it.
//!
//! The runner is the one party that can answer at spawn, for free: it holds the
//! bundle bytes in memory. This module measures the served tree once per spawn
//! and renders ONE key-addressed header line of `QONTINUI_RUNNER_CONTEXT`:
//!
//! ```text
//! [served-corpus: <canonical .claude>] [checkout: <repo> <branch>@<sha12> upstream=<ref> behind=<n> ahead=<m> as-of=<fetch-ts> dirty-claude=<k>] [bundle: <N>/<M> identical-to-build <gitSha> stamped=<k> stamped-tracked=<t> dirty-bundle=<d> served: canonical@<sha12> <c> fetched <ts>, builtin <b>, account <a>, unstamped <u>; identical-to-source <i>/<v> unverifiable=<x>] [provisioned: …] [cwd: …]
//! ```
//!
//! The `bundle` token has three parts. The first, `<N>/<M> identical-to-build`,
//! compares all M files the binary carries against THIS build's bytes — the
//! provisioner-overwrite signature. The second is scoped to the same M paths
//! and read from the served checkout's git: `stamped-tracked` counts files that
//! carry their OWN `qontinui-provenance:` stamp AND are tracked (a stamped
//! tracked file is a clobber proven by the file itself; a stamped untracked
//! file is an ordinary provision), and `dirty-bundle` counts roster paths git
//! reports with tracked changes. Unlike `dirty-claude`, which counts every
//! change under `.claude/`, it never joins an unrelated edit to the bundle. Both
//! read `n/a` outside a git work tree and `UNKNOWN(<code>)` when git could not
//! answer — never `0`. The code is one kebab-case word with no space, so the
//! count stays one whitespace-free field; [`CountUnknown`] lists every code and
//! the one cause each names. The third is source-aware: each present
//! file is sorted by the `source=` of its `qontinui-provenance:` stamp
//! ([`crate::provenance`]) and compared against the copy of that source the
//! process holds in memory — a `canonical` file against the loaded
//! `canonical_corpus` snapshot when its stamp names that snapshot, a `builtin`
//! file against this build's bundle, an unstamped file against this build's
//! bundle too. An account file (`served` / `disk_cache`) is counted, never
//! compared, and a file whose source copy is not in memory is `unverifiable`,
//! never identical. `canonical@unloaded` means this process has loaded no
//! snapshot yet. Reading the snapshot is a lock and an `Arc` clone: the probe
//! never fetches.
//!
//! Every value is a measurement, `UNKNOWN (<reason>)`, or — for the two roster
//! counts — `UNKNOWN(<code>)`, never a default. The
//! tokens are cut at their own first `]`, the line-2 grammar, so no rendered
//! value ever carries a `]`.
//!
//! ## Where the I/O happens
//!
//! [`crate::terminal::runner_context`] is a zero-I/O renderer by contract. The
//! SEAM that spawns a session measures and hands the result in, the same way it
//! hands in the per-session `CoordMcpDelivery`: the terminal spawn starts the
//! measurement with [`probe_start`] as soon as its cwd is known and joins it
//! inside the identity seam, so the measurement overlaps the seam's other work;
//! every other caller uses [`probe`] (start and join at once).
//!
//! **The guarantee: [`probe`] (and a [`ProbeHandle::join`], counted from its
//! [`probe_start`]) returns within
//! [`PROBE_BUDGET`] + [`PROBE_COLLECT_GRACE`], whatever process creation
//! costs.** Two walls make that true (plan
//! `2026-10-19-served-corpus-probe-budget-excludes-process-creation`):
//!
//! - **The inner wall is one absolute deadline for every `git` spawn**, PROCESS
//!   CREATION INCLUDED: each run goes through
//!   [`crate::process_deadline::run_with_deadline_detailed`], whose clock starts
//!   before `spawn()`, not after it. A `git` that could not even START before
//!   the deadline reads `UNKNOWN(timed-out)` with a reason naming process
//!   creation; a child that starts late is killed and parked, never waited on.
//!   Once the budget is spent every later question answers `UNKNOWN (deadline:
//!   …)` at once. (On Windows Rust std holds one process-wide lock across
//!   `CreateProcessW`, so one slow `std::process` or tokio spawn anywhere in
//!   the runner can queue this probe's spawns behind it for minutes — the
//!   plan's 2026-10-19 read of spaceship's span log, 2026-10-15..18, measured
//!   the probe at up to 208 s before this wall existed.)
//! - **The outer wall is the caller's**: the measurement runs on a
//!   `served-corpus-probe` worker thread, and the caller waits for it until
//!   `called_at + PROBE_BUDGET + PROBE_COLLECT_GRACE` and no longer. A worker
//!   still running then — stuck in non-git I/O, say — leaves the caller with
//!   `[served-corpus: UNKNOWN (deadline: served-corpus probe budget … spent)]`.
//!   That UNKNOWN is never memoised; the worker's own late result is, so the
//!   next spawn within [`MEMO_TTL`] reads the real measurement.
//!
//! Concurrent probes of one workdir share ONE measurement (single-flight; keyed
//! on the workdir as spelled, so two spellings of one directory measure
//! twice, and the memo, keyed on the canonical path, then serves the later
//! one), and
//! at most [`MAX_INFLIGHT_PROBES`] measurements run at once; past that a probe
//! answers `UNKNOWN` at once rather than start another thread. The probe never
//! fetches, never writes, and runs `git status` with `GIT_OPTIONAL_LOCKS=0`
//! (so not even the index is refreshed — which is also what makes killing a
//! late `git` harmless), without untracked files, renames or submodules.
//!
//! ## Why the answer is memoised
//!
//! A seam renders the briefing twice: an argv copy BEFORE it provisions the
//! session and an env copy AFTER. Two independent probes could disagree across
//! that write. The memo keys on `(canonical workdir, RUNNER_BUILD_ID)` for
//! [`MEMO_TTL`], so both copies carry the same measurement.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Wall-clock bound on ALL the `git` spawns one probe runs, together. The
/// same 5 s class as `provision_guard::PROBE_BUDGET`: generous for a few local
/// reads, tight for a spawn an operator is waiting on.
pub(crate) const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// How long a probe result is reused for the same workdir and build.
const MEMO_TTL: Duration = Duration::from_secs(30);

/// How long past [`PROBE_BUDGET`] the caller of [`probe`] keeps waiting for
/// the measurement, so a worker that returns at its own deadline, carrying
/// partial measurements, is collected rather than discarded.
///
/// It must cover [`crate::process_deadline::KILL_GRACE`]: a `git` killed at
/// the deadline can take that long to hand its run back, and a shorter wall
/// would throw away every measurement the probe had already made. The plan
/// named 250 ms; that is shorter than the kill grace (500 ms), so the wall is
/// the kill grace plus 100 ms of margin (enforced below).
pub(crate) const PROBE_COLLECT_GRACE: Duration = Duration::from_millis(600);

const _: () = assert!(
    PROBE_COLLECT_GRACE.as_millis() >= crate::process_deadline::KILL_GRACE.as_millis() + 100,
    "PROBE_COLLECT_GRACE must cover the deadline primitive's kill grace"
);

/// Most served-corpus measurement WORKERS running at once. A probe past it
/// answers UNKNOWN at once instead of starting another worker.
///
/// Sized from the terminal-spawn population: 126 terminal spawns in 3.5 days
/// on spaceship (2026-10-15..18) is about 1.5 an hour, and the only structural
/// concurrency is a gate continuation's two renders of ONE workdir, which
/// single-flight folds into one measurement. The other callers
/// ([`probe_async`]: agent runtime, looping supervisor) are rarer still. 4
/// leaves headroom for a burst of distinct workdirs.
///
/// What it does NOT bound: a worker returns at its deadline even when its
/// `git` is still inside `spawn()`, so the `deadline-spawn` helper thread that
/// is waiting on process creation outlives the worker's slot (one per
/// abandoned probe, ended when the kernel returns and the late child is
/// killed). On a box where process creation is stuck those helpers accumulate
/// at the rate probes are made, not at this cap.
pub(crate) const MAX_INFLIGHT_PROBES: usize = 4;

/// The commit this binary was built from, as `/health` reports `gitSha`.
const BUILD_SHA: &str = env!("QONTINUI_GIT_SHA");

/// The build identity the memo keys on, as `/health` reports `buildId`.
const RUNNER_BUILD: &str = env!("RUNNER_BUILD_ID");

/// Upstream ref used when `refs/remotes/origin/HEAD` names none.
const DEFAULT_UPSTREAM: &str = "refs/remotes/origin/main";

/// A measured value, or the reason it could not be measured.
///
/// The reason is DATA that ends up on the header line, not a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Probe<T> {
    Measured(T),
    Unknown(String),
}

/// The branch and commit a checkout's `HEAD` points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    /// `None` on a detached `HEAD`.
    branch: Option<String>,
    /// Full 40-hex commit id.
    sha: String,
}

/// How far `HEAD` is from its upstream, read from the LOCAL remote-tracking ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Upstream {
    /// Short name, e.g. `origin/main`.
    name: String,
    behind: u64,
    ahead: u64,
}

/// One git work tree, as the probe measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoState {
    /// Canonical `git rev-parse --show-toplevel`.
    toplevel: PathBuf,
    head: Probe<Head>,
    upstream: Probe<Upstream>,
    /// When the upstream ref was last fetched, RFC 3339 UTC.
    as_of: Probe<String>,
    /// Tracked entries `git status` reports as changed, in the probed scope.
    dirty: Probe<usize>,
    /// Those entries' paths, relative to [`toplevel`](Self::toplevel).
    changed: Probe<Vec<String>>,
}

/// Whether a path is inside a git work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checkout {
    /// A stated non-checkout, not an UNKNOWN.
    NotAWorkTree,
    Repo(RepoState),
}

/// The `cwd` token: the workdir's own checkout, unless it IS the served one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdCheckout {
    SameAsCorpus,
    Other(Checkout),
}

/// How many of the binary's bundled files the served tree holds unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleIdentity {
    /// Files identical to this build's copy, after EOL normalisation and after
    /// removing a `qontinui-provenance:` key.
    identical: usize,
    /// Every file this binary carries: commands plus every skill file.
    total: usize,
    /// Files that carried a `qontinui-provenance:` key. Canonical sources are
    /// never stamped, so a stamped file inside a tracked tree is a clobber
    /// proven by the file itself.
    stamped: usize,
    /// The `.claude/`-relative paths of those stamped files.
    stamped_rels: Vec<String>,
    /// The roster measured against the served checkout's git.
    in_checkout: BundleInCheckout,
    /// Which rung each present file says it came from, and whether it is still
    /// that rung's bytes.
    sources: SourceIdentity,
}

/// Why a roster count (`stamped-tracked`, `dirty-bundle`) is UNKNOWN, rendered
/// `UNKNOWN(<code>)`. One code per distinct cause, so a reader can tell every
/// UNKNOWN apart from the bundle token alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CountUnknown {
    /// `no-served-corpus`: there is no `.claude/` to measure; the
    /// `served-corpus` token says why.
    NoServedCorpus,
    /// `checkout-unknown`: git could not say whether the corpus is in a work
    /// tree at all; the `checkout` token carries the reason.
    CheckoutUnknown,
    /// `outside-work-tree`: the corpus resolves outside the toplevel git
    /// reported for it, so no roster path maps to a git path.
    OutsideWorkTree,
    /// `status-unknown`: the checkout's `git status` could not be read, so
    /// the changed set `dirty-bundle` filters does not exist; the `checkout`
    /// token's `dirty-claude` carries the reason.
    StatusUnknown,
    /// `ls-files-failed`: `git ls-files` ran and exited non-zero.
    LsFilesFailed,
    /// A `git` run that could not answer — see [`GitError`].
    Git(GitErrorCode),
}

impl CountUnknown {
    fn code(self) -> &'static str {
        match self {
            CountUnknown::NoServedCorpus => "no-served-corpus",
            CountUnknown::CheckoutUnknown => "checkout-unknown",
            CountUnknown::OutsideWorkTree => "outside-work-tree",
            CountUnknown::StatusUnknown => "status-unknown",
            CountUnknown::LsFilesFailed => "ls-files-failed",
            CountUnknown::Git(g) => g.code(),
        }
    }
}

/// One roster count: a measurement, or why there is none.
pub(crate) type Count = Result<usize, CountUnknown>;

/// The bundle roster (the same M paths) as the served checkout's git sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BundleInCheckout {
    /// The served corpus is not inside a git work tree: `n/a`, not zero.
    NotAWorkTree,
    /// Neither count could be asked for — the same cause for both.
    Unknown(CountUnknown),
    Repo {
        /// Stamped files git tracks.
        stamped_tracked: Count,
        /// Roster paths with tracked changes.
        dirty_bundle: Count,
    },
}

impl BundleInCheckout {
    /// `stamped-tracked=<t> dirty-bundle=<d>`: a count, `n/a` outside a work
    /// tree, or `UNKNOWN(<code>)` naming the [`CountUnknown`] cause — which is
    /// in this token itself, so no UNKNOWN here is left without a reason.
    fn render(&self) -> String {
        let n = |v: &Count| match v {
            Ok(n) => n.to_string(),
            Err(why) => format!("UNKNOWN({})", clean(why.code())),
        };
        match self {
            BundleInCheckout::NotAWorkTree => "stamped-tracked=n/a dirty-bundle=n/a".to_string(),
            BundleInCheckout::Unknown(why) => {
                let v = n(&Err(*why));
                format!("stamped-tracked={v} dirty-bundle={v}")
            }
            BundleInCheckout::Repo {
                stamped_tracked,
                dirty_bundle,
            } => format!(
                "stamped-tracked={} dirty-bundle={}",
                n(stamped_tracked),
                n(dirty_bundle)
            ),
        }
    }
}

/// The source-aware half of the `bundle` token: each PRESENT bundled file is
/// sorted by the `source=` of its `qontinui-provenance:` stamp and checked
/// against the copy of THAT source this process holds in memory.
///
/// A skill's helper files carry no stamp of their own (D4); they inherit the
/// stamp of their skill's `SKILL.md`, because a skill directory is provisioned
/// from one source as a unit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SourceIdentity {
    /// The canonical snapshot this process has loaded, when any — the only
    /// canonical generation a `source=canonical` file can be verified against.
    canonical_snapshot: Option<crate::canonical_corpus::CanonicalSnapshot>,
    /// Files stamped `source=canonical`.
    canonical: usize,
    /// Files stamped `source=builtin`.
    builtin: usize,
    /// Files stamped `source=served` or `source=disk_cache`: the account
    /// layer's bodies. The runner holds no copy of them without a network
    /// read, so they are counted and never compared.
    account: usize,
    /// Files with no stamp: repo-authored or checkout source. Compared against
    /// this build's bundle, as before the canonical rung existed.
    unstamped: usize,
    /// Files whose source copy is not in memory: a `source=canonical` stamp
    /// naming a snapshot other than the loaded one (or none loaded), a
    /// `source=builtin` stamp from another build whose bytes differ from this
    /// build's, or an unrecognised `source=`. Never counted as identical.
    unverifiable: usize,
    /// Files compared against their own source's in-memory copy.
    verified: usize,
    /// Of [`verified`](Self::verified), those whose bytes match that copy.
    identical_to_source: usize,
}

impl SourceIdentity {
    /// `served: canonical@<sha12> <c> fetched <ts>, builtin <b>, account <a>,
    /// unstamped <u>; identical-to-source <i>/<v> unverifiable=<x>`.
    fn render(&self) -> String {
        let canonical = match &self.canonical_snapshot {
            Some(s) => format!(
                "canonical@{} {} fetched {}",
                clean(s.short()),
                self.canonical,
                clean(&s.fetched_at)
            ),
            // The tenant's `egress_skill_mirror` switch is the REASON nothing
            // is loaded when it is off — named, so a session can tell "the
            // tenant turned the mirror off" from "the first fetch has not
            // landed yet" (plan 2026-10-10-spec-front-end-phase-9-generic-boundary).
            None if !crate::egress::permit(crate::egress::Flow::SkillMirror).allowed => {
                format!(
                    "canonical@unloaded(egress_skill_mirror=off) {}",
                    self.canonical
                )
            }
            None => format!("canonical@unloaded {}", self.canonical),
        };
        format!(
            "served: {canonical}, builtin {}, account {}, unstamped {}; \
             identical-to-source {}/{} unverifiable={}",
            self.builtin,
            self.account,
            self.unstamped,
            self.identical_to_source,
            self.verified,
            self.unverifiable
        )
    }
}

/// One provisioner's latest pass for a workdir, from the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassSummary {
    written: usize,
    expected: usize,
    /// Skipped-unit counts by `SkipReason::wire()`, zero counts omitted.
    skipped: std::collections::BTreeMap<&'static str, usize>,
    /// When the pass ran, RFC 3339 UTC.
    at: String,
}

/// What the provisioners did for this workdir, per capability, read from
/// `capability_manifest::session_provision_ledger` — the writer's own record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Provisioned {
    commands: Probe<PassSummary>,
    skills: Probe<PassSummary>,
}

impl Provisioned {
    /// `commands written=<w>/<e> skipped-<reason>=<n>… as-of=<ts>; skills …`.
    fn render(&self) -> String {
        let one = |label: &str, p: &Probe<PassSummary>| match p {
            Probe::Measured(s) => {
                let mut out = format!("{label} written={}/{}", s.written, s.expected);
                for (reason, n) in &s.skipped {
                    out.push_str(&format!(" skipped-{}={n}", reason.replace('_', "-")));
                }
                out.push_str(&format!(" as-of={}", s.at));
                out
            }
            Probe::Unknown(r) => format!("{label} {}", unknown_text(r)),
        };
        format!(
            "{}; {}",
            one("commands", &self.commands),
            one("skills", &self.skills)
        )
    }
}

/// The `provisioned` token for `workdir`: the LATEST pass per provisioner.
///
/// The argv copy of the briefing is rendered BEFORE the seam provisions, so
/// the report read there can be a previous spawn's — which is why every pass
/// carries its own `as-of`. No ledger entry is UNKNOWN, never "nothing was
/// provisioned": the ledger is bounded and process-local.
fn provisioned_for(workdir: &Path) -> Probe<Provisioned> {
    let raw = workdir.to_string_lossy();
    let ledger = crate::capability_manifest::session_provision_ledger(&raw).or_else(|| {
        std::fs::canonicalize(workdir).ok().and_then(|c| {
            crate::capability_manifest::session_provision_ledger(&c.to_string_lossy())
        })
    });
    let Some(ledger) = ledger else {
        return Probe::Unknown("no provision recorded for this workdir".to_string());
    };
    let latest = |capability: &str| -> Probe<PassSummary> {
        match ledger
            .reports
            .iter()
            .rev()
            .find(|r| r.capability == capability)
        {
            None => Probe::Unknown("no pass recorded".to_string()),
            Some(r) => {
                let mut skipped = std::collections::BTreeMap::new();
                for unit in &r.skipped {
                    *skipped.entry(unit.reason.wire()).or_insert(0) += 1;
                }
                Probe::Measured(PassSummary {
                    written: r.written,
                    expected: r.expected,
                    skipped,
                    at: r.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                })
            }
        }
    };
    Probe::Measured(Provisioned {
        commands: latest("fleet_commands"),
        skills: latest("fleet_skills"),
    })
}

/// What a spawn was served, measured once at the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServedCorpus {
    corpus: Probe<PathBuf>,
    checkout: Probe<Checkout>,
    bundle: Probe<BundleIdentity>,
    provisioned: Probe<Provisioned>,
    cwd: Probe<CwdCheckout>,
}

impl ServedCorpus {
    /// The value for a call site with no session to measure. Every token reads
    /// `UNKNOWN (<reason>)`, so a caller never fabricates a measurement.
    pub(crate) fn unknown(reason: impl Into<String>) -> ServedCorpus {
        let reason: String = reason.into();
        let r = || reason.clone();
        ServedCorpus {
            corpus: Probe::Unknown(r()),
            checkout: Probe::Unknown(r()),
            bundle: Probe::Unknown(r()),
            provisioned: Probe::Unknown(r()),
            cwd: Probe::Unknown(r()),
        }
    }

    /// The header line, without a trailing newline.
    ///
    /// When the whole value is UNKNOWN for one reason, a single token says so:
    /// five tokens repeating "no session" would be noise.
    pub(crate) fn render_line(&self) -> String {
        if let Probe::Unknown(reason) = &self.corpus {
            let all_same = [
                unknown_reason(&self.checkout),
                unknown_reason(&self.bundle),
                unknown_reason(&self.provisioned),
                unknown_reason(&self.cwd),
            ]
            .iter()
            .all(|r| *r == Some(reason.as_str()));
            if all_same {
                return format!("[served-corpus: UNKNOWN ({})]", clean(reason));
            }
        }
        let corpus = match &self.corpus {
            Probe::Measured(p) => clean(&p.display().to_string()),
            Probe::Unknown(r) => unknown_text(r),
        };
        let checkout = match &self.checkout {
            Probe::Measured(c) => render_checkout(c, "dirty-claude", true),
            Probe::Unknown(r) => unknown_text(r),
        };
        let bundle = match &self.bundle {
            Probe::Measured(b) => format!(
                "{}/{} identical-to-build {BUILD_SHA} stamped={} {} {}",
                b.identical,
                b.total,
                b.stamped,
                b.in_checkout.render(),
                b.sources.render()
            ),
            Probe::Unknown(r) => unknown_text(r),
        };
        let provisioned = match &self.provisioned {
            Probe::Measured(p) => p.render(),
            Probe::Unknown(r) => unknown_text(r),
        };
        let cwd = match &self.cwd {
            Probe::Measured(CwdCheckout::SameAsCorpus) => "same checkout".to_string(),
            Probe::Measured(CwdCheckout::Other(c)) => render_checkout(c, "dirty", false),
            Probe::Unknown(r) => unknown_text(r),
        };
        format!(
            "[served-corpus: {corpus}] [checkout: {checkout}] [bundle: {bundle}] \
             [provisioned: {provisioned}] [cwd: {cwd}]"
        )
    }
}

fn unknown_reason<T>(p: &Probe<T>) -> Option<&str> {
    match p {
        Probe::Unknown(r) => Some(r.as_str()),
        Probe::Measured(_) => None,
    }
}

/// `UNKNOWN (<reason>)`, with the reason made safe for the token grammar.
fn unknown_text(reason: &str) -> String {
    format!("UNKNOWN ({})", clean(reason))
}

/// A value safe inside one `[key: value]` token: no `]` (it would end the
/// token early) and no line break (it would end the header line).
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ']' => ')',
            '[' => '(',
            '\n' | '\r' => ' ',
            c => c,
        })
        .collect()
}

fn render_checkout(c: &Checkout, dirty_key: &str, with_ahead: bool) -> String {
    let repo = match c {
        Checkout::NotAWorkTree => return "none (not a git work tree)".to_string(),
        Checkout::Repo(r) => r,
    };
    let name = repo
        .toplevel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.toplevel.display().to_string());
    // Every git-derived value goes through `clean`: a branch may legally be
    // named `feat]x`, and one `]` would end the token early.
    let head = match &repo.head {
        Probe::Measured(h) => format!(
            "{}@{}",
            clean(h.branch.as_deref().unwrap_or("(detached)")),
            clean(h.sha.get(..12).unwrap_or(&h.sha))
        ),
        Probe::Unknown(r) => format!("HEAD={}", unknown_text(r)),
    };
    let upstream = match &repo.upstream {
        Probe::Measured(u) if with_ahead => format!(
            "upstream={} behind={} ahead={}",
            clean(&u.name),
            u.behind,
            u.ahead
        ),
        Probe::Measured(u) => format!("behind={}", u.behind),
        Probe::Unknown(r) if with_ahead => format!("upstream={}", unknown_text(r)),
        Probe::Unknown(r) => format!("behind={}", unknown_text(r)),
    };
    let as_of = match &repo.as_of {
        Probe::Measured(ts) => clean(ts),
        Probe::Unknown(r) => unknown_text(r),
    };
    let dirty = match &repo.dirty {
        Probe::Measured(n) => n.to_string(),
        Probe::Unknown(r) => unknown_text(r),
    };
    format!(
        "{} {head} {upstream} as-of={as_of} {dirty_key}={dirty}",
        clean(&name)
    )
}

// ── The probe ───────────────────────────────────────────────────────────────

type MemoKey = (PathBuf, &'static str);

fn memo() -> &'static Mutex<HashMap<MemoKey, (Instant, ServedCorpus)>> {
    static MEMO: OnceLock<Mutex<HashMap<MemoKey, (Instant, ServedCorpus)>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Measure what `<workdir>/.claude/` serves, reusing a measurement of the same
/// workdir by the same build taken within [`MEMO_TTL`].
///
/// Never fails and never panics: it runs on a spawn path, and every failure is
/// an `UNKNOWN (<reason>)` token.
///
/// Returns within [`PROBE_BUDGET`] + [`PROBE_COLLECT_GRACE`] of the call — see
/// the module docs. Exactly `probe_start(workdir, None).join()`.
pub(crate) fn probe(workdir: &Path) -> ServedCorpus {
    probe_start(workdir, None).join()
}

/// Start measuring what `<workdir>/.claude/` serves — or join the measurement
/// of that workdir already in flight — WITHOUT waiting for it. The returned
/// handle's [`ProbeHandle::join`] waits until THIS call's
/// `called_at + PROBE_BUDGET + PROBE_COLLECT_GRACE` and no longer, so a caller
/// with other work to do (the terminal spawn seam) can overlap the measurement
/// with it and still keep the bound (plan
/// `2026-10-19-served-corpus-probe-budget-excludes-process-creation`, Phase 5).
///
/// Non-blocking: it takes the in-flight table's lock and at most starts one
/// thread. A dropped handle leaves the worker to finish and memoise.
///
/// `terminal_id` is recorded on the worker's `served_corpus.measure` span so a
/// span-log read can pair a measurement with the terminal spawn that started
/// it (`terminal_spawn.served_corpus_probe{terminal_id}`). A spawn that JOINS a
/// measurement another caller started has no measure span of its own (the
/// span carries the starter's id, empty for [`probe`]), so such a read must
/// leave unpaired spawns and `terminal_id=""` out. The worker's span is
/// deliberately NOT a child of the caller's: a parent stays open until its last
/// child closes, so a worker that outlives its caller would stretch the
/// caller's recorded duration.
pub(crate) fn probe_start(workdir: &Path, terminal_id: Option<&str>) -> ProbeHandle {
    let called_at = Instant::now();
    let deadline = called_at + PROBE_BUDGET;
    let owned = workdir.to_path_buf();
    let terminal_id = terminal_id.unwrap_or_default().to_string();
    start_bounded(
        &PRODUCTION_FLIGHTS,
        (workdir.to_path_buf(), RUNNER_BUILD),
        deadline + PROBE_COLLECT_GRACE,
        format!("deadline: served-corpus probe budget {PROBE_BUDGET:?} spent"),
        move || {
            // The canonicalize, the memo lookup and the measurement all run
            // here, on the worker: the caller only waits. The span is what the
            // spawn seam's `terminal_spawn.served_corpus_probe` (the JOIN wait)
            // is compared against to show the overlap.
            let span = tracing::debug_span!(
                parent: None,
                "served_corpus.measure",
                terminal_id = %terminal_id,
                memo_hit = tracing::field::Empty
            );
            let _entered = span.enter();
            let key_dir = std::fs::canonicalize(&owned).unwrap_or_else(|_| owned.clone());
            let mut measured = false;
            let value = memoised((key_dir, RUNNER_BUILD), MEMO_TTL, || {
                measured = true;
                let canonical = crate::canonical_corpus::latest();
                probe_with_opts(
                    &owned,
                    OsStr::new("git"),
                    PROBE_BUDGET,
                    deadline,
                    std::process::Command::spawn,
                    canonical.as_deref(),
                )
            });
            span.record("memo_hit", !measured);
            value
        },
    )
}

/// The measurements in flight, keyed like the memo but on the workdir AS
/// GIVEN (the canonicalize is part of the measurement, so it cannot key the
/// join). `cap` is [`MAX_INFLIGHT_PROBES`] in production; a test builds its
/// own table so it can fill one without starving every other probe.
struct Flights {
    table: Mutex<HashMap<MemoKey, std::sync::Arc<Flight>>>,
    cap: usize,
}

static PRODUCTION_FLIGHTS: std::sync::LazyLock<Flights> = std::sync::LazyLock::new(|| Flights {
    table: Mutex::new(HashMap::new()),
    cap: MAX_INFLIGHT_PROBES,
});

/// One measurement, joined by every caller that asks for its key while it
/// runs.
#[derive(Default)]
struct Flight {
    value: Mutex<Option<ServedCorpus>>,
    done: std::sync::Condvar,
}

/// A served-corpus measurement started (or joined) by [`probe_start`]. Its
/// wall was fixed when it was STARTED, not when it is joined.
pub(crate) struct ProbeHandle {
    pending: Pending,
    wall: Instant,
    deadline_reason: String,
}

enum Pending {
    /// Answered at start, with nothing to wait for (capacity, or a worker
    /// that could not start).
    Ready(Box<ServedCorpus>),
    /// A measurement in flight — this call's own, or one it joined.
    InFlight(std::sync::Arc<Flight>),
}

impl ProbeHandle {
    /// The measurement, waiting for it until the wall fixed at start and no
    /// longer. A measurement that has finished is returned even when the wall
    /// has passed; one that has not reads the one-token deadline UNKNOWN,
    /// which is never memoised (the worker memoises its own late result).
    pub(crate) fn join(self) -> ServedCorpus {
        let flight = match self.pending {
            Pending::Ready(v) => return *v,
            Pending::InFlight(f) => f,
        };
        let mut g = flight.value.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(v) = g.as_ref() {
                return v.clone();
            }
            let now = Instant::now();
            if now >= self.wall {
                return ServedCorpus::unknown(self.deadline_reason);
            }
            g = flight
                .done
                .wait_timeout(g, self.wall - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// Run `measure` on a worker — or join the one already measuring `key` — and
/// hand back a handle that waits for it until `wall`. Past the wall,
/// `deadline_reason` as a one-token UNKNOWN; past `flights.cap`, a capacity
/// UNKNOWN at once. Neither UNKNOWN is memoised: only `measure` itself writes
/// the memo, when it completes.
fn start_bounded(
    flights: &'static Flights,
    key: MemoKey,
    wall: Instant,
    deadline_reason: String,
    measure: impl FnOnce() -> ServedCorpus + Send + 'static,
) -> ProbeHandle {
    let ready = |v: ServedCorpus| ProbeHandle {
        pending: Pending::Ready(Box::new(v)),
        wall,
        deadline_reason: String::new(),
    };
    let mut table = flights.table.lock().unwrap_or_else(|p| p.into_inner());
    let flight = if let Some(f) = table.get(&key) {
        std::sync::Arc::clone(f)
    } else {
        if table.len() >= flights.cap {
            return ready(ServedCorpus::unknown(
                "served-corpus probe capacity: measurements still in flight",
            ));
        }
        let f = std::sync::Arc::new(Flight::default());
        let worker_flight = std::sync::Arc::clone(&f);
        let worker_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("served-corpus-probe".to_string())
            .spawn(move || {
                // A panicking measurement must still complete its flight:
                // otherwise every later probe of this workdir would join a
                // flight that never ends, and its capacity slot would leak.
                let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(measure))
                    .unwrap_or_else(|_| {
                        ServedCorpus::unknown("served-corpus probe worker panicked")
                    });
                {
                    let mut g = worker_flight
                        .value
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    *g = Some(value);
                }
                worker_flight.done.notify_all();
                let mut table = flights.table.lock().unwrap_or_else(|p| p.into_inner());
                if table
                    .get(&worker_key)
                    .is_some_and(|cur| std::sync::Arc::ptr_eq(cur, &worker_flight))
                {
                    table.remove(&worker_key);
                }
            });
        if spawned.is_err() {
            return ready(ServedCorpus::unknown(
                "served-corpus probe worker could not start",
            ));
        }
        // Inserted while the table lock is still held, so the worker's
        // removal (which takes the same lock) can never run first.
        table.insert(key, std::sync::Arc::clone(&f));
        f
    };
    drop(table);
    ProbeHandle {
        pending: Pending::InFlight(flight),
        wall,
        deadline_reason,
    }
}

/// [`start_bounded`] and an immediate [`ProbeHandle::join`]: what [`probe`]
/// does, over a test's own in-flight table.
#[cfg(test)]
fn probe_bounded(
    flights: &'static Flights,
    key: MemoKey,
    wall: Instant,
    deadline_reason: String,
    measure: impl FnOnce() -> ServedCorpus + Send + 'static,
) -> ServedCorpus {
    start_bounded(flights, key, wall, deadline_reason, measure).join()
}

/// The memo behind [`probe`]: `key`'s value when one was STORED within `ttl`,
/// else `measure()`'s, stored.
///
/// The entry is stamped when the measurement COMPLETES, not when it began: a
/// probe that took most of `ttl` must still be reused for `ttl` after it, or
/// the second render of the same spawn re-measures and the two copies can
/// disagree — the one thing the memo exists to prevent.
fn memoised(key: MemoKey, ttl: Duration, measure: impl FnOnce() -> ServedCorpus) -> ServedCorpus {
    {
        let guard = memo().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, value)) = guard.get(&key) {
            if at.elapsed() < ttl {
                return value.clone();
            }
        }
    }
    let value = measure();
    let done = Instant::now();
    let mut guard = memo().lock().unwrap_or_else(|p| p.into_inner());
    guard.retain(|_, (at, _)| done.duration_since(*at) < ttl);
    guard.insert(key, (done, value.clone()));
    value
}

/// [`probe`] for an async seam: the bounded `git` spawns run on the blocking
/// pool, never on a runtime worker, and hold that pool thread for at most
/// [`PROBE_BUDGET`] + [`PROBE_COLLECT_GRACE`].
pub(crate) async fn probe_async(workdir: impl Into<PathBuf>) -> ServedCorpus {
    let workdir = workdir.into();
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || probe(&workdir))
        .await
        .unwrap_or_else(|_| ServedCorpus::unknown("probe task failed"))
}

/// [`probe`] without the memo, with the `git` program, the probe's overall
/// budget and the loaded canonical snapshot as parameters, so a test can point
/// them at nothing, at a hang, or at a corpus of its own.
#[cfg(test)]
fn probe_with(
    workdir: &Path,
    git_program: &OsStr,
    budget: Duration,
    canonical: Option<&crate::canonical_corpus::CanonicalCorpus>,
) -> ServedCorpus {
    probe_with_opts(
        workdir,
        git_program,
        budget,
        Instant::now() + budget,
        std::process::Command::spawn,
        canonical,
    )
}

/// The measurement itself, with the `git` program, the absolute `git`
/// deadline, the process [`crate::process_deadline::Spawner`] and the loaded
/// canonical snapshot as parameters, so a test can point them at nothing, at
/// a hang, at slow process creation, or at a corpus of its own.
fn probe_with_opts(
    workdir: &Path,
    git_program: &OsStr,
    budget: Duration,
    deadline: Instant,
    spawner: crate::process_deadline::Spawner,
    canonical: Option<&crate::canonical_corpus::CanonicalCorpus>,
) -> ServedCorpus {
    let git = Git::until(git_program, budget, deadline, spawner);
    let claude = workdir.join(".claude");
    let corpus = match std::fs::canonicalize(&claude) {
        Ok(p) if p.is_dir() => Probe::Measured(p),
        Ok(_) => Probe::Unknown("not a directory".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if claude.symlink_metadata().is_ok() {
                Probe::Unknown("dangling symlink".to_string())
            } else {
                Probe::Unknown("absent".to_string())
            }
        }
        Err(e) => Probe::Unknown(format!("unreadable: {e}")),
    };

    let mut identity = bundle_identity(
        match &corpus {
            Probe::Measured(p) => Some(p.as_path()),
            Probe::Unknown(_) => None,
        },
        canonical,
    );

    let checkout = match &corpus {
        Probe::Measured(dir) => probe_checkout(&git, dir, DirtyScope::Subtree),
        Probe::Unknown(_) => Probe::Unknown("no served corpus".to_string()),
    };
    identity.in_checkout = match (&corpus, &checkout) {
        (Probe::Measured(dir), Probe::Measured(c)) => {
            bundle_in_checkout(&git, dir, c, &identity.stamped_rels)
        }
        (Probe::Unknown(_), _) => BundleInCheckout::Unknown(CountUnknown::NoServedCorpus),
        (Probe::Measured(_), Probe::Unknown(_)) => {
            BundleInCheckout::Unknown(CountUnknown::CheckoutUnknown)
        }
    };
    let bundle = Probe::Measured(identity);

    let cwd = probe_cwd(&git, workdir, &checkout);

    ServedCorpus {
        corpus,
        checkout,
        bundle,
        provisioned: provisioned_for(workdir),
        cwd,
    }
}

/// The `cwd` token: `same checkout` when the workdir's work tree is the served
/// corpus's, else the workdir measured on its own.
fn probe_cwd(
    git: &Git<'_>,
    workdir: &Path,
    corpus_checkout: &Probe<Checkout>,
) -> Probe<CwdCheckout> {
    let located = match locate(git, workdir) {
        Ok(l) => l,
        Err(reason) => return Probe::Unknown(reason),
    };
    let Some(located) = located else {
        return match corpus_checkout {
            Probe::Measured(Checkout::NotAWorkTree) => Probe::Measured(CwdCheckout::SameAsCorpus),
            _ => Probe::Measured(CwdCheckout::Other(Checkout::NotAWorkTree)),
        };
    };
    if let Probe::Measured(Checkout::Repo(r)) = corpus_checkout {
        if r.toplevel == located.toplevel {
            return Probe::Measured(CwdCheckout::SameAsCorpus);
        }
    }
    Probe::Measured(CwdCheckout::Other(Checkout::Repo(measure(
        git,
        workdir,
        located,
        DirtyScope::WholeTree,
    ))))
}

/// Which paths `dirty` counts.
#[derive(Clone, Copy)]
enum DirtyScope {
    /// Only under the probed directory — the served `.claude/`.
    Subtree,
    /// The whole work tree.
    WholeTree,
}

fn probe_checkout(git: &Git<'_>, dir: &Path, scope: DirtyScope) -> Probe<Checkout> {
    match locate(git, dir) {
        Ok(None) => Probe::Measured(Checkout::NotAWorkTree),
        Ok(Some(located)) => Probe::Measured(Checkout::Repo(measure(git, dir, located, scope))),
        Err(reason) => Probe::Unknown(reason),
    }
}

/// The bundle roster against the served checkout's git: which stamped files
/// it tracks (one `git ls-files`, run only when a stamped file exists) and
/// which roster paths the checkout's `git status` already reported changed.
fn bundle_in_checkout(
    git: &Git<'_>,
    corpus: &Path,
    checkout: &Checkout,
    stamped_rels: &[String],
) -> BundleInCheckout {
    let repo = match checkout {
        Checkout::NotAWorkTree => return BundleInCheckout::NotAWorkTree,
        Checkout::Repo(r) => r,
    };
    // Roster paths are `.claude/`-relative; git's are toplevel-relative.
    let Some(prefix) = corpus
        .strip_prefix(&repo.toplevel)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
    else {
        return BundleInCheckout::Unknown(CountUnknown::OutsideWorkTree);
    };
    let in_repo = |rel: &str| {
        if prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{prefix}/{rel}")
        }
    };

    let dirty_bundle = match &repo.changed {
        Probe::Measured(changed) => {
            let changed: std::collections::HashSet<&str> =
                changed.iter().map(String::as_str).collect();
            Ok(bundled_files()
                .iter()
                .filter(|f| changed.contains(in_repo(&f.rel).as_str()))
                .count())
        }
        Probe::Unknown(_) => Err(CountUnknown::StatusUnknown),
    };

    let stamped_tracked = if stamped_rels.is_empty() {
        Ok(0)
    } else {
        match git.run(corpus, &["ls-files", "-z", "--full-name", "--", "."]) {
            Err(e) => Err(CountUnknown::Git(e.code)),
            Ok(out) if !out.success => Err(CountUnknown::LsFilesFailed),
            Ok(out) => {
                let tracked: std::collections::HashSet<&str> =
                    out.stdout.split('\0').filter(|p| !p.is_empty()).collect();
                Ok(stamped_rels
                    .iter()
                    .filter(|rel| tracked.contains(in_repo(rel).as_str()))
                    .count())
            }
        }
    };

    BundleInCheckout::Repo {
        stamped_tracked,
        dirty_bundle,
    }
}

/// What one `git rev-parse` says about the work tree around a directory.
struct Located {
    toplevel: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    head: Probe<Head>,
}

/// `Ok(None)` when `dir` is not inside a git work tree.
fn locate(git: &Git<'_>, dir: &Path) -> Result<Option<Located>, String> {
    let out = git.run(
        dir,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ],
    )?;
    if !out.success && out.stderr.contains("not a git repository") {
        return Ok(None);
    }
    let lines: Vec<&str> = out.stdout.lines().collect();
    let (Some(top), Some(gd), Some(cd)) = (lines.first(), lines.get(1), lines.get(2)) else {
        return Err(format!("git rev-parse failed: {}", first_line(&out.stderr)));
    };
    let toplevel = std::fs::canonicalize(top).unwrap_or_else(|_| PathBuf::from(top));
    let git_dir = PathBuf::from(gd);
    // `--git-common-dir` is relative to the directory git ran in.
    let common_dir = {
        let p = PathBuf::from(cd);
        if p.is_absolute() {
            p
        } else {
            dir.join(p)
        }
    };
    let head = if out.success {
        match (lines.get(3), lines.get(4)) {
            (Some(sha), Some(branch)) => Probe::Measured(Head {
                sha: (*sha).to_string(),
                branch: (*branch != "HEAD").then(|| (*branch).to_string()),
            }),
            _ => Probe::Unknown("git rev-parse printed no HEAD".to_string()),
        }
    } else if out.stderr.contains("unknown revision") {
        Probe::Unknown("no commit on HEAD".to_string())
    } else {
        Probe::Unknown(format!("git rev-parse failed: {}", first_line(&out.stderr)))
    };
    Ok(Some(Located {
        toplevel,
        git_dir,
        common_dir,
        head,
    }))
}

fn measure(git: &Git<'_>, dir: &Path, located: Located, scope: DirtyScope) -> RepoState {
    let upstream_ref = upstream_ref(&located.common_dir);
    let upstream_short = upstream_ref
        .strip_prefix("refs/remotes/")
        .unwrap_or(&upstream_ref)
        .to_string();

    let upstream = match &located.head {
        Probe::Unknown(r) => Probe::Unknown(r.clone()),
        Probe::Measured(_) => {
            let range = format!("{upstream_ref}...HEAD");
            match git.run(dir, &["rev-list", "--count", "--left-right", &range]) {
                Err(e) => Probe::Unknown(e.reason),
                Ok(out) if !out.success => {
                    if out.stderr.contains("unknown revision")
                        || out.stderr.contains("bad revision")
                    {
                        Probe::Unknown(format!("no {upstream_short} ref"))
                    } else {
                        Probe::Unknown(format!("git rev-list failed: {}", first_line(&out.stderr)))
                    }
                }
                Ok(out) => {
                    let mut counts = out.stdout.split_whitespace().map(str::parse::<u64>);
                    match (counts.next(), counts.next()) {
                        (Some(Ok(behind)), Some(Ok(ahead))) => Probe::Measured(Upstream {
                            name: upstream_short.clone(),
                            behind,
                            ahead,
                        }),
                        _ => Probe::Unknown("git rev-list printed no counts".to_string()),
                    }
                }
            }
        }
    };

    let as_of = fetch_age(&located, &upstream_ref);

    // Tracked changes only, the cheapest status git offers: no untracked
    // walk, no rename detection, no descent into submodules. `-z` porcelain
    // paths are toplevel-relative and unquoted.
    const STATUS: &[&str] = &[
        "status",
        "--porcelain",
        "-z",
        "--untracked-files=no",
        "--no-renames",
        "--ignore-submodules=all",
    ];
    let mut status_args: Vec<&str> = STATUS.to_vec();
    if let DirtyScope::Subtree = scope {
        status_args.extend(["--", "."]);
    }
    let changed: Probe<Vec<String>> = match git.run(dir, &status_args) {
        Err(e) => Probe::Unknown(e.reason),
        Ok(out) if !out.success => {
            Probe::Unknown(format!("git status failed: {}", first_line(&out.stderr)))
        }
        // Each record is `XY <path>`.
        Ok(out) => Probe::Measured(
            out.stdout
                .split('\0')
                .filter_map(|r| r.get(3..))
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
        ),
    };
    let dirty = match &changed {
        Probe::Measured(paths) => Probe::Measured(paths.len()),
        Probe::Unknown(r) => Probe::Unknown(r.clone()),
    };

    RepoState {
        toplevel: located.toplevel,
        head: located.head,
        upstream,
        as_of,
        dirty,
        changed,
    }
}

/// The upstream ref to measure against: what `refs/remotes/origin/HEAD` names,
/// else `refs/remotes/origin/main`. A file read, not a spawn — `origin/HEAD`
/// is always a loose symbolic ref.
fn upstream_ref(common_dir: &Path) -> String {
    std::fs::read_to_string(common_dir.join("refs/remotes/origin/HEAD"))
        .ok()
        .and_then(|s| s.trim().strip_prefix("ref: ").map(str::to_string))
        .filter(|r| r.starts_with("refs/remotes/"))
        .unwrap_or_else(|| DEFAULT_UPSTREAM.to_string())
}

/// When the upstream ref was last fetched: the newest of `FETCH_HEAD`'s mtime
/// (per-worktree and common) and the newest reflog entry of the upstream ref.
/// A `stat` and a bounded file read — never a spawn, never a fetch.
fn fetch_age(located: &Located, upstream_ref: &str) -> Probe<String> {
    let mut newest: Option<SystemTime> = None;
    let mut consider = |t: SystemTime| {
        if newest.is_none_or(|n| t > n) {
            newest = Some(t);
        }
    };
    for dir in [&located.git_dir, &located.common_dir] {
        if let Ok(t) = std::fs::metadata(dir.join("FETCH_HEAD")).and_then(|m| m.modified()) {
            consider(t);
        }
    }
    if let Some(t) = newest_reflog_time(&located.common_dir.join("logs").join(upstream_ref)) {
        consider(t);
    }
    match newest {
        Some(t) => Probe::Measured(
            chrono::DateTime::<chrono::Utc>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        None => Probe::Unknown("never fetched".to_string()),
    }
}

/// The timestamp of the last entry of a reflog file, reading at most its last
/// 8 KiB. An entry is `<old> <new> <ident> <unix-ts> <tz>\t<message>`.
fn newest_reflog_time(path: &Path) -> Option<SystemTime> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 8 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let (head, _msg) = line.split_once('\t').unwrap_or((line, ""));
    let mut fields = head.split_whitespace().rev();
    let _tz = fields.next()?;
    let secs: u64 = fields.next()?.parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

// ── Bundle identity ─────────────────────────────────────────────────────────

/// Which bundled unit a file belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unit {
    /// `commands/<name>.md`.
    Command(&'static str),
    /// `skills/<name>/<rel>`.
    Skill { name: String, rel: String },
}

/// One file this binary carries.
#[derive(Debug, Clone)]
struct BundledFile {
    /// Path relative to `.claude/`.
    rel: String,
    /// This build's bytes.
    body: String,
    unit: Unit,
}

/// Every file this binary carries: each `FLEET_COMMANDS` entry as
/// `commands/<name>.md`, each embedded skill file as `skills/<name>/<rel>`.
/// Counted from the registries, never hard-coded.
fn bundled_files() -> Vec<BundledFile> {
    let mut out: Vec<BundledFile> = crate::fleet_commands::FLEET_COMMANDS
        .iter()
        .map(|(name, body)| BundledFile {
            rel: format!("commands/{name}.md"),
            body: (*body).to_string(),
            unit: Unit::Command(name),
        })
        .collect();
    for skill in crate::fleet_skills::embedded_skills() {
        for (rel, body) in skill.files {
            out.push(BundledFile {
                rel: format!("skills/{}/{rel}", skill.name),
                body,
                unit: Unit::Skill {
                    name: skill.name.clone(),
                    rel,
                },
            });
        }
    }
    out
}

/// CRLF → LF, so a checkout with `core.autocrlf` does not read as a fork.
fn normalize_eol(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// The skill manifest whose stamp a skill's other files inherit.
const SKILL_MANIFEST: &str = "SKILL.md";

/// The canonical body of `unit` at `corpus`'s snapshot, when it was loaded.
fn canonical_body<'c>(
    corpus: &'c crate::canonical_corpus::CanonicalCorpus,
    unit: &Unit,
) -> Option<&'c str> {
    match unit {
        Unit::Command(name) => corpus.commands.bodies.get(*name).map(String::as_str),
        Unit::Skill { name, rel } => corpus
            .skills
            .skills
            .get(name)
            .and_then(|s| s.files.get(rel))
            .map(String::as_str),
    }
}

/// Compare the served tree against the bundle and, per file, against the
/// source its stamp names. `None` (no served tree) counts every file as
/// missing, which is a measurement: `0/M`.
///
/// Zero network: `canonical` is the snapshot already in memory
/// (`canonical_corpus::latest`), never a fetch.
fn bundle_identity(
    corpus: Option<&Path>,
    canonical: Option<&crate::canonical_corpus::CanonicalCorpus>,
) -> BundleIdentity {
    let files = bundled_files();
    let total = files.len();
    let mut identical = 0;
    let mut stamped = 0;
    let mut stamped_rels = Vec::new();
    let mut sources = SourceIdentity {
        canonical_snapshot: canonical.map(|c| c.snapshot().clone()),
        ..SourceIdentity::default()
    };
    let Some(corpus) = corpus else {
        return BundleIdentity {
            identical,
            total,
            stamped,
            stamped_rels,
            in_checkout: BundleInCheckout::Unknown(CountUnknown::NoServedCorpus),
            sources,
        };
    };

    // Read every file once: (file, own stamp, body with the stamp removed).
    let present: Vec<(
        &BundledFile,
        Option<crate::provenance::ProvenanceLine>,
        String,
    )> = files
        .iter()
        .filter_map(|f| {
            let on_disk = std::fs::read_to_string(corpus.join(&f.rel)).ok()?;
            Some(match crate::provenance::strip_provenance(&on_disk) {
                Some((line, body)) => (f, Some(line), body),
                None => (f, None, on_disk),
            })
        })
        .collect();

    // A skill's helpers inherit its manifest's stamp.
    let skill_stamps: HashMap<&str, &crate::provenance::ProvenanceLine> = present
        .iter()
        .filter_map(|(f, line, _)| match (&f.unit, line) {
            (Unit::Skill { name, rel }, Some(line)) if rel == SKILL_MANIFEST => {
                Some((name.as_str(), line))
            }
            _ => None,
        })
        .collect();

    for (file, own, body) in &present {
        if own.is_some() {
            stamped += 1;
            stamped_rels.push(file.rel.clone());
        }
        let body = normalize_eol(body);
        let same_as_build = body == normalize_eol(&file.body);
        if same_as_build {
            identical += 1;
        }
        let stamp = own.as_ref().or_else(|| match &file.unit {
            Unit::Skill { name, .. } => skill_stamps.get(name.as_str()).copied(),
            Unit::Command(_) => None,
        });
        // `Some(matches)` when the file's own source is in memory to compare
        // against; `None` when it is not.
        let verdict: Option<bool> = match stamp.map(|l| l.source.as_str()) {
            None => {
                sources.unstamped += 1;
                Some(same_as_build)
            }
            Some("canonical") => {
                sources.canonical += 1;
                let stamp_sha = stamp.and_then(|l| l.canonical_sha.as_deref());
                canonical
                    .filter(|c| stamp_sha == Some(c.snapshot().short()))
                    .and_then(|c| canonical_body(c, &file.unit))
                    .map(|source| body == normalize_eol(source))
            }
            Some("builtin") => {
                sources.builtin += 1;
                // Another build's bundle is not in memory: its bytes can be
                // confirmed equal to this build's, never proven different.
                let this_build = stamp.is_some_and(|l| l.runner_build == RUNNER_BUILD);
                (same_as_build || this_build).then_some(same_as_build)
            }
            Some("served" | "disk_cache") => {
                sources.account += 1;
                continue;
            }
            Some(_) => None,
        };
        match verdict {
            Some(matches) => {
                sources.verified += 1;
                if matches {
                    sources.identical_to_source += 1;
                }
            }
            None => sources.unverifiable += 1,
        }
    }
    BundleIdentity {
        identical,
        total,
        stamped,
        stamped_rels,
        // Set by the caller, which alone holds the checkout to measure it in.
        in_checkout: BundleInCheckout::Unknown(CountUnknown::CheckoutUnknown),
        sources,
    }
}

// ── Bounded git ─────────────────────────────────────────────────────────────

/// Why one [`Git::run`] could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitErrorCode {
    /// `deadline`: the probe's budget was already spent, so git was not run.
    Deadline,
    /// `timed-out`: THIS git run was killed at the probe budget, or abandoned
    /// before it could start because process creation ate the budget (the
    /// reason text says which).
    TimedOut,
    /// `git-unavailable`: git could not be spawned.
    GitUnavailable,
    /// `output-incomplete`: git exited but its output could not be read whole.
    OutputIncomplete,
}

impl GitErrorCode {
    fn code(self) -> &'static str {
        match self {
            GitErrorCode::Deadline => "deadline",
            GitErrorCode::TimedOut => "timed-out",
            GitErrorCode::GitUnavailable => "git-unavailable",
            GitErrorCode::OutputIncomplete => "output-incomplete",
        }
    }
}

/// A [`Git::run`] failure: the code a roster count renders, and the reason an
/// `UNKNOWN (<reason>)` token renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitError {
    code: GitErrorCode,
    reason: String,
}

impl From<GitError> for String {
    fn from(e: GitError) -> String {
        e.reason
    }
}

/// The captured result of one completed `git` run.
struct GitOut {
    success: bool,
    stdout: String,
    stderr: String,
}

/// A `git` runner for ONE probe, under ONE absolute deadline that covers
/// process creation. Each spawn gets what is left of the budget, never a fresh
/// allowance, so however many questions the probe asks it returns within the
/// budget plus [`crate::process_deadline::KILL_GRACE`] (a run killed at the
/// deadline hands back within that grace). A spawn failure, and a spent
/// budget, are sticky: every later call fails at once — a spawn failure with
/// the same error, a spent budget as [`GitErrorCode::Deadline`].
struct Git<'a> {
    program: &'a OsStr,
    budget: Duration,
    deadline: Instant,
    spawner: crate::process_deadline::Spawner,
    dead: std::cell::RefCell<Option<GitError>>,
}

impl<'a> Git<'a> {
    /// A runner whose deadline is `budget` from now.
    #[cfg(test)]
    fn new(program: &'a OsStr, budget: Duration) -> Self {
        Self::until(
            program,
            budget,
            Instant::now() + budget,
            std::process::Command::spawn,
        )
    }

    /// A runner whose deadline is the absolute `deadline` (`budget` is what its
    /// reasons report), creating each child through `spawner`.
    fn until(
        program: &'a OsStr,
        budget: Duration,
        deadline: Instant,
        spawner: crate::process_deadline::Spawner,
    ) -> Self {
        Git {
            program,
            budget,
            deadline,
            spawner,
            dead: std::cell::RefCell::new(None),
        }
    }

    /// The error for a question the spent budget cut short: `code` says
    /// whether git was killed mid-run or never started.
    fn deadline_error(&self, code: GitErrorCode) -> GitError {
        GitError {
            code,
            reason: format!("deadline: git probe budget {:?} spent", self.budget),
        }
    }

    /// The command [`Self::run`] spawns. Built on the one scrubbed builder the
    /// canonical mirror uses too: an inherited repository environment
    /// (`GIT_DIR`, `GIT_OBJECT_DIRECTORY`, …) would make `-C` consult the
    /// WRONG repository or object store, and the stderr matches in the probe
    /// are git's untranslated C-locale messages.
    fn command(&self, dir: &Path, args: &[&str]) -> std::process::Command {
        let mut cmd = crate::process_helpers::scrubbed_git(self.program);
        cmd.arg("-C")
            .arg(dir)
            .arg("--literal-pathspecs")
            .args(args)
            // `git status` otherwise refreshes and rewrites the index: this
            // probe never writes.
            .env("GIT_OPTIONAL_LOCKS", "0");
        cmd
    }

    /// `git -C <dir> --literal-pathspecs <args…>`, bounded, read-only.
    ///
    /// `Err` carries a code for a roster count and a reason fit for an
    /// `UNKNOWN (…)` token.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<GitOut, GitError> {
        if let Some(e) = self.dead.borrow().as_ref() {
            return Err(e.clone());
        }
        let cmd = self.command(dir, args);
        // ONE absolute deadline, process creation included: the clock does
        // not wait for `spawn()` to return (plan
        // `2026-10-19-served-corpus-probe-budget-excludes-process-creation`).
        use crate::process_deadline::{DeadlineOutcome, NotStarted};
        let span = tracing::debug_span!(
            "served_corpus.git_run",
            spawn_ms = tracing::field::Empty,
            outcome = tracing::field::Empty
        )
        .entered();
        let result =
            crate::process_deadline::run_with_deadline_spawner(cmd, self.deadline, self.spawner);
        let outcome = match &result {
            Err(_) => "spawn-error",
            Ok(r) => match r.outcome {
                DeadlineOutcome::Completed(_) => "completed",
                DeadlineOutcome::TimedOut { .. } => "timed-out",
                DeadlineOutcome::NotStarted(NotStarted::SpawnPending) => "spawn-pending",
                DeadlineOutcome::NotStarted(NotStarted::DeadlinePassed) => "deadline",
            },
        };
        span.record("outcome", outcome);
        if let Some(d) = result.as_ref().ok().and_then(|r| r.spawn_elapsed) {
            span.record("spawn_ms", u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        }
        drop(span);
        match result {
            Err(e) => {
                let reason = if e.kind() == std::io::ErrorKind::NotFound {
                    "git unavailable".to_string()
                } else {
                    format!("git unavailable: {e}")
                };
                let e = GitError {
                    code: GitErrorCode::GitUnavailable,
                    reason,
                };
                *self.dead.borrow_mut() = Some(e.clone());
                Err(e)
            }
            Ok(run) => match run.outcome {
                DeadlineOutcome::NotStarted(NotStarted::DeadlinePassed) => {
                    let e = self.deadline_error(GitErrorCode::Deadline);
                    *self.dead.borrow_mut() = Some(e.clone());
                    Err(e)
                }
                DeadlineOutcome::NotStarted(NotStarted::SpawnPending) => {
                    // Git never started: process creation ate the budget. Same
                    // `timed-out` code (every consumer of the code grammar
                    // stays unchanged), with a reason that names the cause.
                    *self.dead.borrow_mut() = Some(self.deadline_error(GitErrorCode::Deadline));
                    Err(GitError {
                        code: GitErrorCode::TimedOut,
                        reason: format!(
                            "deadline: git probe budget {:?} spent waiting for process creation",
                            self.budget
                        ),
                    })
                }
                DeadlineOutcome::TimedOut { .. } => {
                    // This run was killed; every later one is never started.
                    *self.dead.borrow_mut() = Some(self.deadline_error(GitErrorCode::Deadline));
                    Err(self.deadline_error(GitErrorCode::TimedOut))
                }
                DeadlineOutcome::Completed(_) if run.truncation.is_some() => Err(GitError {
                    code: GitErrorCode::OutputIncomplete,
                    reason: "git output incomplete".to_string(),
                }),
                DeadlineOutcome::Completed(out) => Ok(GitOut {
                    success: out.status.success(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_commands::{AgentCommandRegistry, CommandSource};
    use crate::agent_skills::AgentSkillRegistry;
    use crate::provision_guard::test_support::assert_not_in_any_repo;
    use std::process::{Command, Stdio};

    /// Run git in `dir` with no dependence on the user's global hooks or
    /// signing config.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn total() -> usize {
        bundled_files().len()
    }

    /// The probe's git cannot be redirected at another repository or object
    /// store, speaks the C locale, and takes no optional lock.
    #[test]
    fn the_probe_git_is_scrubbed_and_lock_free() {
        let git = Git::new(OsStr::new("git"), Duration::from_secs(1));
        let cmd = git.command(Path::new("."), &["status"]);
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
        ] {
            assert_eq!(envs.get(OsStr::new(var)), Some(&None), "{var} is removed");
        }
        for (var, value) in [
            ("LC_ALL", "C"),
            ("LANGUAGE", "C"),
            ("GIT_OPTIONAL_LOCKS", "0"),
        ] {
            assert_eq!(
                envs.get(OsStr::new(var)),
                Some(&Some(OsStr::new(value))),
                "{var}={value}"
            );
        }
    }

    /// A committed checkout whose `.claude/` holds every bundled file verbatim.
    fn checkout_with_bundle() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "--quiet"]);
        for f in bundled_files() {
            let p = tmp.path().join(".claude").join(&f.rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, f.body).unwrap();
        }
        git(tmp.path(), &["add", "--", ".claude"]);
        git(
            tmp.path(),
            &["commit", "--quiet", "--no-verify", "-m", "bundle"],
        );
        tmp
    }

    fn line_for(dir: &Path) -> String {
        probe_with(dir, OsStr::new("git"), PROBE_BUDGET, None).render_line()
    }

    fn provision_both(claude: &Path) {
        crate::fleet_commands::provision_fleet_commands_into(
            &claude.join("commands"),
            &AgentCommandRegistry::new(),
        )
        .expect("provision commands");
        crate::fleet_skills::provision_fleet_skills_into(
            &claude.join("skills"),
            &AgentSkillRegistry::new(),
            None,
        )
        .expect("provision skills");
    }

    #[test]
    fn clobber_signature_is_detectable() {
        let tmp = checkout_with_bundle();
        let claude = tmp.path().join(".claude");
        let m = total();

        // The tracked-destination guard skips every file.
        provision_both(&claude);
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let sha12 = head.trim().get(..12).unwrap().to_string();

        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped=0 stamped-tracked=0 \
                 dirty-bundle=0 served:"
            )),
            "{line}"
        );
        assert!(line.contains(&format!("@{sha12} ")), "{line}");
        assert!(line.contains("dirty-claude=0]"), "{line}");
        assert!(line.contains("[cwd: same checkout]"), "{line}");
        assert!(
            line.contains(&format!(
                "builtin 0, account 0, unstamped {m}; identical-to-source {m}/{m} unverifiable=0]"
            )),
            "checkout source is unstamped and compared against the build: {line}"
        );

        // A peer edits one file.
        let first = bundled_files().into_iter().next().unwrap();
        let edited = claude.join(&first.rel);
        std::fs::write(&edited, format!("{}\npeer edit\n", first.body)).unwrap();
        let line = line_for(tmp.path());
        assert!(line.contains(&format!("[bundle: {}/{m} ", m - 1)), "{line}");
        assert!(line.contains("dirty-claude=1]"), "{line}");
        assert!(
            line.contains("stamped=0 stamped-tracked=0 dirty-bundle=1 served:"),
            "an edit to a bundled file is a bundle change: {line}"
        );

        // Revert it, and clobber a DIFFERENT file the way a pre-guard
        // provisioner would: the build's body plus its provenance key.
        std::fs::write(&edited, &first.body).unwrap();
        let (name, body) = crate::fleet_commands::FLEET_COMMANDS[1];
        std::fs::write(
            claude.join(format!("commands/{name}.md")),
            crate::provenance::with_provenance(
                &crate::provenance::command_canonical(name),
                body,
                CommandSource::Builtin.as_str(),
                None,
            ),
        )
        .unwrap();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped=1 stamped-tracked=1 \
                 dirty-bundle=1 served:"
            )),
            "a stamped TRACKED file is a clobber proven by the file itself: {line}"
        );
        assert!(line.contains("dirty-claude=1]"), "{line}");
    }

    /// The complete clobber signature, computed by the WRITER: every unit
    /// skipped as git-tracked, beside a bundle that is all this build's bytes,
    /// at the checkout's HEAD.
    #[test]
    fn the_ledger_names_the_tracked_skips_beside_the_bundle() {
        let tmp = checkout_with_bundle();
        let claude = tmp.path().join(".claude");
        let wd = tmp.path().to_string_lossy().into_owned();
        let commands = crate::fleet_commands::provision_fleet_commands_into(
            &claude.join("commands"),
            &AgentCommandRegistry::new(),
        )
        .expect("provision commands");
        let skills = crate::fleet_skills::provision_fleet_skills_into(
            &claude.join("skills"),
            &AgentSkillRegistry::new(),
            None,
        )
        .expect("provision skills");
        let (c_n, s_n) = (commands.expected, skills.expected);
        crate::capability_manifest::record_provision(&wd, commands);
        crate::capability_manifest::record_provision(&wd, skills);

        let m = total();
        let sha12 = git(tmp.path(), &["rev-parse", "HEAD"])
            .trim()
            .get(..12)
            .unwrap()
            .to_string();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[provisioned: commands written=0/{c_n} skipped-git-tracked={c_n} as-of="
            )),
            "{line}"
        );
        assert!(
            line.contains(&format!(
                "; skills written=0/{s_n} skipped-git-tracked={s_n} as-of="
            )),
            "{line}"
        );
        assert_eq!(
            c_n + s_n,
            m,
            "the ledger's roster and the bundle count the same files"
        );
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped=0 stamped-tracked=0 \
                 dirty-bundle=0 served:"
            )),
            "{line}"
        );
        assert!(line.contains(&format!("@{sha12} ")), "{line}");
    }

    #[test]
    fn no_ledger_entry_is_unknown_not_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let line = line_for(tmp.path());
        assert!(
            line.contains("[provisioned: UNKNOWN (no provision recorded for this workdir)]"),
            "{line}"
        );
    }

    #[test]
    fn peer_wip_is_not_a_clobber() {
        let tmp = checkout_with_bundle();
        for f in bundled_files() {
            std::fs::write(
                tmp.path().join(".claude").join(f.rel),
                format!("{}\npeer\n", f.body),
            )
            .unwrap();
        }
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: 0/{m} identical-to-build {BUILD_SHA} stamped=0 stamped-tracked=0 \
                 dirty-bundle={m} served:",
                m = total()
            )),
            "{line}"
        );
        assert!(
            line.contains(&format!("dirty-claude={}]", total())),
            "{line}"
        );
    }

    #[test]
    fn untracked_destination_after_provision_is_healthy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        provision_both(&tmp.path().join(".claude"));
        let m = total();
        // Every command body and every SKILL.md carries the key; a skill's
        // other files do not, and inherit their manifest's `source=`.
        let stamped = crate::fleet_commands::FLEET_COMMANDS.len()
            + crate::fleet_skills::embedded_skills().len();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped={stamped} \
                 stamped-tracked=n/a dirty-bundle=n/a served: canonical@unloaded 0, builtin {m}, account 0, unstamped 0; \
                 identical-to-source {m}/{m} unverifiable=0]"
            )),
            "{line}"
        );
        assert!(
            line.contains("[checkout: none (not a git work tree)]"),
            "{line}"
        );
        assert!(line.contains("[cwd: same checkout]"), "{line}");
    }

    // -- the roster counts' UNKNOWN codes --------------------------------------

    /// A checkout at `toplevel` whose `git status` read `changed`.
    fn repo_at(toplevel: &Path, changed: Probe<Vec<String>>) -> Checkout {
        Checkout::Repo(RepoState {
            toplevel: toplevel.to_path_buf(),
            head: Probe::Unknown("test".to_string()),
            upstream: Probe::Unknown("test".to_string()),
            as_of: Probe::Unknown("test".to_string()),
            dirty: Probe::Unknown("test".to_string()),
            changed,
        })
    }

    /// `bundle_in_checkout` for one stamped file, rendered.
    fn counts(git: &Git<'_>, corpus: &Path, checkout: &Checkout) -> String {
        bundle_in_checkout(git, corpus, checkout, &["commands/x.md".to_string()]).render()
    }

    #[test]
    fn every_count_unknown_renders_as_one_spaceless_code() {
        let cases = [
            (CountUnknown::NoServedCorpus, "no-served-corpus"),
            (CountUnknown::CheckoutUnknown, "checkout-unknown"),
            (CountUnknown::OutsideWorkTree, "outside-work-tree"),
            (CountUnknown::StatusUnknown, "status-unknown"),
            (CountUnknown::LsFilesFailed, "ls-files-failed"),
            (CountUnknown::Git(GitErrorCode::Deadline), "deadline"),
            (CountUnknown::Git(GitErrorCode::TimedOut), "timed-out"),
            (
                CountUnknown::Git(GitErrorCode::GitUnavailable),
                "git-unavailable",
            ),
            (
                CountUnknown::Git(GitErrorCode::OutputIncomplete),
                "output-incomplete",
            ),
        ];
        for (why, code) in cases {
            assert_eq!(
                BundleInCheckout::Unknown(why).render(),
                format!("stamped-tracked=UNKNOWN({code}) dirty-bundle=UNKNOWN({code})")
            );
            assert!(code.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        }
    }

    #[test]
    fn a_corpus_outside_its_toplevel_is_outside_work_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let git = Git::new(OsStr::new("git"), PROBE_BUDGET);
        let checkout = repo_at(elsewhere.path(), Probe::Measured(vec![]));
        assert_eq!(
            counts(&git, tmp.path(), &checkout),
            "stamped-tracked=UNKNOWN(outside-work-tree) dirty-bundle=UNKNOWN(outside-work-tree)"
        );
    }

    #[test]
    fn an_unreadable_status_is_status_unknown_and_a_failed_ls_files_is_ls_files_failed() {
        // Not a repository, so `git ls-files` exits non-zero.
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        let git = Git::new(OsStr::new("git"), PROBE_BUDGET);
        let checkout = repo_at(tmp.path(), Probe::Unknown("git status failed".to_string()));
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        assert_eq!(
            counts(&git, &tmp.path().join(".claude"), &checkout),
            "stamped-tracked=UNKNOWN(ls-files-failed) dirty-bundle=UNKNOWN(status-unknown)"
        );
    }

    #[test]
    fn a_git_that_cannot_answer_ls_files_names_why() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let checkout = repo_at(tmp.path(), Probe::Measured(vec![]));
        let corpus = tmp.path().join(".claude");

        let nowhere = tmp.path().join("no-such-git");
        let git = Git::new(nowhere.as_os_str(), PROBE_BUDGET);
        assert_eq!(
            counts(&git, &corpus, &checkout),
            "stamped-tracked=UNKNOWN(git-unavailable) dirty-bundle=0"
        );

        let spent = Git::new(OsStr::new("git"), Duration::ZERO);
        assert_eq!(
            counts(&spent, &corpus, &checkout),
            "stamped-tracked=UNKNOWN(deadline) dirty-bundle=0"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_ls_files_killed_at_the_budget_is_timed_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = tmp.path().join("slow-git");
        crate::canonical_corpus::test_support::executable_script(
            &script,
            "#!/bin/sh\nexec sleep 5\n",
        );
        let git = Git::new(script.as_os_str(), Duration::from_millis(200));
        let checkout = repo_at(tmp.path(), Probe::Measured(vec![]));
        assert_eq!(
            counts(&git, &tmp.path().join(".claude"), &checkout),
            "stamped-tracked=UNKNOWN(timed-out) dirty-bundle=0"
        );
        // The run after it was never started: that is the deadline.
        assert!(matches!(
            git.run(tmp.path(), &["status"]),
            Err(GitError {
                code: GitErrorCode::Deadline,
                ..
            })
        ));
    }

    #[test]
    fn a_missing_claude_dir_is_unknown_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let line = line_for(tmp.path());
        assert!(
            line.starts_with("[served-corpus: UNKNOWN (absent)] "),
            "{line}"
        );
        assert!(
            line.contains("[checkout: UNKNOWN (no served corpus)]"),
            "{line}"
        );
        assert!(line.contains(&format!("[bundle: 0/{} ", total())), "{line}");
        assert!(
            line.contains(
                "stamped-tracked=UNKNOWN(no-served-corpus) dirty-bundle=UNKNOWN(no-served-corpus)"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_missing_git_binary_is_unknown_but_the_bundle_is_still_measured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let nowhere = tmp.path().join("no-such-git");
        let line = probe_with(tmp.path(), nowhere.as_os_str(), PROBE_BUDGET, None).render_line();
        assert!(
            line.contains("[checkout: UNKNOWN (git unavailable)]"),
            "{line}"
        );
        assert!(line.contains("[cwd: UNKNOWN (git unavailable)]"), "{line}");
        assert!(line.contains(&format!("[bundle: 0/{} ", total())), "{line}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_git_times_out_once_and_bounds_the_whole_probe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let script = tmp.path().join("slow-git");
        crate::canonical_corpus::test_support::executable_script(
            &script,
            "#!/bin/sh\nexec sleep 5\n",
        );

        let budget = Duration::from_millis(200);
        let started = Instant::now();
        let line = probe_with(tmp.path(), script.as_os_str(), budget, None).render_line();
        let elapsed = started.elapsed();
        assert!(
            line.contains("[checkout: UNKNOWN (deadline: git probe budget 200ms spent)]"),
            "{line}"
        );
        assert!(
            line.contains("[cwd: UNKNOWN (deadline: git probe budget 200ms spent)]"),
            "{line}"
        );
        assert!(
            line.contains(
                "stamped-tracked=UNKNOWN(checkout-unknown) dirty-bundle=UNKNOWN(checkout-unknown)"
            ),
            "git data that could not be read is UNKNOWN, never 0: {line}"
        );
        assert!(
            elapsed < budget * 2,
            "a hung git must cost one budget, not one per question: took {elapsed:?}"
        );
    }

    /// Several git calls that are each slow but individually fast enough
    /// still share ONE deadline: the probe returns within its budget and the
    /// questions it had no time left for read UNKNOWN (deadline).
    #[cfg(unix)]
    #[test]
    fn slow_git_calls_share_one_deadline() {
        let tmp = checkout_with_bundle();
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("slow-git");
        crate::canonical_corpus::test_support::executable_script(
            &script,
            "#!/bin/sh\nsleep 0.15\nexec git \"$@\"\n",
        );
        // Four spawns (locate, rev-list, status, cwd locate) at 150 ms each
        // need 600 ms; the budget is 400 ms.
        let budget = Duration::from_millis(400);
        let started = Instant::now();
        let line = probe_with(tmp.path(), script.as_os_str(), budget, None).render_line();
        let elapsed = started.elapsed();
        assert!(
            elapsed < budget + Duration::from_millis(300),
            "the probe must end near its budget, not at the sum of its calls: {elapsed:?}"
        );
        assert!(
            line.contains("UNKNOWN (deadline: git probe budget 400ms spent)"),
            "{line}"
        );
    }

    #[test]
    fn never_fetches_never_writes() {
        let tmp = checkout_with_bundle();
        git(
            tmp.path(),
            &["remote", "add", "origin", "/nonexistent/qontinui-origin"],
        );
        let index = std::fs::read(tmp.path().join(".git/index")).unwrap();
        let status_before = git(tmp.path(), &["status", "--porcelain"]);

        let line = line_for(tmp.path());
        assert!(line.contains("as-of=UNKNOWN (never fetched)"), "{line}");
        assert!(
            line.contains("upstream=UNKNOWN (no origin/main ref)"),
            "{line}"
        );

        assert_eq!(git(tmp.path(), &["status", "--porcelain"]), status_before);
        assert_eq!(std::fs::read(tmp.path().join(".git/index")).unwrap(), index);
        assert!(!tmp.path().join(".git/FETCH_HEAD").exists());
    }

    #[test]
    fn behind_and_as_of_are_read_from_the_local_remote_ref() {
        let origin = checkout_with_bundle();
        git(origin.path(), &["branch", "-M", "main"]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let clone = tmp.path().join("clone");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                &origin.path().display().to_string(),
                "clone",
            ],
        );
        git(
            origin.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                "more",
            ],
        );
        git(&clone, &["fetch", "--quiet", "origin"]);

        let line = line_for(&clone);
        assert!(
            line.contains("upstream=origin/main behind=1 ahead=0"),
            "{line}"
        );
        assert!(!line.contains("as-of=UNKNOWN"), "{line}");
    }

    #[cfg(unix)]
    #[test]
    fn a_workdir_in_another_checkout_gets_its_own_cwd_token() {
        let corpus_repo = checkout_with_bundle();
        let work = tempfile::tempdir().expect("tempdir");
        git(work.path(), &["init", "--quiet"]);
        git(
            work.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                "w",
            ],
        );
        std::fs::write(work.path().join("f.txt"), "x").unwrap();
        git(work.path(), &["add", "--", "f.txt"]);
        std::os::unix::fs::symlink(
            corpus_repo.path().join(".claude"),
            work.path().join(".claude"),
        )
        .unwrap();

        let line = line_for(work.path());
        let canonical = std::fs::canonicalize(corpus_repo.path().join(".claude")).unwrap();
        assert!(
            line.starts_with(&format!("[served-corpus: {}] ", canonical.display())),
            "{line}"
        );
        assert!(line.contains("dirty=1]"), "{line}");
        assert!(!line.contains("[cwd: same checkout]"), "{line}");
    }

    /// A loaded canonical snapshot whose `.claude/commands/<name>.md` body is
    /// `body` for the first bundled command, and no skills.
    fn canonical_corpus_with(
        name: &str,
        body: &str,
        sha: &str,
    ) -> crate::canonical_corpus::CanonicalCorpus {
        use crate::canonical_corpus::{
            CanonicalCommands, CanonicalCorpus, CanonicalSkills, CanonicalSnapshot,
        };
        let snapshot = CanonicalSnapshot {
            sha: sha.to_string(),
            fetched_at: "2026-09-26T00:00:00Z".to_string(),
        };
        CanonicalCorpus {
            commands: CanonicalCommands {
                snapshot: snapshot.clone(),
                bodies: [(name.to_string(), body.to_string())].into_iter().collect(),
            },
            skills: CanonicalSkills {
                snapshot,
                skills: Default::default(),
            },
        }
    }

    /// Write ONE command into an otherwise empty `.claude/`, stamped as `source`.
    fn served_one(source: &str, canonical_sha: Option<&str>, body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (name, _) = crate::fleet_commands::FLEET_COMMANDS[0];
        let dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.md")),
            crate::provenance::with_provenance(
                &crate::provenance::command_canonical(name),
                body,
                source,
                canonical_sha,
            ),
        )
        .unwrap();
        tmp
    }

    const CANON_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

    #[test]
    fn a_canonical_stamped_file_is_verified_against_the_loaded_snapshot() {
        let (name, _) = crate::fleet_commands::FLEET_COMMANDS[0];
        let body = "---\ndescription: canonical moved ahead of the build\n---\n# body\n";
        let corpus = canonical_corpus_with(name, body, CANON_SHA);
        let tmp = served_one("canonical", Some(CANON_SHA.get(..12).unwrap()), body);
        let line =
            probe_with(tmp.path(), OsStr::new("git"), PROBE_BUDGET, Some(&corpus)).render_line();
        assert!(
            line.contains(&format!(
                "[bundle: 0/{} identical-to-build {BUILD_SHA} stamped=1 stamped-tracked=n/a \
                 dirty-bundle=n/a served: canonical@{} 1 fetched 2026-09-26T00:00:00Z, builtin 0, account 0, \
                 unstamped 0; identical-to-source 1/1 unverifiable=0]",
                total(),
                CANON_SHA.get(..12).unwrap()
            )),
            "not the build's bytes, but exactly the canonical snapshot's: {line}"
        );

        // An edit after provisioning is a verified mismatch, not unverifiable.
        let path = tmp.path().join(format!(".claude/commands/{name}.md"));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}edited\n")).unwrap();
        let line =
            probe_with(tmp.path(), OsStr::new("git"), PROBE_BUDGET, Some(&corpus)).render_line();
        assert!(
            line.contains("identical-to-source 0/1 unverifiable=0]"),
            "{line}"
        );
    }

    #[test]
    fn a_canonical_stamp_from_another_snapshot_is_unverifiable_not_identical() {
        let (name, _) = crate::fleet_commands::FLEET_COMMANDS[0];
        let body = "---\ndescription: x\n---\n# body\n";
        // The loaded snapshot holds the SAME bytes, but the stamp names a
        // different generation: identity is never inferred across snapshots.
        let corpus = canonical_corpus_with(name, body, CANON_SHA);
        let tmp = served_one("canonical", Some("0123456789ab"), body);
        let line =
            probe_with(tmp.path(), OsStr::new("git"), PROBE_BUDGET, Some(&corpus)).render_line();
        assert!(line.contains("canonical@abcdef012345 1 fetched"), "{line}");
        assert!(
            line.contains("identical-to-source 0/0 unverifiable=1]"),
            "{line}"
        );

        // With no snapshot loaded at all, the same file is unverifiable too.
        let line = probe_with(tmp.path(), OsStr::new("git"), PROBE_BUDGET, None).render_line();
        assert!(
            line.contains("served: canonical@unloaded 1, builtin 0"),
            "{line}"
        );
        assert!(
            line.contains("identical-to-source 0/0 unverifiable=1]"),
            "{line}"
        );
    }

    #[test]
    fn an_account_file_is_counted_as_account_and_never_compared() {
        let tmp = served_one("served", None, "# an account override\n");
        let line = line_for(tmp.path());
        assert!(
            line.contains(
                "served: canonical@unloaded 0, builtin 0, account 1, unstamped 0; \
                 identical-to-source 0/0 unverifiable=0]"
            ),
            "{line}"
        );
    }

    #[test]
    fn a_skills_helpers_inherit_its_manifests_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skills = tmp.path().join(".claude/skills");
        crate::fleet_skills::provision_fleet_skills_into(&skills, &AgentSkillRegistry::new(), None)
            .expect("provision skills");
        let skill_files = crate::fleet_skills::embedded_skills()
            .iter()
            .map(|s| s.files.len())
            .sum::<usize>();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "builtin {skill_files}, account 0, unstamped 0; \
                 identical-to-source {skill_files}/{skill_files} unverifiable=0]"
            )),
            "{line}"
        );
    }

    /// A repo whose `.claude/` is NOT tracked gets an ordinary stamped
    /// provision: stamped files, none of them tracked.
    #[test]
    fn an_untracked_stamped_provision_is_not_a_clobber() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "--quiet"]);
        std::fs::write(tmp.path().join("README.md"), "x").unwrap();
        git(tmp.path(), &["add", "--", "README.md"]);
        git(tmp.path(), &["commit", "--quiet", "--no-verify", "-m", "r"]);
        provision_both(&tmp.path().join(".claude"));
        let line = line_for(tmp.path());
        let stamped = crate::fleet_commands::FLEET_COMMANDS.len()
            + crate::fleet_skills::embedded_skills().len();
        assert!(
            line.contains(&format!(
                "stamped={stamped} stamped-tracked=0 dirty-bundle=0 served:"
            )),
            "{line}"
        );
    }

    /// An edit under `.claude/` to a file the bundle does not carry moves
    /// `dirty-claude`, never `dirty-bundle`.
    #[test]
    fn an_unrelated_tracked_edit_is_not_a_bundle_change() {
        let tmp = checkout_with_bundle();
        let settings = tmp.path().join(".claude/settings.json");
        std::fs::write(&settings, "{}\n").unwrap();
        git(tmp.path(), &["add", "--", ".claude/settings.json"]);
        git(tmp.path(), &["commit", "--quiet", "--no-verify", "-m", "s"]);
        std::fs::write(&settings, "{\"x\": 1}\n").unwrap();
        let line = line_for(tmp.path());
        assert!(line.contains("dirty-claude=1]"), "{line}");
        assert!(line.contains("dirty-bundle=0 served:"), "{line}");
    }

    /// A branch may be named `feat]x`; the rendered value must not close the
    /// `checkout` token early.
    #[test]
    fn a_bracket_in_a_branch_name_cannot_close_the_token() {
        let tmp = checkout_with_bundle();
        git(tmp.path(), &["checkout", "--quiet", "-b", "feat]x"]);
        let line = line_for(tmp.path());
        assert!(line.contains("[checkout: "), "{line}");
        assert!(line.contains(" feat)x@"), "{line}");
        assert!(!line.contains("feat]x"), "{line}");
        let checkout = line
            .split_once("[checkout: ")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(value, _)| value)
            .unwrap();
        assert!(checkout.ends_with("dirty-claude=0"), "{checkout}");
    }

    /// A memo entry is stamped when its measurement COMPLETES: a measurement
    /// slower than the TTL is still reused right after it.
    #[test]
    fn the_memo_is_stamped_after_the_probe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "memo-stamp-test");
        let ttl = Duration::from_millis(300);
        let calls = std::cell::Cell::new(0);
        let slow = || {
            calls.set(calls.get() + 1);
            std::thread::sleep(ttl + Duration::from_millis(100));
            ServedCorpus::unknown("slow")
        };
        memoised(key.clone(), ttl, slow);
        memoised(key.clone(), ttl, || {
            calls.set(calls.get() + 1);
            ServedCorpus::unknown("second")
        });
        assert_eq!(
            calls.get(),
            1,
            "the second render within the TTL of the first's COMPLETION reuses it"
        );
    }

    #[test]
    fn memo_agrees_within_window() {
        let tmp = checkout_with_bundle();
        let first = probe(tmp.path()).render_line();
        // A change inside the window is not seen: the argv copy rendered before
        // provisioning and the env copy rendered after must agree.
        let rel = bundled_files().into_iter().next().unwrap().rel;
        std::fs::write(tmp.path().join(".claude").join(rel), "changed").unwrap();
        assert_eq!(probe(tmp.path()).render_line(), first);
    }

    // ── Process creation inside the budget (plan
    // `2026-10-19-served-corpus-probe-budget-excludes-process-creation`) ──

    const SLOW_SPAWN_BUDGET: Duration = Duration::from_millis(400);

    /// Process creation that costs 3x the whole budget, per spawn.
    fn slow_spawner(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
        std::thread::sleep(SLOW_SPAWN_BUDGET * 3);
        cmd.spawn()
    }

    /// THE defect, as a regression test: every `git` spawn takes 3x the
    /// budget to be created. The probe still returns inside budget + collect
    /// grace, every git-derived value is UNKNOWN (never a measured-looking
    /// default), and what needs no git — the corpus path and the bundle
    /// identity — is still measured.
    #[test]
    fn a_slow_process_creation_cannot_stretch_the_probe() {
        let tmp = checkout_with_bundle();
        let started = Instant::now();
        let line = probe_with_opts(
            tmp.path(),
            OsStr::new("git"),
            SLOW_SPAWN_BUDGET,
            started + SLOW_SPAWN_BUDGET,
            slow_spawner,
            None,
        )
        .render_line();
        let elapsed = started.elapsed();
        assert!(
            elapsed < SLOW_SPAWN_BUDGET + PROBE_COLLECT_GRACE + Duration::from_millis(250),
            "process creation stretched the probe to {elapsed:?}"
        );
        // Normally the first git is the one stuck in process creation and
        // says so; on a box loaded enough that the non-git work before it
        // (the bundle identity) spends the budget first, it reads the plain
        // deadline instead. Either way it is UNKNOWN on the budget.
        assert!(
            line.contains("[checkout: UNKNOWN (deadline: git probe budget 400ms spent"),
            "{line}"
        );
        assert!(
            line.contains("[cwd: UNKNOWN (deadline: git probe budget 400ms spent)]"),
            "{line}"
        );
        assert!(
            line.contains(
                "stamped-tracked=UNKNOWN(checkout-unknown) dirty-bundle=UNKNOWN(checkout-unknown)"
            ),
            "{line}"
        );
        assert!(!line.starts_with("[served-corpus: UNKNOWN"), "{line}");
        assert!(
            line.contains(&format!("[bundle: {}/{} ", total(), total())),
            "{line}"
        );
    }

    /// The spawn that could not start before the deadline keeps the existing
    /// `timed-out` code (D3), so the roster-count grammar is unchanged.
    #[test]
    fn a_git_that_never_started_reads_timed_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let checkout = repo_at(tmp.path(), Probe::Measured(vec![]));
        let started = Instant::now();
        let git = Git::until(
            OsStr::new("git"),
            SLOW_SPAWN_BUDGET,
            started + SLOW_SPAWN_BUDGET,
            slow_spawner,
        );
        assert_eq!(
            counts(&git, &tmp.path().join(".claude"), &checkout),
            "stamped-tracked=UNKNOWN(timed-out) dirty-bundle=0"
        );
        assert!(started.elapsed() < SLOW_SPAWN_BUDGET + Duration::from_millis(250));

        // The reason names process creation. Nothing runs before this first
        // git call on a fresh runner, so the spawn, not the budget, is what
        // ran out.
        let fresh = Git::until(
            OsStr::new("git"),
            SLOW_SPAWN_BUDGET,
            Instant::now() + SLOW_SPAWN_BUDGET,
            slow_spawner,
        );
        match fresh.run(tmp.path(), &["status"]) {
            Err(GitError {
                code: GitErrorCode::TimedOut,
                reason,
            }) => assert!(
                reason.ends_with("spent waiting for process creation"),
                "{reason}"
            ),
            Err(other) => panic!("expected TimedOut, got {other:?}"),
            Ok(_) => panic!("expected TimedOut, git ran"),
        }

        // Sticky: the next question is not even asked.
        assert!(matches!(
            git.run(tmp.path(), &["status"]),
            Err(GitError {
                code: GitErrorCode::Deadline,
                ..
            })
        ));
    }

    fn test_flights(cap: usize) -> &'static Flights {
        Box::leak(Box::new(Flights {
            table: Mutex::new(HashMap::new()),
            cap,
        }))
    }

    /// The outer wall: a measurement that does not come back in time (here:
    /// non-git I/O that hangs) leaves the caller with a one-token UNKNOWN at
    /// budget + grace, and that UNKNOWN is never memoised.
    #[test]
    fn the_caller_is_bounded_even_when_the_measurement_is_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "outer-wall-test");
        let memo_key = key.clone();
        let budget = Duration::from_millis(400);
        let started = Instant::now();
        let got = probe_bounded(
            test_flights(4),
            key.clone(),
            started + budget + PROBE_COLLECT_GRACE,
            format!("deadline: served-corpus probe budget {budget:?} spent"),
            move || {
                memoised(memo_key, MEMO_TTL, || {
                    std::thread::sleep(Duration::from_secs(3));
                    ServedCorpus::unknown("measured too late")
                })
            },
        );
        let elapsed = started.elapsed();
        assert_eq!(
            got.render_line(),
            "[served-corpus: UNKNOWN (deadline: served-corpus probe budget 400ms spent)]"
        );
        assert!(
            elapsed < budget + PROBE_COLLECT_GRACE + Duration::from_millis(250),
            "{elapsed:?}"
        );
        let memo_has_it = memo()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&key);
        assert!(!memo_has_it, "the deadline UNKNOWN must never be memoised");
    }

    /// The outer wall waits long enough to collect a measurement whose git
    /// was KILLED at the deadline: the caller gets the partial measurement
    /// (git questions UNKNOWN on the budget, the bundle measured), never the
    /// one-token deadline UNKNOWN. A SIGKILLed `sleep` hands its run back in
    /// milliseconds, so this would also pass with a too-short grace; the const
    /// assert beside [`PROBE_COLLECT_GRACE`] is what holds the grace above the
    /// kill grace.
    #[cfg(unix)]
    #[test]
    fn the_outer_wall_collects_a_measurement_whose_git_was_killed() {
        let tmp = checkout_with_bundle();
        let script = tmp.path().join("hung-git");
        crate::canonical_corpus::test_support::executable_script(
            &script,
            "#!/bin/sh\nexec sleep 5\n",
        );
        let budget = Duration::from_millis(300);
        let deadline = Instant::now() + budget;
        let workdir = tmp.path().to_path_buf();
        let got = probe_bounded(
            test_flights(4),
            (workdir.clone(), "killed-git-collect-test"),
            deadline + PROBE_COLLECT_GRACE,
            "deadline: outer wall".to_string(),
            move || {
                probe_with_opts(
                    &workdir,
                    script.as_os_str(),
                    budget,
                    deadline,
                    std::process::Command::spawn,
                    None,
                )
            },
        )
        .render_line();
        assert!(!got.starts_with("[served-corpus: UNKNOWN"), "{got}");
        assert!(
            got.contains("[checkout: UNKNOWN (deadline: git probe budget 300ms spent)]"),
            "{got}"
        );
        assert!(
            got.contains(&format!("[bundle: {}/{} ", total(), total())),
            "{got}"
        );
    }

    /// Two concurrent probes of one workdir run ONE measurement.
    #[test]
    fn concurrent_probes_of_one_workdir_share_one_measurement() {
        static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "single-flight-test");
        let call = |key: MemoKey| {
            std::thread::spawn(move || {
                probe_bounded(
                    flights,
                    key,
                    Instant::now() + Duration::from_secs(10),
                    "deadline".to_string(),
                    || {
                        RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(300));
                        ServedCorpus::unknown("one measurement")
                    },
                )
            })
        };
        let a = call(key.clone());
        std::thread::sleep(Duration::from_millis(50));
        let b = call(key);
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        assert_eq!(a, b);
        assert_eq!(
            a.render_line(),
            "[served-corpus: UNKNOWN (one measurement)]"
        );
        assert_eq!(RUNS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A measurement abandoned at the wall still completes, and the next
    /// probe inside the memo window reads ITS result, not a stale default.
    #[test]
    fn a_late_measurement_is_served_to_the_next_probe() {
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "late-completion-test");
        let k1 = key.clone();
        let first = probe_bounded(
            flights,
            key.clone(),
            Instant::now() + Duration::from_millis(200),
            "deadline: first probe".to_string(),
            move || {
                memoised(k1, MEMO_TTL, || {
                    std::thread::sleep(Duration::from_millis(600));
                    ServedCorpus::unknown("late measurement")
                })
            },
        );
        assert_eq!(
            first.render_line(),
            "[served-corpus: UNKNOWN (deadline: first probe)]"
        );
        // Let the abandoned worker finish and leave the table.
        let end = Instant::now() + Duration::from_secs(10);
        while flights
            .table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&key)
        {
            assert!(Instant::now() < end, "the abandoned worker never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
        let k2 = key.clone();
        let second = probe_bounded(
            flights,
            key,
            Instant::now() + Duration::from_secs(5),
            "deadline: second probe".to_string(),
            move || memoised(k2, MEMO_TTL, || ServedCorpus::unknown("re-measured")),
        );
        assert_eq!(
            second.render_line(),
            "[served-corpus: UNKNOWN (late measurement)]"
        );
    }

    /// Past the capacity a probe answers UNKNOWN at once, starting nothing.
    #[test]
    fn a_probe_past_capacity_answers_at_once() {
        let flights = test_flights(2);
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..2 {
            // Each call gives up almost at once; its worker stays in flight.
            let _ = probe_bounded(
                flights,
                (tmp.path().join(format!("busy-{i}")), "capacity-test"),
                Instant::now() + Duration::from_millis(5),
                "deadline".to_string(),
                || {
                    std::thread::sleep(Duration::from_secs(2));
                    ServedCorpus::unknown("busy")
                },
            );
        }
        let started = Instant::now();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_in = std::sync::Arc::clone(&ran);
        let got = probe_bounded(
            flights,
            (tmp.path().join("third"), "capacity-test"),
            Instant::now() + Duration::from_secs(5),
            "deadline".to_string(),
            move || {
                ran_in.store(true, std::sync::atomic::Ordering::SeqCst);
                ServedCorpus::unknown("should not run")
            },
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(
            got.render_line(),
            "[served-corpus: UNKNOWN (served-corpus probe capacity: measurements still in flight)]"
        );
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    // ── Phase 5: the measurement overlaps the caller's other work ──

    /// Started, then the caller does 300 ms of other work, then joins: a
    /// measurement shorter than that work is already done, so the join costs
    /// (almost) nothing — the overlap, proven by elapsed time.
    #[test]
    fn a_join_after_other_work_waits_only_for_what_is_left() {
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let handle = start_bounded(
            flights,
            (tmp.path().to_path_buf(), "overlap-test"),
            Instant::now() + Duration::from_secs(5),
            "deadline".to_string(),
            || {
                std::thread::sleep(Duration::from_millis(100));
                ServedCorpus::unknown("overlapped")
            },
        );
        std::thread::sleep(Duration::from_millis(600));
        let joined_at = Instant::now();
        let got = handle.join();
        assert!(
            joined_at.elapsed() <= Duration::from_millis(100),
            "join waited {:?} for a measurement that should already be done",
            joined_at.elapsed()
        );
        assert_eq!(got.render_line(), "[served-corpus: UNKNOWN (overlapped)]");
    }

    /// The wall is anchored at START: a handle joined after it returns at
    /// once — the measurement when it finished, the deadline UNKNOWN when it
    /// did not.
    #[test]
    fn a_handle_joined_after_its_wall_returns_at_once() {
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let wall = Duration::from_millis(150);

        let finished = start_bounded(
            flights,
            (tmp.path().join("finished"), "late-join-test"),
            Instant::now() + wall,
            "deadline: finished".to_string(),
            || ServedCorpus::unknown("finished in time"),
        );
        let unfinished = start_bounded(
            flights,
            (tmp.path().join("unfinished"), "late-join-test"),
            Instant::now() + wall,
            "deadline: unfinished".to_string(),
            || {
                std::thread::sleep(Duration::from_secs(2));
                ServedCorpus::unknown("too late")
            },
        );
        std::thread::sleep(wall + Duration::from_millis(100));

        let t = Instant::now();
        assert_eq!(
            finished.join().render_line(),
            "[served-corpus: UNKNOWN (finished in time)]"
        );
        assert_eq!(
            unfinished.join().render_line(),
            "[served-corpus: UNKNOWN (deadline: unfinished)]"
        );
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    /// A handle that is dropped unjoined leaves its worker to finish and
    /// memoise, so the next probe inside the memo window reads that result.
    #[test]
    fn a_dropped_handle_still_memoises_its_measurement() {
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "dropped-handle-test");
        let k1 = key.clone();
        drop(start_bounded(
            flights,
            key.clone(),
            Instant::now() + Duration::from_secs(5),
            "deadline".to_string(),
            move || {
                memoised(k1, MEMO_TTL, || {
                    std::thread::sleep(Duration::from_millis(100));
                    ServedCorpus::unknown("measured with nobody waiting")
                })
            },
        ));
        let end = Instant::now() + Duration::from_secs(10);
        while flights
            .table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&key)
        {
            assert!(Instant::now() < end, "the worker never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
        let memoised_value = memo()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .map(|(_, v)| v.render_line());
        assert_eq!(
            memoised_value.as_deref(),
            Some("[served-corpus: UNKNOWN (measured with nobody waiting)]")
        );
    }

    /// A measurement that panics still ends its flight: the joiner gets an
    /// UNKNOWN naming the panic, and the next probe of the key starts afresh
    /// instead of joining a flight that never ends.
    #[test]
    fn a_panicking_measurement_still_ends_its_flight() {
        let flights = test_flights(4);
        let tmp = tempfile::tempdir().expect("tempdir");
        let key: MemoKey = (tmp.path().to_path_buf(), "panic-test");
        let got = probe_bounded(
            flights,
            key.clone(),
            Instant::now() + Duration::from_secs(5),
            "deadline".to_string(),
            || panic!("measurement blew up"),
        );
        assert_eq!(
            got.render_line(),
            "[served-corpus: UNKNOWN (served-corpus probe worker panicked)]"
        );
        let end = Instant::now() + Duration::from_secs(5);
        while flights
            .table
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&key)
        {
            assert!(
                Instant::now() < end,
                "the panicked flight never left the table"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `probe` itself returns inside budget + grace on a real checkout.
    #[test]
    fn probe_returns_inside_its_wall() {
        let tmp = checkout_with_bundle();
        let started = Instant::now();
        let _ = probe(tmp.path());
        assert!(
            started.elapsed() < PROBE_BUDGET + PROBE_COLLECT_GRACE + Duration::from_millis(250)
        );
    }

    #[test]
    fn unknown_renders_one_token() {
        assert_eq!(
            ServedCorpus::unknown("test").render_line(),
            "[served-corpus: UNKNOWN (test)]"
        );
    }

    #[test]
    fn no_rendered_value_can_close_a_token_early() {
        assert_eq!(clean("a]b[c\nd"), "a)b(c d");
    }
}

#[cfg(test)]
mod egress_header_tests {
    use super::*;
    use crate::egress::test_support::pin;
    use crate::egress::{Flow, Level};

    #[test]
    fn the_served_line_names_the_skill_mirror_switch_as_the_reason() {
        let ident = SourceIdentity::default();
        assert!(ident.render().starts_with("served: canonical@unloaded 0,"));
        let _pin = pin(Flow::SkillMirror, Level::Off);
        let line = ident.render();
        assert!(
            line.starts_with("served: canonical@unloaded(egress_skill_mirror=off) 0,"),
            "{line}"
        );
        assert!(!line.contains(']'));
    }
}
