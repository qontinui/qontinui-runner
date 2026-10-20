//! Linux collector: `/proc/meminfo`, `/proc/sys/fs/file-nr`, `/proc/stat`
//! `btime`, and the count of `/proc/[0-9]*`.
//!
//! The mapping (plan D2, vet R6) and the not-applicable axes are documented
//! on [`super::parse::linux_gauges`], which this file feeds. A Linux reading is
//! REPORTING-ONLY in v1: reclaimable slab is cache, so coord's S8 grades only
//! `windows:` sources.

use super::parse::{self, instrument, GaugeReading};
use super::{FailureLog, GaugeSource};

/// The Linux [`GaugeSource`]. Pool tags keep the trait's default (`None`,
/// NOT APPLICABLE): Linux has no pool-tag table.
#[derive(Debug, Default)]
pub(super) struct LinuxCollector;

impl GaugeSource for LinuxCollector {
    fn gauges(&mut self, log: &mut FailureLog) -> GaugeReading {
        let meminfo = log.record(instrument::PROC_MEMINFO, read("/proc/meminfo"));
        let file_nr = log.record(instrument::FILE_NR, read("/proc/sys/fs/file-nr"));
        let stat = log.record(instrument::PROC_STAT, read("/proc/stat"));
        let pids = log.record(instrument::PROC_PIDS, count_pids());
        parse::linux_gauges(
            meminfo.as_deref(),
            file_nr.as_deref(),
            stat.as_deref(),
            pids,
        )
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

/// Live processes: the numeric directories under `/proc`. An entry that
/// vanishes mid-walk (a process exiting) is skipped, not an error.
fn count_pids() -> Result<u64, String> {
    let entries = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?;
    let n = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(parse::is_pid_dir_name))
        .count();
    Ok(u64::try_from(n).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real reads on the machine running the test.
    #[test]
    fn the_live_linux_reads_answer() {
        let mut log = FailureLog::default();
        let g = LinuxCollector.gauges(&mut log);
        assert_eq!(
            g.instruments,
            vec!["proc-meminfo", "file-nr", "proc-stat", "proc-pids"]
        );
        assert!(g.kernel_paged_bytes.is_some_and(|b| b >= 0), "{g:?}");
        assert!(g.kernel_nonpaged_bytes.is_some_and(|b| b > 0), "{g:?}");
        assert!(g.kernel_handle_count.is_some_and(|h| h > 0), "{g:?}");
        assert!(g.os_process_count.is_some_and(|p| p > 0), "{g:?}");
        assert!(g.os_booted_at.is_some(), "{g:?}");
        assert_eq!(g.kernel_token_objects, None);
        assert_eq!(g.kernel_process_objects, None);
        assert_eq!(LinuxCollector.pool_tags(&mut log), None);
    }
}
