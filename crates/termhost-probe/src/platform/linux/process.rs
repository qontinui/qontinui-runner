//! Per-process accounting on Linux: `/proc/<pid>/stat` utime + stime, and
//! `/proc/<pid>/smaps_rollup` PSS (the catalog's memory number) with
//! `/proc/<pid>/status` VmRSS beside it. Per-process GPU engine counters exist
//! only on Windows (PDH).

use crate::platform::common::{parse_kb_line, parse_proc_stat_ticks};
use crate::platform::{GpuReading, ProcMem, ProcessReader};

pub struct ProcReader {
    tick_ns: u64,
}

impl ProcReader {
    pub fn new() -> ProcReader {
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        ProcReader {
            tick_ns: if hz > 0 {
                1_000_000_000 / hz as u64
            } else {
                10_000_000
            },
        }
    }
}

impl ProcessReader for ProcReader {
    fn cpu_ns(&mut self, pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_proc_stat_ticks(&stat).map(|t| t * self.tick_ns)
    }

    fn memory(&mut self, pid: u32) -> Option<ProcMem> {
        let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok();
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok();
        let pss = rollup.as_deref().and_then(|r| parse_kb_line(r, "Pss"));
        let rss = status.as_deref().and_then(|s| parse_kb_line(s, "VmRSS"));
        match (pss, rss) {
            (Some(p), r) => Some(ProcMem {
                catalog_bytes: p,
                rss_bytes: r.unwrap_or(p),
            }),
            // Kernels before 4.14 have no smaps_rollup: no PSS, so no catalog
            // number — reported as unreadable rather than substituted.
            (None, _) => None,
        }
    }

    fn gpu(&mut self) -> GpuReading {
        GpuReading::Unsupported("per-process GPU engine counters are Windows-only (PDH)".into())
    }
}
