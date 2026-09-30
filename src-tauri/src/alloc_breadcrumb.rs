//! The allocation-failure breadcrumb: a `#[global_allocator]` wrapper that
//! writes ONE line to `wedge-incidents.log` when an allocation returns null,
//! before the default alloc error handler aborts the process.
//!
//! # Why it exists
//!
//! Plan `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-
//! git-spawns-are-ungated`, Phase 0 items 4-5. The primary runner on the MSI
//! box died four times in three days (intervals 24 h, 26 h, 5.4 h) with
//! `memory allocation of 2097152 bytes failed` as its last words — and left no
//! durable record at all. A Rust allocation failure does not panic: it runs
//! the default handler, prints that one line to stderr and calls `abort()`, so
//! the panic hook, the crash dump, `runner-panic.log`, `wedge-incidents.log`
//! and `wedge-diagnostics.jsonl` were all bypassed, and the next boot's
//! harvest wrote `unknown (WER harvest)` beside a boilerplate naming an
//! unrelated tao data race.
//!
//! # Why a global allocator and not `set_alloc_error_hook`
//!
//! `std::alloc::set_alloc_error_hook` is nightly-only
//! (`#![feature(alloc_error_hook)]`) and the runner builds on stable `1.95.0`
//! (vet 2026-09-30, defect #1). A thin [`GlobalAlloc`] over
//! [`std::alloc::System`] is the stable seam that sees the null return before
//! `handle_alloc_error` runs — and it also sees fallible `try_reserve`
//! failures the hook never would.
//!
//! # The rules the failure arm lives by
//!
//! It runs inside the allocator of a process that is out of memory, so:
//!
//! - **It allocates nothing.** The line is formatted into a stack buffer by
//!   hand — no `format!`, no `String`, no `chrono`, no `std::thread::current()`
//!   (which can allocate an `Arc` for an unnamed thread).
//! - **It takes no lock.** The file handle is opened at startup
//!   ([`install`]) and kept as a raw descriptor in an atomic; the write is one
//!   raw OS call (`write(2)` / `WriteFile`) — no `std::fs::File` buffering, no
//!   tracing, no stderr lock the dying handler is about to take.
//! - **It writes once.** An atomic flag per wrapper: a failure storm (every
//!   thread failing at once) writes one line, not a thousand.
//! - **It changes nothing.** The null is returned unchanged, so the default
//!   handler prints its line and aborts exactly as before. The wrapper
//!   observes; it never alters the outcome.
//!
//! This no-allocation rule is new here, not inherited: `wedge_diagnostics`
//! forbids tracing, the async runtime and WMI, but itself allocates. And the
//! success path is a single null check on the returned pointer — no counter,
//! no sampling — so the wrapper is free when nothing fails.
//!
//! # The line
//!
//! `wedge-incidents.log`'s existing format (`health_monitor::
//! append_wedge_incident`): `<RFC 3339 UTC> <token> <detail> (pid N)`, with the
//! stable token `alloc_failure`. Only the FORMAT is shared —
//! `append_wedge_incident` allocates (`format!`, `chrono`) and must never be
//! called from here. [`append_incident`] is the allocating twin for ordinary
//! code in the lib crate (the spawn classifier's `commit_exhaustion` lines),
//! routed through the same pre-opened handle.
//!
//! # Why the lib crate
//!
//! The wrapper type and the handle live here so both the allocator (registered
//! in the runner bin's `main.rs`) and `util::resource_exhaustion` (lib) reach
//! ONE handle. Only the runner binary registers it; the lib's other binaries
//! and every test binary keep the plain system allocator, and the wrapper is
//! tested by instantiating it over a stub inner allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::util::resource_exhaustion::{last_memory_reading, MemoryReading};

/// The stable `wedge-incidents.log` token for an allocation failure.
pub const ALLOC_FAILURE_TOKEN: &str = "alloc_failure";

// ---------------------------------------------------------------------------
// The pre-opened handle
// ---------------------------------------------------------------------------

#[cfg(unix)]
static INCIDENT_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
#[cfg(windows)]
static INCIDENT_HANDLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Open `path` (the runner's `wedge-incidents.log`) for append and keep the
/// raw handle for the life of the process. Call once, early in startup; a
/// second call is a no-op. Best-effort: a failure leaves the breadcrumb
/// unwired and returns the error for the caller's log.
///
/// Append mode on both platforms (`O_APPEND` / `FILE_APPEND_DATA`), so a line
/// from here and a line from `append_wedge_incident`'s own short-lived handle
/// interleave whole rather than overwrite each other.
pub fn install(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        let fd = file.into_raw_fd();
        if INCIDENT_FD
            .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Already installed: close the duplicate rather than leak it.
            // SAFETY: `fd` came from `into_raw_fd` just above and is owned here.
            drop(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        let handle = file.into_raw_handle() as usize;
        if INCIDENT_HANDLE
            .compare_exchange(0, handle, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // SAFETY: `handle` came from `into_raw_handle` just above.
            drop(unsafe {
                <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(handle as _)
            });
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        drop(file);
    }
    Ok(())
}

/// Write `bytes` through the pre-opened handle with one raw OS call. No
/// allocation, no lock; a no-op before [`install`]. Errors are swallowed: the
/// process is already sick and a failed breadcrumb must never make it worse.
fn raw_write(bytes: &[u8]) {
    #[cfg(unix)]
    {
        let fd = INCIDENT_FD.load(Ordering::Acquire);
        if fd < 0 {
            return;
        }
        let mut rest = bytes;
        // Bounded: a regular-file O_APPEND write of a few hundred bytes is
        // whole in practice; the loop only covers a signal-interrupted write.
        for _ in 0..4 {
            if rest.is_empty() {
                return;
            }
            // SAFETY: `fd` is the descriptor `install` leaked for the life of
            // the process; `rest` is a valid readable slice.
            let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
            if n <= 0 {
                return;
            }
            rest = &rest[n as usize..];
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        let handle = INCIDENT_HANDLE.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut written: u32 = 0;
        // SAFETY: `handle` is the file handle `install` leaked for the life of
        // the process; the buffer is valid for `len` bytes; no OVERLAPPED.
        unsafe {
            WriteFile(
                handle as _,
                bytes.as_ptr(),
                bytes.len().min(u32::MAX as usize) as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = bytes;
    }
}

/// Append one `wedge-incidents.log` line (`<RFC 3339> <token> <detail> (pid
/// N)`) through the pre-opened handle. ALLOCATES — for ordinary code only
/// (the spawn classifier's episode lines), never from the allocator.
pub fn append_incident(token: &str, detail: &str) {
    let line = format!(
        "{} {} {} (pid {})\n",
        chrono::Utc::now().to_rfc3339(),
        token,
        detail,
        std::process::id()
    );
    raw_write(line.as_bytes());
}

// ---------------------------------------------------------------------------
// The wrapper
// ---------------------------------------------------------------------------

/// Where the failure arm's one line goes. Abstracted so the wrapper can be
/// tested over a capturing sink; production is [`IncidentFileSink`].
pub trait BreadcrumbSink: Sync {
    /// Write one pre-formatted line. Called at most once per wrapper, from
    /// inside a failing allocator: implementations must not allocate or lock
    /// in production.
    fn write_line(&self, line: &[u8]);
}

/// The production sink: [`raw_write`] to the handle [`install`] opened.
pub struct IncidentFileSink;

impl BreadcrumbSink for IncidentFileSink {
    fn write_line(&self, line: &[u8]) {
        raw_write(line);
    }
}

/// A [`GlobalAlloc`] that forwards every call to `inner` and, the first time
/// an allocation returns null, writes one breadcrumb line to `sink`.
///
/// Generic over the inner allocator and the sink so the failure arm is
/// testable without exhausting a real machine.
pub struct BreadcrumbAlloc<A, S> {
    inner: A,
    sink: S,
    fired: AtomicBool,
}

/// The runner binary's global allocator type.
pub type RunnerAlloc = BreadcrumbAlloc<System, IncidentFileSink>;

impl RunnerAlloc {
    /// The value `main.rs` registers with `#[global_allocator]`.
    pub const fn runner() -> Self {
        BreadcrumbAlloc::new(System, IncidentFileSink)
    }
}

impl<A, S> BreadcrumbAlloc<A, S> {
    pub const fn new(inner: A, sink: S) -> Self {
        BreadcrumbAlloc {
            inner,
            sink,
            fired: AtomicBool::new(false),
        }
    }
}

/// Which `GlobalAlloc` entry point failed.
#[derive(Debug, Clone, Copy)]
enum AllocOp {
    Alloc,
    AllocZeroed,
    Realloc,
}

impl AllocOp {
    fn as_str(self) -> &'static str {
        match self {
            AllocOp::Alloc => "alloc",
            AllocOp::AllocZeroed => "alloc_zeroed",
            AllocOp::Realloc => "realloc",
        }
    }
}

impl<A, S: BreadcrumbSink> BreadcrumbAlloc<A, S> {
    /// The failure arm. `#[cold]` + `#[inline(never)]` so the success path
    /// compiles to the null check and a not-taken branch, nothing more.
    #[cold]
    #[inline(never)]
    fn on_null(&self, op: AllocOp, size: usize, align: usize) {
        if self.fired.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut line = LineBuf::new();
        format_alloc_failure_line(
            &mut line,
            unix_now(),
            op,
            size,
            align,
            current_thread_id(),
            last_memory_reading(),
            std::process::id(),
        );
        self.sink.write_line(line.as_bytes());
    }
}

// SAFETY: every method forwards to `inner` with the caller's arguments
// unchanged and returns its result unchanged; the only addition is a side
// effect on the null path that neither allocates nor touches the pointer.
unsafe impl<A: GlobalAlloc, S: BreadcrumbSink> GlobalAlloc for BreadcrumbAlloc<A, S> {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = self.inner.alloc(layout);
        if p.is_null() {
            self.on_null(AllocOp::Alloc, layout.size(), layout.align());
        }
        p
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = self.inner.alloc_zeroed(layout);
        if p.is_null() {
            self.on_null(AllocOp::AllocZeroed, layout.size(), layout.align());
        }
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.dealloc(ptr, layout)
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = self.inner.realloc(ptr, layout, new_size);
        if p.is_null() {
            self.on_null(AllocOp::Realloc, new_size, layout.align());
        }
        p
    }
}

// ---------------------------------------------------------------------------
// Allocation-free formatting
// ---------------------------------------------------------------------------

/// Capacity of the line buffer. The longest line this writes is ~420 bytes;
/// anything past the capacity is dropped rather than overflowing.
const LINE_CAP: usize = 640;

/// A fixed stack buffer with just enough formatting for one line.
struct LineBuf {
    buf: [u8; LINE_CAP],
    len: usize,
}

impl LineBuf {
    const fn new() -> Self {
        LineBuf {
            buf: [0; LINE_CAP],
            len: 0,
        }
    }

    fn push(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len == LINE_CAP {
                return;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    fn push_u64(&mut self, mut n: u64) {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for &d in &digits[i..] {
            if self.len == LINE_CAP {
                return;
            }
            self.buf[self.len] = d;
            self.len += 1;
        }
    }

    /// `n` zero-padded to `width` digits (for date/time fields).
    fn push_padded(&mut self, n: u64, width: usize) {
        let mut tmp = LineBuf::new();
        tmp.push_u64(n);
        for _ in tmp.len..width {
            self.push("0");
        }
        self.push(tmp.as_str_lossless());
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Everything pushed is ASCII, so this never fails; an empty string is
    /// returned rather than a panic if that ever stops being true.
    fn as_str_lossless(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

/// `(unix seconds, milliseconds within the second)`, allocation-free.
fn unix_now() -> (u64, u32) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs(), d.subsec_millis())
}

/// The calling thread's OS id, without `std::thread::current()` (which may
/// allocate the handle of an unnamed thread).
fn current_thread_id() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY: no preconditions.
        u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid has no preconditions and cannot fail.
        unsafe { libc::syscall(libc::SYS_gettid) as u64 }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: no preconditions.
        unsafe { libc::pthread_self() as u64 }
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

/// Civil (year, month, day) from days since 1970-01-01 — Howard Hinnant's
/// `civil_from_days`, integer-only.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDTHH:MM:SS.mmm+00:00` — RFC 3339 UTC, the shape
/// `chrono::Utc::now().to_rfc3339()` produces for the other lines in the file
/// (at millisecond rather than nanosecond precision), so the harvest parses
/// both with one parser.
fn push_rfc3339(line: &mut LineBuf, (secs, millis): (u64, u32)) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    line.push_padded(y.max(0) as u64, 4);
    line.push("-");
    line.push_padded(m, 2);
    line.push("-");
    line.push_padded(d, 2);
    line.push("T");
    line.push_padded(rem / 3600, 2);
    line.push(":");
    line.push_padded(rem % 3600 / 60, 2);
    line.push(":");
    line.push_padded(rem % 60, 2);
    line.push(".");
    line.push_padded(u64::from(millis), 3);
    line.push("+00:00");
}

/// Format the one `alloc_failure` line. Allocation-free; a separate function
/// so a test can pin the exact shape the next-boot harvest parses.
#[allow(clippy::too_many_arguments)]
fn format_alloc_failure_line(
    line: &mut LineBuf,
    now: (u64, u32),
    op: AllocOp,
    size: usize,
    align: usize,
    thread_id: u64,
    reading: Option<(MemoryReading, u64)>,
    pid: u32,
) {
    push_rfc3339(line, now);
    line.push(" ");
    line.push(ALLOC_FAILURE_TOKEN);
    line.push(" memory allocation of ");
    line.push_u64(size as u64);
    line.push(" bytes failed (");
    line.push(op.as_str());
    line.push(", align ");
    line.push_u64(align as u64);
    line.push(", thread ");
    line.push_u64(thread_id);
    line.push(") — ");
    match reading {
        Some((r, at_secs)) => {
            line.push("last memory reading ");
            line.push_u64(now.0.saturating_sub(at_secs));
            line.push("s old: free_commit ");
            line.push_u64(r.free_commit);
            line.push(" bytes, commit_limit ");
            line.push_u64(r.commit_limit);
            line.push(" bytes, free_phys ");
            line.push_u64(r.free_phys);
            line.push(" bytes");
        }
        None => line.push("no memory reading was cached"),
    }
    line.push(
        "; unless the caller used a fallible API (try_reserve) the default alloc error \
               handler aborts this process next (pid ",
    );
    line.push_u64(u64::from(pid));
    line.push(")\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// An inner allocator that returns null while `failing` is set, and
    /// forwards to `System` otherwise.
    struct StubAlloc {
        failing: AtomicBool,
    }

    unsafe impl GlobalAlloc for StubAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if self.failing.load(Ordering::SeqCst) {
                std::ptr::null_mut()
            } else {
                System.alloc(layout)
            }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if self.failing.load(Ordering::SeqCst) {
                std::ptr::null_mut()
            } else {
                System.alloc_zeroed(layout)
            }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout)
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if self.failing.load(Ordering::SeqCst) {
                std::ptr::null_mut()
            } else {
                System.realloc(ptr, layout, new_size)
            }
        }
    }

    /// A capturing sink. It allocates, which is fine: under test the wrapper
    /// is called directly, not registered as the process allocator.
    struct TestSink {
        lines: Mutex<Vec<Vec<u8>>>,
        calls: AtomicUsize,
    }

    impl BreadcrumbSink for TestSink {
        fn write_line(&self, line: &[u8]) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.lines.lock().unwrap().push(line.to_vec());
        }
    }

    fn harness() -> BreadcrumbAlloc<StubAlloc, TestSink> {
        BreadcrumbAlloc::new(
            StubAlloc {
                failing: AtomicBool::new(false),
            },
            TestSink {
                lines: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            },
        )
    }

    /// Plan Phase 0 verification (b2): N consecutive failures write exactly
    /// ONE breadcrumb and return null every time — the wrapper observes, it
    /// never converts a failure into anything else.
    #[test]
    fn n_consecutive_failures_write_one_breadcrumb_and_return_null_each_time() {
        let a = harness();
        let layout = Layout::from_size_align(2_097_152, 8).unwrap();

        // Success path: pointers come back, nothing is written.
        let p = unsafe { a.alloc(layout) };
        assert!(!p.is_null());
        unsafe { a.dealloc(p, layout) };
        assert_eq!(a.sink.calls.load(Ordering::SeqCst), 0);

        a.inner.failing.store(true, Ordering::SeqCst);
        for _ in 0..50 {
            assert!(unsafe { a.alloc(layout) }.is_null());
            assert!(unsafe { a.alloc_zeroed(layout) }.is_null());
        }
        let mut dummy = 0u8;
        assert!(unsafe { a.realloc(&mut dummy, Layout::new::<u8>(), 4096) }.is_null());
        assert_eq!(
            a.sink.calls.load(Ordering::SeqCst),
            1,
            "a failure storm writes one line"
        );

        let lines = a.sink.lines.lock().unwrap();
        let line = std::str::from_utf8(&lines[0]).expect("ASCII/UTF-8 line");
        assert!(line
            .contains(" alloc_failure memory allocation of 2097152 bytes failed (alloc, align 8"));
        assert!(line.ends_with(&format!("(pid {})\n", std::process::id())));
    }

    /// The line is the `wedge-incidents.log` shape — an RFC 3339 timestamp
    /// chrono parses, then the token — and carries the cached reading.
    #[test]
    fn the_line_has_the_wedge_incident_shape_and_the_reading() {
        let mut line = LineBuf::new();
        let reading = MemoryReading {
            free_commit: 12_345,
            commit_limit: 77_000_000_000,
            free_phys: 3_473_344_000,
        };
        // 2026-09-23T01:25:01.441Z — abort #4.
        format_alloc_failure_line(
            &mut line,
            (1_790_126_701, 441),
            AllocOp::Alloc,
            2_097_152,
            16,
            4242,
            Some((reading, 1_790_126_671)),
            777,
        );
        let s = std::str::from_utf8(line.as_bytes()).unwrap();
        let mut parts = s.splitn(3, ' ');
        let ts = parts.next().unwrap();
        assert_eq!(ts, "2026-09-23T01:25:01.441+00:00");
        assert!(chrono::DateTime::parse_from_rfc3339(ts).is_ok());
        assert_eq!(parts.next().unwrap(), ALLOC_FAILURE_TOKEN);
        assert!(s.contains("thread 4242"));
        assert!(s.contains("30s old: free_commit 12345 bytes, commit_limit 77000000000 bytes, free_phys 3473344000 bytes"));
        assert!(s.ends_with("(pid 777)\n"));
    }

    /// The hand-rolled calendar agrees with chrono across eras and leap days.
    #[test]
    fn the_allocation_free_timestamp_matches_chrono() {
        for secs in [
            0u64,
            951_782_400,
            1_709_164_800,
            1_790_126_701,
            4_102_444_799,
        ] {
            let mut line = LineBuf::new();
            push_rfc3339(&mut line, (secs, 7));
            let expected = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 7_000_000)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
            assert_eq!(line.as_str_lossless(), expected);
        }
    }

    /// Before `install` the production sink is a silent no-op — which is what
    /// every test binary and every non-runner binary sees.
    #[test]
    fn the_production_sink_is_inert_until_installed() {
        IncidentFileSink.write_line(b"never written\n");
    }
}
