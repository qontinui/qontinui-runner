//! Crash observability — capture the abort family that WER catches but the
//! in-process crash-txt handler cannot.
//!
//! # Why this exists (Phase 4 of
//! `2026-07-19-runner-session-restore-mass-strand-and-git-popup`)
//!
//! On 2026-07-19 the primary runner died with a Windows `BEX64` / `APPCRASH`
//! (`0xc0000409` / `STATUS_STACK_BUFFER_OVERRUN`) — the tao
//! `Rc<EventLoopRunner>` event-loop abort family (see
//! `reference_runner_0xc0000409_is_rust_abort_awslc_jent`). The runner's OWN
//! crash-txt handler did NOT fire: `0xc0000409` is a `__fastfail` /
//! stack-cookie abort that deliberately bypasses SEH, so no live in-process
//! catch (neither a `std::panic` hook nor `SetUnhandledExceptionFilter`) is
//! ever invoked for it. Only Windows Error Reporting recorded the crash, so
//! `.dev-logs` looked like a clean stop and diagnosis needed Event Viewer.
//!
//! This module closes the observability gap in two complementary ways:
//!
//! 1. **Boot-time harvest** ([`harvest_boot_crash_evidence`]) — the robust
//!    path for the fastfail family. Since the abort bypasses every live
//!    catch, we reconstruct the evidence at the NEXT boot: when the prior
//!    shutdown was unclean (the existing [`crate::session::shutdown_marker`]
//!    signal already tells us this), we query the Windows Application event
//!    log / WER for the most-recent `qontinui-runner*` crash and write a
//!    `.dev-logs/crash_<unix-ms>.txt` breadcrumb. `.dev-logs` is no longer
//!    silent about this abort family, and the existing
//!    [`crate::crash_dumps`] startup scanner surfaces it on `/health`.
//!
//! 2. **Live crash writer** ([`install_live_crash_writer`]) — best-effort
//!    capture of the DELIVERABLE subset. The `std::panic` hooks installed in
//!    `main` already cover unwinding panics; on Windows this additionally
//!    installs a `SetUnhandledExceptionFilter` for deliverable structured
//!    exceptions (access violations, etc.). **It does NOT — and cannot —
//!    catch `0xc0000409`**; the boot-harvest above is what covers that family.
//!
//! ## Scope note (deliberate)
//!
//! This is the OBSERVABILITY half of Phase 4 only. Root-causing the tao
//! `Rc<EventLoopRunner>` data race that produces the `0xc0000409` abort is a
//! SEPARATE deep investigation tracked under the crash-family reference
//! `reference_runner_0xc0000409_is_rust_abort_awslc_jent`; this module does
//! not touch windowing / event-loop code.
//!
//! ## Fail-open contract
//!
//! Every path here is best-effort: a failed event-log query, a write error,
//! or a non-Windows host degrades to a minimal breadcrumb (or none) and NEVER
//! blocks boot or panics.

use std::path::Path;
#[cfg(windows)]
use std::time::Duration;

use tracing::{info, warn};

use crate::session::shutdown_marker::BootClassification;

/// Hard bound on the Windows event-log query so a hung `powershell` can never
/// stall the (background) harvest thread indefinitely.
#[cfg(windows)]
const WER_QUERY_TIMEOUT: Duration = Duration::from_secs(8);

/// Cap on the raw WER/event message we embed, so a pathological multi-KB
/// event body can't bloat the breadcrumb. Used by the event-log parse path
/// (Windows) and its tests.
#[cfg(any(windows, test))]
const RAW_SNIPPET_MAX: usize = 1800;

/// Parsed crash evidence gathered from Windows (WER / Application event log).
/// All fields optional — a partial or empty set still produces a useful
/// breadcrumb.
#[derive(Debug, Default, Clone)]
pub(crate) struct CrashEvidence {
    /// Exception code, e.g. `0xc0000409`.
    pub exception_code: Option<String>,
    /// Fault bucket / event name, e.g. `BEX64`, `APPCRASH`.
    pub fault_bucket: Option<String>,
    /// Faulting module or application name, e.g. `qontinui-runner-primary.exe`.
    pub faulting_module: Option<String>,
    /// Event source, e.g. `Application Error`, `Windows Error Reporting`.
    pub source: Option<String>,
    /// The event's `TimeCreated`, ISO-8601 if available.
    pub event_time: Option<String>,
    /// A trimmed snippet of the raw event message for triage.
    pub raw_snippet: Option<String>,
}

impl CrashEvidence {
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.exception_code.is_none()
            && self.fault_bucket.is_none()
            && self.faulting_module.is_none()
            && self.source.is_none()
            && self.event_time.is_none()
            && self.raw_snippet.is_none()
    }
}

/// Whether this boot should harvest crash evidence.
///
/// Harvest ONLY when the prior shutdown was unclean AND a prior marker
/// actually existed. `crash_recovery` is `true` for the absent-marker
/// first-ever-boot case too (the safe default in `shutdown_marker`), but that
/// is NOT a crash — requiring `prior_marker_at.is_some()` means a real prior
/// process ran and never flipped its marker clean (crash / OOM kill /
/// `taskkill /F` / power loss). This gate prevents writing a phantom crash
/// artifact on a brand-new install.
pub(crate) fn should_harvest(boot: BootClassification) -> bool {
    boot.crash_recovery && boot.prior_marker_at.is_some()
}

/// Boot-time crash harvest. Call once, early in boot, AFTER the shutdown
/// marker has been classified. No-op unless [`should_harvest`] holds.
///
/// Writes a minimal breadcrumb SYNCHRONOUSLY (so `.dev-logs` is instantly
/// non-silent and the startup crash scan can surface it this boot), then
/// enriches it with WER / event-log detail in a BACKGROUND thread so the
/// (bounded) event-log query never blocks boot. Both writes target the same
/// `crash_<unix-ms>.txt` path.
pub fn harvest_boot_crash_evidence(boot: BootClassification) {
    // Whole body wrapped so nothing here can ever panic the boot thread.
    let _ = std::panic::catch_unwind(move || {
        if !should_harvest(boot) {
            return;
        }

        let detected_at_ms = chrono::Utc::now().timestamp_millis();
        let prior = boot.prior_marker_at;
        let dir = crate::logging::get_crash_dump_dir();
        let path = dir.join(format!("crash_{detected_at_ms}.txt"));

        // The prior run's own last words, if it left any: an `alloc_failure`
        // line written from inside its failing allocator, or the opening line
        // of a commit/resource exhaustion episode it died inside. Read
        // synchronously — one bounded tail read of a local file — so even the
        // minimal breadcrumb names the cause.
        let incidents =
            crate::health_monitor::wedge_incidents_path(&crate::paths::get_dev_logs_dir());
        let exhaustion = find_prior_exhaustion(
            &read_incident_tail(&incidents),
            prior.unwrap_or(0),
            detected_at_ms,
            std::process::id(),
        );

        // 1) Minimal, instant breadcrumb — better than silence, and enough for
        //    the startup scan to flag `derived_status: errored` this boot.
        let minimal =
            format_harvest_breadcrumb(detected_at_ms, prior, &CrashEvidence::empty(), &exhaustion);
        if let Err(e) = write_breadcrumb(&path, &minimal) {
            warn!(
                error = %e,
                path = %path.display(),
                "crash harvest: failed to write breadcrumb — degrading to silence this boot"
            );
            return;
        }
        info!(
            path = %path.display(),
            exhaustion_breadcrumbs = exhaustion.len(),
            "post-crash boot harvest: prior shutdown was unclean — wrote crash breadcrumb"
        );

        // 2) Enrich with WER detail off the boot path. The gather is bounded
        //    and fail-open; if it finds nothing the minimal breadcrumb stands.
        let spawn = std::thread::Builder::new()
            .name("crash-harvest".into())
            .spawn(move || {
                let _ = std::panic::catch_unwind(move || {
                    let evidence = gather_windows_crash_evidence();
                    if evidence.is_empty() {
                        return;
                    }
                    let enriched =
                        format_harvest_breadcrumb(detected_at_ms, prior, &evidence, &exhaustion);
                    if write_breadcrumb(&path, &enriched).is_ok() {
                        info!(
                            path = %path.display(),
                            code = evidence.exception_code.as_deref().unwrap_or("unknown"),
                            bucket = evidence.fault_bucket.as_deref().unwrap_or("unknown"),
                            "post-crash boot harvest: enriched breadcrumb with WER/event-log detail"
                        );
                    }
                });
            });
        if let Err(e) = spawn {
            warn!(error = %e, "crash harvest: could not spawn enrichment thread (minimal breadcrumb stands)");
        }
    });
}

/// Install the best-effort live crash writer.
///
/// Also pre-opens `wedge-incidents.log` for the allocation-failure breadcrumb
/// (`qontinui_runner_lib::alloc_breadcrumb`) — the one live writer that DOES
/// run before a `handle_alloc_error` abort, because it sits in the global
/// allocator rather than in a hook the abort bypasses.
///
/// The `std::panic` hooks installed in `main` (`startup_panic` +
/// `logging::setup_panic_handler`) already write a `crash_*.txt` for the
/// CATCHABLE unwinding-panic subset on every platform. On Windows this
/// additionally installs a `SetUnhandledExceptionFilter` for the deliverable
/// structured-exception subset (access violations, etc.).
///
/// **Honest limitation:** neither hook catches `0xc0000409` / `BEX64`
/// (`__fastfail` bypasses SEH). The boot-harvest ([`harvest_boot_crash_evidence`])
/// is the path that covers that abort family.
pub fn install_live_crash_writer() {
    // The allocation-failure breadcrumb's handle, opened NOW because its
    // writer runs inside a failing allocator and can open nothing then; and
    // the live memory probe the spawn classifier's edge event reads. Plan
    // `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-git-spawns-are-ungated`
    // Phase 0.
    let incidents = crate::health_monitor::wedge_incidents_path(&crate::paths::get_dev_logs_dir());
    if let Err(e) = qontinui_runner_lib::alloc_breadcrumb::install(&incidents) {
        warn!(
            error = %e,
            path = %incidents.display(),
            "alloc breadcrumb: could not pre-open wedge-incidents.log — an allocation-failure \
             abort will leave no alloc_failure line this run"
        );
    }
    qontinui_runner_lib::util::resource_exhaustion::register_memory_reader(
        crate::fleet::resource_sample::exhaustion_memory_reading,
    );
    qontinui_runner_lib::util::resource_exhaustion::prewarm_stderr_needles();

    #[cfg(windows)]
    win_seh::install();
    #[cfg(not(windows))]
    {
        // No structured-exception mechanism off Windows. The std::panic hooks
        // installed in `main` already cover unwinding panics cross-platform.
    }
}

/// Best-effort atomic-ish write of the breadcrumb body. Creates the parent
/// directory; overwrites any existing file at `path` (the enrichment pass
/// rewrites the synchronous minimal breadcrumb in place).
fn write_breadcrumb(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

// ---------------------------------------------------------------------------
// The prior run's exhaustion breadcrumbs (wedge-incidents.log)
// ---------------------------------------------------------------------------

/// `wedge-incidents.log` tokens that name a resource exhaustion, most specific
/// first. `alloc_failure` is written by the allocator wrapper when an
/// allocation returns null (which a graceful `try_reserve` also does, so the
/// line alone is not proof of the abort); the other four open a spawn-failure
/// episode (`util::resource_exhaustion::incident_token`), and each episode's
/// close is the same token + `_closed`. Plan
/// `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-git-spawns-are-ungated`
/// Phase 0 item 5.
const EXHAUSTION_TOKENS: [&str; 5] = [
    "alloc_failure",
    "commit_exhaustion",
    "commit_exhaustion_suspected",
    "resource_exhaustion",
    "resource_exhaustion_suspected",
];

/// How much of the incident log's tail the harvest reads. The file is
/// append-only and never rotated; the prior run's lines are at its end, and
/// 64 KiB is ~150 incident lines — far more than one run writes.
const INCIDENT_TAIL_BYTES: u64 = 64 * 1024;

/// At most this many exhaustion lines are carried into the crash file.
const MAX_EXHAUSTION_LINES: usize = 8;

/// One exhaustion line the prior run left in `wedge-incidents.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PriorExhaustion {
    /// One of [`EXHAUSTION_TOKENS`].
    pub token: &'static str,
    /// The line's own timestamp, unix ms.
    pub at_ms: i64,
    /// The line verbatim (trimmed) — it carries the memory reading.
    pub line: String,
    /// Whether this was the prior run's LAST incident line of any kind inside
    /// the window — the only position from which an `alloc_failure` may be
    /// named as the likely cause of the unclean exit rather than merely as
    /// something that happened during the run.
    pub last_in_window: bool,
}

/// Read the last [`INCIDENT_TAIL_BYTES`] of the incident log, lossily. Empty
/// on any failure: a missing file is the ordinary case (nothing ever written),
/// and the harvest must never fail over it.
fn read_incident_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > INCIDENT_TAIL_BYTES && f.seek(SeekFrom::Start(len - INCIDENT_TAIL_BYTES)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The exhaustion lines the DEAD run left inside `[since_ms, until_ms]` — from
/// its boot (the shutdown marker's `at`) to this boot's detection — that were
/// still OPEN when it died. PURE, so the attribution is settleable in a test.
///
/// Three filters, each for a way the window lies:
///
/// - **Time.** A line outside the window belongs to an EARLIER run: the file
///   is never truncated, so an old `alloc_failure` would otherwise name every
///   later unrelated death.
/// - **Process.** Every writer ends its line `(pid N)`. Lines from `own_pid`
///   (THIS boot — `install_live_crash_writer` runs before the harvest, so this
///   process can already have written lines) are dropped. The window can also
///   hold lines from OTHER live processes sharing the dev-logs path, so
///   pairing and "last line" are judged only among the lines of ONE pid: the
///   pid of the last in-window line that HAS a pid — the process that stopped
///   writing last.
/// - **Unattributable lines are barriers.** A timestamped in-window line with
///   no parseable pid (a garbled line, or one from a build whose writer
///   predates the trailing pid) is never an entry — it cannot be attributed —
///   but it does prove something was written after what precedes it, so it
///   demotes the "last line" claim of every earlier entry.
/// - **Closure.** An episode whose `<token>_closed` line (same `kind=`, same
///   pid) follows its opening line had ENDED before the process died.
///
/// Documented limits: the shutdown marker records no pid, so "the dead run's
/// pid" is inferred from the last pid-bearing line — if another live process
/// sharing the path wrote last, its pid is chosen, and nothing here probes
/// whether that pid is still alive. A pid the OS reused between the two runs
/// is indistinguishable from the dead run's.
pub(crate) fn find_prior_exhaustion(
    log: &str,
    since_ms: i64,
    until_ms: i64,
    own_pid: u32,
) -> Vec<PriorExhaustion> {
    // Every timestamped in-window line, with its pid where one parses; this
    // boot's own lines are gone.
    let candidates: Vec<(&str, &str, i64, Option<u32>)> = log
        .lines()
        .filter_map(|raw| {
            let line = raw.trim();
            let mut parts = line.splitn(3, ' ');
            let ts = parts.next()?;
            let token = parts.next()?;
            let at_ms = chrono::DateTime::parse_from_rfc3339(ts)
                .ok()?
                .timestamp_millis();
            let pid = line_pid(line);
            ((since_ms..=until_ms).contains(&at_ms) && pid != Some(own_pid))
                .then_some((line, token, at_ms, pid))
        })
        .collect();
    let Some(dead_pid) = candidates.iter().rev().find_map(|&(_, _, _, pid)| pid) else {
        return Vec::new();
    };
    // The dead run's lines plus the unattributable barriers, in order; other
    // processes' lines say nothing about the dead run.
    let dead_lines: Vec<_> = candidates
        .into_iter()
        .filter(|&(_, _, _, pid)| pid.is_none() || pid == Some(dead_pid))
        .collect();
    let last_seq = dead_lines.len().checked_sub(1);

    // (sequence number among the dead run's lines, entry)
    let mut found: Vec<(usize, PriorExhaustion)> = Vec::new();
    for (seq, &(line, token, at_ms, pid)) in dead_lines.iter().enumerate() {
        if pid.is_none() {
            // A barrier: counted for "last line", never an entry or a close.
            continue;
        }
        if let Some(open) = EXHAUSTION_TOKENS.iter().copied().find(|t| *t == token) {
            found.push((
                seq,
                PriorExhaustion {
                    token: open,
                    at_ms,
                    line: line.to_string(),
                    last_in_window: false,
                },
            ));
        } else if let Some(base) = token.strip_suffix("_closed") {
            // Close the latest open episode with the same token and kind.
            let kind = kind_field(line);
            if let Some(pos) = found
                .iter()
                .rposition(|(_, e)| e.token == base && kind_field(&e.line) == kind)
            {
                found.remove(pos);
            }
        }
    }
    let mut found: Vec<PriorExhaustion> = found
        .into_iter()
        .map(|(seq, mut e)| {
            e.last_in_window = Some(seq) == last_seq;
            e
        })
        .collect();
    // The latest lines win.
    if found.len() > MAX_EXHAUSTION_LINES {
        found.drain(..found.len() - MAX_EXHAUSTION_LINES);
    }
    found
}

/// The `N` of an incident line's trailing `(pid N)`.
fn line_pid(line: &str) -> Option<u32> {
    line.strip_suffix(')')?.rsplit_once("(pid ")?.1.parse().ok()
}

/// The `kind=` field of an incident line, which pairs an episode's opening
/// line with its `_closed` line.
fn kind_field(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("kind="))
}

/// The line the crash file is headlined by:
///
/// 1. an `alloc_failure` that is the dead run's LAST line — the likely cause;
/// 2. else whatever IS the last line (an episode still open at death) — it
///    outranks an earlier `alloc_failure`, which a graceful `try_reserve`
///    failure also writes and the process may have survived;
/// 3. else the latest line of the first [`EXHAUSTION_TOKENS`] entry present.
fn headline_exhaustion(found: &[PriorExhaustion]) -> Option<&PriorExhaustion> {
    found.iter().find(|e| e.last_in_window).or_else(|| {
        EXHAUSTION_TOKENS
            .iter()
            .find_map(|t| found.iter().rev().find(|e| e.token == *t))
    })
}

/// The text after `<timestamp> <token> `, i.e. what the line says.
fn exhaustion_detail(e: &PriorExhaustion) -> &str {
    e.line.splitn(3, ' ').nth(2).unwrap_or("")
}

// Placeholder `source` / `faulting_module` for a breadcrumb written when no
// platform crash-evidence store was consulted.
//
// Windows has WER and the Application event log; nothing else does. Keeping
// these platform-selected stops a non-Windows breadcrumb from naming a
// subsystem that cannot exist on the host that wrote it. Deliberately plain
// `//` rather than `///`: a doc comment would attach to the `#[cfg(windows)]`
// arm only, so the explanation would disappear on exactly the platform it is
// about.
#[cfg(windows)]
const NO_EVIDENCE_SOURCE: &str = "Windows Error Reporting / Application event log";
#[cfg(not(windows))]
const NO_EVIDENCE_SOURCE: &str = "no platform crash-evidence source on this OS";

#[cfg(windows)]
const NO_EVIDENCE_MODULE: &str = "unknown (WER harvest)";
#[cfg(not(windows))]
const NO_EVIDENCE_MODULE: &str = "unknown (no OS crash-evidence source)";

/// Format the `.dev-logs/crash_*.txt` breadcrumb.
///
/// Reuses the section layout of `logging::write_crash_dump` (`=== PANIC
/// LOCATION ===`, `=== PANIC MESSAGE ===`, `=== THREAD INFO ===`) so the
/// existing [`crate::crash_dumps`] startup scanner parses it and surfaces it
/// on `/health`, while adding a `=== POST-CRASH HARVEST ===` banner that makes
/// the reconstructed-at-next-boot nature explicit.
///
/// `exhaustion` is what [`find_prior_exhaustion`] recovered from the prior
/// run's `wedge-incidents.log` lines. When it is non-empty the PANIC LOCATION
/// and PANIC MESSAGE name the exhaustion (and the message carries the memory
/// reading) — those two lines are what `/health` surfaces, and before this the
/// only thing they could say about an allocation-failure abort was
/// `unknown (WER harvest)`.
pub(crate) fn format_harvest_breadcrumb(
    detected_at_ms: i64,
    prior_marker_at_ms: Option<i64>,
    ev: &CrashEvidence,
    exhaustion: &[PriorExhaustion],
) -> String {
    let detected = iso_millis(detected_at_ms);
    let prior = prior_marker_at_ms
        .map(iso_millis)
        .unwrap_or_else(|| "n/a (no prior marker)".to_string());

    let exception_code = ev.exception_code.as_deref().unwrap_or("unknown");
    let bucket = ev.fault_bucket.as_deref().unwrap_or("unknown");
    let module = ev.faulting_module.as_deref().unwrap_or(NO_EVIDENCE_MODULE);
    let source = ev.source.as_deref().unwrap_or(NO_EVIDENCE_SOURCE);
    let event_time = ev.event_time.as_deref().unwrap_or("unknown");

    // The headline has to stay honest about WHICH platform produced it. WER
    // exists only on Windows, so on every other host the enrichment gather is a
    // compiled-out no-op (`gather_windows_crash_evidence` returns `empty()`) and
    // the breadcrumb is a pure unclean-shutdown marker. Labelling that
    // "via Windows Error Reporting" on Linux invented a source that cannot
    // exist, and because `Restart=always` re-runs the harvest on every boot, a
    // crash-looping unit fills `.dev-logs/` with dumps naming a Windows
    // subsystem — 26 of them on `merytshost` before this was noticed. The
    // `detail` block below already said "or ran on a non-Windows host"; the
    // headline that the `/health` scanner surfaces did not.
    let panic_message = if ev.is_empty() {
        format!("post-crash harvest: prior shutdown was unclean, no detail available ({source})")
    } else {
        format!("post-crash WER harvest: exception {exception_code} ({bucket}) via {source}")
    };

    // The prior run's own breadcrumb outranks every inference from the exit
    // code: it was written by the dying process about the thing that killed
    // it. The WER detail (when present) stays in its own section below.
    //
    // It never says "aborted on" by itself: an `alloc_failure` line is also
    // written by a graceful (`try_reserve`) failure the process survived. Only
    // when it is the run's LAST incident line is it named as the likely cause.
    let (location, panic_message) = match headline_exhaustion(exhaustion) {
        Some(e) if e.token == "alloc_failure" && e.last_in_window => (
            "commit exhaustion — an allocation failure was the prior run's last incident line \
             (alloc_failure breadcrumb, wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: the prior run's LAST recorded incident was an ALLOCATION \
                 FAILURE (commit exhaustion) — the likely cause of the unclean exit: {}",
                exhaustion_detail(e)
            ),
        ),
        Some(e) if e.token == "alloc_failure" => (
            "commit exhaustion — an allocation failure was recorded during the prior run \
             (alloc_failure breadcrumb, wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: an ALLOCATION FAILURE (commit exhaustion) was recorded \
                 during the prior run, though not as its last incident line: {}",
                exhaustion_detail(e)
            ),
        ),
        Some(e) if e.token == "commit_exhaustion" => (
            "commit exhaustion — the prior run died inside an open commit-exhaustion episode \
             (commit_exhaustion breadcrumb, wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: prior shutdown was unclean while COMMIT EXHAUSTION was \
                 being reported: {}",
                exhaustion_detail(e)
            ),
        ),
        Some(e) if e.token == "commit_exhaustion_suspected" => (
            "commit exhaustion (suspected) — the prior run died inside an open episode \
             evidenced only by child stderr text (commit_exhaustion_suspected breadcrumb, \
             wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: prior shutdown was unclean while commit exhaustion was \
                 SUSPECTED (from a failed child's stderr): {}",
                exhaustion_detail(e)
            ),
        ),
        Some(e) if e.token == "resource_exhaustion_suspected" => (
            "resource exhaustion (suspected) — the prior run died inside an open episode \
             evidenced only by child stderr text (resource_exhaustion_suspected breadcrumb, \
             wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: prior shutdown was unclean while resource exhaustion was \
                 SUSPECTED (from a failed child's stderr): {}",
                exhaustion_detail(e)
            ),
        ),
        Some(e) => (
            "resource exhaustion — the prior run died inside an open exhaustion episode \
             (resource_exhaustion breadcrumb, wedge-incidents.log)"
                .to_string(),
            format!(
                "post-crash harvest: prior shutdown was unclean while RESOURCE EXHAUSTION was \
                 being reported: {}",
                exhaustion_detail(e)
            ),
        ),
        None => (module.to_string(), panic_message),
    };
    let exhaustion_section = if exhaustion.is_empty() {
        "none attributed — no alloc_failure line and no still-open commit_exhaustion / \
         commit_exhaustion_suspected / resource_exhaustion / resource_exhaustion_suspected \
         episode in wedge-incidents.log could be attributed to the prior run. That is not \
         proof it wrote none: lines with no parseable (pid N), lines from this boot and \
         lines from other processes are never attributed, and a closed episode is not listed"
            .to_string()
    } else {
        exhaustion
            .iter()
            .map(|e| e.line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let detail = if ev.is_empty() {
        "No WER / Application-event-log detail was available at harvest time \
         (query returned nothing, timed out, or ran on a non-Windows host). \
         This breadcrumb still records that the prior shutdown was unclean."
            .to_string()
    } else {
        format!(
            "Source: {source}\n\
             Exception code: {exception_code}\n\
             Fault bucket: {bucket}\n\
             Faulting module: {module}\n\
             Event time: {event_time}\n\
             Raw:\n{raw}",
            raw = ev.raw_snippet.as_deref().unwrap_or("<none>"),
        )
    };

    format!(
        "=== QONTINUI RUNNER CRASH DUMP ===\n\
         Timestamp: {detected}\n\
         Version: {version}\n\
         Kind: post-crash-boot-harvest\n\
         \n\
         === POST-CRASH HARVEST ===\n\
         Prior shutdown was UNCLEAN — the previous runner process died without\n\
         flipping its shutdown marker to clean:true (crash, allocation-failure\n\
         abort, OOM kill, taskkill /F, or power loss). This artifact was\n\
         RECONSTRUCTED at the next boot, not written live: an abort through\n\
         __fastfail (0xc0000409 / BEX64) bypasses in-process SEH, so the live\n\
         crash-txt handler cannot fire for it. That exit code does NOT name a\n\
         cause: a Rust allocation failure (handle_alloc_error, e.g. under commit\n\
         exhaustion) aborts the same way as the tao Rc<EventLoopRunner> data race\n\
         (reference_runner_0xc0000409_is_rust_abort_awslc_jent). The prior run's\n\
         own alloc_failure / commit_exhaustion lines in wedge-incidents.log, when\n\
         present, do name it — see RESOURCE EXHAUSTION BREADCRUMBS below.\n\
         Detected at boot: {detected}\n\
         Prior shutdown marker at: {prior}\n\
         \n\
         === PANIC LOCATION ===\n\
         {location}\n\
         \n\
         === PANIC MESSAGE ===\n\
         {panic_message}\n\
         \n\
         === RESOURCE EXHAUSTION BREADCRUMBS ===\n\
         {exhaustion_section}\n\
         \n\
         === WER / EVENT LOG DETAIL ===\n\
         {detail}\n\
         \n\
         === THREAD INFO ===\n\
         n/a (reconstructed at next boot; not a live thread)\n\
         \n\
         === END CRASH DUMP ===\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// Format a unix-millis instant as ISO-8601 UTC; falls back to the raw millis
/// on the (impossible in practice) out-of-range case.
fn iso_millis(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| format!("{ms} (unix ms)"))
}

// ---------------------------------------------------------------------------
// Windows event-log gather (bounded, fail-open)
// ---------------------------------------------------------------------------

/// Gather the most-recent `qontinui-runner*` crash evidence from the Windows
/// Application event log (Application Error + Windows Error Reporting
/// providers). Returns [`CrashEvidence::empty`] on any failure or on
/// non-Windows hosts.
#[cfg(windows)]
fn gather_windows_crash_evidence() -> CrashEvidence {
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW — the runner is a GUI (windows) subsystem app; without
    // this flag spawning console-subsystem powershell.exe would flash a
    // console window during a crash-recovery boot.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // Most-recent Application Error / WER event mentioning qontinui-runner.
    // -MaxEvents caps the scan; SilentlyContinue keeps it from erroring when
    // the providers or matching events are absent.
    let script = "\
        $ErrorActionPreference='SilentlyContinue';\
        $e = Get-WinEvent -FilterHashtable @{LogName='Application'; \
             ProviderName='Application Error','Windows Error Reporting'} \
             -MaxEvents 80 | \
             Where-Object { $_.Message -match 'qontinui-runner' } | \
             Select-Object -First 1;\
        if ($e) {\
          Write-Output ('TimeCreated=' + $e.TimeCreated.ToString('o'));\
          Write-Output ('Provider=' + $e.ProviderName);\
          Write-Output '---MESSAGE---';\
          Write-Output $e.Message;\
        }";

    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NonInteractive",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .creation_flags(CREATE_NO_WINDOW);

    match run_bounded(cmd, WER_QUERY_TIMEOUT) {
        Some(out) => parse_windows_event_output(&out),
        None => CrashEvidence::empty(),
    }
}

#[cfg(not(windows))]
fn gather_windows_crash_evidence() -> CrashEvidence {
    // No WER / Application event log off Windows — the harvest keeps its
    // minimal breadcrumb.
    CrashEvidence::empty()
}

/// Run a child process with a hard timeout, returning its captured stdout.
/// On timeout the child is killed and whatever was read so far is returned.
/// `None` only when the child could not be spawned.
#[cfg(windows)]
fn run_bounded(mut cmd: std::process::Command, timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::time::Instant;

    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }

    // The reader returns once the pipe closes (process exit / kill).
    Some(reader.join().unwrap_or_default())
}

/// Parse the powershell gather output into [`CrashEvidence`]. Pure string
/// work — unit-tested cross-platform with synthetic event bodies.
#[cfg(any(windows, test))]
pub(crate) fn parse_windows_event_output(out: &str) -> CrashEvidence {
    let out = out.trim();
    if out.is_empty() {
        return CrashEvidence::empty();
    }

    // Header lines (before the `---MESSAGE---` fence) carry TimeCreated /
    // Provider; everything after is the raw event message.
    let (header, message) = match out.split_once("---MESSAGE---") {
        Some((h, m)) => (h, m.trim()),
        None => (out, ""),
    };

    let event_time = header_value(header, "TimeCreated=");
    let source = header_value(header, "Provider=");

    let exception_code = find_exception_code(message);
    let fault_bucket = find_fault_bucket(message);
    let faulting_module = find_faulting_module(message);

    let raw_snippet = if message.is_empty() {
        None
    } else {
        Some(truncate(message, RAW_SNIPPET_MAX))
    };

    let ev = CrashEvidence {
        exception_code,
        fault_bucket,
        faulting_module,
        source,
        event_time,
        raw_snippet,
    };

    // If the fence was absent and we extracted nothing, treat as empty so the
    // minimal breadcrumb stands rather than embedding noise.
    if ev.is_empty() {
        CrashEvidence::empty()
    } else {
        ev
    }
}

/// First `key=value` header line's value (from the `TimeCreated=`/`Provider=`
/// preamble the gather script emits).
#[cfg(any(windows, test))]
fn header_value(header: &str, key: &str) -> Option<String> {
    header.lines().find_map(|l| {
        l.trim()
            .strip_prefix(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Value following a `Field name:` label anywhere in the message (e.g.
/// `Exception code: 0xc0000409`).
#[cfg(any(windows, test))]
fn labelled_value(message: &str, label: &str) -> Option<String> {
    message.lines().find_map(|l| {
        l.trim()
            .strip_prefix(label)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Exception code: prefer the explicit `Exception code:` label, else scan for
/// a bare `0x????????` 8-hex token (the WER problem-signature form).
#[cfg(any(windows, test))]
fn find_exception_code(message: &str) -> Option<String> {
    if let Some(v) = labelled_value(message, "Exception code:") {
        // An explicit label is authoritative — take it verbatim.
        return Some(normalize_hex(&v).unwrap_or(v));
    }
    // No label — scan bare `0x…` tokens, but accept ONLY error-severity codes.
    // A crash exception code (STATUS_STACK_BUFFER_OVERRUN 0xc0000409, access
    // violation 0xc0000005, …) is an NTSTATUS with the severity field set to
    // ERROR, i.e. the top hex nibble is >= 0x8. This rejects the many benign
    // 8-hex `0x…` tokens WER messages carry — `time stamp: 0x00000000`, offsets,
    // fault-offset 0x0000… — which would otherwise be reported as a bogus
    // exception code and mislead triage.
    message
        .split(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .filter_map(normalize_hex)
        .find(|code| is_error_severity_hex(code))
}

/// True iff a normalized `0x????????` code has its NTSTATUS severity field set
/// to error/warning (top nibble >= 0x8) — the shape of a real fault code.
#[cfg(any(windows, test))]
fn is_error_severity_hex(code: &str) -> bool {
    code.strip_prefix("0x")
        .and_then(|h| h.chars().next())
        .and_then(|c| c.to_digit(16))
        .is_some_and(|nibble| nibble >= 0x8)
}

/// Normalize a `0x????????` (8 hex digits) token to lowercase `0x…`. `None`
/// when the token isn't an 8-digit hex literal.
#[cfg(any(windows, test))]
fn normalize_hex(tok: &str) -> Option<String> {
    let tok = tok.trim_matches(|c: char| c == '"' || c == '(' || c == ')');
    let hex = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X"))?;
    if hex.len() == 8 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("0x{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

/// Fault bucket / event name — known crash-family tokens first, else the WER
/// `Fault bucket ...` / `Event Name:` line.
#[cfg(any(windows, test))]
fn find_fault_bucket(message: &str) -> Option<String> {
    for token in ["BEX64", "BEX", "APPCRASH", "APPHANG", "CLR20r3"] {
        if message.contains(token) {
            return Some(token.to_string());
        }
    }
    labelled_value(message, "Event Name:")
}

/// Faulting module (preferred) or application name, trimmed to the leading
/// segment before any `, version=` / `,` suffix.
#[cfg(any(windows, test))]
fn find_faulting_module(message: &str) -> Option<String> {
    let raw = labelled_value(message, "Faulting module name:")
        .or_else(|| labelled_value(message, "Faulting application name:"))?;
    let head = raw.split(',').next().unwrap_or(&raw).trim();
    if head.is_empty() {
        None
    } else {
        Some(head.to_string())
    }
}

/// Truncate `s` to at most `max` chars on a char boundary, appending an
/// ellipsis marker when truncated.
#[cfg(any(windows, test))]
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" …[truncated]");
    out
}

// ---------------------------------------------------------------------------
// Windows live SEH crash writer
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win_seh {
    //! Best-effort `SetUnhandledExceptionFilter` for the DELIVERABLE
    //! structured-exception subset. This exists purely to add a live catch for
    //! exceptions that ARE routed through SEH (access violations, etc.).
    //!
    //! It intentionally does NOT try to catch `0xc0000409` /
    //! `STATUS_STACK_BUFFER_OVERRUN` and its `__fastfail` siblings — those
    //! terminate the process without ever invoking a top-level filter. The
    //! boot-harvest in the parent module is the path that captures that
    //! family.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    };

    /// `EXCEPTION_CONTINUE_SEARCH` — after we write our artifact, let the OS
    /// continue default processing (WER) when there is no prior filter to
    /// chain to.
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    type Filter = unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32;
    type OptFilter = Option<Filter>;

    /// Previously-installed top-level filter (e.g. Sentry/crashpad in release
    /// builds), stored as a `usize` so we can CHAIN to it rather than clobber
    /// it. `Option<fn>` has the null-pointer niche, so `None` transmutes to
    /// `0` and a real filter to its address.
    static PREV_FILTER: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn install() {
        // SAFETY: `SetUnhandledExceptionFilter` is always safe to call; it
        // swaps the process-wide top-level filter and returns the prior one.
        // Transmuting `Option<fn>` ↔ `usize` is sound: both are pointer-sized
        // and the fn-pointer null niche maps `None` ↔ `0`.
        unsafe {
            let prev: OptFilter = SetUnhandledExceptionFilter(Some(top_level_filter));
            PREV_FILTER.store(
                std::mem::transmute::<OptFilter, usize>(prev),
                Ordering::SeqCst,
            );
        }
    }

    /// The installed top-level exception filter. Runs on the faulting thread
    /// for a DELIVERABLE structured exception (the process is about to die).
    unsafe extern "system" fn top_level_filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // Wrap in catch_unwind so a fault while writing can't escalate to a
        // recursive abort. Best-effort only.
        let _ = std::panic::catch_unwind(|| {
            // SAFETY: `info` is the OS-provided pointer for this fault; the
            // reader tolerates null. This runs inside a fresh closure, which
            // is not an unsafe context even though the outer fn is `unsafe`.
            let (code, addr) = unsafe { read_exception(info) };
            let message = format!(
                "unhandled structured exception {code:#010x} at {addr:?} \
                 (deliverable SEH; NOT a __fastfail — 0xc0000409/BEX64 bypasses this filter)"
            );
            crate::logging::write_crash_dump(
                "SetUnhandledExceptionFilter (Windows SEH)",
                &message,
                &format!("{}", std::backtrace::Backtrace::force_capture()),
            );
        });

        // Chain to a previously-installed filter if any, else continue search
        // so WER still records / terminates as usual. SAFETY: the stored usize
        // came from transmuting a valid `Option<Filter>` in `install`.
        let prev: OptFilter =
            std::mem::transmute::<usize, OptFilter>(PREV_FILTER.load(Ordering::SeqCst));
        match prev {
            Some(f) => f(info),
            None => EXCEPTION_CONTINUE_SEARCH,
        }
    }

    /// Read the exception code + faulting address from `EXCEPTION_POINTERS`,
    /// tolerating null pointers.
    unsafe fn read_exception(info: *const EXCEPTION_POINTERS) -> (u32, *mut core::ffi::c_void) {
        if info.is_null() {
            return (0, core::ptr::null_mut());
        }
        let rec = (*info).ExceptionRecord;
        if rec.is_null() {
            return (0, core::ptr::null_mut());
        }
        ((*rec).ExceptionCode as u32, (*rec).ExceptionAddress)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::shutdown_marker::BootClassification;

    fn boot(crash_recovery: bool, prior_marker_at: Option<i64>) -> BootClassification {
        BootClassification {
            crash_recovery,
            prior_marker_at,
        }
    }

    #[test]
    fn should_harvest_only_on_unclean_boot_with_prior_marker() {
        // Genuine crash: unclean + a prior marker existed.
        assert!(should_harvest(boot(true, Some(1_000))));
        // Clean planned restart: never harvest.
        assert!(!should_harvest(boot(false, Some(1_000))));
        // First-ever boot: unclean (safe default) but NO prior marker — not a
        // crash, must not write a phantom artifact.
        assert!(!should_harvest(boot(true, None)));
        // Clean + no marker (shouldn't occur, but must not harvest).
        assert!(!should_harvest(boot(false, None)));
    }

    #[test]
    fn breadcrumb_with_evidence_has_scanner_sections_and_detail() {
        let ev = CrashEvidence {
            exception_code: Some("0xc0000409".into()),
            fault_bucket: Some("BEX64".into()),
            faulting_module: Some("qontinui-runner-primary.exe".into()),
            source: Some("Application Error".into()),
            event_time: Some("2026-07-19T00:20:14.000Z".into()),
            raw_snippet: Some("Faulting application name: qontinui-runner-primary.exe".into()),
        };
        let body = format_harvest_breadcrumb(1_752_000_000_000, Some(1_751_999_000_000), &ev, &[]);

        // Sections the existing crash_dumps scanner keys on must be present so
        // it can surface this on /health.
        assert!(body.contains("=== PANIC LOCATION ==="));
        assert!(body.contains("=== PANIC MESSAGE ==="));
        assert!(body.contains("=== THREAD INFO ==="));
        // Harvest-specific banner + the WER detail.
        assert!(body.contains("=== POST-CRASH HARVEST ==="));
        assert!(body.contains("Kind: post-crash-boot-harvest"));
        assert!(body.contains("0xc0000409"));
        assert!(body.contains("BEX64"));
        assert!(body.contains("qontinui-runner-primary.exe"));
        // The faulting module is the parseable PANIC LOCATION line.
        assert!(body.contains("=== PANIC LOCATION ===\nqontinui-runner-primary.exe"));
    }

    #[test]
    fn breadcrumb_with_empty_evidence_is_still_a_valid_crash_dump() {
        let body = format_harvest_breadcrumb(1_752_000_000_000, None, &CrashEvidence::empty(), &[]);
        assert!(body.starts_with("=== QONTINUI RUNNER CRASH DUMP ==="));
        assert!(body.contains("No WER / Application-event-log detail was available"));
        assert!(body.contains("Prior shutdown marker at: n/a (no prior marker)"));
        // Still parseable by the scanner (fallback location + message).
        assert!(body.contains(&format!("=== PANIC LOCATION ===\n{NO_EVIDENCE_MODULE}")));
        assert!(body.contains("=== PANIC MESSAGE ==="));
    }

    /// A no-evidence breadcrumb must not name a crash-evidence subsystem the
    /// host does not have. Off Windows there is no WER and no Application event
    /// log, so `gather_windows_crash_evidence` is a compiled-out no-op and the
    /// breadcrumb is a pure unclean-shutdown marker.
    #[test]
    fn empty_evidence_headline_does_not_invent_a_windows_source() {
        let body = format_harvest_breadcrumb(1_752_000_000_000, None, &CrashEvidence::empty(), &[]);
        assert!(
            body.contains("post-crash harvest: prior shutdown was unclean"),
            "empty evidence should use the neutral headline, got:\n{body}"
        );

        #[cfg(not(windows))]
        {
            assert!(
                !body.contains("Windows Error Reporting"),
                "non-Windows breadcrumb must not cite WER, got:\n{body}"
            );
            assert!(
                !body.contains("post-crash WER harvest"),
                "non-Windows breadcrumb must not claim a WER harvest, got:\n{body}"
            );
        }
    }

    /// Real evidence only ever comes from the Windows gather, so the WER
    /// headline stays exactly as it was when there is something to report.
    #[test]
    fn populated_evidence_keeps_the_wer_headline() {
        let ev = CrashEvidence {
            exception_code: Some("0xc0000409".into()),
            fault_bucket: Some("BEX64".into()),
            faulting_module: Some("qontinui-runner-primary.exe".into()),
            source: Some("Application Error".into()),
            event_time: Some("2026-07-19T00:20:14.000Z".into()),
            raw_snippet: Some("Faulting application name: qontinui-runner-primary.exe".into()),
        };
        let body = format_harvest_breadcrumb(1_752_000_000_000, None, &ev, &[]);
        assert!(body.contains("post-crash WER harvest: exception 0xc0000409 (BEX64)"));
    }

    /// The prior run's `wedge-incidents.log`, as the MSI box would have left
    /// it at abort #4: an unrelated line from an EARLIER run, the opening of a
    /// commit-exhaustion episode, and the allocator's last line.
    fn incident_log() -> String {
        [
            "2026-09-22T20:01:30.000+00:00 alloc_failure memory allocation of 1048576 bytes \
             failed (alloc, align 8, thread 1) — no memory reading was cached; … (pid 11)",
            "2026-09-23T01:20:00.123456789+00:00 backend_wedged runner backend wedged (pid 22)",
            "2026-09-23T01:24:58.000000000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"worktree_census: git\" — free_commit 1048576 bytes, commit_limit \
             76996083712 bytes, free_phys 3556704256 bytes (reading 0s old) (pid 22)",
            "2026-09-23T01:25:01.441+00:00 alloc_failure memory allocation of 2097152 bytes \
             failed (alloc, align 8, thread 4242) — last memory reading 3s old: free_commit \
             524288 bytes, commit_limit 76996083712 bytes, free_phys 3473344000 bytes; unless \
             the caller used a fallible API (try_reserve) the default alloc error handler \
             aborts this process next (pid 22)",
            "not a timestamp alloc_failure garbage",
        ]
        .join("\n")
    }

    /// 2026-09-23T00:00:00Z (the prior run's boot) and 01:30:00Z (this boot).
    const PRIOR_BOOT_MS: i64 = 1_790_121_600_000;
    const THIS_BOOT_MS: i64 = 1_790_127_000_000;
    /// The harvesting process's own pid in these tests.
    const THIS_PID: u32 = 99;

    /// Only the prior run's exhaustion lines are attributed: the earlier run's
    /// `alloc_failure`, the wedge line and the unparseable line are not.
    #[test]
    fn find_prior_exhaustion_keeps_only_this_runs_exhaustion_lines() {
        let found = find_prior_exhaustion(&incident_log(), PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let tokens: Vec<_> = found.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec!["commit_exhaustion", "alloc_failure"]);
        assert!(found[1].line.contains("2097152 bytes"));
        assert!(find_prior_exhaustion("", PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID).is_empty());
    }

    /// Plan `2026-09-23-resource-guard-floors-are-constants-and-the-runners-
    /// own-git-spawns-are-ungated` Phase 0 verification (c): given the prior
    /// run's `alloc_failure` breadcrumb, the crash file's PANIC LOCATION and
    /// PANIC MESSAGE name commit exhaustion and carry the reading — not
    /// `unknown (WER harvest)`.
    #[test]
    fn an_alloc_failure_breadcrumb_names_commit_exhaustion_in_the_crash_file() {
        let found = find_prior_exhaustion(&incident_log(), PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let body = format_harvest_breadcrumb(
            THIS_BOOT_MS,
            Some(PRIOR_BOOT_MS),
            &CrashEvidence::empty(),
            &found,
        );
        // Read back through the /health scanner's own section parser.
        let location = crate::crash_dumps::extract_section(&body, "=== PANIC LOCATION ===")
            .expect("a PANIC LOCATION line");
        assert!(location.starts_with("commit exhaustion"), "{location}");
        assert!(!body.contains(NO_EVIDENCE_MODULE), "{body}");
        // The scanner surfaces the FIRST line only, so the reading must be on it.
        let message = crate::crash_dumps::extract_section(&body, "=== PANIC MESSAGE ===")
            .expect("a PANIC MESSAGE line");
        assert!(
            message.contains("LAST recorded incident was an ALLOCATION FAILURE"),
            "{message}"
        );
        assert!(message.contains("2097152 bytes"), "{message}");
        assert!(
            message.contains("free_commit 524288 bytes"),
            "the reading: {message}"
        );
        // Both of the prior run's lines are carried verbatim.
        assert!(body.contains("=== RESOURCE EXHAUSTION BREADCRUMBS ===\n2026-09-23T01:24:58"));
    }

    /// An episode line alone (no allocator line) still names commit
    /// exhaustion — as the condition the run died in, not as the cause.
    #[test]
    fn a_commit_exhaustion_episode_alone_is_named_as_the_condition() {
        let found = vec![PriorExhaustion {
            token: "commit_exhaustion",
            at_ms: PRIOR_BOOT_MS + 1,
            line: "2026-09-23T00:00:00.001+00:00 commit_exhaustion kind=commit os_code=1455 \
                   caller=\"git_trunk: git\" — free_commit 1 bytes (pid 9)"
                .to_string(),
            last_in_window: true,
        }];
        let body = format_harvest_breadcrumb(
            THIS_BOOT_MS,
            Some(PRIOR_BOOT_MS),
            &CrashEvidence::empty(),
            &found,
        );
        assert!(body.contains("=== PANIC LOCATION ===\ncommit exhaustion"));
        assert!(body.contains("while COMMIT EXHAUSTION was being reported: kind=commit"));
    }

    /// An episode whose `_closed` line follows it had ENDED before the
    /// process died, so it is not attributed; one of another kind stays.
    #[test]
    fn a_closed_episode_is_not_attributed() {
        let log = [
            "2026-09-23T00:10:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"a\" — x (pid 1)",
            "2026-09-23T00:11:00.000+00:00 resource_exhaustion kind=task_limit os_code=11 \
             caller=\"b\" — x (pid 1)",
            "2026-09-23T00:20:00.000+00:00 commit_exhaustion_closed kind=commit \
             suppressed_repeats=4 duration_ms=100 ended_by=spawn_succeeded (pid 1)",
        ]
        .join("\n");
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let tokens: Vec<_> = found.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec!["resource_exhaustion"]);
        // Not the last line (the close is), so nothing claims last position.
        assert!(!found[0].last_in_window);
    }

    /// An `alloc_failure` that is NOT the run's last incident line (a graceful
    /// `try_reserve` failure the process survived) is reported as recorded,
    /// never as the cause.
    #[test]
    fn an_alloc_failure_that_is_not_last_is_only_recorded() {
        let log = [
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation of 64 bytes \
             failed (alloc, align 8, thread 1) — x (pid 1)",
            "2026-09-23T00:30:00.000+00:00 backend_wedged runner backend wedged (pid 1)",
        ]
        .join("\n");
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        assert_eq!(found.len(), 1);
        assert!(!found[0].last_in_window);
        let body = format_harvest_breadcrumb(
            THIS_BOOT_MS,
            Some(PRIOR_BOOT_MS),
            &CrashEvidence::empty(),
            &found,
        );
        let message = crate::crash_dumps::extract_section(&body, "=== PANIC MESSAGE ===").unwrap();
        assert!(
            message.contains("was recorded during the prior run"),
            "{message}"
        );
        assert!(!message.contains("likely cause"), "{message}");
        assert!(!body.contains("aborted on"), "{body}");
    }

    /// A stderr-only episode keeps "suspected" all the way into the crash
    /// file's PANIC LOCATION.
    #[test]
    fn a_suspected_episode_stays_suspected_in_the_crash_file() {
        let log = "2026-09-23T00:10:00.000+00:00 commit_exhaustion_suspected \
                   kind=commit_exhaustion_suspected os_code=1455 caller=\"c\" — x (pid 1)";
        let found = find_prior_exhaustion(log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        assert_eq!(found[0].token, "commit_exhaustion_suspected");
        let body = format_harvest_breadcrumb(
            THIS_BOOT_MS,
            Some(PRIOR_BOOT_MS),
            &CrashEvidence::empty(),
            &found,
        );
        let location =
            crate::crash_dumps::extract_section(&body, "=== PANIC LOCATION ===").unwrap();
        assert!(
            location.starts_with("commit exhaustion (suspected)"),
            "{location}"
        );
    }

    /// THIS boot's own lines (written before the harvest runs) neither demote
    /// the dead run's `alloc_failure` from last position nor count as its
    /// incidents.
    #[test]
    fn this_boots_own_lines_do_not_demote_the_dead_runs_alloc_failure() {
        let log = format!(
            "{}\n2026-09-23T01:29:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"worktree_census: git\" — x (pid {THIS_PID})",
            incident_log()
        );
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let tokens: Vec<_> = found.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec!["commit_exhaustion", "alloc_failure"]);
        assert!(
            found[1].last_in_window,
            "the dead run's last line is still last"
        );
        assert!(found.iter().all(|e| e.line.ends_with("(pid 22)")));
    }

    /// Another live process's `_closed` line does not close the dead run's
    /// episode, and its lines are not the dead run's.
    #[test]
    fn a_foreign_pids_close_does_not_close_the_dead_runs_episode() {
        let log = [
            "2026-09-23T00:10:00.000+00:00 resource_exhaustion kind=task_limit os_code=11 \
             caller=\"a\" — x (pid 7)",
            "2026-09-23T00:10:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"a\" — x (pid 22)",
            "2026-09-23T00:20:00.000+00:00 commit_exhaustion_closed kind=commit \
             suppressed_repeats=1 duration_ms=1 ended_by=spawn_succeeded (pid 7)",
            "2026-09-23T00:21:00.000+00:00 backend_wedged runner backend wedged (pid 22)",
        ]
        .join("\n");
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let tokens: Vec<_> = found.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec!["commit_exhaustion"], "{found:?}");
        assert!(found[0].line.ends_with("(pid 22)"));
    }

    /// A still-open episode that is the dead run's last line outranks an
    /// earlier `alloc_failure` (which a surviving `try_reserve` also writes).
    #[test]
    fn a_last_open_episode_outranks_an_earlier_alloc_failure() {
        let log = [
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation of 64 bytes \
             failed (alloc, align 8, thread 1) — x (pid 22)",
            "2026-09-23T00:30:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"a\" — x (pid 22)",
        ]
        .join("\n");
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        assert_eq!(
            headline_exhaustion(&found).unwrap().token,
            "commit_exhaustion"
        );
    }

    /// The watchdog's line (and any line with no parseable pid) written after
    /// a survived `alloc_failure` proves the run went on: the allocation
    /// failure is NOT the likely cause.
    #[test]
    fn a_watchdog_line_after_an_alloc_failure_demotes_it() {
        for watchdog in [
            // Current format: trailing pid, same run.
            "2026-09-23T00:40:00.000+00:00 WATCHDOG backend_silent — pid 22, probe heartbeat \
             90s old. Written by the runtime-independent watchdog thread. (pid 22)",
            // Old format: no trailing pid — an unattributable barrier.
            "2026-09-23T00:40:00.000+00:00 WATCHDOG backend_silent — pid 22, probe heartbeat \
             90s old. Written by the runtime-independent watchdog thread.",
        ] {
            let log = format!(
                "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation of 64 bytes \
                 failed (alloc, align 8, thread 1) — x (pid 22)\n{watchdog}"
            );
            let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
            assert_eq!(found.len(), 1);
            assert!(!found[0].last_in_window, "{watchdog}");
            let body = format_harvest_breadcrumb(
                THIS_BOOT_MS,
                Some(PRIOR_BOOT_MS),
                &CrashEvidence::empty(),
                &found,
            );
            assert!(!body.contains("likely cause"), "{body}");
        }
    }

    /// A garbled or missing `(pid N)` is never attributed — even on an
    /// exhaustion token — and a lone such line yields nothing.
    #[test]
    fn a_line_without_a_parseable_pid_is_never_attributed() {
        for bad in [
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation of 64 bytes failed",
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation (pid twelve)",
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation (pid 12",
        ] {
            assert!(
                find_prior_exhaustion(bad, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID).is_empty(),
                "{bad}"
            );
        }
        assert_eq!(line_pid("x (pid 12)"), Some(12));
        assert_eq!(line_pid("x (pid 12) trailing"), None);
    }

    /// When the dead run's last line is a `_closed` line, its episode is gone
    /// and the earlier `alloc_failure` is not last.
    #[test]
    fn a_closed_line_as_the_last_line_closes_and_demotes() {
        let log = [
            "2026-09-23T00:10:00.000+00:00 alloc_failure memory allocation of 64 bytes \
             failed (alloc, align 8, thread 1) — x (pid 22)",
            "2026-09-23T00:20:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
             caller=\"a\" — x (pid 22)",
            "2026-09-23T00:30:00.000+00:00 commit_exhaustion_closed kind=commit \
             suppressed_repeats=0 duration_ms=0 ended_by=spawn_succeeded (pid 22)",
        ]
        .join("\n");
        let found = find_prior_exhaustion(&log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        let tokens: Vec<_> = found.iter().map(|e| e.token).collect();
        assert_eq!(tokens, vec!["alloc_failure"]);
        assert!(!found[0].last_in_window);
    }

    /// A caller label that itself contains `(pid` does not confuse the pid
    /// parse: only the TRAILING `(pid N)` counts.
    #[test]
    fn a_caller_label_containing_pid_text_does_not_confuse_attribution() {
        let log = "2026-09-23T00:10:00.000+00:00 commit_exhaustion kind=commit os_code=1455 \
                   caller=\"probe (pid 7) helper\" — x (pid 22)";
        let found = find_prior_exhaustion(log, PRIOR_BOOT_MS, THIS_BOOT_MS, THIS_PID);
        assert_eq!(found.len(), 1);
        assert!(found[0].last_in_window);
        assert_eq!(line_pid(log), Some(22));
    }

    /// The boilerplate no longer implies the tao data race for every
    /// 0xc0000409: it names the allocation-failure member of the family and
    /// says the exit code alone names no cause.
    #[test]
    fn the_boilerplate_does_not_pin_every_fastfail_on_tao() {
        let body = format_harvest_breadcrumb(THIS_BOOT_MS, None, &CrashEvidence::empty(), &[]);
        assert!(body.contains("That exit code does NOT name a"));
        assert!(body.contains("handle_alloc_error"));
        assert!(!body.contains("root-cause is tracked separately"));
        assert!(body.contains("=== RESOURCE EXHAUSTION BREADCRUMBS ===\nnone attributed"));
        assert!(!body.contains("left no"));
        assert!(!body.contains("aborted on"));
    }

    #[test]
    fn parse_application_error_event() {
        // Shape of a Windows "Application Error" (Event ID 1000) message.
        let out = "TimeCreated=2026-07-19T00:20:14.1234567+00:00\n\
                   Provider=Application Error\n\
                   ---MESSAGE---\n\
                   Faulting application name: qontinui-runner-primary.exe, version: 1.0.3.0, time stamp: 0x00000000\n\
                   Faulting module name: qontinui-runner-primary.exe, version: 1.0.3.0, time stamp: 0x00000000\n\
                   Exception code: 0xc0000409\n\
                   Fault offset: 0x0000000001abcdef\n\
                   Faulting process id: 0x1a2b\n";
        let ev = parse_windows_event_output(out);
        assert_eq!(ev.exception_code.as_deref(), Some("0xc0000409"));
        assert_eq!(
            ev.faulting_module.as_deref(),
            Some("qontinui-runner-primary.exe")
        );
        assert_eq!(ev.source.as_deref(), Some("Application Error"));
        assert_eq!(
            ev.event_time.as_deref(),
            Some("2026-07-19T00:20:14.1234567+00:00")
        );
        assert!(!ev.is_empty());
    }

    #[test]
    fn parse_wer_bex64_bucket_event() {
        // Shape of a "Windows Error Reporting" (Event ID 1001) message.
        let out = "TimeCreated=2026-07-19T00:20:15.000+00:00\n\
                   Provider=Windows Error Reporting\n\
                   ---MESSAGE---\n\
                   Fault bucket 129873, type 5\n\
                   Event Name: BEX64\n\
                   Response: Not available\n\
                   P1: qontinui-runner-primary.exe\n\
                   P2: 1.0.3.0\n\
                   P8: c0000409\n";
        let ev = parse_windows_event_output(out);
        assert_eq!(ev.fault_bucket.as_deref(), Some("BEX64"));
        assert_eq!(ev.source.as_deref(), Some("Windows Error Reporting"));
        // No `Exception code:` label and `c0000409` lacks a `0x` prefix → no
        // 8-hex token, so exception_code is legitimately absent here.
        assert!(ev.exception_code.is_none());
        assert!(!ev.is_empty());
    }

    #[test]
    fn parse_empty_output_is_empty_evidence() {
        assert!(parse_windows_event_output("").is_empty());
        assert!(parse_windows_event_output("   \n  ").is_empty());
    }

    #[test]
    fn find_exception_code_scans_bare_hex_token() {
        assert_eq!(
            find_exception_code("Problem signature P8: 0xC0000409 more"),
            Some("0xc0000409".to_string())
        );
        // A short hex is not an exception code.
        assert_eq!(find_exception_code("value 0x1234 here"), None);
        // A benign 8-hex token (no `Exception code:` label) must NOT be reported
        // as the exception code — it has success/info severity (top nibble < 8).
        assert_eq!(
            find_exception_code(
                "Faulting application ..., time stamp: 0x00000000, offset 0x0007a1b2"
            ),
            None,
            "a low-severity 0x00000000 timestamp is not a fault code"
        );
        // But an explicit label is authoritative even for an all-zero value.
        assert_eq!(
            find_exception_code("Exception code: 0x00000000"),
            Some("0x00000000".to_string())
        );
        // A real access-violation code found bare is accepted (severity 0xc).
        assert_eq!(
            find_exception_code("... 0xc0000005 ..."),
            Some("0xc0000005".to_string())
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "héllo wörld";
        let t = truncate(s, 4);
        assert!(t.starts_with("héll"));
        assert!(t.contains("[truncated]"));
        // No truncation when short enough.
        assert_eq!(truncate("abc", 10), "abc");
    }
}
