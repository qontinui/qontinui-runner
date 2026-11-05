//! Top commit consumers for the host-lane resource sample (plan
//! `2026-11-05-devops-a-host-commit-climb-to-99-7-percent-names-no-consumer-and-reads-ok-two-minutes-out`,
//! D1 / Phase 3).
//!
//! # Why this exists
//!
//! On 2026-11-05 `spaceship`'s commit charge climbed from 0.66 to 0.997 in 14
//! minutes, and nothing on any coord surface could name the process that
//! consumed ~110 GB of it: the sample carried machine totals and the runner's
//! own thread count, and no field naming any other process. This module adds
//! that field.
//!
//! # Shape on the wire
//!
//! ```text
//! top_consumers = {"entries": [{"pid": u32, "name": str, "private_bytes": u64,
//!                               "started_at": RFC3339 UTC | null}],   // ≤ 5, desc
//!                  "scanned": u64, "unreadable": u64,
//!                  "attributed_private_bytes": u64}
//! ```
//!
//! * `private_bytes` is the per-process COMMIT charge on Windows
//!   (`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`) — working set is physical
//!   memory and the wrong instrument for a commit-limit axis — and
//!   `RssAnon + VmSwap` on Linux, the analogue for the swap-led Linux lanes.
//! * `scanned` / `unreadable` / `attributed_private_bytes` ship WITH the list:
//!   without completeness counts a top-5 that misses the real consumer reads
//!   the same as one that names it.
//!
//! # What it never does
//!
//! * **Spawn.** No `tasklist`, no `ps`, no child of any kind: a host near commit
//!   exhaustion cannot be relied on to spawn one (`0xc0000142`). A structural
//!   pin in `resource_sample.rs`'s tests refuses a process spawn in this file.
//! * **Publish a command line.** argv and the environment carry secrets.
//!   `name` is the image name only, sanitised to printable characters and
//!   capped at [`NAME_MAX_CHARS`].
//! * **Fabricate an empty list.** When enumeration itself fails the whole field
//!   is `None` — UNKNOWN — never `{"entries": [], "scanned": 0}`.

use serde::Serialize;

/// How many entries the published list carries.
pub(crate) const TOP_CONSUMERS_MAX: usize = 5;

/// Cap on a published image name, in characters.
pub(crate) const NAME_MAX_CHARS: usize = 128;

/// One ranked process.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ConsumerEntry {
    pub(crate) pid: u32,
    /// Image name only — never a path, never a command line.
    pub(crate) name: String,
    pub(crate) private_bytes: u64,
    /// Process creation time, RFC3339 UTC, so a reader can tell a reused pid
    /// apart. `None` when the OS would not say.
    pub(crate) started_at: Option<String>,
}

/// The published `top_consumers` object.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TopConsumers {
    pub(crate) entries: Vec<ConsumerEntry>,
    /// Processes enumerated.
    pub(crate) scanned: u64,
    /// Processes enumerated but whose memory could not be read (an unelevated
    /// runner cannot open protected or other-session processes; on Linux, a
    /// process that exited mid-scan or a hidden `/proc` entry).
    pub(crate) unreadable: u64,
    /// Sum of `private_bytes` over every READABLE process. What it may be set
    /// against differs by OS:
    ///
    /// * **Windows** — a sum of `PrivateUsage`, i.e. per-process commit charge;
    ///   set against `commit_used_bytes` it shows how much commit sits outside
    ///   process-private memory (AWE, shared sections, kernel pools).
    /// * **Linux** — a sum of `RssAnon + VmSwap`. It can DOUBLE-COUNT
    ///   copy-on-write anonymous pages shared between a parent and its forked
    ///   children, and it EXCLUDES shmem / tmpfs. It is a ranking aid, not an
    ///   accounting identity: never subtract it from a commit or used-memory
    ///   total.
    pub(crate) attributed_private_bytes: u64,
}

/// Sanitise an image name: printable characters only, capped at
/// [`NAME_MAX_CHARS`]. A NUL in jsonb drops every lane of coord's push.
pub(crate) fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX_CHARS)
        .collect()
}

/// Rank `rows` (already-read processes) into the published object.
///
/// Pure, so the ordering, the cap and the sums are testable on every OS.
pub(crate) fn rank(
    mut rows: Vec<ConsumerEntry>,
    scanned: u64,
    unreadable: u64,
    top_n: usize,
) -> TopConsumers {
    let attributed_private_bytes = rows
        .iter()
        .fold(0u64, |a, r| a.saturating_add(r.private_bytes));
    rows.sort_by(|a, b| {
        b.private_bytes
            .cmp(&a.private_bytes)
            .then(a.pid.cmp(&b.pid))
    });
    rows.truncate(top_n);
    TopConsumers {
        entries: rows,
        scanned,
        unreadable,
        attributed_private_bytes,
    }
}

/// Collect the published field: the top [`TOP_CONSUMERS_MAX`] by private bytes.
pub(crate) fn collect() -> Option<TopConsumers> {
    collect_top(TOP_CONSUMERS_MAX)
}

/// Collect with an explicit cap (tests pass a large one to find their own pid
/// on a busy box). `None` when the process table cannot be enumerated at all.
pub(crate) fn collect_top(top_n: usize) -> Option<TopConsumers> {
    finish(scan(), top_n)
}

/// The UNKNOWN-vs-measured decision, pure so it is testable on every OS.
///
/// `None` when the scan itself failed, and ALSO when it "succeeded" having
/// enumerated nothing: a live process table always holds at least the
/// enumerating process, so `scanned == 0` is a failed enumeration, and
/// publishing it would render an empty list as "no consumers".
pub(crate) fn finish(
    scan: Option<(Vec<ConsumerEntry>, u64, u64)>,
    top_n: usize,
) -> Option<TopConsumers> {
    let (rows, scanned, unreadable) = scan?;
    if scanned == 0 {
        return None;
    }
    Some(rank(rows, scanned, unreadable, top_n))
}

/// The file-name part of an image path (`C:\x\node.exe` → `node.exe`),
/// splitting on either separator. `None` for an empty result.
pub(crate) fn image_file_name(path: &str) -> Option<String> {
    let leaf = path.rsplit(['\\', '/']).next()?.trim();
    (!leaf.is_empty()).then(|| sanitize_name(leaf))
}

/// Format a Unix timestamp (seconds) as RFC3339 UTC.
fn rfc3339_utc(unix_secs: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

// ─── Windows ────────────────────────────────────────────────────────────────

/// Read one process by pid: `Some(entry)` when readable, `None` when
/// `OpenProcess` or the memory query was refused (counted as `unreadable`).
///
/// `name` is taken from the OPENED handle (`QueryFullProcessImageNameW`), not
/// the snapshot: between the snapshot and `OpenProcess` the pid can exit and be
/// reused, and the handle is the one thing that names the process whose memory
/// is being read. `snapshot_name` is the fallback only when that query fails.
#[cfg(target_os = "windows")]
pub(crate) fn read_process(pid: u32, snapshot_name: &str) -> Option<ConsumerEntry> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: plain FFI call; a null return is handled below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let counters = crate::health_monitor::process_memory_counters(handle);
    let started_at = windows_creation_time(handle);
    let name = windows_image_name(handle).unwrap_or_else(|| sanitize_name(snapshot_name));
    // SAFETY: closing the handle we opened, exactly once.
    unsafe { CloseHandle(handle) };
    let counters = counters?;
    Some(ConsumerEntry {
        pid,
        name,
        private_bytes: counters.PrivateUsage as u64,
        started_at,
    })
}

/// The image file name of an opened process. `PROCESS_QUERY_LIMITED_INFORMATION`
/// suffices for `QueryFullProcessImageNameW`.
#[cfg(target_os = "windows")]
fn windows_image_name(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};

    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; on success `len` is set to the written
    // length, excluding the terminator.
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(buf.get(..len as usize)?);
    image_file_name(&path)
}

#[cfg(target_os = "windows")]
fn windows_creation_time(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
    // SAFETY: valid handle with PROCESS_QUERY_LIMITED_INFORMATION.
    if unsafe { GetProcessTimes(handle, &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    let ticks = (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime);
    // FILETIME counts 100 ns ticks since 1601-01-01; Unix epoch is this many
    // ticks later.
    const EPOCH_DIFF_TICKS: u64 = 116_444_736_000_000_000;
    let unix = ticks.checked_sub(EPOCH_DIFF_TICKS)? / 10_000_000;
    rfc3339_utc(i64::try_from(unix).ok()?)
}

#[cfg(target_os = "windows")]
fn scan() -> Option<(Vec<ConsumerEntry>, u64, u64)> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: plain FFI call; the invalid-handle return is handled below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut rows = Vec::new();
    let mut scanned = 0u64;
    let mut unreadable = 0u64;

    let mut pe = MaybeUninit::<PROCESSENTRY32W>::zeroed();
    // SAFETY: writing the size field of our own zeroed buffer.
    unsafe { (*pe.as_mut_ptr()).dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32 };

    // SAFETY: valid snapshot handle and a sized entry buffer.
    if unsafe { Process32FirstW(snapshot, pe.as_mut_ptr()) } != 0 {
        loop {
            // SAFETY: filled by a successful Process32FirstW / Process32NextW.
            let entry = unsafe { pe.assume_init_ref() };
            let pid = entry.th32ProcessID;
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(entry.szExeFile.get(..len).unwrap_or(&[]));
            // pid 0 is the System Idle Process — not a process with memory.
            if pid != 0 {
                scanned += 1;
                match read_process(pid, &name) {
                    Some(row) => rows.push(row),
                    None => unreadable += 1,
                }
            }
            // SAFETY: as above.
            if unsafe { Process32NextW(snapshot, pe.as_mut_ptr()) } == 0 {
                break;
            }
        }
    }

    // SAFETY: closing the snapshot we opened, exactly once.
    unsafe { CloseHandle(snapshot) };
    Some((rows, scanned, unreadable))
}

// ─── Linux ──────────────────────────────────────────────────────────────────

/// Parse `/proc/<pid>/status` into `(name, private_bytes)`.
///
/// `private_bytes = (RssAnon + VmSwap) × 1024`. A kernel thread has neither
/// line and reads as 0 private bytes, which is true of it. `None` when the
/// `Name:` line is missing — not a status file.
pub(crate) fn parse_proc_status(status: &str) -> Option<(String, u64)> {
    let mut name = None;
    let mut rss_anon_kb = 0u64;
    let mut swap_kb = 0u64;
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let kb = || {
            value
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0)
        };
        match key {
            "Name" => name = Some(sanitize_name(value.trim())),
            "RssAnon" => rss_anon_kb = kb(),
            "VmSwap" => swap_kb = kb(),
            _ => {}
        }
    }
    Some((
        name?,
        rss_anon_kb.saturating_add(swap_kb).saturating_mul(1024),
    ))
}

/// Parse field 22 (`starttime`, clock ticks since boot) out of
/// `/proc/<pid>/stat`. `comm` may contain spaces and parentheses, so the split
/// is at the LAST `)`.
pub(crate) fn parse_proc_stat_starttime(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    // `rest` starts at field 3 (state); field 22 is index 19 from there.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// `btime` (boot time, Unix seconds) out of `/proc/stat`.
pub(crate) fn parse_proc_stat_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

/// Read one `/proc/<pid>` directory (or `/proc/self`). `None` = unreadable.
#[cfg(target_os = "linux")]
pub(crate) fn read_proc_dir(
    dir: &std::path::Path,
    pid: u32,
    boot_secs: Option<i64>,
    clk_tck: Option<u64>,
) -> Option<ConsumerEntry> {
    let status = std::fs::read_to_string(dir.join("status")).ok()?;
    let (name, private_bytes) = parse_proc_status(&status)?;
    let started_at = match (boot_secs, clk_tck) {
        (Some(boot), Some(tck)) if tck > 0 => std::fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|s| parse_proc_stat_starttime(&s))
            .and_then(|ticks| i64::try_from(ticks / tck).ok())
            .and_then(|secs| boot.checked_add(secs))
            .and_then(rfc3339_utc),
        _ => None,
    };
    Some(ConsumerEntry {
        pid,
        name,
        private_bytes,
        started_at,
    })
}

#[cfg(target_os = "linux")]
fn clk_tck() -> Option<u64> {
    // SAFETY: sysconf is a pure libc query.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(v).ok().filter(|&t| t > 0)
}

#[cfg(target_os = "linux")]
fn scan() -> Option<(Vec<ConsumerEntry>, u64, u64)> {
    let boot = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| parse_proc_stat_btime(&s));
    let tck = clk_tck();
    let mut rows = Vec::new();
    let mut scanned = 0u64;
    let mut unreadable = 0u64;
    for dent in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = dent
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        scanned += 1;
        match read_proc_dir(&dent.path(), pid, boot, tck) {
            Some(row) => rows.push(row),
            None => unreadable += 1,
        }
    }
    Some((rows, scanned, unreadable))
}

// ─── Other OS ───────────────────────────────────────────────────────────────

/// No reader: the field is omitted (UNKNOWN), never an empty list.
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn scan() -> Option<(Vec<ConsumerEntry>, u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(pid: u32, bytes: u64) -> ConsumerEntry {
        ConsumerEntry {
            pid,
            name: format!("p{pid}"),
            private_bytes: bytes,
            started_at: None,
        }
    }

    #[test]
    fn rank_orders_desc_caps_and_sums_every_readable_row() {
        let rows = (1..=8).map(|i| e(i, u64::from(i) * 10)).collect();
        let t = rank(rows, 10, 2, TOP_CONSUMERS_MAX);
        assert_eq!(
            t.entries.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![8, 7, 6, 5, 4]
        );
        // The sum covers all eight readable rows, not just the published five.
        assert_eq!(t.attributed_private_bytes, (1..=8).sum::<u64>() * 10);
        assert_eq!((t.scanned, t.unreadable), (10, 2));
    }

    #[test]
    fn finish_is_unknown_on_a_failed_or_empty_scan_and_measured_otherwise() {
        assert_eq!(
            finish(None, TOP_CONSUMERS_MAX),
            None,
            "a failed scan is UNKNOWN"
        );
        assert_eq!(
            finish(Some((vec![], 0, 0)), TOP_CONSUMERS_MAX),
            None,
            "a scan that enumerated nothing failed — never an empty list"
        );
        let t = finish(Some((vec![e(1, 10), e(2, 20)], 3, 1)), TOP_CONSUMERS_MAX)
            .expect("a non-empty scan is measured");
        assert_eq!(
            t.entries.iter().map(|r| r.pid).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            (t.scanned, t.unreadable, t.attributed_private_bytes),
            (3, 1, 30)
        );
        // All-unreadable is still a measurement: scanned > 0, nothing attributed.
        let u = finish(Some((vec![], 4, 4)), TOP_CONSUMERS_MAX).expect("scanned > 0");
        assert!(u.entries.is_empty());
        assert_eq!(u.unreadable, 4);
    }

    #[test]
    fn image_file_name_keeps_only_the_leaf() {
        let win = ["C:", "Program Files", "nodejs", "node.exe"].join(&'\\'.to_string());
        assert_eq!(image_file_name(&win).as_deref(), Some("node.exe"));
        assert_eq!(image_file_name("/usr/bin/node").as_deref(), Some("node"));
        assert_eq!(image_file_name("node.exe").as_deref(), Some("node.exe"));
        assert_eq!(image_file_name(""), None);
        assert_eq!(image_file_name("C:/dir/"), None);
    }

    #[test]
    fn sanitize_strips_control_chars_and_caps_length() {
        assert_eq!(sanitize_name("no\0de\n.exe"), "node.exe");
        assert_eq!(
            sanitize_name(&"x".repeat(500)).chars().count(),
            NAME_MAX_CHARS
        );
    }

    #[test]
    fn proc_status_parses_anon_plus_swap() {
        let s = "Name:\tnode\nUmask:\t0022\nRssAnon:\t  2048 kB\nRssFile:\t 999 kB\nVmSwap:\t  1024 kB\n";
        assert_eq!(parse_proc_status(s), Some(("node".into(), 3072 * 1024)));
        // A kernel thread: no RssAnon/VmSwap — zero private bytes, still readable.
        assert_eq!(
            parse_proc_status("Name:\tkthreadd\nState:\tS\n"),
            Some(("kthreadd".into(), 0))
        );
        assert_eq!(parse_proc_status("garbage"), None);
    }

    #[test]
    fn proc_stat_starttime_survives_a_comm_with_spaces_and_parens() {
        // Fields 3..=21 are filler; field 22 (starttime) = 4242.
        let filler = (3..=21)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let stat = format!("123 (a (weird) name) {filler} 4242 99 100");
        assert_eq!(parse_proc_stat_starttime(&stat), Some(4242));
        assert_eq!(
            parse_proc_stat_btime("cpu 1 2\nbtime 1700000000\n"),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn the_published_shape_is_the_d1_contract() {
        let t = rank(vec![e(7, 100)], 3, 1, TOP_CONSUMERS_MAX);
        let v = serde_json::to_value(&t).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                "attributed_private_bytes",
                "entries",
                "scanned",
                "unreadable"
            ]
        );
        let entry = v["entries"][0].as_object().unwrap();
        let mut ek: Vec<_> = entry.keys().cloned().collect();
        ek.sort();
        assert_eq!(ek, vec!["name", "pid", "private_bytes", "started_at"]);
    }

    /// The contract floor both OS tests assert: own private bytes rose by at
    /// least this much.
    const FLOOR: usize = 256 * 1024 * 1024;
    /// What they allocate: the floor plus 64 MiB of slack, so a sibling test
    /// freeing memory between the two readings cannot red this one.
    const ALLOC: usize = FLOOR + 64 * 1024 * 1024;

    /// Allocate and TOUCH 320 MiB (nonzero bytes, so the pages are anonymous
    /// and resident), and the reader against `/proc/self` must see ≥ 256 MiB.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_own_private_bytes_rise_by_the_allocation() {
        let me = std::process::id();
        let read = || {
            read_proc_dir(std::path::Path::new("/proc/self"), me, None, None)
                .expect("/proc/self/status must be readable")
                .private_bytes
        };
        let before = read();
        let buf = vec![0xA5u8; ALLOC];
        std::hint::black_box(&buf);
        let after = read();
        assert!(
            after.saturating_sub(before) >= FLOOR as u64,
            "private bytes rose {} → {} (Δ {}), expected ≥ {FLOOR}",
            before,
            after,
            after.saturating_sub(before)
        );
        // And the full collector, uncapped, names this process.
        let all = collect_top(usize::MAX).expect("the /proc scan must succeed");
        assert!(all.scanned > 0);
        let mine = all
            .entries
            .iter()
            .find(|r| r.pid == me)
            .expect("own pid must appear in the uncapped collector output");
        assert!(mine.private_bytes >= FLOOR as u64);
        assert!(
            mine.started_at.is_some(),
            "started_at must resolve from /proc stat + btime"
        );
        drop(buf);
    }

    /// Commit 320 MiB with `VirtualAlloc(MEM_COMMIT)` — commit charge without
    /// touching a page — and the reader must see `PrivateUsage` rise ≥ 256 MiB.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_own_private_bytes_rise_by_a_committed_allocation() {
        use windows_sys::Win32::System::Memory::{
            VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
        };
        let me = std::process::id();
        let read = || {
            read_process(me, "self")
                .expect("own process must be readable")
                .private_bytes
        };
        let before = read();
        // SAFETY: a fresh anonymous reservation+commit, released below.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                ALLOC,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        assert!(!p.is_null(), "VirtualAlloc of {ALLOC} bytes failed");
        let after = read();
        let all = collect_top(usize::MAX);
        // SAFETY: releasing the region we allocated, exactly once.
        unsafe { VirtualFree(p, 0, MEM_RELEASE) };
        assert!(
            after.saturating_sub(before) >= FLOOR as u64,
            "PrivateUsage rose {before} → {after}, expected ≥ {FLOOR}"
        );
        let all = all.expect("the ToolHelp scan must succeed");
        assert!(all.scanned > 0);
        let mine = all
            .entries
            .iter()
            .find(|r| r.pid == me)
            .expect("own pid must appear in the uncapped collector output");
        assert!(mine.private_bytes >= FLOOR as u64);
        assert!(mine.started_at.is_some());
    }
}
