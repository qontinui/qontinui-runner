//! Kernel pressure axes for the host lane (plan
//! `2026-09-30-the-fleet-machine-is-not-a-first-class-coord-entity-and-coord-has-no-resource-model`
//! §3.4, Phase 2.2): load average (1/5/15 min), PSI `memory|cpu|io` ×
//! `some|full` × `avg10|avg60`, the monotonic `oom_kill` counter and the
//! kernel `boot_id`.
//!
//! ## Absence is UNKNOWN, and the manifest says WHICH absence
//!
//! Every value is `Option`, and each sample also carries a `measured`
//! manifest mapping an axis name to one of three words:
//!
//! * `measured` — the probe ran and produced the value(s) beside it;
//! * `not_supported` — this platform has no such instrument (PSI and load on
//!   Windows). A proxy (Windows processor-queue length for load) is NOT
//!   equivalent and is deliberately not published;
//! * `unavailable` — the platform has the instrument but this read failed (a
//!   kernel built without `CONFIG_PSI`, booted with `psi=0`, an unreadable
//!   procfs file).
//!
//! The two failure words are distinct facts: "this Windows box cannot report
//! PSI" is permanent and needs no action; "this Linux box stopped reporting
//! PSI" is a regression someone should look at. A bare NULL would collapse
//! them, and a `0` would read as "no pressure" — served policy
//! `verification-and-evidence` `unknown-must-not-render-as-a-default`.
//!
//! Every function here is either a pure parser (unit-tested against recorded
//! procfs text) or a thin file read around one.

use std::collections::BTreeMap;

/// Axis names in the `measured` manifest — the contract's exact spellings.
pub(crate) const AXIS_LOAD_1M: &str = "load_1m";
pub(crate) const AXIS_LOAD_5M: &str = "load_5m";
pub(crate) const AXIS_LOAD_15M: &str = "load_15m";
pub(crate) const AXIS_PSI_MEMORY: &str = "psi_memory";
pub(crate) const AXIS_PSI_CPU: &str = "psi_cpu";
pub(crate) const AXIS_PSI_IO: &str = "psi_io";
pub(crate) const AXIS_OOM_KILL_TOTAL: &str = "oom_kill_total";
pub(crate) const AXIS_BOOT_ID: &str = "boot_id";

/// What the manifest says about one axis. The variant names are the wire
/// words, so `Measured::Measured` is deliberate.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Measured {
    Measured,
    NotSupported,
    Unavailable,
}

impl Measured {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Measured::Measured => "measured",
            Measured::NotSupported => "not_supported",
            Measured::Unavailable => "unavailable",
        }
    }

    /// `measured` when the read produced something, else `unavailable`.
    pub(crate) fn from_read<T>(value: &Option<T>) -> Self {
        if value.is_some() {
            Measured::Measured
        } else {
            Measured::Unavailable
        }
    }
}

/// `/proc/loadavg`'s three averages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LoadAvg {
    pub(crate) one: f64,
    pub(crate) five: f64,
    pub(crate) fifteen: f64,
}

/// Parse `/proc/loadavg` (`"32.07 28.81 31.69 31/16803 2177659"`).
///
/// Recognised by SHAPE, not position: the first line whose first three
/// whitespace tokens are all finite non-negative floats and whose fourth is a
/// `running/total` pair. That lets the `wsl` lane find it inside a `cat` of
/// several procfs files without a marker. All three or nothing — a partial
/// triple is not a reading.
pub(crate) fn parse_loadavg(text: &str) -> Option<LoadAvg> {
    text.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let one = parse_nonneg(it.next()?)?;
        let five = parse_nonneg(it.next()?)?;
        let fifteen = parse_nonneg(it.next()?)?;
        let tasks = it.next()?;
        let (running, total) = tasks.split_once('/')?;
        running.parse::<u64>().ok()?;
        total.parse::<u64>().ok()?;
        Some(LoadAvg { one, five, fifteen })
    })
}

fn parse_nonneg(tok: &str) -> Option<f64> {
    tok.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// One PSI file's two lines, each as `(avg10, avg60)` percent.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Psi {
    pub(crate) some: Option<(f64, f64)>,
    /// `None` when the line is absent — `/proc/pressure/cpu` has no `full`
    /// line on kernels before 5.13, which is UNKNOWN, not "zero stall".
    pub(crate) full: Option<(f64, f64)>,
}

/// Parse one `/proc/pressure/<resource>` file:
///
/// ```text
/// some avg10=2.12 avg60=7.81 avg300=2.78 total=28433288250
/// full avg10=1.95 avg60=7.18 avg300=2.56 total=25269186748
/// ```
///
/// `None` when neither line parses (the file is not a PSI file at all).
pub(crate) fn parse_psi(text: &str) -> Option<Psi> {
    let mut psi = Psi::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let Some(kind) = it.next() else { continue };
        let mut avg10 = None;
        let mut avg60 = None;
        for kv in it {
            match kv.split_once('=') {
                Some(("avg10", v)) => avg10 = parse_nonneg(v),
                Some(("avg60", v)) => avg60 = parse_nonneg(v),
                _ => {}
            }
        }
        let pair = match (avg10, avg60) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        };
        match kind {
            "some" => psi.some = pair,
            "full" => psi.full = pair,
            _ => {}
        }
    }
    (psi.some.is_some() || psi.full.is_some()).then_some(psi)
}

/// The `oom_kill` counter from `/proc/vmstat` (monotonic since boot; present
/// on kernels >= 4.13). `None` when the line is absent.
pub(crate) fn parse_vmstat_oom_kill(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next()? == "oom_kill")
            .then(|| it.next()?.parse::<u64>().ok())
            .flatten()
    })
}

/// The `oom_kill` counter from a cgroup v2 `memory.events` file — same line
/// shape as `/proc/vmstat`, scoped to one cgroup (hierarchical).
pub(crate) fn parse_memory_events_oom_kill(text: &str) -> Option<u64> {
    parse_vmstat_oom_kill(text)
}

/// A `boot_id` as the kernel writes it: a trimmed, non-empty UUID-shaped
/// token. Anything else is `None` rather than an id that would never change.
pub(crate) fn parse_boot_id(text: &str) -> Option<String> {
    let t = text.trim();
    let ok = t.len() == 36 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    ok.then(|| t.to_ascii_lowercase())
}

/// Everything this module adds to a host-lane sample, plus its manifest.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct HostAxes {
    pub(crate) load: Option<LoadAvg>,
    pub(crate) psi_memory: Option<Psi>,
    pub(crate) psi_cpu: Option<Psi>,
    pub(crate) psi_io: Option<Psi>,
    pub(crate) oom_kill_total: Option<u64>,
    pub(crate) boot_id: Option<String>,
    pub(crate) measured: BTreeMap<String, String>,
}

impl HostAxes {
    fn mark(&mut self, axis: &str, m: Measured) {
        self.measured
            .insert(axis.to_string(), m.as_str().to_string());
    }
}

/// Read a procfs-style file; `None` on any error.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Assemble [`HostAxes`] from already-read file contents. PURE — the Linux
/// collector passes real reads, tests pass fixtures, and a `None` input is an
/// unreadable file (→ `unavailable`).
pub(crate) fn linux_axes_from(
    loadavg: Option<&str>,
    psi_memory: Option<&str>,
    psi_cpu: Option<&str>,
    psi_io: Option<&str>,
    vmstat: Option<&str>,
    boot_id: Option<&str>,
) -> HostAxes {
    let mut a = HostAxes {
        load: loadavg.and_then(parse_loadavg),
        psi_memory: psi_memory.and_then(parse_psi),
        psi_cpu: psi_cpu.and_then(parse_psi),
        psi_io: psi_io.and_then(parse_psi),
        oom_kill_total: vmstat.and_then(parse_vmstat_oom_kill),
        boot_id: boot_id.and_then(parse_boot_id),
        measured: BTreeMap::new(),
    };
    let load = Measured::from_read(&a.load);
    for axis in [AXIS_LOAD_1M, AXIS_LOAD_5M, AXIS_LOAD_15M] {
        a.mark(axis, load);
    }
    a.mark(AXIS_PSI_MEMORY, Measured::from_read(&a.psi_memory));
    a.mark(AXIS_PSI_CPU, Measured::from_read(&a.psi_cpu));
    a.mark(AXIS_PSI_IO, Measured::from_read(&a.psi_io));
    a.mark(AXIS_OOM_KILL_TOTAL, Measured::from_read(&a.oom_kill_total));
    a.mark(AXIS_BOOT_ID, Measured::from_read(&a.boot_id));
    a
}

/// The Windows reading: every axis this module owns is `not_supported`.
/// Windows has no load average (processor-queue length is a different
/// quantity), no PSI, no `oom_kill` counter and no kernel boot UUID.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_axes() -> HostAxes {
    let mut a = HostAxes::default();
    for axis in [
        AXIS_LOAD_1M,
        AXIS_LOAD_5M,
        AXIS_LOAD_15M,
        AXIS_PSI_MEMORY,
        AXIS_PSI_CPU,
        AXIS_PSI_IO,
        AXIS_OOM_KILL_TOTAL,
        AXIS_BOOT_ID,
    ] {
        a.mark(axis, Measured::NotSupported);
    }
    a
}

/// Collect this host's axes. Cheap: six small procfs reads on Linux, one
/// sysinfo call on macOS, nothing on Windows.
pub(crate) fn collect() -> HostAxes {
    #[cfg(target_os = "linux")]
    {
        let loadavg = read_text("/proc/loadavg");
        let mem = read_text("/proc/pressure/memory");
        let cpu = read_text("/proc/pressure/cpu");
        let io = read_text("/proc/pressure/io");
        let vmstat = read_text("/proc/vmstat");
        let boot = read_text("/proc/sys/kernel/random/boot_id");
        linux_axes_from(
            loadavg.as_deref(),
            mem.as_deref(),
            cpu.as_deref(),
            io.as_deref(),
            vmstat.as_deref(),
            boot.as_deref(),
        )
    }
    #[cfg(windows)]
    {
        windows_axes()
    }
    #[cfg(all(not(target_os = "linux"), not(windows)))]
    {
        // macOS / BSD: sysinfo reads getloadavg(3); the kernel has no PSI, no
        // oom_kill counter, and no procfs boot_id.
        let mut a = HostAxes::default();
        let l = sysinfo::System::load_average();
        a.load = Some(LoadAvg {
            one: l.one,
            five: l.five,
            fifteen: l.fifteen,
        });
        for axis in [AXIS_LOAD_1M, AXIS_LOAD_5M, AXIS_LOAD_15M] {
            a.mark(axis, Measured::Measured);
        }
        for axis in [
            AXIS_PSI_MEMORY,
            AXIS_PSI_CPU,
            AXIS_PSI_IO,
            AXIS_OOM_KILL_TOTAL,
            AXIS_BOOT_ID,
        ] {
            a.mark(axis, Measured::NotSupported);
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded on a fleet host (synthetic values in the recorded shape).
    const PSI_MEMORY: &str = "some avg10=2.12 avg60=7.81 avg300=2.78 total=28433288250\n\
                              full avg10=1.95 avg60=7.18 avg300=2.56 total=25269186748\n";
    const PSI_CPU: &str = "some avg10=0.96 avg60=0.96 avg300=1.36 total=40931471255\n\
                           full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
    /// A pre-5.13 kernel: `/proc/pressure/cpu` has no `full` line.
    const PSI_CPU_OLD_KERNEL: &str = "some avg10=12.50 avg60=9.25 avg300=4.00 total=123\n";
    const PSI_IO: &str = "some avg10=2.97 avg60=9.17 avg300=3.48 total=32272603610\n\
                          full avg10=2.29 avg60=7.60 avg300=2.84 total=15696812751\n";
    const LOADAVG: &str = "32.07 28.81 31.69 31/16803 2177659\n";
    const VMSTAT: &str =
        "nr_free_pages 1234\nnr_zone_inactive_anon 5\npgfault 99\noom_kill 6\nnuma_hit 1\n";
    const BOOT_ID: &str = "0f0e0d0c-0b0a-4908-8706-050403020100\n";

    #[test]
    fn psi_parses_both_lines_to_avg10_and_avg60() {
        let p = parse_psi(PSI_MEMORY).unwrap();
        assert_eq!(p.some, Some((2.12, 7.81)));
        assert_eq!(p.full, Some((1.95, 7.18)));
    }

    #[test]
    fn a_cpu_file_without_a_full_line_is_unknown_full_not_zero() {
        let p = parse_psi(PSI_CPU_OLD_KERNEL).unwrap();
        assert_eq!(p.some, Some((12.5, 9.25)));
        assert_eq!(p.full, None);
        // And a real zero line stays a real zero.
        assert_eq!(parse_psi(PSI_CPU).unwrap().full, Some((0.0, 0.0)));
    }

    #[test]
    fn non_psi_text_is_none() {
        assert_eq!(parse_psi(""), None);
        assert_eq!(parse_psi("garbage here\n"), None);
    }

    #[test]
    fn loadavg_parses_all_three_or_nothing() {
        assert_eq!(
            parse_loadavg(LOADAVG),
            Some(LoadAvg {
                one: 32.07,
                five: 28.81,
                fifteen: 31.69
            })
        );
        assert_eq!(parse_loadavg("32.07 28.81\n"), None);
        // Found by shape inside a multi-file cat (the wsl lane's probe).
        let mixed = format!("MemTotal: 100 kB\n192146\n{LOADAVG}");
        assert_eq!(parse_loadavg(&mixed).unwrap().fifteen, 31.69);
    }

    #[test]
    fn vmstat_oom_kill_is_read_by_key_and_absent_is_none() {
        assert_eq!(parse_vmstat_oom_kill(VMSTAT), Some(6));
        assert_eq!(parse_vmstat_oom_kill("oom_kill_foo 3\npgfault 1\n"), None);
    }

    #[test]
    fn boot_id_is_validated() {
        assert_eq!(
            parse_boot_id(BOOT_ID).as_deref(),
            Some("0f0e0d0c-0b0a-4908-8706-050403020100")
        );
        assert_eq!(parse_boot_id(""), None);
        assert_eq!(parse_boot_id("not-a-boot-id"), None);
    }

    #[test]
    fn the_linux_manifest_names_every_axis_and_distinguishes_unavailable() {
        let a = linux_axes_from(
            Some(LOADAVG),
            Some(PSI_MEMORY),
            None, // unreadable cpu pressure file
            Some(PSI_IO),
            Some(VMSTAT),
            Some(BOOT_ID),
        );
        let m: Vec<(&str, &str)> = a
            .measured
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            m,
            vec![
                ("boot_id", "measured"),
                ("load_15m", "measured"),
                ("load_1m", "measured"),
                ("load_5m", "measured"),
                ("oom_kill_total", "measured"),
                ("psi_cpu", "unavailable"),
                ("psi_io", "measured"),
                ("psi_memory", "measured"),
            ]
        );
        assert_eq!(a.psi_cpu, None);
        assert_eq!(a.oom_kill_total, Some(6));
    }

    #[test]
    fn windows_reports_not_supported_never_zero() {
        let a = windows_axes();
        assert_eq!(a.measured.len(), 8);
        assert!(a.measured.values().all(|v| v == "not_supported"));
        assert_eq!(a.load, None);
        assert_eq!(a.psi_memory, None);
        assert_eq!(a.oom_kill_total, None);
    }
}
