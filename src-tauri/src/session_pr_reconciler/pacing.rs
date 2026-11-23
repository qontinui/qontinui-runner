//! Which GitHub reads a reconcile cycle may skip — plan
//! `2026-10-30-session-pr-reconciler-drains-the-user-github-rest-budget-re-polling-terminal-prs`,
//! Phase 2 (D1, D2, D3).
//!
//! Before this, every tick re-resolved every open session's PRs from GitHub:
//! one `list_prs_for_head` per (session, branch) and one `get_pr` per stored row
//! the branch pass missed — ~79% of them already merged or closed. The ticker
//! restarts as soon as a tick ends, so that was ~9,000 calls/h of demand against
//! the identity's 5,000/h.
//!
//! The rules, each a pure function here so it unit-tests without PG, git or the
//! network:
//!
//! - **D1 — savings per hour, not per tick.** Every skip is keyed on time since
//!   the last read (or on a changed head sha), never on tick count, so a cheaper
//!   tick that runs more often does not re-spend.
//! - **D2 — a terminal PR is not re-polled.** A `github-merge` / `ff-land` row is
//!   irreversible (and monotone in `upsert_session_pr`): never re-read, not on a
//!   runner start, not on a nudge. Every other non-open row takes its
//!   transitions from its repo's listings; a per-row `get_pr` survives only when
//!   no fresh, complete listing governs the repo, or when one does but holds no
//!   servable observation of this row (a failed re-read after a closed-listing
//!   gap, or an open PR dropped from a complete open listing) — so a failed read
//!   is retried on the row's own timer rather than skipped forever. That timer
//!   is [`OPEN_REFRESH`] for an open row, and for a closed one every 30 min for
//!   24 h from this process's first observation of the row as closed, then
//!   every 6 h.
//! - **D3 — discovery is per repo.** One conditional open listing plus a
//!   closed-since-mark listing per repo, at most every [`DISCOVERY_INTERVAL`],
//!   keyed by `head.label` (never `head.ref` alone: forks). The per-branch lookup
//!   survives only for a branch with no known PR whose head moved — or whose
//!   repo's discovery failed or overflowed — and never more often than
//!   [`DISCOVERY_INTERVAL`].
//!
//! All memory is process-local and keyed by `(repo, pr_number)`, so a PR stored
//! under several sessions is read once per interval. An empty memo — a runner
//! start — makes every non-terminal row due once.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::trigger_system::github_api::{HeadPr, PrStatus, RepoPrListing};

/// Per-row floor for an OPEN row whose repo no fresh, complete listing governs
/// (D1). A governed repo's rows are paced by [`DISCOVERY_INTERVAL`] instead.
pub(super) const OPEN_REFRESH: Duration = Duration::from_secs(120);

/// How often one repo is discovered (both listings), at most (D1, D3). Also the
/// retry floor for a per-branch lookup.
pub(super) const DISCOVERY_INTERVAL: Duration = Duration::from_secs(120);

/// The discovery floor on the on-demand nudge path. Unfloored, a dropdown held
/// open would re-list every busy repo about every 40 s (D1).
pub(super) const NUDGE_DISCOVERY_FLOOR: Duration = Duration::from_secs(30);

/// A closed, mutable row in an ungoverned repo: re-read this often for the first
/// [`CLOSED_EARLY_WINDOW`] after this process first saw it closed…
pub(super) const CLOSED_REFRESH_EARLY: Duration = Duration::from_secs(30 * 60);
/// …measured from the process-local first observation (the row stores no
/// `closed_at`)…
pub(super) const CLOSED_EARLY_WINDOW: Duration = Duration::from_secs(24 * 3600);
/// …and this often after it.
pub(super) const CLOSED_REFRESH_LATE: Duration = Duration::from_secs(6 * 3600);

/// How long a COMPLETE discovery keeps governing its repo's rows. Twice the
/// discovery interval, so a repo discovered on schedule never drops out of
/// governance between two discoveries, while one whose discovery stopped (it
/// left the discovery set, or the budget deferred it) falls back to the per-row
/// timers rather than being trusted indefinitely.
pub(super) const LISTING_GOVERNS_FOR: Duration = Duration::from_secs(2 * 120);

/// Ceiling on each process-local memo map. Crossing it clears that map, which
/// costs one re-read of what it held — the same cost as a runner start, never a
/// wrong answer.
const MEMO_MAX: usize = 20_000;

/// `(owner/name, pr_number)` — the memo key a PR is read under once per
/// interval, whichever sessions store it.
pub(super) type PrKey = (String, i64);

/// How a stored row may change, from its own columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowClass {
    /// `land_signal` `github-merge` or `ff-land`: irreversible. Never re-read.
    Immutable,
    /// `pr_state` `open` (or never recorded).
    Open,
    /// Closed and not irreversibly landed: `land-unknown`, `coord-label` (which a
    /// later confident `not-landed` may demote), or a closed `not-landed`.
    ClosedMutable,
}

/// Classify a stored row. Pure.
pub(super) fn row_class(land_signal: Option<&str>, pr_state: Option<&str>) -> RowClass {
    match (land_signal, pr_state) {
        (Some("github-merge" | "ff-land"), _) => RowClass::Immutable,
        (_, Some("open") | None) => RowClass::Open,
        _ => RowClass::ClosedMutable,
    }
}

/// [`status_refresh_due`]'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RefreshDue {
    /// Irreversible — never re-read (D2).
    Terminal,
    /// Read recently enough, or its repo's listing governs it.
    Fresh,
    /// A `get_pr` is due.
    Due,
}

/// D2 plus the [`OPEN_REFRESH`] floor: is a per-row `get_pr` due? Pure.
///
/// - `last_read` — when this process last read the PR (an observation from any
///   source, or a failed attempt). `None` is a runner start: due once.
/// - `first_closed_seen` — this process's first observation of the row as
///   [`RowClass::ClosedMutable`].
/// - `governed_by_listing` — a fresh, complete discovery of the row's repo
///   carries its transitions ([`RepoDiscovery::governs`]) AND holds a servable
///   observation of this row; [`plan_row_refresh`] passes `false` when the
///   listing governs the repo but has nothing to serve for the row, so the
///   per-row timer paces it.
/// - `nudge` — the on-demand path for this row's own session; exempts an OPEN
///   row from [`OPEN_REFRESH`], and nothing else.
pub(super) fn status_refresh_due(
    class: RowClass,
    last_read: Option<Instant>,
    first_closed_seen: Option<Instant>,
    governed_by_listing: bool,
    nudge: bool,
    now: Instant,
) -> RefreshDue {
    // THE TERMINAL SKIP (D2): before every other rule, so neither a runner
    // start nor a nudge nor any elapsed interval can re-read a landed row.
    if class == RowClass::Immutable {
        return RefreshDue::Terminal;
    }
    let Some(last) = last_read else {
        return RefreshDue::Due;
    };
    if governed_by_listing {
        return RefreshDue::Fresh;
    }
    let interval = if class == RowClass::Open {
        if nudge {
            return RefreshDue::Due;
        }
        OPEN_REFRESH
    } else {
        let first = first_closed_seen.unwrap_or(last);
        if now.saturating_duration_since(first) < CLOSED_EARLY_WINDOW {
            CLOSED_REFRESH_EARLY
        } else {
            CLOSED_REFRESH_LATE
        }
    };
    if now.saturating_duration_since(last) >= interval {
        RefreshDue::Due
    } else {
        RefreshDue::Fresh
    }
}

/// What the status pass does with one stored row this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowPlan {
    /// Irreversible; counted in `skipped_terminal`.
    SkipTerminal,
    /// A newer observation (a listing, or another session's read) is in the
    /// memo: write it to this session's row with no request.
    Apply,
    /// Nothing new and nothing due; counted in `skipped_fresh`.
    SkipFresh,
    /// `get_pr`.
    Read,
}

/// The inputs [`plan_row_refresh`] decides on.
#[derive(Debug, Clone, Copy)]
pub(super) struct RowInputs {
    pub class: RowClass,
    /// When the memo's observation of the PR last CHANGED (the version a row
    /// is compared against for [`RowPlan::Apply`]).
    pub observed_at: Option<Instant>,
    /// When the PR was last READ, changed or not (what the timers pace).
    pub read_at: Option<Instant>,
    /// When a failed `get_pr` was last attempted (a retry is paced like a read).
    pub attempted_at: Option<Instant>,
    /// The observation last written to THIS session's row.
    pub applied_at: Option<Instant>,
    /// When THIS session's row was last written (wall time) — paces the local
    /// re-probe of a governed closed row.
    pub applied_wall: Option<Instant>,
    /// Reads at or before this instant no longer count: the repo's closed
    /// listing overflowed, so a change may have fallen in the unread gap
    /// ([`RepoDiscovery::stale_before`]). Each such row is re-read once.
    pub stale_before: Option<Instant>,
    pub first_closed_seen: Option<Instant>,
    pub governed_by_listing: bool,
    pub nudge: bool,
}

/// Plan one stored row: terminal skip first, then a free apply of anything newer
/// than the row holds, then the per-row timer, then — for a closed row its
/// repo's listing governs — a periodic LOCAL re-probe of the land cascade
/// (a `git fetch` can turn a `land-unknown` into an `ff-land` without GitHub's
/// `updated_at` moving). Pure.
pub(super) fn plan_row_refresh(i: RowInputs, now: Instant) -> RowPlan {
    let last_read = match (i.read_at, i.attempted_at) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
    .filter(|r| i.stale_before.is_none_or(|s| *r > s));
    // An observation read before a closed-listing gap — or none at all (never
    // read, or dropped as no longer open) — is never written to a row.
    let fresh_observation = i
        .read_at
        .is_some_and(|r| i.stale_before.is_none_or(|s| r > s));
    // A listing governs a row only through an observation it can serve. With
    // none (a failed re-read after a gap, or a dropped open PR), a failed
    // attempt must not read as governed-fresh: the row is paced by its
    // per-row timer, so it is re-read after that interval instead of never.
    let governed = i.governed_by_listing && fresh_observation;
    let due = status_refresh_due(
        i.class,
        last_read,
        i.first_closed_seen,
        governed,
        i.nudge,
        now,
    );
    if due == RefreshDue::Terminal {
        return RowPlan::SkipTerminal;
    }
    if let Some(observed) = i.observed_at.filter(|_| fresh_observation) {
        if i.applied_at.is_none_or(|applied| observed > applied) {
            return RowPlan::Apply;
        }
    }
    if due == RefreshDue::Due {
        return RowPlan::Read;
    }
    if i.class == RowClass::ClosedMutable
        && i.governed_by_listing
        && fresh_observation
        && i.observed_at.is_some()
        && i.applied_wall
            .is_none_or(|w| now.saturating_duration_since(w) >= CLOSED_REFRESH_EARLY)
    {
        return RowPlan::Apply;
    }
    RowPlan::SkipFresh
}

/// D3: is this repo's discovery due? `floor` is [`DISCOVERY_INTERVAL`], or
/// [`NUDGE_DISCOVERY_FLOOR`] on the nudge path. Pure.
pub(super) fn discovery_due(memo: Option<&RepoDiscovery>, floor: Duration, now: Instant) -> bool {
    memo.and_then(|d| d.last_attempt)
        .is_none_or(|at| now.saturating_duration_since(at) >= floor)
}

/// What attribution does for one session branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BranchPlan {
    /// The discovered map carries PRs for this head label: attribute them, no
    /// request.
    FromDiscovery,
    /// A per-branch `list_prs_for_head`.
    Lookup,
    /// Nothing to ask GitHub this cycle.
    Nothing,
}

/// Per-branch lookup memory: the head sha last resolved, and when a lookup
/// last ran (successful or not).
#[derive(Debug, Clone, Default)]
pub(super) struct BranchMemo {
    pub sha: String,
    pub last_lookup: Option<Instant>,
}

/// D3: resolve one session branch against discovery first. Pure.
///
/// `known_stored` — the session already has a stored row for this branch; its
/// status is the status pass's job, so no lookup. `discovery_complete` — the
/// repo's discovery is fresh and neither failed nor overflowed. A lookup runs
/// only for a branch with no known PR whose head sha moved since the last
/// resolution (or that was never resolved), or — when discovery is incomplete —
/// for any branch with no known PR; never sooner than [`DISCOVERY_INTERVAL`]
/// after the previous lookup.
pub(super) fn branch_plan(
    in_discovery: bool,
    known_stored: bool,
    discovery_complete: bool,
    memo: Option<&BranchMemo>,
    sha: &str,
    now: Instant,
) -> BranchPlan {
    // Only a COMPLETE, fresh discovery may stand in for a lookup: an old
    // observation in an ungoverned repo would otherwise be served forever.
    if in_discovery && discovery_complete {
        return BranchPlan::FromDiscovery;
    }
    if known_stored {
        return BranchPlan::Nothing;
    }
    if memo
        .and_then(|m| m.last_lookup)
        .is_some_and(|at| now.saturating_duration_since(at) < DISCOVERY_INTERVAL)
    {
        return BranchPlan::Nothing;
    }
    if !discovery_complete || memo.is_none_or(|m| m.sha != sha) {
        BranchPlan::Lookup
    } else {
        BranchPlan::Nothing
    }
}

/// `<owner>:<ref>` as the key a branch is looked up under. GitHub logins are
/// case-insensitive, and the local remote URL's spelling may differ from the
/// API's, so the owner half is lowercased; a ref is case-sensitive. Pure.
pub(super) fn head_label_key(owner: &str, branch: &str) -> String {
    format!("{}:{branch}", owner.to_ascii_lowercase())
}

/// [`head_label_key`] of a payload's `head.label`; `None` when it has no
/// `owner:` half.
fn listed_label_key(label: &str) -> Option<String> {
    let (owner, branch) = label.split_once(':')?;
    if owner.is_empty() || branch.is_empty() {
        return None;
    }
    Some(head_label_key(owner, branch))
}

/// Per-repo discovery state.
#[derive(Debug, Default)]
pub(super) struct RepoDiscovery {
    /// The last discovery attempt (successful or failed; a deferral is not an
    /// attempt — it is retried as soon as the budget allows).
    pub last_attempt: Option<Instant>,
    /// That attempt read both listings in full: no error, no overflow.
    pub complete: bool,
    /// High-water `updated_at` of the closed listing.
    pub closed_mark: Option<String>,
    /// Set by [`ReconcileMemo::mark_closed_gap`] to the start of the discovery
    /// whose closed listing last OVERFLOWED. The mark still advances (or the
    /// repo would overflow forever), so changes in the unread pages between the
    /// cap and the old mark are never listed; every row of the repo read at or
    /// before this instant is therefore re-read once.
    pub stale_before: Option<Instant>,
    /// head-label key → PR numbers.
    by_label: HashMap<String, BTreeSet<i64>>,
}

impl RepoDiscovery {
    /// Does a fresh, complete discovery carry this repo's transitions?
    pub(super) fn governs(&self, now: Instant) -> bool {
        self.complete
            && self
                .last_attempt
                .is_some_and(|at| now.saturating_duration_since(at) <= LISTING_GOVERNS_FOR)
    }
}

/// One observation of a PR, from a listing, a per-branch lookup or a `get_pr`.
#[derive(Debug, Clone)]
pub(super) struct Observed {
    pub pr: HeadPr,
    /// When the observed PR last CHANGED. An identical re-read (a `304`
    /// replay, an unchanged listing) leaves it alone, so nothing is re-written
    /// for a PR that did not move.
    pub at: Instant,
    /// When it was last read, changed or not — what the per-row timers pace.
    pub read_at: Instant,
}

/// The reconciler's cross-tick, process-local memory.
#[derive(Debug, Default)]
pub(super) struct ReconcileMemo {
    pub repos: HashMap<String, RepoDiscovery>,
    prs: HashMap<PrKey, Observed>,
    attempted: HashMap<PrKey, Instant>,
    first_closed_seen: HashMap<PrKey, Instant>,
    /// `(observation version, wall time)` last written to each session's row.
    applied: HashMap<(Uuid, PrKey), (Instant, Instant)>,
    branches: HashMap<(String, String), BranchMemo>,
}

impl ReconcileMemo {
    /// Record an observation of `pr` in `repo_full`, indexing its head label.
    pub(super) fn observe(&mut self, repo_full: &str, pr: HeadPr, now: Instant) {
        if let Some(key) = listed_label_key(&pr.head_label) {
            self.repos
                .entry(repo_full.to_string())
                .or_default()
                .by_label
                .entry(key)
                .or_default()
                .insert(pr.number as i64);
        }
        let key = (repo_full.to_string(), pr.number as i64);
        self.attempted.remove(&key);
        match self.prs.get_mut(&key) {
            Some(prev) if prev.pr == pr => prev.read_at = now,
            _ => {
                self.prs.insert(
                    key,
                    Observed {
                        pr,
                        at: now,
                        read_at: now,
                    },
                );
            }
        }
    }

    /// Record a failed per-row read, so its retry is paced like a read.
    pub(super) fn note_attempt(&mut self, key: PrKey, now: Instant) {
        self.attempted.insert(key, now);
    }

    pub(super) fn observation(&self, key: &PrKey) -> Option<&Observed> {
        self.prs.get(key)
    }

    pub(super) fn attempted_at(&self, key: &PrKey) -> Option<Instant> {
        self.attempted.get(key).copied()
    }

    /// The PRs observed for `<owner>:<branch>` in `repo_full` — by head label,
    /// so a fork's same-named branch never matches.
    pub(super) fn prs_for_branch(
        &self,
        repo_full: &str,
        owner: &str,
        branch: &str,
    ) -> Vec<Observed> {
        let Some(d) = self.repos.get(repo_full) else {
            return Vec::new();
        };
        let Some(numbers) = d.by_label.get(&head_label_key(owner, branch)) else {
            return Vec::new();
        };
        numbers
            .iter()
            .filter_map(|n| self.prs.get(&(repo_full.to_string(), *n)))
            // An observation read before a closed-listing gap may have missed
            // a transition: it is not served (the row falls to the status
            // pass, which re-reads it once).
            .filter(|o| d.stale_before.is_none_or(|s| o.read_at > s))
            .cloned()
            .collect()
    }

    /// A COMPLETE open listing of `repo_full` returned exactly `listed_open`.
    /// Every other observation still recorded as `open` is no longer known to
    /// be open — it closed (and the closed listing may have missed it: lag, or
    /// an overflow gap) — so it is dropped: attribution stops serving it, and
    /// its row is re-read once by the status pass. Call it before observing
    /// the closed listing, which re-records any PR it carries.
    pub(super) fn drop_unlisted_open(&mut self, repo_full: &str, listed_open: &BTreeSet<i64>) {
        self.prs.retain(|(repo, number), o| {
            repo != repo_full || o.pr.state != "open" || listed_open.contains(number)
        });
    }

    /// A CLOSED listing overflowed: the pages between the cap and the old mark
    /// were never read, so a change there is unseen. Every observation of the
    /// repo read before `listing_started` (this discovery's start — so the
    /// listing's own reads count) is stale ([`RepoDiscovery::stale_before`]).
    /// An OPEN-listing overflow leaves no such gap (the listing is simply
    /// incomplete, and the repo is not governed), so it does not call this.
    pub(super) fn mark_closed_gap(&mut self, repo_full: &str, listing_started: Instant) {
        self.repos
            .entry(repo_full.to_string())
            .or_default()
            .stale_before = Some(listing_started);
    }

    /// End one discovery: both listings were read (their PRs already
    /// [`Self::observe`]d). Advances the closed high-water mark to the newest
    /// closed `updated_at` seen.
    pub(super) fn finish_discovery(
        &mut self,
        repo_full: &str,
        closed: &[HeadPr],
        complete: bool,
        now: Instant,
    ) {
        let d = self.repos.entry(repo_full.to_string()).or_default();
        for pr in closed {
            let Some(u) = pr.updated_at.as_deref() else {
                continue;
            };
            if newer_than(u, d.closed_mark.as_deref()) {
                d.closed_mark = Some(u.to_string());
            }
        }
        d.last_attempt = Some(now);
        d.complete = complete;
    }

    /// Record one discovery's listings — THE wiring `discover_repo` calls, so
    /// the overflow rules are tested where they act. Returns whether either
    /// listing overflowed. `started` is taken before the first request, `open_at`
    /// / `closed_at` when each listing answered; `closed` is `None` when the
    /// closed listing was not read (deferred, or failed — the caller records
    /// that), in which case only the open listing's free data is kept.
    ///
    /// - A COMPLETE open listing drops every other observation still recorded
    ///   open ([`Self::drop_unlisted_open`]); an overflowed one drops nothing.
    /// - Only a CLOSED-listing overflow marks a gap
    ///   ([`Self::mark_closed_gap`]), stamped at `started`, so this discovery's
    ///   own reads survive it; an open-listing overflow marks nothing.
    pub(super) fn apply_discovery(
        &mut self,
        repo_full: &str,
        open: RepoPrListing,
        open_at: Instant,
        closed: Option<(RepoPrListing, Instant)>,
        started: Instant,
    ) -> bool {
        if !open.overflow {
            let listed: BTreeSet<i64> = open.prs.iter().map(|pr| pr.number as i64).collect();
            self.drop_unlisted_open(repo_full, &listed);
        }
        for pr in open.prs {
            self.observe(repo_full, pr, open_at);
        }
        let Some((closed, closed_at)) = closed else {
            return open.overflow;
        };
        if closed.overflow {
            self.mark_closed_gap(repo_full, started);
        }
        for pr in &closed.prs {
            self.observe(repo_full, pr.clone(), closed_at);
        }
        let overflow = open.overflow || closed.overflow;
        self.finish_discovery(repo_full, &closed.prs, !overflow, closed_at);
        overflow
    }

    /// A discovery that failed: retried after the interval, and meanwhile the
    /// repo's rows fall back to their per-row timers.
    pub(super) fn discovery_failed(&mut self, repo_full: &str, now: Instant) {
        let d = self.repos.entry(repo_full.to_string()).or_default();
        d.last_attempt = Some(now);
        d.complete = false;
    }

    /// This process's first observation of `key` as closed-and-mutable.
    pub(super) fn note_closed(&mut self, key: &PrKey, now: Instant) -> Instant {
        *self.first_closed_seen.entry(key.clone()).or_insert(now)
    }

    pub(super) fn applied_at(&self, session: Uuid, key: &PrKey) -> Option<Instant> {
        self.applied.get(&(session, key.clone())).map(|(v, _)| *v)
    }

    /// Everything [`plan_row_refresh`] needs about one session's stored row,
    /// read off the memo. `first_closed_seen` comes from [`Self::note_closed`]
    /// for a closed row; `nudge` is the caller's.
    pub(super) fn row_inputs(
        &self,
        session: Uuid,
        key: &PrKey,
        class: RowClass,
        first_closed_seen: Option<Instant>,
        nudge: bool,
        now: Instant,
    ) -> RowInputs {
        let observed = self.prs.get(key);
        let discovery = self.repos.get(&key.0);
        RowInputs {
            class,
            observed_at: observed.map(|o| o.at),
            read_at: observed.map(|o| o.read_at),
            attempted_at: self.attempted_at(key),
            applied_at: self.applied_at(session, key),
            applied_wall: self.applied_wall(session, key),
            stale_before: discovery.and_then(|d| d.stale_before),
            first_closed_seen,
            governed_by_listing: discovery.is_some_and(|d| d.governs(now)),
            nudge,
        }
    }

    pub(super) fn applied_wall(&self, session: Uuid, key: &PrKey) -> Option<Instant> {
        self.applied.get(&(session, key.clone())).map(|(_, w)| *w)
    }

    /// `observed_at` (the observation's version) was written to the session's
    /// row at `now`.
    pub(super) fn mark_applied(
        &mut self,
        session: Uuid,
        key: PrKey,
        observed_at: Instant,
        now: Instant,
    ) {
        self.applied.insert((session, key), (observed_at, now));
    }

    pub(super) fn branch(&self, repo_full: &str, branch: &str) -> Option<&BranchMemo> {
        self.branches
            .get(&(repo_full.to_string(), branch.to_string()))
    }

    /// The branch's head as of this cycle; `lookup_at` when a lookup ran.
    pub(super) fn note_branch(
        &mut self,
        repo_full: &str,
        branch: &str,
        sha: &str,
        lookup_at: Option<Instant>,
    ) {
        let m = self
            .branches
            .entry((repo_full.to_string(), branch.to_string()))
            .or_default();
        m.sha = sha.to_string();
        if lookup_at.is_some() {
            m.last_lookup = lookup_at;
        }
    }

    /// A per-branch lookup FAILED: pace its retry by [`DISCOVERY_INTERVAL`],
    /// but do not record the head as resolved — otherwise a complete discovery
    /// would read the unchanged sha as "already looked up" and never retry.
    pub(super) fn note_lookup_failed(&mut self, repo_full: &str, branch: &str, now: Instant) {
        self.branches
            .entry((repo_full.to_string(), branch.to_string()))
            .or_default()
            .last_lookup = Some(now);
    }

    /// Keep every map bounded (see [`MEMO_MAX`]).
    pub(super) fn prune(&mut self) {
        if self.prs.len() > MEMO_MAX {
            self.prs.clear();
            for d in self.repos.values_mut() {
                d.by_label.clear();
            }
        }
        if self.attempted.len() > MEMO_MAX {
            self.attempted.clear();
        }
        if self.first_closed_seen.len() > MEMO_MAX {
            self.first_closed_seen.clear();
        }
        if self.applied.len() > MEMO_MAX {
            self.applied.clear();
        }
        if self.branches.len() > MEMO_MAX {
            self.branches.clear();
        }
    }
}

/// Is RFC3339 `candidate` strictly newer than `mark` (or is there no mark)?
/// An unparseable candidate never advances a mark. Pure.
fn newer_than(candidate: &str, mark: Option<&str>) -> bool {
    let Ok(c) = chrono::DateTime::parse_from_rfc3339(candidate) else {
        return false;
    };
    match mark.and_then(|m| chrono::DateTime::parse_from_rfc3339(m).ok()) {
        Some(m) => c > m,
        None => true,
    }
}

/// A `get_pr` answer as an observation. `get_pr` reads no head label, so the
/// observation leaves the label index as it was.
pub(super) fn observed_from_status(number: i64, pr: PrStatus) -> HeadPr {
    HeadPr {
        number: number as u64,
        state: pr.state,
        merged: pr.merged,
        merged_at: pr.merged_at,
        closed_at: pr.closed_at,
        head_ref: String::new(),
        head_label: String::new(),
        head_sha: pr.head_sha,
        base_ref: pr.base_ref,
        updated_at: None,
        labels: pr.labels,
    }
}

/// One cycle's pacing counters, for the `tick done` line. A skip has to be
/// observable, not inferred.
#[derive(Debug, Default)]
pub(super) struct TickTally {
    /// Stored rows not re-read because they are irreversibly landed.
    pub skipped_terminal: usize,
    /// Stored rows answered without a `get_pr`: nothing due (a timer, or a
    /// governing listing), or a newer observation already in the memo.
    pub skipped_fresh: usize,
    /// Discovery requests answered (charged or `304`).
    pub discovery_calls: usize,
    /// Discoveries whose listing hit the paging bound (incomplete) — the open
    /// or the closed listing. An overflowed open listing counts even when the
    /// closed listing was then deferred or failed.
    pub discovery_overflow: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANDED: [&str; 2] = ["github-merge", "ff-land"];

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn pr(number: u64, state: &str, label: &str, updated_at: &str) -> HeadPr {
        HeadPr {
            number,
            state: state.to_string(),
            merged: false,
            merged_at: None,
            closed_at: None,
            head_ref: label
                .split_once(':')
                .map(|(_, b)| b)
                .unwrap_or("")
                .to_string(),
            head_label: label.to_string(),
            head_sha: "sha".to_string(),
            base_ref: "main".to_string(),
            updated_at: Some(updated_at.to_string()),
            labels: Vec::new(),
        }
    }

    #[test]
    fn row_class_reads_the_stored_land_signal_first() {
        for s in LANDED {
            assert_eq!(row_class(Some(s), Some("merged")), RowClass::Immutable);
        }
        assert_eq!(
            row_class(Some("coord-label"), Some("merged")),
            RowClass::ClosedMutable
        );
        assert_eq!(
            row_class(Some("land-unknown"), Some("closed")),
            RowClass::ClosedMutable
        );
        assert_eq!(row_class(Some("not-landed"), Some("open")), RowClass::Open);
        assert_eq!(row_class(None, None), RowClass::Open);
    }

    /// D2: a landed row is never due — not after any interval, not on a runner
    /// start (nothing read yet), not when its repo is ungoverned.
    #[test]
    fn a_landed_row_is_never_due() {
        let t0 = Instant::now();
        for land_signal in LANDED {
            let class = row_class(Some(land_signal), Some("merged"));
            for last_read in [None, Some(t0)] {
                for later in [secs(0), secs(119), OPEN_REFRESH, CLOSED_REFRESH_LATE * 10] {
                    for governed in [false, true] {
                        assert_eq!(
                            status_refresh_due(class, last_read, None, governed, false, t0 + later),
                            RefreshDue::Terminal,
                            "{land_signal} last_read={last_read:?} +{later:?}"
                        );
                    }
                }
            }
            // And the plan the status pass acts on never reads it, even with
            // a newer observation in the memo.
            let plan = plan_row_refresh(
                RowInputs {
                    class,
                    observed_at: Some(t0 + secs(1)),
                    read_at: Some(t0 + secs(1)),
                    attempted_at: None,
                    applied_at: None,
                    applied_wall: None,
                    stale_before: Some(t0 + secs(2)),
                    first_closed_seen: None,
                    governed_by_listing: false,
                    nudge: true,
                },
                t0 + secs(3600),
            );
            assert_eq!(plan, RowPlan::SkipTerminal);
        }
    }

    /// D1/D2: the nudge path exempts only OPEN rows from `OPEN_REFRESH`; it
    /// never re-reads a `github-merge` / `ff-land` row.
    #[test]
    fn a_nudge_never_rereads_a_landed_row() {
        let t0 = Instant::now();
        for land_signal in LANDED {
            for last_read in [None, Some(t0)] {
                assert_eq!(
                    status_refresh_due(
                        row_class(Some(land_signal), Some("merged")),
                        last_read,
                        None,
                        false,
                        true,
                        t0 + secs(1),
                    ),
                    RefreshDue::Terminal
                );
            }
        }
        // The same nudge DOES re-read an open row inside OPEN_REFRESH…
        assert_eq!(
            status_refresh_due(RowClass::Open, Some(t0), None, false, true, t0 + secs(1)),
            RefreshDue::Due
        );
        // …and not a closed one.
        assert_eq!(
            status_refresh_due(
                RowClass::ClosedMutable,
                Some(t0),
                Some(t0),
                false,
                true,
                t0 + secs(1)
            ),
            RefreshDue::Fresh
        );
    }

    #[test]
    fn an_open_row_is_not_due_within_open_refresh() {
        let t0 = Instant::now();
        let due =
            |after| status_refresh_due(RowClass::Open, Some(t0), None, false, false, t0 + after);
        assert_eq!(due(secs(1)), RefreshDue::Fresh);
        assert_eq!(due(OPEN_REFRESH - secs(1)), RefreshDue::Fresh);
        assert_eq!(due(OPEN_REFRESH), RefreshDue::Due);
    }

    /// D2: an undiscovered repo's `land-unknown` row is due after 30 min for its
    /// first 24 h closed, then every 6 h.
    #[test]
    fn an_undiscovered_land_unknown_row_is_due_after_30_min_then_every_6_h() {
        let t0 = Instant::now();
        let class = row_class(Some("land-unknown"), Some("closed"));
        let due = |last: Instant, now: Instant| {
            status_refresh_due(class, Some(last), Some(t0), false, false, now)
        };
        assert_eq!(
            due(t0, t0 + CLOSED_REFRESH_EARLY - secs(1)),
            RefreshDue::Fresh
        );
        assert_eq!(due(t0, t0 + CLOSED_REFRESH_EARLY), RefreshDue::Due);
        // Past the first day the floor widens to 6 h.
        let day = t0 + CLOSED_EARLY_WINDOW;
        assert_eq!(due(day, day + CLOSED_REFRESH_EARLY), RefreshDue::Fresh);
        assert_eq!(
            due(day, day + CLOSED_REFRESH_LATE - secs(1)),
            RefreshDue::Fresh
        );
        assert_eq!(due(day, day + CLOSED_REFRESH_LATE), RefreshDue::Due);
        // A coord-label row is paced the same way (demotable, so mutable).
        assert_eq!(
            status_refresh_due(
                row_class(Some("coord-label"), Some("merged")),
                Some(t0),
                Some(t0),
                false,
                false,
                t0 + CLOSED_REFRESH_EARLY,
            ),
            RefreshDue::Due
        );
    }

    /// A runner start (nothing read by this process yet) re-reads each
    /// non-immutable row once.
    #[test]
    fn a_runner_start_rereads_each_non_immutable_row_once() {
        let t0 = Instant::now();
        for class in [RowClass::Open, RowClass::ClosedMutable] {
            for governed in [false, true] {
                assert_eq!(
                    status_refresh_due(class, None, None, governed, false, t0),
                    RefreshDue::Due
                );
            }
        }
    }

    /// D3: a discovered repo's closed row is refreshed from the discovery map
    /// — `Apply` — and never by a `get_pr`.
    #[test]
    fn a_discovered_repos_closed_row_is_refreshed_from_the_discovery_map_with_zero_get_pr() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let key: PrKey = ("o/r".to_string(), 9);
        // Read once at start; applied to the session's row.
        memo.observe("o/r", pr(9, "closed", "o:b", "2026-11-19T10:00:00Z"), t0);
        let session = Uuid::nil();
        memo.mark_applied(session, key.clone(), t0, t0);
        memo.finish_discovery("o/r", &[], true, t0);

        // Later the closed listing carries a label change (coord:landed).
        let later = t0 + DISCOVERY_INTERVAL;
        let mut relabelled = pr(9, "closed", "o:b", "2026-11-19T10:05:00Z");
        relabelled.labels = vec!["coord:landed".to_string()];
        memo.observe("o/r", relabelled.clone(), later);
        memo.finish_discovery("o/r", &[relabelled], true, later);

        let inputs = |now: Instant, memo: &ReconcileMemo| {
            memo.row_inputs(session, &key, RowClass::ClosedMutable, Some(t0), false, now)
        };
        let now = later + secs(1);
        assert_eq!(plan_row_refresh(inputs(now, &memo), now), RowPlan::Apply);
        assert_eq!(
            memo.observation(&key).map(|o| o.pr.labels.clone()),
            Some(vec!["coord:landed".to_string()])
        );
        memo.mark_applied(session, key.clone(), later, later + secs(1));
        // Applied: nothing more to do, and no read — ever, while governed —
        // even past the per-row 30-min floor.
        let now = later + secs(2);
        memo.finish_discovery("o/r", &[], true, now);
        assert_eq!(
            plan_row_refresh(inputs(now, &memo), now),
            RowPlan::SkipFresh
        );
        // Past it, the governed row is re-probed LOCALLY (Apply of the same
        // observation) — still never a get_pr.
        let now = later + CLOSED_REFRESH_EARLY + secs(2);
        memo.finish_discovery("o/r", &[], true, now);
        assert_eq!(plan_row_refresh(inputs(now, &memo), now), RowPlan::Apply);
    }

    /// D1: two ticks 5 s apart re-read nothing. Tick 1 (a runner start) reads
    /// every non-terminal row once; tick 2 reads none.
    #[test]
    fn two_ticks_five_seconds_apart_reread_nothing() {
        let t0 = Instant::now();
        let session = Uuid::nil();
        let rows: Vec<(PrKey, RowClass)> = vec![
            (
                ("o/r".into(), 1),
                row_class(Some("github-merge"), Some("merged")),
            ),
            (
                ("o/r".into(), 2),
                row_class(Some("ff-land"), Some("merged")),
            ),
            (
                ("o/r".into(), 3),
                row_class(Some("land-unknown"), Some("closed")),
            ),
            (
                ("o/r".into(), 4),
                row_class(Some("coord-label"), Some("merged")),
            ),
            (
                ("o/r".into(), 5),
                row_class(Some("not-landed"), Some("open")),
            ),
        ];
        let mut memo = ReconcileMemo::default();
        let tick = |memo: &mut ReconcileMemo, now: Instant| -> usize {
            let mut reads = 0;
            for (key, class) in &rows {
                let first_closed =
                    (*class == RowClass::ClosedMutable).then(|| memo.note_closed(key, now));
                let plan = plan_row_refresh(
                    memo.row_inputs(session, key, *class, first_closed, false, now),
                    now,
                );
                match plan {
                    RowPlan::Read => {
                        reads += 1;
                        memo.observe(&key.0, pr(key.1 as u64, "closed", "", "x"), now);
                        memo.mark_applied(session, key.clone(), now, now);
                    }
                    RowPlan::Apply => {
                        let at = memo.observation(key).map(|o| o.at).unwrap_or(now);
                        memo.mark_applied(session, key.clone(), at, now);
                    }
                    RowPlan::SkipTerminal | RowPlan::SkipFresh => {}
                }
            }
            reads
        };
        assert_eq!(
            tick(&mut memo, t0),
            3,
            "a start reads each non-terminal row once"
        );
        assert_eq!(
            tick(&mut memo, t0 + secs(5)),
            0,
            "a fast tick re-spends nothing"
        );
    }

    /// An ungoverned repo's old observation never stands in for a lookup:
    /// with discovery incomplete, a listed-but-unstored branch is looked up
    /// (on the floor) rather than served the frozen observation forever.
    #[test]
    fn only_a_complete_discovery_stands_in_for_a_lookup() {
        let t0 = Instant::now();
        assert_eq!(
            branch_plan(true, false, true, None, "s", t0),
            BranchPlan::FromDiscovery
        );
        assert_eq!(
            branch_plan(true, false, false, None, "s", t0),
            BranchPlan::Lookup
        );
        // Stored: the status pass's per-row timers own it.
        assert_eq!(
            branch_plan(true, true, false, None, "s", t0),
            BranchPlan::Nothing
        );
    }

    /// A failed lookup is retried after the floor even though the head did
    /// not move.
    #[test]
    fn a_failed_lookup_is_retried_after_the_floor() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.note_lookup_failed("o/r", "b", t0);
        let plan = |now| branch_plan(false, false, true, memo.branch("o/r", "b"), "sha", now);
        assert_eq!(plan(t0 + secs(5)), BranchPlan::Nothing);
        assert_eq!(plan(t0 + DISCOVERY_INTERVAL), BranchPlan::Lookup);
    }

    /// An identical re-read moves `read_at` (what the timers pace) but not
    /// `at` (what rows are re-written for): an unchanged PR is not re-applied.
    #[test]
    fn an_unchanged_reread_does_not_reapply() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let key: PrKey = ("o/r".into(), 4);
        let session = Uuid::nil();
        let open = pr(4, "open", "o:b", "2026-11-19T10:00:00Z");
        memo.observe("o/r", open.clone(), t0);
        memo.mark_applied(session, key.clone(), t0, t0);
        memo.finish_discovery("o/r", &[], true, t0);

        let later = t0 + DISCOVERY_INTERVAL;
        memo.observe("o/r", open, later);
        memo.finish_discovery("o/r", &[], true, later);
        let o = memo.observation(&key).expect("observed");
        assert_eq!(o.at, t0, "unchanged: same version");
        assert_eq!(o.read_at, later);
        assert_eq!(
            plan_row_refresh(
                memo.row_inputs(session, &key, RowClass::Open, None, false, later),
                later
            ),
            RowPlan::SkipFresh
        );
    }

    /// An overflowed closed listing leaves an unread gap: every row of the
    /// repo read before it is re-read once, governed or not.
    #[test]
    fn an_overflowed_closed_listing_rereads_the_repos_rows_once() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let key: PrKey = ("o/r".into(), 6);
        let session = Uuid::nil();
        memo.observe("o/r", pr(6, "closed", "o:b", "2026-11-19T09:00:00Z"), t0);
        memo.mark_applied(session, key.clone(), t0, t0);

        // A discovery started at `gap` whose CLOSED listing overflowed.
        let gap = t0 + secs(10);
        memo.mark_closed_gap("o/r", gap);
        memo.finish_discovery("o/r", &[], false, gap + secs(1));
        let now = gap + secs(2);
        memo.finish_discovery("o/r", &[], true, now); // next one complete
        let inputs = |memo: &ReconcileMemo, now| {
            memo.row_inputs(session, &key, RowClass::ClosedMutable, Some(t0), false, now)
        };
        assert_eq!(plan_row_refresh(inputs(&memo, now), now), RowPlan::Read);

        // Once re-read after the gap, it is governed again.
        memo.observe("o/r", pr(6, "closed", "o:b", "2026-11-19T09:00:00Z"), now);
        memo.mark_applied(session, key.clone(), now, now);
        let next = now + secs(5);
        assert_eq!(
            plan_row_refresh(inputs(&memo, next), next),
            RowPlan::SkipFresh
        );
    }

    /// A persistently overflowing OPEN listing leaves no gap behind a mark:
    /// it marks nothing stale, so rows keep their per-row timers instead of
    /// being re-read every cycle.
    #[test]
    fn a_persistent_open_overflow_does_not_reread_every_cycle() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let session = Uuid::nil();
        let closed: PrKey = ("o/r".into(), 1);
        let open: PrKey = ("o/r".into(), 2);
        memo.observe("o/r", pr(1, "closed", "o:a", "2026-11-19T09:00:00Z"), t0);
        memo.observe("o/r", pr(2, "open", "o:b", "2026-11-19T09:00:00Z"), t0);
        memo.mark_applied(session, closed.clone(), t0, t0);
        memo.mark_applied(session, open.clone(), t0, t0);
        for cycle in 1..=3u64 {
            // open overflowed, closed complete: `complete = false`, no gap.
            let now = t0 + DISCOVERY_INTERVAL * cycle as u32;
            memo.finish_discovery("o/r", &[], false, now);
            assert!(!memo.repos["o/r"].governs(now));
            assert_eq!(memo.repos["o/r"].stale_before, None);
            assert_eq!(
                plan_row_refresh(
                    memo.row_inputs(
                        session,
                        &closed,
                        RowClass::ClosedMutable,
                        Some(t0),
                        false,
                        now
                    ),
                    now
                ),
                RowPlan::SkipFresh,
                "cycle {cycle}: a closed row waits for its 30-min timer"
            );
        }
    }

    /// A closed-listing gap marks only reads from BEFORE the discovery: the
    /// listing's own observations (made after it started) still count.
    #[test]
    fn a_closed_gap_keeps_the_listings_own_reads() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let session = Uuid::nil();
        let key: PrKey = ("o/r".into(), 3);
        let started = t0;
        memo.mark_closed_gap("o/r", started);
        let now = started + secs(1);
        memo.observe("o/r", pr(3, "closed", "o:c", "2026-11-19T09:00:00Z"), now);
        memo.mark_applied(session, key.clone(), now, now);
        memo.finish_discovery("o/r", &[], false, now);
        assert_ne!(
            plan_row_refresh(
                memo.row_inputs(
                    session,
                    &key,
                    RowClass::ClosedMutable,
                    Some(now),
                    false,
                    now
                ),
                now
            ),
            RowPlan::Read
        );
        assert_eq!(memo.prs_for_branch("o/r", "o", "c").len(), 1, "served");
    }

    /// A PR seen open and absent from a later COMPLETE open listing is no
    /// longer served as open: attribution drops it and its row is re-read
    /// once.
    #[test]
    fn an_open_pr_missing_from_a_complete_open_listing_is_not_served() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let session = Uuid::nil();
        let key: PrKey = ("o/r".into(), 5);
        memo.observe("o/r", pr(5, "open", "o:b", "2026-11-19T09:00:00Z"), t0);
        memo.mark_applied(session, key.clone(), t0, t0);
        memo.finish_discovery("o/r", &[], true, t0);
        assert_eq!(memo.prs_for_branch("o/r", "o", "b").len(), 1);

        // Next discovery: complete open listing without #5, and the closed
        // listing missed it too.
        let later = t0 + DISCOVERY_INTERVAL;
        memo.drop_unlisted_open("o/r", &BTreeSet::new());
        memo.finish_discovery("o/r", &[], true, later);
        assert!(memo.prs_for_branch("o/r", "o", "b").is_empty());
        assert_eq!(
            plan_row_refresh(
                memo.row_inputs(session, &key, RowClass::Open, None, false, later),
                later
            ),
            RowPlan::Read,
            "the row is re-read once"
        );
        // A closed PR is untouched by the open listing.
        memo.observe("o/r", pr(6, "closed", "o:d", "2026-11-19T09:00:00Z"), later);
        memo.drop_unlisted_open("o/r", &BTreeSet::new());
        assert_eq!(memo.prs_for_branch("o/r", "o", "d").len(), 1);
    }

    /// A stale observation (read before a closed-listing gap) is not served
    /// to attribution.
    #[test]
    fn a_stale_observation_is_not_served_to_attribution() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.observe("o/r", pr(7, "open", "o:e", "2026-11-19T09:00:00Z"), t0);
        memo.mark_closed_gap("o/r", t0 + secs(1));
        assert!(memo.prs_for_branch("o/r", "o", "e").is_empty());
    }

    fn listing(prs: Vec<HeadPr>, overflow: bool) -> RepoPrListing {
        RepoPrListing {
            prs,
            overflow,
            requests: 1,
        }
    }

    /// The overflow rules, through the wiring `discover_repo` calls, for all
    /// four open/closed overflow combinations. A previously-seen open PR #1
    /// absent from this open listing; a pre-discovery read of #2 (closed).
    #[test]
    fn apply_discovery_handles_every_overflow_combination() {
        for (open_overflow, closed_overflow) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let why = format!("open_overflow={open_overflow} closed_overflow={closed_overflow}");
            let t0 = Instant::now();
            let mut memo = ReconcileMemo::default();
            memo.observe("o/r", pr(1, "open", "o:gone", "2026-11-19T08:00:00Z"), t0);
            memo.observe("o/r", pr(2, "closed", "o:old", "2026-11-19T08:00:00Z"), t0);

            let started = t0 + secs(1);
            let open_at = started + secs(1);
            let closed_at = open_at + secs(1);
            let open = listing(
                vec![pr(3, "open", "o:new", "2026-11-19T09:00:00Z")],
                open_overflow,
            );
            let closed = listing(
                vec![pr(4, "closed", "o:shut", "2026-11-19T09:30:00Z")],
                closed_overflow,
            );
            let overflowed =
                memo.apply_discovery("o/r", open, open_at, Some((closed, closed_at)), started);

            assert_eq!(overflowed, open_overflow || closed_overflow, "{why}");
            let d = &memo.repos["o/r"];
            assert_eq!(d.complete, !(open_overflow || closed_overflow), "{why}");
            // Only a closed overflow marks a gap, at the discovery's start.
            assert_eq!(d.stale_before, closed_overflow.then_some(started), "{why}");
            // Only a complete open listing drops the unlisted open PR.
            assert_eq!(
                memo.prs_for_branch("o/r", "o", "gone").is_empty(),
                !open_overflow || closed_overflow,
                "{why}"
            );
            // Reads made DURING the listing survive a closed-overflow gap.
            assert_eq!(memo.prs_for_branch("o/r", "o", "new").len(), 1, "{why}");
            assert_eq!(memo.prs_for_branch("o/r", "o", "shut").len(), 1, "{why}");
            // A read from BEFORE the discovery does not survive one.
            assert_eq!(
                memo.prs_for_branch("o/r", "o", "old").is_empty(),
                closed_overflow,
                "{why}"
            );
            assert_eq!(
                d.closed_mark.as_deref(),
                Some("2026-11-19T09:30:00Z"),
                "{why}"
            );
        }
    }

    /// A closed listing that was not read keeps the open listing's data and
    /// records no attempt.
    #[test]
    fn apply_discovery_without_a_closed_listing_records_no_attempt() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let open = listing(vec![pr(3, "open", "o:new", "2026-11-19T09:00:00Z")], false);
        assert!(!memo.apply_discovery("o/r", open, t0, None, t0));
        assert_eq!(memo.prs_for_branch("o/r", "o", "new").len(), 1);
        assert!(memo.repos["o/r"].last_attempt.is_none());
    }

    /// A pre-gap observation whose re-read FAILED, in a repo a fresh, complete
    /// listing governs, is neither written nor left Fresh forever: it is
    /// paced by its per-row timer and re-read after the interval.
    #[test]
    fn a_failed_reread_after_a_gap_is_retried_on_the_per_row_timer() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let session = Uuid::nil();
        let key: PrKey = ("o/r".into(), 9);
        memo.observe("o/r", pr(9, "closed", "o:y", "2026-11-19T09:00:00Z"), t0);
        memo.mark_closed_gap("o/r", t0 + secs(1));
        memo.note_attempt(key.clone(), t0 + secs(2)); // the re-read failed
        let plan = |memo: &mut ReconcileMemo, now: Instant| {
            memo.finish_discovery("o/r", &[], true, now); // governed throughout
            assert!(memo.repos["o/r"].governs(now));
            plan_row_refresh(
                memo.row_inputs(session, &key, RowClass::ClosedMutable, Some(t0), false, now),
                now,
            )
        };
        // Neither applied (stale) nor locally re-probed (stale), nor re-read
        // before its timer.
        assert_eq!(plan(&mut memo, t0 + secs(3)), RowPlan::SkipFresh);
        // After the per-row interval, re-read — not Fresh forever.
        assert_eq!(
            plan(&mut memo, t0 + secs(2) + CLOSED_REFRESH_EARLY),
            RowPlan::Read
        );
    }

    /// A pre-gap observation is never written to a row: the row is re-read.
    #[test]
    fn a_pre_gap_observation_is_not_applied() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        let session = Uuid::nil();
        let key: PrKey = ("o/r".into(), 8);
        // Observed (never applied to this session's row), then a gap.
        memo.observe("o/r", pr(8, "closed", "o:x", "2026-11-19T09:00:00Z"), t0);
        memo.mark_closed_gap("o/r", t0 + secs(1));
        let now = t0 + secs(2);
        assert_eq!(
            plan_row_refresh(
                memo.row_inputs(session, &key, RowClass::ClosedMutable, Some(t0), false, now),
                now
            ),
            RowPlan::Read
        );
    }

    /// A failed read is retried no sooner than a successful one would be.
    #[test]
    fn a_failed_read_is_paced_like_a_read() {
        let t0 = Instant::now();
        let i = |attempted_at| RowInputs {
            class: RowClass::Open,
            observed_at: None,
            read_at: None,
            attempted_at,
            applied_at: None,
            applied_wall: None,
            stale_before: None,
            first_closed_seen: None,
            governed_by_listing: false,
            nudge: false,
        };
        assert_eq!(plan_row_refresh(i(None), t0), RowPlan::Read);
        assert_eq!(
            plan_row_refresh(i(Some(t0)), t0 + secs(5)),
            RowPlan::SkipFresh
        );
        assert_eq!(
            plan_row_refresh(i(Some(t0)), t0 + OPEN_REFRESH),
            RowPlan::Read
        );
    }

    #[test]
    fn discovery_is_due_once_per_floor() {
        let t0 = Instant::now();
        assert!(
            discovery_due(None, DISCOVERY_INTERVAL, t0),
            "never discovered"
        );
        let mut d = RepoDiscovery::default();
        assert!(
            discovery_due(Some(&d), DISCOVERY_INTERVAL, t0),
            "never attempted"
        );
        d.last_attempt = Some(t0);
        assert!(!discovery_due(Some(&d), DISCOVERY_INTERVAL, t0 + secs(5)));
        assert!(!discovery_due(
            Some(&d),
            DISCOVERY_INTERVAL,
            t0 + DISCOVERY_INTERVAL - secs(1)
        ));
        assert!(discovery_due(
            Some(&d),
            DISCOVERY_INTERVAL,
            t0 + DISCOVERY_INTERVAL
        ));
        // The nudge floor is shorter, but still a floor.
        assert!(!discovery_due(
            Some(&d),
            NUDGE_DISCOVERY_FLOOR,
            t0 + secs(5)
        ));
        assert!(discovery_due(
            Some(&d),
            NUDGE_DISCOVERY_FLOOR,
            t0 + NUDGE_DISCOVERY_FLOOR
        ));
    }

    #[test]
    fn a_listing_governs_only_while_complete_and_fresh() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.finish_discovery("o/r", &[], true, t0);
        assert!(memo.repos["o/r"].governs(t0 + DISCOVERY_INTERVAL));
        assert!(!memo.repos["o/r"].governs(t0 + LISTING_GOVERNS_FOR + secs(1)));
        memo.finish_discovery("o/r", &[], false, t0);
        assert!(
            !memo.repos["o/r"].governs(t0),
            "an overflowed listing proves nothing"
        );
        memo.discovery_failed("o/r", t0);
        assert!(!memo.repos["o/r"].governs(t0));
    }

    /// D3: a PR opened after the branch's last push is attributed from
    /// discovery — the branch's head is unchanged, so no per-branch call.
    #[test]
    fn discovery_attributes_a_pr_on_an_unchanged_branch_with_no_per_branch_call() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        // An earlier cycle resolved the branch: no PR then.
        memo.note_branch("o/r", "feat/x", "headsha", Some(t0));
        memo.finish_discovery("o/r", &[], true, t0);
        let later = t0 + DISCOVERY_INTERVAL * 3;
        assert_eq!(
            branch_plan(
                false,
                false,
                true,
                memo.branch("o/r", "feat/x"),
                "headsha",
                later
            ),
            BranchPlan::Nothing,
            "unchanged head, no PR known: no call"
        );
        // The PR is opened; the next open listing carries it.
        memo.observe(
            "o/r",
            pr(42, "open", "O:feat/x", "2026-11-19T10:00:00Z"),
            later,
        );
        let found = memo.prs_for_branch("o/r", "o", "feat/x");
        assert_eq!(
            found.iter().map(|o| o.pr.number).collect::<Vec<_>>(),
            vec![42]
        );
        assert_eq!(
            branch_plan(
                !found.is_empty(),
                false,
                true,
                memo.branch("o/r", "feat/x"),
                "headsha",
                later
            ),
            BranchPlan::FromDiscovery
        );
    }

    #[test]
    fn a_per_branch_lookup_runs_only_for_a_moved_head_or_an_incomplete_discovery() {
        let t0 = Instant::now();
        let memo = BranchMemo {
            sha: "old".into(),
            last_lookup: Some(t0),
        };
        let later = t0 + DISCOVERY_INTERVAL;
        assert_eq!(
            branch_plan(false, false, true, None, "s", t0),
            BranchPlan::Lookup,
            "never resolved"
        );
        assert_eq!(
            branch_plan(false, false, true, Some(&memo), "new", later),
            BranchPlan::Lookup
        );
        assert_eq!(
            branch_plan(false, false, true, Some(&memo), "old", later),
            BranchPlan::Nothing
        );
        assert_eq!(
            branch_plan(false, false, false, Some(&memo), "old", later),
            BranchPlan::Lookup
        );
        // Retried no sooner than DISCOVERY_INTERVAL.
        assert_eq!(
            branch_plan(false, false, false, Some(&memo), "new", t0 + secs(5)),
            BranchPlan::Nothing
        );
        // A branch whose PR is already stored is the status pass's job.
        assert_eq!(
            branch_plan(false, true, false, None, "s", t0),
            BranchPlan::Nothing
        );
    }

    /// D3: a PR that opened and closed between two cycles is in neither open
    /// listing, and is found through the closed page.
    #[test]
    fn a_pr_that_opened_and_closed_between_cycles_is_found_through_the_closed_page() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.finish_discovery(
            "o/r",
            &[pr(1, "closed", "o:old", "2026-11-19T09:00:00Z")],
            true,
            t0,
        );
        assert_eq!(
            memo.repos["o/r"].closed_mark.as_deref(),
            Some("2026-11-19T09:00:00Z")
        );

        // Next cycle: the open listing is empty; the closed page carries #8.
        let later = t0 + DISCOVERY_INTERVAL;
        let closed = vec![pr(8, "closed", "o:quick", "2026-11-19T09:30:00Z")];
        for p in &closed {
            memo.observe("o/r", p.clone(), later);
        }
        memo.finish_discovery("o/r", &closed, true, later);
        let found = memo.prs_for_branch("o/r", "o", "quick");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pr.number, 8);
        assert_eq!(found[0].pr.state, "closed");
        assert_eq!(
            memo.repos["o/r"].closed_mark.as_deref(),
            Some("2026-11-19T09:30:00Z"),
            "the mark advances to the newest closed update"
        );
    }

    #[test]
    fn the_closed_mark_never_moves_backwards() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.finish_discovery(
            "o/r",
            &[pr(1, "closed", "o:a", "2026-11-19T10:00:00Z")],
            true,
            t0,
        );
        memo.finish_discovery(
            "o/r",
            &[pr(2, "closed", "o:b", "2026-11-19T08:00:00Z")],
            true,
            t0,
        );
        assert_eq!(
            memo.repos["o/r"].closed_mark.as_deref(),
            Some("2026-11-19T10:00:00Z")
        );
    }

    /// D3: a repo-wide listing carries fork PRs; one whose head ref has our
    /// branch's name is never attributed to our branch.
    #[test]
    fn a_forks_same_named_branch_is_never_attributed() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.observe(
            "o/r",
            pr(5, "open", "forker:feat/x", "2026-11-19T10:00:00Z"),
            t0,
        );
        assert!(memo.prs_for_branch("o/r", "o", "feat/x").is_empty());
        assert_eq!(memo.prs_for_branch("o/r", "forker", "feat/x").len(), 1);
        // And a listing for a different repo never leaks in.
        assert!(memo
            .prs_for_branch("o/other", "forker", "feat/x")
            .is_empty());
    }

    #[test]
    fn a_get_pr_observation_keeps_the_label_index() {
        let t0 = Instant::now();
        let mut memo = ReconcileMemo::default();
        memo.observe("o/r", pr(3, "open", "o:b", "2026-11-19T10:00:00Z"), t0);
        let status = PrStatus {
            state: "closed".into(),
            merged: true,
            merged_at: Some("2026-11-19T11:00:00Z".into()),
            head_sha: "h".into(),
            base_ref: "main".into(),
            closed_at: Some("2026-11-19T11:00:00Z".into()),
            labels: Vec::new(),
        };
        memo.observe("o/r", observed_from_status(3, status), t0 + secs(1));
        let found = memo.prs_for_branch("o/r", "o", "b");
        assert_eq!(found.len(), 1);
        assert!(
            found[0].pr.merged,
            "the newer get_pr observation is the one served"
        );
    }
}
