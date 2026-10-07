//! The spawn-failure classifier: which OS resource, if any, a failed spawn
//! says this machine (or this process) has run out of — plus the
//! descriptor-exhaustion stamp that was this module's first arm.
//!
//! # Why one classifier
//!
//! Plan `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-
//! git-spawns-are-ungated`, Phase 0. The MSI operator box aborted four times in
//! three days with `os error 1455` (`ERROR_COMMITMENT_LIMIT`) in the log dozens
//! of times per rotation — and to every consumer that error was
//! indistinguishable from *"git is not on PATH"*, because
//! `process_helpers::run_probe_inner` interpolated the `io::Error` into a WARN
//! and returned a unit `DegradeReason::SpawnError`. The only `raw_os_error`
//! classifier on that spawn path was this module's descriptor stamp, matching
//! `EMFILE`/`ENFILE` alone. So it was generalised rather than joined by a second
//! classifier (vet 2026-09-30, defect #2): one [`classify_os_code`], one
//! [`SpawnFailure`] verdict the degrade carries, and the fd stamp kept, exactly
//! as it was, as one arm.
//!
//! ## Match the CODE, never the message text
//!
//! The message on the box that motivated this is German (*"Die
//! Auslagerungsdatei ist zu klein …"*). A substring match on `"paging file"`
//! finds nothing there, and one on `"Auslagerungsdatei"` finds nothing on an
//! English box. [`classify_os_code`] therefore reads `raw_os_error()` only.
//!
//! The one place no code is available is a child that ran and whose OWN child
//! launch failed (`git.exe` → `error launching git: <FormatMessage text>`, exit
//! 1). There the only evidence is text, so [`commit_exhaustion_in_stderr`]
//! derives its needles from THIS process's OS at runtime — the message the OS
//! itself renders for each commit-exhaustion code, in this box's language —
//! and the verdict it feeds is explicitly `commit_exhaustion_suspected`, never
//! an assertion.
//!
//! ## One event per episode, not one WARN per failure
//!
//! 29 identical warns in one log rotation is not 29 pieces of information.
//! [`report_exhaustion`] is edge-triggered per (kind, evidence): the first
//! failure of an episode emits ONE structured `resource_exhaustion` record
//! (kind, OS code, the caller's label, and a memory reading taken at that
//! instant); repeats inside the episode only count; the episode's closing
//! record carries the `suppressed_repeats` count. Same discipline as
//! `resource_guard::note_ladder_coercion`.
//!
//! # Why the lib crate
//!
//! `process_helpers` — where spawn-time exhaustion surfaces first — is compiled
//! into both the lib and the runner bin. A static declared in a shared module
//! is two statics, each seeing half the traffic. So this module is declared
//! ONLY by the lib, and every caller in either crate spells it
//! `qontinui_runner_lib::util::resource_exhaustion`, the same arrangement
//! `wedge_diagnostics` uses for its blocking-pool counter. The episode book and
//! the cached memory reading depend on that for the same reason: one episode,
//! one reading, whichever crate's copy of `process_helpers` saw the failure.
//!
//! # The descriptor stamp (plan `2026-09-09-the-pong-receive-path-has-no-
//! liveness-signal-so-fd-exhaustion-still-reads-as-ui-death`)
//!
//! On `merytshost` (2026-09-02) the runner logged 8,267 `Too many open files
//! (os error 24)` and, over the same stretch, ran a webview-recreate ladder to
//! `EXHAUSTED` 240 times — because the `/ui-bridge/pong` that keeps `ui_dead`
//! false arrives on an accepted socket, i.e. needs a descriptor, and nothing
//! told the death verdict so.
//!
//! The stamp is one of TWO independent authorities for "this process is out of
//! descriptors" (`verification-and-evidence`
//! `a-control-must-test-the-property-it-names`); the other is the headroom read
//! in the bin's `util::egress_context::fd_headroom`. The stamp is the one that
//! still works at TOTAL exhaustion, where the headroom census itself cannot
//! open `/proc/self/fd`. Its contract is unchanged: one `raw_os_error()` read
//! and, only on a match, two relaxed atomic stores. It records; it decides
//! nothing. The decision is `ui_error::classify_fd_pressure`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// The descriptor stamp
// ---------------------------------------------------------------------------

/// Wall-clock ms of the last `EMFILE`/`ENFILE` any wired site observed.
/// 0 = none ever. A module-level atomic for the reason `PING_EMIT_FAIL_MS` is
/// one: the heartbeat that reads it holds no runtime handle to ask anything.
static FD_EXHAUSTED_MS: AtomicU64 = AtomicU64::new(0);
/// Monotonic count of descriptor-exhaustion errors observed.
static FD_EXHAUSTED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Whether a raw OS error code means "this process (or the system) is out of
/// file descriptors".
///
/// Platform-specific on purpose: 24/23 are `EMFILE`/`ENFILE` only under POSIX.
/// On Windows an `io::Error`'s raw code is a Win32 / Winsock code, where 24 is
/// `ERROR_BAD_LENGTH` and 23 is `ERROR_CRC` — matching those would stamp
/// starvation on unrelated failures and suppress a recovery on no evidence.
pub fn is_fd_exhaustion_code(code: i32) -> bool {
    #[cfg(unix)]
    {
        code == libc::EMFILE || code == libc::ENFILE
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES, WSAEMFILE.
        code == 4 || code == 10024
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        false
    }
}

/// Stamp a descriptor-exhaustion error, if `e` is one. Returns whether it
/// stamped. Call it wherever an `io::Error` from an `open` / `spawn` / `pipe` /
/// `accept` is in hand; an error with no OS code never stamps.
pub fn note_fd_exhaustion(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(code) if is_fd_exhaustion_code(code) => {
            FD_EXHAUSTED_MS.store(now_ms(), Ordering::Relaxed);
            FD_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

/// `(exhaustion_errors_total, last_exhausted_ms)`. `last_exhausted_ms == 0`
/// means none has ever been observed — UNKNOWN-shaped history, never an age.
pub fn fd_exhaustion_report() -> (u64, u64) {
    (
        FD_EXHAUSTED_COUNT.load(Ordering::Relaxed),
        FD_EXHAUSTED_MS.load(Ordering::Relaxed),
    )
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Which OS resource a failure says is exhausted.
///
/// Four kinds, not three: POSIX `EAGAIN` on a spawn is **its own kind**
/// (`task_limit`), not folded into `no_system_resources`. On Linux a
/// `fork`/`clone` returns `EAGAIN` when the task ceiling is hit —
/// `RLIMIT_NPROC`, `kernel.pid_max`, `kernel.threads-max` or a cgroup
/// `pids.max` — and that ceiling is a different pool with a different remedy
/// from memory: `machine-resources.md` already documents the task/PID ceiling
/// as a separate exhaustion signature beside OOM, and a reader told "system
/// resources" would go looking at memory. Windows `ERROR_NO_SYSTEM_RESOURCES`
/// (1450) is the kernel-pool / handle-quota family, which has no POSIX twin —
/// so the two stay apart rather than sharing a vague bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExhaustionKind {
    /// Out of file descriptors / handles (`EMFILE`, `ENFILE`; Win32 4, 10024).
    Fd,
    /// Out of commit (Windows 1455 `ERROR_COMMITMENT_LIMIT`, 8
    /// `ERROR_NOT_ENOUGH_MEMORY`) or memory (POSIX `ENOMEM`).
    Commit,
    /// Windows 1450 `ERROR_NO_SYSTEM_RESOURCES` — kernel pool / quota.
    NoSystemResources,
    /// POSIX `EAGAIN` from a spawn — the task/PID ceiling.
    TaskLimit,
}

impl ExhaustionKind {
    /// Stable, greppable token for logs and structured fields.
    pub fn as_str(self) -> &'static str {
        match self {
            ExhaustionKind::Fd => "fd",
            ExhaustionKind::Commit => "commit",
            ExhaustionKind::NoSystemResources => "no_system_resources",
            ExhaustionKind::TaskLimit => "task_limit",
        }
    }

    const ALL: [ExhaustionKind; 4] = [
        ExhaustionKind::Fd,
        ExhaustionKind::Commit,
        ExhaustionKind::NoSystemResources,
        ExhaustionKind::TaskLimit,
    ];

    fn index(self) -> usize {
        match self {
            ExhaustionKind::Fd => 0,
            ExhaustionKind::Commit => 1,
            ExhaustionKind::NoSystemResources => 2,
            ExhaustionKind::TaskLimit => 3,
        }
    }
}

/// Map a raw OS error code onto the resource it says is exhausted. `None` for
/// every code that is not an exhaustion code — which is the overwhelmingly
/// common case (`ENOENT`: the binary is not installed; `EACCES`; …).
///
/// Codes are per-platform for the reason [`is_fd_exhaustion_code`] states: a
/// Windows `io::Error` carries Win32 codes, where POSIX numbers mean something
/// else entirely (Win32 12 is `ERROR_INVALID_ACCESS`, not `ENOMEM`).
pub fn classify_os_code(code: i32) -> Option<ExhaustionKind> {
    if is_fd_exhaustion_code(code) {
        return Some(ExhaustionKind::Fd);
    }
    #[cfg(windows)]
    {
        match code {
            // ERROR_COMMITMENT_LIMIT — "The paging file is too small for this
            // operation to complete." The abort signature on the MSI box.
            1455 => Some(ExhaustionKind::Commit),
            // ERROR_NOT_ENOUGH_MEMORY — what CreateProcess returns when the
            // new process's initial commit cannot be charged.
            8 => Some(ExhaustionKind::Commit),
            // ERROR_NO_SYSTEM_RESOURCES.
            1450 => Some(ExhaustionKind::NoSystemResources),
            _ => None,
        }
    }
    #[cfg(unix)]
    {
        if code == libc::ENOMEM {
            Some(ExhaustionKind::Commit)
        } else if code == libc::EAGAIN {
            Some(ExhaustionKind::TaskLimit)
        } else {
            None
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// The classified reason a spawn failed — what `DegradeReason::SpawnError`
/// carries, so a caller can tell `ERROR_COMMITMENT_LIMIT` from "git is not on
/// PATH" instead of reading both as one unit variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnFailure {
    /// The `io::Error`'s raw OS code, when it had one.
    pub os_code: Option<i32>,
    /// The resource the code says is exhausted; `None` for an ordinary
    /// failure (missing binary, permission, …).
    pub exhaustion: Option<ExhaustionKind>,
}

impl SpawnFailure {
    /// A spawn that was never attempted (a refused, non-allowlisted command):
    /// no OS code, no exhaustion. Spelled as a constant so such a site says
    /// what it means rather than constructing an empty verdict by hand.
    pub const NOT_ATTEMPTED: SpawnFailure = SpawnFailure {
        os_code: None,
        exhaustion: None,
    };

    /// Classify an `io::Error` on its raw OS code alone. Pure — no stamp, no
    /// event; [`report_exhaustion`] is the side-effecting half.
    pub fn classify(e: &std::io::Error) -> Self {
        let os_code = e.raw_os_error();
        SpawnFailure {
            os_code,
            exhaustion: os_code.and_then(classify_os_code),
        }
    }
}

// ---------------------------------------------------------------------------
// The second shape: a child whose OWN launch failed
// ---------------------------------------------------------------------------

/// The OS codes that mean commit exhaustion on this platform — the codes whose
/// rendered text [`commit_exhaustion_in_stderr`] looks for.
#[cfg(windows)]
const COMMIT_CODES: &[i32] = &[1455, 8];
#[cfg(unix)]
const COMMIT_CODES: &[i32] = &[libc::ENOMEM];
#[cfg(not(any(unix, windows)))]
const COMMIT_CODES: &[i32] = &[];

/// English renderings, searched IN ADDITION to the OS-derived ones. A child can
/// render in a different UI language from this process (a service account, an
/// `LANG=C` git), and English is the most common such mismatch.
#[cfg(windows)]
const ENGLISH_COMMIT_MESSAGES: &[(i32, &str)] = &[
    (
        1455,
        "The paging file is too small for this operation to complete",
    ),
    (
        8,
        "Not enough memory resources are available to process this command",
    ),
];
#[cfg(not(windows))]
const ENGLISH_COMMIT_MESSAGES: &[(i32, &str)] = &[(12, "Cannot allocate memory")];

/// Shortest ASCII run accepted as a needle. Below this the run is too generic
/// to be evidence (`"a"`, `"Windows"`), so the whole message is used instead.
const MIN_ASCII_NEEDLE: usize = 16;

/// One piece of text that, found in a child's stderr, suggests the given code.
struct StderrNeedle {
    code: i32,
    text: String,
    /// `true` for the `os error N` form: the match must not continue into
    /// another digit, or `os error 8` would match `os error 87`.
    digit_bounded: bool,
}

/// Build the stderr needles now. Called at startup so the first classification
/// — which happens under memory pressure by definition — does not also pay for
/// rendering and allocating them.
pub fn prewarm_stderr_needles() {
    let _ = stderr_needles();
}

/// The needles, built once. Built from THIS process's OS: on Windows
/// `io::Error::to_string()` renders through `FormatMessageW` in the user's
/// default language — the same call Git for Windows' launcher makes to print
/// `error launching git: …` — so the German box gets German needles and the
/// English box English ones, with no language table to keep current.
fn stderr_needles() -> &'static [StderrNeedle] {
    static NEEDLES: OnceLock<Vec<StderrNeedle>> = OnceLock::new();
    NEEDLES.get_or_init(|| {
        let mut out = Vec::new();
        for &code in COMMIT_CODES {
            // A Rust child reports its own spawn failure as
            // `… (os error 1455)` — locale-independent, and exact.
            out.push(StderrNeedle {
                code,
                text: format!("os error {code}"),
                digit_bounded: true,
            });
            let rendered = std::io::Error::from_raw_os_error(code).to_string();
            if let Some(needle) = message_needle(&rendered) {
                out.push(StderrNeedle {
                    code,
                    text: needle,
                    digit_bounded: false,
                });
            }
        }
        for &(code, msg) in ENGLISH_COMMIT_MESSAGES {
            if COMMIT_CODES.contains(&code) && !out.iter().any(|n| n.text == msg) {
                out.push(StderrNeedle {
                    code,
                    text: msg.to_string(),
                    digit_bounded: false,
                });
            }
        }
        out
    })
}

/// Reduce an OS-rendered error message to the needle that survives a child's
/// output encoding.
///
/// Two transformations, each for a measured reason:
///
/// 1. Strip Rust's ` (os error N)` suffix and the trailing period — the child
///    prints the OS text, not Rust's decoration, and may end the line
///    differently.
/// 2. Prefer the **longest ASCII run** of the message. Git for Windows' launcher
///    writes through the console code page, not UTF-8, so every non-ASCII
///    character (`ü` in *"durchzuführen"*) may reach us as a different byte
///    sequence. *"Die Auslagerungsdatei ist zu klein, um diesen Vorgang
///    durchzuf"* is 62 ASCII characters and identical in every encoding. Where
///    the language has no long ASCII run (Japanese, Russian) the whole message
///    is the needle and matches only a UTF-8 child — which is the best a text
///    match can do, and why the verdict is only "suspected".
fn message_needle(rendered: &str) -> Option<String> {
    let base = rendered
        .rsplit_once(" (os error ")
        .map_or(rendered, |(detail, _)| detail);
    let base = base.trim().trim_end_matches('.').trim_end();
    // A platform with no text for the code renders "Unknown error N" — that
    // is not evidence of anything.
    if base.is_empty() || base.starts_with("Unknown error") {
        return None;
    }
    let longest_ascii = base
        .split(|c: char| !c.is_ascii())
        .map(str::trim)
        .max_by_key(|run| run.len())
        .unwrap_or("");
    if longest_ascii.len() >= MIN_ASCII_NEEDLE {
        Some(longest_ascii.to_string())
    } else {
        Some(base.to_string())
    }
}

/// Whether `haystack` contains `needle` at a position not followed by another
/// ASCII digit.
fn contains_digit_bounded(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(i, m)| {
        !haystack.as_bytes()[i + m.len()..]
            .first()
            .is_some_and(u8::is_ascii_digit)
    })
}

/// If a failed child's stderr carries the text of a commit-exhaustion error,
/// the code it suggests. The second failure shape of plan
/// `2026-09-23-…-ungated` Phase 0: `git.exe` started, *its* child launch
/// failed with `ERROR_COMMITMENT_LIMIT`, and the failure reached us as an
/// ordinary `exit 1` whose only evidence is text. A caller that checks the
/// exit status alone reads commit exhaustion as "git said no".
///
/// Text, so SUSPECTED: a `Some` here must be reported as
/// `commit_exhaustion_suspected`, never as the OS-evidenced `commit` kind.
pub fn commit_exhaustion_in_stderr(stderr: &[u8]) -> Option<i32> {
    if stderr.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(stderr);
    stderr_needles().iter().find_map(|n| {
        let hit = if n.digit_bounded {
            contains_digit_bounded(&text, &n.text)
        } else {
            text.contains(n.text.as_str())
        };
        hit.then_some(n.code)
    })
}

// ---------------------------------------------------------------------------
// The last memory reading
// ---------------------------------------------------------------------------

/// One memory reading, in bytes. `commit_limit` is `ullTotalPageFile` on
/// Windows; off Windows it is whatever the bin's `memory_status` reports
/// (`MemTotal` today — Phase 1 of the plan is what gives it an honest
/// `CommitLimit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReading {
    pub free_commit: u64,
    pub commit_limit: u64,
    pub free_phys: u64,
}

// The last reading any sampler took, as plain atomics — because the
// allocation-failure breadcrumb (`crate::alloc_breadcrumb`) reads it from
// inside a failing allocator, where a lock or an allocation is forbidden. The
// three values are stored independently, so a reader racing a writer can see
// a mix of two adjacent readings; for a breadcrumb that says "roughly where
// the machine stood", that is the right trade against a lock.
static LAST_FREE_COMMIT: AtomicU64 = AtomicU64::new(0);
static LAST_COMMIT_LIMIT: AtomicU64 = AtomicU64::new(0);
static LAST_FREE_PHYS: AtomicU64 = AtomicU64::new(0);
/// Unix seconds of the last reading; 0 = none ever taken (UNKNOWN, not zero).
static LAST_READING_AT_SECS: AtomicU64 = AtomicU64::new(0);

/// Record a memory reading. Called by the bin's `fleet::resource_sample`
/// every time it reads the OS — the spawn gate and the 30 s fleet publish —
/// so the cache is as fresh as the runner's own sampling, at zero extra
/// syscalls.
pub fn note_memory_reading(r: MemoryReading) {
    LAST_FREE_COMMIT.store(r.free_commit, Ordering::Relaxed);
    LAST_COMMIT_LIMIT.store(r.commit_limit, Ordering::Relaxed);
    LAST_FREE_PHYS.store(r.free_phys, Ordering::Relaxed);
    LAST_READING_AT_SECS.store(now_ms() / 1000, Ordering::Relaxed);
}

/// The last recorded reading and the unix second it was taken at, or `None`
/// when nothing has ever been recorded. Lock-free and allocation-free: safe
/// from inside a failing allocator.
pub fn last_memory_reading() -> Option<(MemoryReading, u64)> {
    let at = LAST_READING_AT_SECS.load(Ordering::Relaxed);
    (at != 0).then(|| {
        (
            MemoryReading {
                free_commit: LAST_FREE_COMMIT.load(Ordering::Relaxed),
                commit_limit: LAST_COMMIT_LIMIT.load(Ordering::Relaxed),
                free_phys: LAST_FREE_PHYS.load(Ordering::Relaxed),
            },
            at,
        )
    })
}

/// The bin's live memory probe, registered at startup. A lib module cannot
/// name `fleet::resource_sample` (bin-only), and the edge event wants the
/// reading AT the failure rather than one up to a publish interval old — the
/// recurrence this exists for accelerated from 26 h to 5.4 h, and a stale
/// "31 GiB free commit" beside an `ERROR_COMMITMENT_LIMIT` would be read as a
/// contradiction rather than a lag.
static MEMORY_READER: OnceLock<fn() -> Option<MemoryReading>> = OnceLock::new();

/// Register the live memory probe. First registration wins.
pub fn register_memory_reader(reader: fn() -> Option<MemoryReading>) {
    let _ = MEMORY_READER.set(reader);
}

/// A reading for an edge event: live when a probe is registered and answers,
/// else the cache. Returns the reading and its age in seconds (0 = live).
fn reading_for_event() -> Option<(MemoryReading, u64)> {
    if let Some(r) = MEMORY_READER.get().and_then(|read| read()) {
        return Some((r, 0));
    }
    last_memory_reading().map(|(r, at)| (r, (now_ms() / 1000).saturating_sub(at)))
}

// ---------------------------------------------------------------------------
// Episodes — one structured event per episode
// ---------------------------------------------------------------------------

/// What the classification rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A raw OS error code — an assertion.
    OsCode,
    /// Text in a failed child's stderr — a suspicion
    /// (`commit_exhaustion_suspected`).
    ChildStderr,
}

impl Evidence {
    fn index(self) -> usize {
        match self {
            Evidence::OsCode => 0,
            Evidence::ChildStderr => 1,
        }
    }

    const ALL: [Evidence; 2] = [Evidence::OsCode, Evidence::ChildStderr];
}

/// The `kind` field of the structured record: the kind token, with the
/// text-evidenced commit case spelled `commit_exhaustion_suspected` so no
/// reader mistakes a suspicion for an OS-reported code.
pub fn event_kind_token(kind: ExhaustionKind, evidence: Evidence) -> &'static str {
    match (kind, evidence) {
        (k, Evidence::OsCode) => k.as_str(),
        (ExhaustionKind::Commit, Evidence::ChildStderr) => "commit_exhaustion_suspected",
        (ExhaustionKind::Fd, Evidence::ChildStderr) => "fd_suspected",
        (ExhaustionKind::NoSystemResources, Evidence::ChildStderr) => {
            "no_system_resources_suspected"
        }
        (ExhaustionKind::TaskLimit, Evidence::ChildStderr) => "task_limit_suspected",
    }
}

/// How long a (kind, evidence) slot must go without a failure before its
/// episode may close.
///
/// An episode closes at the first SUCCESSFUL spawn after this quiet interval,
/// or is re-opened by the first failure after it. Ending on the first success
/// alone was rejected: under commit pressure spawns flap — census fails, the
/// next `git_trunk` squeezes through — and a record per flip is the WARN flood
/// again, arriving through the mechanism meant to stop it. 300 s is the period
/// of the slowest frequent spawner (the auto-fresh engine), so an episode
/// spans one pressure event while the next occurrence (the tightest measured
/// interval was 5.4 h) gets a record of its own.
pub const EPISODE_QUIET_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    open: bool,
    opened_ms: u64,
    last_failure_ms: u64,
    suppressed: u64,
}

/// An episode that closed, for its closing record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedEpisode {
    /// Failures inside the episode that emitted nothing.
    pub suppressed_repeats: u64,
    /// First failure to last failure.
    pub duration_ms: u64,
}

/// What one failure did to its episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureTransition {
    /// First failure: emit the one structured record.
    Opened,
    /// Inside an open episode: count, emit nothing.
    Repeat,
    /// First failure after a quiet interval with no intervening success: the
    /// old episode closes (its record is owed) and a new one opens.
    Reopened(ClosedEpisode),
}

/// The per-(kind, evidence) edge-trigger state. PURE over an injected clock,
/// so the discipline is settleable in a test; [`report_exhaustion`] and
/// [`note_spawn_succeeded`] are the impure wrappers over one global book.
#[derive(Debug, Default)]
pub struct EpisodeBook {
    slots: [[Slot; 2]; 4],
}

impl EpisodeBook {
    pub const fn new() -> Self {
        const EMPTY: Slot = Slot {
            open: false,
            opened_ms: 0,
            last_failure_ms: 0,
            suppressed: 0,
        };
        EpisodeBook {
            slots: [[EMPTY; 2]; 4],
        }
    }

    fn slot(&mut self, kind: ExhaustionKind, evidence: Evidence) -> &mut Slot {
        &mut self.slots[kind.index()][evidence.index()]
    }

    /// Record one failure at `now_ms`.
    pub fn on_failure(
        &mut self,
        kind: ExhaustionKind,
        evidence: Evidence,
        now_ms: u64,
    ) -> FailureTransition {
        let slot = self.slot(kind, evidence);
        if !slot.open {
            *slot = Slot {
                open: true,
                opened_ms: now_ms,
                last_failure_ms: now_ms,
                suppressed: 0,
            };
            return FailureTransition::Opened;
        }
        if now_ms.saturating_sub(slot.last_failure_ms) >= EPISODE_QUIET_MS {
            let closed = ClosedEpisode {
                suppressed_repeats: slot.suppressed,
                duration_ms: slot.last_failure_ms.saturating_sub(slot.opened_ms),
            };
            *slot = Slot {
                open: true,
                opened_ms: now_ms,
                last_failure_ms: now_ms,
                suppressed: 0,
            };
            return FailureTransition::Reopened(closed);
        }
        slot.suppressed += 1;
        slot.last_failure_ms = now_ms;
        FailureTransition::Repeat
    }

    /// Record a successful spawn at `now_ms`: every episode quiet for at least
    /// [`EPISODE_QUIET_MS`] closes, and `on_close` hears each one.
    pub fn on_success(
        &mut self,
        now_ms: u64,
        mut on_close: impl FnMut(ExhaustionKind, Evidence, ClosedEpisode),
    ) {
        for kind in ExhaustionKind::ALL {
            for evidence in Evidence::ALL {
                let slot = self.slot(kind, evidence);
                if slot.open && now_ms.saturating_sub(slot.last_failure_ms) >= EPISODE_QUIET_MS {
                    let closed = ClosedEpisode {
                        suppressed_repeats: slot.suppressed,
                        duration_ms: slot.last_failure_ms.saturating_sub(slot.opened_ms),
                    };
                    *slot = Slot::default();
                    on_close(kind, evidence, closed);
                }
            }
        }
    }

    /// Whether any episode is open.
    pub fn any_open(&self) -> bool {
        self.slots.iter().flatten().any(|s| s.open)
    }
}

static BOOK: Mutex<EpisodeBook> = Mutex::new(EpisodeBook::new());
/// Mirror of `BOOK.any_open()`, so a successful spawn — the overwhelmingly
/// common case — costs one relaxed load and never touches the lock.
static ANY_OPEN: AtomicBool = AtomicBool::new(false);

/// Report one classified exhaustion failure from `caller` (the call site's own
/// label, e.g. `worktree_census: git`). Returns the episode transition so a
/// caller — and a test — can see whether a record was emitted.
///
/// On an opening edge this emits ONE `resource_exhaustion` WARN carrying the
/// kind, the OS code, the caller, and a memory reading taken now; and, for
/// every kind except `fd`, appends a line to `wedge-incidents.log` (token from
/// [`incident_token`]) so the next boot's crash harvest can name the episode
/// the process died in. The episode's close appends the matching
/// `<token>_closed` line, so the harvest can tell an episode the process died
/// INSIDE from one that had already ended.
/// `fd` stays out of that file: it has its own stamp and its own consumer
/// (`ui_error::classify_fd_pressure`), and its floods (5,841 in one day) would
/// bury the incidents the file exists for.
///
/// Inside an episode it logs at DEBUG only — the WARN per failure this
/// replaces is exactly the noise the plan measured.
pub fn report_exhaustion(
    kind: ExhaustionKind,
    evidence: Evidence,
    os_code: Option<i32>,
    caller: &str,
) -> FailureTransition {
    let now = now_ms();
    let transition = {
        let mut book = BOOK.lock().unwrap_or_else(|p| p.into_inner());
        let t = book.on_failure(kind, evidence, now);
        ANY_OPEN.store(book.any_open(), Ordering::Relaxed);
        t
    };
    let token = event_kind_token(kind, evidence);
    match transition {
        FailureTransition::Repeat => {
            tracing::debug!(
                kind = token,
                os_code = ?os_code,
                caller,
                "resource_exhaustion: repeat inside an open episode (counted, not re-logged)"
            );
        }
        FailureTransition::Reopened(closed) => {
            emit_closed(kind, evidence, closed, "quiet_interval_then_failure");
            emit_opened(kind, evidence, os_code, caller);
        }
        FailureTransition::Opened => emit_opened(kind, evidence, os_code, caller),
    }
    transition
}

/// Tell the episode book a spawn succeeded. One relaxed load when no episode
/// is open, which is every call on a healthy machine.
pub fn note_spawn_succeeded() {
    if !ANY_OPEN.load(Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    let mut closed = Vec::new();
    {
        let mut book = BOOK.lock().unwrap_or_else(|p| p.into_inner());
        book.on_success(now, |k, e, c| closed.push((k, e, c)));
        ANY_OPEN.store(book.any_open(), Ordering::Relaxed);
    }
    for (kind, evidence, c) in closed {
        emit_closed(kind, evidence, c, "spawn_succeeded_after_quiet_interval");
    }
}

fn emit_opened(kind: ExhaustionKind, evidence: Evidence, os_code: Option<i32>, caller: &str) {
    let token = event_kind_token(kind, evidence);
    let reading = reading_for_event();
    let (free_commit, commit_limit, free_phys, reading_age_s) = match reading {
        Some((r, age)) => (
            Some(r.free_commit),
            Some(r.commit_limit),
            Some(r.free_phys),
            Some(age),
        ),
        None => (None, None, None, None),
    };
    tracing::warn!(
        event = "resource_exhaustion",
        kind = token,
        os_code = ?os_code,
        caller,
        free_commit = ?free_commit,
        commit_limit = ?commit_limit,
        free_phys = ?free_phys,
        reading_age_s = ?reading_age_s,
        suppressed_repeats = 0u64,
        "resource_exhaustion: {token} exhaustion episode OPENED by {caller} (os code {code}) — \
         further failures of this kind are counted, not logged, until the episode closes",
        code = os_code.map_or_else(|| "none".to_string(), |c| c.to_string()),
    );
    let Some(incident_token) = incident_token(kind, evidence) else {
        return;
    };
    let reading_text = match reading {
        Some((r, age)) => format!(
            "free_commit {} bytes, commit_limit {} bytes, free_phys {} bytes (reading {age}s old)",
            r.free_commit, r.commit_limit, r.free_phys
        ),
        None => "no memory reading available".to_string(),
    };
    let detail = format!(
        "kind={token} os_code={} caller={caller:?} — {reading_text}",
        os_code.map_or_else(|| "none".to_string(), |c| c.to_string()),
    );
    crate::alloc_breadcrumb::append_incident(incident_token, &detail);
}

/// The `wedge-incidents.log` token an episode of this (kind, evidence) opens
/// with, or `None` for `fd` (kept out of the file — see [`report_exhaustion`]).
/// Its close is the same token with [`CLOSED_SUFFIX`]. The text-evidenced
/// cases keep "suspected" in the token, so the next-boot harvest can
/// never render a suspicion as an OS-reported fact.
pub fn incident_token(kind: ExhaustionKind, evidence: Evidence) -> Option<&'static str> {
    match (kind, evidence) {
        (ExhaustionKind::Fd, _) => None,
        (ExhaustionKind::Commit, Evidence::OsCode) => Some("commit_exhaustion"),
        (ExhaustionKind::Commit, Evidence::ChildStderr) => Some("commit_exhaustion_suspected"),
        (ExhaustionKind::NoSystemResources | ExhaustionKind::TaskLimit, Evidence::OsCode) => {
            Some("resource_exhaustion")
        }
        (ExhaustionKind::NoSystemResources | ExhaustionKind::TaskLimit, Evidence::ChildStderr) => {
            Some("resource_exhaustion_suspected")
        }
    }
}

/// Appended to an [`incident_token`] on the line that closes its episode.
pub const CLOSED_SUFFIX: &str = "_closed";

fn emit_closed(kind: ExhaustionKind, evidence: Evidence, c: ClosedEpisode, ended_by: &str) {
    tracing::info!(
        event = "resource_exhaustion_closed",
        kind = event_kind_token(kind, evidence),
        suppressed_repeats = c.suppressed_repeats,
        duration_ms = c.duration_ms,
        ended_by,
        "resource_exhaustion: {} episode CLOSED ({ended_by}) after {} suppressed repeat(s) over {}ms",
        event_kind_token(kind, evidence),
        c.suppressed_repeats,
        c.duration_ms,
    );
    // The close path is ordinary code (never the allocator), so allocating
    // here is fine. `kind=` matches the opening line's field, which is what
    // the harvest pairs on.
    if let Some(token) = incident_token(kind, evidence) {
        crate::alloc_breadcrumb::append_incident(
            &format!("{token}{CLOSED_SUFFIX}"),
            &format!(
                "kind={} suppressed_repeats={} duration_ms={} ended_by={ended_by}",
                event_kind_token(kind, evidence),
                c.suppressed_repeats,
                c.duration_ms
            ),
        );
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only EMFILE/ENFILE stamp. Any other OS error, and any error with no OS
    /// code at all, must leave the stamp alone — a stamp on an unrelated
    /// failure would suppress UI recovery on no evidence.
    #[cfg(unix)]
    #[test]
    fn note_fd_exhaustion_only_stamps_on_emfile_or_enfile() {
        use std::io::Error;
        assert!(is_fd_exhaustion_code(libc::EMFILE));
        assert!(is_fd_exhaustion_code(libc::ENFILE));
        for code in [libc::ENOENT, libc::EACCES, libc::EAGAIN, libc::ENOMEM, 0] {
            assert!(!is_fd_exhaustion_code(code), "code {code} must not stamp");
            assert!(!note_fd_exhaustion(&Error::from_raw_os_error(code)));
        }
        assert!(!note_fd_exhaustion(&Error::other(
            "Too many open files (but no OS code)"
        )));

        let (count_before, _) = fd_exhaustion_report();
        assert!(note_fd_exhaustion(&Error::from_raw_os_error(libc::EMFILE)));
        assert!(note_fd_exhaustion(&Error::from_raw_os_error(libc::ENFILE)));
        let (count_after, last_ms) = fd_exhaustion_report();
        // `>=`: the counter is process-wide and monotonic, so ours are counted
        // whatever else in this test binary stamps concurrently.
        assert!(count_after >= count_before + 2);
        assert!(last_ms > 0, "a stamp must record a wall-clock time");
    }

    /// Windows codes 24/23 are NOT EMFILE/ENFILE (they are ERROR_BAD_LENGTH /
    /// ERROR_CRC); only the Win32/Winsock exhaustion codes stamp.
    #[cfg(windows)]
    #[test]
    fn windows_fd_exhaustion_codes_are_win32_not_posix() {
        assert!(is_fd_exhaustion_code(4));
        assert!(is_fd_exhaustion_code(10024));
        assert!(!is_fd_exhaustion_code(24));
        assert!(!is_fd_exhaustion_code(23));
    }

    /// The fd arm of the generalised classifier is the SAME predicate as the
    /// stamp — generalising must not have moved it.
    #[test]
    fn the_fd_arm_classifies_exactly_what_the_stamp_stamps() {
        for code in -1..11_000 {
            assert_eq!(
                classify_os_code(code) == Some(ExhaustionKind::Fd),
                is_fd_exhaustion_code(code),
                "code {code}"
            );
        }
    }

    /// Plan Phase 0 verification (a): 1455 classifies as commit on the CODE,
    /// whatever the message body says. A custom error carrying the German
    /// text and no code must NOT classify — text is never the code path.
    #[cfg(windows)]
    #[test]
    fn error_commitment_limit_classifies_as_commit_on_the_code_alone() {
        let e = std::io::Error::from_raw_os_error(1455);
        assert_eq!(
            SpawnFailure::classify(&e),
            SpawnFailure {
                os_code: Some(1455),
                exhaustion: Some(ExhaustionKind::Commit)
            }
        );
        assert_eq!(classify_os_code(8), Some(ExhaustionKind::Commit));
        assert_eq!(
            classify_os_code(1450),
            Some(ExhaustionKind::NoSystemResources)
        );
        // ERROR_FILE_NOT_FOUND — git not on PATH — is not exhaustion.
        assert_eq!(classify_os_code(2), None);
        let german = std::io::Error::other(
            "Die Auslagerungsdatei ist zu klein, um diesen Vorgang durchzuführen.",
        );
        assert_eq!(SpawnFailure::classify(&german).exhaustion, None);
    }

    /// The POSIX half: ENOMEM is commit, EAGAIN is its own task-limit kind,
    /// ENOENT (binary missing) is nothing. And the message language cannot
    /// matter, because the classifier never reads it.
    #[cfg(unix)]
    #[test]
    fn posix_codes_classify_on_the_code_alone() {
        let e = std::io::Error::from_raw_os_error(libc::ENOMEM);
        assert_eq!(
            SpawnFailure::classify(&e),
            SpawnFailure {
                os_code: Some(libc::ENOMEM),
                exhaustion: Some(ExhaustionKind::Commit)
            }
        );
        assert_eq!(
            classify_os_code(libc::EAGAIN),
            Some(ExhaustionKind::TaskLimit)
        );
        assert_eq!(classify_os_code(libc::ENOENT), None);
        assert_eq!(classify_os_code(libc::EACCES), None);
        // Windows' 1455 is not a POSIX code at all.
        assert_eq!(classify_os_code(1455), None);
        let german = std::io::Error::other(
            "Die Auslagerungsdatei ist zu klein, um diesen Vorgang durchzuführen.",
        );
        assert_eq!(SpawnFailure::classify(&german), SpawnFailure::NOT_ATTEMPTED);
    }

    /// Plan Phase 0 verification (a), the edge trigger: N consecutive
    /// failures inside one episode produce exactly ONE opening record, and
    /// the episode's close carries `suppressed_repeats == N - 1`.
    #[test]
    fn n_failures_in_one_episode_emit_one_record_and_count_n_minus_one() {
        const N: u64 = 29; // one rotation's worth, as measured.
        let mut book = EpisodeBook::new();
        let mut opened = 0;
        for i in 0..N {
            match book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 1_000 + i * 1_000) {
                FailureTransition::Opened => opened += 1,
                FailureTransition::Repeat => {}
                other => panic!("unexpected {other:?} inside one episode"),
            }
        }
        assert_eq!(opened, 1, "exactly one record per episode");
        assert!(book.any_open());

        // A success INSIDE the quiet interval does not close it (flap-proof).
        let last = 1_000 + (N - 1) * 1_000;
        let mut closes = Vec::new();
        book.on_success(last + 1, |k, e, c| closes.push((k, e, c)));
        assert!(closes.is_empty(), "a flap must not end the episode");

        book.on_success(last + EPISODE_QUIET_MS, |k, e, c| closes.push((k, e, c)));
        assert_eq!(
            closes,
            vec![(
                ExhaustionKind::Commit,
                Evidence::OsCode,
                ClosedEpisode {
                    suppressed_repeats: N - 1,
                    duration_ms: (N - 1) * 1_000
                }
            )]
        );
        assert!(!book.any_open());

        // And the next failure opens a fresh episode with its own record.
        assert_eq!(
            book.on_failure(
                ExhaustionKind::Commit,
                Evidence::OsCode,
                last + 2 * EPISODE_QUIET_MS
            ),
            FailureTransition::Opened
        );
    }

    /// A failure after a quiet interval with no success in between closes the
    /// old episode and opens a new one — the old count is not lost.
    #[test]
    fn a_failure_after_a_quiet_interval_reopens_and_reports_the_old_count() {
        let mut book = EpisodeBook::new();
        book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 0);
        book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 10);
        book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 20);
        assert_eq!(
            book.on_failure(
                ExhaustionKind::Commit,
                Evidence::OsCode,
                20 + EPISODE_QUIET_MS
            ),
            FailureTransition::Reopened(ClosedEpisode {
                suppressed_repeats: 2,
                duration_ms: 20
            })
        );
    }

    /// Kinds and evidence are separate episodes: an fd flood does not
    /// swallow the commit record, and a suspicion does not swallow a code.
    #[test]
    fn episodes_are_per_kind_and_per_evidence() {
        let mut book = EpisodeBook::new();
        assert_eq!(
            book.on_failure(ExhaustionKind::Fd, Evidence::OsCode, 0),
            FailureTransition::Opened
        );
        assert_eq!(
            book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 1),
            FailureTransition::Opened
        );
        assert_eq!(
            book.on_failure(ExhaustionKind::Commit, Evidence::ChildStderr, 2),
            FailureTransition::Opened
        );
        assert_eq!(
            book.on_failure(ExhaustionKind::Commit, Evidence::OsCode, 3),
            FailureTransition::Repeat
        );
    }

    /// The global wrapper applies the same edge: across N reports, exactly one
    /// opens. Uses the task-limit/stderr slot, which nothing else in this test
    /// binary reports into, so concurrent tests cannot perturb the count.
    #[test]
    fn report_exhaustion_emits_once_across_n_repeats() {
        let transitions: Vec<_> = (0..10)
            .map(|_| {
                report_exhaustion(
                    ExhaustionKind::TaskLimit,
                    Evidence::ChildStderr,
                    None,
                    "test: report_exhaustion edge",
                )
            })
            .collect();
        assert_eq!(transitions[0], FailureTransition::Opened);
        assert!(transitions[1..]
            .iter()
            .all(|t| *t == FailureTransition::Repeat));
    }

    /// Plan Phase 0 verification (b): a child that exited non-zero with the
    /// paging-file error in its stderr is SUSPECTED commit exhaustion. The
    /// needle is derived from this OS, so the test feeds back what this OS
    /// renders — in whatever language this box runs.
    #[test]
    fn a_child_stderr_carrying_the_os_text_is_commit_exhaustion_suspected() {
        for &code in COMMIT_CODES {
            let rendered = std::io::Error::from_raw_os_error(code).to_string();
            let os_text = rendered.split(" (os error ").next().unwrap();
            let stderr = format!("error launching git: {os_text}\r\n");
            assert_eq!(
                commit_exhaustion_in_stderr(stderr.as_bytes()),
                Some(code),
                "stderr {stderr:?}"
            );
            // A Rust child reports `(os error N)` — locale-independent.
            let rust_child = format!("spawn failed: something (os error {code})\n");
            assert_eq!(
                commit_exhaustion_in_stderr(rust_child.as_bytes()),
                Some(code)
            );
        }
        assert_eq!(
            event_kind_token(ExhaustionKind::Commit, Evidence::ChildStderr),
            "commit_exhaustion_suspected"
        );
    }

    /// An ordinary git failure is not a suspicion, and `os error N` must not
    /// match a longer code that merely starts with the same digits.
    #[test]
    fn ordinary_stderr_is_not_suspected() {
        assert_eq!(
            commit_exhaustion_in_stderr(b"fatal: not a git repository (or any parent)\n"),
            None
        );
        assert_eq!(commit_exhaustion_in_stderr(b""), None);
        for &code in COMMIT_CODES {
            let longer = format!("(os error {code}7)");
            assert_eq!(commit_exhaustion_in_stderr(longer.as_bytes()), None);
        }
    }

    /// The German rendering on the MSI box, reaching us through a non-UTF-8
    /// console code page: the ASCII-run needle still matches, which is the
    /// point of preferring it. (`0x81` is `ü` in CP850.)
    #[test]
    fn the_ascii_run_needle_survives_a_non_utf8_code_page() {
        let needle = message_needle(
            "Die Auslagerungsdatei ist zu klein, um diesen Vorgang durchzuführen. (os error 1455)",
        )
        .unwrap();
        assert_eq!(
            needle,
            "Die Auslagerungsdatei ist zu klein, um diesen Vorgang durchzuf"
        );
        let mut cp850 = b"error launching git: Die Auslagerungsdatei ist zu klein, um diesen \
                          Vorgang durchzuf"
            .to_vec();
        cp850.extend_from_slice(&[0x81]);
        cp850.extend_from_slice(b"hren.\r\n");
        assert!(String::from_utf8_lossy(&cp850).contains(needle.as_str()));
        // The English rendering keeps its whole text.
        assert_eq!(
            message_needle(
                "The paging file is too small for this operation to complete. (os error 1455)"
            )
            .unwrap(),
            "The paging file is too small for this operation to complete"
        );
        assert_eq!(message_needle("Unknown error 1455 (os error 1455)"), None);
    }

    /// The incident tokens: fd stays out of the file, and a text-evidenced
    /// commit episode keeps "suspected" in its token.
    #[test]
    fn incident_tokens_keep_suspicion_and_exclude_fd() {
        assert_eq!(incident_token(ExhaustionKind::Fd, Evidence::OsCode), None);
        assert_eq!(
            incident_token(ExhaustionKind::Commit, Evidence::OsCode),
            Some("commit_exhaustion")
        );
        assert_eq!(
            incident_token(ExhaustionKind::Commit, Evidence::ChildStderr),
            Some("commit_exhaustion_suspected")
        );
        assert_eq!(
            incident_token(ExhaustionKind::TaskLimit, Evidence::OsCode),
            Some("resource_exhaustion")
        );
        assert_eq!(
            incident_token(ExhaustionKind::TaskLimit, Evidence::ChildStderr),
            Some("resource_exhaustion_suspected")
        );
    }

    /// The cached reading is UNKNOWN until something records one.
    #[test]
    fn a_recorded_memory_reading_reads_back() {
        let r = MemoryReading {
            free_commit: 1 << 30,
            commit_limit: 71 << 30,
            free_phys: 3 << 30,
        };
        note_memory_reading(r);
        let (back, at) = last_memory_reading().expect("a reading was recorded");
        assert_eq!(back, r);
        assert!(at > 0);
    }
}
