//! Pure parsers and gauge composition for the `os-health` collector.
//!
//! Everything here is a function of bytes or text, with no I/O and no
//! platform gate, so the fixture tests run on every OS CI builds — including
//! the Linux job, which can never call `NtQueryObject` itself. The Windows and
//! Linux collectors (`windows.rs`, `linux.rs`) do the reads and hand the raw
//! results here.
//!
//! ## UNKNOWN, never a false number
//!
//! Every parser returns `Option`. A buffer that is truncated, malformed, or
//! missing the entry that proves it is the structure we think it is yields
//! `None`, which the snapshot carries as UNKNOWN. A zero is never used as a
//! stand-in: on these axes a fabricated 0 reads as the healthiest box in the
//! fleet (plan `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-
//! creation-latency` D6).

use chrono::{DateTime, Utc};
use serde::Serialize;

/// `OBJECT_TYPE_INFORMATION` (x64) — the byte offsets the parser reads.
///
/// `NtQueryObject(NULL, ObjectTypesInformation)` is undocumented. The layout
/// below is the one Process Hacker reads, and it is VERIFIED here only against
/// the real buffer captured on Windows 10.0.26200 (`object_types_26200.bin`,
/// tested below) — no claim is made for any other build. A build whose layout
/// differs fails the parse's own sanity checks (an odd name length, an absurd
/// type count, no `Token` or no `Process` row) and reads UNKNOWN, never a false
/// number. The buffer is a `u32` type count, padded to 8, then one entry per
/// type:
///
/// | Offset | Field |
/// |---|---|
/// | `+0x00` | `UNICODE_STRING TypeName` — `Length: u16`, `MaximumLength: u16`, pad, `Buffer: *u16` (16 bytes) |
/// | `+0x10` | `TotalNumberOfObjects: u32` |
/// | `+0x14` | `TotalNumberOfHandles: u32` |
/// | `+0x68` | the UTF-16LE name, `Length` bytes |
///
/// and the next entry starts at `+0x68 + align8(MaximumLength)`. The name's
/// `Buffer` pointer is ignored: it points into the caller's buffer at `+0x68`,
/// and following a pointer out of a byte slice is exactly what a pure parser
/// must not do.
const OT_HEADER_BYTES: usize = 8;
const OT_OBJECTS_OFFSET: usize = 0x10;
const OT_HANDLES_OFFSET: usize = 0x14;
const OT_NAME_OFFSET: usize = 0x68;
/// No Windows build has more than ~80 object types; a count past this is a
/// buffer we have misread, not a reading.
const OT_MAX_TYPES: u32 = 4096;

/// `SYSTEM_POOLTAG` (x64): 40 bytes per entry, from offset 8.
///
/// | Offset | Field |
/// |---|---|
/// | `+0` | `Tag: [u8; 4]` |
/// | `+4` | `PagedAllocs: u32` |
/// | `+8` | `PagedFrees: u32` |
/// | `+16` | `PagedUsed: u64` |
/// | `+24` | `NonPagedAllocs: u32` |
/// | `+28` | `NonPagedFrees: u32` |
/// | `+32` | `NonPagedUsed: u64` |
const PT_HEADER_BYTES: usize = 8;
const PT_STRIDE: usize = 40;

/// How many pool tags the snapshot keeps (plan D2: the top 8 by bytes).
pub(crate) const POOL_TOP_N: usize = 8;

fn read_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

/// Saturate a non-negative count into the `bigint` columns' `i64`.
pub(crate) fn clamp_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// One `OBJECT_TYPE_INFORMATION` entry, as far as this collector reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjectTypeEntry {
    pub(crate) name: String,
    pub(crate) objects: u32,
    pub(crate) handles: u32,
}

/// Parse every entry of an `ObjectTypesInformation` buffer.
///
/// `None` when the buffer is shorter than its header, declares an absurd type
/// count, or ends before ANY declared entry does (a truncated read). A
/// truncated buffer is never parsed "as far as it goes": the entries that did
/// fit are real, but the collector cannot tell a missing `Token` row from one
/// that was cut off, so the whole read is UNKNOWN.
pub(crate) fn parse_object_types(buf: &[u8]) -> Option<Vec<ObjectTypeEntry>> {
    let count = read_u32(buf, 0)?;
    if count == 0 || count > OT_MAX_TYPES || buf.len() < OT_HEADER_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    let mut off = OT_HEADER_BYTES;
    for _ in 0..count {
        let length = usize::from(read_u16(buf, off)?);
        let max_length = usize::from(read_u16(buf, off.checked_add(2)?)?);
        // A UTF-16 name has an even byte length that fits its own buffer;
        // anything else is a misread, not a short name.
        if !length.is_multiple_of(2) || length > max_length {
            return None;
        }
        let objects = read_u32(buf, off.checked_add(OT_OBJECTS_OFFSET)?)?;
        let handles = read_u32(buf, off.checked_add(OT_HANDLES_OFFSET)?)?;
        let name_at = off.checked_add(OT_NAME_OFFSET)?;
        let name_bytes = buf.get(name_at..name_at.checked_add(length)?)?;
        let units: Vec<u16> = name_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let name = String::from_utf16(&units).ok()?;
        out.push(ObjectTypeEntry {
            name,
            objects,
            handles,
        });
        // The next entry. Only bounds-checked when it is READ — the last
        // entry's padding may legitimately end exactly at the buffer's end.
        off = name_at.checked_add(max_length.checked_add(7)? & !7)?;
    }
    Some(out)
}

/// The two counts the snapshot publishes, out of one `ObjectTypesInformation`
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectTypeCounts {
    /// `Token` `TotalNumberOfObjects` — the leading indicator of the
    /// `win32kfull.sys` 10.0.26100.9711 leak (4,365,962 on degraded spaceship).
    pub(crate) token_objects: u32,
    /// `Process` `TotalNumberOfObjects` — live processes plus zombies.
    pub(crate) process_objects: u32,
}

/// Token and Process object counts, or `None` (UNKNOWN).
///
/// **A buffer with no `Token` entry, or no `Process` entry, is UNKNOWN — not
/// "zero tokens", and not "Token known, Process unknown".** Every Windows
/// kernel has both types; a parse that does not find one of them has misread
/// the layout, and then neither count from it can be trusted (plan Risks: "a
/// parse sanity check — a Token entry must exist — guards it"; the Phase 4
/// review extended the same rule to `Process`).
pub(crate) fn object_type_counts(buf: &[u8]) -> Option<ObjectTypeCounts> {
    let entries = parse_object_types(buf)?;
    let token = entries.iter().find(|e| e.name == "Token")?;
    let process = entries.iter().find(|e| e.name == "Process")?;
    Some(ObjectTypeCounts {
        token_objects: token.objects,
        process_objects: process.objects,
    })
}

/// One pool tag as published: `{tag, bytes, outstanding_allocs}` (plan D2's
/// `kernel_pool_top_tags` element, the same keys on the wire and in coord's
/// alert detail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PoolTag {
    /// The four tag bytes rendered by [`render_pool_tag`]: one printable ASCII
    /// char per byte, so a trailing space survives (`"D2d "` is a different
    /// tag from `"D2d"`) and a NUL never reaches the wire.
    pub(crate) tag: String,
    /// `PagedUsed + NonPagedUsed`.
    pub(crate) bytes: i64,
    /// `(PagedAllocs - PagedFrees) + (NonPagedAllocs - NonPagedFrees)`, each
    /// difference taken MODULO 2^32 and then widened ([`outstanding`]).
    ///
    /// The four counters are free-running `u32`s that wrap independently. On a
    /// tag that has turned over more than 2^32 allocations, `PagedAllocs` has
    /// wrapped and reads SMALLER than `PagedFrees`, while the true outstanding
    /// count is still small and positive. `allocs.wrapping_sub(frees)` recovers
    /// it exactly as long as fewer than 2^32 allocations are live at once,
    /// which no pool can hold. A signed difference would instead publish a
    /// large negative number for exactly the busiest tags.
    pub(crate) outstanding_allocs: i64,
}

/// The character a non-printable tag byte is rendered as.
pub(crate) const POOL_TAG_UNPRINTABLE: char = '?';

/// Render four raw pool-tag bytes as exactly four printable ASCII chars.
///
/// Pool tags are raw `u32`s, not text: 22 of the 3,379 tags in the incident
/// buffer end in a NUL (`TEE\0`, `PXa\0`, `API\0`, …), and a driver may set
/// any byte. A NUL serialized into `kernel_pool_top_tags` reaches Postgres as
/// `\u0000`, which `jsonb` refuses (SQLSTATE 22P05) — and that refusal drops
/// the WHOLE resource-sample INSERT, every other column with it. So every byte
/// outside `0x20..=0x7E` becomes [`POOL_TAG_UNPRINTABLE`].
///
/// One char per byte, never an escape sequence: coord keeps a tag only when it
/// is at most 4 chars (`POOL_TAG_MAX_CHARS`) and otherwise NULLs the whole
/// array, so `\x00` would trade one dropped row for a dropped column.
pub(crate) fn render_pool_tag(raw: [u8; 4]) -> String {
    raw.iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                char::from(b)
            } else {
                POOL_TAG_UNPRINTABLE
            }
        })
        .collect()
}

/// A parsed `SystemPoolTagInformation` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PoolTagTable {
    /// How many tags the kernel reported.
    pub(crate) tag_count: usize,
    /// The largest [`POOL_TOP_N`] by bytes, largest first. Ties keep the
    /// kernel's order (a stable sort).
    pub(crate) top: Vec<PoolTag>,
}

/// Live allocations of one pool out of its free-running `u32` counters: the
/// difference modulo 2^32 (see [`PoolTag::outstanding_allocs`]).
fn outstanding(allocs: u32, frees: u32) -> i64 {
    i64::from(allocs.wrapping_sub(frees))
}

/// Parse a `SystemPoolTagInformation` buffer (x64 layout).
///
/// `None` for a buffer shorter than its declared entries (truncated) and for
/// an empty table, which no running kernel produces — publishing `[]` would
/// read as "no pool in use".
pub(crate) fn parse_pool_tags(buf: &[u8]) -> Option<PoolTagTable> {
    let count = usize::try_from(read_u32(buf, 0)?).ok()?;
    if count == 0 {
        return None;
    }
    let needed = count.checked_mul(PT_STRIDE)?.checked_add(PT_HEADER_BYTES)?;
    if buf.len() < needed {
        return None;
    }
    let mut tags = Vec::with_capacity(count);
    for i in 0..count {
        let o = PT_HEADER_BYTES + i * PT_STRIDE;
        let raw: [u8; 4] = buf.get(o..o + 4)?.try_into().ok()?;
        let tag = render_pool_tag(raw);
        let paged_allocs = read_u32(buf, o + 4)?;
        let paged_frees = read_u32(buf, o + 8)?;
        let paged_used = read_u64(buf, o + 16)?;
        let nonpaged_allocs = read_u32(buf, o + 24)?;
        let nonpaged_frees = read_u32(buf, o + 28)?;
        let nonpaged_used = read_u64(buf, o + 32)?;
        tags.push(PoolTag {
            tag,
            bytes: clamp_i64(paged_used.saturating_add(nonpaged_used)),
            outstanding_allocs: outstanding(paged_allocs, paged_frees)
                + outstanding(nonpaged_allocs, nonpaged_frees),
        });
    }
    // Largest first; stable, so equal byte counts keep the kernel's order.
    tags.sort_by_key(|t| std::cmp::Reverse(t.bytes));
    tags.truncate(POOL_TOP_N);
    Some(PoolTagTable {
        tag_count: count,
        top: tags,
    })
}

/// `K32GetPerformanceInfo`'s fields this collector reads, already copied out
/// of the Win32 struct so composition is testable off Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PerfInfo {
    pub(crate) kernel_paged_pages: u64,
    pub(crate) kernel_nonpaged_pages: u64,
    pub(crate) page_size: u64,
    pub(crate) handle_count: u32,
    pub(crate) process_count: u32,
}

/// One tick's gauge readings plus the instruments that ANSWERED them.
///
/// `instruments` feeds `kernel_source`: a name is pushed only when its read
/// produced at least one reading, so a peer can tell "this instrument failed"
/// (absent from an applicable platform's list) from "this instrument does not
/// exist here" (absent from the platform's list altogether).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GaugeReading {
    pub(crate) kernel_paged_bytes: Option<i64>,
    pub(crate) kernel_nonpaged_bytes: Option<i64>,
    pub(crate) kernel_handle_count: Option<i64>,
    pub(crate) kernel_token_objects: Option<i64>,
    pub(crate) kernel_process_objects: Option<i64>,
    pub(crate) os_process_count: Option<i64>,
    pub(crate) os_booted_at: Option<DateTime<Utc>>,
    pub(crate) instruments: Vec<&'static str>,
}

/// Instrument names, as they appear after the `windows:` / `linux:` prefix of
/// `kernel_source`.
pub(crate) mod instrument {
    pub(crate) const GET_PERFORMANCE_INFO: &str = "getperformanceinfo";
    pub(crate) const OBJECT_TYPES: &str = "objecttypes";
    pub(crate) const POOL_TAGS: &str = "pooltags";
    pub(crate) const PROC_MEMINFO: &str = "proc-meminfo";
    pub(crate) const FILE_NR: &str = "file-nr";
    pub(crate) const PROC_STAT: &str = "proc-stat";
    pub(crate) const PROC_PIDS: &str = "proc-pids";
    /// The D3 probe: named once its rolling window holds at least one result.
    /// Coord accepts the name verbatim in `kernel_source`.
    pub(crate) const SPAWN_PROBE: &str = "spawn-probe";
}

/// Compose a Windows tick. Each input is `None` when that read failed.
///
/// `booted_at` comes from `GetTickCount64`, which cannot fail, so it is not
/// named in `kernel_source`; it is `None` only if the date arithmetic
/// overflowed.
pub(crate) fn windows_gauges(
    perf: Option<PerfInfo>,
    objects: Option<ObjectTypeCounts>,
    booted_at: Option<DateTime<Utc>>,
) -> GaugeReading {
    let mut g = GaugeReading {
        os_booted_at: booted_at,
        ..GaugeReading::default()
    };
    if let Some(p) = perf {
        // A zero page size would turn every page count into 0 bytes — a
        // number, and the wrong one.
        if p.page_size > 0 {
            g.kernel_paged_bytes =
                Some(clamp_i64(p.kernel_paged_pages.saturating_mul(p.page_size)));
            g.kernel_nonpaged_bytes = Some(clamp_i64(
                p.kernel_nonpaged_pages.saturating_mul(p.page_size),
            ));
        }
        // No live Windows has zero handles or zero processes; a zero is a
        // read that did not happen.
        g.kernel_handle_count = (p.handle_count > 0).then(|| i64::from(p.handle_count));
        g.os_process_count = (p.process_count > 0).then(|| i64::from(p.process_count));
        if g.kernel_paged_bytes.is_some()
            || g.kernel_handle_count.is_some()
            || g.os_process_count.is_some()
        {
            g.instruments.push(instrument::GET_PERFORMANCE_INFO);
        }
    }
    if let Some(o) = objects {
        g.kernel_token_objects = Some(i64::from(o.token_objects));
        g.kernel_process_objects = Some(i64::from(o.process_objects));
        g.instruments.push(instrument::OBJECT_TYPES);
    }
    g
}

/// One `/proc/meminfo` value in BYTES — the same `Key:  N kB` reading the
/// `wsl` lane's `parse_meminfo` does, through the one shared helper.
fn meminfo_bytes(meminfo: &str, key: &str) -> Option<i64> {
    crate::fleet::resource_sample::meminfo_value_bytes(meminfo, key).map(clamp_i64)
}

/// `/proc/sys/fs/file-nr`'s first field: allocated file handles.
pub(crate) fn parse_file_nr(text: &str) -> Option<i64> {
    text.split_whitespace()
        .next()?
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
}

/// `/proc/stat`'s `btime` line: the boot instant in Unix seconds.
pub(crate) fn parse_btime(stat: &str) -> Option<DateTime<Utc>> {
    let secs = stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|s| *s > 0)?;
    DateTime::from_timestamp(secs, 0)
}

/// Compose a Linux tick from the raw reads (`None` = that file could not be
/// read).
///
/// The D2 mapping (vet R6): paged = `SReclaimable`; nonpaged = `SUnreclaim` +
/// `KernelStack` + `PageTables`. The nonpaged sum is published only when ALL
/// THREE terms parsed — a partial sum is a smaller, false number.
///
/// Token and Process objects and pool tags do not exist on Linux: they stay
/// `None`, and `kernel_source` never names them, which is how a reader tells
/// NOT APPLICABLE from a failed read (D6).
pub(crate) fn linux_gauges(
    meminfo: Option<&str>,
    file_nr: Option<&str>,
    stat: Option<&str>,
    pid_count: Option<u64>,
) -> GaugeReading {
    let mut g = GaugeReading::default();
    if let Some(m) = meminfo {
        g.kernel_paged_bytes = meminfo_bytes(m, "SReclaimable:");
        g.kernel_nonpaged_bytes = match (
            meminfo_bytes(m, "SUnreclaim:"),
            meminfo_bytes(m, "KernelStack:"),
            meminfo_bytes(m, "PageTables:"),
        ) {
            (Some(a), Some(b), Some(c)) => Some(a.saturating_add(b).saturating_add(c)),
            _ => None,
        };
        if g.kernel_paged_bytes.is_some() || g.kernel_nonpaged_bytes.is_some() {
            g.instruments.push(instrument::PROC_MEMINFO);
        }
    }
    if let Some(h) = file_nr.and_then(parse_file_nr) {
        g.kernel_handle_count = Some(h);
        g.instruments.push(instrument::FILE_NR);
    }
    if let Some(b) = stat.and_then(parse_btime) {
        g.os_booted_at = Some(b);
        g.instruments.push(instrument::PROC_STAT);
    }
    // We are a live process ourselves, so a successful walk counts >= 1; a
    // zero is a walk that saw nothing, not an empty machine.
    if let Some(n) = pid_count.filter(|n| *n > 0) {
        g.os_process_count = Some(clamp_i64(n));
        g.instruments.push(instrument::PROC_PIDS);
    }
    g
}

/// Whether a `/proc` directory entry name is a pid (`/proc/[0-9]*`).
pub(crate) fn is_pid_dir_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// `SYSTEM_TIMEOFDAY_INFORMATION` (`NtQuerySystemInformation` class 3): its
/// first field is `BootTime`, a `LARGE_INTEGER` FILETIME (100 ns ticks since
/// 1601-01-01 UTC). 48 bytes on x64; only the first 8 are read.
pub(crate) const TIME_OF_DAY_BYTES: usize = 48;
/// FILETIME ticks between 1601-01-01 and the Unix epoch.
const FILETIME_UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

/// The kernel's own boot instant out of a `SystemTimeOfDayInformation`
/// buffer, floored to the whole second; `None` for a short buffer or a value
/// that is not a plausible instant (zero, or not after the Unix epoch).
///
/// The kernel holds this value for the life of the boot, so every process
/// that reads it — this runner, the same runner after a restart, the next
/// build — publishes the SAME `os_booted_at`. The fallback,
/// `now - GetTickCount64()`, is re-derived per process and can land a second
/// apart across a restart, which would split one boot in two for coord's S7
/// slope (it groups samples by `os_booted_at`).
pub(crate) fn parse_boot_time_of_day(buf: &[u8]) -> Option<DateTime<Utc>> {
    let ticks = i64::try_from(read_u64(buf, 0)?).ok()?;
    let since_epoch = ticks.checked_sub(FILETIME_UNIX_EPOCH_TICKS)?;
    if since_epoch <= 0 {
        return None;
    }
    DateTime::from_timestamp(since_epoch / 10_000_000, 0)
}

/// The boot instant a Windows tick publishes: the kernel's `BootTime` when
/// that query answered, else the `GetTickCount64` derivation held stable by
/// [`stable_boot_time`]. PURE, so the choice is testable off Windows.
pub(crate) fn resolve_boot_time(
    kernel: Option<DateTime<Utc>>,
    prev: Option<DateTime<Utc>>,
    tick_derived: Option<DateTime<Utc>>,
    tolerance_secs: i64,
) -> Option<DateTime<Utc>> {
    kernel.or_else(|| {
        tick_derived
            .map(|fresh| stable_boot_time(prev, fresh, tolerance_secs))
            .or(prev)
    })
}

/// Keep a boot instant stable across ticks — the FALLBACK path only.
///
/// The collector uses the kernel's own `BootTime` ([`parse_boot_time_of_day`])
/// as is. Only when that query fails does it derive one as
/// `now - GetTickCount64()`, which jitters by the tick
/// resolution (~16 ms) and drifts as NTP slews the wall clock. Coord's S7
/// slope groups samples by `os_booted_at`, so a value that moved every tick
/// would split one boot into thousands. The previous value is therefore kept
/// unless the fresh one differs by more than `tolerance_secs` — which only a
/// real clock step can cause, since this process cannot outlive a reboot.
pub(crate) fn stable_boot_time(
    prev: Option<DateTime<Utc>>,
    fresh: DateTime<Utc>,
    tolerance_secs: i64,
) -> DateTime<Utc> {
    match prev {
        Some(p) if (fresh - p).num_seconds().abs() <= tolerance_secs => p,
        _ => fresh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The REAL `NtQueryObject(NULL, ObjectTypesInformation)` buffer captured
    /// on degraded spaceship, 2026-10-19T08:20:52Z (Windows 10.0.26200, during
    /// the `win32kfull.sys` token leak). Not hand-made: plan [policy: testing].
    const OBJECT_TYPES_26200: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/os_health/object_types_26200.bin"
    ));
    /// The REAL `NtQuerySystemInformation(SystemPoolTagInformation)` buffer
    /// from the same capture.
    const POOL_TAGS_26200: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/os_health/pool_tags_26200.bin"
    ));
    /// The capture's own expectations, written by the reference reader that
    /// took the buffers.
    const CAPTURE_26200: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/os_health/capture_26200.json"
    ));

    #[test]
    fn the_incident_object_types_buffer_parses_to_the_captured_counts() {
        assert_eq!(OBJECT_TYPES_26200.len(), 10_056);
        let entries = parse_object_types(OBJECT_TYPES_26200).expect("the real buffer parses");
        assert_eq!(entries.len(), 75);
        let token = entries.iter().find(|e| e.name == "Token").unwrap();
        assert_eq!((token.objects, token.handles), (4_365_962, 2_447));
        let process = entries.iter().find(|e| e.name == "Process").unwrap();
        assert_eq!((process.objects, process.handles), (2_930, 11_843));
        // First and last entries: the walk stayed aligned the whole way.
        assert_eq!(entries[0].name, "Type");
        assert_eq!(entries[74].name, "CrossVmMutant");

        let counts = object_type_counts(OBJECT_TYPES_26200).unwrap();
        assert_eq!(
            counts,
            ObjectTypeCounts {
                token_objects: 4_365_962,
                process_objects: 2_930,
            }
        );
    }

    #[test]
    fn an_object_types_buffer_with_no_process_entry_is_unknown() {
        // The real buffer with ONE byte changed: "Process" -> "Procesz". The
        // Process entry sits at 0x2e8; the final 's' is UTF-16 unit 6.
        let mut b = OBJECT_TYPES_26200.to_vec();
        let s_at = 0x2e8 + 0x68 + 6 * 2;
        assert_eq!(&b[s_at..s_at + 2], &[b's', 0]);
        b[s_at] = b'z';
        let entries = parse_object_types(&b).expect("still a well-formed buffer");
        assert!(entries.iter().any(|e| e.name == "Token"));
        assert!(entries.iter().all(|e| e.name != "Process"));
        // Token is present and correct, and the read is still UNKNOWN: a
        // buffer missing a row every kernel has is a misread.
        assert_eq!(object_type_counts(&b), None);
    }

    #[test]
    fn a_truncated_object_types_buffer_is_unknown_not_a_partial_count() {
        // Cut inside the entry list, including well past Token (0x188) and
        // Process (0x2e8): the rows that fit are real, the read is still
        // UNKNOWN. 10_040 cuts the LAST entry's name (`CrossVmMutant`,
        // 10_024..10_050), so even a buffer missing only its tail is refused.
        for cut in [0, 4, 8, 0x200, 5_000, 10_040] {
            let truncated = &OBJECT_TYPES_26200[..cut];
            assert_eq!(parse_object_types(truncated), None, "cut at {cut}");
            assert_eq!(object_type_counts(truncated), None, "cut at {cut}");
        }
    }

    #[test]
    fn an_object_types_buffer_with_no_token_entry_is_unknown() {
        // The real buffer with ONE byte changed: "Token" -> "Tokem". The
        // Token entry sits at 0x188, its name at 0x188 + 0x68; the 'n' is
        // UTF-16 unit 4.
        let mut b = OBJECT_TYPES_26200.to_vec();
        let n_at = 0x188 + 0x68 + 4 * 2;
        assert_eq!(&b[n_at..n_at + 2], &[b'n', 0]);
        b[n_at] = b'm';
        let entries = parse_object_types(&b).expect("still a well-formed buffer");
        assert!(entries.iter().all(|e| e.name != "Token"));
        assert_eq!(object_type_counts(&b), None);
    }

    #[test]
    fn a_misread_name_length_is_unknown() {
        // An odd UTF-16 byte length is a layout we did not expect.
        let mut b = OBJECT_TYPES_26200.to_vec();
        b[8] = 9; // Type's Length 8 -> 9
        assert_eq!(parse_object_types(&b), None);
        // So is a declared type count past anything Windows has.
        let mut b = OBJECT_TYPES_26200.to_vec();
        b[0..4].copy_from_slice(&5_000u32.to_le_bytes());
        assert_eq!(parse_object_types(&b), None);
    }

    #[test]
    fn the_incident_pool_tag_buffer_parses_to_the_captured_top_eight() {
        let table = parse_pool_tags(POOL_TAGS_26200).expect("the real buffer parses");
        assert_eq!(table.tag_count, 3_379);
        let got: Vec<(&str, i64, i64)> = table
            .top
            .iter()
            .map(|t| (t.tag.as_str(), t.bytes, t.outstanding_allocs))
            .collect();
        assert_eq!(
            got,
            vec![
                ("File", 9_693_669_888, 24_238_128),
                ("Toke", 9_499_968_256, 4_365_962),
                ("SeAt", 1_727_989_760, 17_816_508),
                ("D2d ", 759_218_944, 23_725_583),
                ("SeTd", 628_488_688, 4_365_962),
                ("FMfn", 566_967_584, 1_043_957),
                ("SeTl", 558_843_136, 4_365_962),
                ("NtfF", 544_487_424, 327_216),
            ]
        );
    }

    #[test]
    fn the_literal_expectations_are_the_capture_files_own() {
        // The literals above are typed by hand; this ties them to what the
        // reference reader recorded when it took the buffers, so a typo in
        // either cannot pass silently.
        let c: serde_json::Value = serde_json::from_str(CAPTURE_26200).unwrap();
        assert_eq!(c["object_types_information"]["type_count"], 75);
        assert_eq!(c["object_types_information"]["token"]["objects"], 4_365_962);
        assert_eq!(c["object_types_information"]["process"]["objects"], 2_930);
        assert_eq!(c["pool_tag_information"]["tag_count"], 3_379);
        let top = parse_pool_tags(POOL_TAGS_26200).unwrap().top;
        let expected = c["pool_tag_information"]["top8"].as_array().unwrap();
        assert_eq!(expected.len(), top.len());
        for (want, got) in expected.iter().zip(&top) {
            assert_eq!(want["tag"], got.tag.as_str());
            assert_eq!(want["bytes"], got.bytes);
            assert_eq!(want["outstanding_allocs"], got.outstanding_allocs);
        }
    }

    #[test]
    fn a_truncated_or_empty_pool_tag_buffer_is_unknown() {
        for cut in [0, 7, 8, 48, POOL_TAGS_26200.len() - 1] {
            assert_eq!(
                parse_pool_tags(&POOL_TAGS_26200[..cut]),
                None,
                "cut at {cut}"
            );
        }
        let mut empty = vec![0u8; 8];
        empty[0..4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_pool_tags(&empty), None);
    }

    #[test]
    fn a_tag_byte_outside_printable_ascii_renders_as_one_question_mark() {
        assert_eq!(render_pool_tag(*b"D2d "), "D2d ");
        assert_eq!(render_pool_tag(*b"TEE\0"), "TEE?");
        assert_eq!(render_pool_tag([0, 0x7F, 0x80, 0xFF]), "????");
        assert_eq!(render_pool_tag([b'~', b' ', 0x1F, b'A']), "~ ?A");
    }

    /// The REAL `TEE\0` entry (index 464 of the incident buffer), promoted to
    /// the top by giving it the most bytes: it renders as four printable ASCII
    /// chars, and the published JSON carries no NUL escape — which Postgres
    /// `jsonb` refuses (22P05), dropping the whole sample INSERT.
    #[test]
    fn a_real_nul_bearing_tag_never_reaches_the_wire_as_a_nul() {
        const TEE: usize = 464;
        let mut b = POOL_TAGS_26200.to_vec();
        let o = PT_HEADER_BYTES + TEE * PT_STRIDE;
        assert_eq!(&b[o..o + 4], b"TEE\0", "the fixture's NUL-bearing tag");
        b[o + 16..o + 24].copy_from_slice(&(64u64 << 30).to_le_bytes());
        let top = parse_pool_tags(&b).unwrap().top;
        assert_eq!(top[0].tag, "TEE?");
        for t in &top {
            assert_eq!(t.tag.chars().count(), 4, "{t:?}");
            assert!(t.tag.bytes().all(|c| (0x20..=0x7E).contains(&c)), "{t:?}");
        }
        let wire = serde_json::to_string(&top).unwrap();
        assert!(!wire.contains("\\u0000"), "{wire}");
        assert!(!wire.contains('\0'), "{wire}");
    }

    #[test]
    fn outstanding_allocs_survive_a_wrapped_alloc_counter() {
        // A busy tag whose PagedAllocs has wrapped past 2^32 while its frees
        // have not yet: allocs reads 3, frees 0xFFFF_FFF0, so 19 are live.
        // The nonpaged pool has not wrapped: 1 alloc, 0 frees.
        let mut b = vec![0u8; 8 + 40];
        b[0..4].copy_from_slice(&1u32.to_le_bytes());
        b[8..12].copy_from_slice(b"Wrap");
        b[12..16].copy_from_slice(&3u32.to_le_bytes()); // paged allocs (wrapped)
        b[16..20].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // paged frees
        b[24..32].copy_from_slice(&100u64.to_le_bytes()); // paged used
        b[32..36].copy_from_slice(&1u32.to_le_bytes()); // nonpaged allocs
        b[36..40].copy_from_slice(&0u32.to_le_bytes()); // nonpaged frees
        b[40..48].copy_from_slice(&28u64.to_le_bytes()); // nonpaged used
        let t = parse_pool_tags(&b).unwrap();
        assert_eq!(
            t.top,
            vec![PoolTag {
                tag: "Wrap".into(),
                bytes: 128,
                outstanding_allocs: 20,
            }]
        );
        assert_eq!(outstanding(3, 0xFFFF_FFF0), 19);
        assert_eq!(outstanding(10, 4), 6);
        assert_eq!(outstanding(0, 0), 0);
    }

    #[test]
    fn the_kernel_boot_time_parses_from_time_of_day_information() {
        // 2026-10-15T06:12:00.123Z as FILETIME ticks: floored to the second.
        let unix = 1_792_044_720_i64;
        let ticks = unix * 10_000_000 + 1_230_000 + FILETIME_UNIX_EPOCH_TICKS;
        let mut b = vec![0u8; TIME_OF_DAY_BYTES];
        b[0..8].copy_from_slice(&u64::try_from(ticks).unwrap().to_le_bytes());
        assert_eq!(parse_boot_time_of_day(&b), Some(at(unix)));
        assert_eq!(at(unix).to_rfc3339(), "2026-10-15T06:12:00+00:00");
        // Short, zero or pre-epoch buffers are UNKNOWN.
        assert_eq!(parse_boot_time_of_day(&b[..7]), None);
        assert_eq!(parse_boot_time_of_day(&[0u8; TIME_OF_DAY_BYTES]), None);
        let mut early = vec![0u8; TIME_OF_DAY_BYTES];
        early[0..8].copy_from_slice(&1_000u64.to_le_bytes());
        assert_eq!(parse_boot_time_of_day(&early), None);
    }

    #[test]
    fn the_kernel_boot_time_wins_and_the_tick_derivation_is_only_a_fallback() {
        let kernel = at(1_760_000_000);
        let prev = at(1_760_000_003);
        let tick = at(1_760_000_004);
        // The kernel answered: its value, whatever the derivation says.
        assert_eq!(
            resolve_boot_time(Some(kernel), Some(prev), Some(tick), 120),
            Some(kernel)
        );
        assert_eq!(
            resolve_boot_time(Some(kernel), None, None, 120),
            Some(kernel)
        );
        // It did not: the derivation, held to the previous value within tolerance.
        assert_eq!(
            resolve_boot_time(None, Some(prev), Some(tick), 120),
            Some(prev)
        );
        assert_eq!(resolve_boot_time(None, None, Some(tick), 120), Some(tick));
        // Neither answered this tick: keep what we had.
        assert_eq!(resolve_boot_time(None, Some(prev), None, 120), Some(prev));
        assert_eq!(resolve_boot_time(None, None, None, 120), None);
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn a_failed_windows_read_is_none_not_zero_and_is_not_named_as_answering() {
        let booted = at(1_760_000_000);
        // Both reads failed: every gauge UNKNOWN, only the boot time answers.
        let g = windows_gauges(None, None, Some(booted));
        assert_eq!(g.kernel_paged_bytes, None);
        assert_eq!(g.kernel_nonpaged_bytes, None);
        assert_eq!(g.kernel_handle_count, None);
        assert_eq!(g.os_process_count, None);
        assert_eq!(g.kernel_token_objects, None);
        assert_eq!(g.kernel_process_objects, None);
        assert_eq!(g.os_booted_at, Some(booted));
        assert!(g.instruments.is_empty());

        // The incident's GetPerformanceInfo (capture.json): pages x page size.
        let perf = PerfInfo {
            kernel_paged_pages: 24_282_075_136 / 4_096,
            kernel_nonpaged_pages: 15_075_627_008 / 4_096,
            page_size: 4_096,
            handle_count: 406_585,
            process_count: 1_442,
        };
        let objects = object_type_counts(OBJECT_TYPES_26200);
        let g = windows_gauges(Some(perf), objects, Some(booted));
        assert_eq!(g.kernel_paged_bytes, Some(24_282_075_136));
        assert_eq!(g.kernel_nonpaged_bytes, Some(15_075_627_008));
        assert_eq!(g.kernel_handle_count, Some(406_585));
        assert_eq!(g.os_process_count, Some(1_442));
        assert_eq!(g.kernel_token_objects, Some(4_365_962));
        assert_eq!(g.kernel_process_objects, Some(2_930));
        assert_eq!(g.instruments, vec!["getperformanceinfo", "objecttypes"]);

        // Only the object-types read failed: its two gauges are UNKNOWN and it
        // is absent from the answering list, while GetPerformanceInfo's stand.
        let g = windows_gauges(Some(perf), None, Some(booted));
        assert_eq!(g.kernel_token_objects, None);
        assert_eq!(g.kernel_process_objects, None);
        assert_eq!(g.kernel_handle_count, Some(406_585));
        assert_eq!(g.instruments, vec!["getperformanceinfo"]);

        // A zero page size is not "zero bytes of pool".
        let g = windows_gauges(
            Some(PerfInfo {
                page_size: 0,
                ..perf
            }),
            None,
            Some(booted),
        );
        assert_eq!(g.kernel_paged_bytes, None);
        assert_eq!(g.kernel_nonpaged_bytes, None);
    }

    const MEMINFO: &str = "MemTotal:       16303428 kB\n\
        MemAvailable:   12000000 kB\n\
        Slab:             900000 kB\n\
        SReclaimable:     600000 kB\n\
        SUnreclaim:       300000 kB\n\
        KernelStack:       20000 kB\n\
        PageTables:        40000 kB\n";

    #[test]
    fn linux_gauges_follow_the_d2_mapping_and_leave_windows_axes_not_applicable() {
        let stat = "cpu  1 2 3 4\nbtime 1760000000\nprocesses 12345\n";
        let g = linux_gauges(
            Some(MEMINFO),
            Some("4128\t0\t9223372036854775807\n"),
            Some(stat),
            Some(312),
        );
        // paged = SReclaimable only — NOT Slab, which already contains SUnreclaim.
        assert_eq!(g.kernel_paged_bytes, Some(600_000 * 1024));
        // nonpaged = SUnreclaim + KernelStack + PageTables.
        assert_eq!(
            g.kernel_nonpaged_bytes,
            Some((300_000 + 20_000 + 40_000) * 1024)
        );
        assert_eq!(g.kernel_handle_count, Some(4_128));
        assert_eq!(g.os_booted_at, Some(at(1_760_000_000)));
        assert_eq!(g.os_process_count, Some(312));
        // NOT APPLICABLE on Linux: no Token or Process object type exists.
        assert_eq!(g.kernel_token_objects, None);
        assert_eq!(g.kernel_process_objects, None);
        assert_eq!(
            g.instruments,
            vec!["proc-meminfo", "file-nr", "proc-stat", "proc-pids"]
        );
    }

    #[test]
    fn a_failed_linux_read_is_none_not_zero() {
        let g = linux_gauges(None, None, None, None);
        assert_eq!(g, GaugeReading::default());

        // A meminfo missing one nonpaged term publishes no partial sum.
        let partial = "SReclaimable: 600000 kB\nSUnreclaim: 300000 kB\n";
        let g = linux_gauges(Some(partial), Some("garbage"), Some("cpu 1\n"), Some(0));
        assert_eq!(g.kernel_paged_bytes, Some(600_000 * 1024));
        assert_eq!(g.kernel_nonpaged_bytes, None);
        assert_eq!(g.kernel_handle_count, None);
        assert_eq!(g.os_booted_at, None);
        assert_eq!(
            g.os_process_count, None,
            "a walk that counted 0 is not 0 processes"
        );
        assert_eq!(g.instruments, vec!["proc-meminfo"]);
    }

    #[test]
    fn pid_dir_names_are_all_digits() {
        assert!(is_pid_dir_name("1"));
        assert!(is_pid_dir_name("31337"));
        assert!(!is_pid_dir_name("self"));
        assert!(!is_pid_dir_name("12a"));
        assert!(!is_pid_dir_name(""));
    }

    #[test]
    fn the_boot_time_holds_through_jitter_and_follows_a_clock_step() {
        let first = at(1_760_000_000);
        assert_eq!(stable_boot_time(None, first, 120), first);
        // Tick jitter and NTP slew: held.
        assert_eq!(stable_boot_time(Some(first), at(1_760_000_001), 120), first);
        assert_eq!(stable_boot_time(Some(first), at(1_759_999_900), 120), first);
        // A clock step past the tolerance: followed.
        assert_eq!(
            stable_boot_time(Some(first), at(1_760_000_500), 120),
            at(1_760_000_500)
        );
    }

    #[test]
    fn narrowing_saturates() {
        assert_eq!(clamp_i64(5), 5);
        assert_eq!(clamp_i64(u64::MAX), i64::MAX);
    }
}
