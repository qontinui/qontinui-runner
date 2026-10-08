//! Runner LAUNCH identity **as read from the process env** — the one canonical
//! reader of `QONTINUI_INSTANCE_NAME` (which instance is this?) and
//! `QONTINUI_SERVER_MODE` (was it launched headless?).
//!
//! In the LIB crate for the same reason as `runner_breadcrumb` / `mcp_spill`:
//! a second bin cannot import from the runner bin's module tree, and
//! `bin/qontinui_profile.rs` needs the SAME primary/secondary predicate the
//! runner bin's tier-persist guard uses ([`crate::profiles::promote_tier_to_account`]).
//! One module ⇒ one predicate ⇒ the two doors cannot drift.
//!
//! The rest of `crate::instance` (path scoping, `RunnerKind`, the WebView2
//! data dir, primary registration) stays in the runner bin: it reaches into
//! `crate::mcp::types` and `crate::session`, which are bin-only. `instance.rs`
//! re-exports these two functions, so every `crate::instance::is_secondary()`
//! call site in the bin resolves here.
//!
//! It is also the one reader of `QONTINUI_INSTANCE_ROOT` — the single directory
//! a SUBJECT runner (one launched by a harness runner, plan
//! `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`)
//! keeps every runner-owned location under. It lives here, not in the bin's
//! `instance.rs`, because the LIB's `pair` and `auth` must consult it too.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The env var naming a subject runner's instance root (D1 of plan
/// `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`).
pub const INSTANCE_ROOT_ENV: &str = "QONTINUI_INSTANCE_ROOT";

/// `<root>/secure` — the secure-storage / binding-store directory a subject
/// defaults `QONTINUI_SECURE_STORAGE_DIR` to.
pub const INSTANCE_ROOT_SECURE_SUBDIR: &str = "secure";
/// `<root>/config` — the default for `QONTINUI_CONFIG_DIR`.
pub const INSTANCE_ROOT_CONFIG_SUBDIR: &str = "config";
/// `<root>/home` — the default for `QONTINUI_HOME` (the `~/.qontinui` seam).
pub const INSTANCE_ROOT_HOME_SUBDIR: &str = "home";

/// This runner's instance root, if it was launched with a non-blank
/// `QONTINUI_INSTANCE_ROOT`.
///
/// A RELATIVE value is still returned — and still makes this runner a
/// secondary (fail-closed) — so that every isolation decision keyed on it holds;
/// the runner bin's startup refuses to run under a relative root
/// (`instance::enforce_instance_root_at_startup`). `None` means "not a subject":
/// every behaviour is then exactly what it was before the root existed.
pub fn instance_root() -> Option<PathBuf> {
    instance_root_from(std::env::var_os("QONTINUI_INSTANCE_ROOT"))
}

/// Env-free core of [`instance_root`]. A blank (empty or whitespace-only) value
/// is not a root — the shape a `systemd` unit's `Environment=FOO=` produces —
/// matching how [`crate::ambient::qontinui_dir_from`] treats a blank override.
pub fn instance_root_from(raw: Option<OsString>) -> Option<PathBuf> {
    raw.filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

/// The raw instance name from the env, if set and non-empty.
///
/// The supervisor sets `QONTINUI_INSTANCE_NAME` when spawning non-primary
/// runners (temp runners, named runners).
pub fn instance_name() -> Option<String> {
    std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// True when this runner was launched as a non-primary instance.
///
/// This is the CONSERVATIVE side of the primary/secondary distinction, and it
/// is deliberately conservative for the settings-write guard: a secondary
/// launched with only `QONTINUI_INSTANCE_NAME` (no `QONTINUI_CONFIG_DIR`)
/// resolves the primary's SHARED `settings.json`, so any writer that could
/// demote the primary must refuse on this predicate alone.
///
/// Note this is a WEAKER check than
/// `process_capture::primary_proxy::is_secondary` (which additionally requires
/// a primary port to proxy to) and than `instance::data_subdir`'s fail-closed
/// `resolve_data_subdir` (which additionally detects a NAMELESS secondary by
/// port). Those exist for different questions — path isolation and request
/// proxying — and both build on this one.
///
/// An instance root (`QONTINUI_INSTANCE_ROOT`) also makes a runner secondary
/// (D3 of plan
/// `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`):
/// a subject started with only a root must skip the single-instance plugin, or
/// it would forward its argv to the harness and exit.
pub fn is_secondary() -> bool {
    is_secondary_from(instance_name().as_deref(), instance_root().as_deref())
}

/// Env-free core of [`is_secondary`].
pub fn is_secondary_from(name: Option<&str>, instance_root: Option<&Path>) -> bool {
    name.is_some() || instance_root.is_some()
}

/// `QONTINUI_SERVER_MODE` — was this process launched headless (`1` / `true`,
/// case-insensitive)? The ONE parse of that variable in the tree.
///
/// In the LIB for the same reason as [`is_secondary`], and then one more: the
/// runner bin's `launch_env::server_mode_from_env` re-exports it (so
/// `RunnerLaunchEnv` and `settings::load_settings_full` keep their call sites),
/// AND `profiles::read_runner_tier` needs it. That reader is the tier answer
/// every in-process coord consumer gets, and it has to agree with the tier
/// `settings::load_settings` resolves — a hardcoded `false` there meant a
/// headless NAMED secondary ran its relay as Tier 2 while
/// `profiles::connected_coord_base` returned `None` for the same process.
///
/// # Why a free function and not a field on the launch snapshot
///
/// The typed `RunnerLaunchEnv` snapshot lives on Tauri app state and is `None`
/// until `main()` has taken it, but settings (and the tier) are read from paths
/// that run before, beside and entirely outside `main()`'s setup — the
/// `config_report` path, the `qontinui_profile` bin, tests. A `None` there
/// would silently read as "not headless", i.e. the exact defect this shared
/// accessor exists to prevent. So the accessor is shared, not the value.
pub fn server_mode() -> bool {
    std::env::var("QONTINUI_SERVER_MODE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_root_is_absent_when_unset_or_blank() {
        assert_eq!(instance_root_from(None), None);
        assert_eq!(instance_root_from(Some(OsString::from(""))), None);
        assert_eq!(instance_root_from(Some(OsString::from("   "))), None);
    }

    #[test]
    fn instance_root_is_the_raw_value_when_set() {
        assert_eq!(
            instance_root_from(Some(OsString::from("/srv/subject-1"))),
            Some(PathBuf::from("/srv/subject-1"))
        );
        // A relative root is still a root (fail-closed secondary); startup is
        // what refuses it.
        assert_eq!(
            instance_root_from(Some(OsString::from("subject"))),
            Some(PathBuf::from("subject"))
        );
    }

    #[test]
    fn an_instance_root_alone_makes_the_runner_secondary() {
        assert!(!is_secondary_from(None, None), "the primary stays primary");
        assert!(is_secondary_from(Some("test-1"), None));
        assert!(is_secondary_from(None, Some(Path::new("/srv/subject-1"))));
        assert!(is_secondary_from(
            Some("test-1"),
            Some(Path::new("/srv/subject-1"))
        ));
    }
}
