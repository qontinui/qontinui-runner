//! Windows collector: `K32GetPerformanceInfo`, `GetTickCount64`,
//! `NtQueryObject(NULL, ObjectTypesInformation)` and
//! `NtQuerySystemInformation(SystemPoolTagInformation)`.
//!
//! All four are bound through `windows-sys` 0.59 (features `Wdk_Foundation`,
//! `Wdk_System_SystemInformation`, `Win32_System_ProcessStatus`,
//! `Win32_System_SystemInformation`) — never a hand-declared `extern`. A raw
//! extern resolved by symbol name is the construct that produced
//! `0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND` at LOAD time after a green compile
//! (`crates/runner-win32/src/lib.rs`). `wedge_diagnostics.rs`'s hand-declared
//! `NtQuerySystemInformation` is therefore not the template here.
//!
//! The plan records these reads answering as a basic user
//! (`runas /trustlevel:0x20000`) on spaceship on 2026-10-19. That is an
//! operator's observation, not something this file's tests establish: the live
//! test below runs at whatever integrity level the test process has, which on
//! a developer box is usually a filtered admin token.
//!
//! The boot instant is the kernel's own `BootTime`
//! (`NtQuerySystemInformation(SystemTimeOfDayInformation)`), so it is the same
//! value in every process for the life of the boot; `now - GetTickCount64()`
//! is only the fallback when that query fails.
//!
//! The parsing is in [`super::parse`], compiled on every OS so the fixture
//! tests run on Linux CI too; this file only does the reads.

use std::ffi::c_void;

use chrono::{DateTime, TimeDelta, Utc};
use windows_sys::Wdk::Foundation::{NtQueryObject, OBJECT_INFORMATION_CLASS};
use windows_sys::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS,
};
use windows_sys::Win32::Foundation::{NTSTATUS, STATUS_INFO_LENGTH_MISMATCH};
use windows_sys::Win32::System::ProcessStatus::{K32GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

use super::parse::{self, instrument, GaugeReading, ObjectTypeCounts, PerfInfo, PoolTag};
use super::{FailureLog, GaugeSource};

/// `ObjectTypesInformation`. `windows-sys` names classes 0 and 2 only; 3 is
/// the undocumented-but-stable all-types query (see [`parse`]).
const OBJECT_TYPES_INFORMATION: OBJECT_INFORMATION_CLASS = 3;
/// `SystemPoolTagInformation`; not among the classes `windows-sys` names.
const SYSTEM_POOL_TAG_INFORMATION: SYSTEM_INFORMATION_CLASS = 22;
/// `SystemTimeOfDayInformation`, the class `windows-sys` names.
const SYSTEM_TIME_OF_DAY_INFORMATION: SYSTEM_INFORMATION_CLASS =
    windows_sys::Wdk::System::SystemInformation::SystemTimeOfDayInformation;

/// First-try buffer sizes. Measured on degraded spaceship: 10,056 bytes of
/// object types and 135,168 of pool tags (3,379 tags), so one call normally
/// suffices.
const OBJECT_TYPES_INITIAL_BYTES: usize = 16 * 1024;
const POOL_TAGS_INITIAL_BYTES: usize = 256 * 1024;
/// Hard ceiling on a grown buffer: past this the kernel is describing
/// something we have misunderstood, not a pool-tag table.
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;
/// Bounded attempts at the grow-on-`STATUS_INFO_LENGTH_MISMATCH` loop.
const MAX_QUERY_ATTEMPTS: usize = 8;
/// See [`parse::stable_boot_time`].
const BOOT_TIME_TOLERANCE_SECS: i64 = 120;

/// The Windows [`GaugeSource`]. Holds the boot instant so it stays stable
/// across ticks.
#[derive(Debug, Default)]
pub(super) struct WindowsCollector {
    booted_at: Option<DateTime<Utc>>,
}

impl GaugeSource for WindowsCollector {
    fn gauges(&mut self, log: &mut FailureLog) -> GaugeReading {
        let perf = log.record(instrument::GET_PERFORMANCE_INFO, performance_info());
        let objects = log.record(instrument::OBJECT_TYPES, object_types());
        let kernel_boot = log.record("boot-time-of-day", kernel_boot_time());
        // The tick derivation is only computed when the kernel did not answer.
        let tick_derived = if kernel_boot.is_none() {
            boot_time_from_tick_count()
        } else {
            None
        };
        self.booted_at = parse::resolve_boot_time(
            kernel_boot,
            self.booted_at,
            tick_derived,
            BOOT_TIME_TOLERANCE_SECS,
        );
        parse::windows_gauges(perf, objects, self.booted_at)
    }

    fn pool_tags(&mut self, log: &mut FailureLog) -> Option<Vec<PoolTag>> {
        log.record(instrument::POOL_TAGS, pool_tags())
    }
}

/// The kernel's `BootTime`, out of
/// `NtQuerySystemInformation(SystemTimeOfDayInformation)`.
fn kernel_boot_time() -> Result<DateTime<Utc>, String> {
    let buf = nt_query(
        "NtQuerySystemInformation(SystemTimeOfDayInformation)",
        parse::TIME_OF_DAY_BYTES,
        |ptr, len, ret| {
            // SAFETY: `ptr` points to `len` writable, 8-aligned bytes and `ret`
            // to a live u32, both owned by `nt_query` for the duration of the
            // call.
            unsafe { NtQuerySystemInformation(SYSTEM_TIME_OF_DAY_INFORMATION, ptr, len, ret) }
        },
    )?;
    parse::parse_boot_time_of_day(buf.bytes()).ok_or_else(|| {
        format!(
            "SystemTimeOfDayInformation: {} bytes carried no plausible BootTime",
            buf.len
        )
    })
}

/// The fallback: `now - GetTickCount64()`, floored to the whole second.
fn boot_time_from_tick_count() -> Option<DateTime<Utc>> {
    // SAFETY: no arguments, no preconditions; cannot fail.
    let uptime_ms = unsafe { GetTickCount64() };
    let uptime = TimeDelta::try_milliseconds(i64::try_from(uptime_ms).ok()?)?;
    let booted = Utc::now().checked_sub_signed(uptime)?;
    DateTime::from_timestamp(booted.timestamp(), 0)
}

/// `K32GetPerformanceInfo`: kernel paged/nonpaged pages, page size, the
/// system-wide handle count and the live process count.
fn performance_info() -> Result<PerfInfo, String> {
    let cb = u32::try_from(std::mem::size_of::<PERFORMANCE_INFORMATION>())
        .map_err(|_| "PERFORMANCE_INFORMATION does not fit a u32 size".to_string())?;
    // SAFETY: PERFORMANCE_INFORMATION is a plain C struct of integers, for
    // which all-zero is a valid value.
    let mut pi: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
    pi.cb = cb;
    // SAFETY: `pi` is a live, writable PERFORMANCE_INFORMATION of exactly
    // `cb` bytes, which is what the call requires.
    let ok = unsafe { K32GetPerformanceInfo(&mut pi, cb) };
    if ok == 0 {
        return Err(format!(
            "K32GetPerformanceInfo failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let wide = |v: usize| u64::try_from(v).unwrap_or(u64::MAX);
    Ok(PerfInfo {
        kernel_paged_pages: wide(pi.KernelPaged),
        kernel_nonpaged_pages: wide(pi.KernelNonpaged),
        page_size: wide(pi.PageSize),
        handle_count: pi.HandleCount,
        process_count: pi.ProcessCount,
    })
}

/// `NtQueryObject(NULL, ObjectTypesInformation)` → Token / Process counts.
fn object_types() -> Result<ObjectTypeCounts, String> {
    if !cfg!(target_pointer_width = "64") {
        return Err("ObjectTypesInformation: the parser reads the x64 layout only".into());
    }
    let buf = nt_query(
        "NtQueryObject(ObjectTypesInformation)",
        OBJECT_TYPES_INITIAL_BYTES,
        |ptr, len, ret| {
            // SAFETY: a NULL handle with ObjectTypesInformation queries the
            // system-wide type table and touches no handle; `ptr` points to
            // `len` writable, 8-aligned bytes and `ret` to a live u32, both
            // owned by `nt_query` for the duration of the call.
            unsafe {
                NtQueryObject(
                    std::ptr::null_mut(),
                    OBJECT_TYPES_INFORMATION,
                    ptr,
                    len,
                    ret,
                )
            }
        },
    )?;
    parse::object_type_counts(buf.bytes()).ok_or_else(|| {
        format!(
            "ObjectTypesInformation: {} bytes did not parse, or carried no Token entry \
             — published as UNKNOWN",
            buf.len
        )
    })
}

/// `NtQuerySystemInformation(SystemPoolTagInformation)` → the top tags.
fn pool_tags() -> Result<Vec<PoolTag>, String> {
    if !cfg!(target_pointer_width = "64") {
        return Err("SystemPoolTagInformation: the parser reads the x64 layout only".into());
    }
    let buf = nt_query(
        "NtQuerySystemInformation(SystemPoolTagInformation)",
        POOL_TAGS_INITIAL_BYTES,
        |ptr, len, ret| {
            // SAFETY: `ptr` points to `len` writable, 8-aligned bytes and `ret`
            // to a live u32, both owned by `nt_query` for the duration of the
            // call.
            unsafe { NtQuerySystemInformation(SYSTEM_POOL_TAG_INFORMATION, ptr, len, ret) }
        },
    )?;
    parse::parse_pool_tags(buf.bytes())
        .map(|t| t.top)
        .ok_or_else(|| {
            format!(
                "SystemPoolTagInformation: {} bytes did not parse — published as UNKNOWN",
                buf.len
            )
        })
}

/// A query result in an 8-aligned buffer (`Vec<u64>`), of which the first
/// `len` bytes are the answer.
#[derive(Debug)]
struct AlignedBuf {
    words: Vec<u64>,
    len: usize,
}

impl AlignedBuf {
    fn bytes(&self) -> &[u8] {
        let cap = self.words.len().saturating_mul(8);
        // SAFETY: `words` owns `cap` initialized bytes (every `u64` was
        // zero-initialized and then possibly written by the kernel), `len` is
        // clamped to `cap`, and `u8` has alignment 1.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.len.min(cap)) }
    }
}

/// Run an NT query that reports `STATUS_INFO_LENGTH_MISMATCH` for a short
/// buffer, growing an 8-aligned buffer until it fits — at most
/// [`MAX_QUERY_ATTEMPTS`] times and never past [`MAX_QUERY_BYTES`].
fn nt_query(
    what: &str,
    initial_bytes: usize,
    mut call: impl FnMut(*mut c_void, u32, *mut u32) -> NTSTATUS,
) -> Result<AlignedBuf, String> {
    let mut want = initial_bytes.max(8);
    for _ in 0..MAX_QUERY_ATTEMPTS {
        let words = want.div_ceil(8);
        let cap_bytes = words.saturating_mul(8);
        let cap = u32::try_from(cap_bytes)
            .map_err(|_| format!("{what}: {cap_bytes} bytes does not fit a u32 length"))?;
        let mut buf = vec![0u64; words];
        let mut returned: u32 = 0;
        let status = call(buf.as_mut_ptr().cast::<c_void>(), cap, &mut returned);
        if status >= 0 {
            // A success with no (or an impossible) length is the whole buffer;
            // the parser bounds-checks every read either way.
            let len = match usize::try_from(returned) {
                Ok(n) if n > 0 && n <= cap_bytes => n,
                _ => cap_bytes,
            };
            return Ok(AlignedBuf { words: buf, len });
        }
        if status != STATUS_INFO_LENGTH_MISMATCH {
            return Err(format!("{what}: NTSTATUS {:#010x}", status as u32));
        }
        // At least double, and past what the kernel said it needs plus a
        // quarter for a table that grew between the two calls.
        let reported = usize::try_from(returned).unwrap_or(0);
        want = reported
            .saturating_add(reported / 4)
            .max(cap_bytes.saturating_mul(2));
        if want > MAX_QUERY_BYTES {
            return Err(format!(
                "{what}: would need more than {MAX_QUERY_BYTES} bytes — refused"
            ));
        }
    }
    Err(format!(
        "{what}: still STATUS_INFO_LENGTH_MISMATCH after {MAX_QUERY_ATTEMPTS} attempts"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real calls, on the machine running the test: every Windows axis
    /// answers, and the numbers are plausible for a live box. It runs at the
    /// test process's own integrity level, so it does NOT prove the reads work
    /// unelevated (see the module docs).
    #[test]
    fn the_live_windows_reads_answer() {
        let mut log = FailureLog::default();
        let mut c = WindowsCollector::default();
        let g = c.gauges(&mut log);
        assert_eq!(g.instruments, vec!["getperformanceinfo", "objecttypes"]);
        assert!(g.kernel_paged_bytes.is_some_and(|b| b > 0), "{g:?}");
        assert!(g.kernel_nonpaged_bytes.is_some_and(|b| b > 0), "{g:?}");
        assert!(g.kernel_handle_count.is_some_and(|h| h > 0), "{g:?}");
        assert!(g.os_process_count.is_some_and(|p| p > 0), "{g:?}");
        // Live counts, not plausibility bounds against each other: the two
        // reads are separate calls on a box that spawns constantly.
        assert!(g.kernel_token_objects.is_some_and(|t| t > 0), "{g:?}");
        assert!(g.kernel_process_objects.is_some_and(|p| p > 0), "{g:?}");
        let booted = g.os_booted_at.unwrap();
        assert!(booted < Utc::now(), "{booted}");

        let tags = c
            .pool_tags(&mut log)
            .expect("SystemPoolTagInformation answers");
        assert!(!tags.is_empty() && tags.len() <= 8, "{tags:?}");
        assert!(
            tags.windows(2).all(|w| w[0].bytes >= w[1].bytes),
            "{tags:?}"
        );
    }

    /// The boot instant is the kernel's `BootTime`: the same in a FRESH
    /// collector (a restarted runner) as in a long-lived one, and exactly the
    /// value the kernel query returns rather than a per-process derivation.
    #[test]
    fn the_boot_time_is_the_kernels_and_survives_a_restart() {
        let kernel = kernel_boot_time().expect("SystemTimeOfDayInformation answers");
        assert!(kernel < Utc::now(), "{kernel}");
        let mut log = FailureLog::default();
        let mut c = WindowsCollector::default();
        let first = c.gauges(&mut log).os_booted_at;
        assert_eq!(first, Some(kernel));
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        assert_eq!(c.gauges(&mut log).os_booted_at, first);
        // A new collector — what a restarted runner builds — agrees exactly.
        let mut restarted = WindowsCollector::default();
        assert_eq!(
            restarted.gauges(&mut FailureLog::default()).os_booted_at,
            first
        );
    }

    /// The grow loop: a short buffer is retried bigger, a non-mismatch failure
    /// stops at once, and an endless mismatch is bounded.
    #[test]
    fn the_query_buffer_grows_and_is_bounded() {
        let mut sizes = Vec::new();
        let buf = nt_query("grow", 16, |ptr, len, ret| {
            sizes.push(len);
            if len < 100 {
                // SAFETY: `ret` is the live u32 `nt_query` passed.
                unsafe { *ret = 100 };
                return STATUS_INFO_LENGTH_MISMATCH;
            }
            // SAFETY: `ptr` points to `len` >= 100 writable bytes.
            unsafe { *ptr.cast::<u8>() = 7 };
            unsafe { *ret = 100 };
            0
        })
        .unwrap();
        assert_eq!(sizes, vec![16, 128]);
        assert_eq!(buf.len, 100);
        assert_eq!(buf.bytes()[0], 7);
        assert!((buf.words.as_ptr() as usize).is_multiple_of(8), "8-aligned");

        let err = nt_query("denied", 16, |_, _, _| 0xC000_0022_u32 as i32).unwrap_err();
        assert!(err.contains("0xc0000022"), "{err}");

        let mut calls = 0;
        let err = nt_query("endless", 16, |_, _, _| {
            calls += 1;
            STATUS_INFO_LENGTH_MISMATCH
        })
        .unwrap_err();
        assert!(calls <= MAX_QUERY_ATTEMPTS, "{calls}");
        assert!(err.contains("endless"), "{err}");
    }
}
