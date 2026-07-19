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

        // 1) Minimal, instant breadcrumb — better than silence, and enough for
        //    the startup scan to flag `derived_status: errored` this boot.
        let minimal =
            format_harvest_breadcrumb(detected_at_ms, prior, &CrashEvidence::empty());
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
                        format_harvest_breadcrumb(detected_at_ms, prior, &evidence);
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

/// Format the `.dev-logs/crash_*.txt` breadcrumb.
///
/// Reuses the section layout of `logging::write_crash_dump` (`=== PANIC
/// LOCATION ===`, `=== PANIC MESSAGE ===`, `=== THREAD INFO ===`) so the
/// existing [`crate::crash_dumps`] startup scanner parses it and surfaces it
/// on `/health`, while adding a `=== POST-CRASH HARVEST ===` banner that makes
/// the reconstructed-at-next-boot nature explicit.
pub(crate) fn format_harvest_breadcrumb(
    detected_at_ms: i64,
    prior_marker_at_ms: Option<i64>,
    ev: &CrashEvidence,
) -> String {
    let detected = iso_millis(detected_at_ms);
    let prior = prior_marker_at_ms
        .map(iso_millis)
        .unwrap_or_else(|| "n/a (no prior marker)".to_string());

    let exception_code = ev.exception_code.as_deref().unwrap_or("unknown");
    let bucket = ev.fault_bucket.as_deref().unwrap_or("unknown");
    let module = ev
        .faulting_module
        .as_deref()
        .unwrap_or("unknown (WER harvest)");
    let source = ev
        .source
        .as_deref()
        .unwrap_or("Windows Error Reporting / Application event log");
    let event_time = ev.event_time.as_deref().unwrap_or("unknown");

    let panic_message =
        format!("post-crash WER harvest: exception {exception_code} ({bucket}) via {source}");

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
         flipping its shutdown marker to clean:true (crash, OOM kill, taskkill /F,\n\
         or power loss). This artifact was RECONSTRUCTED at the next boot, not\n\
         written live: the 0xc0000409 __fastfail / BEX64 abort family bypasses\n\
         in-process SEH, so the live crash-txt handler cannot fire for it. See\n\
         reference_runner_0xc0000409_is_rust_abort_awslc_jent; the tao\n\
         Rc<EventLoopRunner> data-race root-cause is tracked separately.\n\
         Detected at boot: {detected}\n\
         Prior shutdown marker at: {prior}\n\
         \n\
         === PANIC LOCATION ===\n\
         {module}\n\
         \n\
         === PANIC MESSAGE ===\n\
         {panic_message}\n\
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
        let body = format_harvest_breadcrumb(1_752_000_000_000, Some(1_751_999_000_000), &ev);

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
        let body =
            format_harvest_breadcrumb(1_752_000_000_000, None, &CrashEvidence::empty());
        assert!(body.starts_with("=== QONTINUI RUNNER CRASH DUMP ==="));
        assert!(body.contains("No WER / Application-event-log detail was available"));
        assert!(body.contains("Prior shutdown marker at: n/a (no prior marker)"));
        // Still parseable by the scanner (fallback location + message).
        assert!(body.contains("=== PANIC LOCATION ===\nunknown (WER harvest)"));
        assert!(body.contains("=== PANIC MESSAGE ==="));
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
        assert_eq!(ev.faulting_module.as_deref(), Some("qontinui-runner-primary.exe"));
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
            find_exception_code("Faulting application ..., time stamp: 0x00000000, offset 0x0007a1b2"),
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
