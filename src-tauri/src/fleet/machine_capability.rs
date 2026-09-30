//! How big this machine is — the capability the byte ladder is a fraction of.
//!
//! Plan `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-git-spawns-are-ungated`,
//! Phase 1. Behaviour-neutral: nothing here changes a verdict. It PUBLISHES the
//! machine's size so that Phase 2's shadow derivation
//! ([`crate::resource_guard::commit_ladder_scale`]) has an honest input and so
//! an operator can read the same number off `/health` without shelling out.
//!
//! ## Why the guard needs the SIZE of the box, not only its free headroom
//!
//! Every enforcing memory floor in the fleet reads free COMMIT against an
//! absolute GiB constant (3 / 1.5 GiB for sessions, 4 / 8 GiB for `ci_node`).
//! Those constants are a headroom figure wearing an absolute number's clothes:
//! on the MSI operator box the 1.5 GiB critical floor is 2.1 % of a 71.71 GiB
//! commit limit, and `CreateProcess` had been failing with `os error 1455` for
//! minutes before free commit fell that low. The fix the plan derives is to keep
//! the ladder's ORDER constant and make its SCALE a function of the box — which
//! needs the box's commit limit as a first-class, published fact.
//!
//! ## What each field is, per OS
//!
//! | Field | Windows | Linux |
//! |---|---|---|
//! | `commit_limit` | `ullTotalPageFile` — the commit limit (RAM + pagefile) | `/proc/meminfo` `CommitLimit` |
//! | `commit_charged` | `ullTotalPageFile − ullAvailPageFile` | `/proc/meminfo` `Committed_AS` |
//! | `phys_total` | `ullTotalPhys` | sysinfo `total_memory()` (`MemTotal`) |
//! | `cores` | `available_parallelism` | `available_parallelism` |
//! | `pagefile_*` | the live files (`ExistingPageFiles` + their sizes), checked against `PagingFiles` | UNKNOWN — no Windows pagefile off Windows |
//!
//! `commit_limit` and `phys_total` on Windows come out of the ONE
//! `GlobalMemoryStatusEx` buffer the fleet sample and the spawn gate already
//! fill ([`super::resource_sample`]'s `memory_status`), so the capability costs
//! zero new memory syscalls there — the same argument plan
//! `2026-08-08-memory-floors-watch-commit-and-physical` Phase 1 made for the
//! physical pair.
//!
//! ## The Linux asymmetry this block makes visible
//!
//! Off Windows, `memory_status` writes sysinfo's `MemTotal` / `MemAvailable`
//! into BOTH its commit pair and its physical pair, so the fleet sample's
//! `commit_total_bytes` on Linux is `MemTotal` and a "commit floor" there is a
//! physical floor wearing the name. That reading is deliberately left alone in
//! this phase (it pairs with the free-commit number `ci_node` and coord's CI
//! ranking already decide on — changing one half of the pair would move a
//! verdict). What changes is that `commit_limit` here is the kernel's real
//! `CommitLimit`, so `commit_limit != MemTotal` on a Linux `/health` is itself
//! the proof the asymmetry is now a published fact rather than a silent
//! identity. Note also that Linux ENFORCES `CommitLimit` only under
//! `vm.overcommit_memory = 2`; under the default heuristic mode it is a size
//! measure the kernel does not refuse on, which is still exactly what a SCALE
//! needs, and is why the Linux free-commit reading is a follow-up rather than
//! part of this phase.
//!
//! ## Where it is published, and where it is deliberately NOT (yet)
//!
//! - **`/health` → `machineCapability`** — the whole block, every field, with
//!   `capabilityUnknown` naming the reason for each field that could not be
//!   read.
//! - **The fleet sample** already carries the three capability figures coord
//!   has columns for — `cpu_cores`, `mem_total_bytes` (= `phys_total`) and, on
//!   Windows, `commit_total_bytes` (= `commit_limit`). The pagefile trio and a
//!   Linux `CommitLimit` column are NOT added to the wire: `coord.device_resource_samples`
//!   has no such columns (alembic in qontinui-web is the sole author of
//!   `coord.*` schema), so coord's ingest would route the keys into
//!   `ResourceSampleItem::unknown_fields` and `tracing::warn!` on every 30 s
//!   push — the same inert-and-noisy trade `ResourceSample::thread_count`'s doc
//!   already declines. The migration lands first; the publisher follows it.
//!
//! ## UNKNOWN is a reason, never a zero
//!
//! Every field is an `Option`, and every `None` has an entry in
//! `capability_unknown` saying why. A commit limit of `0` is not "a box with no
//! memory" — it is a probe that answered nonsense — so it is recorded as
//! UNKNOWN and never divided by (served policy `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`). Phase 2's scale takes its 1.0
//! identity arm on exactly that UNKNOWN.

use std::collections::BTreeMap;

use serde::Serialize;

use super::resource_sample::MemoryStatus;

/// Bytes in one MiB — the unit the `PagingFiles` registry value quotes sizes in.
const MIB: u64 = 1024 * 1024;

/// The machine's capability, as published on `/health`.
///
/// Field names are the `/health` JSON keys (camelCase), and the keys of
/// [`Self::capability_unknown`] are the same JSON names — so a reader can join
/// "which field is null" to "why" without a lookup table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineCapability {
    /// The commit limit in bytes: how much memory the OS will promise in total
    /// before `CreateProcess` / `VirtualAlloc` start failing with
    /// `ERROR_COMMITMENT_LIMIT` (Windows) or `CommitLimit` (Linux). The input
    /// Phase 2's ladder scale divides.
    pub(crate) commit_limit: Option<u64>,
    /// Which instrument produced [`Self::commit_limit`] — so a Windows
    /// `ullTotalPageFile` and a Linux `CommitLimit` are never mistaken for one
    /// another on a fleet-wide read. Always present, even when the reading is
    /// UNKNOWN: it names the instrument that was ASKED.
    pub(crate) commit_limit_source: &'static str,
    /// Commit currently charged, in bytes. A READING carried beside the
    /// capability rather than a capability itself, published so the Linux
    /// follow-up (free commit = `CommitLimit − Committed_AS` instead of
    /// `MemAvailable`) can be priced off `/health` before anyone changes the
    /// verdict-bearing number.
    pub(crate) commit_charged: Option<u64>,
    /// Physical RAM visible to the OS, in bytes.
    pub(crate) phys_total: Option<u64>,
    /// Usable parallelism (`available_parallelism`: honours cgroup quotas and
    /// affinity), the same figure the fleet sample publishes as `cpu_cores`.
    pub(crate) cores: Option<u32>,
    /// Bytes currently allocated to pagefiles on disk (the sum of the LIVE
    /// files' sizes — what `Win32_PageFileUsage.AllocatedBaseSize` reports).
    /// `Some(0)` is a real reading: a box with no live pagefile (and none
    /// configured — see [`assess_pagefile`] for why both must agree).
    pub(crate) pagefile_allocated: Option<u64>,
    /// The configured maximum pagefile size in bytes, summed over every file.
    /// UNKNOWN for a system-managed pagefile, which has no configured maximum —
    /// Windows grows it on demand — and UNKNOWN while a changed configuration is
    /// pending a reboot (see [`configuration_is_live`]).
    pub(crate) pagefile_max: Option<u64>,
    /// Whether the commit limit is FIXED: every pagefile has
    /// `initial == maximum`, or there is no pagefile at all. On a fixed box
    /// "the paging file is too small" (`os error 1455`) is TERMINAL — Windows
    /// will never grow the file — whereas on a system-managed one it is often
    /// transient. See [`pagefile_fixed`]. UNKNOWN while a changed configuration
    /// is pending a reboot.
    pub(crate) pagefile_fixed: Option<bool>,
    /// Why each `None` field above is `None`, keyed by its JSON name. Empty
    /// when every field was read.
    pub(crate) capability_unknown: BTreeMap<&'static str, String>,
}

/// How one `PagingFiles` registry entry sizes its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PagefileSize {
    /// `"C:\pagefile.sys 40960 40960"` — an operator-chosen initial and
    /// maximum, in MiB.
    Custom { initial_mib: u64, max_mib: u64 },
    /// `"C:\pagefile.sys"`, `"C:\pagefile.sys 0 0"` or `"?:\pagefile.sys"` —
    /// Windows sizes (and grows) the file itself. `?:` is "automatically manage
    /// paging file size for all drives".
    SystemManaged,
}

/// One parsed line of the `PagingFiles` registry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PagefileEntry {
    pub(crate) path: String,
    pub(crate) size: PagefileSize,
}

/// One pagefile that is live THIS boot (an `ExistingPageFiles` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LivePagefile {
    /// Normalised by [`normalize_pagefile_path`].
    pub(crate) path: String,
    /// Its size on disk right now — can fail per file.
    pub(crate) size: Result<u64, String>,
}

/// What the pagefile probe found. The two halves come from different moments
/// and must never be conflated:
///
/// - `configured` is `PagingFiles` — the **NEXT-BOOT** configuration. System
///   Properties writes it the instant the operator clicks OK, and it may take
///   effect only after a reboot.
/// - `live` is `ExistingPageFiles` plus each file's size — what is in effect
///   THIS boot.
///
/// So the allocated figure is always summed from `live`, and the configuration
/// describes growability only once [`configuration_is_live`] shows it is the
/// one actually in effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PagefileReading {
    pub(crate) configured: Result<Vec<PagefileEntry>, String>,
    /// `Ok(None)` = the `ExistingPageFiles` value is ABSENT (distinct from
    /// unreadable) — see [`assess_pagefile`] for the one case that may read it.
    pub(crate) live: Result<Option<Vec<LivePagefile>>, String>,
    /// When the `Memory Management` key was last written, unix seconds.
    pub(crate) config_written_at: Result<i64, String>,
    /// When the system booted, unix seconds.
    pub(crate) booted_at: Result<i64, String>,
}

/// Writes to the `Memory Management` key within this many seconds of boot are
/// the boot's OWN writes, not a pending change (300 s).
///
/// The session manager rewrites `ExistingPageFiles` — a value in this very key
/// — at every boot, so the key's last-write time is ALWAYS a little after boot.
/// Without a grace window every machine would read as "changed since boot".
/// 300 s is generous for that early-boot write and short enough that an
/// operator is unlikely to have opened System Properties and changed the
/// pagefile inside it. That residual — a change made within 5 minutes of boot
/// — is one of the limits [`assess_pagefile`]'s doc names.
pub(crate) const BOOT_WRITE_GRACE_SECS: i64 = 300;

/// Was the pagefile configuration written after this boot (beyond the boot's
/// own writes)? PURE.
pub(crate) fn written_since_boot(written_at: i64, booted_at: i64) -> bool {
    written_at > booted_at.saturating_add(BOOT_WRITE_GRACE_SECS)
}

/// A Windows `FILETIME` (100 ns ticks since 1601-01-01 UTC) as unix seconds.
/// `None` for a zero (never-set) time. PURE.
pub(crate) fn filetime_to_unix_secs(high: u32, low: u32) -> Option<i64> {
    const TICKS_PER_SEC: u64 = 10_000_000;
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    let ticks = (u64::from(high) << 32) | u64::from(low);
    if ticks == 0 {
        return None;
    }
    i64::try_from(ticks / TICKS_PER_SEC)
        .ok()
        .map(|secs| secs - EPOCH_DIFF_SECS)
}

/// What the pagefile reading establishes: the bytes allocated now, and the
/// configuration in effect this boot (or why neither can be said). PURE.
///
/// ## Allocation
///
/// Always the LIVE files. The one case with no live list that still answers is
/// a genuinely pagefile-less box: `ExistingPageFiles` absent AND `PagingFiles`
/// empty — both halves agreeing — which is a real `0`. Any other absence is
/// UNKNOWN: nothing here can tell "no pagefile" from "a value that did not
/// answer" on one half alone.
///
/// ## Configuration in effect — three disqualifiers, any one is enough
///
/// 1. **Written since boot.** `PagingFiles` may take effect only after a
///    reboot, and a same-path change (fixed 40960/40960 → system-managed, or →
///    16384/65536 while the live file still sits inside the new band) is
///    invisible to a path/size comparison. The registry CAN answer "was this
///    key written after this boot?" honestly, so a write past
///    [`BOOT_WRITE_GRACE_SECS`] after boot is "configuration changed since boot
///    (pending reboot)". An unreadable write time or boot time is UNKNOWN, not
///    live.
/// 2. **Paths** and 3. **sizes** disagree with the live files —
///    [`configuration_is_live`].
///
/// ## Residual limits, stated rather than hidden
///
/// - The write time is per KEY, not per value: any post-boot write to another
///   `Memory Management` value (some security-mitigation tooling writes there)
///   also reads as "changed since boot". That errs toward UNKNOWN, never toward
///   a wrong answer.
/// - A change made within [`BOOT_WRITE_GRACE_SECS`] of boot that also keeps the
///   same paths and a live size inside the new band is not detected.
/// - The boot time comes from the tick count (`sysinfo::System::boot_time`);
///   with Fast Startup a "shutdown" is a hibernation that does not reset it.
pub(crate) fn assess_pagefile(
    reading: PagefileReading,
) -> (Result<u64, String>, Result<Vec<PagefileEntry>, String>) {
    let PagefileReading {
        configured,
        live,
        config_written_at,
        booted_at,
    } = reading;
    match live {
        Err(reason) => {
            let in_effect = configured.and_then(|_| {
                Err(format!(
                    "the live pagefile list is unreadable ({reason}), so whether PagingFiles \
                     (the next-boot configuration) is the one in effect cannot be established"
                ))
            });
            (Err(reason), in_effect)
        }
        Ok(None) => match configured {
            Ok(entries) if entries.is_empty() => (Ok(0), Ok(entries)),
            Ok(entries) => {
                let reason = format!(
                    "ExistingPageFiles is absent while PagingFiles names {:?} — the live \
                     pagefiles cannot be established",
                    entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>()
                );
                (Err(reason.clone()), Err(reason))
            }
            Err(reason) => (
                Err(format!(
                    "ExistingPageFiles is absent and PagingFiles is unreadable ({reason})"
                )),
                Err(reason),
            ),
        },
        Ok(Some(live)) => {
            let allocated = live.iter().try_fold(0u64, |acc, l| match &l.size {
                Ok(bytes) => Ok(acc.saturating_add(*bytes)),
                Err(reason) => Err(reason.clone()),
            });
            let in_effect = configured.and_then(|configured| {
                let (written_at, booted_at) = match (config_written_at, booted_at) {
                    (Ok(w), Ok(b)) => (w, b),
                    (Err(r), _) | (_, Err(r)) => {
                        return Err(format!(
                            "whether the pagefile configuration changed since boot cannot be \
                             established ({r})"
                        ))
                    }
                };
                if written_since_boot(written_at, booted_at) {
                    return Err(format!(
                        "configuration changed since boot (pending reboot): the Memory \
                         Management key was written {}s after boot",
                        written_at - booted_at
                    ));
                }
                if !configuration_is_live(&configured, &live) {
                    return Err(format!(
                        "configuration pending reboot: PagingFiles (applied at the next boot) \
                         names {:?}, which does not match the live pagefiles {:?}",
                        configured
                            .iter()
                            .map(|e| e.path.as_str())
                            .collect::<Vec<_>>(),
                        live.iter()
                            .map(|l| (l.path.as_str(), l.size.as_ref().ok()))
                            .collect::<Vec<_>>(),
                    ));
                }
                Ok(configured)
            });
            (allocated, in_effect)
        }
    }
}

/// Everything [`assemble`] needs, each input either a value or the reason it
/// could not be read. The one impure step per platform ([`probe_from`]) fills
/// this; the assembly itself is pure so the UNKNOWN discipline is testable on
/// every OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityInputs {
    pub(crate) commit_limit: Result<u64, String>,
    pub(crate) commit_limit_source: &'static str,
    pub(crate) commit_charged: Result<u64, String>,
    pub(crate) phys_total: Result<u64, String>,
    pub(crate) cores: Result<u32, String>,
    pub(crate) pagefile: Result<PagefileReading, String>,
}

/// Parse the `PagingFiles` `REG_MULTI_SZ` lines. PURE.
///
/// Blank lines are skipped — an empty value (or one holding only `""`) is how
/// Windows spells "no pagefile", and that is a real configuration, not an
/// error. A line whose sizes are present but unparseable is an `Err`: guessing
/// "system-managed" for it would publish a `pagefile_fixed = false` nobody
/// measured.
pub(crate) fn parse_paging_files<S: AsRef<str>>(lines: &[S]) -> Result<Vec<PagefileEntry>, String> {
    let mut entries = Vec::new();
    for raw in lines {
        let line = raw.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let (path, size) = match tokens.as_slice() {
            [path] => (path.to_string(), PagefileSize::SystemManaged),
            [path, initial, max] => {
                let parse = |t: &str| {
                    t.parse::<u64>().map_err(|_| {
                        format!("PagingFiles entry {line:?}: size {t:?} is not a number")
                    })
                };
                let (initial_mib, max_mib) = (parse(initial)?, parse(max)?);
                let size = if initial_mib == 0 && max_mib == 0 {
                    PagefileSize::SystemManaged
                } else {
                    PagefileSize::Custom {
                        initial_mib,
                        max_mib,
                    }
                };
                (path.to_string(), size)
            }
            _ => {
                return Err(format!(
                    "PagingFiles entry {line:?} is neither `<path>` nor `<path> <initial> <max>`"
                ))
            }
        };
        // `?:\pagefile.sys` is the "manage all drives automatically" spelling,
        // whatever sizes happen to follow it.
        let size = if path.starts_with("?:") {
            PagefileSize::SystemManaged
        } else {
            size
        };
        entries.push(PagefileEntry { path, size });
    }
    Ok(entries)
}

/// Is the commit limit fixed? PURE.
///
/// `true` when every pagefile is custom-sized with `initial == max` — Windows
/// never grows such a file, so the commit limit is `RAM + Σ max` for the life
/// of the boot and a 1455 is terminal. `false` as soon as ANY file is
/// system-managed or custom-sized with room to grow (`initial < max`), because
/// then the OS can extend the file and the next attempt may succeed.
///
/// **No pagefile at all is `true`**, and that is a decision, not an accident:
/// the field answers "can the commit limit grow by pagefile expansion?", and
/// with no pagefile it cannot — the limit is physical RAM alone, and running
/// out of it is exactly as terminal as on a fixed file. Reporting `false` there
/// would tell an operator reading a 1455 that the OS might recover when it
/// never will.
pub(crate) fn pagefile_fixed(entries: &[PagefileEntry]) -> bool {
    entries.iter().all(|e| match e.size {
        PagefileSize::Custom {
            initial_mib,
            max_mib,
        } => initial_mib == max_mib,
        PagefileSize::SystemManaged => false,
    })
}

/// The configured maximum, summed, in bytes — or why there is none. PURE.
fn pagefile_max(entries: &[PagefileEntry]) -> Result<u64, String> {
    let mut total: u64 = 0;
    for e in entries {
        match e.size {
            PagefileSize::Custom { max_mib, .. } => {
                total = total.saturating_add(max_mib.saturating_mul(MIB));
            }
            PagefileSize::SystemManaged => {
                return Err(format!(
                    "{} is system-managed: Windows grows it on demand, so no maximum is configured",
                    e.path
                ))
            }
        }
    }
    Ok(total)
}

/// Normalise a pagefile path for comparison: trimmed, the NT `\??\` prefix
/// `ExistingPageFiles` carries stripped, lower-cased (Windows paths are
/// case-insensitive, and the two registry values need not agree on case).
/// PURE.
pub(crate) fn normalize_pagefile_path(raw: &str) -> String {
    let t = raw.trim();
    t.strip_prefix(r"\??\").unwrap_or(t).to_ascii_lowercase()
}

/// Is the `PagingFiles` configuration the one in effect THIS boot? PURE.
///
/// `PagingFiles` is written immediately and applied at the next boot, so after
/// an operator changes it the registry describes a machine that does not exist
/// yet. Two checks, both against what is live:
///
/// 1. **Paths.** The configured file set must equal the live set. A `?:`
///    ("manage all drives automatically") entry matches any non-empty live set
///    whose every file has the same name after its drive letter.
/// 2. **Sizes.** For each custom-sized entry, the live file's size must lie in
///    `[initial, max]` — a custom pagefile is created at `initial` and only ever
///    grows toward `max`, so a size outside that band means the sizes changed
///    and are pending. A live size that could not be read cannot rule that out,
///    so it answers `false` too: "cannot establish" is not "consistent".
///
/// No pagefile configured and none live is consistent (a box with no pagefile).
pub(crate) fn configuration_is_live(configured: &[PagefileEntry], live: &[LivePagefile]) -> bool {
    use std::collections::BTreeSet;
    let live_paths: BTreeSet<&str> = live.iter().map(|l| l.path.as_str()).collect();
    let (automatic, explicit): (Vec<&PagefileEntry>, Vec<&PagefileEntry>) =
        configured.iter().partition(|e| e.path.starts_with("?:"));

    let paths_match = if automatic.is_empty() {
        let configured_paths: BTreeSet<String> = explicit
            .iter()
            .map(|e| normalize_pagefile_path(&e.path))
            .collect();
        configured_paths
            .iter()
            .map(String::as_str)
            .eq(live_paths.iter().copied())
    } else {
        // `?:\pagefile.sys` → tail `:\pagefile.sys`, matched against each live
        // path's tail after its one-byte drive letter.
        let tails: BTreeSet<String> = automatic
            .iter()
            .filter_map(|e| {
                normalize_pagefile_path(&e.path)
                    .get(1..)
                    .map(str::to_string)
            })
            .collect();
        !live.is_empty()
            && live_paths
                .iter()
                .all(|p| p.get(1..).is_some_and(|t| tails.contains(t)))
            && explicit
                .iter()
                .all(|e| live_paths.contains(normalize_pagefile_path(&e.path).as_str()))
    };
    if !paths_match {
        return false;
    }

    explicit.iter().all(|e| match e.size {
        PagefileSize::SystemManaged => true,
        PagefileSize::Custom {
            initial_mib,
            max_mib,
        } => {
            let path = normalize_pagefile_path(&e.path);
            live.iter()
                .filter(|l| l.path == path)
                .all(|l| match l.size {
                    Ok(bytes) => {
                        bytes >= initial_mib.saturating_mul(MIB)
                            && bytes <= max_mib.saturating_mul(MIB)
                    }
                    Err(_) => false,
                })
        }
    })
}

/// Pull `CommitLimit` and `Committed_AS` out of `/proc/meminfo` text. PURE.
///
/// Each is `Err` with a reason when its key is absent or unparseable — never a
/// substitute. In particular a missing `CommitLimit` does NOT fall back to
/// `MemTotal`: that silent identity is precisely what this phase exists to end.
pub(crate) fn parse_commit_meminfo(text: &str) -> (Result<u64, String>, Result<u64, String>) {
    let read = |key: &str| {
        super::resource_sample::meminfo_kb(text, key)
            .ok_or_else(|| format!("/proc/meminfo carries no parseable `{key}` line"))
    };
    (read("CommitLimit:"), read("Committed_AS:"))
}

/// Fold [`CapabilityInputs`] into the published block. PURE.
///
/// A zero `commit_limit` / `phys_total` / `cores` is recorded as UNKNOWN with a
/// reason: none of those can be zero on a machine that is running this code, so
/// a zero is a probe that answered nonsense. The pagefile figures are the
/// exception — `0` allocated bytes on a box with no pagefile is a measurement.
pub(crate) fn assemble(inputs: CapabilityInputs) -> MachineCapability {
    let mut unknown = BTreeMap::new();
    let mut known_nonzero = |name: &'static str, r: Result<u64, String>| match r {
        Ok(0) => {
            unknown.insert(
                name,
                "the probe reported 0 — UNKNOWN, never a machine with none".to_string(),
            );
            None
        }
        Ok(v) => Some(v),
        Err(reason) => {
            unknown.insert(name, reason);
            None
        }
    };
    let commit_limit = known_nonzero("commitLimit", inputs.commit_limit);
    let phys_total = known_nonzero("physTotal", inputs.phys_total);
    let cores = known_nonzero("cores", inputs.cores.map(u64::from))
        .map(|c| u32::try_from(c).unwrap_or(u32::MAX));

    let commit_charged = match inputs.commit_charged {
        Ok(v) => Some(v),
        Err(reason) => {
            unknown.insert("commitCharged", reason);
            None
        }
    };

    let (pagefile_allocated, pagefile_max_bytes, pagefile_fixed_flag) = match inputs.pagefile {
        Ok(reading) => {
            let (allocated, in_effect) = assess_pagefile(reading);
            let allocated = match allocated {
                Ok(v) => Some(v),
                Err(reason) => {
                    unknown.insert("pagefileAllocated", reason);
                    None
                }
            };
            match in_effect {
                Ok(entries) => {
                    let max = match pagefile_max(&entries) {
                        Ok(v) => Some(v),
                        Err(reason) => {
                            unknown.insert("pagefileMax", reason);
                            None
                        }
                    };
                    (allocated, max, Some(pagefile_fixed(&entries)))
                }
                Err(reason) => {
                    unknown.insert("pagefileMax", reason.clone());
                    unknown.insert("pagefileFixed", reason);
                    (allocated, None, None)
                }
            }
        }
        Err(reason) => {
            for name in ["pagefileAllocated", "pagefileMax", "pagefileFixed"] {
                unknown.insert(name, reason.clone());
            }
            (None, None, None)
        }
    };

    MachineCapability {
        commit_limit,
        commit_limit_source: inputs.commit_limit_source,
        commit_charged,
        phys_total,
        cores,
        pagefile_allocated,
        pagefile_max: pagefile_max_bytes,
        pagefile_fixed: pagefile_fixed_flag,
        capability_unknown: unknown,
    }
}

/// Every capability field's `/health` name, in wire order.
pub(crate) const CAPABILITY_FIELDS: [&str; 7] = [
    "commitLimit",
    "commitCharged",
    "physTotal",
    "cores",
    "pagefileAllocated",
    "pagefileMax",
    "pagefileFixed",
];

/// Which instrument this platform reads the commit limit from.
#[cfg(windows)]
pub(crate) const COMMIT_LIMIT_SOURCE: &str = "GlobalMemoryStatusEx.ullTotalPageFile";
/// Which instrument this platform reads the commit limit from.
#[cfg(not(windows))]
pub(crate) const COMMIT_LIMIT_SOURCE: &str = "/proc/meminfo CommitLimit";

/// A capability in which NOTHING could be read: every field null, every field
/// carrying `reason` — for a caller whose probe never ran at all (`/health`'s
/// blocking task failing to join). The same shape as a partial read, so a
/// consumer needs one code path, not two.
pub(crate) fn unknown_everywhere(reason: &str) -> MachineCapability {
    MachineCapability {
        commit_limit: None,
        commit_limit_source: COMMIT_LIMIT_SOURCE,
        commit_charged: None,
        phys_total: None,
        cores: None,
        pagefile_allocated: None,
        pagefile_max: None,
        pagefile_fixed: None,
        capability_unknown: CAPABILITY_FIELDS
            .iter()
            .map(|name| (*name, reason.to_string()))
            .collect(),
    }
}

/// The machine's capability, built around a memory reading the caller already
/// took. Blocking (a registry read and pagefile `stat`s on Windows, a
/// `/proc/meminfo` read on Linux) — call it from a blocking context.
///
/// Takes the reading rather than making its own so the fleet sampler, which
/// already holds one, does not pay a second `GlobalMemoryStatusEx` for the same
/// four numbers. **Never called on the spawn path**: the gate's own reading
/// stays [`super::resource_sample::spawn_gate_reading`], one syscall and
/// nothing else.
pub(super) fn probe_from(reading: Option<MemoryStatus>) -> MachineCapability {
    assemble(platform_inputs(reading))
}

/// [`probe_from`] with a fresh reading — for `/health`, which holds none.
pub(crate) fn probe() -> MachineCapability {
    probe_from(super::resource_sample::memory_status())
}

fn cores() -> Result<u32, String> {
    std::thread::available_parallelism()
        .map(|n| n.get().min(u32::MAX as usize) as u32)
        .map_err(|e| format!("available_parallelism failed: {e}"))
}

#[cfg(windows)]
fn platform_inputs(reading: Option<MemoryStatus>) -> CapabilityInputs {
    const NO_READING: &str = "GlobalMemoryStatusEx failed";
    CapabilityInputs {
        commit_limit: reading
            .map(|m| m.commit_total)
            .ok_or_else(|| NO_READING.to_string()),
        commit_limit_source: COMMIT_LIMIT_SOURCE,
        commit_charged: reading
            .map(|m| m.commit_total.saturating_sub(m.commit_available))
            .ok_or_else(|| NO_READING.to_string()),
        phys_total: reading
            .map(|m| m.phys_total)
            .ok_or_else(|| NO_READING.to_string()),
        cores: cores(),
        pagefile: windows_pagefile::reading(),
    }
}

#[cfg(not(windows))]
fn platform_inputs(reading: Option<MemoryStatus>) -> CapabilityInputs {
    let meminfo = std::fs::read_to_string("/proc/meminfo")
        .map_err(|e| format!("/proc/meminfo unreadable: {e}"));
    let (commit_limit, commit_charged) = match &meminfo {
        Ok(text) => parse_commit_meminfo(text),
        Err(reason) => (Err(reason.clone()), Err(reason.clone())),
    };
    CapabilityInputs {
        commit_limit,
        commit_limit_source: COMMIT_LIMIT_SOURCE,
        commit_charged,
        phys_total: non_windows_phys_total(reading, meminfo.as_deref()),
        cores: cores(),
        pagefile: Err(
            "no Windows pagefile on this OS — commit is backed by swap here, not a \
                       pagefile (see the fleet sample's swap_total_bytes)"
                .to_string(),
        ),
    }
}

/// Physical RAM off Windows. PURE over its two sources.
///
/// `memory_status` withholds its WHOLE reading when `MemAvailable` is 0 (it
/// will not publish half a pair), which says nothing about how much RAM the box
/// has. So a missing reading falls back to `/proc/meminfo` `MemTotal` — the line
/// sysinfo itself reads on Linux — and only when neither answers is the field
/// UNKNOWN, with a reason naming both.
#[cfg(not(windows))]
fn non_windows_phys_total(
    reading: Option<MemoryStatus>,
    meminfo: Result<&str, &String>,
) -> Result<u64, String> {
    if let Some(m) = reading {
        return Ok(m.phys_total);
    }
    match meminfo {
        Ok(text) => super::resource_sample::meminfo_kb(text, "MemTotal:").ok_or_else(|| {
            "no memory reading (sysinfo reported MemAvailable = 0, or failed) and \
             /proc/meminfo carries no parseable `MemTotal:` line"
                .to_string()
        }),
        Err(reason) => Err(format!(
            "no memory reading (sysinfo reported MemAvailable = 0, or failed) and {reason}"
        )),
    }
}

/// The Windows pagefile probe: the registry, not WMI.
///
/// ## Why the registry and not `Win32_PageFileSetting` / `Win32_PageFileUsage`
///
/// Both WMI classes are views over the same two `Memory Management` registry
/// values read here — `PagingFiles` (the configuration) and `ExistingPageFiles`
/// (the live files) — so the registry is the source, not an approximation of
/// it. Reading it directly is a handful of registry calls, needs no COM apartment on the
/// calling thread and forks nothing. WMI costs hundreds of milliseconds, needs
/// COM initialised, and — measured in this very plan's incident — is one of the
/// things that FAILS under commit exhaustion: a .NET assembly load for the WMI
/// transcript scan reported a phantom "missing `System.Management.Automation`"
/// seconds before abort #4. A capability probe that breaks on the condition it
/// describes is the wrong instrument.
///
/// ## Read fresh every time, and never trusted as live on its own
///
/// `PagingFiles` is the **next-boot** configuration: System Properties writes it
/// immediately and may take effect only after a reboot. So it is NOT cached for
/// the process lifetime (a change made mid-run would otherwise be published as
/// the live state until the runner restarted), and it describes growability
/// only once [`configuration_is_live`] has matched it against
/// `ExistingPageFiles` — the files actually live this boot — and their sizes.
/// Both values and the sizes are re-read on every call — cheap, and only ever
/// from the ~30 s fleet sampler or `/health`'s blocking task (never the async
/// runtime, never the spawn path), and a system-managed pagefile GROWS under
/// load, so a cached size would publish the one number most likely to have
/// changed at the moment it matters.
///
/// `ExistingPageFiles` absent is UNKNOWN, not "no pagefile": nothing here can
/// tell a box with no pagefile from a registry that did not answer.
#[cfg(windows)]
mod windows_pagefile {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    use super::{
        filetime_to_unix_secs, normalize_pagefile_path, parse_paging_files, LivePagefile,
        PagefileReading,
    };

    const MEMORY_MANAGEMENT: &str =
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management";

    fn memory_management() -> Result<RegKey, String> {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(MEMORY_MANAGEMENT)
            .map_err(|e| format!("HKLM\\{MEMORY_MANAGEMENT} unreadable: {e}"))
    }

    /// The live pagefiles, each with its size. `ExistingPageFiles` lists them
    /// in NT form (`\??\C:\pagefile.sys`); `std::fs::metadata` reads an in-use
    /// `pagefile.sys` through its `FindFirstFileW` fallback.
    fn live(key: &RegKey) -> Result<Option<Vec<LivePagefile>>, String> {
        let raw: Vec<String> = match key.get_value("ExistingPageFiles") {
            Ok(raw) => raw,
            // ABSENT is not unreadable — `assess_pagefile` may read it as a
            // pagefile-less box when `PagingFiles` agrees.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("ExistingPageFiles registry value unreadable: {e}")),
        };
        Ok(Some(
            raw.iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| {
                    let path = normalize_pagefile_path(s);
                    let size = std::fs::metadata(&path)
                        .map(|m| m.len())
                        .map_err(|e| format!("pagefile {path} could not be stat'ed: {e}"));
                    LivePagefile { path, size }
                })
                .collect(),
        ))
    }

    /// When the `Memory Management` key was last written, unix seconds.
    fn config_written_at(key: &RegKey) -> Result<i64, String> {
        let info = key
            .query_info()
            .map_err(|e| format!("Memory Management key metadata unreadable: {e}"))?;
        let ft = &info.last_write_time;
        filetime_to_unix_secs(ft.dwHighDateTime, ft.dwLowDateTime)
            .ok_or_else(|| "Memory Management key reports no last-write time".to_string())
    }

    /// When this boot started, unix seconds (`sysinfo` derives it from the
    /// tick count on Windows).
    fn booted_at() -> Result<i64, String> {
        match i64::try_from(sysinfo::System::boot_time()) {
            Ok(0) | Err(_) => Err("system boot time unavailable".to_string()),
            Ok(t) => Ok(t),
        }
    }

    pub(super) fn reading() -> Result<PagefileReading, String> {
        let key = memory_management()?;
        let configured = key
            .get_value::<Vec<String>, _>("PagingFiles")
            .map_err(|e| format!("PagingFiles registry value unreadable: {e}"))
            .and_then(|lines| parse_paging_files(&lines));
        Ok(PagefileReading {
            configured,
            live: live(&key),
            config_written_at: config_written_at(&key),
            booted_at: booted_at(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn custom(path: &str, initial_mib: u64, max_mib: u64) -> PagefileEntry {
        PagefileEntry {
            path: path.to_string(),
            size: PagefileSize::Custom {
                initial_mib,
                max_mib,
            },
        }
    }

    /// The MSI operator box as measured 2026-09-23: one 40960 MiB file,
    /// initial == maximum.
    #[test]
    fn the_msi_pagefile_parses_as_fixed() {
        let entries = parse_paging_files(&[r"C:\pagefile.sys 40960 40960"]).unwrap();
        assert_eq!(entries, vec![custom(r"C:\pagefile.sys", 40960, 40960)]);
        assert!(pagefile_fixed(&entries));
        assert_eq!(pagefile_max(&entries), Ok(40 * GIB));
    }

    #[test]
    fn every_system_managed_spelling_is_not_fixed() {
        for line in [
            r"C:\pagefile.sys",
            r"C:\pagefile.sys 0 0",
            r"?:\pagefile.sys",
            r"?:\pagefile.sys 4096 8192",
        ] {
            let entries = parse_paging_files(&[line]).unwrap();
            assert_eq!(entries[0].size, PagefileSize::SystemManaged, "{line}");
            assert!(!pagefile_fixed(&entries), "{line} must not read as fixed");
            assert!(
                pagefile_max(&entries).is_err(),
                "{line} has no configured max"
            );
        }
    }

    #[test]
    fn a_growable_custom_file_is_not_fixed() {
        let entries = parse_paging_files(&[r"C:\pagefile.sys 4096 16384"]).unwrap();
        assert!(!pagefile_fixed(&entries));
        assert_eq!(pagefile_max(&entries), Ok(16 * GIB));
    }

    /// One growable file among fixed ones un-fixes the whole limit: the OS can
    /// still grow THAT file, so a 1455 is not terminal.
    #[test]
    fn one_growable_file_among_fixed_ones_is_not_fixed() {
        let entries =
            parse_paging_files(&[r"C:\pagefile.sys 8192 8192", r"D:\pagefile.sys"]).unwrap();
        assert!(!pagefile_fixed(&entries));
        let both_fixed =
            parse_paging_files(&[r"C:\pagefile.sys 8192 8192", r"D:\pagefile.sys 1024 1024"])
                .unwrap();
        assert!(pagefile_fixed(&both_fixed));
        assert_eq!(pagefile_max(&both_fixed), Ok(9 * GIB));
    }

    /// No pagefile is FIXED (the commit limit cannot grow) with a real max and
    /// allocation of zero — see `pagefile_fixed`'s doc for why that is decided,
    /// not defaulted.
    #[test]
    fn no_pagefile_is_fixed_with_a_real_zero_max() {
        for lines in [Vec::<&str>::new(), vec![""], vec!["  ", ""]] {
            let entries = parse_paging_files(&lines).unwrap();
            assert!(entries.is_empty());
            assert!(pagefile_fixed(&entries));
            assert_eq!(pagefile_max(&entries), Ok(0));
        }
    }

    #[test]
    fn an_unparseable_size_is_an_error_not_a_guess() {
        assert!(parse_paging_files(&[r"C:\pagefile.sys forty 40960"]).is_err());
        assert!(parse_paging_files(&[r"C:\pagefile.sys 40960"]).is_err());
    }

    const MEMINFO_FIXTURE: &str = "\
MemTotal:       395144208 kB
MemFree:        12345678 kB
MemAvailable:   301234567 kB
SwapTotal:       8388604 kB
SwapFree:        8000000 kB
CommitLimit:    205960708 kB
Committed_AS:   98765432 kB
";

    /// Linux `commit_limit` is `CommitLimit`, and it is NOT `MemTotal` — the
    /// asymmetry Phase 1 makes visible.
    #[test]
    fn meminfo_commit_pair_is_read_from_its_own_keys() {
        let (limit, charged) = parse_commit_meminfo(MEMINFO_FIXTURE);
        assert_eq!(limit, Ok(205_960_708 * 1024));
        assert_eq!(charged, Ok(98_765_432 * 1024));
        assert_ne!(
            limit,
            Ok(395_144_208 * 1024),
            "CommitLimit must never be MemTotal"
        );
    }

    #[test]
    fn meminfo_without_commit_keys_is_unknown_never_memtotal() {
        let text = "MemTotal:       395144208 kB\nMemAvailable:   301234567 kB\n";
        let (limit, charged) = parse_commit_meminfo(text);
        assert!(limit.is_err());
        assert!(charged.is_err());
        let cap = assemble(CapabilityInputs {
            commit_limit: limit,
            commit_limit_source: "/proc/meminfo CommitLimit",
            commit_charged: charged,
            phys_total: Ok(395_144_208 * 1024),
            cores: Ok(48),
            pagefile: Err("no pagefile on this OS".into()),
        });
        assert_eq!(cap.commit_limit, None);
        assert!(cap.capability_unknown.contains_key("commitLimit"));
        assert!(cap.capability_unknown.contains_key("commitCharged"));
        for name in ["pagefileAllocated", "pagefileMax", "pagefileFixed"] {
            assert!(cap.capability_unknown.contains_key(name), "{name}");
        }
        assert_eq!(cap.phys_total, Some(395_144_208 * 1024));
    }

    fn msi_inputs() -> CapabilityInputs {
        CapabilityInputs {
            commit_limit: Ok(75_191_424 * 1024),
            commit_limit_source: "GlobalMemoryStatusEx.ullTotalPageFile",
            commit_charged: Ok((75_191_424 - 32_678_912) * 1024),
            phys_total: Ok(33_248_384 * 1024),
            cores: Ok(16),
            pagefile: Ok(msi_pagefile(Ok(40 * GIB))),
        }
    }

    /// Boot at a fixed instant, and the key's last write 10 s later — the
    /// session manager's own boot-time write, inside the grace window.
    const BOOT: i64 = 1_790_000_000;

    fn at_boot() -> PagefileReading {
        PagefileReading {
            configured: Ok(vec![]),
            live: Ok(Some(vec![])),
            config_written_at: Ok(BOOT + 10),
            booted_at: Ok(BOOT),
        }
    }

    fn live(path: &str, size: Result<u64, String>) -> LivePagefile {
        LivePagefile {
            path: normalize_pagefile_path(path),
            size,
        }
    }

    /// The MSI box's pagefile as the registry reports it: configured
    /// `C:\pagefile.sys 40960 40960`, live as `\??\C:\pagefile.sys`.
    fn msi_pagefile(size: Result<u64, String>) -> PagefileReading {
        PagefileReading {
            configured: Ok(vec![custom(r"C:\pagefile.sys", 40960, 40960)]),
            live: Ok(Some(vec![live(r"\??\C:\pagefile.sys", size)])),
            ..at_boot()
        }
    }

    /// Phase 1's verification reading for the MSI box, end to end through the
    /// pure assembly — and `commit_limit == phys_total + pagefile_allocated`
    /// holds (71.71 = 31.71 + 40).
    #[test]
    fn the_msi_box_assembles_to_its_measured_capability() {
        let cap = assemble(msi_inputs());
        assert!(
            cap.capability_unknown.is_empty(),
            "{:?}",
            cap.capability_unknown
        );
        assert_eq!(cap.cores, Some(16));
        assert_eq!(cap.pagefile_fixed, Some(true));
        assert_eq!(cap.pagefile_max, Some(40 * GIB));
        assert_eq!(
            cap.commit_limit,
            Some(cap.phys_total.unwrap() + cap.pagefile_allocated.unwrap())
        );
    }

    /// A zero is UNKNOWN with a reason, never a published zero.
    #[test]
    fn a_zero_capability_is_unknown_with_a_reason() {
        let cap = assemble(CapabilityInputs {
            commit_limit: Ok(0),
            phys_total: Ok(0),
            cores: Ok(0),
            ..msi_inputs()
        });
        assert_eq!(cap.commit_limit, None);
        assert_eq!(cap.phys_total, None);
        assert_eq!(cap.cores, None);
        for name in ["commitLimit", "physTotal", "cores"] {
            assert!(cap.capability_unknown.contains_key(name), "{name}");
        }
    }

    /// A live size that cannot be read blinds BOTH the allocation and the
    /// fixed flag: without it, a pending resize of the same path cannot be
    /// ruled out, and "cannot establish" is not "consistent".
    #[test]
    fn an_unreadable_live_size_is_unknown_for_allocation_and_growability() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(msi_pagefile(Err("stat failed".into()))),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, None);
        assert_eq!(cap.pagefile_fixed, None);
        assert_eq!(cap.pagefile_max, None);
        assert_eq!(
            cap.capability_unknown.keys().copied().collect::<Vec<_>>(),
            vec!["pagefileAllocated", "pagefileFixed", "pagefileMax"]
        );
    }

    /// W1 — `PagingFiles` is the NEXT-BOOT configuration. An operator who
    /// moved the pagefile to D: has not moved it yet: the allocation is still
    /// the live C: file, and growability is UNKNOWN "pending reboot" rather
    /// than a description of a machine that does not exist yet.
    #[test]
    fn a_changed_path_is_pending_reboot_and_allocation_stays_live() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![custom(r"D:\pagefile.sys", 8192, 8192)]),
                live: Ok(Some(vec![live(r"\??\C:\pagefile.sys", Ok(40 * GIB))])),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, Some(40 * GIB));
        assert_eq!(cap.pagefile_fixed, None);
        assert_eq!(cap.pagefile_max, None);
        assert!(cap.capability_unknown["pagefileFixed"].contains("pending reboot"));
    }

    /// Same path, new sizes: the live file sits outside the configured
    /// `[initial, max]` band, so the resize has not happened yet.
    #[test]
    fn a_changed_size_on_the_same_path_is_pending_reboot() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![custom(r"C:\pagefile.sys", 16384, 16384)]),
                live: Ok(Some(vec![live(r"\??\C:\pagefile.sys", Ok(40 * GIB))])),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_fixed, None);
        assert!(cap.capability_unknown["pagefileMax"].contains("pending reboot"));
    }

    /// Pagefile REMOVED for the next boot: the configuration is empty, but the
    /// live file still occupies 40 GiB — never `Some(0)`.
    #[test]
    fn an_empty_configuration_never_zeroes_a_live_allocation() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![]),
                live: Ok(Some(vec![live(r"\??\C:\pagefile.sys", Ok(40 * GIB))])),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, Some(40 * GIB));
        assert_eq!(cap.pagefile_fixed, None);
    }

    /// No pagefile configured and none live: consistent, fixed, a real zero.
    #[test]
    fn no_pagefile_configured_or_live_is_a_real_fixed_zero() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![]),
                live: Ok(Some(vec![])),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, Some(0));
        assert_eq!(cap.pagefile_max, Some(0));
        assert_eq!(cap.pagefile_fixed, Some(true));
    }

    /// An unreadable live list keeps the allocation AND growability unknown.
    #[test]
    fn an_unreadable_live_list_is_unknown_not_consistent() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![custom(r"C:\pagefile.sys", 40960, 40960)]),
                live: Err("ExistingPageFiles registry value unreadable".into()),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, None);
        assert_eq!(cap.pagefile_fixed, None);
        assert!(cap.capability_unknown["pagefileFixed"].contains("cannot be established"));
    }

    #[test]
    fn configuration_is_live_matching_rules() {
        let growable = [custom(r"C:\pagefile.sys", 4096, 16384)];
        // Grown within the band is still the live configuration.
        assert!(configuration_is_live(
            &growable,
            &[live(r"\??\c:\PAGEFILE.SYS", Ok(10 * GIB))]
        ));
        // Automatic all-drives matches any live set with the same file name.
        let automatic = parse_paging_files(&[r"?:\pagefile.sys"]).unwrap();
        assert!(configuration_is_live(
            &automatic,
            &[
                live(r"\??\C:\pagefile.sys", Ok(GIB)),
                live(r"\??\D:\pagefile.sys", Ok(GIB))
            ]
        ));
        assert!(!configuration_is_live(&automatic, &[]));
        // System-managed on a named drive needs only the path.
        let managed = parse_paging_files(&[r"C:\pagefile.sys"]).unwrap();
        assert!(configuration_is_live(
            &managed,
            &[live(r"\??\C:\pagefile.sys", Err("stat failed".into()))]
        ));
    }

    /// Round 2 #1 — a same-path change (fixed → system-managed, or a resize
    /// whose band still contains the live file) is invisible to the path/size
    /// check; the key's post-boot write time is what disqualifies it.
    #[test]
    fn a_write_after_boot_is_pending_even_on_the_same_path() {
        // 40960/40960 → 16384/65536: the live 40 GiB file sits INSIDE the new
        // band, so only the write time can tell.
        let resized = PagefileReading {
            configured: Ok(vec![custom(r"C:\pagefile.sys", 16384, 65536)]),
            live: Ok(Some(vec![live(r"\??\C:\pagefile.sys", Ok(40 * GIB))])),
            config_written_at: Ok(BOOT + 3600),
            ..at_boot()
        };
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(resized.clone()),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_fixed, None);
        assert!(cap.capability_unknown["pagefileFixed"].contains("changed since boot"));
        assert_eq!(
            cap.pagefile_allocated,
            Some(40 * GIB),
            "allocation stays live"
        );
        // The same reading, written only at boot, IS live (and growable).
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                config_written_at: Ok(BOOT + 10),
                ..resized
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_fixed, Some(false));
    }

    #[test]
    fn an_unreadable_write_or_boot_time_is_unknown_not_live() {
        for reading in [
            PagefileReading {
                config_written_at: Err("metadata unreadable".into()),
                ..msi_pagefile(Ok(40 * GIB))
            },
            PagefileReading {
                booted_at: Err("boot time unavailable".into()),
                ..msi_pagefile(Ok(40 * GIB))
            },
        ] {
            let cap = assemble(CapabilityInputs {
                pagefile: Ok(reading),
                ..msi_inputs()
            });
            assert_eq!(cap.pagefile_fixed, None);
            assert!(cap.capability_unknown["pagefileFixed"].contains("cannot be established"));
        }
    }

    #[test]
    fn the_boot_grace_window_and_filetime_conversion() {
        assert!(!written_since_boot(BOOT + BOOT_WRITE_GRACE_SECS, BOOT));
        assert!(written_since_boot(BOOT + BOOT_WRITE_GRACE_SECS + 1, BOOT));
        assert!(!written_since_boot(BOOT - 100, BOOT));
        // 1970-01-01T00:00:00Z is 116444736000000000 ticks after 1601.
        let epoch: u64 = 116_444_736_000_000_000;
        assert_eq!(
            filetime_to_unix_secs((epoch >> 32) as u32, epoch as u32),
            Some(0)
        );
        let later = epoch + 1_790_000_000 * 10_000_000;
        assert_eq!(
            filetime_to_unix_secs((later >> 32) as u32, later as u32),
            Some(1_790_000_000)
        );
        assert_eq!(filetime_to_unix_secs(0, 0), None);
    }

    /// Round 2 #2 — a genuinely pagefile-less box: `ExistingPageFiles` absent
    /// AND `PagingFiles` empty is a real, fixed zero. Absent with a non-empty
    /// configuration stays UNKNOWN.
    #[test]
    fn absent_live_list_is_zero_only_when_the_configuration_agrees() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                live: Ok(None),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, Some(0));
        assert_eq!(cap.pagefile_fixed, Some(true));
        assert_eq!(cap.pagefile_max, Some(0));

        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Ok(vec![custom(r"C:\pagefile.sys", 40960, 40960)]),
                live: Ok(None),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, None);
        assert_eq!(cap.pagefile_fixed, None);
        assert_eq!(cap.pagefile_max, None);

        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                configured: Err("PagingFiles unreadable".into()),
                live: Ok(None),
                ..at_boot()
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, None);
        assert_eq!(cap.pagefile_fixed, None);
    }

    /// Round 2 #5 — physical RAM off Windows: the reading wins, `MemTotal` is
    /// the fallback when `memory_status` withheld its reading, and a double
    /// failure names both sources.
    #[cfg(not(windows))]
    #[test]
    fn non_windows_phys_total_falls_back_to_memtotal() {
        let reading = super::super::resource_sample::MemoryStatus {
            commit_total: 8 * GIB,
            commit_available: GIB,
            phys_total: 8 * GIB,
            phys_available: GIB,
        };
        let meminfo = "MemTotal:       8388608 kB\n".to_string();
        assert_eq!(
            non_windows_phys_total(Some(reading), Ok(&meminfo)),
            Ok(8 * GIB)
        );
        assert_eq!(non_windows_phys_total(None, Ok(&meminfo)), Ok(8 * GIB));
        let no_line = non_windows_phys_total(None, Ok("MemFree: 1 kB\n")).unwrap_err();
        assert!(no_line.contains("MemAvailable = 0") && no_line.contains("MemTotal"));
        let unreadable = "/proc/meminfo unreadable: denied".to_string();
        let both = non_windows_phys_total(None, Err(&unreadable)).unwrap_err();
        assert!(both.contains("MemAvailable = 0") && both.contains("denied"));
    }

    /// W3 — a probe that never ran is null everywhere, with the reason under
    /// EVERY field name.
    #[test]
    fn unknown_everywhere_names_every_field() {
        let cap = unknown_everywhere("join failed");
        let json = serde_json::to_value(&cap).unwrap();
        for name in CAPABILITY_FIELDS {
            assert!(json[name].is_null(), "{name}");
            assert_eq!(json["capabilityUnknown"][name], "join failed", "{name}");
        }
    }

    /// The `/health` wire names — pinned so the coordinator's verification and
    /// any dashboard reading them cannot drift silently.
    #[test]
    fn the_health_block_uses_camel_case_wire_names() {
        let json = serde_json::to_value(assemble(msi_inputs())).unwrap();
        for key in [
            "commitLimit",
            "commitLimitSource",
            "commitCharged",
            "physTotal",
            "cores",
            "pagefileAllocated",
            "pagefileMax",
            "pagefileFixed",
            "capabilityUnknown",
        ] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
    }

    /// The live probe on THIS host either reads a commit limit or says why not.
    #[test]
    fn the_live_probe_never_publishes_a_silent_gap() {
        let cap = probe();
        assert!(cap.commit_limit.is_some() || cap.capability_unknown.contains_key("commitLimit"));
        assert!(
            cap.pagefile_fixed.is_some() || cap.capability_unknown.contains_key("pagefileFixed")
        );
    }
}
