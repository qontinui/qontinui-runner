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
use std::path::{Component, Path, PathBuf};

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
/// `<root>/embedded-pg` — the default for `QONTINUI_EMBEDDED_PG_DIR`: a subject
/// runs its own private cluster, never the machine-shared one.
pub const INSTANCE_ROOT_EMBEDDED_PG_SUBDIR: &str = "embedded-pg";
/// `<root>/logs` — the default for `QONTINUI_RUNNER_LOG_DIR` and
/// `QONTINUI_PANIC_LOG_DIR` (the startup panic log lands in the former).
pub const INSTANCE_ROOT_LOGS_SUBDIR: &str = "logs";
/// `<root>/webview` — the default for `WEBVIEW2_USER_DATA_FOLDER`.
pub const INSTANCE_ROOT_WEBVIEW_SUBDIR: &str = "webview";
/// `<root>/startup-refusal.log` — where a refused start is recorded, because a
/// Windows GUI-subsystem binary has no stderr anyone can read.
pub const INSTANCE_ROOT_REFUSAL_LOG: &str = "startup-refusal.log";

/// This runner's instance root, if it was launched with a non-blank
/// `QONTINUI_INSTANCE_ROOT`.
///
/// A RELATIVE value is still returned — and still makes this runner a
/// secondary (fail-closed) — so that every isolation decision keyed on it holds;
/// every binary's startup refuses to run under a relative root
/// ([`enforce_instance_root_or_exit`]). `None` means "not a subject":
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
///
/// What a root-only subject therefore also skips, BY DESIGN, because each is a
/// primary-only, machine-wide duty the harness already performs: the scheduler
/// service and managed-process auto-start (`main.rs`, "skip for secondary
/// instances" — duplicate executions and port conflicts with the harness), the
/// PostgreSQL instance-registry self-registration, and restoring previously
/// active instances. A subject is the thing under test, not a second fleet
/// member. What it does NOT skip is persisting its own settings: those guards
/// read [`shares_primary_settings`], not this predicate.
pub fn is_secondary() -> bool {
    is_secondary_from(instance_name().as_deref(), instance_root().as_deref())
}

/// Env-free core of [`is_secondary`].
pub fn is_secondary_from(name: Option<&str>, instance_root: Option<&Path>) -> bool {
    name.is_some() || instance_root.is_some()
}

/// True when this runner's `settings.json` may be the PRIMARY's — the predicate
/// every settings / tier WRITE guard refuses on.
///
/// A named secondary resolves the primary's shared `settings.json` unless its
/// launcher redirected it, so those guards keep the conservative
/// [`is_secondary`] answer for it — unchanged. A runner under an instance root
/// is the exception: its config is `<root>/config` by construction (the launcher
/// may only point `QONTINUI_CONFIG_DIR` inside the root, and startup defaults it
/// there), so it can never clobber the harness's file, and it must be able to
/// persist its own sign-in and tier like any installation.
pub fn shares_primary_settings() -> bool {
    shares_primary_settings_from(instance_name().as_deref(), instance_root().as_deref())
}

/// Env-free core of [`shares_primary_settings`].
pub fn shares_primary_settings_from(name: Option<&str>, instance_root: Option<&Path>) -> bool {
    is_secondary_from(name, instance_root) && instance_root.is_none()
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

// ============================================================================
// The OS keychain gate
// ============================================================================

/// May this process touch the OS keychain at all?
///
/// `false` when `QONTINUI_DISABLE_KEYCHAIN` is set (any value — the long-standing
/// meaning of the switch in `auth`) or when this runner is under an instance
/// root (D2 of plan
/// `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`):
/// keychain entries are keyed on fixed service names (`com.qontinui.runner`,
/// `com.qontinui.runner.ai`, …) SHARED by every runner on the box, so a subject
/// reading one would act on the harness's secrets and writing one would
/// overwrite them.
///
/// Every `keyring::Entry::new` outside `auth.rs` sits behind this gate — the
/// `ambient` source-scan test `every_keyring_entry_is_behind_the_keychain_gate`
/// enforces it. A gated read answers "no entry", a gated delete is a no-op, and
/// a gated write fails with [`keychain_disabled_error`].
pub fn keychain_allowed() -> bool {
    keychain_allowed_from(
        std::env::var_os("QONTINUI_DISABLE_KEYCHAIN"),
        instance_root().as_deref(),
    )
}

/// Env-free core of [`keychain_allowed`].
pub fn keychain_allowed_from(
    disable_keychain: Option<OsString>,
    instance_root: Option<&Path>,
) -> bool {
    disable_keychain.is_none() && instance_root.is_none()
}

/// The error a gated keychain WRITE returns: a clear refusal rather than a
/// silent success that would lose the secret.
pub fn keychain_disabled_error() -> keyring::Error {
    keyring::Error::NoStorageAccess(
        "the OS keychain is disabled for this runner (QONTINUI_DISABLE_KEYCHAIN is set, or it \
         runs under QONTINUI_INSTANCE_ROOT and must not share the machine keychain)"
            .into(),
    )
}

// ============================================================================
// Subject-runner instance root — the startup contract
// (plan `2026-10-04-a-subject-runner-must-be-fully-isolated-from-the-harness-runner`,
// Phase 2: D1 defaults, D2 no keychain, D3 port refusal)
// ============================================================================

/// Exit status of a process that refuses to start under an invalid instance
/// root: 78, `EX_CONFIG` in `sysexits.h` — a launch-configuration error, kept
/// distinct from the `1` / `2` the runner's `main` uses for an application
/// error / a panic, so a launcher can tell "you launched me wrong" from "I
/// crashed".
pub const INSTANCE_ROOT_REFUSAL_EXIT: i32 = 78;

/// The location overrides that, under an instance root, may only point INSIDE
/// it — the runner-read `*_DIR` keys of `ambient::AMBIENT_ENV_KEYS`, plus
/// `QONTINUI_HOME` and the WebView2 profile folder. Refused rather than clamped
/// (plan D1): a silent clamp would hide the launcher bug that set them.
pub const ROOT_CONFINED_ENV_KEYS: &[&str] = &[
    "QONTINUI_CAPABILITY_STATE_DIR",
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_EMBEDDED_PG_DIR",
    "QONTINUI_HOME",
    "QONTINUI_PANIC_LOG_DIR",
    "QONTINUI_PROMPTS_DIR",
    "QONTINUI_RUNNER_LOG_DIR",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_SESSION_NAMES_DIR",
    "WEBVIEW2_USER_DATA_FOLDER",
];

/// Whether the launch contract checks `QONTINUI_PORT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortCheck<'a> {
    /// The runner bin, which binds its API on `QONTINUI_PORT`: the raw value
    /// must be set, parse EXACTLY as the runtime parses it, and not be the
    /// primary's port (D3).
    Required(Option<&'a str>),
    /// A CLI bin, which binds nothing.
    NotApplicable,
}

/// Is an env value absent for the purposes of a path override? Unset, empty and
/// whitespace-only all read as "no override" — the same rule
/// `ambient::qontinui_dir_from` applies.
fn is_blank(value: Option<&OsString>) -> bool {
    value.is_none_or(|v| v.to_string_lossy().trim().is_empty())
}

/// `path` with `.` components dropped and `..` applied, without touching the
/// filesystem — so a path that does not exist yet can still be judged, and a
/// symlink is not followed. A trailing separator is not a component, so
/// `/a/b/` and `/a/b` normalize alike.
pub fn lexically_normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn has_parent_dir(path: &Path) -> bool {
    path.components().any(|c| c == Component::ParentDir)
}

/// The stable instance id of a root: `root-<16 hex>`, a 64-bit FNV-1a over the
/// root's [`lexically_normalized`] path, so a trailing separator or a `./`
/// does not change it. FNV rather than `DefaultHasher` because the id names
/// on-disk state and must not move between Rust releases.
pub fn rooted_instance_id(root: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lexically_normalized(root).to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("root-{hash:016x}")
}

/// The machine-global directories an instance root must not CONTAIN: the
/// user's data-local, config and home directories and `~/.qontinui`. A root
/// that is one of them, or an ancestor of one, would put the harness's own
/// state inside the subject's root.
pub fn machine_global_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    [
        dirs::data_local_dir(),
        dirs::config_dir(),
        crate::ambient::qontinui_dir_from(None, home.clone()),
        home,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Env-free check of a subject's launch environment, run BEFORE the
/// root-derived defaults are applied, so only what the launcher actually
/// supplied is judged.
///
/// - `root` must be absolute, carry no `..`, not be a filesystem root, and not
///   be (an ancestor of) any of `machine_dirs` ([`machine_global_dirs`]).
/// - `port`: see [`PortCheck`]. Validated EXACTLY as `mcp::types::get_mcp_api_port`
///   and `launch_env` parse it — no trimming — because a value they fail to
///   parse silently falls back to the primary's port.
/// - `overrides`: each [`ROOT_CONFINED_ENV_KEYS`] key with its raw value; a
///   non-blank value must be absolute, carry no `..`, and lie lexically inside
///   `root` (the root itself counts).
///
/// Every problem is reported, not only the first, so one refused launch names
/// everything the launcher has to fix.
pub fn validate_instance_root(
    root: &Path,
    port: PortCheck<'_>,
    overrides: &[(&str, Option<OsString>)],
    machine_dirs: &[PathBuf],
) -> Result<(), String> {
    if let Some(problem) = root_problem(root, machine_dirs) {
        return Err(problem);
    }
    let mut problems: Vec<String> = Vec::new();

    if let PortCheck::Required(raw) = port {
        let primary = crate::runner_breadcrumb::PRIMARY_PORT;
        match raw {
            None | Some("") => problems.push(format!(
                "QONTINUI_PORT is unset; a runner under an instance root needs its own explicit \
                 port (never the primary's {primary})"
            )),
            Some(raw) => match raw.parse::<u16>() {
                Err(_) => problems.push(format!(
                    "QONTINUI_PORT={raw:?} is not a valid port (the runtime would silently fall \
                     back to {primary})"
                )),
                Ok(0) => problems.push("QONTINUI_PORT=0 is not a fixed port".to_string()),
                Ok(p) if p == primary => problems.push(format!(
                    "QONTINUI_PORT={p} is the primary runner's port; a runner under an instance \
                     root must use another"
                )),
                Ok(_) => {}
            },
        }
    }

    let normalized_root = lexically_normalized(root);
    for (key, value) in overrides {
        let Some(value) = value.as_ref().filter(|v| !is_blank(Some(v))) else {
            continue;
        };
        let path = Path::new(value);
        if !path.is_absolute()
            || has_parent_dir(path)
            || !lexically_normalized(path).starts_with(&normalized_root)
        {
            problems.push(format!(
                "{key}={path:?} is outside the instance root {root:?} (it must be an absolute \
                 path inside it, with no `..`)"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// The root-level refusals: why `root` itself cannot be an instance root, if
/// it cannot. Separate from the rest of [`validate_instance_root`] because a
/// root that passes these is safe to write a refusal log into.
fn root_problem(root: &Path, machine_dirs: &[PathBuf]) -> Option<String> {
    if !root.is_absolute() {
        return Some(format!(
            "QONTINUI_INSTANCE_ROOT must be an absolute path, got {root:?}"
        ));
    }
    if has_parent_dir(root) {
        return Some(format!(
            "QONTINUI_INSTANCE_ROOT must not contain `..`, got {root:?}"
        ));
    }
    let normalized = lexically_normalized(root);
    if !normalized
        .components()
        .any(|c| matches!(c, Component::Normal(_)))
    {
        return Some(format!(
            "QONTINUI_INSTANCE_ROOT {root:?} is a filesystem root"
        ));
    }
    for dir in machine_dirs {
        if lexically_normalized(dir).starts_with(&normalized) {
            return Some(format!(
                "QONTINUI_INSTANCE_ROOT {root:?} contains the machine-global directory {dir:?}; \
                 a subject's root must hold only its own state"
            ));
        }
    }
    None
}

/// One environment default an instance root implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRootDefault {
    pub key: &'static str,
    pub value: OsString,
    /// The value is a directory created before it is exported.
    pub is_dir: bool,
}

/// Env-free core of the defaults a subject derives from its root (D1, D2): for
/// each key the launcher left unset, the value to export.
///
/// - `QONTINUI_SECURE_STORAGE_DIR` → `<root>/secure` (binding store, token store)
/// - `QONTINUI_CONFIG_DIR` → `<root>/config` (`settings.json`)
/// - `QONTINUI_HOME` → `<root>/home` (the `~/.qontinui` seam)
/// - `QONTINUI_EMBEDDED_PG_DIR` → `<root>/embedded-pg` (a private cluster)
/// - `QONTINUI_RUNNER_LOG_DIR`, `QONTINUI_PANIC_LOG_DIR` → `<root>/logs`
/// - `WEBVIEW2_USER_DATA_FOLDER` → `<root>/webview`
/// - `QONTINUI_DISABLE_KEYCHAIN` → `1`. [`keychain_allowed`] already answers
///   `false` under a root; exporting the switch as well carries that to `auth`
///   (whose own `keychain_enabled_env` reads only the switch) and to every
///   child the subject spawns (D5).
///
/// `current(key)` is the key's present value. A path key counts as unset when
/// blank (its readers ignore a blank value); the keychain switch only when
/// absent, because ANY value of it — even empty — already means "disabled".
pub fn instance_root_defaults(
    root: &Path,
    current: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<InstanceRootDefault> {
    let dirs: [(&'static str, &str); 7] = [
        ("QONTINUI_SECURE_STORAGE_DIR", INSTANCE_ROOT_SECURE_SUBDIR),
        ("QONTINUI_CONFIG_DIR", INSTANCE_ROOT_CONFIG_SUBDIR),
        ("QONTINUI_HOME", INSTANCE_ROOT_HOME_SUBDIR),
        ("QONTINUI_EMBEDDED_PG_DIR", INSTANCE_ROOT_EMBEDDED_PG_SUBDIR),
        ("QONTINUI_RUNNER_LOG_DIR", INSTANCE_ROOT_LOGS_SUBDIR),
        ("QONTINUI_PANIC_LOG_DIR", INSTANCE_ROOT_LOGS_SUBDIR),
        ("WEBVIEW2_USER_DATA_FOLDER", INSTANCE_ROOT_WEBVIEW_SUBDIR),
    ];
    let mut out: Vec<InstanceRootDefault> = dirs
        .into_iter()
        .filter(|(key, _)| is_blank(current(key).as_ref()))
        .map(|(key, sub)| InstanceRootDefault {
            key,
            value: root.join(sub).into_os_string(),
            is_dir: true,
        })
        .collect();
    if current("QONTINUI_DISABLE_KEYCHAIN").is_none() {
        out.push(InstanceRootDefault {
            key: "QONTINUI_DISABLE_KEYCHAIN",
            value: OsString::from("1"),
            is_dir: false,
        });
    }
    out
}

/// Why a process refused to start under its instance root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRootRefusal {
    pub root: PathBuf,
    pub reason: String,
    /// The root itself passed the root-level checks, so a refusal log may be
    /// written into it.
    pub root_usable: bool,
}

/// Validate this process's launch environment against its instance root and
/// export the root-derived defaults. `Ok(None)` without a root (nothing read
/// beyond the root variable, nothing written); `Ok(Some(root))` once every
/// default is exported and its directory created.
///
/// Mutates the process environment, so it must run while the process is still
/// effectively single-threaded — first thing in `main`, before anything else
/// reads a path. In a `#[tokio::main]` binary the runtime's workers already
/// exist but have run nothing, so none of them can be mid-read of the env.
pub fn apply_instance_root_env(require_port: bool) -> Result<Option<PathBuf>, InstanceRootRefusal> {
    let Some(root) = instance_root() else {
        return Ok(None);
    };
    let machine_dirs = machine_global_dirs();
    let refusal = |reason: String| InstanceRootRefusal {
        root_usable: root_problem(&root, &machine_dirs).is_none(),
        root: root.clone(),
        reason,
    };
    let overrides: Vec<(&str, Option<OsString>)> = ROOT_CONFINED_ENV_KEYS
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    let raw_port = std::env::var("QONTINUI_PORT").ok();
    let port = if require_port {
        PortCheck::Required(raw_port.as_deref())
    } else {
        PortCheck::NotApplicable
    };
    validate_instance_root(&root, port, &overrides, &machine_dirs).map_err(&refusal)?;
    for default in instance_root_defaults(&root, &|key| std::env::var_os(key)) {
        if default.is_dir {
            std::fs::create_dir_all(&default.value).map_err(|e| {
                refusal(format!(
                    "cannot create {} at {:?}: {e}",
                    default.key, default.value
                ))
            })?;
        }
        std::env::set_var(default.key, &default.value);
    }
    Ok(Some(root))
}

/// The one startup call every binary in this crate makes first: a no-op
/// without an instance root; under one, [`apply_instance_root_env`] or a
/// refusal — printed to stderr, appended to `<root>/startup-refusal.log` when
/// the root itself is usable (a Windows GUI-subsystem runner has no readable
/// stderr), and exit [`INSTANCE_ROOT_REFUSAL_EXIT`].
///
/// `require_port` is true only for the runner bin, which binds `QONTINUI_PORT`.
pub fn enforce_instance_root_or_exit(require_port: bool) {
    if let Err(refusal) = apply_instance_root_env(require_port) {
        let line = format!(
            "{}: refusing to start under QONTINUI_INSTANCE_ROOT={}: {}",
            std::env::args()
                .next()
                .unwrap_or_else(|| "qontinui".to_string()),
            refusal.root.display(),
            refusal.reason
        );
        eprintln!("{line}");
        if refusal.root_usable {
            write_refusal_log(&refusal.root, &line);
        }
        std::process::exit(INSTANCE_ROOT_REFUSAL_EXIT);
    }
}

/// Best-effort append of `line` to `<root>/startup-refusal.log`.
fn write_refusal_log(root: &Path, line: &str) {
    use std::io::Write as _;
    let _ = std::fs::create_dir_all(root);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(INSTANCE_ROOT_REFUSAL_LOG))
    {
        let _ = writeln!(file, "{} {line}", chrono::Utc::now().to_rfc3339());
    }
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

    #[test]
    fn only_a_rootless_secondary_shares_the_primarys_settings() {
        let root = Path::new("/srv/subject-1");
        assert!(!shares_primary_settings_from(None, None), "the primary");
        assert!(
            shares_primary_settings_from(Some("test-1"), None),
            "a named secondary, unchanged"
        );
        assert!(!shares_primary_settings_from(None, Some(root)));
        assert!(!shares_primary_settings_from(Some("test-1"), Some(root)));
    }

    #[test]
    fn the_keychain_gate_is_closed_by_the_switch_or_a_root() {
        let root = Path::new("/srv/subject-1");
        assert!(
            keychain_allowed_from(None, None),
            "the primary keeps its keychain"
        );
        assert!(!keychain_allowed_from(Some(OsString::from("1")), None));
        assert!(
            !keychain_allowed_from(Some(OsString::new()), None),
            "any value disables"
        );
        assert!(!keychain_allowed_from(None, Some(root)));
        assert!(!keychain_allowed_from(
            Some(OsString::from("0")),
            Some(root)
        ));
        assert!(matches!(
            keychain_disabled_error(),
            keyring::Error::NoStorageAccess(_)
        ));
    }

    // -- validate_instance_root -------------------------------------------

    /// An absolute path on whichever OS the test runs on.
    fn abs(rel: &str) -> PathBuf {
        std::env::temp_dir()
            .join("qontinui-instance-root-tests")
            .join(rel)
    }

    fn os(p: &Path) -> Option<OsString> {
        Some(p.as_os_str().to_owned())
    }

    fn no_overrides() -> Vec<(&'static str, Option<OsString>)> {
        ROOT_CONFINED_ENV_KEYS.iter().map(|k| (*k, None)).collect()
    }

    fn with_override(
        key: &'static str,
        value: Option<OsString>,
    ) -> Vec<(&'static str, Option<OsString>)> {
        ROOT_CONFINED_ENV_KEYS
            .iter()
            .map(|k| (*k, if *k == key { value.clone() } else { None }))
            .collect()
    }

    const PORT: PortCheck<'static> = PortCheck::Required(Some("9881"));

    #[test]
    fn validate_accepts_an_absolute_root_with_its_own_port_and_inside_overrides() {
        let root = abs("subject-ok");
        assert_eq!(
            validate_instance_root(&root, PORT, &no_overrides(), &[]),
            Ok(())
        );
        let inside: Vec<(&'static str, Option<OsString>)> = ROOT_CONFINED_ENV_KEYS
            .iter()
            .map(|k| (*k, os(&root.join("deep").join(k.to_ascii_lowercase()))))
            .collect();
        assert_eq!(validate_instance_root(&root, PORT, &inside, &[]), Ok(()));
        // The root itself, `.` components and a trailing separator are inside.
        let dotted = root.join(".").join("config");
        assert_eq!(
            validate_instance_root(
                &root,
                PORT,
                &with_override("QONTINUI_CONFIG_DIR", os(&dotted)),
                &[]
            ),
            Ok(())
        );
        assert_eq!(
            validate_instance_root(&root, PORT, &with_override("QONTINUI_HOME", os(&root)), &[]),
            Ok(())
        );
        // Blank overrides are no overrides.
        assert_eq!(
            validate_instance_root(
                &root,
                PORT,
                &with_override("QONTINUI_HOME", Some(OsString::from("  "))),
                &[]
            ),
            Ok(())
        );
        // A CLI bin needs no port at all.
        assert_eq!(
            validate_instance_root(&root, PortCheck::NotApplicable, &no_overrides(), &[]),
            Ok(())
        );
    }

    #[test]
    fn validate_refuses_a_relative_dotted_or_filesystem_root() {
        let err =
            validate_instance_root(Path::new("subject"), PORT, &no_overrides(), &[]).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
        let dotted = abs("a").join("..").join("subject");
        let err = validate_instance_root(&dotted, PORT, &no_overrides(), &[]).unwrap_err();
        assert!(err.contains("`..`"), "{err}");
        let fs_root: PathBuf = abs("x").ancestors().last().unwrap().to_path_buf();
        let err = validate_instance_root(&fs_root, PORT, &no_overrides(), &[]).unwrap_err();
        assert!(err.contains("filesystem root"), "{err}");
    }

    #[test]
    fn validate_refuses_a_root_that_contains_a_machine_global_dir() {
        let home = abs("home-of-user");
        let machine = vec![home.join(".local").join("share"), home.clone()];
        for root in [
            home.clone(),
            abs(""),
            PathBuf::from(format!("{}/", home.display())),
        ] {
            let err = validate_instance_root(&root, PORT, &no_overrides(), &machine).unwrap_err();
            assert!(err.contains("machine-global"), "{root:?}: {err}");
        }
        // A root BELOW home is fine — that is where subjects live.
        assert_eq!(
            validate_instance_root(
                &home.join("subjects").join("a"),
                PORT,
                &no_overrides(),
                &machine
            ),
            Ok(())
        );
    }

    #[test]
    fn validate_refuses_a_missing_invalid_or_primary_port_exactly_as_the_runtime_parses() {
        let root = abs("subject-port");
        for port in [None, Some("")] {
            let err =
                validate_instance_root(&root, PortCheck::Required(port), &no_overrides(), &[])
                    .unwrap_err();
            assert!(err.contains("QONTINUI_PORT is unset"), "{port:?}: {err}");
        }
        // The runtime's `parse()` rejects surrounding whitespace and falls back
        // to the primary's port, so the contract refuses it — no trimming.
        for port in [" 9881", "9881 ", "  ", "nope", "70000"] {
            let err = validate_instance_root(
                &root,
                PortCheck::Required(Some(port)),
                &no_overrides(),
                &[],
            )
            .unwrap_err();
            assert!(err.contains("not a valid port"), "{port:?}: {err}");
        }
        let err =
            validate_instance_root(&root, PortCheck::Required(Some("0")), &no_overrides(), &[])
                .unwrap_err();
        assert!(err.contains("not a fixed port"), "{err}");
        let primary = crate::runner_breadcrumb::PRIMARY_PORT.to_string();
        let err = validate_instance_root(
            &root,
            PortCheck::Required(Some(&primary)),
            &no_overrides(),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("primary runner's port"), "{err}");
    }

    #[test]
    fn validate_refuses_every_confined_override_outside_the_root() {
        let root = abs("subject-confined");
        let outside = abs("harness-state");
        for key in ROOT_CONFINED_ENV_KEYS {
            let err = validate_instance_root(&root, PORT, &with_override(key, os(&outside)), &[])
                .unwrap_err();
            assert!(
                err.contains(key) && err.contains("outside the instance root"),
                "{err}"
            );
        }
        // `..` is refused even when it would resolve back inside; a sibling
        // that merely shares a prefix and a relative override are outside.
        let refused = [
            root.join("x").join("..").join("config"),
            root.join("..").join("harness"),
            PathBuf::from(format!("{}-sibling", root.display())),
            PathBuf::from("relative/secure"),
        ];
        for path in refused {
            let got = validate_instance_root(
                &root,
                PORT,
                &with_override("QONTINUI_SECURE_STORAGE_DIR", os(&path)),
                &[],
            );
            assert!(got.is_err(), "{path:?} must be refused");
        }
    }

    #[test]
    fn validate_reports_every_problem_at_once() {
        let root = abs("subject-many");
        let mut overrides = with_override("QONTINUI_CONFIG_DIR", os(&abs("elsewhere")));
        overrides.push(("QONTINUI_HOME", os(&abs("elsewhere-home"))));
        let err =
            validate_instance_root(&root, PortCheck::Required(None), &overrides, &[]).unwrap_err();
        assert!(err.contains("QONTINUI_PORT"), "{err}");
        assert!(err.contains("QONTINUI_CONFIG_DIR"), "{err}");
        assert!(err.contains("QONTINUI_HOME"), "{err}");
    }

    // -- defaults ---------------------------------------------------------

    #[test]
    fn defaults_fill_every_unset_key_under_the_root() {
        let root = abs("subject-defaults");
        let got = instance_root_defaults(&root, &|_| None);
        let pairs: Vec<(&str, OsString, bool)> = got
            .iter()
            .map(|d| (d.key, d.value.clone(), d.is_dir))
            .collect();
        let dir = |sub: &str| root.join(sub).into_os_string();
        assert_eq!(
            pairs,
            vec![
                ("QONTINUI_SECURE_STORAGE_DIR", dir("secure"), true),
                ("QONTINUI_CONFIG_DIR", dir("config"), true),
                ("QONTINUI_HOME", dir("home"), true),
                ("QONTINUI_EMBEDDED_PG_DIR", dir("embedded-pg"), true),
                ("QONTINUI_RUNNER_LOG_DIR", dir("logs"), true),
                ("QONTINUI_PANIC_LOG_DIR", dir("logs"), true),
                ("WEBVIEW2_USER_DATA_FOLDER", dir("webview"), true),
                ("QONTINUI_DISABLE_KEYCHAIN", OsString::from("1"), false),
            ]
        );
        // Every default is itself inside the root, so the runner's own choices
        // could never trip its own containment check.
        let applied: Vec<(&str, Option<OsString>)> = got
            .iter()
            .filter(|d| d.is_dir)
            .map(|d| (d.key, Some(d.value.clone())))
            .collect();
        assert_eq!(validate_instance_root(&root, PORT, &applied, &[]), Ok(()));
    }

    #[test]
    fn defaults_never_override_what_the_launcher_set() {
        let root = abs("subject-launcher-set");
        let set = root.join("my-secure").into_os_string();
        let got = instance_root_defaults(&root, &|key| match key {
            "QONTINUI_SECURE_STORAGE_DIR" => Some(set.clone()),
            "QONTINUI_HOME" => Some(OsString::from("")), // blank = unset
            "QONTINUI_DISABLE_KEYCHAIN" => Some(OsString::new()), // any value = disabled
            _ => None,
        });
        let keys: Vec<&str> = got.iter().map(|d| d.key).collect();
        assert!(!keys.contains(&"QONTINUI_SECURE_STORAGE_DIR"));
        assert!(!keys.contains(&"QONTINUI_DISABLE_KEYCHAIN"));
        assert!(keys.contains(&"QONTINUI_HOME"));
    }

    #[test]
    fn the_rooted_instance_id_ignores_trailing_separators_and_dots() {
        let root = abs("subject-id");
        let id = rooted_instance_id(&root);
        assert!(
            id.starts_with("root-") && id.len() == "root-".len() + 16,
            "{id}"
        );
        assert_eq!(
            rooted_instance_id(&PathBuf::from(format!("{}/", root.display()))),
            id
        );
        assert_eq!(rooted_instance_id(&root.join(".")), id);
        assert_ne!(rooted_instance_id(&abs("subject-other")), id);
        // Pinned: an on-disk name must not move between releases.
        assert_eq!(rooted_instance_id(Path::new("a")), "root-af63dc4c8601ec8c");
    }

    // -- through the real entry points (process env, isolated fixture) ----

    /// The fixture points these at its own dir — OUTSIDE a subject root — so a
    /// subject test clears them and lets the root defaults fill them.
    const FIXTURE_DIR_KEYS: [&str; 3] = [
        "QONTINUI_HOME",
        "QONTINUI_CONFIG_DIR",
        "QONTINUI_SECURE_STORAGE_DIR",
    ];

    /// Enter a subject launch: fixture env, a root at `<fixture>/subject`, the
    /// fixture's own dir keys cleared, and `WEBVIEW2_USER_DATA_FOLDER` (not an
    /// ambient key) captured for restore.
    fn subject_env(
        amb: &crate::test_env::IsolatedAmbient,
    ) -> (PathBuf, crate::test_env::EnvVarRestore) {
        let restore = crate::test_env::EnvVarRestore::capture(&["WEBVIEW2_USER_DATA_FOLDER"]);
        std::env::remove_var("WEBVIEW2_USER_DATA_FOLDER");
        for key in FIXTURE_DIR_KEYS {
            std::env::remove_var(key);
        }
        let root = amb.dir().join("subject");
        std::env::set_var("QONTINUI_INSTANCE_ROOT", &root);
        (root, restore)
    }

    #[test]
    fn apply_exports_every_default_and_creates_the_dirs() {
        let amb = crate::test_env::isolated_ambient();
        let (root, _restore) = subject_env(&amb);
        std::env::set_var("QONTINUI_PORT", "9881");
        std::env::remove_var("QONTINUI_DISABLE_KEYCHAIN");

        assert_eq!(apply_instance_root_env(true), Ok(Some(root.clone())));

        for default in instance_root_defaults(&root, &|_| None) {
            assert_eq!(
                std::env::var_os(default.key),
                Some(default.value.clone()),
                "{} must be exported",
                default.key
            );
            if default.is_dir {
                assert!(
                    Path::new(&default.value).is_dir(),
                    "{} must exist",
                    default.key
                );
            }
        }
        // The rest of the process now reads the subject's own locations.
        assert_eq!(crate::ambient::qontinui_dir(), Some(root.join("home")));
        assert!(!keychain_allowed());
        assert!(is_secondary());
        assert!(!shares_primary_settings());
    }

    #[test]
    fn apply_refuses_the_primary_port_and_writes_nothing() {
        let amb = crate::test_env::isolated_ambient();
        let (root, _restore) = subject_env(&amb);
        std::env::set_var(
            "QONTINUI_PORT",
            crate::runner_breadcrumb::PRIMARY_PORT.to_string(),
        );

        let refusal = apply_instance_root_env(true).unwrap_err();
        assert!(
            refusal.reason.contains("primary runner's port"),
            "{refusal:?}"
        );
        assert!(refusal.root_usable);
        assert!(
            !root.join("secure").exists(),
            "a refused launch creates nothing"
        );
        assert_eq!(std::env::var_os("QONTINUI_SECURE_STORAGE_DIR"), None);

        // A CLI bin under the same root does not care about the port.
        assert_eq!(apply_instance_root_env(false), Ok(Some(root)));
    }

    #[test]
    fn without_a_root_apply_is_a_no_op_and_the_primary_is_unchanged() {
        let _amb = crate::test_env::isolated_ambient();
        let before: Vec<Option<OsString>> = ROOT_CONFINED_ENV_KEYS
            .iter()
            .map(std::env::var_os)
            .collect();
        assert_eq!(apply_instance_root_env(true), Ok(None));
        let after: Vec<Option<OsString>> = ROOT_CONFINED_ENV_KEYS
            .iter()
            .map(std::env::var_os)
            .collect();
        assert_eq!(before, after);
        assert!(!is_secondary());
        assert!(!shares_primary_settings());
        std::env::remove_var("QONTINUI_DISABLE_KEYCHAIN");
        assert!(keychain_allowed());
    }
}
