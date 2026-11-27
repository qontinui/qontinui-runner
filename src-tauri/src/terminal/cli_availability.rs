//! An account migration WAITS for a stably launchable `claude` instead of
//! failing while the CLI is mid-reinstall.
//!
//! Plan `2026-11-10-migration-holds-for-missing-claude-cli` D1 / Phase 4. A
//! usage-limit migration's old pane sits at its limit menu, and the usage-limit
//! scanner fires a frozen screen's hint once (`usage_limit.rs`'s firing
//! policy): a migration that gives up on a missing CLI strands the session on
//! its exhausted account until a human moves it. A 24 h sampler on nilan
//! (2026-11-10) measured 908 unusable `bin/claude` windows, max 278.5 s, so a
//! fixed retry budget of seconds cannot cover them. The migration therefore
//! HOLDS, under the old pane, the lifecycle poll hold and the continuation
//! anchor reservation it already keeps, polling every
//! [`MIGRATION_CLI_HOLD_TICK`] up to [`MIGRATION_CLI_HOLD_CEILING`] (twice the
//! largest measured window, with margin).
//!
//! **What "launchable" means.** The existing resolver
//! ([`crate::agent_runtime::try_resolve_launchable_claude_bin`]) names a path —
//! no third probe is added — and the hold judges that path ITSELF
//! ([`judge_resolved`]): it exists, carries an exec bit, is not npm's
//! placeholder stub, and has settled ([`settle_of`] — the one shared
//! quiescence predicate, change time at least 2 s old). The judgement matters
//! for two shapes the resolver passes: an absolute `QONTINUI_CLAUDE_BIN`
//! override, which it returns unjudged, and a native image still being
//! written (the sampler saw 87 such windows, as small as 11.8 MB of 341.9 MB).
//!
//! **Only the migration holds — both of its doors.** The automatic usage-limit
//! migration and the operator's manual `terminal_migrate_session_account` go
//! through the same respawn, so both hold. The resume door and the respawn
//! receiver keep
//! their short typed `claude_cli_unavailable:not_found` refusal: an HTTP caller
//! waits on the door, and coord already re-drives that head.
//!
//! The hold watches its `abort` check every tick ([`old_pane_aborts_hold`]) —
//! a user close of the old pane or of a pane this respawn already spawned
//! (both held by the respawn's close hold), or, before the first spawn, the
//! old pane gone — so a user's close is never kept pending for the whole
//! ceiling. The same check is polled while the hold first waits out a
//! runner-owned CLI install ([`await_unless_aborted`]).

use std::future::Future;
use std::path::Path;
use std::time::{Duration, SystemTime};

use qontinui_runner_lib::intercept_core::npm_placeholder::is_npm_placeholder;
use qontinui_runner_lib::intercept_core::settled::{settle_of, Settle};
use tracing::warn;

use super::respawn_launch::{SpawnNextError, SpawnedPane};
use crate::claude_cli_spawn::CLI_UNAVAILABLE_HEAD;

/// How long a migration waits, in all, for a launchable `claude` before it
/// gives up and leaves the session on the old pane. Twice the largest measured
/// unusable window (278.5 s → 557 s), rounded up to 15 min.
pub(crate) const MIGRATION_CLI_HOLD_CEILING: Duration = Duration::from_secs(15 * 60);

/// The hold's poll cadence.
pub(crate) const MIGRATION_CLI_HOLD_TICK: Duration = Duration::from_secs(1);

/// What the last look found instead of a launchable `claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LastSeen {
    /// Nothing launchable resolved (no file, or the resolver missed: it also
    /// skips a PATH stub and a non-executable PATH hit), or the stat failed.
    Absent,
    /// The resolved path is npm's placeholder stub.
    Stub,
    /// The resolved path has no exec bit.
    NotExecutable,
    /// The resolved path's target changed less than 2 s ago: an install may
    /// still be writing it.
    Changing,
}

impl LastSeen {
    /// The `claude_cli_unavailable:<kind>` token.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Stub => "stub",
            Self::NotExecutable => "not_executable",
            Self::Changing => "changing",
        }
    }
}

impl std::fmt::Display for LastSeen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.token())
    }
}

/// One look at the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Look {
    Launchable(String),
    Unlaunchable(LastSeen),
}

/// Judge a RESOLVED path: present, executable, not the stub, settled. A failed
/// stat is [`LastSeen::Absent`]. A change time that cannot be read is treated
/// as settled ([`Settle::Unknown`], as `settled.rs` prescribes for a launch
/// gate).
pub(crate) fn judge_resolved(path: &Path, now: SystemTime) -> Result<(), LastSeen> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Err(LastSeen::Absent);
    };
    if !meta.is_file() {
        return Err(LastSeen::Absent);
    }
    if !has_exec_bit(&meta) {
        return Err(LastSeen::NotExecutable);
    }
    if is_npm_placeholder(path) {
        return Err(LastSeen::Stub);
    }
    match settle_of(path, now) {
        Settle::Settled | Settle::Unknown => Ok(()),
        Settle::Changing { .. } => Err(LastSeen::Changing),
    }
}

#[cfg(unix)]
fn has_exec_bit(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Windows has no exec bit; the resolver only names extensions
/// `CreateProcessW` launches.
#[cfg(not(unix))]
fn has_exec_bit(_meta: &std::fs::Metadata) -> bool {
    true
}

/// What the resolver answered (`None` = a miss), judged at `now`.
pub(crate) fn look_at(resolved: Option<String>, now: SystemTime) -> Look {
    let Some(path) = resolved else {
        return Look::Unlaunchable(LastSeen::Absent);
    };
    match judge_resolved(Path::new(&path), now) {
        Ok(()) => Look::Launchable(path),
        Err(seen) => Look::Unlaunchable(seen),
    }
}

/// Where the hold's looks come from: the real resolver in production, a script
/// in the tests.
pub(crate) trait CliProbe: Send {
    fn look(&mut self) -> impl Future<Output = Look> + Send;
}

/// The production probe: the existing resolver, off the async worker (it stats
/// and reads the filesystem), then [`look_at`]. One probe serves one
/// migration's hold, so `warned` keeps a failing resolver to one warning per
/// hold rather than one per tick.
#[derive(Default)]
pub(crate) struct ResolverProbe {
    warned: bool,
}

impl CliProbe for ResolverProbe {
    async fn look(&mut self) -> Look {
        qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(|| {
            look_at(
                crate::agent_runtime::try_resolve_launchable_claude_bin(),
                SystemTime::now(),
            )
        })
        .await
        .unwrap_or_else(|e| self.on_resolver_failure(&e))
    }
}

impl ResolverProbe {
    /// A failed resolver task reads as an absent CLI. A panicking resolver
    /// would otherwise read that way for the whole ceiling with nothing naming
    /// the panic (review r7 A4), so the first failure warns; later ones log at
    /// debug, since the hold looks every tick (review r8 A2).
    fn on_resolver_failure(&mut self, e: &dyn std::fmt::Display) -> Look {
        if self.warned {
            tracing::debug!(error = %e, "migration: the Claude CLI resolver task failed again");
        } else {
            self.warned = true;
            warn!(error = %e, "migration: the Claude CLI resolver task failed — treating the CLI as absent");
        }
        Look::Unlaunchable(LastSeen::Absent)
    }
}

/// The hold ran to its ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliUnavailable {
    /// How long the migration held, in all.
    pub waited: Duration,
    pub last_seen: LastSeen,
}

impl std::fmt::Display for CliUnavailable {
    /// `claude_cli_unavailable:<kind> after <n>s`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{CLI_UNAVAILABLE_HEAD}{} after {}s",
            self.last_seen.token(),
            self.waited.as_secs()
        )
    }
}

/// The migration hold's `abort` rule (plan D1). `user_closed`: a USER close of
/// the old pane, or of any pane this respawn already spawned — it always ends
/// the hold: the session is to end (or stay), not move. The old pane being
/// GONE some other way (its CLI exited, the pane vanished) ends only the FIRST
/// spawn's hold — nothing has moved yet, so there is nothing left to migrate.
/// Once a pane has been spawned, the respawn is the session's only rescue: a
/// retry must still spawn, and `confirm_respawn` settles the old pane's fate
/// itself.
pub(crate) fn old_pane_aborts_hold(user_closed: bool, gone: bool, first_spawn: bool) -> bool {
    user_closed || (first_spawn && gone)
}

/// Await `fut`, polling `abort` every `tick` meanwhile. `None` when `abort`
/// answered true first (`fut` is dropped unfinished).
pub(crate) async fn await_unless_aborted<F, A>(
    fut: F,
    abort: &mut A,
    tick: Duration,
) -> Option<F::Output>
where
    F: Future,
    A: FnMut() -> bool,
{
    tokio::pin!(fut);
    loop {
        if abort() {
            return None;
        }
        tokio::select! {
            out = &mut fut => return Some(out),
            () = tokio::time::sleep(tick) => {}
        }
    }
}

/// Why [`await_launchable_claude`] returned no path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HoldEnd {
    CliUnavailable(CliUnavailable),
    /// `abort` answered true (see [`old_pane_aborts_hold`]).
    Aborted,
}

/// One migration's hold state: its ceiling and cadence, how long it has held
/// so far across EVERY spawn of the migration (a retry, or a spawn error that
/// re-enters the hold, spends the same budget), and whether `on_hold` fired.
#[derive(Debug, Clone)]
pub(crate) struct CliHold {
    ceiling: Duration,
    tick: Duration,
    held: Duration,
    announced: bool,
}

impl CliHold {
    pub(crate) fn new(ceiling: Duration, tick: Duration) -> Self {
        Self {
            ceiling,
            tick,
            held: Duration::ZERO,
            announced: false,
        }
    }

    /// [`MIGRATION_CLI_HOLD_CEILING`] at [`MIGRATION_CLI_HOLD_TICK`].
    pub(crate) fn for_migration() -> Self {
        Self::new(MIGRATION_CLI_HOLD_CEILING, MIGRATION_CLI_HOLD_TICK)
    }

    pub(crate) fn ceiling(&self) -> Duration {
        self.ceiling
    }

    /// Whether this migration has had to wait (its `on_hold` fired).
    pub(crate) fn announced(&self) -> bool {
        self.announced
    }

    /// How long this migration has held so far.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Duration {
        self.held
    }
}

/// Wait until `probe` finds a launchable `claude`, the hold's ceiling is spent,
/// or `abort` answers true (checked before every look).
///
/// `first_miss` re-enters the hold as though a look had just missed (the
/// spawn-error path, [`launch_with_hold`]), so the wait is not skipped by a
/// look that flips back at once. `on_hold` fires once per [`CliHold`] — the
/// first time it has to wait — with what that look saw. A hit on the first
/// look costs no wait and no `on_hold`.
pub(crate) async fn await_launchable_claude<P, H, A>(
    hold: &mut CliHold,
    probe: &mut P,
    first_miss: Option<LastSeen>,
    on_hold: &mut H,
    abort: &mut A,
) -> Result<String, HoldEnd>
where
    P: CliProbe,
    H: FnMut(LastSeen) + Send,
    A: FnMut() -> bool + Send,
{
    let episode = tokio::time::Instant::now();
    let mut miss = first_miss;
    loop {
        if abort() {
            hold.held += episode.elapsed();
            return Err(HoldEnd::Aborted);
        }
        let seen = match miss.take() {
            Some(seen) => seen,
            None => match probe.look().await {
                Look::Launchable(path) => {
                    hold.held += episode.elapsed();
                    return Ok(path);
                }
                Look::Unlaunchable(seen) => seen,
            },
        };
        let waited = hold.held + episode.elapsed();
        if waited >= hold.ceiling {
            hold.held = waited;
            return Err(HoldEnd::CliUnavailable(CliUnavailable {
                waited,
                last_seen: seen,
            }));
        }
        if !hold.announced {
            hold.announced = true;
            on_hold(seen);
        }
        tokio::time::sleep(hold.tick).await;
    }
}

/// How many spawns one [`launch_with_hold`] call (one `spawn_next`, i.e. one
/// respawn attempt) may try. A resolver flapping between launchable and not
/// while every spawn errors would otherwise spawn once per tick until the
/// ceiling; after this many failed spawns the last spawn error is returned.
/// The count is per call, not per migration: the [`CliHold`] budget spans
/// every attempt of a migration, this cap does not, so a migration's retries
/// can each try this many.
pub(crate) const MAX_SPAWNS_PER_ATTEMPT: usize = 3;

/// Hold for a launchable `claude`, then `spawn` with it. A spawn ERROR is
/// followed by one more look: still launchable, and the error is the spawn's
/// own ([`SpawnNextError::Spawn`]); no longer launchable (the CLI went away
/// between the look and the exec), and the hold is re-entered under the same
/// ceiling instead of ending the attempt — at most [`MAX_SPAWNS_PER_ATTEMPT`]
/// spawns per call. The spawn that reaches that cap is not followed by a look:
/// its error is returned as the spawn's own, judged on the look before it.
/// The time a failed spawn and its look take is charged to the hold's budget
/// (it is time the session spent waiting for a launchable CLI); a successful
/// spawn's time is not — the hold is over.
pub(crate) async fn launch_with_hold<P, H, A, S, SF>(
    hold: &mut CliHold,
    probe: &mut P,
    on_hold: &mut H,
    abort: &mut A,
    mut spawn: S,
) -> Result<SpawnedPane, SpawnNextError>
where
    P: CliProbe,
    H: FnMut(LastSeen) + Send,
    A: FnMut() -> bool + Send,
    S: FnMut(String) -> SF + Send,
    SF: Future<Output = Result<SpawnedPane, String>> + Send,
{
    let mut first_miss = None;
    let mut spawns = 0;
    loop {
        let bin =
            match await_launchable_claude(hold, probe, first_miss.take(), on_hold, abort).await {
                Ok(bin) => bin,
                Err(HoldEnd::CliUnavailable(c)) => return Err(SpawnNextError::CliUnavailable(c)),
                Err(HoldEnd::Aborted) => return Err(SpawnNextError::HoldAborted),
            };
        let started = tokio::time::Instant::now();
        spawns += 1;
        let error = match spawn(bin).await {
            Ok(pane) => return Ok(pane),
            Err(e) => e,
        };
        if spawns >= MAX_SPAWNS_PER_ATTEMPT {
            hold.held += started.elapsed();
            warn!(
                error = %error,
                spawns,
                "migration: the respawn could not spawn {spawns} times in one hold — giving up on this attempt"
            );
            return Err(SpawnNextError::Spawn(error));
        }
        let look = probe.look().await;
        hold.held += started.elapsed();
        match look {
            Look::Launchable(_) => return Err(SpawnNextError::Spawn(error)),
            Look::Unlaunchable(seen) => {
                warn!(
                    error = %error,
                    last_seen = %seen,
                    "migration: the respawn could not spawn and the Claude CLI is no longer launchable — holding again"
                );
                first_miss = Some(seen);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_failing_resolver_reads_absent_and_warns_only_the_first_time() {
        let mut probe = super::ResolverProbe::default();
        assert!(!probe.warned);
        assert_eq!(
            probe.on_resolver_failure(&"task panicked"),
            super::Look::Unlaunchable(super::LastSeen::Absent)
        );
        assert!(probe.warned, "the first failure must mark the probe warned");
        assert_eq!(
            probe.on_resolver_failure(&"task panicked"),
            super::Look::Unlaunchable(super::LastSeen::Absent)
        );
        assert!(probe.warned);
    }

    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    const TICK: Duration = Duration::from_secs(1);
    const CEILING: Duration = Duration::from_secs(30);

    /// Answers a scripted look per call; past the script, the last one.
    struct Script {
        looks: VecDeque<Look>,
        last: Look,
        calls: usize,
    }

    impl Script {
        fn new(looks: Vec<Look>) -> Self {
            let last = looks
                .last()
                .cloned()
                .unwrap_or(Look::Unlaunchable(LastSeen::Absent));
            Self {
                looks: looks.into(),
                last,
                calls: 0,
            }
        }
    }

    impl CliProbe for Script {
        async fn look(&mut self) -> Look {
            self.calls += 1;
            self.looks.pop_front().unwrap_or_else(|| self.last.clone())
        }
    }

    fn hit() -> Look {
        Look::Launchable("/opt/claude".into())
    }

    fn miss(seen: LastSeen) -> Look {
        Look::Unlaunchable(seen)
    }

    /// Runs the hold with recording callbacks: (result, on_hold calls).
    async fn hold_with(
        hold: &mut CliHold,
        probe: &mut impl CliProbe,
        abort_after_checks: Option<usize>,
    ) -> (Result<String, HoldEnd>, Vec<LastSeen>) {
        let mut announced = Vec::new();
        let mut checks = 0;
        let mut on_hold = |seen| announced.push(seen);
        let mut abort = || {
            checks += 1;
            abort_after_checks.is_some_and(|n| checks > n)
        };
        let out = await_launchable_claude(hold, probe, None, &mut on_hold, &mut abort).await;
        (out, announced)
    }

    #[tokio::test(start_paused = true)]
    async fn a_launchable_cli_at_once_costs_no_wait_and_no_hold() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![hit()]);
        let start = tokio::time::Instant::now();
        let (out, announced) = hold_with(&mut hold, &mut probe, None).await;
        assert_eq!(out, Ok("/opt/claude".to_string()));
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(hold.held(), Duration::ZERO);
        assert!(announced.is_empty(), "on_hold must not fire: {announced:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn two_misses_then_a_hit_wait_two_ticks_and_announce_once() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![miss(LastSeen::Absent), miss(LastSeen::Absent), hit()]);
        let (out, announced) = hold_with(&mut hold, &mut probe, None).await;
        assert_eq!(out, Ok("/opt/claude".to_string()));
        assert_eq!(hold.held(), Duration::from_secs(2));
        assert_eq!(
            announced,
            vec![LastSeen::Absent],
            "on_hold fires exactly once"
        );
        assert_eq!(probe.calls, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stub_then_the_real_binary_is_waited_out() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![miss(LastSeen::Stub), hit()]);
        let (out, announced) = hold_with(&mut hold, &mut probe, None).await;
        assert_eq!(out, Ok("/opt/claude".to_string()));
        assert_eq!(announced, vec![LastSeen::Stub]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_ceiling_ends_the_hold_with_a_typed_error() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![miss(LastSeen::Changing), miss(LastSeen::Absent)]);
        let (out, announced) =
            tokio::time::timeout(CEILING * 4, hold_with(&mut hold, &mut probe, None))
                .await
                .expect("the ceiling must end the hold");
        assert_eq!(
            out,
            Err(HoldEnd::CliUnavailable(CliUnavailable {
                waited: CEILING,
                last_seen: LastSeen::Absent,
            }))
        );
        assert_eq!(
            announced,
            vec![LastSeen::Changing],
            "once, with the first miss"
        );
        assert_eq!(
            CliUnavailable {
                waited: CEILING,
                last_seen: LastSeen::Absent
            }
            .to_string(),
            "claude_cli_unavailable:absent after 30s"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn on_hold_fires_once_per_migration_across_spawns() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut first = Script::new(vec![miss(LastSeen::Absent), hit()]);
        let (_, a) = hold_with(&mut hold, &mut first, None).await;
        let mut second = Script::new(vec![miss(LastSeen::Stub), hit()]);
        let (_, b) = hold_with(&mut hold, &mut second, None).await;
        assert_eq!(a, vec![LastSeen::Absent]);
        assert!(
            b.is_empty(),
            "a second hold in the same migration is not re-announced"
        );
        assert_eq!(
            hold.held(),
            Duration::from_secs(2),
            "both waits spend one budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_old_pane_closing_mid_hold_ends_it_without_a_path() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![miss(LastSeen::Absent)]);
        let (out, _) = hold_with(&mut hold, &mut probe, Some(3)).await;
        assert_eq!(out, Err(HoldEnd::Aborted));
        assert_eq!(probe.calls, 3, "abort is checked before every look");
    }

    #[tokio::test(start_paused = true)]
    async fn the_install_wait_returns_its_output_or_ends_on_abort() {
        let mut never = || false;
        let out = await_unless_aborted(
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                7
            },
            &mut never,
            TICK,
        )
        .await;
        assert_eq!(out, Some(7));
        let mut checks = 0;
        let mut after_two = || {
            checks += 1;
            checks > 2
        };
        let start = tokio::time::Instant::now();
        let out = await_unless_aborted(std::future::pending::<()>(), &mut after_two, TICK).await;
        assert_eq!(out, None, "an install wait is cut short by the abort");
        assert_eq!(start.elapsed(), TICK * 2, "seen on the next tick");
    }

    // ---- launch_with_hold ---------------------------------------------------

    type Spawns = Arc<Mutex<Vec<String>>>;

    /// A hold that reaches its ceiling must END: bounded at four ceilings of
    /// (paused) time, so a hold that never stops fails instead of hanging.
    async fn bounded<F: std::future::Future>(f: F) -> F::Output {
        tokio::time::timeout(CEILING * 4, f)
            .await
            .expect("the hold must end at its ceiling")
    }

    /// A spawn that fails `fail_first` times, recording each binary it got.
    fn spawner(
        spawns: &Spawns,
        fail_first: usize,
    ) -> impl FnMut(String) -> std::future::Ready<Result<SpawnedPane, String>> + Send {
        let spawns = Arc::clone(spawns);
        move |bin| {
            let mut s = spawns.lock().unwrap();
            s.push(bin);
            std::future::ready(if s.len() <= fail_first {
                Err("exec failed".to_string())
            } else {
                Ok(SpawnedPane::bare(format!("new-{}", s.len())))
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_spawn_error_with_the_cli_gone_re_enters_the_hold() {
        let mut hold = CliHold::new(CEILING, TICK);
        // hit → spawn fails → re-look misses → hold one tick → hit → spawn ok.
        let mut probe = Script::new(vec![hit(), miss(LastSeen::Changing), hit()]);
        let spawns = Spawns::default();
        let mut announced = Vec::new();
        let out = launch_with_hold(
            &mut hold,
            &mut probe,
            &mut |s| announced.push(s),
            &mut || false,
            spawner(&spawns, 1),
        )
        .await;
        assert_eq!(out, Ok(SpawnedPane::bare("new-2")));
        assert_eq!(spawns.lock().unwrap().len(), 2);
        assert_eq!(announced, vec![LastSeen::Changing]);
        assert_eq!(hold.held(), TICK, "the re-entered hold waited one tick");
    }

    /// Review r5 A6: a resolver flapping between launchable and not while
    /// every spawn fails stops after MAX_SPAWNS_PER_ATTEMPT spawns, and the time
    /// the failed spawns took is charged to the hold.
    #[tokio::test(start_paused = true)]
    async fn a_flapping_cli_with_failing_spawns_stops_at_the_spawn_cap() {
        let mut hold = CliHold::new(CEILING, TICK);
        let looks: Vec<Look> = (0..20)
            .flat_map(|_| [hit(), miss(LastSeen::Changing)])
            .collect();
        let mut probe = Script::new(looks);
        let spawns = Spawns::default();
        let counted = Arc::clone(&spawns);
        let out = launch_with_hold(
            &mut hold,
            &mut probe,
            &mut |_| {},
            &mut || false,
            move |bin| {
                counted.lock().unwrap().push(bin);
                async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Err::<SpawnedPane, String>("exec failed".into())
                }
            },
        )
        .await;
        assert_eq!(out, Err(SpawnNextError::Spawn("exec failed".into())));
        assert_eq!(spawns.lock().unwrap().len(), MAX_SPAWNS_PER_ATTEMPT);
        // Review r6 A1: no look follows the spawn that reaches the cap — a hit
        // before each of the 3 spawns, a miss after the first 2, nothing after
        // the third.
        assert_eq!(probe.calls, 5);
        // 3 spawns x 2 s, plus the 1 s tick before each of the 2 re-entries.
        assert_eq!(hold.held(), Duration::from_secs(8));
    }

    #[tokio::test(start_paused = true)]
    async fn a_spawn_error_with_the_cli_still_launchable_is_the_spawns_own() {
        let mut hold = CliHold::new(CEILING, TICK);
        let mut probe = Script::new(vec![hit()]);
        let spawns = Spawns::default();
        let out = launch_with_hold(
            &mut hold,
            &mut probe,
            &mut |_| {},
            &mut || false,
            spawner(&spawns, 1),
        )
        .await;
        assert_eq!(out, Err(SpawnNextError::Spawn("exec failed".into())));
        assert_eq!(spawns.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn launch_with_hold_spawns_nothing_past_the_ceiling_or_an_old_pane_close() {
        let mut hold = CliHold::new(CEILING, TICK);
        let spawns = Spawns::default();
        let out = bounded(launch_with_hold(
            &mut hold,
            &mut Script::new(vec![miss(LastSeen::Stub)]),
            &mut |_| {},
            &mut || false,
            spawner(&spawns, 0),
        ))
        .await;
        assert_eq!(
            out,
            Err(SpawnNextError::CliUnavailable(CliUnavailable {
                waited: CEILING,
                last_seen: LastSeen::Stub
            }))
        );
        let mut hold = CliHold::new(CEILING, TICK);
        let out = launch_with_hold(
            &mut hold,
            &mut Script::new(vec![miss(LastSeen::Absent)]),
            &mut |_| {},
            &mut || true,
            spawner(&spawns, 0),
        )
        .await;
        assert_eq!(out, Err(SpawnNextError::HoldAborted));
        assert!(spawns.lock().unwrap().is_empty(), "no pane was spawned");
    }

    // ---- the real judgement over real files ---------------------------------

    #[cfg(unix)]
    fn write_file(dir: &Path, name: &str, body: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[cfg(unix)]
    fn settled_now() -> SystemTime {
        SystemTime::now() + Duration::from_secs(10)
    }

    #[cfg(unix)]
    const STUB_BODY: &str = "echo \"Error: claude native binary not installed.\" >&2\nexit 1\n";

    #[cfg(unix)]
    #[test]
    fn judge_resolved_reads_every_unlaunchable_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let real = write_file(tmp.path(), "claude", "\x7fELF....", 0o755);
        let stub = write_file(tmp.path(), "stub", STUB_BODY, 0o755);
        let plain = write_file(tmp.path(), "plain", "\x7fELF....", 0o644);
        assert_eq!(judge_resolved(&real, settled_now()), Ok(()));
        assert_eq!(
            judge_resolved(&real, SystemTime::now() - Duration::from_secs(1)),
            Err(LastSeen::Changing),
            "a target whose change time is not 2 s old is still being written"
        );
        assert_eq!(judge_resolved(&stub, settled_now()), Err(LastSeen::Stub));
        assert_eq!(
            judge_resolved(&plain, settled_now()),
            Err(LastSeen::NotExecutable)
        );
        assert_eq!(
            judge_resolved(&tmp.path().join("gone"), settled_now()),
            Err(LastSeen::Absent)
        );
        assert_eq!(
            judge_resolved(tmp.path(), settled_now()),
            Err(LastSeen::Absent)
        );
    }

    /// The resolver returns an absolute `QONTINUI_CLAUDE_BIN` override
    /// UNJUDGED (`try_resolve_launchable_claude_bin`), so what it hands
    /// [`look_at`] can be a stub or a missing file: both hold, neither spawns.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn an_absolute_override_naming_a_stub_or_a_missing_file_holds_rather_than_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let stub = write_file(tmp.path(), "claude", STUB_BODY, 0o755);
        let missing = tmp.path().join("no-such-claude");
        for (path, seen) in [(stub, LastSeen::Stub), (missing, LastSeen::Absent)] {
            struct Override(String);
            impl CliProbe for Override {
                async fn look(&mut self) -> Look {
                    look_at(Some(self.0.clone()), settled_now())
                }
            }
            let mut hold = CliHold::new(CEILING, TICK);
            let spawns = Spawns::default();
            let out = bounded(launch_with_hold(
                &mut hold,
                &mut Override(path.to_string_lossy().into_owned()),
                &mut |_| {},
                &mut || false,
                spawner(&spawns, 0),
            ))
            .await;
            assert_eq!(
                out,
                Err(SpawnNextError::CliUnavailable(CliUnavailable {
                    waited: CEILING,
                    last_seen: seen
                }))
            );
            assert!(
                spawns.lock().unwrap().is_empty(),
                "{seen:?}: nothing spawned"
            );
        }
    }

    /// Changing, then quiescent — through the REAL predicate: a probe looking
    /// at one freshly written binary with a clock that starts at its write and
    /// advances one tick per look.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_binary_still_being_written_is_held_until_it_settles() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_file(tmp.path(), "claude", "\x7fELF....", 0o755);
        struct Clocked {
            path: std::path::PathBuf,
            now: SystemTime,
            looks: Vec<Look>,
        }
        impl CliProbe for Clocked {
            async fn look(&mut self) -> Look {
                let l = look_at(Some(self.path.to_string_lossy().into_owned()), self.now);
                self.now += TICK;
                self.looks.push(l.clone());
                l
            }
        }
        let mut probe = Clocked {
            path: bin.clone(),
            now: SystemTime::now(),
            looks: Vec::new(),
        };
        let mut hold = CliHold::new(CEILING, TICK);
        let (out, announced) = hold_with(&mut hold, &mut probe, None).await;
        assert_eq!(out, Ok(bin.to_string_lossy().into_owned()));
        assert_eq!(announced, vec![LastSeen::Changing]);
        assert!(
            probe.looks.len() >= 3,
            "at least two Changing looks before the 2 s settle: {:?}",
            probe.looks
        );
        assert!(probe.looks[..probe.looks.len() - 1]
            .iter()
            .all(|l| *l == Look::Unlaunchable(LastSeen::Changing)));
    }
}
