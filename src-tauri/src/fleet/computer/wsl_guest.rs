//! WSL guests as separate computers (plan Phase 2.3, the WSL arm).
//!
//! A WSL distro has its own `/etc/machine-id`, its own systemd and its own
//! `actions.runner.*` units, while its RAM is carved from the Windows host —
//! so it is reported as its own computer (`kind = wsl_guest`) whose
//! `parent_identity_hash` is the host's.
//!
//! ## Never wake a stopped distro
//!
//! `wsl.exe -d <distro> -e …` STARTS a stopped distro, and its exit re-arms
//! WSL's poweroff timer — a probe built from it manufactures the liveness it
//! reports (34 distro boot cycles in 2h20m on one fleet host; the supervisor's
//! `wsl_util.rs` gate records it). So a guest is probed ONLY when
//! `wsl.exe --list --running --quiet` — which reads state without starting
//! anything — lists it, in the same tick.
//!
//! ## One fork per running guest
//!
//! Everything a guest report needs comes from one `wsl.exe --exec sh -c` run of
//! [`GUEST_SCRIPT`], tab-tagged so a missing file costs its own line and
//! nothing else, and bounded by `resource_sample::wsl_probe` (timeout + kill-on-close job).

use std::collections::BTreeMap;

use super::identity::identity_hash_of;
use super::services::{parse_systemctl_show, UnitProps};

/// Parse `wsl --list --running --quiet`.
///
/// Tolerates NUL-interleaved UTF-16 read lossily, a BOM, CRLF, blank lines,
/// and the localized "no running distributions" notice — a distro name is a
/// single whitespace-free token, so any line with a space is a notice. Same
/// rule as the supervisor's `wsl_util::parse_running_distros`.
pub(crate) fn parse_running_distros(raw: &str) -> Vec<String> {
    raw.replace(['\0', '\u{feff}'], "")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.chars().any(char::is_whitespace))
        .map(str::to_string)
        .collect()
}

/// Docker Desktop's utility distros are Docker's VM plumbing, not fleet
/// computers: no systemd, no watched units, and an identity that is not the
/// operator's to track.
pub(crate) fn is_reportable_distro(name: &str) -> bool {
    !name.to_ascii_lowercase().starts_with("docker-desktop")
}

/// The single in-guest probe. One line on purpose (it crosses a Windows
/// command line). `TZ=UTC` pins `StateChangeTimestamp` to a zone the parser
/// trusts; `.runner` is read without `sudo` (a `0700` home simply yields no
/// line and the unit-name fallback applies).
pub(crate) const GUEST_SCRIPT: &str = r#"printf 'MACHINE_ID\t%s\n' "$(cat /etc/machine-id 2>/dev/null)"; printf 'BOOT_ID\t%s\n' "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"; printf 'HOSTNAME\t%s\n' "$(cat /proc/sys/kernel/hostname 2>/dev/null)"; printf 'KERNEL\t%s\n' "$(uname -r 2>/dev/null)"; printf 'ARCH\t%s\n' "$(uname -m 2>/dev/null)"; printf 'OS\t%s\n' "$(. /etc/os-release 2>/dev/null; printf '%s' "$NAME")"; printf 'OS_VERSION\t%s\n' "$(. /etc/os-release 2>/dev/null; printf '%s' "$VERSION_ID")"; printf 'NPROC\t%s\n' "$(nproc 2>/dev/null)"; awk '/^MemTotal:/{printf "MEM_TOTAL_KB\t%s\n",$2} /^SwapTotal:/{printf "SWAP_TOTAL_KB\t%s\n",$2}' /proc/meminfo 2>/dev/null; printf 'DISK_TOTAL\t%s\n' "$(df -B1 / 2>/dev/null | awk 'NR==2{print $2}')"; awk '/^btime /{printf "BTIME\t%s\n",$2}' /proc/stat 2>/dev/null; awk '/^oom_kill /{printf "OOM_KILL\t%s\n",$2}' /proc/vmstat 2>/dev/null; printf 'PID1\t%s\n' "$(cat /proc/1/comm 2>/dev/null)"; units=$(systemctl list-units --type=service --plain --no-legend --all 'actions.runner.*' 2>/dev/null | awk '{print $1}'); printf 'SHOW_BEGIN\n'; if [ -n "$units" ]; then TZ=UTC systemctl show $units -p Id,ActiveState,SubState,Result,Restart,OOMPolicy,MemoryMax,MemoryPeak,NRestarts,StateChangeTimestamp,ExecMainStatus,WorkingDirectory,ExecStart,ControlGroup,LoadState 2>/dev/null; fi; printf '\nSHOW_END\n'; for u in $units; do wd=$(systemctl show -p WorkingDirectory --value "$u" 2>/dev/null); if [ -n "$wd" ] && [ -r "$wd/.runner" ]; then printf 'RUNNERFILE\t%s\t%s\n' "$u" "$(tr -d '\n\r\t' < "$wd/.runner")"; fi; done; printf 'PROBE_END\n'"#;

/// Everything one guest probe produced. Holds the identity HASH, never the
/// raw machine id — [`parse_guest_probe`] hashes it and drops it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct GuestProbe {
    pub(crate) identity_hash: String,
    pub(crate) boot_id: Option<String>,
    pub(crate) hostname: Option<String>,
    pub(crate) kernel: Option<String>,
    pub(crate) arch: Option<String>,
    pub(crate) os: Option<String>,
    pub(crate) os_version: Option<String>,
    pub(crate) cpu_cores: Option<i32>,
    pub(crate) memory_total_bytes: Option<u64>,
    pub(crate) swap_total_bytes: Option<u64>,
    pub(crate) disk_total_bytes: Option<u64>,
    pub(crate) booted_at: Option<String>,
    pub(crate) oom_kill_total: Option<u64>,
    /// PID 1 is systemd, so the unit list is authoritative (an empty list
    /// means "no runner units", not "could not ask").
    pub(crate) systemd: bool,
    pub(crate) units: Vec<UnitProps>,
    pub(crate) runner_files: BTreeMap<String, String>,
}

fn nz(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Parse [`GUEST_SCRIPT`] output. `None` when the terminating `PROBE_END` is
/// missing (a truncated probe is not a reading) or when the guest has no
/// machine id (it cannot be identified, so it cannot be reported).
pub(crate) fn parse_guest_probe(text: &str) -> Option<GuestProbe> {
    if !text.lines().any(|l| l.trim() == "PROBE_END") {
        return None;
    }
    let mut g = GuestProbe::default();
    let mut machine_id: Option<String> = None;
    let mut show = String::new();
    let mut in_show = false;
    for line in text.lines() {
        if in_show {
            if line.trim() == "SHOW_END" {
                in_show = false;
            } else {
                show.push_str(line);
                show.push('\n');
            }
            continue;
        }
        if line.trim() == "SHOW_BEGIN" {
            in_show = true;
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let tag = parts.next().unwrap_or("");
        let v = parts.next().unwrap_or("");
        match tag {
            "MACHINE_ID" => machine_id = nz(v),
            "BOOT_ID" => g.boot_id = crate::fleet::host_axes::parse_boot_id(v),
            "HOSTNAME" => g.hostname = nz(v),
            "KERNEL" => g.kernel = nz(v),
            "ARCH" => g.arch = nz(v),
            "OS" => g.os = nz(v),
            "OS_VERSION" => g.os_version = nz(v),
            "NPROC" => g.cpu_cores = v.trim().parse().ok().filter(|n: &i32| *n > 0),
            "MEM_TOTAL_KB" => {
                g.memory_total_bytes = v.trim().parse::<u64>().ok().map(|k| k.saturating_mul(1024))
            }
            "SWAP_TOTAL_KB" => {
                g.swap_total_bytes = v.trim().parse::<u64>().ok().map(|k| k.saturating_mul(1024))
            }
            "DISK_TOTAL" => g.disk_total_bytes = v.trim().parse().ok().filter(|n: &u64| *n > 0),
            "BTIME" => {
                g.booted_at = v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(super::identity::epoch_secs_to_rfc3339)
            }
            "OOM_KILL" => g.oom_kill_total = v.trim().parse().ok(),
            "PID1" => g.systemd = v.trim() == "systemd",
            "RUNNERFILE" => {
                if let Some(json) = parts.next() {
                    g.runner_files.insert(v.to_string(), json.to_string());
                }
            }
            _ => {}
        }
    }
    g.identity_hash = identity_hash_of(machine_id.as_deref()?)?;
    g.units = parse_systemctl_show(&show);
    Some(g)
}

#[cfg(windows)]
pub(crate) mod probe {
    use super::{
        is_reportable_distro, parse_guest_probe, parse_running_distros, GuestProbe, GUEST_SCRIPT,
    };
    use crate::fleet::resource_sample::{decode_utf16le, wsl_probe};

    /// Probe every RUNNING, reportable distro. A distro that is not listed as
    /// running this tick is not touched at all.
    pub(crate) async fn collect() -> Vec<GuestProbe> {
        let Some(list) = wsl_probe(&["--list", "--running", "--quiet"]).await else {
            return Vec::new();
        };
        if !list.status.success() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for distro in parse_running_distros(&decode_utf16le(&list.stdout))
            .into_iter()
            .filter(|d| is_reportable_distro(d))
        {
            let Some(o) =
                wsl_probe(&["-d", distro.as_str(), "--exec", "sh", "-c", GUEST_SCRIPT]).await
            else {
                continue;
            };
            // procfs / systemctl output is UTF-8 even though `--list` is UTF-16.
            if let Some(g) = parse_guest_probe(&String::from_utf8_lossy(&o.stdout)) {
                out.push(g);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wsl --list --running --quiet` as a lossy UTF-8 read of its UTF-16LE
    /// bytes: a BOM, a NUL after every character, CRLF.
    fn utf16ish(lines: &[&str]) -> String {
        let mut s = String::from("\u{feff}");
        for l in lines {
            for c in l.chars() {
                s.push(c);
                s.push('\0');
            }
            s.push_str("\r\0\n\0");
        }
        s
    }

    #[test]
    fn running_distros_survive_the_utf16_quirk() {
        assert_eq!(
            parse_running_distros(&utf16ish(&["Ubuntu-24.04", "docker-desktop"])),
            vec!["Ubuntu-24.04".to_string(), "docker-desktop".to_string()]
        );
        assert!(
            parse_running_distros(&utf16ish(&["There are no running distributions."])).is_empty()
        );
        assert!(
            parse_running_distros(&utf16ish(&["Es werden keine Distributionen ausgeführt."]))
                .is_empty()
        );
    }

    #[test]
    fn docker_desktop_is_not_a_fleet_computer() {
        assert!(!is_reportable_distro("docker-desktop"));
        assert!(!is_reportable_distro("docker-desktop-data"));
        assert!(is_reportable_distro("Ubuntu-24.04"));
    }

    pub(crate) const GUEST_OUTPUT: &str = "MACHINE_ID\t0123456789abcdef0123456789abcdef\n\
BOOT_ID\t9a1b2c3d-0000-4111-8222-333344445555\n\
HOSTNAME\twslbox\n\
KERNEL\t5.15.153.1-microsoft-standard-WSL2\n\
ARCH\tx86_64\n\
OS\tUbuntu\n\
OS_VERSION\t24.04\n\
NPROC\t16\n\
MEM_TOTAL_KB\t32768000\n\
SWAP_TOTAL_KB\t8388608\n\
DISK_TOTAL\t1081101176832\n\
BTIME\t1790000000\n\
OOM_KILL\t3\n\
PID1\tsystemd\n\
SHOW_BEGIN\n\
Id=actions.runner.example-org-example-repo.wslbox.service\n\
ActiveState=failed\n\
SubState=failed\n\
Result=oom-kill\n\
Restart=no\n\
OOMPolicy=stop\n\
MemoryMax=infinity\n\
NRestarts=0\n\
StateChangeTimestamp=Wed 2026-09-30 01:48:16 UTC\n\
ExecMainStatus=0\n\
\n\
SHOW_END\n\
RUNNERFILE\tactions.runner.example-org-example-repo.wslbox.service\t{  \"agentName\": \"wslbox\",  \"gitHubUrl\": \"https://github.com/example-org/example-repo\"}\n\
PROBE_END\n";

    #[test]
    fn a_guest_probe_parses_and_hashes_its_machine_id() {
        let g = parse_guest_probe(GUEST_OUTPUT).unwrap();
        assert_eq!(
            g.identity_hash,
            "14c5ba9f46f5b062dfae3190c6c7e1d25a6ba40a5043c5c78cb882c3215dc16f"
        );
        assert_eq!(g.hostname.as_deref(), Some("wslbox"));
        assert_eq!(g.cpu_cores, Some(16));
        assert_eq!(g.memory_total_bytes, Some(32_768_000 * 1024));
        assert_eq!(g.oom_kill_total, Some(3));
        assert!(g.systemd);
        assert_eq!(g.units.len(), 1);
        assert_eq!(g.units[0].result.as_deref(), Some("oom-kill"));
        assert_eq!(g.booted_at.as_deref(), Some("2026-09-21T14:13:20Z"));
        assert!(g
            .runner_files
            .contains_key("actions.runner.example-org-example-repo.wslbox.service"));
        // The raw id appears nowhere in the parsed value.
        assert!(!format!("{g:?}").contains("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn a_truncated_or_unidentifiable_probe_is_no_reading() {
        let truncated = GUEST_OUTPUT.replace("PROBE_END\n", "");
        assert_eq!(parse_guest_probe(&truncated), None);
        let no_id = GUEST_OUTPUT.replace("0123456789abcdef0123456789abcdef", "");
        assert_eq!(parse_guest_probe(&no_id), None);
    }
}
