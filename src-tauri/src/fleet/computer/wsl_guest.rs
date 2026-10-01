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
///
/// A name starting with `-` is refused outright: it is passed to
/// `wsl.exe -d <name>`, where it would be parsed as an option.
pub(crate) fn is_reportable_distro(name: &str) -> bool {
    !name.starts_with('-') && !name.to_ascii_lowercase().starts_with("docker-desktop")
}

/// Every WSL2 distro runs in ONE utility VM with one kernel, so `/proc/vmstat`
/// `oom_kill` is the same counter in every guest. Reporting it per guest would
/// count each VM-level kill once per distro; keep it on the first guest of
/// each `boot_id` (the VM) and drop it from the rest.
///
/// And when a runner is ACTIVE inside any guest of a VM, that runner owns the
/// VM-wide remainder (it has full cgroup attribution, which this probe does
/// not): the host's probe then reports NO counter for that VM at all.
///
/// Both sides key the remainder `oom_kill:[boot, "", total]`, but coord
/// dedupes per COMPUTER, so that id merges a hand-over race ONLY when the
/// host's owner guest for the VM is the very distro the in-guest runner
/// reports for. With several distros the two reports land on different
/// computers and a kill in the race window is counted on each — the
/// remaining, accepted double count. PURE.
///
/// The guests are first sorted by `identity_hash`, so the owner is STABLE
/// across ticks regardless of `wsl --list` order — an owner that flipped
/// between guests would turn one counter into two interleaved baselines.
pub(crate) fn attribute_vm_oom_once(guests: &mut [GuestProbe]) {
    guests.sort_by(|a, b| a.identity_hash.cmp(&b.identity_hash));
    let runner_vms: std::collections::BTreeSet<String> = guests
        .iter()
        .filter(|g| g.runner_active == Some(true))
        .map(|g| g.boot_id.clone().unwrap_or_default())
        .collect();
    for g in guests.iter_mut() {
        if runner_vms.contains(&g.boot_id.clone().unwrap_or_default()) {
            g.oom_kill_total = None;
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for g in guests.iter_mut() {
        let vm = g.boot_id.clone().unwrap_or_default();
        if !seen.insert(vm) {
            g.oom_kill_total = None;
        }
    }
}

/// The single in-guest probe. One line on purpose (it crosses a Windows
/// command line). `TZ=UTC` pins `StateChangeTimestamp` to a zone the parser
/// trusts; `.runner` is read without `sudo` (a `0700` home simply yields no
/// line and the unit-name fallback applies).
///
/// **The unit set matches the in-guest scan's.** `list-units --all` lists only
/// LOADED units, so an installed runner that is disabled, stopped and
/// unloaded would be missing — and a "complete" inventory without it would
/// delete its row. The script therefore unions `list-units --all` with
/// `list-unit-files` (templates `foo@.service` excluded), exactly the
/// `ListUnitsByPatterns` ∪ `ListUnitFilesByPatterns` the runner's D-Bus scan
/// uses, and `systemctl show` on an unloaded name loads it on demand just as
/// that scan's `LoadUnit` does — so both reporters produce the same rows.
/// `LIST_RC` is 0 only when BOTH listings exited 0; `UNIT_COUNT` is the size
/// of the union and `SHOW_RC` the exit of `systemctl show` — a listing is only
/// a COMPLETE inventory when show also exited 0 and returned one `Id=` record
/// per listed unit (see [`GuestProbe::inventory_complete`]).
pub(crate) const GUEST_SCRIPT: &str = r#"printf 'MACHINE_ID\t%s\n' "$(cat /etc/machine-id 2>/dev/null)"; printf 'BOOT_ID\t%s\n' "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"; printf 'HOSTNAME\t%s\n' "$(cat /proc/sys/kernel/hostname 2>/dev/null)"; printf 'KERNEL\t%s\n' "$(uname -r 2>/dev/null)"; printf 'ARCH\t%s\n' "$(uname -m 2>/dev/null)"; printf 'OS\t%s\n' "$(. /etc/os-release 2>/dev/null; printf '%s' "$NAME")"; printf 'OS_VERSION\t%s\n' "$(. /etc/os-release 2>/dev/null; printf '%s' "$VERSION_ID")"; printf 'NPROC\t%s\n' "$(nproc 2>/dev/null)"; awk '/^MemTotal:/{printf "MEM_TOTAL_KB\t%s\n",$2} /^SwapTotal:/{printf "SWAP_TOTAL_KB\t%s\n",$2}' /proc/meminfo 2>/dev/null; printf 'DISK_TOTAL\t%s\n' "$(df -B1 / 2>/dev/null | awk 'NR==2{print $2}')"; awk '/^btime /{printf "BTIME\t%s\n",$2}' /proc/stat 2>/dev/null; awk '/^oom_kill /{printf "OOM_KILL\t%s\n",$2}' /proc/vmstat 2>/dev/null; printf 'PID1\t%s\n' "$(cat /proc/1/comm 2>/dev/null)"; sf="$HOME/.qontinui/runner/computer-observer.json"; fresh=0; if [ -n "$HOME" ] && [ -f "$sf" ]; then m=$(stat -c %Y "$sf" 2>/dev/null); n=$(date +%s); if [ -n "$m" ] && [ $((n - m)) -lt 600 ]; then fresh=1; fi; fi; sysl=$(systemctl list-units --type=service --state=active --plain --no-legend 'qontinui-runner*' 2>/dev/null); src=$?; usrl=$(XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}" systemctl --user list-units --type=service --state=active --plain --no-legend 'qontinui-runner*' 2>/dev/null); urc=$?; act=$(printf '%s\n%s\n' "$sysl" "$usrl" | awk 'NF' | wc -l); if [ "$fresh" = 1 ] || [ "$act" -gt 0 ]; then printf 'RUNNER_ACTIVE\t1\n'; elif [ "$src" = 0 ] && [ "$urc" = 0 ] && [ -n "$HOME" ]; then printf 'RUNNER_ACTIVE\t0\n'; fi; listing=$(systemctl list-units --type=service --plain --no-legend --all 'actions.runner.*' 2>/dev/null); lrc=$?; files=$(systemctl list-unit-files --type=service --no-legend 'actions.runner.*' 2>/dev/null); frc=$?; if [ "$lrc" = 0 ] && [ "$frc" = 0 ]; then printf 'LIST_RC\t0\n'; elif [ "$lrc" != 0 ]; then printf 'LIST_RC\t%s\n' "$lrc"; else printf 'LIST_RC\t%s\n' "$frc"; fi; units=$(printf '%s\n%s\n' "$listing" "$files" | awk 'NF && $1 !~ /@\./ {print $1}' | sort -u); printf 'UNIT_COUNT\t%s\n' "$(printf '%s\n' "$units" | awk 'NF' | wc -l)"; printf 'SHOW_BEGIN\n'; shrc=0; if [ -n "$units" ]; then TZ=UTC systemctl show $units -p Id,ActiveState,SubState,Result,Restart,OOMPolicy,MemoryMax,MemoryPeak,NRestarts,StateChangeTimestamp,ExecMainStatus,WorkingDirectory,ExecStart,ControlGroup,LoadState 2>/dev/null; shrc=$?; fi; printf '\nSHOW_END\n'; printf 'SHOW_RC\t%s\n' "$shrc"; for u in $units; do wd=$(systemctl show -p WorkingDirectory --value "$u" 2>/dev/null); if [ -n "$wd" ] && [ -r "$wd/.runner" ]; then printf 'RUNNERFILE\t%s\t%s\n' "$u" "$(tr -d '\n\r\t' < "$wd/.runner")"; fi; done; printf 'PROBE_END\n'"#;

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
    /// PID 1 is systemd.
    pub(crate) systemd: bool,
    /// Exit status of `systemctl list-units`. The unit list is authoritative
    /// (an empty list means "no runner units", not "could not ask") only when
    /// systemd is PID 1 AND this is `Some(0)`.
    pub(crate) list_rc: Option<i32>,
    /// A runner is REPORTING from inside this guest — that runner owns the
    /// VM's OOM remainder. Decided in the guest by either signal:
    /// * the in-guest reporter's state file
    ///   (`$HOME/.qontinui/runner/computer-observer.json`, the runner's default
    ///   state location for the guest's default user) was written within
    ///   2 × the full-report cadence — it is rewritten every tick;
    /// * a `qontinui-runner*` unit is active on the system bus or the default
    ///   user's bus (`XDG_RUNTIME_DIR` defaulted to `/run/user/$(id -u)`).
    ///
    /// `Some(false)` only when the file is absent/stale AND both `systemctl`
    /// queries answered; otherwise the guest prints nothing and this is
    /// `None` — and the host then reports the remainder (loud over silent).
    /// A runner whose state dir is moved by `QONTINUI_HOME` is visible only
    /// through its unit.
    pub(crate) runner_active: Option<bool>,
    /// Units the union listing named (`UNIT_COUNT`).
    pub(crate) unit_count: Option<usize>,
    /// Exit status of `systemctl show` (`SHOW_RC`).
    pub(crate) show_rc: Option<i32>,
    /// `Id=` records in the show block, counted raw (before any filtering),
    /// so it is comparable with [`Self::unit_count`].
    pub(crate) show_ids: usize,
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
                if line.starts_with("Id=") {
                    g.show_ids += 1;
                }
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
            "LIST_RC" => g.list_rc = v.trim().parse().ok(),
            "UNIT_COUNT" => g.unit_count = v.trim().parse().ok(),
            "SHOW_RC" => g.show_rc = v.trim().parse().ok(),
            "RUNNER_ACTIVE" => g.runner_active = v.trim().parse::<u32>().ok().map(|n| n > 0),
            "RUNNERFILE" => {
                if let Some(json) = parts.next() {
                    g.runner_files.insert(v.to_string(), json.to_string());
                }
            }
            _ => {}
        }
    }
    // A guest is Linux: the same machine-id(5) rule as the host.
    let machine_id = machine_id.filter(|m| super::identity::valid_linux_machine_id(m))?;
    g.identity_hash = identity_hash_of(&machine_id)?;
    g.units = parse_systemctl_show(&show);
    Some(g)
}

impl GuestProbe {
    /// Whether this probe is a COMPLETE inventory of the guest's
    /// `actions.runner.*` units: both listings exited 0, `systemctl show`
    /// exited 0, and it returned exactly one `Id=` record per listed unit. A
    /// failed or timed-out show yields zero rows — claimed complete, that
    /// would delete every stored runner row and resolve their alerts.
    pub(crate) fn inventory_complete(&self) -> bool {
        self.list_rc == Some(0) && self.show_rc == Some(0) && self.unit_count == Some(self.show_ids)
    }
}

#[cfg(windows)]
pub(crate) mod probe {
    use super::{
        is_reportable_distro, parse_guest_probe, parse_running_distros, GuestProbe, GUEST_SCRIPT,
    };
    use crate::fleet::resource_sample::{decode_utf16le, wsl_probe};

    /// The distros running RIGHT NOW (non-waking read), or `None` when the
    /// listing itself failed.
    async fn running() -> Option<Vec<String>> {
        let list = wsl_probe(&["--list", "--running", "--quiet"]).await?;
        list.status
            .success()
            .then(|| parse_running_distros(&decode_utf16le(&list.stdout)))
    }

    /// Probe every RUNNING, reportable distro. A distro that is not listed as
    /// running is not touched at all — and the list is re-read immediately
    /// before EACH probe, because a guest probe takes seconds and a distro
    /// that stopped meanwhile would be woken by `wsl -d`.
    pub(crate) async fn collect() -> Vec<GuestProbe> {
        let Some(initial) = running().await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for distro in initial.into_iter().filter(|d| is_reportable_distro(d)) {
            match running().await {
                Some(now) if now.contains(&distro) => {}
                _ => continue,
            }
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
        super::attribute_vm_oom_once(&mut out);
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
        assert!(!is_reportable_distro("-d"));
        assert!(!is_reportable_distro("--exec"));
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
LIST_RC\t0\n\
RUNNER_ACTIVE\t0\n\
UNIT_COUNT\t1\n\
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
SHOW_RC\t0\n\
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

    /// Run the REAL guest script under `sh` against a stub `systemctl`, so
    /// the shell half (the list-units ∪ list-unit-files union, the template
    /// exclusion, `LIST_RC`) is exercised, not just the parser.
    #[cfg(unix)]
    fn run_guest_script(fail_unit_files: bool) -> GuestProbe {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "qontinui-guest-script-{}-{}",
            std::process::id(),
            fail_unit_files
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("systemctl");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
case "$*" in
  *list-unit-files*)
    [ -n "$FAIL_UNIT_FILES" ] && exit 1
    printf 'actions.runner.example-org-example-repo.wslbox.service enabled enabled\n'
    printf 'actions.runner.example-org-other-repo.wslbox.service disabled enabled\n'
    printf 'actions.runner.example-template@.service static -\n' ;;
  *list-units*actions.runner*)
    printf 'actions.runner.example-org-example-repo.wslbox.service loaded active running GitHub Actions Runner\n' ;;
  *list-units*) : ;;
  *show*)
    for u in "$@"; do
      case "$u" in
        actions.runner.example-org-example-repo.wslbox.service)
          printf 'Id=%s\nLoadState=loaded\nActiveState=active\nSubState=running\n\n' "$u" ;;
        actions.runner.*)
          printf 'Id=%s\nLoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\n\n' "$u" ;;
      esac
    done ;;
esac
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", GUEST_SCRIPT])
            .env("PATH", path)
            .env("HOME", &dir);
        if fail_unit_files {
            cmd.env("FAIL_UNIT_FILES", "1");
        } else {
            cmd.env_remove("FAIL_UNIT_FILES");
        }
        let out =
            crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(20))
                .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let _ = std::fs::remove_dir_all(&dir);
        // The test host's own machine-id may be absent (containers); the
        // probe needs one to parse, so substitute a synthetic id line.
        let text = text
            .lines()
            .map(|l| {
                if l.starts_with("MACHINE_ID\t") {
                    "MACHINE_ID\tfedcba9876543210fedcba9876543210".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        parse_guest_probe(&format!("{text}\n")).expect("the script output parses")
    }

    /// A runner unit that is installed but disabled, stopped and UNLOADED is
    /// absent from `list-units --all`; the union with `list-unit-files` puts
    /// it back, with the same `inactive/dead` row the in-guest D-Bus scan
    /// reports. Templates are excluded, as that scan excludes them.
    #[cfg(unix)]
    #[test]
    fn a_unit_known_only_to_list_unit_files_is_reported() {
        let g = run_guest_script(false);
        let units: Vec<(&str, Option<&str>)> = g
            .units
            .iter()
            .map(|u| (u.unit.as_str(), u.active_state.as_deref()))
            .collect();
        assert_eq!(
            units,
            vec![
                (
                    "actions.runner.example-org-example-repo.wslbox.service",
                    Some("active")
                ),
                (
                    "actions.runner.example-org-other-repo.wslbox.service",
                    Some("inactive")
                ),
            ]
        );
        assert_eq!(g.list_rc, Some(0));
        assert_eq!((g.unit_count, g.show_rc, g.show_ids), (Some(2), Some(0), 2));
        assert!(g.inventory_complete(), "the counts cover the union");
        // A failed list-unit-files makes the listing incomplete.
        assert_eq!(run_guest_script(true).list_rc, Some(1));
    }

    #[test]
    fn a_guest_inventory_is_complete_only_when_show_answered_for_every_unit() {
        let g = parse_guest_probe(GUEST_OUTPUT).unwrap();
        assert_eq!((g.unit_count, g.show_rc, g.show_ids), (Some(1), Some(0), 1));
        assert!(g.inventory_complete());
        // `systemctl show` failed: zero or partial rows, never complete.
        let failed = parse_guest_probe(&GUEST_OUTPUT.replace("SHOW_RC\t0", "SHOW_RC\t1")).unwrap();
        assert!(!failed.inventory_complete());
        // Show answered for fewer units than were listed.
        let short =
            parse_guest_probe(&GUEST_OUTPUT.replace("UNIT_COUNT\t1", "UNIT_COUNT\t2")).unwrap();
        assert!(!short.inventory_complete());
        // A probe from an older script (no counts): not complete.
        let old = parse_guest_probe(
            &GUEST_OUTPUT
                .replace("UNIT_COUNT\t1\n", "")
                .replace("SHOW_RC\t0\n", ""),
        )
        .unwrap();
        assert!(!old.inventory_complete());
        // The listing failed: not complete.
        let listing = parse_guest_probe(&GUEST_OUTPUT.replace("LIST_RC\t0", "LIST_RC\t1")).unwrap();
        assert!(!listing.inventory_complete());
    }

    #[test]
    fn runner_liveness_is_three_valued_and_matches_the_reporter() {
        // The script checks the state file the reporter actually writes, at
        // 2 × the full cadence.
        assert!(GUEST_SCRIPT.contains("/.qontinui/runner/computer-observer.json"));
        let window = 2 * crate::fleet::computer::REPORT_FULL_SECS;
        assert!(GUEST_SCRIPT.contains(&format!("-lt {window} ]")));
        assert!(GUEST_SCRIPT.contains("XDG_RUNTIME_DIR=\"${XDG_RUNTIME_DIR:-/run/user/$(id -u)}\""));
        // Undeterminable: the guest prints an empty value or no line → None.
        let empty = GUEST_OUTPUT.replace("RUNNER_ACTIVE\t0\n", "RUNNER_ACTIVE\t\n");
        assert_eq!(parse_guest_probe(&empty).unwrap().runner_active, None);
        let absent = GUEST_OUTPUT.replace("RUNNER_ACTIVE\t0\n", "");
        let g = parse_guest_probe(&absent).unwrap();
        assert_eq!(g.runner_active, None);
        // ... and None means the host KEEPS the counter (loud over silent).
        let mut v = vec![g];
        attribute_vm_oom_once(&mut v);
        assert_eq!(v[0].oom_kill_total, Some(3));
    }

    #[test]
    fn an_active_guest_runner_takes_the_vm_remainder_from_the_host() {
        let g = parse_guest_probe(GUEST_OUTPUT).unwrap();
        assert_eq!(g.runner_active, Some(false));
        // No runner inside: the host's probe keeps the counter (reports the
        // remainder) on the VM's owner guest.
        let mut none = vec![g.clone()];
        attribute_vm_oom_once(&mut none);
        assert_eq!(none[0].oom_kill_total, Some(3));
        // A runner active in ANY guest of the VM: the host reports no
        // counter for that VM, so no remainder from this side.
        let active =
            parse_guest_probe(&GUEST_OUTPUT.replace("RUNNER_ACTIVE\t0", "RUNNER_ACTIVE\t1"))
                .unwrap();
        assert_eq!(active.runner_active, Some(true));
        let mut other = g.clone();
        other.identity_hash = "b".repeat(64);
        other.runner_active = Some(true);
        let mut vm = vec![g.clone(), other];
        attribute_vm_oom_once(&mut vm);
        assert!(vm.iter().all(|g| g.oom_kill_total.is_none()));
        // ... but only for THAT VM.
        let mut second_vm = g.clone();
        second_vm.identity_hash = "c".repeat(64);
        second_vm.boot_id = Some("11111111-2222-4333-8444-555555555555".into());
        let mut runner_here = g;
        runner_here.runner_active = Some(true);
        let mut both = vec![runner_here, second_vm];
        attribute_vm_oom_once(&mut both);
        let ooms: Vec<Option<u64>> = both.iter().map(|g| g.oom_kill_total).collect();
        assert_eq!(ooms, vec![None, Some(3)]);
    }

    #[test]
    fn a_failed_unit_listing_is_recorded() {
        assert_eq!(parse_guest_probe(GUEST_OUTPUT).unwrap().list_rc, Some(0));
        let failed = GUEST_OUTPUT.replace("LIST_RC\t0", "LIST_RC\t1");
        assert_eq!(parse_guest_probe(&failed).unwrap().list_rc, Some(1));
        let absent = GUEST_OUTPUT.replace("LIST_RC\t0\n", "");
        assert_eq!(parse_guest_probe(&absent).unwrap().list_rc, None);
    }

    #[test]
    fn a_vm_level_oom_counter_is_reported_once_per_vm() {
        let g = parse_guest_probe(GUEST_OUTPUT).unwrap();
        let mut other = g.clone();
        other.identity_hash = "b".repeat(64);
        let mut second_vm = g.clone();
        second_vm.boot_id = Some("11111111-2222-4333-8444-555555555555".into());
        // Listed with the higher identity first: ownership still goes to the
        // lowest identity of each VM, whatever the listing order.
        let mut guests = vec![other, g, second_vm];
        attribute_vm_oom_once(&mut guests);
        let got: Vec<(String, Option<u64>)> = guests
            .iter()
            .map(|g| (g.identity_hash.chars().take(4).collect(), g.oom_kill_total))
            .collect();
        assert_eq!(
            got,
            vec![
                ("14c5".to_string(), Some(3)),
                ("14c5".to_string(), Some(3)),
                ("bbbb".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_truncated_or_unidentifiable_probe_is_no_reading() {
        let truncated = GUEST_OUTPUT.replace("PROBE_END\n", "");
        assert_eq!(parse_guest_probe(&truncated), None);
        let no_id = GUEST_OUTPUT.replace("0123456789abcdef0123456789abcdef", "");
        assert_eq!(parse_guest_probe(&no_id), None);
        let uninit = GUEST_OUTPUT.replace("0123456789abcdef0123456789abcdef", "uninitialized");
        assert_eq!(parse_guest_probe(&uninit), None);
    }
}
