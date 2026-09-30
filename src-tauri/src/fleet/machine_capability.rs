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
//! | `pagefile_*` | the `PagingFiles` registry value + the live files' sizes | UNKNOWN — Linux has no pagefile |
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
    /// Bytes currently allocated to pagefiles on disk (the sum of the live
    /// files' sizes — what `Win32_PageFileUsage.AllocatedBaseSize` reports).
    /// `Some(0)` is a real reading: a box configured with no pagefile.
    pub(crate) pagefile_allocated: Option<u64>,
    /// The configured maximum pagefile size in bytes, summed over every file.
    /// UNKNOWN for a system-managed pagefile, which has no configured maximum —
    /// Windows grows it on demand.
    pub(crate) pagefile_max: Option<u64>,
    /// Whether the commit limit is FIXED: every pagefile has
    /// `initial == maximum`, or there is no pagefile at all. On a fixed box
    /// "the paging file is too small" (`os error 1455`) is TERMINAL — Windows
    /// will never grow the file — whereas on a system-managed one it is often
    /// transient. See [`pagefile_fixed`].
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

/// What the pagefile probe found: the configuration, and the bytes the live
/// files currently occupy (which can fail independently of the configuration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PagefileReading {
    pub(crate) entries: Vec<PagefileEntry>,
    pub(crate) allocated: Result<u64, String>,
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
            let allocated = match reading.allocated {
                Ok(v) => Some(v),
                Err(reason) => {
                    unknown.insert("pagefileAllocated", reason);
                    None
                }
            };
            let max = match pagefile_max(&reading.entries) {
                Ok(v) => Some(v),
                Err(reason) => {
                    unknown.insert("pagefileMax", reason);
                    None
                }
            };
            (allocated, max, Some(pagefile_fixed(&reading.entries)))
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
        commit_limit_source: "GlobalMemoryStatusEx.ullTotalPageFile",
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
    let (commit_limit, commit_charged) = match std::fs::read_to_string("/proc/meminfo") {
        Ok(text) => parse_commit_meminfo(&text),
        Err(e) => {
            let reason = format!("/proc/meminfo unreadable: {e}");
            (Err(reason.clone()), Err(reason))
        }
    };
    CapabilityInputs {
        commit_limit,
        commit_limit_source: "/proc/meminfo CommitLimit",
        commit_charged,
        phys_total: reading
            .map(|m| m.phys_total)
            .ok_or_else(|| "sysinfo reported no memory".to_string()),
        cores: cores(),
        pagefile: Err(
            "no pagefile on this OS — Linux backs commit with swap (the fleet sample's \
             swap_total_bytes)"
                .to_string(),
        ),
    }
}

/// The Windows pagefile probe: the registry, not WMI.
///
/// ## Why the registry and not `Win32_PageFileSetting` / `Win32_PageFileUsage`
///
/// Both WMI classes are views over the same two `Memory Management` registry
/// values read here — `PagingFiles` (the configuration) and `ExistingPageFiles`
/// (the live files) — so the registry is the source, not an approximation of
/// it. Reading it directly costs microseconds, needs no COM apartment on the
/// calling thread and forks nothing. WMI costs hundreds of milliseconds, needs
/// COM initialised, and — measured in this very plan's incident — is one of the
/// things that FAILS under commit exhaustion: a .NET assembly load for the WMI
/// transcript scan reported a phantom "missing `System.Management.Automation`"
/// seconds before abort #4. A capability probe that breaks on the condition it
/// describes is the wrong instrument.
///
/// ## Cached once, except for the live size
///
/// The configuration only changes across a reboot, so it is read ONCE per
/// process (first capability read, which is always on a blocking-pool thread —
/// never on the async runtime or the spawn path) and cached, including a
/// failure. The allocated size is re-`stat`ed on every read: a system-managed
/// pagefile GROWS under load, and a cached size would publish the one number
/// most likely to have changed at the moment it matters.
#[cfg(windows)]
mod windows_pagefile {
    use std::sync::OnceLock;

    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    use super::{parse_paging_files, PagefileEntry, PagefileReading};

    const MEMORY_MANAGEMENT: &str =
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management";

    fn memory_management() -> Result<RegKey, String> {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(MEMORY_MANAGEMENT)
            .map_err(|e| format!("HKLM\\{MEMORY_MANAGEMENT} unreadable: {e}"))
    }

    fn configuration() -> &'static Result<Vec<PagefileEntry>, String> {
        static CONFIG: OnceLock<Result<Vec<PagefileEntry>, String>> = OnceLock::new();
        CONFIG.get_or_init(|| {
            let key = memory_management()?;
            let lines: Vec<String> = key
                .get_value("PagingFiles")
                .map_err(|e| format!("PagingFiles registry value unreadable: {e}"))?;
            parse_paging_files(&lines)
        })
    }

    /// The bytes the live pagefiles occupy right now. `ExistingPageFiles`
    /// lists them in NT form (`\??\C:\pagefile.sys`); `std::fs::metadata`
    /// reads an in-use `pagefile.sys` through its `FindFirstFileW` fallback.
    fn allocated(entries: &[PagefileEntry]) -> Result<u64, String> {
        if entries.is_empty() {
            return Ok(0);
        }
        let key = memory_management()?;
        let live: Vec<String> = key
            .get_value("ExistingPageFiles")
            .map_err(|e| format!("ExistingPageFiles registry value unreadable: {e}"))?;
        let mut total: u64 = 0;
        for raw in live.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let path = raw.strip_prefix(r"\??\").unwrap_or(raw);
            let len = std::fs::metadata(path)
                .map_err(|e| format!("pagefile {path} could not be stat'ed: {e}"))?
                .len();
            total = total.saturating_add(len);
        }
        Ok(total)
    }

    pub(super) fn reading() -> Result<PagefileReading, String> {
        let entries = configuration().clone()?;
        let allocated = allocated(&entries);
        Ok(PagefileReading { entries, allocated })
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
            pagefile: Ok(PagefileReading {
                entries: vec![custom(r"C:\pagefile.sys", 40960, 40960)],
                allocated: Ok(40 * GIB),
            }),
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

    /// An allocation read can fail on its own while the configuration still
    /// answers `pagefileFixed` — the two are independent.
    #[test]
    fn an_unreadable_allocation_does_not_blind_the_fixed_flag() {
        let cap = assemble(CapabilityInputs {
            pagefile: Ok(PagefileReading {
                entries: vec![custom(r"C:\pagefile.sys", 40960, 40960)],
                allocated: Err("stat failed".into()),
            }),
            ..msi_inputs()
        });
        assert_eq!(cap.pagefile_allocated, None);
        assert_eq!(cap.pagefile_fixed, Some(true));
        assert_eq!(
            cap.capability_unknown.keys().copied().collect::<Vec<_>>(),
            vec!["pagefileAllocated"]
        );
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
