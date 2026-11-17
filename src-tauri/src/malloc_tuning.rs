//! glibc malloc tuning for the runner process: cap the arenas at startup, read
//! the allocator on every health-monitor tick, and hand freed memory back to
//! the OS with an hourly `malloc_trim(0)`.
//!
//! # Why it exists
//!
//! Plan `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail`, Phases 1
//! and 3. A primary runner reached 65.7 GiB RSS after 6.6 days, and 64.1 GiB
//! of it sat in 1,495 glibc arena heaps. Two temp runners on the same build ran
//! the same 65-minute pane-churn workload (`qontinui-dev-notes`
//! `devops-data/2026-11-11-runner-rss-nilan/phase6.md`):
//!
//! - with glibc's defaults RSS grew 684 MiB/h, and the memory malloc itself
//!   counts as FREE grew 712 MiB/h; about 55 MiB was live once the panes closed;
//! - with `MALLOC_ARENA_MAX=2` RSS grew 15 MiB/h, at a cost of +14 % CPU;
//! - `malloc_trim(0)` on the default arm returned 680 MiB in 0.135 s.
//!
//! So the growth is freed memory that glibc keeps per arena, not live objects.
//! glibc allows up to 8 arenas per core (1,024 on a 128-core host), and a
//! runner with 80 panes of 4 threads each touches hundreds of them.
//!
//! # Why in the runner and not in a unit file
//!
//! A Linux user's .deb or AppImage runner has no systemd unit, so a unit-file
//! `MALLOC_ARENA_MAX` never reaches them, and a variable set there is inherited
//! by every child the runner spawns (cargo, `claude`, PostgreSQL). Windows
//! (HeapAlloc) and macOS (libmalloc) have no glibc arenas. So the machinery is
//! gated on `all(target_os = "linux", target_env = "gnu")`, and every public
//! function has a second, explicit arm that reports `unsupported` elsewhere.
//!
//! # The operator's environment wins
//!
//! glibc itself reads `MALLOC_ARENA_MAX`, and `glibc.malloc.arena_max` inside
//! `GLIBC_TUNABLES`, at process start. When either names a cap the runner
//! leaves it alone (`source=env`): an in-process `mallopt` would override it,
//! and honouring it keeps the A/B control arm, tuning N, and a unit-file
//! stop-gap possible. Skipping `mallopt` is safe only for a value glibc
//! certainly applied, so the runner reads both settings no more permissively
//! than glibc 2.41 does (measured; see `read_cap` and `tunable_arena_max`).
//! Any other value names no cap here: the runner applies its compiled cap, and
//! its startup log names the value it ignored.
//!
//! # A return code of 1 is not evidence that the cap holds
//!
//! glibc answers 1 to `mallopt(M_ARENA_MAX, 0)`, which does nothing. It also
//! answers 1 to a late `mallopt(M_ARENA_MAX, 2)`: glibc fixes its arena limit
//! the first time it needs one after more than `arena_test` (8) arenas exist,
//! and ignores the parameter from then on. So startup records an ATTEMPT
//! (requested N, source, return code). Whether the cap holds is read from the
//! arena count, which the health monitor logs and `/health` serves. The same
//! fact is why `apply_arena_cap_at_startup` is `main()`'s first statement: that
//! is the one point at which the process is certainly single-threaded
//! (`tests/arena_cap_is_mains_first_statement.rs` keeps it there).
//!
//! # The hourly trim
//!
//! The cap does not touch the main arena's brk heap (2,909.5 MiB on that
//! primary); only `malloc_trim` returns that memory. The health monitor's 60 s
//! metrics thread calls `on_metrics_tick`, which samples on every tick, trims
//! hourly (0.016 s measured with the cap at 2), and logs one INFO `allocator`
//! line every 10 minutes and after each trim. `QONTINUI_MALLOC_TRIM=0`, read
//! once at startup, turns the trim off, so an A/B control arm can run glibc's
//! own behaviour; the `allocator` line and `/health` say which way it is set.
//!
//! Publishing these readings to coord needs a `coord.device_resource_samples`
//! column pair (a qontinui-web alembic revision) and coord ingest: a recorded
//! follow-up to the parent plan's Phase 4, not part of this module.

/// The arena cap the runner applies when the operator's environment names
/// none.
///
/// 2 is the measured value. On the Phase 6 harness (`qontinui-dev-notes`
/// `devops-data/2026-11-11-runner-rss-nilan/phase6.md`) glibc's default grew
/// 684 MiB/h and `MALLOC_ARENA_MAX=2` grew 15 MiB/h, at a cost of +14 % CPU.
/// The plan's N=2 against N=4 probe has not run (take 4 if its workload slope
/// is at most 50 MiB/h and its steady-window CPU is below 2's), and the plan's
/// rule when that probe cannot run is to ship 2. It is worth running: 2's CPU
/// cost was measured with about 190 threads sharing 2 arenas, and a busy
/// primary runs about 550, so the contention grows with the quantity that
/// grows.
pub const ARENA_CAP: u64 = 2;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use glibc::{
    apply_arena_cap_at_startup, decide_arena_cap, health_json, log_startup_attempt,
    on_metrics_tick, sample, startup_attempt, trim, AllocatorSample, ArenaCapAttempt,
    ArenaCapDecision, ArenaEnv, CapSource, TrimResult,
};
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub use unsupported::{
    apply_arena_cap_at_startup, health_json, log_startup_attempt, on_metrics_tick,
};

/// glibc malloc: the cap, the reading, the trim.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    use std::fmt::Display;
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};
    use tracing::{info, warn};

    use super::ARENA_CAP;

    const MALLOC_ARENA_MAX: &str = "MALLOC_ARENA_MAX";
    const GLIBC_TUNABLES: &str = "GLIBC_TUNABLES";
    /// The tunable that `MALLOC_ARENA_MAX` is glibc's alias for.
    const TUNABLE_ARENA_MAX: &str = "glibc.malloc.arena_max";
    /// How often a metrics tick also logs the INFO `allocator` line.
    const LINE_INTERVAL: Duration = Duration::from_secs(10 * 60);
    /// How often a metrics tick runs `malloc_trim(0)`.
    const TRIM_INTERVAL: Duration = Duration::from_secs(60 * 60);
    /// The hourly trim's kill switch: `0` turns it off, so an A/B control arm
    /// runs glibc's own behaviour; unset or any other value leaves it on.
    /// Read once, at startup (declared in `ambient::AMBIENT_ENV_KEYS`).
    const TRIM_SWITCH_ENV: &str = "QONTINUI_MALLOC_TRIM";
    /// The `malloc_info` element that opens one arena's record.
    const ARENA_RECORD: &[u8] = b"<heap nr=";
    /// The longest operator value an `ignored` entry echoes, in chars.
    const ECHOED_VALUE_CHARS: usize = 64;
    const MIB: u64 = 1024 * 1024;

    // ── Phase 1: the cap ────────────────────────────────────────────────────

    /// The two settings glibc reads an arena cap from, captured once.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ArenaEnv {
        /// `MALLOC_ARENA_MAX`, decoded lossily.
        pub malloc_arena_max: Option<String>,
        /// `GLIBC_TUNABLES`, decoded lossily.
        pub glibc_tunables: Option<String>,
    }

    impl ArenaEnv {
        /// Read both from this process's environment.
        pub fn from_process() -> Self {
            let read = |name: &str| {
                std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
            };
            Self {
                malloc_arena_max: read(MALLOC_ARENA_MAX),
                glibc_tunables: read(GLIBC_TUNABLES),
            }
        }
    }

    /// Where an arena cap came from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CapSource {
        /// No operator setting names a cap, so the runner applies
        /// [`ARENA_CAP`] with `mallopt`.
        Compiled,
        /// The operator's environment names the cap. glibc applied it at
        /// process start, and the runner leaves it alone.
        Env,
    }

    impl CapSource {
        pub fn as_str(self) -> &'static str {
            match self {
                Self::Compiled => "compiled",
                Self::Env => "env",
            }
        }
    }

    /// What [`decide_arena_cap`] decided.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ArenaCapDecision {
        /// The arena cap this decision puts in force.
        pub cap: u64,
        pub source: CapSource,
        /// The variable that named the cap when `source` is `Env`:
        /// `GLIBC_TUNABLES` or `MALLOC_ARENA_MAX`.
        pub env_var: Option<&'static str>,
        /// Settings that are present but name no cap this runner can rely on,
        /// as `name="value"` (see `read_cap` and `tunable_arena_max`).
        pub ignored: Vec<String>,
    }

    impl ArenaCapDecision {
        fn env(cap: u64, env_var: &'static str, ignored: Vec<String>) -> Self {
            Self {
                cap,
                source: CapSource::Env,
                env_var: Some(env_var),
                ignored,
            }
        }

        fn compiled(ignored: Vec<String>) -> Self {
            Self {
                cap: ARENA_CAP,
                source: CapSource::Compiled,
                env_var: None,
                ignored,
            }
        }
    }

    /// Decide the arena cap: the environment's own cap when glibc certainly
    /// applied one (`source = env`), else [`ARENA_CAP`] (`source = compiled`).
    ///
    /// `source = env` makes the runner skip `mallopt`, so it is chosen only
    /// for a value glibc reads exactly as this module does. Everything else is
    /// recorded in `ignored`:
    /// - a `glibc.malloc.arena_max` in `GLIBC_TUNABLES` comes first, as it
    ///   does inside glibc. If its value is invalid to glibc too, or glibc
    ///   rejects the whole string, glibc honours `MALLOC_ARENA_MAX` instead,
    ///   and so does this. If its value is one glibc may read some other way
    ///   (`CapValue::Unread`), the compiled cap applies at once, because only
    ///   `mallopt` makes the cap certain then;
    /// - `MALLOC_ARENA_MAX` comes next. An empty or blank variable is no
    ///   setting at all.
    pub fn decide_arena_cap(env: &ArenaEnv) -> ArenaCapDecision {
        let mut ignored = Vec::new();
        if let Some(tunables) = env.glibc_tunables.as_deref() {
            match tunable_arena_max(tunables) {
                TunableArenaMax::Unset => {}
                TunableArenaMax::StringRejected => {
                    ignored.push(format!("{GLIBC_TUNABLES}={:?}", echoed(tunables)));
                }
                TunableArenaMax::Value(raw) => {
                    let read = read_cap(raw);
                    if let CapValue::Cap(cap) = read {
                        return ArenaCapDecision::env(cap, GLIBC_TUNABLES, ignored);
                    }
                    ignored.push(format!("{TUNABLE_ARENA_MAX}={:?}", echoed(raw)));
                    if read == CapValue::Unread {
                        return ArenaCapDecision::compiled(ignored);
                    }
                }
            }
        }
        if let Some(raw) = env
            .malloc_arena_max
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if let CapValue::Cap(cap) = read_cap(raw) {
                return ArenaCapDecision::env(cap, MALLOC_ARENA_MAX, ignored);
            }
            ignored.push(format!("{MALLOC_ARENA_MAX}={:?}", echoed(raw)));
        }
        ArenaCapDecision::compiled(ignored)
    }

    /// How one arena-cap value reads ([`read_cap`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CapValue {
        /// A plain positive decimal integer, which glibc reads as this cap.
        Cap(u64),
        /// A value glibc 2.41 rejects too, so it names no cap at all.
        Invalid,
        /// A value glibc accepts but this module does not read: a valid
        /// octal or hex number after a leading `0`, or a `-` (glibc negates,
        /// so `-1` is SIZE_MAX, which caps nothing). What glibc applied is
        /// not known. A leading-`0` or `-` value glibc rejects is `Invalid`.
        Unread,
    }

    /// Read one arena-cap value, never more permissively than glibc 2.41.
    ///
    /// A value read as a cap makes the runner skip `mallopt`, so accepting one
    /// glibc rejects would leave the runner uncapped while it logs
    /// `source=env`. glibc skips leading spaces and tabs (nothing else), takes
    /// an optional sign, and rejects any character after the number; it reads
    /// a leading `0` as octal or hex. Measured on 2.41 with 32 live threads:
    /// `"2"`, `" 2"`, `"\t2"`, `"+2"` and `" +2"` capped at 2; `"2 "`, `"2\n"`,
    /// `"\n2"`, `"+ 2"`, `"08"` and a number past `u64::MAX` left it uncapped;
    /// `"04"` capped at 4, and `"-1"` left it uncapped.
    fn read_cap(raw: &str) -> CapValue {
        let unsigned = raw.trim_start_matches([' ', '\t']);
        if let Some(body) = unsigned.strip_prefix('-') {
            return unread_unless_rejected(body);
        }
        let digits = unsigned.strip_prefix('+').unwrap_or(unsigned);
        if digits.starts_with('0') {
            return unread_unless_rejected(digits);
        }
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return CapValue::Invalid;
        }
        // Digits only, the first of them 1-9: a positive number, or one too
        // big for a u64, which glibc rejects as well.
        digits.parse().map_or(CapValue::Invalid, CapValue::Cap)
    }

    /// A value this module does not read (a `-` or a leading `0`): `Invalid`
    /// when glibc 2.41 rejects it too, so the next setting still stands, and
    /// `Unread` only when glibc may have applied it. `body` is the text after
    /// the blanks and one sign. Measured with `ld.so --list-tunables`: `0`,
    /// `00`, `08`, `09`, `0x`, `-0`, `+08` and `-08` are rejected (glibc then
    /// honours `MALLOC_ARENA_MAX`), while `04`, `0x4` and `-1` are applied.
    fn unread_unless_rejected(body: &str) -> CapValue {
        if glibc_rejects(body) {
            CapValue::Invalid
        } else {
            CapValue::Unread
        }
    }

    /// Whether glibc 2.41 rejects `body` as an arena-cap number: hex after
    /// `0x`/`0X`, octal after any other leading `0`, decimal otherwise, and
    /// zero (or an overflow) rejected.
    fn glibc_rejects(body: &str) -> bool {
        let (digits, radix) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            Some(hex) => (hex, 16),
            None if body.starts_with('0') => (body, 8),
            None => (body, 10),
        };
        // The digit check is needed because `from_str_radix` accepts a
        // leading `+`, which glibc rejects both after the sign and after a
        // `0x` prefix.
        !digits.chars().all(|c| c.is_digit(radix))
            || !u64::from_str_radix(digits, radix).is_ok_and(|n| n != 0)
    }

    /// What a `GLIBC_TUNABLES` string sets `glibc.malloc.arena_max` to
    /// ([`tunable_arena_max`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TunableArenaMax<'a> {
        /// No entry names it (an empty string included).
        Unset,
        /// The value of the last entry that names it.
        Value(&'a str),
        /// An entry names it, but glibc rejects the whole string, so nothing
        /// in the string applies.
        StringRejected,
    }

    /// Read `GLIBC_TUNABLES` the way glibc 2.41 does: `name=value` entries
    /// joined by `:`. Each rule here was measured on 2.41:
    /// - glibc rejects the WHOLE string when an entry's value contains `=`, or
    ///   when the last entry has no `=` (a trailing `:` makes an empty last
    ///   entry): `arena_max=2:`, `arena_max=2:foo` and `arena_max=2=3` all
    ///   left it uncapped. An earlier entry with no `=` is skipped:
    ///   `foo:arena_max=2` and `:arena_max=2` capped at 2;
    /// - only the LAST entry for a tunable counts, valid or not:
    ///   `arena_max=2:arena_max=x` left it uncapped, and
    ///   `arena_max=x:arena_max=2` capped at 2;
    /// - a name must match exactly: ` glibc.malloc.arena_max=2` set nothing.
    fn tunable_arena_max(tunables: &str) -> TunableArenaMax<'_> {
        let entries: Vec<&str> = tunables.split(':').collect();
        let names_it = |entry: &&str| entry.split('=').next() == Some(TUNABLE_ARENA_MAX);
        if !entries.iter().any(names_it) {
            return TunableArenaMax::Unset;
        }
        let mut value = None;
        for (index, entry) in entries.iter().enumerate() {
            match entry.split_once('=') {
                None if index + 1 == entries.len() => return TunableArenaMax::StringRejected,
                None => {}
                Some((_, rest)) if rest.contains('=') => return TunableArenaMax::StringRejected,
                Some((name, rest)) if name == TUNABLE_ARENA_MAX => value = Some(rest),
                Some(_) => {}
            }
        }
        value.map_or(TunableArenaMax::Unset, TunableArenaMax::Value)
    }

    /// An operator value as an `ignored` entry echoes it: at most
    /// [`ECHOED_VALUE_CHARS`] chars. Callers Debug-quote it, so a control
    /// character cannot forge a log line.
    fn echoed(raw: &str) -> String {
        raw.chars().take(ECHOED_VALUE_CHARS).collect()
    }

    /// What [`apply_arena_cap_at_startup`] decided and did: an attempt, not
    /// proof (see the module doc).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ArenaCapAttempt {
        pub decision: ArenaCapDecision,
        /// `mallopt(M_ARENA_MAX, cap)`'s return code when the runner made the
        /// call (`source = compiled`). `None` when the environment named the
        /// cap and no call was made. glibc answers 1 even when the call
        /// changes nothing.
        pub mallopt_rc: Option<i32>,
    }

    static STARTUP_ATTEMPT: OnceLock<ArenaCapAttempt> = OnceLock::new();

    /// Keeps apart this module's calls to the two functions glibc marks
    /// `const:mallopt` (`mallopt` and `mallinfo2`). attributes(7) makes a
    /// function with that mark unsafe to run beside another use of the same
    /// internal state, and names a lock around such calls as the workaround.
    static MALLOPT_STATE: Mutex<()> = Mutex::new(());

    /// Decide the arena cap and, unless the operator's environment names one,
    /// apply it with `mallopt(M_ARENA_MAX, ARENA_CAP)`. Also reads the trim
    /// kill switch, so that it too is read at startup.
    ///
    /// `main()`'s first statement, so that it runs before any other thread
    /// exists. Once per process: a second call changes nothing. Logging does
    /// not exist yet when it runs, so it logs nothing; [`log_startup_attempt`]
    /// reports the recorded attempt from `run_app`.
    pub fn apply_arena_cap_at_startup() {
        STARTUP_ATTEMPT.get_or_init(|| {
            let decision = decide_arena_cap(&ArenaEnv::from_process());
            let mallopt_rc = (decision.source == CapSource::Compiled).then(|| {
                let value = libc::c_int::try_from(decision.cap).unwrap_or(libc::c_int::MAX);
                let _state = lock(&MALLOPT_STATE);
                // SAFETY: two integers in, and no memory of ours touched. The
                // glibc manual marks `mallopt` MT-Unsafe (init,
                // const:mallopt). In the runner this is main()'s first
                // statement, so no other thread exists to race it; in any
                // process, `MALLOPT_STATE` keeps it apart from this module's
                // `mallinfo2`. The return code is recorded, never acted on.
                unsafe { libc::mallopt(libc::M_ARENA_MAX, value) }
            });
            ArenaCapAttempt {
                decision,
                mallopt_rc,
            }
        });
        let _ = trim_enabled();
    }

    /// The attempt this process recorded, or `None` when nothing called
    /// [`apply_arena_cap_at_startup`] (any binary but the runner and the
    /// `arena_cap_holds` test).
    pub fn startup_attempt() -> Option<&'static ArenaCapAttempt> {
        STARTUP_ATTEMPT.get()
    }

    /// Log the recorded attempt once logging exists. Worded as an attempt: the
    /// `allocator` line's `arena_count` is what shows whether the cap holds.
    pub fn log_startup_attempt() {
        let Some(attempt) = startup_attempt() else {
            warn!(
                "allocator: no arena-cap attempt was recorded in this process, so the runner \
                 applied no cap (glibc's own default allows up to 8 arenas per core); main() \
                 must apply the cap as its first statement"
            );
            return;
        };
        let decision = &attempt.decision;
        match decision.source {
            CapSource::Compiled => info!(
                requested = decision.cap,
                source = decision.source.as_str(),
                mallopt_rc = %field_or(attempt.mallopt_rc, "none"),
                "allocator: arena cap attempted with mallopt(M_ARENA_MAX); a return code of 1 \
                 does not show that it holds, the allocator line's arena_count does"
            ),
            CapSource::Env => info!(
                requested = decision.cap,
                source = decision.source.as_str(),
                env_var = decision.env_var.unwrap_or("-"),
                "allocator: arena cap left to the operator's environment, which glibc read at \
                 process start; no mallopt call"
            ),
        }
        if !decision.ignored.is_empty() {
            warn!(
                ignored = %decision.ignored.join(", "),
                "allocator: ignored an arena-cap setting that is not plain positive decimal, or \
                 that glibc rejects; the cap attempted is the one logged above"
            );
        }
        if !trim_enabled() {
            info!(
                env_var = TRIM_SWITCH_ENV,
                "allocator: the hourly malloc_trim(0) is off, by its kill switch"
            );
        } else if let Some(value) =
            trim_switch_warning_value(std::env::var_os(TRIM_SWITCH_ENV).as_deref())
        {
            warn!(
                env_var = TRIM_SWITCH_ENV,
                value = %value,
                "allocator: the trim kill switch is set to a value other than 0, so the hourly \
                 malloc_trim(0) stays ON; only 0 turns it off"
            );
        }
    }

    /// What the startup WARN names for the kill switch's raw OS value, already
    /// Debug-quoted and truncated (so a control character cannot forge a log
    /// line), or `None` when no WARN is due. The value is read lossily, so a
    /// non-UTF-8 value is named too.
    fn trim_switch_warning_value(raw: Option<&std::ffi::OsStr>) -> Option<String> {
        let lossy = raw.map(|v| v.to_string_lossy());
        trim_switch_unrecognised(lossy.as_deref()).map(|v| format!("{:?}", echoed(v)))
    }

    /// The kill switch's raw value when it is set, non-blank, not the one
    /// value that turns the trim off, and not an explicit "on" spelling
    /// (`1`, `true`, `on`, `yes`): a `false`, `off` or `no` someone meant as
    /// off, which must not leave the trim on silently. The caller reads the
    /// variable lossily, so a non-UTF-8 value is named too.
    fn trim_switch_unrecognised(value: Option<&str>) -> Option<&str> {
        value.filter(|v| {
            let word = v.trim();
            !word.is_empty()
                && trim_enabled_from(Some(v))
                && !["1", "true", "on", "yes"]
                    .iter()
                    .any(|on| word.eq_ignore_ascii_case(on))
        })
    }

    // ── Phase 3: the reading and the trim ───────────────────────────────────

    /// One reading of the allocator.
    ///
    /// The byte totals are malloc's own books, not residency: pages that
    /// `malloc_trim` hands back to the OS stay counted as free until their
    /// chunks are reused, so RSS is what shows what the OS still holds. A
    /// rising `in_use_bytes` on a capped runner is a live leak.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AllocatorSample {
        pub sampled_at: chrono::DateTime<chrono::Utc>,
        /// Bytes malloc holds from the OS: `mallinfo2` `arena + hblkhd`.
        pub system_bytes: u64,
        /// Bytes in live allocations: `uordblks + hblkhd`.
        pub in_use_bytes: u64,
        /// Bytes malloc holds but counts as free: `fordblks`. Freed memory
        /// that glibc keeps rather than returns shows up here.
        pub free_bytes: u64,
        /// Arenas, counted as `<heap nr=` records in `malloc_info`'s output.
        /// `None` is UNKNOWN (the output could not be read), never zero.
        pub arena_count: Option<u64>,
    }

    /// Read the allocator: the `mallinfo2()` totals plus the arena count.
    pub fn sample() -> AllocatorSample {
        let info = {
            let _state = lock(&MALLOPT_STATE);
            // SAFETY: no arguments, and the struct comes back by value.
            // man 3 mallinfo2 marks it MT-Unsafe init const:mallopt. init:
            // malloc is initialised by the process's first allocation, which
            // precedes every thread. const:mallopt: `MALLOPT_STATE`
            // keeps every `mallopt` and `mallinfo2` this module makes apart.
            unsafe { libc::mallinfo2() }
        };
        let bytes = |value: libc::size_t| value as u64;
        AllocatorSample {
            sampled_at: chrono::Utc::now(),
            system_bytes: bytes(info.arena).saturating_add(bytes(info.hblkhd)),
            in_use_bytes: bytes(info.uordblks).saturating_add(bytes(info.hblkhd)),
            free_bytes: bytes(info.fordblks),
            arena_count: arena_count(),
        }
    }

    /// The number of arenas. `malloc_info`'s XML, captured in memory through
    /// `open_memstream`, holds one `<heap nr="N">` record per arena. `None`
    /// when the stream cannot be opened or written, or when the output holds
    /// no record (the main arena always exists, so zero means the format
    /// changed).
    fn arena_count() -> Option<u64> {
        let mut buf: *mut libc::c_char = std::ptr::null_mut();
        let mut len: libc::size_t = 0;
        // SAFETY: man 3 open_memstream marks it MT-Safe. It keeps both
        // out-pointers and writes the buffer's address and length through
        // them on every flush and at close. Both point at locals of this
        // frame, which outlive the stream: it is closed below, before either
        // is read.
        let stream = unsafe { libc::open_memstream(&raw mut buf, &raw mut len) };
        if stream.is_null() {
            return None;
        }
        // SAFETY: man 3 malloc_info marks it MT-Safe. It writes only to
        // `stream`, the stream opened above, and 0 is the only options value
        // it accepts.
        let info_rc = unsafe { libc::malloc_info(0, stream) };
        // SAFETY: closes the stream opened above, exactly once. Afterwards
        // `buf` holds the final buffer and `len` its length, excluding the
        // trailing NUL.
        let close_rc = unsafe { libc::fclose(stream) };
        let count = if info_rc == 0 && close_rc == 0 && !buf.is_null() {
            // SAFETY: after a successful close, `buf` points at `len`
            // initialised bytes that this function owns until the `free` below.
            let xml = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), len) };
            Some(count_arena_records(xml)).filter(|&count| count > 0)
        } else {
            None
        };
        if !buf.is_null() {
            // SAFETY: the stream allocated `buf` with malloc, and it is the
            // caller's to free once the stream is closed. Freed once; `xml`
            // is not used past this point.
            unsafe { libc::free(buf.cast()) };
        }
        count
    }

    /// How many arena records a `malloc_info` document holds.
    fn count_arena_records(xml: &[u8]) -> u64 {
        xml.windows(ARENA_RECORD.len())
            .filter(|window| *window == ARENA_RECORD)
            .count() as u64
    }

    /// One `malloc_trim(0)`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TrimResult {
        pub at: chrono::DateTime<chrono::Utc>,
        /// `malloc_trim`'s answer: 1 when it released memory, 0 when it found
        /// none to release.
        pub rc: i32,
        pub duration: Duration,
        /// RSS just before the trim; `None` where the reading failed.
        pub rss_before: Option<u64>,
        /// RSS just after the trim; `None` where the reading failed.
        pub rss_after: Option<u64>,
    }

    impl TrimResult {
        /// RSS the trim gave back. `None` (UNKNOWN) when either reading
        /// failed; 0 when RSS did not fall, since other threads keep
        /// allocating meanwhile.
        pub fn released_bytes(&self) -> Option<u64> {
            Some(self.rss_before?.saturating_sub(self.rss_after?))
        }
    }

    /// Return freed memory to the OS with `malloc_trim(0)`, timing the call
    /// and reading RSS on either side of it with `read_rss_bytes`, for which 0
    /// means unreadable.
    pub fn trim(read_rss_bytes: fn() -> u64) -> TrimResult {
        let rss_before = rss(read_rss_bytes);
        let started = Instant::now();
        // SAFETY: man 3 malloc_trim marks it MT-Safe. It takes only a padding
        // size and touches no memory of ours.
        let rc = unsafe { libc::malloc_trim(0) };
        let duration = started.elapsed();
        let rss_after = rss(read_rss_bytes);
        TrimResult {
            at: chrono::Utc::now(),
            rc,
            duration,
            rss_before,
            rss_after,
        }
    }

    fn rss(read_rss_bytes: fn() -> u64) -> Option<u64> {
        Some(read_rss_bytes()).filter(|&bytes| bytes > 0)
    }

    static TRIM_ENABLED: OnceLock<bool> = OnceLock::new();

    /// Whether the hourly trim runs: [`TRIM_SWITCH_ENV`] as read the first
    /// time this is asked, which in the runner is startup
    /// ([`apply_arena_cap_at_startup`] asks).
    fn trim_enabled() -> bool {
        *TRIM_ENABLED
            .get_or_init(|| trim_enabled_from(std::env::var(TRIM_SWITCH_ENV).ok().as_deref()))
    }

    /// The kill switch's reading. Only `0` (surrounding whitespace aside)
    /// turns the trim off, as for the runner's other `0`-means-off switches;
    /// unset, unreadable or any other value leaves it on.
    fn trim_enabled_from(value: Option<&str>) -> bool {
        value.is_none_or(|value| value.trim() != "0")
    }

    // ── The metrics tick: memo, cadence, log line ───────────────────────────

    /// What the last metrics tick saw. `/health` serves it.
    #[derive(Debug)]
    struct Memo {
        sample: Option<AllocatorSample>,
        rss_bytes: Option<u64>,
        last_trim: Option<TrimResult>,
    }

    impl Memo {
        const EMPTY: Self = Self {
            sample: None,
            rss_bytes: None,
            last_trim: None,
        };
    }

    static MEMO: Mutex<Memo> = Mutex::new(Memo::EMPTY);
    static CADENCE: Mutex<Cadence> = Mutex::new(Cadence::new());

    /// Lock, recovering a poisoned lock: a panic elsewhere must not blind this
    /// reading on exactly the process that is already in trouble.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// When a metrics tick also logs the `allocator` line, and when it trims.
    #[derive(Debug)]
    struct Cadence {
        first_tick: Option<Instant>,
        last_line: Option<Instant>,
        last_trim: Option<Instant>,
    }

    /// One tick's extra work.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Due {
        trim: bool,
        line: bool,
    }

    impl Cadence {
        const fn new() -> Self {
            Self {
                first_tick: None,
                last_line: None,
                last_trim: None,
            }
        }

        /// This tick's work, recorded as done. The first tick logs a line (the
        /// baseline) and does not trim. With the trim enabled, a trim is due
        /// [`TRIM_INTERVAL`] after the first tick or the last trim; a line is
        /// due with each trim and [`LINE_INTERVAL`] after the last line.
        fn due(&mut self, now: Instant, trim_enabled: bool) -> Due {
            let first_tick = *self.first_tick.get_or_insert(now);
            let trim_anchor = self.last_trim.unwrap_or(first_tick);
            let trim = trim_enabled && now.saturating_duration_since(trim_anchor) >= TRIM_INTERVAL;
            if trim {
                self.last_trim = Some(now);
            }
            let line = trim
                || self
                    .last_line
                    .is_none_or(|last| now.saturating_duration_since(last) >= LINE_INTERVAL);
            if line {
                self.last_line = Some(now);
            }
            Due { trim, line }
        }
    }

    /// One health-monitor metrics tick (every 60 s). Samples the allocator
    /// and memoises the reading for `/health`; runs `malloc_trim(0)` hourly
    /// unless its kill switch is set; logs the INFO `allocator` line every 10
    /// minutes and after each trim, with a WARN beside it when the arena count
    /// exceeds the cap. `read_rss_bytes` is the monitor's own RSS reader, for
    /// which 0 means unreadable.
    pub fn on_metrics_tick(read_rss_bytes: fn() -> u64) {
        let trim_on = trim_enabled();
        let due = lock(&CADENCE).due(Instant::now(), trim_on);
        let trimmed = due.trim.then(|| trim(read_rss_bytes));
        // Sampled after the trim, so the line that follows one shows what the
        // trim left behind.
        let sample = sample();
        let rss_bytes = rss(read_rss_bytes);
        let last_trim = {
            let mut memo = lock(&MEMO);
            if trimmed.is_some() {
                memo.last_trim = trimmed;
            }
            memo.sample = Some(sample.clone());
            memo.rss_bytes = rss_bytes;
            memo.last_trim.clone()
        };
        if due.line {
            log_allocator_line(
                startup_attempt(),
                trim_on,
                &sample,
                rss_bytes,
                last_trim.as_ref(),
            );
        }
    }

    /// The INFO `allocator` line, and the WARN when the arena count exceeds
    /// the cap. An unreadable count is logged as `unknown` and is not a fault.
    fn log_allocator_line(
        attempt: Option<&ArenaCapAttempt>,
        trim_on: bool,
        sample: &AllocatorSample,
        rss_bytes: Option<u64>,
        last_trim: Option<&TrimResult>,
    ) {
        let cap = attempt.map(|attempt| attempt.decision.cap);
        let cap_source = attempt.map_or("none", |attempt| attempt.decision.source.as_str());
        let last_trim_released_mb = last_trim.map_or_else(
            || "none".to_string(),
            |trim| field_or(trim.released_bytes().map(mib), "unknown"),
        );
        info!(
            rss_mb = %field_or(rss_bytes.map(mib), "unknown"),
            arena_count = %field_or(sample.arena_count, "unknown"),
            cap = %field_or(cap, "none"),
            cap_source,
            in_use_mb = mib(sample.in_use_bytes),
            free_mb = mib(sample.free_bytes),
            system_mb = mib(sample.system_bytes),
            trim = if trim_on { "on" } else { "off" },
            last_trim_released_mb = %last_trim_released_mb,
            last_trim_ms = %field_or(last_trim.map(|trim| trim.duration.as_millis()), "none"),
            "allocator"
        );
        if cap_exceeded(sample.arena_count, cap) == Some(true) {
            warn!(
                arena_count = %field_or(sample.arena_count, "unknown"),
                cap = %field_or(cap, "none"),
                cap_source,
                "allocator: arena_count exceeds the arena cap, so the cap is not in effect"
            );
        }
    }

    /// Whether an arena count exceeds the cap. `None` (UNKNOWN) when the
    /// count could not be read or no cap was attempted.
    fn cap_exceeded(arena_count: Option<u64>, cap: Option<u64>) -> Option<bool> {
        Some(arena_count? > cap?)
    }

    fn mib(bytes: u64) -> u64 {
        bytes / MIB
    }

    /// A log field's value, or the word that stands for its absence.
    fn field_or<T: Display>(value: Option<T>, absent: &str) -> String {
        value.map_or_else(|| absent.to_string(), |value| value.to_string())
    }

    /// `/health`'s `allocator` block, from the last metrics tick's memo:
    ///
    /// - `status`: `sampled`, or `unknown` before the first tick (never zeros);
    /// - `cap`: `{requested, source, envVar, ignored, malloptRc}` from the
    ///   startup attempt, `null` when none was recorded;
    /// - `arenaCount` and `arenaCountExceedsCap`: `null` is UNKNOWN;
    /// - `rssMb` (`null` when unreadable), `inUseMb`, `freeMb`, `systemMb`;
    /// - `trim`: `{enabled, killSwitchEnv}`, whether the hourly trim runs;
    /// - `lastTrim`: `{at, rc, ms, releasedMb, rssBeforeMb, rssAfterMb}`,
    ///   `null` before the first hourly trim or while the trim is off;
    /// - `sampledAt`: RFC 3339.
    pub fn health_json() -> Value {
        health_json_from(startup_attempt(), trim_enabled(), &lock(&MEMO))
    }

    /// [`health_json`] over a given attempt, trim switch and memo, so that its
    /// shapes are pinned by tests.
    fn health_json_from(attempt: Option<&ArenaCapAttempt>, trim_on: bool, memo: &Memo) -> Value {
        let cap = attempt.map_or(Value::Null, |attempt| {
            json!({
                "requested": attempt.decision.cap,
                "source": attempt.decision.source.as_str(),
                "envVar": attempt.decision.env_var,
                "ignored": attempt.decision.ignored,
                "malloptRc": attempt.mallopt_rc,
            })
        });
        let trim = json!({ "enabled": trim_on, "killSwitchEnv": TRIM_SWITCH_ENV });
        let Some(sample) = memo.sample.as_ref() else {
            return json!({
                "status": "unknown",
                "reason": "no allocator sample has been taken in this process yet; the health \
                           monitor samples every 60 s",
                "cap": cap,
                "trim": trim,
            });
        };
        json!({
            "status": "sampled",
            "cap": cap,
            "trim": trim,
            "arenaCount": sample.arena_count,
            "arenaCountExceedsCap": cap_exceeded(
                sample.arena_count,
                attempt.map(|attempt| attempt.decision.cap),
            ),
            "rssMb": memo.rss_bytes.map(mib),
            "inUseMb": mib(sample.in_use_bytes),
            "freeMb": mib(sample.free_bytes),
            "systemMb": mib(sample.system_bytes),
            "lastTrim": memo.last_trim.as_ref().map(|trim| json!({
                "at": trim.at.to_rfc3339(),
                "rc": trim.rc,
                "ms": u64::try_from(trim.duration.as_millis()).unwrap_or(u64::MAX),
                "releasedMb": trim.released_bytes().map(mib),
                "rssBeforeMb": trim.rss_before.map(mib),
                "rssAfterMb": trim.rss_after.map(mib),
            })),
            "sampledAt": sample.sampled_at.to_rfc3339(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn env(malloc_arena_max: Option<&str>, glibc_tunables: Option<&str>) -> ArenaEnv {
            ArenaEnv {
                malloc_arena_max: malloc_arena_max.map(str::to_owned),
                glibc_tunables: glibc_tunables.map(str::to_owned),
            }
        }

        // ── decide_arena_cap ──

        #[test]
        fn an_unset_environment_takes_the_compiled_cap() {
            assert_eq!(
                decide_arena_cap(&env(None, None)),
                ArenaCapDecision {
                    cap: ARENA_CAP,
                    source: CapSource::Compiled,
                    env_var: None,
                    ignored: vec![],
                }
            );
        }

        #[test]
        fn malloc_arena_max_names_the_cap_and_the_runner_leaves_it_alone() {
            // glibc skips leading spaces and tabs and takes a `+` (measured).
            for (raw, cap) in [("8", 8), (" 2", 2), ("\t2", 2), ("+2", 2), (" +2", 2)] {
                let decision = decide_arena_cap(&env(Some(raw), None));
                assert_eq!(
                    (decision.cap, decision.source, decision.env_var),
                    (cap, CapSource::Env, Some("MALLOC_ARENA_MAX")),
                    "MALLOC_ARENA_MAX={raw:?}"
                );
                assert!(decision.ignored.is_empty(), "MALLOC_ARENA_MAX={raw:?}");
            }
        }

        #[test]
        fn a_tunables_string_with_arena_max_names_the_cap() {
            // glibc skips an earlier entry with no `=` (measured).
            for (tunables, cap) in [
                (
                    "glibc.malloc.trim_threshold=131072:glibc.malloc.arena_max=4",
                    4,
                ),
                (":glibc.malloc.arena_max=2", 2),
                ("foo:glibc.malloc.arena_max=2", 2),
                ("glibc.malloc.arena_max= 2", 2),
            ] {
                let decision = decide_arena_cap(&env(None, Some(tunables)));
                assert_eq!(
                    (decision.cap, decision.source, decision.env_var),
                    (cap, CapSource::Env, Some("GLIBC_TUNABLES")),
                    "GLIBC_TUNABLES={tunables:?}"
                );
            }
        }

        #[test]
        fn a_tunables_string_without_arena_max_takes_the_compiled_cap() {
            // `arena_maxx` is a different name, not a spelling of arena_max.
            let decision = decide_arena_cap(&env(
                None,
                Some("glibc.malloc.trim_threshold=131072:glibc.malloc.arena_maxx=3"),
            ));
            assert_eq!(
                (decision.cap, decision.source, decision.env_var),
                (ARENA_CAP, CapSource::Compiled, None)
            );
            assert!(decision.ignored.is_empty());
        }

        #[test]
        fn a_malformed_value_names_no_cap_so_the_compiled_cap_applies_and_says_so() {
            // Measured on glibc 2.41: "2 ", "2\n", "\n2", "+ 2" and "08" leave
            // it uncapped. "04", "0x4" and "-1" it reads some other way, so
            // only mallopt makes the cap certain.
            for raw in [
                "abc", "0", "-1", "8abc", "2.5", "2 ", "2\n", "\n2", "+ 2", "+", "08", "04", "0x4",
            ] {
                let decision = decide_arena_cap(&env(Some(raw), None));
                assert_eq!(
                    (decision.cap, decision.source),
                    (ARENA_CAP, CapSource::Compiled),
                    "MALLOC_ARENA_MAX={raw:?}"
                );
                assert_eq!(
                    decision.ignored,
                    vec![format!("MALLOC_ARENA_MAX={raw:?}")],
                    "MALLOC_ARENA_MAX={raw:?}"
                );
            }
            for value in ["lots", "2 "] {
                let tunables = format!("glibc.malloc.arena_max={value}");
                let decision = decide_arena_cap(&env(None, Some(&tunables)));
                assert_eq!(decision.source, CapSource::Compiled, "{tunables:?}");
                assert_eq!(
                    decision.ignored,
                    vec![format!("glibc.malloc.arena_max={value:?}")],
                    "{tunables:?}"
                );
            }
        }

        #[test]
        fn an_ignored_value_is_quoted_and_truncated() {
            // Quoted: a newline in the value cannot start a forged log line.
            let decision = decide_arena_cap(&env(Some("2\nforged line"), None));
            assert_eq!(
                decision.ignored,
                vec![r#"MALLOC_ARENA_MAX="2\nforged line""#]
            );
            // Truncated to 64 chars (100 digits also overflow, so name no cap).
            let decision = decide_arena_cap(&env(Some(&"9".repeat(100)), None));
            assert_eq!(decision.source, CapSource::Compiled);
            assert_eq!(
                decision.ignored,
                vec![format!("MALLOC_ARENA_MAX={:?}", "9".repeat(64))]
            );
        }

        #[test]
        fn an_empty_variable_is_no_setting_at_all() {
            for raw in ["", "  "] {
                let decision = decide_arena_cap(&env(Some(raw), None));
                assert_eq!(decision.source, CapSource::Compiled, "{raw:?}");
                assert!(decision.ignored.is_empty(), "{raw:?}");
            }
        }

        #[test]
        fn a_valid_tunable_outranks_the_alias_and_a_malformed_one_does_not() {
            let decision = decide_arena_cap(&env(Some("8"), Some("glibc.malloc.arena_max=4")));
            assert_eq!(
                (decision.cap, decision.env_var),
                (4, Some("GLIBC_TUNABLES"))
            );

            let decision = decide_arena_cap(&env(Some("8"), Some("glibc.malloc.arena_max=x")));
            assert_eq!(
                (decision.cap, decision.env_var),
                (8, Some("MALLOC_ARENA_MAX"))
            );
            assert_eq!(decision.ignored, vec![r#"glibc.malloc.arena_max="x""#]);
        }

        #[test]
        fn only_the_last_tunable_entry_counts_valid_or_not() {
            // Measured on glibc 2.41: `4:6:x` left it uncapped (the valid 4
            // and 6 count for nothing), and `x:2` capped at 2.
            let tunables =
                "glibc.malloc.arena_max=4:glibc.malloc.arena_max=6:glibc.malloc.arena_max=x";
            let decision = decide_arena_cap(&env(None, Some(tunables)));
            assert_eq!(
                (decision.cap, decision.source),
                (ARENA_CAP, CapSource::Compiled)
            );
            assert_eq!(decision.ignored, vec![r#"glibc.malloc.arena_max="x""#]);
            // An invalid last entry means glibc honours the alias instead.
            let decision = decide_arena_cap(&env(Some("3"), Some(tunables)));
            assert_eq!(
                (decision.cap, decision.env_var),
                (3, Some("MALLOC_ARENA_MAX"))
            );

            let tunables = "glibc.malloc.arena_max=x:glibc.malloc.arena_max=2";
            let decision = decide_arena_cap(&env(None, Some(tunables)));
            assert_eq!(
                (decision.cap, decision.env_var),
                (2, Some("GLIBC_TUNABLES"))
            );
            assert!(decision.ignored.is_empty());
        }

        #[test]
        fn a_tunables_string_glibc_rejects_whole_names_no_cap() {
            // Measured on glibc 2.41: each of these left it uncapped, with
            // "invalid GLIBC_TUNABLES ... ignored" for the whole string.
            for tunables in [
                "glibc.malloc.arena_max=2:",
                "glibc.malloc.arena_max=2:foo",
                "glibc.malloc.arena_max=2=3",
                "glibc.malloc.arena_max=2:glibc.malloc.arena_max",
            ] {
                let whole = vec![format!("GLIBC_TUNABLES={tunables:?}")];
                let decision = decide_arena_cap(&env(None, Some(tunables)));
                assert_eq!(decision.source, CapSource::Compiled, "{tunables:?}");
                assert_eq!(decision.ignored, whole, "{tunables:?}");
                // With the whole string ignored, glibc honours the alias.
                let decision = decide_arena_cap(&env(Some("4"), Some(tunables)));
                assert_eq!(
                    (decision.cap, decision.env_var),
                    (4, Some("MALLOC_ARENA_MAX")),
                    "{tunables:?}"
                );
                assert_eq!(decision.ignored, whole, "{tunables:?}");
            }
            // A name must match exactly, so this sets nothing at all.
            let decision = decide_arena_cap(&env(None, Some(" glibc.malloc.arena_max=2")));
            assert_eq!(decision.source, CapSource::Compiled);
            assert!(decision.ignored.is_empty());
        }

        #[test]
        fn a_tunable_glibc_may_read_another_way_leaves_only_the_compiled_cap() {
            // glibc reads "04" as octal 4 and "0x400" as 1024, and "-1" as
            // SIZE_MAX (measured: uncapped). Whatever it applied, the alias
            // did not stand in for it, so only mallopt makes the cap certain.
            for value in [
                "04",
                "0x400",
                "-1",
                "0X4",
                "0xA",
                "0x100000000",
                "-18446744073709551615",
            ] {
                let tunables = format!("glibc.malloc.arena_max={value}");
                let decision = decide_arena_cap(&env(Some("8"), Some(&tunables)));
                assert_eq!(
                    (decision.cap, decision.source),
                    (ARENA_CAP, CapSource::Compiled),
                    "{tunables:?}"
                );
                assert_eq!(
                    decision.ignored,
                    vec![format!("glibc.malloc.arena_max={value:?}")],
                    "{tunables:?}"
                );
            }
        }

        #[test]
        fn a_leading_zero_or_minus_tunable_glibc_rejects_lets_the_alias_stand() {
            // glibc 2.41 rejects each of these (measured with
            // `ld.so --list-tunables`: arena_max stays at the alias's 0x8), so
            // MALLOC_ARENA_MAX still stands, as it does inside glibc.
            for value in [
                "0", "00", "08", "09", "0x", "0X", "-0", "+0", "+08", "-08", "-+1", "0x+1",
                "-0x+1", "+0x+1",
            ] {
                let tunables = format!("glibc.malloc.arena_max={value}");
                let decision = decide_arena_cap(&env(Some("8"), Some(&tunables)));
                assert_eq!(
                    (decision.cap, decision.source, decision.env_var),
                    (8, CapSource::Env, Some(MALLOC_ARENA_MAX)),
                    "{tunables:?}"
                );
                assert_eq!(
                    decision.ignored,
                    vec![format!("glibc.malloc.arena_max={value:?}")],
                    "{tunables:?}"
                );
            }
        }

        // ── the reading ──

        #[test]
        fn malloc_info_holds_one_heap_record_per_arena() {
            let xml = br#"<malloc version="1">
<heap nr="0">
<sizes>
  <size from="17" to="32" total="64" count="2"/>
  <unsorted from="1041" to="1041" total="1041" count="1"/>
</sizes>
<total type="fast" count="2" size="64"/>
<total type="rest" count="1" size="133000"/>
<system type="current" size="135168"/>
<system type="max" size="135168"/>
<aspace type="total" size="135168"/>
<aspace type="mprotect" size="135168"/>
</heap>
<heap nr="1">
<sizes>
</sizes>
<total type="fast" count="0" size="0"/>
<total type="rest" count="1" size="131000"/>
<system type="current" size="135168"/>
<system type="max" size="135168"/>
<aspace type="total" size="135168"/>
<aspace type="mprotect" size="135168"/>
<aspace type="subheaps" size="1"/>
</heap>
<total type="fast" count="2" size="64"/>
<total type="rest" count="2" size="264000"/>
<total type="mmap" count="0" size="0"/>
<system type="current" size="270336"/>
<system type="max" size="270336"/>
<aspace type="total" size="270336"/>
<aspace type="mprotect" size="270336"/>
</malloc>
"#;
            assert_eq!(count_arena_records(xml), 2);
            assert_eq!(count_arena_records(b""), 0);
        }

        /// The FFI path against this test process's own allocator.
        #[test]
        fn a_live_sample_counts_arenas_and_its_totals_add_up() {
            let sample = sample();
            assert!(
                sample.arena_count.is_some_and(|count| count >= 1),
                "the main arena always exists: {sample:?}"
            );
            assert!(sample.system_bytes > 0, "{sample:?}");
            // mallinfo2 counts each arena's system memory as uordblks + fordblks.
            assert_eq!(
                sample.in_use_bytes + sample.free_bytes,
                sample.system_bytes,
                "{sample:?}"
            );
        }

        #[test]
        fn a_live_trim_reads_rss_once_on_each_side_and_an_unreadable_one_is_unknown() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static READS: AtomicU64 = AtomicU64::new(0);
            // 10 MiB on the first reading, 4 MiB on every later one.
            fn stepping() -> u64 {
                if READS.fetch_add(1, Ordering::SeqCst) == 0 {
                    10 * MIB
                } else {
                    4 * MIB
                }
            }
            let trimmed = trim(stepping);
            assert_eq!(READS.load(Ordering::SeqCst), 2, "{trimmed:?}");
            assert_eq!(
                (trimmed.rss_before, trimmed.rss_after),
                (Some(10 * MIB), Some(4 * MIB)),
                "{trimmed:?}"
            );
            assert_eq!(trimmed.released_bytes(), Some(6 * MIB), "{trimmed:?}");

            fn unreadable() -> u64 {
                0
            }
            let trimmed = trim(unreadable);
            assert_eq!(trimmed.released_bytes(), None, "{trimmed:?}");
        }

        fn trim_result(rss_before: Option<u64>, rss_after: Option<u64>) -> TrimResult {
            TrimResult {
                at: chrono::Utc::now(),
                rc: 1,
                duration: Duration::from_millis(16),
                rss_before,
                rss_after,
            }
        }

        #[test]
        fn released_rss_is_unknown_when_either_reading_failed_and_never_negative() {
            assert_eq!(trim_result(Some(10), Some(4)).released_bytes(), Some(6));
            assert_eq!(trim_result(Some(4), Some(10)).released_bytes(), Some(0));
            assert_eq!(trim_result(None, Some(4)).released_bytes(), None);
            assert_eq!(trim_result(Some(10), None).released_bytes(), None);
        }

        #[test]
        fn an_arena_count_above_the_cap_is_a_breach_and_an_unreadable_one_is_unknown() {
            assert_eq!(cap_exceeded(Some(3), Some(2)), Some(true));
            assert_eq!(cap_exceeded(Some(2), Some(2)), Some(false));
            assert_eq!(cap_exceeded(None, Some(2)), None);
            assert_eq!(cap_exceeded(Some(3), None), None);
        }

        // ── the cadence ──

        /// Which minutes of a run do extra work, ticking every `tick_secs`.
        fn worked_minutes(tick_secs: u64, ticks: u64, trim_enabled: bool) -> Vec<(u64, Due)> {
            let start = Instant::now();
            let mut cadence = Cadence::new();
            (0..ticks)
                .filter_map(|tick| {
                    let elapsed = Duration::from_secs(tick * tick_secs);
                    let due = cadence.due(start + elapsed, trim_enabled);
                    (due.line || due.trim).then_some((elapsed.as_secs() / 60, due))
                })
                .collect()
        }

        const LINE: Due = Due {
            trim: false,
            line: true,
        };

        #[test]
        fn the_line_runs_at_start_every_ten_minutes_and_with_each_hourly_trim() {
            let trim = Due {
                trim: true,
                line: true,
            };
            let expected: Vec<(u64, Due)> = (0..=12)
                .map(|n| (n * 10, if n > 0 && n % 6 == 0 { trim } else { LINE }))
                .collect();
            assert_eq!(worked_minutes(60, 121, true), expected);
        }

        #[test]
        fn the_trim_kill_switch_stops_the_trim_but_not_the_line() {
            let expected: Vec<(u64, Due)> = (0..=12).map(|n| (n * 10, LINE)).collect();
            assert_eq!(worked_minutes(60, 121, false), expected);
        }

        #[test]
        fn only_zero_turns_the_trim_off() {
            for off in ["0", " 0", "0\n"] {
                assert!(!trim_enabled_from(Some(off)), "{off:?}");
            }
            for on in ["1", "", "off", "false", "00"] {
                assert!(trim_enabled_from(Some(on)), "{on:?}");
            }
            assert!(trim_enabled_from(None));
            assert_eq!(trim_switch_unrecognised(Some("off")), Some("off"));
            assert_eq!(trim_switch_unrecognised(Some("no")), Some("no"));
            assert_eq!(trim_switch_unrecognised(Some("false")), Some("false"));
            assert_eq!(trim_switch_unrecognised(Some("0")), None);
            assert_eq!(trim_switch_unrecognised(Some(" 0 ")), None);
            assert_eq!(trim_switch_unrecognised(Some("")), None);
            assert_eq!(trim_switch_unrecognised(None), None);
            assert_eq!(trim_switch_unrecognised(Some("  ")), None);
            for on in ["1", "true", "ON", " yes "] {
                assert_eq!(trim_switch_unrecognised(Some(on)), None, "{on:?}");
            }
        }

        #[test]
        fn the_trim_switch_warning_names_the_value_quoted_and_lossily() {
            use std::ffi::OsStr;
            // Quoted: a newline cannot start a second log line.
            assert_eq!(
                trim_switch_warning_value(Some(OsStr::new("off\nforged"))).as_deref(),
                Some("\"off\\nforged\"")
            );
            // Lossy: a non-UTF-8 value is named, never silently left on.
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let named = trim_switch_warning_value(Some(OsStr::from_bytes(b"of\xfff")));
                assert_eq!(named.as_deref(), Some("\"of\u{fffd}f\""));
            }
            assert_eq!(trim_switch_warning_value(Some(OsStr::new("0"))), None);
            assert_eq!(trim_switch_warning_value(Some(OsStr::new("true"))), None);
            assert_eq!(trim_switch_warning_value(None), None);
            // Truncated: a long value is cut to 64 chars before quoting.
            assert_eq!(
                trim_switch_warning_value(Some(OsStr::new(&"x".repeat(100)))),
                Some(format!("{:?}", "x".repeat(64)))
            );
        }

        #[test]
        fn a_slow_tick_does_not_skip_a_line() {
            // The metrics loop's ticks are at least 60 s apart, never less; a
            // tick that runs late still lands the line on the tenth tick.
            let lines: Vec<u64> = worked_minutes(61, 21, true)
                .into_iter()
                .map(|(minute, _)| minute)
                .collect();
            assert_eq!(lines, vec![0, 10, 20]);
        }

        // ── /health ──

        fn sample_with(arena_count: Option<u64>) -> AllocatorSample {
            AllocatorSample {
                sampled_at: chrono::Utc::now(),
                system_bytes: 300 * MIB,
                in_use_bytes: 200 * MIB,
                free_bytes: 100 * MIB,
                arena_count,
            }
        }

        #[test]
        fn health_is_unknown_before_the_first_sample_never_zero() {
            let block = health_json_from(None, true, &Memo::EMPTY);
            assert_eq!(block["status"], "unknown");
            assert!(block["cap"].is_null());
            assert!(block.get("arenaCount").is_none(), "{block}");
            assert!(block.get("inUseMb").is_none(), "{block}");
            // The switch is known before any sample is.
            assert_eq!(block["trim"]["enabled"], true, "{block}");
            assert_eq!(block["trim"]["killSwitchEnv"], TRIM_SWITCH_ENV, "{block}");
        }

        #[test]
        fn health_serves_the_sample_the_cap_and_the_last_trim() {
            let attempt = ArenaCapAttempt {
                decision: decide_arena_cap(&ArenaEnv::default()),
                mallopt_rc: Some(1),
            };
            let memo = Memo {
                sample: Some(sample_with(Some(3))),
                rss_bytes: Some(512 * MIB),
                last_trim: Some(trim_result(Some(600 * MIB), Some(520 * MIB))),
            };
            let block = health_json_from(Some(&attempt), true, &memo);
            assert_eq!(block["status"], "sampled");
            assert_eq!(block["trim"]["enabled"], true);
            assert_eq!(block["cap"]["requested"], ARENA_CAP);
            assert_eq!(block["cap"]["source"], "compiled");
            assert_eq!(block["cap"]["malloptRc"], 1);
            assert_eq!(block["arenaCount"], 3);
            assert_eq!(block["arenaCountExceedsCap"], true);
            assert_eq!(block["rssMb"], 512);
            assert_eq!(block["inUseMb"], 200);
            assert_eq!(block["freeMb"], 100);
            assert_eq!(block["systemMb"], 300);
            assert_eq!(block["lastTrim"]["releasedMb"], 80);
            assert_eq!(block["lastTrim"]["ms"], 16);
            assert!(block["sampledAt"].as_str().is_some(), "{block}");
        }

        #[test]
        fn health_serves_an_unreadable_count_as_null_never_zero() {
            let memo = Memo {
                sample: Some(sample_with(None)),
                rss_bytes: None,
                last_trim: None,
            };
            let block = health_json_from(None, false, &memo);
            assert!(block["arenaCount"].is_null(), "{block}");
            assert!(block["arenaCountExceedsCap"].is_null(), "{block}");
            assert!(block["rssMb"].is_null(), "{block}");
            assert!(block["lastTrim"].is_null(), "{block}");
            assert_eq!(block["trim"]["enabled"], false, "{block}");
        }

        /// The only test that drives the process-wide memo and cadence.
        #[test]
        fn one_metrics_tick_publishes_a_sampled_block() {
            fn rss() -> u64 {
                64 * MIB
            }
            on_metrics_tick(rss);
            let block = health_json();
            assert_eq!(block["status"], "sampled", "{block}");
            assert_eq!(block["rssMb"], 64, "{block}");
            assert!(
                block["arenaCount"].as_u64().is_some_and(|count| count >= 1),
                "{block}"
            );
            assert!(
                block["lastTrim"].is_null(),
                "the first tick never trims: {block}"
            );
            assert_eq!(block["trim"]["killSwitchEnv"], TRIM_SWITCH_ENV, "{block}");
        }
    }
}

/// Every platform but linux-gnu: no glibc arenas, so nothing to cap, count or
/// trim, and every surface says `unsupported` rather than serving zeros.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod unsupported {
    use serde_json::{json, Value};
    use tracing::info;

    /// No glibc arenas on this platform, so there is no cap to apply.
    pub fn apply_arena_cap_at_startup() {}

    /// Say once that no cap is attempted, so the absence of an `allocator`
    /// series in the log is explained rather than silent.
    pub fn log_startup_attempt() {
        info!(
            "allocator: this platform's allocator has no glibc arenas, so no arena cap is \
             attempted and no allocator line is logged"
        );
    }

    /// Nothing to sample or trim on this platform.
    pub fn on_metrics_tick(_read_rss_bytes: fn() -> u64) {}

    /// `/health`'s `allocator` block: `unsupported`, never zeros.
    pub fn health_json() -> Value {
        json!({
            "status": "unsupported",
            "reason": "glibc malloc arenas exist only on linux-gnu; this platform's allocator has \
                       none to cap, count or trim",
        })
    }
}
