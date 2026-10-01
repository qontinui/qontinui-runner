//! Computer identity and static capacity (plan §3.1, Phase 2.1).
//!
//! ## The identity hash, and why the raw id never leaves this module
//!
//! A computer is keyed by `identity_hash` = lowercase hex
//! HMAC-SHA256(key = `b"qontinui-computer-identity-v1"`, msg = the trimmed OS
//! machine id) — systemd's `sd_id128_get_machine_app_specific` pattern. The
//! raw id (`/etc/machine-id`, Windows `MachineGuid`, macOS `IOPlatformUUID`)
//! is a stable cross-application tracking key; hashing it under an
//! app-specific key gives coord a stable per-computer key that no other
//! application can correlate with. The raw id is read, hashed and dropped in
//! [`identity_hash_of`]; it is never logged, stored or sent.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The HMAC key — the contract's app id. Changing it re-keys every computer in
/// the fleet, so it is versioned in the string itself.
pub(crate) const IDENTITY_KEY: &[u8] = b"qontinui-computer-identity-v1";

/// HMAC the trimmed raw machine id. `None` for a blank id — hashing an empty
/// string would give every machine without an id the SAME identity, which is
/// the silent merge §6 forbids.
pub(crate) fn identity_hash_of(raw_machine_id: &str) -> Option<String> {
    let trimmed = raw_machine_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(IDENTITY_KEY).ok()?;
    mac.update(trimmed.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Parse `ioreg -rd1 -c IOPlatformExpertDevice` for `IOPlatformUUID`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_ioreg_platform_uuid(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (_, rest) = l.split_once("\"IOPlatformUUID\"")?;
        let v = rest.split_once('=')?.1.trim().trim_matches('"').trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// This OS's raw machine id. Private on purpose — see the module docs.
fn raw_machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .filter(|s| !s.trim().is_empty())
    }
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
        use winreg::RegKey;
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(
                r"SOFTWARE\Microsoft\Cryptography",
                KEY_READ | KEY_WOW64_64KEY,
            )
            .ok()?
            .get_value::<String, _>("MachineGuid")
            .ok()
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = crate::process_helpers::no_window("ioreg");
        cmd.args(["-rd1", "-c", "IOPlatformExpertDevice"]);
        let out =
            crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(5))
                .ok()?;
        parse_ioreg_platform_uuid(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        None
    }
}

/// This host's identity hash, computed once per process (the machine id does
/// not change under a running process). `None` = this computer cannot be
/// identified, and nothing is reported for it.
pub(crate) fn host_identity_hash() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| raw_machine_id().and_then(|id| identity_hash_of(&id)))
        .clone()
}

/// `host` | `wsl_guest` | `container` — what the computer the runner PROCESS
/// runs on is. A Linux runner inside WSL is itself on a guest (its parent is
/// not identifiable from inside, so `parent_identity_hash` stays null); inside
/// a container it is a container.
pub(crate) fn classify_host_kind(
    osrelease: Option<&str>,
    in_container_marker: bool,
) -> &'static str {
    if in_container_marker {
        return "container";
    }
    if osrelease
        .map(|r| r.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
    {
        return "wsl_guest";
    }
    "host"
}

pub(crate) fn host_kind() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        let osrelease = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
        let container = std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists();
        classify_host_kind(osrelease.as_deref(), container)
    }
    #[cfg(not(target_os = "linux"))]
    {
        "host"
    }
}

/// Static capacity + boot facts for the host computer.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct StaticFacts {
    pub(crate) hostname: Option<String>,
    pub(crate) os: Option<String>,
    pub(crate) os_version: Option<String>,
    pub(crate) kernel: Option<String>,
    pub(crate) arch: Option<String>,
    pub(crate) cpu_cores: Option<i32>,
    pub(crate) memory_total_bytes: Option<u64>,
    pub(crate) swap_total_bytes: Option<u64>,
    pub(crate) disk_total_bytes: Option<u64>,
    pub(crate) booted_at: Option<String>,
}

/// Seconds-since-epoch → RFC3339; `0` (sysinfo's "could not read") is `None`.
pub(crate) fn epoch_secs_to_rfc3339(secs: u64) -> Option<String> {
    if secs == 0 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(secs).ok()?, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Collect [`StaticFacts`] for the host. Blocking (sysinfo disk enumeration) —
/// the caller runs it on the blocking pool.
pub(crate) fn collect_static() -> StaticFacts {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let mem = sys.total_memory();

    // Swap capacity is a real, independent figure on Linux/macOS. On Windows
    // sysinfo derives "swap" from the commit charge — see resource_sample's
    // host lane for why that must not be published under a swap name.
    #[cfg(not(windows))]
    let swap = Some(sys.total_swap());
    #[cfg(windows)]
    let swap: Option<u64> = None;

    StaticFacts {
        hostname: sysinfo::System::host_name().filter(|s| !s.trim().is_empty()),
        os: sysinfo::System::name(),
        os_version: sysinfo::System::os_version(),
        kernel: sysinfo::System::kernel_version(),
        arch: Some(std::env::consts::ARCH.to_string()),
        cpu_cores: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get().min(i32::MAX as usize) as i32),
        memory_total_bytes: (mem > 0).then_some(mem),
        swap_total_bytes: swap,
        disk_total_bytes: system_volume_total(),
        booted_at: epoch_secs_to_rfc3339(sysinfo::System::boot_time()),
    }
}

/// Total size of the system volume: `/` on unix, `%SystemDrive%\` on Windows.
/// `None` when it is not among the enumerated disks.
fn system_volume_total() -> Option<u64> {
    // `%SystemDrive%` unset is UNKNOWN, not a guessed drive letter.
    #[cfg(windows)]
    let root = std::path::PathBuf::from(format!("{}\\", std::env::var("SystemDrive").ok()?));
    #[cfg(not(windows))]
    let root = std::path::PathBuf::from("/");
    sysinfo::Disks::new_with_refreshed_list()
        .list()
        .iter()
        .find(|d| d.mount_point() == root.as_path())
        .map(|d| d.total_space())
        .filter(|t| *t > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal vectors computed independently (Python `hmac`), so the test
    /// pins the algorithm, the key and the trimming — not this module's
    /// own constant.
    #[test]
    fn identity_hash_matches_an_independent_hmac() {
        assert_eq!(
            identity_hash_of("0123456789abcdef0123456789abcdef\n").as_deref(),
            Some("14c5ba9f46f5b062dfae3190c6c7e1d25a6ba40a5043c5c78cb882c3215dc16f")
        );
        // Windows MachineGuid shape: case is preserved (raw bytes, trimmed).
        assert_eq!(
            identity_hash_of("  00000000-1111-4222-8333-444455556666 ").as_deref(),
            Some("b6723a4aa0a28c9e28211fa6d4cb817dc54626b7c03404e9a00e5e73fd4e259b")
        );
    }

    #[test]
    fn a_blank_machine_id_has_no_identity() {
        assert_eq!(identity_hash_of(""), None);
        assert_eq!(identity_hash_of(" \n"), None);
    }

    #[test]
    fn ioreg_platform_uuid_parses() {
        let text = r#"+-o Mac-mini  <class IOPlatformExpertDevice, id 0x100000110, registered>
    {
      "IOPlatformSerialNumber" = "C07XXXXX"
      "IOPlatformUUID" = "A1B2C3D4-0000-1111-2222-333344445555"
    }"#;
        assert_eq!(
            parse_ioreg_platform_uuid(text).as_deref(),
            Some("A1B2C3D4-0000-1111-2222-333344445555")
        );
        assert_eq!(parse_ioreg_platform_uuid("nothing"), None);
    }

    #[test]
    fn host_kind_classification() {
        assert_eq!(
            classify_host_kind(Some("6.1.0-example-amd64\n"), false),
            "host"
        );
        assert_eq!(
            classify_host_kind(Some("5.15.153.1-microsoft-standard-WSL2\n"), false),
            "wsl_guest"
        );
        assert_eq!(classify_host_kind(Some("6.12.0"), true), "container");
        assert_eq!(classify_host_kind(None, false), "host");
    }

    #[test]
    fn boot_time_zero_is_unknown() {
        assert_eq!(epoch_secs_to_rfc3339(0), None);
        assert_eq!(
            epoch_secs_to_rfc3339(1_790_000_000).as_deref(),
            Some("2026-09-21T14:13:20Z")
        );
    }
}
