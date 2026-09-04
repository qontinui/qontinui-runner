//! The runner's ONE door to ambient machine state: the per-user `~/.qontinui`
//! directory, the `machine.json` inside it, and the process-environment keys
//! that change what the runner resolves.
//!
//! Plan `2026-09-03-runner-tests-read-ambient-machine-state`. Before this
//! module, 27 sites built `dirs::home_dir()/.qontinui/machine.json` by hand
//! (five of them with their own serde struct), and a test that reached one of
//! them measured the developer's box: on a configured operator machine
//! `session::tests::tenant_scope_of_…` failed deterministically because the
//! REAL `machine.json` carried an `active_tenant_id`, while CI's clean runner
//! passed. There is no `env -u` for a file, which is why the fix is a seam and
//! not another note.
//!
//! Three things live here, and nothing else:
//!
//! 1. **Resolvers** — [`qontinui_dir`] (honours `QONTINUI_HOME`, else
//!    `<home>/.qontinui`) as a thin wrapper over the PURE
//!    [`qontinui_dir_from`], plus [`machine_json_path`], [`runner_dir`] and
//!    the `_or_cwd` variants that keep the historical `"."` fallback.
//! 2. **One `machine.json` reader** — [`MachineJson`] / [`read_machine_json`],
//!    alias-aware for the legacy `machine_id` spelling and tolerant of a file
//!    that carries BOTH spellings (a serde `alias` errors on that as a
//!    duplicate field; `pair::ensure_device_id_persisted` writes exactly that
//!    shape).
//! 3. **The declared env surface** — [`AMBIENT_ENV_KEYS`], the generalisation
//!    of `profiles::COORD_BASE_ENV_KEYS` to every `QONTINUI_*` / `COORD_*`
//!    key the crate reads, kept honest by a source-scan test below.
//!
//! `QONTINUI_HOME` is a TEST/OVERRIDE knob. `machine_identity` used to say
//! "deliberately no env override, so a supervisor-spawned temp runner shares
//! the primary's identity" — that property still holds, because the
//! supervisor never sets `QONTINUI_HOME`; only a process that explicitly asks
//! for a different `.qontinui` gets one.
//!
//! In debug builds (and under the `test-fixtures` feature) the module also
//! carries [`test_support`]: the [`test_support::IsolatedAmbient`] fixture that
//! points every resolver at a per-test temp dir, and the canary that panics —
//! naming the source — when a test reads the seam with no fixture live. Both
//! are gated exactly like `mcp::test_fixtures` and are absent from release
//! builds; the canary is dormant until a test binary arms it at start.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

/// Env var that overrides the `~/.qontinui` directory outright.
pub const QONTINUI_HOME_ENV: &str = "QONTINUI_HOME";

/// File name of the per-device identity file inside [`qontinui_dir`].
pub const MACHINE_JSON_FILE: &str = "machine.json";

/// The `.qontinui` directory for the current user.
///
/// `QONTINUI_HOME` (set, non-blank) wins; else `dirs::home_dir()/.qontinui`;
/// `None` only when neither resolves. This is the ONLY function in the crate
/// allowed to spell `".qontinui"` next to a home directory — a source-scan
/// test enforces it.
pub fn qontinui_dir() -> Option<PathBuf> {
    let override_dir = std::env::var_os(QONTINUI_HOME_ENV);
    canary(if override_dir.is_some() {
        "~/.qontinui (QONTINUI_HOME set)"
    } else {
        "~/.qontinui (QONTINUI_HOME unset)"
    });
    qontinui_dir_from(override_dir, dirs::home_dir())
}

/// Pure core of [`qontinui_dir`]: precedence over injected inputs, so the rule
/// is unit-testable with no `set_var` at all.
pub fn qontinui_dir_from(override_dir: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        if !dir.to_string_lossy().trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    home.map(|h| h.join(".qontinui"))
}

/// [`qontinui_dir`], falling back to `./.qontinui` when no home resolves.
///
/// This preserves the historical behaviour of the call sites that used
/// `dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".qontinui")`;
/// prefer [`qontinui_dir`] and handling `None` in new code.
pub fn qontinui_dir_or_cwd() -> PathBuf {
    qontinui_dir().unwrap_or_else(|| Path::new(".").join(".qontinui"))
}

/// `<qontinui_dir>/runner` — the runner's established app-data dir (session
/// outbox, `session-restore/`, the port breadcrumb, spill store …).
pub fn runner_dir() -> Option<PathBuf> {
    qontinui_dir().map(|d| d.join("runner"))
}

/// [`runner_dir`] with the `./.qontinui/runner` fallback of [`qontinui_dir_or_cwd`].
pub fn runner_dir_or_cwd() -> PathBuf {
    qontinui_dir_or_cwd().join("runner")
}

/// `<qontinui_dir>/machine.json` — the per-device identity file.
pub fn machine_json_path() -> Option<PathBuf> {
    qontinui_dir().map(|d| d.join(MACHINE_JSON_FILE))
}

// ---------------------------------------------------------------------------
// machine.json
// ---------------------------------------------------------------------------

/// The on-disk shape of `machine.json`, as every reader in the runner needs it.
///
/// Fields are `Option` because the file grows additively and every reader
/// tolerates absence: a pre-rename file spells the identity `machine_id`, a
/// hand-repaired one may carry only `device_id`, and `active_tenant_id` is
/// written only once an operator pins a tenant. Writers (`pair`,
/// `commands::tenant`, `qontinui_profile device init`) round-trip the raw JSON
/// object instead so unknown sibling fields survive; this type is the READ
/// shape, plus [`MachineJson::to_value`] for test fixtures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineJson {
    /// `device_id`, or the legacy `machine_id` when only that is present.
    /// Trimmed; `None` when absent, blank, or not a string.
    pub device_id: Option<String>,
    /// `hostname`, trimmed; `None` when absent or blank.
    pub hostname: Option<String>,
    /// Operator-chosen display name.
    pub name: Option<String>,
    /// `active_tenant_id` exactly as written — kept RAW because
    /// `tenant_pin` must tell an absent/`null` field (no tenant stated) from a
    /// malformed one (a stated tenant that cannot be honoured), and a typed
    /// `Option<String>` would collapse the second into the first.
    pub active_tenant_id: Option<Value>,
}

impl MachineJson {
    /// Parse the bytes of a `machine.json`.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, MachineJsonParseError> {
        let value: Value = serde_json::from_slice(bytes).map_err(MachineJsonParseError::Json)?;
        Self::from_value(&value).ok_or(MachineJsonParseError::NotAnObject)
    }

    /// Project a parsed JSON value. `None` when it is not an object.
    pub fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let str_field = |key: &str| -> Option<String> {
            obj.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Some(Self {
            device_id: str_field("device_id").or_else(|| str_field("machine_id")),
            hostname: str_field("hostname"),
            name: str_field("name"),
            active_tenant_id: obj.get("active_tenant_id").cloned(),
        })
    }

    /// The device identity as a UUID, when present and well-formed.
    pub fn device_uuid(&self) -> Option<Uuid> {
        Uuid::parse_str(self.device_id.as_deref()?).ok()
    }

    /// `active_tenant_id` when it is a non-blank string, trimmed.
    pub fn active_tenant_id_str(&self) -> Option<&str> {
        self.active_tenant_id
            .as_ref()
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// `active_tenant_id` as a UUID, when present and well-formed.
    pub fn active_tenant_uuid(&self) -> Option<Uuid> {
        Uuid::parse_str(self.active_tenant_id_str()?).ok()
    }

    /// Serialise to the on-disk object shape (canonical `device_id` key; only
    /// the fields that are `Some`). Used by the test fixture's writer.
    pub fn to_value(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(d) = &self.device_id {
            obj.insert("device_id".into(), Value::String(d.clone()));
        }
        if let Some(h) = &self.hostname {
            obj.insert("hostname".into(), Value::String(h.clone()));
        }
        if let Some(n) = &self.name {
            obj.insert("name".into(), Value::String(n.clone()));
        }
        if let Some(t) = &self.active_tenant_id {
            obj.insert("active_tenant_id".into(), t.clone());
        }
        Value::Object(obj)
    }
}

/// Why `machine.json` bytes did not yield a [`MachineJson`].
#[derive(Debug)]
pub enum MachineJsonParseError {
    /// Not valid JSON.
    Json(serde_json::Error),
    /// Valid JSON, but not an object.
    NotAnObject,
}

impl fmt::Display for MachineJsonParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "not valid JSON: {e}"),
            Self::NotAnObject => f.write_str("valid JSON but not an object"),
        }
    }
}

impl std::error::Error for MachineJsonParseError {}

/// Why [`read_machine_json`] could not produce a [`MachineJson`].
#[derive(Debug)]
pub enum MachineJsonError {
    /// Neither `QONTINUI_HOME` nor a home directory resolved.
    NoHomeDir,
    /// The file could not be read — including the ordinary "does not exist"
    /// case; see [`MachineJsonError::is_missing`].
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file was read but did not parse as a JSON object.
    Parse {
        path: PathBuf,
        source: MachineJsonParseError,
    },
}

impl MachineJsonError {
    /// `true` when the file simply is not there (the un-initialised device
    /// case), as opposed to unreadable or malformed.
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }

    /// The path that was read, when one resolved.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::NoHomeDir => None,
            Self::Read { path, .. } | Self::Parse { path, .. } => Some(path),
        }
    }
}

impl fmt::Display for MachineJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHomeDir => {
                f.write_str("could not resolve home directory (no QONTINUI_HOME, no home)")
            }
            Self::Read { path, source } => write!(f, "read {}: {source}", path.display()),
            Self::Parse { path, source } => write!(f, "parse {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for MachineJsonError {}

/// Read and parse the `machine.json` at an explicit path. Never creates it.
pub fn read_machine_json_at(path: &Path) -> Result<MachineJson, MachineJsonError> {
    let bytes = std::fs::read(path).map_err(|source| MachineJsonError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    MachineJson::from_slice(&bytes).map_err(|source| MachineJsonError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Read and parse this machine's `machine.json` ([`machine_json_path`]).
pub fn read_machine_json() -> Result<MachineJson, MachineJsonError> {
    canary("~/.qontinui/machine.json");
    let path = machine_json_path().ok_or(MachineJsonError::NoHomeDir)?;
    read_machine_json_at(&path)
}

// ---------------------------------------------------------------------------
// The declared env surface
// ---------------------------------------------------------------------------

/// Every process-environment key that can change what this crate resolves —
/// `profiles::COORD_BASE_ENV_KEYS` generalised to the whole crate.
///
/// Membership is measured, not remembered: the source-scan test
/// `every_literal_qontinui_or_coord_env_read_is_declared` fails when a
/// `std::env::var("QONTINUI_…")` / `("COORD_…")` literal appears in
/// `src-tauri/src` that is not in this list, and the fixture captures and
/// restores exactly this list around every test. Keys read through a `const`
/// (`env::var(SOME_ENV)`) are listed too, by hand; the scan cannot see them.
/// Sorted, unique — a test pins both so an addition has one obvious place.
pub const AMBIENT_ENV_KEYS: &[&str] = &[
    "COORD_ADMIN_SECRET",
    "COORD_BUDGET_REPUBLISH_SECS",
    "COORD_CLAUDE_PROBE_TTL_SECS",
    "COORD_HEARTBEAT_INTERVAL_SECS",
    "COORD_HTTP_URL",
    "COORD_LOW_DISK_CRIT_BYTES",
    "COORD_LOW_DISK_WARN_BYTES",
    "COORD_MCP_PERSIST_NONCES",
    "COORD_ORPHAN_TARGET_GRACE_SECS",
    "COORD_ORPHAN_TARGET_KEEP",
    "COORD_ORPHAN_TARGET_REAP_ENABLED",
    "COORD_PULL_EXECUTOR_ENABLED",
    "COORD_RESOURCE_SAMPLE_SECS",
    "COORD_SESSION_ATTRIBUTION_ENABLED",
    "COORD_SESSION_ATTRIBUTION_INTERVAL_SECS",
    "COORD_SESSION_BUS_ENABLED",
    "COORD_SESSION_BUS_INTERVAL_SECS",
    "COORD_TREE_FETCH_INTERVAL_SECS",
    "COORD_TREE_PUBLISH_INTERVAL_SECS",
    "COORD_URL",
    "COORD_WORKTREE_ROOT",
    "DATABASE_URL",
    "HOME",
    "QONTINUI_ACCOUNT_USAGE_REPORT_DISABLED",
    "QONTINUI_AGENT_GIT_EMAIL",
    "QONTINUI_AGENT_GIT_NAME",
    "QONTINUI_AGENT_JWT_EXP_COMPRESS_SECS",
    "QONTINUI_AGENT_LOGS_FROM_SESSIONS",
    "QONTINUI_AGENT_RUNTIME_E2E",
    "QONTINUI_AGENT_WORKTREE_MODE",
    "QONTINUI_ALLOW_NO_DB",
    "QONTINUI_API_URL",
    "QONTINUI_AUTO_RESPONSE_FETCH_INTERVAL_SECS",
    "QONTINUI_AUTO_RESPONSE_SCORE_TIMEOUT_SECS",
    "QONTINUI_BENCH_REPOS",
    "QONTINUI_BROWSE_DIRS",
    "QONTINUI_CENSUS_CHUNK_ROWS",
    "QONTINUI_CENSUS_CHUNK_SECS",
    "QONTINUI_CENSUS_POST_TIMEOUT_SECS",
    "QONTINUI_CLAUDE_BIN",
    "QONTINUI_CLAUDE_HOOK_SETTINGS",
    "QONTINUI_CODE_GRAPH_ROOTS",
    "QONTINUI_CODE_SEM_HELPER",
    "QONTINUI_COMMIT_FORWARDER_ENABLED",
    "QONTINUI_COMMIT_LINEAGE_GIT_TIMEOUT_SECS",
    "QONTINUI_COMMIT_LINEAGE_REPORT",
    "QONTINUI_COMPRESSION_THRESHOLD",
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_CONTEXT_HANDOFF",
    "QONTINUI_CONTEXT_HANDOFF_THRESHOLD_PCT",
    "QONTINUI_CONTINUATION_SESSION_CAP",
    "QONTINUI_COORDINATOR_AUTO_REVIEW_ENABLED",
    "QONTINUI_COORDINATOR_INITIAL_DELAY_SECS",
    "QONTINUI_COORDINATOR_INTERVAL_SECS",
    "QONTINUI_COORDINATOR_MAX_LLM_CALLS_PER_HOUR",
    "QONTINUI_COORDINATOR_RUST_SCHEDULER",
    "QONTINUI_COORDINATOR_SHADOW",
    "QONTINUI_CRASH_DUMP_FRESHNESS_SECS",
    "QONTINUI_DEVENV_AUTO_ENROLL",
    "QONTINUI_DEV_BOOTSTRAP",
    "QONTINUI_DEV_ENDPOINTS",
    "QONTINUI_DIRTY_POLL_INTERVAL_SECS",
    "QONTINUI_DISABLE_GIT_SUPERVISION",
    "QONTINUI_DISABLE_KEYCHAIN",
    "QONTINUI_DISK_SURVEY_INTERVAL_SECS",
    "QONTINUI_DISTINCT_MARKER_AAAAA",
    "QONTINUI_DISTINCT_MARKER_BBBBB",
    "QONTINUI_DRAIN_TIMEOUT_MS",
    "QONTINUI_E2E_ARGS_OUT",
    "QONTINUI_EDIT_EFFECT_LOOP_ENABLED",
    "QONTINUI_EMBEDDED_PG_DIR",
    "QONTINUI_ENV",
    "QONTINUI_ENVELOPE_AUDIT_PANIC",
    "QONTINUI_ENV_CAPTURE_INTERVAL_SECS",
    "QONTINUI_EXTERNAL_VOLUME_GUID",
    "QONTINUI_EXTERNAL_VOLUME_PATH",
    "QONTINUI_FS_BACKSTOP_ENABLED",
    "QONTINUI_FS_BACKSTOP_INTERVAL_SECS",
    "QONTINUI_FS_OBSERVER_ALLOWLIST",
    "QONTINUI_FS_OBSERVER_ENABLED",
    "QONTINUI_GIT_CRED_DEBUG",
    "QONTINUI_HEADLESS_ONLY",
    "QONTINUI_HOME",
    "QONTINUI_INSTALL_FX_LIVE",
    "QONTINUI_INSTALL_INTERCEPT_ENABLED",
    "QONTINUI_INSTALL_INTERCEPT_GUARD",
    "QONTINUI_INSTALL_INTERCEPT_MODE",
    "QONTINUI_INSTALL_INTERCEPT_PORT",
    "QONTINUI_INSTALL_INTERCEPT_SHIM_DIR",
    "QONTINUI_INSTALL_OVERRIDE",
    "QONTINUI_INSTANCE_NAME",
    "QONTINUI_LOOPING_AGENT_TICK_MS",
    "QONTINUI_MACHINE_ID",
    "QONTINUI_MAINTENANCE_INTERVAL_SECS",
    "QONTINUI_MCP_CONFIG",
    "QONTINUI_MCP_SPILL_MAX_BYTES",
    "QONTINUI_MCP_SPILL_THRESHOLD_BYTES",
    "QONTINUI_NODE_PATH",
    "QONTINUI_ORPHAN_TARGET_INTERVAL_SECS",
    "QONTINUI_PANIC_LOG_DIR",
    "QONTINUI_PINNED_SESSION_ID",
    "QONTINUI_PLANS_ARCHIVE_DIR",
    "QONTINUI_PLANS_DIR",
    "QONTINUI_PLAN_ADAPTER_DIR",
    "QONTINUI_PLAN_ADAPTER_INTERVAL_SECS",
    "QONTINUI_PLAN_LIBRARY_SYNC",
    "QONTINUI_PLAN_LIBRARY_WRITE",
    "QONTINUI_POLICY_INJECTION",
    "QONTINUI_PORT",
    "QONTINUI_PRIMARY_PORT",
    "QONTINUI_PRM_URL",
    "QONTINUI_PROCESS_LOG_RETENTION_DAYS",
    "QONTINUI_PROJECT_ROOT",
    "QONTINUI_PROMPTS_DIR",
    "QONTINUI_PROMPT_AUDIT_REPORT_DISABLED",
    "QONTINUI_PROVISIONING_GATE_ENFORCE",
    "QONTINUI_PUSHER_INTERVAL_SECS",
    "QONTINUI_PUSHER_JITTER_SECS",
    "QONTINUI_PUSHER_PUSH_TIMEOUT_SECS",
    "QONTINUI_PYTHON_PATH",
    "QONTINUI_REAP_RESTART",
    "QONTINUI_RESTATE_ADMIN_PORT",
    "QONTINUI_RESTATE_EXTERNAL_ADMIN_URL",
    "QONTINUI_RESTATE_EXTERNAL_INGRESS_URL",
    "QONTINUI_RESTATE_INGRESS_PORT",
    "QONTINUI_RESTATE_SERVICE_PORT",
    "QONTINUI_ROOT",
    "QONTINUI_RUNNER_API_PORT",
    "QONTINUI_RUNNER_API_URL",
    "QONTINUI_RUNNER_ID",
    "QONTINUI_RUNNER_LOG_DIR",
    "QONTINUI_RUNNER_PRIMARY_URL",
    "QONTINUI_RUNNER_ROLE",
    "QONTINUI_RUNNER_TIER",
    "QONTINUI_RUNNER_TOKEN",
    "QONTINUI_SCRIPTED_OUTPUT",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_SERVER_MODE",
    "QONTINUI_SESSION_AUTOMATION_REGISTER",
    "QONTINUI_SESSION_NAMES_DIR",
    "QONTINUI_SESSION_WORKTREES",
    "QONTINUI_SHIM_E2E_EXE",
    "QONTINUI_SPAWN_AUTHZ_DISABLED",
    "QONTINUI_SPAWN_AUTHZ_POLICY_REQUIRED_FLOOR",
    "QONTINUI_SPAWN_OUTCOME_ENABLED",
    "QONTINUI_SPECS_ROOT",
    "QONTINUI_SPEC_COVERAGE_FLOOR",
    "QONTINUI_SSM_REGION",
    "QONTINUI_STATE_DERIVE_INITIAL_DELAY_SECS",
    "QONTINUI_STATE_DERIVE_INTERVAL_SECS",
    "QONTINUI_STATE_DERIVE_WINDOW_DAYS",
    "QONTINUI_STOP_HOOK_CONTINUATION",
    "QONTINUI_STOP_HOOK_CONTINUATION_CAP",
    "QONTINUI_STOP_HOOK_STATUS_OVERRIDE",
    "QONTINUI_SUPERVISOR_PORT",
    "QONTINUI_SUPERVISOR_URL",
    "QONTINUI_TERMINAL_ID",
    "QONTINUI_TERMINAL_SANITIZE",
    "QONTINUI_TEST_AUTO_LOGIN_EMAIL",
    "QONTINUI_UI_BRIDGE_LLM_RECOVERY",
    "QONTINUI_UI_BRIDGE_MULTI_WINDOW",
    "QONTINUI_UI_BRIDGE_VIEW_CONTROL",
    "QONTINUI_VISION_FORCE_CAPTURE_FAIL",
    "QONTINUI_VISION_OCR_ENDPOINT",
    "QONTINUI_VISION_OCR_MODEL",
    "QONTINUI_VISION_RAW",
    "QONTINUI_VISION_VLM_ENDPOINT",
    "QONTINUI_VISION_VLM_MODEL",
    "QONTINUI_VOLUME_SAMPLE_INTERVAL_SECS",
    "QONTINUI_WEB_BACKEND_URL",
    "QONTINUI_WEB_BASE",
    "QONTINUI_WINDOW_DECORATIONS",
    "QONTINUI_WORKSPACE_ROOT",
    "QONTINUI_WORKTREE_BACKSTOP_MAX_AGE_SECS",
    "QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS",
    "QONTINUI_WORKTREE_EMPTY_SESSION_MIN_AGE_SECS",
    "QONTINUI_WORKTREE_PRUNE_INTERVAL_SECS",
    "QONTINUI_WORKTREE_RECLAIM_ACTIVITY_WINDOW_SECS",
    "QONTINUI_WORKTREE_RECLAIM_INTERVAL_SECS",
    "QONTINUI_WORKTREE_ROOT",
    "QONTINUI_WORLD_STATE_VERIFIER",
    "QONTINUI_WORLD_STATE_VERIFIER_ENDPOINT",
    "QONTINUI_WORLD_STATE_VERIFIER_MODEL",
    "QONTINUI_WSL_DISTRO",
    "USERPROFILE",
];

// ---------------------------------------------------------------------------
// The canary
// ---------------------------------------------------------------------------

/// Record that `source` — one concrete piece of ambient machine state — is
/// about to be read.
///
/// In release builds this is a no-op. In debug/test builds it panics, naming
/// `source`, when a test binary has armed it ([`test_support::arm`]) and the
/// read happens with no [`test_support::IsolatedAmbient`] guard live (D2's
/// three-case rule lives in [`test_support`]). Call sites are the seam's own
/// resolvers plus the two resolvers outside it that read ambient state
/// directly: `agent_worktree::canonical_paths::default_canonical_path`
/// (`$QONTINUI_ROOT`) and `profiles::connected_coord_base` (`$COORD_HTTP_URL`
/// and the profile/tier files).
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub fn canary(source: &str) {
    test_support::check(source);
}

/// Release-build twin of [`canary`]: nothing.
#[cfg(not(any(debug_assertions, feature = "test-fixtures")))]
#[inline(always)]
pub fn canary(_source: &str) {}

/// Test fixture and canary state. Gated exactly like `mcp::test_fixtures`
/// (`mcp/mod.rs`), which CI asserts stays out of release builds; a
/// `#[cfg(test)]` item in the lib would be invisible to the runner-bin test
/// binary, where the lib is an ordinary dependency (plan D3).
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub mod test_support {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    use super::{machine_json_path, MachineJson, AMBIENT_ENV_KEYS};

    /// Stem of the plan that introduced the seam — named in the canary's panic
    /// so a session that has never heard of this class can find the write-up.
    pub const PLAN_STEM: &str = "2026-09-03-runner-tests-read-ambient-machine-state";

    /// A profile name no real `profiles.json` can carry, so the profile arm of
    /// `profiles::resolve_coord_base()` misses deterministically everywhere.
    pub const NO_SUCH_PROFILE: &str = "__qontinui_test_no_such_profile__";

    /// Keys the fixture points at its temp dir.
    pub const KEYS_SET_TO_DIR: &[&str] = &[
        "QONTINUI_HOME",
        "QONTINUI_CONFIG_DIR",
        "QONTINUI_SECURE_STORAGE_DIR",
        "HOME",
        "USERPROFILE",
    ];

    /// Keys the fixture removes, so the process looks unconfigured.
    pub const KEYS_REMOVED: &[&str] = &[
        "COORD_HTTP_URL",
        "QONTINUI_WORKSPACE_ROOT",
        "QONTINUI_SERVER_MODE",
        "QONTINUI_RUNNER_TOKEN",
        "QONTINUI_RUNNER_TIER",
        "DATABASE_URL",
    ];

    /// The one process-wide lock every env-touching test serialises on.
    /// `std::env` is process-global; two tests mutating it in parallel clobber
    /// each other mid-read. The 08-25 plan found TWO of these (one per crate
    /// root) — this is the fold: the bin's `test_env` re-exports this module.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    /// Set once per test binary by [`arm`]; the canary is inert before that.
    static CANARY_ARMED: AtomicBool = AtomicBool::new(false);
    /// Number of [`IsolatedAmbient`] guards currently live (0 or 1 — they hold
    /// [`ENV_LOCK`]). D2 case 2: a read from ANY thread while a guard is live
    /// sees the guard's temp dir, never the machine.
    static LIVE_GUARDS: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        /// D2 case 1: the guard was constructed on this thread.
        static GUARD_ON_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
        /// [`ENV_LOCK`] is held by this thread — turns a re-entrant
        /// `env_lock()` (a guaranteed deadlock) into a diagnosable panic.
        static LOCK_HELD_BY_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
    }

    /// Arm the canary. Called by `#[ctor]` in each crate root under
    /// `#[cfg(test)]`, so it runs before any test in the binary (D3).
    pub fn arm() {
        CANARY_ARMED.store(true, Ordering::SeqCst);
    }

    /// Whether [`arm`] has run in this process.
    pub fn is_armed() -> bool {
        CANARY_ARMED.load(Ordering::SeqCst)
    }

    /// Number of live [`IsolatedAmbient`] guards.
    pub fn live_guards() -> usize {
        LIVE_GUARDS.load(Ordering::SeqCst)
    }

    /// D2's three-case rule.
    pub(super) fn check(source: &str) {
        if !CANARY_ARMED.load(Ordering::Relaxed) {
            return;
        }
        if GUARD_ON_THIS_THREAD.with(Cell::get) {
            return;
        }
        if LIVE_GUARDS.load(Ordering::SeqCst) > 0 {
            return;
        }
        panic!(
            "ambient read of {source} from a test with no isolated_ambient() guard — \
             see plan {PLAN_STEM}"
        );
    }

    /// The held env lock. Dropping it releases the lock.
    #[derive(Debug)]
    pub struct EnvLock(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl Drop for EnvLock {
        fn drop(&mut self) {
            LOCK_HELD_BY_THIS_THREAD.with(|h| h.set(false));
        }
    }

    /// Acquire the shared env lock. Hold the returned guard for the whole body
    /// of any test that touches `std::env` and does not use
    /// [`isolated_ambient`] (which holds it for you). Poison-recovering, so a
    /// panicking test cannot cascade-fail the rest.
    pub fn env_lock() -> EnvLock {
        assert!(
            !LOCK_HELD_BY_THIS_THREAD.with(Cell::get),
            "env_lock() called while this thread already holds it — an \
             isolated_ambient() guard already owns the lock for this test; \
             drop the explicit env_lock() call (a second acquisition would deadlock)"
        );
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        LOCK_HELD_BY_THIS_THREAD.with(|h| h.set(true));
        EnvLock(guard)
    }

    /// RAII guard that restores the captured env vars to their pre-capture
    /// values on drop (including the panic path).
    pub struct EnvVarRestore {
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarRestore {
        pub fn capture(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
            Self { saved }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// A test's private machine.
    ///
    /// While live: `~/.qontinui` (via `QONTINUI_HOME`), `HOME`/`USERPROFILE`,
    /// the runner's config dir and secure-storage dir all point at one
    /// per-test temp dir; `QONTINUI_ROOT` points at a CREATED `<dir>/root`;
    /// every key in [`KEYS_REMOVED`] is unset; `QONTINUI_ENV` names a profile
    /// that cannot exist; the process-global runtime tier override is cleared.
    /// On drop — panic path included — every key in [`AMBIENT_ENV_KEYS`] and
    /// the tier override are restored, then the lock is released.
    ///
    /// Field order is drop order: env restored, then the temp dir removed,
    /// then the lock released. The explicit `Drop` runs first and retires the
    /// guard from the canary's count BEFORE the machine's env comes back, so
    /// an unguarded reader on another thread meets the canary rather than the
    /// machine.
    pub struct IsolatedAmbient {
        restore: EnvVarRestore,
        dir: tempfile::TempDir,
        lock: Option<EnvLock>,
        prev_tier_override: Option<&'static str>,
    }

    /// Construct an [`IsolatedAmbient`] guard: take the env lock, capture the
    /// declared surface, and point the process at a fresh temp dir.
    pub fn isolated_ambient() -> IsolatedAmbient {
        IsolatedAmbient::build(env_lock())
    }

    impl IsolatedAmbient {
        pub(super) fn build(lock: EnvLock) -> Self {
            let restore = EnvVarRestore::capture(AMBIENT_ENV_KEYS);
            let dir = tempfile::tempdir().expect("isolated_ambient: create temp dir");
            let root = dir.path().join("root");
            std::fs::create_dir_all(&root).expect("isolated_ambient: create <dir>/root");
            for k in KEYS_SET_TO_DIR {
                std::env::set_var(k, dir.path());
            }
            std::env::set_var("QONTINUI_ROOT", &root);
            for k in KEYS_REMOVED {
                std::env::remove_var(k);
            }
            std::env::set_var("QONTINUI_ENV", NO_SUCH_PROFILE);
            let prev_tier_override = crate::profiles::runtime_tier_override();
            crate::profiles::set_runtime_tier_override(None);
            LIVE_GUARDS.fetch_add(1, Ordering::SeqCst);
            GUARD_ON_THIS_THREAD.with(|g| g.set(true));
            Self {
                restore,
                dir,
                lock: Some(lock),
                prev_tier_override,
            }
        }

        /// The isolated home: `QONTINUI_HOME`, `HOME`, `QONTINUI_CONFIG_DIR`
        /// and `QONTINUI_SECURE_STORAGE_DIR` all resolve here.
        pub fn dir(&self) -> &Path {
            self.dir.path()
        }

        /// The created `$QONTINUI_ROOT` (`<dir>/root`).
        pub fn root(&self) -> PathBuf {
            self.dir.path().join("root")
        }

        /// Write `<dir>/settings.json` — the runner tier document
        /// (`QONTINUI_CONFIG_DIR` points here).
        pub fn write_settings_json(&self, body: &str) {
            std::fs::write(self.dir.path().join("settings.json"), body)
                .expect("isolated_ambient: write settings.json");
        }

        /// Write THIS test's `machine.json` where [`machine_json_path`] resolves.
        pub fn write_machine_json(&self, machine: &MachineJson) {
            let path = machine_json_path().expect("isolated_ambient: machine_json_path");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("isolated_ambient: create machine.json dir");
            }
            let body = serde_json::to_vec_pretty(&machine.to_value())
                .expect("isolated_ambient: serialise machine.json");
            std::fs::write(&path, body).expect("isolated_ambient: write machine.json");
        }

        /// Tear the guard down but KEEP the env lock — for a test that must
        /// inspect the restored environment without letting a sibling in.
        pub(super) fn into_lock(mut self) -> EnvLock {
            self.lock.take().expect("into_lock: lock already taken")
        }
    }

    impl Drop for IsolatedAmbient {
        fn drop(&mut self) {
            GUARD_ON_THIS_THREAD.with(|g| g.set(false));
            LIVE_GUARDS.fetch_sub(1, Ordering::SeqCst);
            crate::profiles::set_runtime_tier_override(self.prev_tier_override);
            // `restore`, `dir`, `lock` drop in declaration order after this.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::collections::BTreeSet;

    // -- pure precedence ----------------------------------------------------

    #[test]
    fn override_wins_over_home() {
        let got = qontinui_dir_from(
            Some(OsString::from("/override/dir")),
            Some(PathBuf::from("/home/someone")),
        );
        assert_eq!(got, Some(PathBuf::from("/override/dir")));
    }

    #[test]
    fn blank_override_falls_through_to_home() {
        for blank in ["", "   "] {
            let got = qontinui_dir_from(
                Some(OsString::from(blank)),
                Some(PathBuf::from("/home/someone")),
            );
            assert_eq!(
                got,
                Some(PathBuf::from("/home/someone").join(".qontinui")),
                "override {blank:?}"
            );
        }
    }

    #[test]
    fn nothing_resolves_to_none() {
        assert_eq!(qontinui_dir_from(None, None), None);
        assert_eq!(qontinui_dir_from(Some(OsString::from("")), None), None);
    }

    // -- machine.json -------------------------------------------------------

    #[test]
    fn reads_canonical_and_legacy_identity_spellings() {
        let canonical =
            MachineJson::from_slice(br#"{"device_id":" abc ","hostname":"h"}"#).unwrap();
        assert_eq!(canonical.device_id.as_deref(), Some("abc"));
        assert_eq!(canonical.hostname.as_deref(), Some("h"));

        let legacy = MachineJson::from_slice(br#"{"machine_id":"def"}"#).unwrap();
        assert_eq!(legacy.device_id.as_deref(), Some("def"));

        // Both spellings at once — what `pair::ensure_device_id_persisted`
        // writes. A serde `alias` rejects this as a duplicate field; the seam
        // must not.
        let both = MachineJson::from_slice(br#"{"machine_id":"old","device_id":"new"}"#).unwrap();
        assert_eq!(both.device_id.as_deref(), Some("new"));
    }

    #[test]
    fn blank_or_non_string_identity_is_absent() {
        assert_eq!(
            MachineJson::from_slice(br#"{"device_id":"  "}"#)
                .unwrap()
                .device_id,
            None
        );
        assert_eq!(
            MachineJson::from_slice(br#"{"device_id":7}"#)
                .unwrap()
                .device_id,
            None
        );
        assert_eq!(
            MachineJson::from_slice(b"{}").unwrap(),
            MachineJson::default()
        );
    }

    #[test]
    fn invalid_json_and_non_objects_are_parse_errors() {
        assert!(matches!(
            MachineJson::from_slice(b"{ not json"),
            Err(MachineJsonParseError::Json(_))
        ));
        assert!(matches!(
            MachineJson::from_slice(b"[1,2]"),
            Err(MachineJsonParseError::NotAnObject)
        ));
    }

    #[test]
    fn active_tenant_id_keeps_its_raw_shape() {
        let id = Uuid::new_v4();
        let pinned =
            MachineJson::from_slice(format!(r#"{{"active_tenant_id":" {id} "}}"#).as_bytes())
                .unwrap();
        assert_eq!(pinned.active_tenant_id_str(), Some(id.to_string().as_str()));
        assert_eq!(pinned.active_tenant_uuid(), Some(id));

        let null = MachineJson::from_slice(br#"{"active_tenant_id":null}"#).unwrap();
        assert_eq!(null.active_tenant_id, Some(Value::Null));
        assert_eq!(null.active_tenant_id_str(), None);

        let malformed = MachineJson::from_slice(br#"{"active_tenant_id":"not-a-uuid"}"#).unwrap();
        assert_eq!(malformed.active_tenant_id_str(), Some("not-a-uuid"));
        assert_eq!(malformed.active_tenant_uuid(), None);

        let absent = MachineJson::from_slice(b"{}").unwrap();
        assert_eq!(absent.active_tenant_id, None);
    }

    #[test]
    fn to_value_round_trips_through_from_value() {
        let m = MachineJson {
            device_id: Some(Uuid::new_v4().to_string()),
            hostname: Some("box".into()),
            name: None,
            active_tenant_id: Some(Value::String(Uuid::new_v4().to_string())),
        };
        assert_eq!(MachineJson::from_value(&m.to_value()), Some(m.clone()));
        assert!(
            m.to_value().get("name").is_none(),
            "None fields are not written"
        );
    }

    #[test]
    fn read_at_distinguishes_missing_from_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("machine.json");
        let err = read_machine_json_at(&missing).unwrap_err();
        assert!(err.is_missing(), "{err}");
        assert_eq!(err.path(), Some(missing.as_path()));
        assert!(!missing.exists(), "a read must never create the file");

        std::fs::write(&missing, b"{ nope").unwrap();
        let err = read_machine_json_at(&missing).unwrap_err();
        assert!(!err.is_missing());
        assert!(matches!(err, MachineJsonError::Parse { .. }), "{err}");
    }

    // -- the fixture and the canary ----------------------------------------

    #[test]
    fn guarded_read_sees_the_fixture_home_not_the_machine() {
        let amb = isolated_ambient();
        assert_eq!(qontinui_dir().as_deref(), Some(amb.dir()));
        assert_eq!(runner_dir(), Some(amb.dir().join("runner")));
        assert_eq!(machine_json_path(), Some(amb.dir().join("machine.json")));
        assert!(amb.root().is_dir(), "QONTINUI_ROOT must be a CREATED dir");
        assert_eq!(
            std::env::var_os("QONTINUI_ROOT").as_deref(),
            Some(amb.root().as_os_str())
        );
        assert!(
            read_machine_json().unwrap_err().is_missing(),
            "a fresh fixture has no machine.json"
        );

        let tenant = Uuid::new_v4();
        amb.write_machine_json(&MachineJson {
            device_id: Some(Uuid::new_v4().to_string()),
            active_tenant_id: Some(Value::String(tenant.to_string())),
            ..MachineJson::default()
        });
        assert_eq!(
            read_machine_json().unwrap().active_tenant_uuid(),
            Some(tenant)
        );
    }

    /// The canary itself. Holding the env lock guarantees no guard is live
    /// (every guard holds that lock), so this is deterministically D2 case 3
    /// and cannot be masked by a concurrent guarded test.
    #[test]
    #[should_panic(expected = "ambient read of")]
    fn unguarded_read_of_the_seam_panics_naming_the_source() {
        let _g = env_lock();
        assert!(
            is_armed(),
            "the #[ctor] in lib.rs must have armed the canary"
        );
        assert_eq!(live_guards(), 0);
        let _ = qontinui_dir();
    }

    #[test]
    fn env_lock_is_not_reentrant_and_says_so() {
        let amb = isolated_ambient();
        let err = std::panic::catch_unwind(env_lock).expect_err("second acquisition must panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(msg.contains("already holds it"), "got: {msg}");
        drop(amb);
    }

    /// The generalisation of the bin's old `isolate_coord_env_pins_every_declared_key`:
    /// the fixture must capture EVERY declared key (restore on drop), set the
    /// ones it says it sets, remove the ones it says it removes, and leave the
    /// rest exactly as captured.
    #[test]
    fn isolated_ambient_pins_every_declared_key() {
        const SENTINEL: &str = "__qontinui_ambient_sentinel__";
        let lock = env_lock();
        let _outer = EnvVarRestore::capture(AMBIENT_ENV_KEYS);
        for k in AMBIENT_ENV_KEYS {
            std::env::set_var(k, SENTINEL);
        }
        let prev_tier = crate::profiles::runtime_tier_override();
        crate::profiles::set_runtime_tier_override(Some(crate::profiles::LOCAL_TIER));

        let amb = IsolatedAmbient::build(lock);
        let dir = amb.dir().as_os_str().to_owned();
        for k in AMBIENT_ENV_KEYS {
            let got = std::env::var_os(k);
            if KEYS_SET_TO_DIR.contains(k) {
                assert_eq!(
                    got.as_deref(),
                    Some(dir.as_os_str()),
                    "{k} must point at the fixture dir"
                );
            } else if KEYS_REMOVED.contains(k) {
                assert_eq!(got, None, "{k} must be removed");
            } else if *k == "QONTINUI_ROOT" {
                assert_eq!(got.as_deref(), Some(amb.root().as_os_str()));
            } else if *k == "QONTINUI_ENV" {
                assert_eq!(got.as_deref(), Some(std::ffi::OsStr::new(NO_SUCH_PROFILE)));
            } else {
                assert_eq!(
                    got.as_deref(),
                    Some(std::ffi::OsStr::new(SENTINEL)),
                    "{k} is declared but the fixture neither set nor removed it, \
                     so it must be left as captured"
                );
            }
        }
        assert_eq!(crate::profiles::runtime_tier_override(), None);

        // Restore-on-drop, checked while STILL holding the lock.
        let _lock = amb.into_lock();
        for k in AMBIENT_ENV_KEYS {
            assert_eq!(
                std::env::var(k).ok().as_deref(),
                Some(SENTINEL),
                "{k} was not restored when the fixture dropped"
            );
        }
        assert_eq!(
            crate::profiles::runtime_tier_override(),
            Some(crate::profiles::LOCAL_TIER)
        );
        crate::profiles::set_runtime_tier_override(prev_tier);
    }

    #[test]
    fn ambient_env_keys_are_sorted_unique_and_cover_the_fixture() {
        let mut sorted = AMBIENT_ENV_KEYS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, AMBIENT_ENV_KEYS,
            "AMBIENT_ENV_KEYS must be sorted and unique"
        );
        let set: BTreeSet<&str> = AMBIENT_ENV_KEYS.iter().copied().collect();
        for k in crate::profiles::COORD_BASE_ENV_KEYS
            .iter()
            .chain(KEYS_SET_TO_DIR)
            .chain(KEYS_REMOVED)
            .chain(&["QONTINUI_ROOT", "QONTINUI_ENV", QONTINUI_HOME_ENV])
        {
            assert!(set.contains(k), "{k} must be in AMBIENT_ENV_KEYS");
        }
    }

    // -- source-scan drift guards ------------------------------------------

    /// Every `.rs` under `src-tauri/src`, with its path relative to that dir
    /// and `/`-separated on every OS.
    fn rust_sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let src = std::fs::read_to_string(path).unwrap();
            out.push((rel, src));
        }
        assert!(out.len() > 100, "the walk found only {} files", out.len());
        out
    }

    /// Blank out `//` and `/* */` comments outside string literals, preserving
    /// byte offsets and newlines so line numbers still resolve.
    fn strip_comments(src: &str) -> String {
        let bytes = src.as_bytes();
        let mut out = src.to_string().into_bytes();
        let mut i = 0;
        let mut in_str = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_str {
                if b == b'\\' {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_str = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => {
                    in_str = true;
                    i += 1;
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        out[i] = b' ';
                        i += 1;
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/'))
                    {
                        if bytes[i] != b'\n' {
                            out[i] = b' ';
                        }
                        i += 1;
                    }
                    if i < bytes.len() {
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                    }
                }
                _ => i += 1,
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn line_of(src: &str, offset: usize) -> usize {
        src[..offset].matches('\n').count() + 1
    }

    /// Files allowed to spell `".qontinui"` because theirs is a PROJECT-local
    /// `<repo>/.qontinui/…`, not the user's home. Each entry must still be in
    /// use, so a routed file cannot linger here.
    const PROJECT_LOCAL_QONTINUI_DIRS: &[(&str, &str)] = &[
        (
            "constraint_engine/config.rs",
            "<repo>/.qontinui/<CONFIG_FILENAME> — a per-project constraints file",
        ),
        (
            "context/project_contexts.rs",
            "<repo>/.qontinui/contexts/ — project-local prompt contexts",
        ),
        (
            "mcp/constraints_api.rs",
            "<repo>/.qontinui/<CONFIG_FILENAME> — same file as constraint_engine",
        ),
        (
            "workflow/dag_sync.rs",
            "<repo>/.qontinui/workflows — project-local workflow files",
        ),
        (
            "instance.rs",
            "tests hand scope_path a RELATIVE .qontinui/runner base; it resolves no home",
        ),
    ];

    /// Drift guard (a): outside this module, nobody builds `<home>/.qontinui`.
    #[test]
    fn no_qontinui_dir_is_built_outside_the_seam() {
        let mut violations = Vec::new();
        let mut allowlist_used: BTreeSet<&str> = BTreeSet::new();
        for (rel, src) in rust_sources() {
            if rel == "ambient.rs" {
                continue;
            }
            let code = strip_comments(&src);
            for (idx, _) in code.match_indices("home_dir()") {
                let window = &code[idx..code.len().min(idx + 400)];
                if window.contains(".qontinui") {
                    violations.push(format!(
                        "{rel}:{} — a `home_dir()` chained into `.qontinui`; use ambient::qontinui_dir()",
                        line_of(&code, idx)
                    ));
                }
            }
            for (idx, _) in code.match_indices("\".qontinui\"") {
                match PROJECT_LOCAL_QONTINUI_DIRS.iter().find(|(f, _)| *f == rel) {
                    Some((f, _)) => {
                        allowlist_used.insert(f);
                    }
                    None => violations.push(format!(
                        "{rel}:{} — the literal \".qontinui\" belongs to ambient::qontinui_dir() \
                         (or, for a PROJECT-local .qontinui, to PROJECT_LOCAL_QONTINUI_DIRS)",
                        line_of(&code, idx)
                    )),
                }
            }
        }
        for (f, _) in PROJECT_LOCAL_QONTINUI_DIRS {
            assert!(
                allowlist_used.contains(f),
                "{f} is allow-listed but no longer spells \".qontinui\" — remove the entry"
            );
        }
        assert!(
            violations.is_empty(),
            "~/.qontinui built outside ambient.rs:\n  {}",
            violations.join("\n  ")
        );
    }

    /// Drift guard (b): every `env::var("QONTINUI_…")` / `("COORD_…")` literal
    /// in the crate is declared in [`AMBIENT_ENV_KEYS`], so the fixture pins it.
    #[test]
    fn every_literal_qontinui_or_coord_env_read_is_declared() {
        let declared: BTreeSet<&str> = AMBIENT_ENV_KEYS.iter().copied().collect();
        let mut violations = BTreeSet::new();
        for (rel, src) in rust_sources() {
            let code = strip_comments(&src);
            for needle in ["env::var(", "env::var_os("] {
                for (idx, _) in code.match_indices(needle) {
                    let rest = code[idx + needle.len()..].trim_start();
                    let Some(lit) = rest.strip_prefix('"') else {
                        continue;
                    };
                    let Some(end) = lit.find('"') else {
                        continue;
                    };
                    let key = &lit[..end];
                    if (key.starts_with("QONTINUI_") || key.starts_with("COORD_"))
                        && !declared.contains(key)
                    {
                        violations.insert(format!("{key} ({rel}:{})", line_of(&code, idx)));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "env keys read but not declared in ambient::AMBIENT_ENV_KEYS (add them, sorted):\n  {}",
            violations.into_iter().collect::<Vec<_>>().join("\n  ")
        );
    }
}
