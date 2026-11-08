//! Typed Terminal pages for autonomous spawns.
//!
//! Every terminal the runner opens on its own — a coord gate continuation, a
//! condition check, a looping agent, a steward — lands on a page named for its
//! KIND, `<group>-<n>`: `post-merge-1`, `post-merge-2`, `impl-1`, `stewards-1`.
//! A page holds at most [`PAGE_ZONE_CEILING`] terminals; when `post-merge-2` is
//! full the next post-merge spawn opens `post-merge-3`. The lowest-numbered page
//! with room always wins, so a page emptied by closed sessions is refilled before
//! a higher one grows.
//!
//! This layers on [`super::page_placement`], the one page-capacity authority:
//! the ceiling is its [`PAGE_ZONE_CEILING`], and a page's load is its
//! [`page_loads`](super::page_placement::page_loads) — live terminals PLUS the
//! rows a boot restore is about to bring back — so a typed page whose restore
//! has not finished re-creating its panes is not refilled by new spawns. Only
//! the CHOICE among pages differs: a typed spawn picks among its own group's
//! `<prefix>-<n>` pages instead of `default`-then-fewest.
//!
//! The page id IS its title. Page labels are frontend-only state (see the
//! `pageId` membership note in `mcp/terminals.rs`; only `page_id` strings travel
//! on terminals and durable session records), so a readable id is what lets the
//! frontend title the page without a second store — `useTerminalPages.ts`
//! `reconcilePages` names a synthesized page by its id when the id is one of
//! these typed pages (`isSpawnGroupPageId`), and "Page N" otherwise. It also makes placement survive a restart: the durable
//! record carries the page id, so a restored session re-lands on its own page.
//!
//! Operator-opened terminals never come through here; they open on the page the
//! operator is looking at, exactly as before.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::page_placement::PAGE_ZONE_CEILING;

/// What kind of autonomous session a spawn is, which decides its page group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SpawnGroup {
    /// A follow-up dispatched when a PR merged (`post-merge-<repo>#<n>`).
    PostMerge,
    /// A follow-up on a PR that is still open (`pr-<repo>#<n>`): CI-green gates,
    /// PR babysitting.
    Pr,
    /// A work-unit dispatch implementing a plan (`impl-<plan stem>`).
    Impl,
    /// A one-shot repair: red-main fixers, PR fixers, alert responders.
    Fixer,
    /// Long-lived singleton loops: the merge-train / dev-ops / cleanup / findings
    /// / plan stewards and every looping agent (the Merge Shepherd among them).
    Steward,
    /// A coord condition check (`check-<condition>`).
    Check,
    /// Any other gate continuation (`resume-<resource>`).
    Resume,
    /// An autonomous spawn that names no recognised kind.
    Spawned,
}

impl SpawnGroup {
    /// Whether `page_id` is one of this group's pages, `<prefix>-<n>` with
    /// `n >= 1` (the frontend's `isSpawnGroupPageId` shape).
    pub(crate) fn owns_page(self, page_id: &str) -> bool {
        page_id
            .strip_prefix(self.page_prefix())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|n| {
                !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
            })
    }

    /// Every group.
    pub(crate) const ALL: [Self; 8] = [
        Self::PostMerge,
        Self::Pr,
        Self::Impl,
        Self::Fixer,
        Self::Steward,
        Self::Check,
        Self::Resume,
        Self::Spawned,
    ];

    /// The page-id prefix; pages are `<prefix>-<n>`. The frontend titles a page
    /// by its id only when the prefix is in `SPAWN_GROUP_PAGE_PREFIXES`
    /// (`src/components/terminal/useTerminalPages.ts`); a test below keeps the
    /// two lists equal.
    pub(crate) fn page_prefix(self) -> &'static str {
        match self {
            Self::PostMerge => "post-merge",
            Self::Pr => "pr",
            Self::Impl => "impl",
            Self::Fixer => "fixer",
            Self::Steward => "stewards",
            Self::Check => "checks",
            Self::Resume => "resume",
            Self::Spawned => "spawned",
        }
    }

    /// Classify a spawn from its session name (coord's intent-derived
    /// `session_name`, or the runner's own `loop-` / `check-` / steward name) and
    /// its initial prompt.
    ///
    /// The NAME decides first wherever it names a kind, because coord derived it
    /// from facts (the gate predicate, the work unit) rather than prose. The
    /// prompt's leading slash command is consulted only for the generic names
    /// (`pr-`, `resume-`, none): a `resume-` continuation that runs
    /// `/fix-red-main` is a fixer, while a `post-merge-` one that mentions
    /// `/babysit-prs` stays a post-merge follow-up.
    pub(crate) fn classify(name: Option<&str>, prompt: Option<&str>) -> Self {
        let name = name.map(str::trim).unwrap_or("").to_ascii_lowercase();
        if name.starts_with("post-merge-") {
            return Self::PostMerge;
        }
        if name.starts_with("impl-") {
            return Self::Impl;
        }
        if name.starts_with("check-") {
            return Self::Check;
        }
        if FIXER_NAME_PREFIXES.iter().any(|p| name.starts_with(p)) {
            return Self::Fixer;
        }
        // Coord's generic names end in a free-form subject (a file stem), so a
        // `resume-…-steward` names a resource, not a steward session.
        let coord_generic = name.starts_with("pr-") || name.starts_with("resume-");
        if name.starts_with("loop-") || (!coord_generic && is_steward_skill(&name)) {
            return Self::Steward;
        }
        if let Some((skill, rest)) = prompt.and_then(leading_slash_command) {
            if is_fixer_skill(skill)
                || (skill == "merge-train-steward" && rest.contains("--respond-to-alert"))
            {
                return Self::Fixer;
            }
            if is_steward_skill(skill) {
                return Self::Steward;
            }
        }
        if name.starts_with("pr-") {
            return Self::Pr;
        }
        if name.starts_with("resume-") {
            return Self::Resume;
        }
        Self::Spawned
    }
}

/// Whether `page_id` is any group's typed page (`<prefix>-<n>`, `n >= 1`) —
/// the Rust twin of the frontend's `isSpawnGroupPageId`.
pub(crate) fn is_spawn_group_page(page_id: &str) -> bool {
    SpawnGroup::ALL.iter().any(|g| g.owns_page(page_id))
}

/// The name prefixes coord's fixer lanes stamp on a launch (plan
/// `2026-11-08-coord-fixers-open-terminal-tabs-and-spawn-kinds-are-capped`, the
/// wire-contract table): `fix-<repo>#<pr>` (pr_fix), `shepherd-<repo>#<pr>`
/// (the unlandable-PR shepherd tiers), `review-<repo>#<pr>` (escalate-review),
/// `red-main-<repo>` (the red-main fixer) and `respond-…` (the red-main and
/// admission-stall responders). Like `post-merge-` / `impl-` / `check-` these
/// are SPECIFIC names, so they decide before the prompt is read.
const FIXER_NAME_PREFIXES: [&str; 5] = ["fix-", "shepherd-", "review-", "red-main-", "respond-"];

/// Skills whose session is a one-shot repair.
fn is_fixer_skill(skill: &str) -> bool {
    matches!(
        skill,
        "fix"
            | "ufix"
            | "fix-red-main"
            | "test-fix"
            | "code-fix"
            | "auto-fix"
            | "adjudicate-bundle-red"
    )
}

/// Skills whose session is a long-lived steward loop.
fn is_steward_skill(skill: &str) -> bool {
    skill.ends_with("-steward") || skill == "merge-shepherd"
}

/// The first `/skill` the prompt invokes, and the rest of that line. Only the
/// prompt's first non-empty line is read: coord puts the invocation there, and
/// a slash command quoted later in a brief is context, not the session's job.
fn leading_slash_command(prompt: &str) -> Option<(&str, &str)> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    let at = line
        .match_indices('/')
        .find(|(i, _)| {
            *i == 0
                || line
                    .get(..*i)
                    .is_some_and(|before| before.ends_with(char::is_whitespace))
        })?
        .0;
    let after = line.get(at + 1..)?;
    let end = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(after.len());
    let (skill, rest) = after.split_at_checked(end)?;
    if skill.is_empty() || !skill.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    Some((skill, rest))
}

/// The page a new `group` terminal lands on: `<prefix>-<n>` for the smallest
/// `n >= 1` whose page holds fewer than `ceiling` terminals. A page absent from
/// `counts` is empty, so when every existing page is full this opens the next
/// one.
pub(crate) fn pick_group_page(
    group: SpawnGroup,
    counts: &HashMap<String, usize>,
    ceiling: usize,
) -> String {
    let prefix = group.page_prefix();
    (1usize..)
        .map(|n| format!("{prefix}-{n}"))
        .find(|id| counts.get(id).copied().unwrap_or(0) < ceiling.max(1))
        .expect("an unbounded range always yields a page with room")
}

/// The process-wide reservation ledger.
///
/// `pages`: placements chosen but not yet created, per page. A spawn path picks
/// its page, then awaits (account selection, trust warm-up) before the terminal
/// exists. Two spawns racing through that window would both read the same count
/// and overfill one page; counting the reservation closes it.
///
/// `slots`: per-group admissions under the kind cap — a headless run for its
/// whole life, and a terminal run from admission until its session starts (see
/// [`KindSlot`]).
#[derive(Default)]
struct Reservations {
    pages: HashMap<String, usize>,
    slots: HashMap<SpawnGroup, usize>,
}

fn lock_pending() -> std::sync::MutexGuard<'static, Reservations> {
    static PENDING: OnceLock<Mutex<Reservations>> = OnceLock::new();
    PENDING
        .get_or_init(|| Mutex::new(Reservations::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn decrement<K: std::hash::Hash + Eq>(map: &mut HashMap<K, usize>, key: &K) {
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

/// A chosen page, reserved until the terminal is created. Release it (or drop
/// it) right after the create returns: once the terminal is listed by the
/// manager a held reservation would count it twice.
#[derive(Debug)]
pub(crate) struct PagePlacement {
    page_id: String,
}

impl PagePlacement {
    pub(crate) fn page_id(&self) -> &str {
        &self.page_id
    }

    /// End the reservation. Same as dropping it; named so call sites read.
    pub(crate) fn release(self) {}
}

impl Drop for PagePlacement {
    fn drop(&mut self) {
        decrement(&mut lock_pending().pages, &self.page_id);
    }
}

/// `group`'s per-page counts: its pending page reservations plus the
/// `page_loads` of its own `<prefix>-<n>` pages. Called under the lock.
fn group_page_counts<I>(
    group: SpawnGroup,
    res: &Reservations,
    page_loads: I,
) -> HashMap<String, usize>
where
    I: IntoIterator<Item = (String, usize)>,
{
    let mut counts: HashMap<String, usize> = res
        .pages
        .iter()
        .filter(|(id, _)| group.owns_page(id))
        .map(|(id, n)| (id.clone(), *n))
        .collect();
    for (id, load) in page_loads {
        if group.owns_page(&id) {
            *counts.entry(id).or_insert(0) += load;
        }
    }
    counts
}

/// Pick and reserve `group`'s page. Called under the lock.
fn reserve_page<I>(group: SpawnGroup, res: &mut Reservations, page_loads: I) -> PagePlacement
where
    I: IntoIterator<Item = (String, usize)>,
{
    let counts = group_page_counts(group, res, page_loads);
    let page_id = pick_group_page(group, &counts, PAGE_ZONE_CEILING);
    *res.pages.entry(page_id.clone()).or_insert(0) += 1;
    PagePlacement { page_id }
}

/// Choose and reserve the page for a new `group` terminal.
///
/// Cost, accepted: the loads are read while the process-wide reservation lock
/// is held, so concurrent typed spawns serialize behind one terminal list and
/// one lifecycle-store read. That ordering is what keeps two spawns from both
/// reading a page as having room (a create-and-release cannot slip between the
/// read and the reservation); spawns are rare enough that the wait is small.
/// `page_loads` yields
/// per-page loads — at the call sites,
/// [`page_placement::page_loads`](super::page_placement::page_loads), so rows
/// awaiting restore count — of which only `group`'s own pages are kept. It is
/// read UNDER the reservation lock, so a spawn that creates its terminal and
/// releases its reservation cannot slip between this read and this reservation.
///
/// UNCAPPED. The kind cap is an admission taken earlier, by [`try_reserve`],
/// whose [`KindSlot`] the caller holds across this placement until its session
/// starts; spawns that are never capped (stewards, condition checks) place
/// here with no slot at all.
pub(crate) fn place<I>(group: SpawnGroup, page_loads: impl FnOnce() -> I) -> PagePlacement
where
    I: IntoIterator<Item = (String, usize)>,
{
    let mut res = lock_pending();
    reserve_page(group, &mut res, page_loads())
}

// =============================================================================
// Per-kind live caps (plan
// `2026-11-08-coord-fixers-open-terminal-tabs-and-spawn-kinds-are-capped`
// Phase 3)
// =============================================================================

/// The default live cap for each group, before the operator's `spawn_caps`
/// overrides. `None` = unlimited.
///
/// What is ADMITTED against a cap: coord launches (`LaunchPayload`, both arms)
/// and gate continuations — the spawns that have a re-offerable deferral door.
/// Condition checks (`ConditionCheckPayload`) and the runner's own steward /
/// looping-agent spawns are placed uncapped: a condition check has no deferral
/// door (only a run-failed report), so it is not refused here. Its tab still
/// COUNTS toward the `checks` cap while it runs, so `checks` bounds the
/// launches and continuations classified `check-…`, not condition checks.
pub(crate) fn default_spawn_cap(group: SpawnGroup) -> Option<usize> {
    match group {
        SpawnGroup::Fixer => Some(6),
        SpawnGroup::PostMerge
        | SpawnGroup::Pr
        | SpawnGroup::Impl
        | SpawnGroup::Resume
        | SpawnGroup::Check
        | SpawnGroup::Spawned => Some(9),
        SpawnGroup::Steward => None,
    }
}

/// The effective cap for `group` under the operator's `overrides` (settings
/// `spawn_caps`, keyed by page prefix). An override replaces its own key's
/// default only — a key absent from the map keeps the default — and `0` means
/// unlimited. `stewards` is never capped whatever the map says: a steward is a
/// singleton loop, and deferring one would stop a lane rather than bound it.
pub(crate) fn effective_spawn_cap(
    group: SpawnGroup,
    overrides: &HashMap<String, usize>,
) -> Option<usize> {
    if group == SpawnGroup::Steward {
        return None;
    }
    match overrides.get(group.page_prefix()) {
        Some(0) => None,
        Some(n) => Some(*n),
        None => default_spawn_cap(group),
    }
}

/// A refused admission: `group` already holds `count` live sessions against
/// its `cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AtCap {
    pub(crate) group: SpawnGroup,
    pub(crate) count: usize,
    pub(crate) cap: usize,
}

/// One admitted autonomous session of `group`, counted against its kind's cap
/// while it is held.
///
/// The count it adds is what makes the cap PER KIND rather than per tab: a
/// headless run (a `LaunchPayload` without `presentation: terminal`, or a
/// headless gate continuation) holds its slot for its whole life, so a headless
/// fixer counts against the fixer cap exactly as a fixer tab does. A terminal
/// run holds it from admission until its session STARTS — across its page
/// placement and any never-started retry — and drops it then, when its running
/// terminal carries the count. Dropping the slot releases it.
///
/// Two edges, both accepted: while a terminal session is starting it is
/// counted twice (its slot, and its reservation or its running terminal), so a
/// concurrent admission in that window can be refused one early — the safe
/// direction, and coord re-offers a refused launch; and a tab the operator
/// drags off its kind's pages stops counting, because the count is per page
/// (`<prefix>-<n>`).
#[derive(Debug)]
pub(crate) struct KindSlot {
    group: SpawnGroup,
}

impl Drop for KindSlot {
    fn drop(&mut self) {
        decrement(&mut lock_pending().slots, &self.group);
    }
}

/// `group`'s live count: the load on its pages (live terminals plus rows
/// awaiting restore), its pending page reservations, and its page-less slots.
/// Called under the lock.
fn group_count<I>(group: SpawnGroup, res: &Reservations, page_loads: I) -> usize
where
    I: IntoIterator<Item = (String, usize)>,
{
    group_page_counts(group, res, page_loads)
        .values()
        .sum::<usize>()
        + res.slots.get(&group).copied().unwrap_or(0)
}

/// Admit one more `group` session against `cap` (`None` = unlimited), or
/// refuse with the count that filled it.
///
/// The count is read and the slot taken under the SAME lock [`place`] reserves
/// under, with `page_loads` read inside it, so two concurrent admissions at
/// `cap - 1` cannot both pass: the second one counts the first one's slot.
pub(crate) fn try_reserve<I>(
    group: SpawnGroup,
    cap: Option<usize>,
    page_loads: impl FnOnce() -> I,
) -> Result<KindSlot, AtCap>
where
    I: IntoIterator<Item = (String, usize)>,
{
    let mut res = lock_pending();
    if let Some(cap) = cap {
        let count = group_count(group, &res, page_loads());
        if count >= cap {
            return Err(AtCap { group, count, cap });
        }
    }
    *res.slots.entry(group).or_insert(0) += 1;
    Ok(KindSlot { group })
}

/// Every group's live count and effective cap, for `GET /spawn-caps`. Read
/// under the reservation lock, so it agrees with what [`try_reserve`] would
/// count at the same instant.
pub(crate) fn snapshot<I>(
    overrides: &HashMap<String, usize>,
    page_loads: impl FnOnce() -> I,
) -> Vec<(SpawnGroup, usize, Option<usize>)>
where
    I: IntoIterator<Item = (String, usize)>,
{
    let res = lock_pending();
    let loads: Vec<(String, usize)> = page_loads().into_iter().collect();
    SpawnGroup::ALL
        .iter()
        .map(|&g| {
            (
                g,
                group_count(g, &res, loads.iter().cloned()),
                effective_spawn_cap(g, overrides),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pairs: &[(&str, usize)]) -> HashMap<String, usize> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn coord_session_names_pick_their_group() {
        let cases = [
            ("post-merge-runner#1863", SpawnGroup::PostMerge),
            ("pr-coord#12", SpawnGroup::Pr),
            ("impl-terminal-titlebar-shows-session…", SpawnGroup::Impl),
            ("check-deploy-healthy", SpawnGroup::Check),
            ("resume-claim-abc", SpawnGroup::Resume),
            ("loop-Merge Shepherd", SpawnGroup::Steward),
            ("merge-train-steward", SpawnGroup::Steward),
            ("cleanup-steward", SpawnGroup::Steward),
            ("dev-ops-steward", SpawnGroup::Steward),
            ("Gate continuation", SpawnGroup::Spawned),
        ];
        for (name, want) in cases {
            assert_eq!(SpawnGroup::classify(Some(name), None), want, "{name}");
        }
        assert_eq!(SpawnGroup::classify(None, None), SpawnGroup::Spawned);
    }

    #[test]
    fn coord_fixer_lane_names_are_fixers() {
        // One name per lane in the plan's wire-contract table.
        for name in [
            "fix-runner#1863",
            "shepherd-coord#42",
            "review-web#7",
            "red-main-runner",
            "respond-red-main-coord",
            "respond-stall-runner",
        ] {
            assert_eq!(
                SpawnGroup::classify(Some(name), None),
                SpawnGroup::Fixer,
                "{name}"
            );
        }
    }

    #[test]
    fn a_fixer_name_is_not_overridden_by_the_prompt() {
        // Name first: a fixer whose brief opens with a steward or PR skill is
        // still a fixer, and a fixer name ending in `-steward` is not a steward.
        assert_eq!(
            SpawnGroup::classify(Some("shepherd-runner#9"), Some("/babysit-prs 9")),
            SpawnGroup::Fixer
        );
        assert_eq!(
            SpawnGroup::classify(Some("fix-merge-train-steward"), Some("/cleanup-steward")),
            SpawnGroup::Fixer
        );
        // The prefix must be the whole leading word: `prefix-` names are not
        // fixers, and neither are names that merely contain a fixer word.
        assert_eq!(
            SpawnGroup::classify(Some("prefix-thing"), None),
            SpawnGroup::Spawned
        );
        assert_eq!(
            SpawnGroup::classify(Some("resume-fix-red-main"), None),
            SpawnGroup::Resume
        );
    }

    #[test]
    fn a_generic_name_defers_to_the_prompts_leading_skill() {
        let fix = Some("/fix-red-main qontinui/qontinui-coord\n\nbrief");
        assert_eq!(
            SpawnGroup::classify(Some("resume-red-main"), fix),
            SpawnGroup::Fixer
        );
        assert_eq!(
            SpawnGroup::classify(None, Some("run /ufix --once")),
            SpawnGroup::Fixer
        );
        let responder = Some("/merge-train-steward --respond-to-alert 42 --once");
        assert_eq!(SpawnGroup::classify(None, responder), SpawnGroup::Fixer);
        assert_eq!(
            SpawnGroup::classify(Some("resume-x"), Some("/cleanup-steward --once")),
            SpawnGroup::Steward
        );
        assert_eq!(
            SpawnGroup::classify(Some("pr-runner#5"), Some("/babysit-prs")),
            SpawnGroup::Pr
        );
        // A resource stem that happens to end in `-steward` is not a steward.
        assert_eq!(
            SpawnGroup::classify(Some("resume-retire-the-plan-steward"), None),
            SpawnGroup::Resume
        );
    }

    #[test]
    fn a_specific_name_is_not_overridden_by_the_prompt() {
        let prompt = Some("/fix-red-main qontinui/qontinui-runner");
        assert_eq!(
            SpawnGroup::classify(Some("post-merge-runner#1"), prompt),
            SpawnGroup::PostMerge
        );
        assert_eq!(
            SpawnGroup::classify(Some("impl-x"), prompt),
            SpawnGroup::Impl
        );
    }

    #[test]
    fn only_the_first_line_and_a_word_initial_slash_count() {
        // A path is not a slash command.
        assert_eq!(
            SpawnGroup::classify(None, Some("see src/fix.rs")),
            SpawnGroup::Spawned
        );
        // A skill quoted in the brief body is context, not the job.
        assert_eq!(
            SpawnGroup::classify(None, Some("Look at PR 4\nthen /fix-red-main")),
            SpawnGroup::Spawned
        );
    }

    #[test]
    fn picks_the_lowest_numbered_page_with_room() {
        let c = PAGE_ZONE_CEILING;
        assert_eq!(
            pick_group_page(SpawnGroup::PostMerge, &counts(&[]), c),
            "post-merge-1"
        );
        assert_eq!(
            pick_group_page(SpawnGroup::PostMerge, &counts(&[("post-merge-1", 8)]), c),
            "post-merge-1"
        );
        assert_eq!(
            pick_group_page(
                SpawnGroup::PostMerge,
                &counts(&[("post-merge-1", 9), ("post-merge-2", 9)]),
                c
            ),
            "post-merge-3"
        );
        // A freed slot on a lower page is refilled before a higher page grows.
        assert_eq!(
            pick_group_page(
                SpawnGroup::PostMerge,
                &counts(&[("post-merge-1", 4), ("post-merge-2", 3)]),
                c
            ),
            "post-merge-1"
        );
        // A gap (page 1 closed) is reopened.
        assert_eq!(
            pick_group_page(SpawnGroup::Impl, &counts(&[("impl-2", 9)]), c),
            "impl-1"
        );
        // Other groups' pages and the operator's pages are never chosen.
        assert_eq!(
            pick_group_page(
                SpawnGroup::Steward,
                &counts(&[("default", 0), ("post-merge-1", 0)]),
                c
            ),
            "stewards-1"
        );
    }

    #[test]
    fn a_reservation_counts_until_released() {
        // A group no other test uses, so parallel tests share no page.
        let live = || || vec![("checks-1".to_string(), PAGE_ZONE_CEILING - 1)];
        let first = place(SpawnGroup::Check, live());
        assert_eq!(first.page_id(), "checks-1");
        let second = place(SpawnGroup::Check, live());
        assert_eq!(
            second.page_id(),
            "checks-2",
            "the reserved last slot of checks-1 is taken"
        );
        first.release();
        second.release();
        let third = place(SpawnGroup::Check, live());
        assert_eq!(third.page_id(), "checks-1", "releasing frees the slot");
    }

    #[test]
    fn restore_aware_loads_fill_a_typed_page_and_other_pages_are_ignored() {
        // `resume-1` is full only once its awaiting-restore rows are counted
        // (the load `page_placement::page_loads` reports), so the next resume
        // spawn opens `resume-2`. Loads on `default` and on another group's
        // pages never count against this group. A group no other test uses.
        let loads = || {
            vec![
                ("default".to_string(), 50),
                ("resume-1".to_string(), PAGE_ZONE_CEILING),
                ("pr-2".to_string(), PAGE_ZONE_CEILING),
                ("resume-10x".to_string(), PAGE_ZONE_CEILING),
            ]
        };
        let p = place(SpawnGroup::Resume, loads);
        assert_eq!(p.page_id(), "resume-2");
    }

    #[test]
    fn any_groups_numbered_page_is_a_typed_page() {
        assert!(is_spawn_group_page("post-merge-3"));
        assert!(is_spawn_group_page("stewards-1"));
        assert!(!is_spawn_group_page("default"));
        assert!(!is_spawn_group_page("3f2b8c1e-9a4d-4e2f-b1c7-123456789012"));
        assert!(!is_spawn_group_page("minted-1"));
    }

    #[test]
    fn concurrent_placements_never_overfill_a_page() {
        // A group no other test uses. One slot left on `spawned-1`: of eight
        // racing placements exactly one takes it and the rest open `spawned-2`.
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    place(SpawnGroup::Spawned, || {
                        vec![("spawned-1".to_string(), PAGE_ZONE_CEILING - 1)]
                    })
                })
            })
            .collect();
        let placements: Vec<PagePlacement> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();
        let on_first = placements
            .iter()
            .filter(|p| p.page_id() == "spawned-1")
            .count();
        assert_eq!(on_first, 1);
        assert!(placements
            .iter()
            .all(|p| p.page_id() == "spawned-1" || p.page_id() == "spawned-2"));
    }

    #[test]
    fn a_group_owns_only_its_own_numbered_pages() {
        assert!(SpawnGroup::PostMerge.owns_page("post-merge-1"));
        assert!(SpawnGroup::Pr.owns_page("pr-12"));
        assert!(!SpawnGroup::Pr.owns_page("post-merge-1"));
        assert!(!SpawnGroup::Pr.owns_page("pr-0"));
        assert!(!SpawnGroup::Pr.owns_page("pr-"));
        assert!(!SpawnGroup::Pr.owns_page("pr-1a"));
        assert!(!SpawnGroup::Steward.owns_page("default"));
    }

    #[test]
    fn the_frontend_titles_exactly_these_page_prefixes() {
        let ts_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/components/terminal/useTerminalPages.ts");
        let ts = std::fs::read_to_string(&ts_path).expect("read useTerminalPages.ts");
        let start = ts
            .find("SPAWN_GROUP_PAGE_PREFIXES = [")
            .expect("SPAWN_GROUP_PAGE_PREFIXES declared");
        let list = ts
            .get(start..)
            .and_then(|tail| tail.split_once(']'))
            .map(|(list, _)| list)
            .expect("list closes");
        let mut ts_prefixes: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
        let mut rust_prefixes: Vec<&str> =
            SpawnGroup::ALL.iter().map(|g| g.page_prefix()).collect();
        ts_prefixes.sort_unstable();
        rust_prefixes.sort_unstable();
        assert_eq!(ts_prefixes, rust_prefixes);
    }

    // ---- per-kind caps. Each test uses a group no placement test above uses,
    // because the reservation ledger is process-global and tests run in
    // parallel.

    fn no_loads() -> Vec<(String, usize)> {
        Vec::new()
    }

    #[test]
    fn spawn_cap_defaults_and_overrides() {
        let none = HashMap::new();
        assert_eq!(effective_spawn_cap(SpawnGroup::Fixer, &none), Some(6));
        for g in [
            SpawnGroup::PostMerge,
            SpawnGroup::Pr,
            SpawnGroup::Impl,
            SpawnGroup::Resume,
            SpawnGroup::Check,
            SpawnGroup::Spawned,
        ] {
            assert_eq!(effective_spawn_cap(g, &none), Some(9), "{g:?}");
        }
        // An override replaces only its own key; `0` is unlimited.
        let o = counts(&[("fixer", 0), ("pr", 2)]);
        assert_eq!(effective_spawn_cap(SpawnGroup::Fixer, &o), None);
        assert_eq!(effective_spawn_cap(SpawnGroup::Pr, &o), Some(2));
        assert_eq!(effective_spawn_cap(SpawnGroup::Impl, &o), Some(9));
    }

    #[test]
    fn stewards_are_never_capped() {
        assert_eq!(default_spawn_cap(SpawnGroup::Steward), None);
        let o = counts(&[("stewards", 1)]);
        assert_eq!(effective_spawn_cap(SpawnGroup::Steward, &o), None);
        // So no load on their pages ever refuses one.
        let full = || vec![("stewards-1".to_string(), 1000)];
        let slot = try_reserve(
            SpawnGroup::Steward,
            effective_spawn_cap(SpawnGroup::Steward, &o),
            full,
        );
        assert!(slot.is_ok());
    }

    #[test]
    fn an_unlimited_cap_admits_any_load() {
        let heavy = || vec![("post-merge-1".to_string(), 500)];
        let slots: Vec<_> = (0..20)
            .map(|_| try_reserve(SpawnGroup::PostMerge, None, heavy).expect("unlimited"))
            .collect();
        assert_eq!(slots.len(), 20);
    }

    #[test]
    fn a_full_kind_is_refused_with_its_count_until_a_session_ends() {
        // One live fixer tab on `fixer-1` (a different page's load and another
        // group's page do not count), cap 2.
        let loads = || {
            vec![
                ("fixer-1".to_string(), 1),
                ("default".to_string(), 40),
                ("pr-1".to_string(), 9),
            ]
        };
        let first = try_reserve(SpawnGroup::Fixer, Some(2), loads).expect("1 < 2");
        let refused = try_reserve(SpawnGroup::Fixer, Some(2), loads).unwrap_err();
        assert_eq!(
            refused,
            AtCap {
                group: SpawnGroup::Fixer,
                count: 2,
                cap: 2
            }
        );
        drop(first);
        assert!(try_reserve(SpawnGroup::Fixer, Some(2), loads).is_ok());
    }

    #[test]
    fn a_slot_counts_until_dropped_and_a_starting_terminal_counts_twice() {
        let slot = try_reserve(SpawnGroup::Impl, Some(2), no_loads).expect("empty kind");
        // A terminal run places while still holding its slot: the start window
        // over-counts by one (the accepted, safe-direction edge).
        let placement = place(SpawnGroup::Impl, no_loads);
        assert_eq!(placement.page_id(), "impl-1");
        let refused = try_reserve(SpawnGroup::Impl, Some(2), no_loads).unwrap_err();
        assert_eq!(refused.count, 2);
        // Created and running: the manager's load carries it, the reservation
        // is released, and the slot drops at the start verdict.
        placement.release();
        drop(slot);
        let running = || vec![("impl-1".to_string(), 1)];
        assert_eq!(
            try_reserve(SpawnGroup::Impl, Some(1), running)
                .unwrap_err()
                .count,
            1
        );
        // The session ends.
        assert!(try_reserve(SpawnGroup::Impl, Some(1), no_loads).is_ok());
    }

    #[test]
    fn concurrent_admissions_at_cap_minus_one_admit_exactly_one() {
        // Two live `pr` tabs against a cap of 3: of eight racing admissions
        // exactly one passes.
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    try_reserve(SpawnGroup::Pr, Some(3), || vec![("pr-1".to_string(), 2)])
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| e.count == 3 && e.cap == 3));
    }
}
