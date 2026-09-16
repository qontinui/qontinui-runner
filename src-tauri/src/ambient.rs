//! The ONE seam through which this crate reads **ambient machine state** —
//! `~/.qontinui/` and the process environment that decides where `~/.qontinui/`
//! is.
//!
//! # The problem this exists for
//!
//! Plan `2026-09-03-runner-tests-read-ambient-machine-state`. A handful of code
//! paths called `dirs::home_dir()` directly and read a real file under it. A
//! test exercising such a path passes on a clean CI runner and fails on a
//! configured developer box — and it fails as an ordinary assertion mismatch,
//! so it reads as *"my diff broke this"* rather than *"this test reached
//! outside its fixture"*.
//!
//! CI on qontinui/qontinui-runner#1325 measured that precisely: the full suite
//! run under a deliberately poisoned `$HOME` on ubuntu-22.04 and
//! windows-latest reddened exactly ONE test on each leg —
//! `session::tests::tenant_scope_of_distinguishes_owned_from_both_unknowns`,
//! with `left: Owned(6c0a78b7-…) right: Unresolved`. It had read the poisoned
//! `machine.json::active_tenant_id`.
//!
//! # What this module provides
//!
//! 1. **One reader.** [`qontinui_dir`], [`runner_dir`], [`machine_json_path`]
//!    and [`read_machine_json`] are the supported way to reach that directory,
//!    so there is exactly one place to point at a fixture. Before D5(c),
//!    twenty-seven sites built `dirs::home_dir()/.qontinui/…` by hand, five of
//!    them with their own `machine.json` serde struct; every one is routed
//!    through here now, and a source-scan test keeps it that way.
//! 2. **One env surface.** [`AMBIENT_ENV_KEYS`] names every process variable
//!    that can change where an ambient read lands, so a fixture can capture and
//!    restore the whole surface instead of a hand-maintained subset that drifts.
//!    A second source-scan test fails when a `QONTINUI_*` / `COORD_*` literal
//!    is read anywhere in the crate without being declared here.
//! 3. **A canary.** In a test process, an ambient read taken with no live
//!    [`test_support::IsolatedAmbient`] guard is **deflected to an empty home**
//!    and reported once, naming the concrete source. That is the deliverable:
//!    a test cannot reach the machine at all, so the failure mode stops
//!    existing rather than merely becoming legible.
//!
//!    The plan specified `panic!` here. Building it and measuring said
//!    otherwise: ~100 tests across 10 modules take an unguarded ambient read,
//!    almost all of them incidentally, and all of them currently green.
//!    Deflection removes the class for every one of them without a
//!    hundred-test migration; [`test_support::strict_canary`] keeps the panic
//!    for the thread that asks. See [`canary`] for the full argument.
//!
//!    The canary is evaluated by this module's own resolvers and by the two
//!    resolvers outside it that read ambient state the seam does not own:
//!    `workspace_paths::runner_workspace_root` (`$QONTINUI_ROOT` and friends —
//!    the funnel every workspace-root reader, `default_canonical_path`
//!    included, goes through) and `profiles::connected_coord_base`
//!    (`$COORD_HTTP_URL`, `profiles.json`, `settings.json`). Each handles a
//!    [`Verdict::Deflect`] by answering what the fixture's empty machine would.
//!
//! # The canary's reach — a runtime property, not a `cfg`
//!
//! It has to cover BOTH crate roots. `cfg(test)` cannot: it is set only while
//! compiling a crate's own test binary, and the runner *binary* crate
//! (`main.rs`) links this rlib compiled WITHOUT it — yet `main.rs`'s module
//! tree is exactly where the one test this plan exists for lives. Widening to
//! `debug_assertions` alone is worse: that is on in an ordinary dev build of
//! the runner, where no fixture is ever live, so the canary would panic on the
//! first real read and brick dev runs.
//!
//! So the *code* is compiled under `any(test, debug_assertions)` and the
//! *decision to evaluate* is made at runtime by
//! [`test_support::canary_armed`] — `cfg!(test)`, an explicit
//! [`test_support::arm_canary`], or "this executable is a cargo test binary in
//! `<target>/<profile>/deps/`". Read that function's docs for why the
//! heuristic's failure modes are asymmetric on purpose.
//!
//! # Release builds carry none of this
//!
//! Both [`test_support`] and the canary body are gated on
//! `any(test, debug_assertions)`, so a release build compiles neither — same
//! discipline as the `mcp::test_fixtures` seam the `seam-gate` CI job guards.
//!
//! # `QONTINUI_HOME` is an override knob, not a runner-facing setting
//!
//! `machine_identity` used to say "deliberately no env override, so a
//! supervisor-spawned temp runner shares the primary's identity". That property
//! still holds: the supervisor never sets `QONTINUI_HOME`, so only a process
//! that explicitly asks for a different `.qontinui` gets one.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// The file under [`qontinui_dir`] that records this machine's identity and its
/// operator-stated tenant. Named once so the readers cannot drift.
pub const MACHINE_JSON: &str = "machine.json";

/// Env var that overrides the `~/.qontinui` directory outright.
pub const QONTINUI_HOME_ENV: &str = "QONTINUI_HOME";

/// Every process environment variable that can change what an ambient read
/// answers — the capture list a fixture must restore to be hermetic.
///
/// It is a SUPERSET of [`crate::profiles::COORD_BASE_ENV_KEYS`] (the lib's own
/// declaration of the coord-base surface); `ambient_env_keys_cover_coord_base`
/// below asserts that containment so the two cannot drift apart silently.
///
/// The keys the fixture itself steers:
///
/// - `QONTINUI_HOME` — the explicit override honored by [`qontinui_dir`].
/// - `HOME` / `USERPROFILE` — what `dirs::home_dir()` reads when the override
///   is absent, on unix and Windows respectively.
/// - `QONTINUI_ROOT` / `QONTINUI_WORKSPACE_ROOT` — workspace discovery.
/// - `QONTINUI_PLANS_DIR` — the plan corpus authoring directory.
/// - `QONTINUI_DISABLE_KEYCHAIN` — flips the credential store to a file
///   backend; a headless Linux box exports it and a test that inherits it
///   exercises a different code path than CI does.
/// - `DATABASE_URL` — set on a developer box and in DB-gated CI, unset
///   elsewhere.
/// - `CLAUDE_CONFIG_DIR` — the Claude account in force on this box. A runner
///   launched from an account shell inherits it, and anything that derives
///   the per-account scratch root (`terminal::session::apply_session_temp_root`
///   CREATES `<~/.qontinui>/scratch/<account>`) then materialises a directory
///   named after the operator's account inside the fixture; a clean CI runner
///   exports no account at all. Removed, so the fixture answers as CI does.
///
/// The rest is **measured, not remembered**: the source-scan test
/// `every_literal_qontinui_or_coord_env_read_is_declared` fails when a
/// `std::env::var("QONTINUI_…")` / `("COORD_…")` literal appears in the
/// PRODUCTION code under `src-tauri/src` — `#[cfg(test)]` modules and
/// `#[test]` fns are outside the scan — that is not in this list, and the
/// fixture captures and restores exactly this list around every test. A key
/// that exists only inside a test (an e2e opt-in, a marker a test plants) is
/// not the ambient surface and is NOT listed: the test that reads it owns it.
/// Keys read through a `const` (`env::var(SOME_ENV)`) are listed too, by
/// hand; the scan cannot see them. Sorted, unique — a test pins both so an
/// addition has one obvious place.
pub const AMBIENT_ENV_KEYS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "COORD_ADMIN_SECRET",
    "COORD_BUDGET_REPUBLISH_SECS",
    "COORD_CLAUDE_PROBE_TTL_SECS",
    "COORD_DEVICE_JWT",
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
    "QONTINUI_AGENT_WORKTREE_MODE",
    "QONTINUI_ALLOW_NO_DB",
    "QONTINUI_API_URL",
    "QONTINUI_AUTO_RESPONSE_FETCH_INTERVAL_SECS",
    "QONTINUI_AUTO_RESPONSE_SCORE_TIMEOUT_SECS",
    "QONTINUI_BROWSE_DIRS",
    "QONTINUI_CAPABILITY_STATE_DIR",
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
    "QONTINUI_DRAIN_TIMEOUT_MS",
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
    "QONTINUI_SPAWN_AUTHZ_DISABLED",
    "QONTINUI_SPAWN_AUTHZ_POLICY_REQUIRED_FLOOR",
    "QONTINUI_SPAWN_OUTCOME_ENABLED",
    "QONTINUI_SPAWN_STALL_SECS",
    "QONTINUI_SPAWN_TENANT_CREDENTIAL",
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
    "QONTINUI_TRUST_GATE_TIER",
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

/// `~/.qontinui/machine.json` as this codebase reads it — ONE serde struct, so
/// the several parsers that grew independently have a single type to converge
/// on.
///
/// `active_tenant_id` is deliberately left as a raw [`serde_json::Value`]:
/// [`crate::tenant_pin`] draws a three-way distinction from it that a
/// `Option<Uuid>` would erase — an **absent** field is a legitimate
/// single-tenant install (`Unpinned`), while a **present but malformed** value
/// is a machine that tried to state its tenant and produced garbage
/// (`Unresolvable`).
///
/// The string fields are NORMALISED by [`parse_machine_json`]: trimmed, and a
/// blank or non-string value reads as absent. `device_id` additionally falls
/// back to the legacy `machine_id` spelling a pre-rename file carries — and a
/// file carrying BOTH spellings (what `pair::ensure_device_id_persisted`
/// writes) reads the canonical one, where a serde `alias` would reject it as a
/// duplicate field.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct MachineJson {
    /// This machine's coord device id, when the document carries one — under
    /// `device_id`, or the legacy `machine_id`.
    pub device_id: Option<String>,
    /// `hostname`, trimmed; `None` when absent or blank.
    pub hostname: Option<String>,
    /// Operator-chosen display name.
    pub name: Option<String>,
    /// Raw and UNVALIDATED — a [`serde_json::Value`] rather than an
    /// `Option<Uuid>` so that "stated, but not a UUID" survives the parse and
    /// can be classified as `Unresolvable` instead of silently becoming
    /// "never stated".
    ///
    /// `None` means the key was absent **or** explicitly `null`: serde folds
    /// a JSON `null` into `None` for an `Option<T>` field, and this type does
    /// not fight that with the `Option<Option<_>>` + `deserialize_with` dance.
    /// Nothing needs the difference — [`crate::tenant_pin`] answers `Unpinned`
    /// to both (`pin_from_active_tenant_id`), because a key that is absent and
    /// a key explicitly set to null are the same statement: no tenant was
    /// named. The distinction that DOES matter is a stated-but-garbage value,
    /// which is `Some(_)` here and is preserved.
    pub active_tenant_id: Option<serde_json::Value>,
    /// Every other key, so a caller needing a field this struct does not name
    /// can reach it without introducing a second parser.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// NOT from the document: `false` when the file could not be read at all,
    /// or was not valid JSON.
    ///
    /// This is the distinction an all-`None` struct cannot carry, and
    /// [`crate::tenant_pin`] needs it: an unreadable file is `Unresolvable`
    /// while a readable one missing the field is `Unpinned`.
    #[serde(skip)]
    pub readable: bool,
}

impl MachineJson {
    /// The device identity as a UUID, when present and well-formed.
    pub fn device_uuid(&self) -> Option<uuid::Uuid> {
        uuid::Uuid::parse_str(self.device_id.as_deref()?).ok()
    }

    /// `active_tenant_id` when it is a non-blank string, trimmed.
    pub fn active_tenant_id_str(&self) -> Option<&str> {
        self.active_tenant_id
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// `active_tenant_id` as a UUID, when present and well-formed.
    pub fn active_tenant_uuid(&self) -> Option<uuid::Uuid> {
        uuid::Uuid::parse_str(self.active_tenant_id_str()?).ok()
    }

    /// Apply the string normalisation the struct docs promise.
    fn normalise(mut self) -> Self {
        fn clean(field: Option<String>) -> Option<String> {
            field
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        self.device_id = clean(self.device_id).or_else(|| {
            self.extra
                .get("machine_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
        self.hostname = clean(self.hostname);
        self.name = clean(self.name);
        self
    }
}

/// Why [`try_read_machine_json`] / [`read_machine_json_at`] could not produce
/// a [`MachineJson`]. The total reader, [`read_machine_json`], folds all of
/// these into `readable: false`; callers that report the failure to an
/// operator (`machine_identity::read_device_id_at`, the worktree and terminal
/// device-id readers) want the path and the cause.
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
        source: serde_json::Error,
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

/// The `~/.qontinui` directory: `$QONTINUI_HOME` when set and non-blank,
/// otherwise `dirs::home_dir()?/.qontinui`.
///
/// `None` when there is no home directory to derive one from — the same
/// "cannot state anything" outcome callers already handled. This is the ONLY
/// function in the crate allowed to spell `".qontinui"` next to a home
/// directory; a source-scan test enforces it.
pub fn qontinui_dir() -> Option<PathBuf> {
    if canary("~/.qontinui") == Verdict::Deflect {
        return Some(deflected_dir());
    }
    qontinui_dir_unchecked()
}

/// Pure core of [`qontinui_dir`]: precedence over injected inputs, so the rule
/// is unit-testable with no `set_var` at all.
///
/// A blank override is not an override — it is the shape a `systemd` unit's
/// `Environment=FOO=` produces, and treating it as a path would resolve every
/// ambient read to the process cwd.
pub fn qontinui_dir_from(override_dir: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        if !dir.to_string_lossy().trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    home.map(|h| h.join(".qontinui"))
}

/// [`qontinui_dir`], falling back to `./.qontinui` when no home resolves —
/// for the call sites that need *a* path rather than an error when the box
/// has no home directory (the `dirs::home_dir().unwrap_or_else(|| ".")`
/// shape they all used to spell by hand).
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

/// The path of [`MACHINE_JSON`] under [`qontinui_dir`].
pub fn machine_json_path() -> Option<PathBuf> {
    if canary("~/.qontinui/machine.json") == Verdict::Deflect {
        return Some(deflected_dir().join(MACHINE_JSON));
    }
    qontinui_dir_unchecked().map(|d| d.join(MACHINE_JSON))
}

/// Read and parse [`MACHINE_JSON`].
///
/// Total: every failure (no home dir, missing file, unreadable file,
/// unparseable JSON) yields a [`MachineJson::default`] with `readable: false`
/// rather than an error — the callers all had to fold those cases anyway, and
/// folding them once here is the point of the seam. A caller that must SAY
/// why the read failed uses [`try_read_machine_json`].
pub fn read_machine_json() -> MachineJson {
    try_read_machine_json().unwrap_or_default()
}

/// [`read_machine_json`] with the failure kept: the path that was read and the
/// I/O or parse error, for the readers whose job is to tell an operator what
/// is wrong with the file.
///
/// Under deflection the answer is the same one a clean box gives — the file is
/// missing from the (empty) deflected home.
pub fn try_read_machine_json() -> Result<MachineJson, MachineJsonError> {
    if canary("~/.qontinui/machine.json") == Verdict::Deflect {
        // The deflected home is empty by construction, so this is the same
        // answer a clean CI runner gives — deterministically, on every box.
        let path = deflected_dir().join(MACHINE_JSON);
        return Err(MachineJsonError::Read {
            path,
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
    }
    let path = qontinui_dir_unchecked()
        .ok_or(MachineJsonError::NoHomeDir)?
        .join(MACHINE_JSON);
    read_machine_json_at(&path)
}

/// Read and parse the `machine.json` at an explicit path. Never creates it.
///
/// Not canaried: the path is the caller's, and the resolver it came from
/// already reported the ambient source.
pub fn read_machine_json_at(path: &Path) -> Result<MachineJson, MachineJsonError> {
    let bytes = std::fs::read(path).map_err(|source| MachineJsonError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    try_parse_machine_json(&bytes).map_err(|source| MachineJsonError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// The parse half of [`read_machine_json`], split out so every outcome is
/// reachable from a test without touching the filesystem or `$HOME`.
pub fn parse_machine_json(bytes: &[u8]) -> MachineJson {
    try_parse_machine_json(bytes).unwrap_or_default()
}

/// [`parse_machine_json`] with the serde error kept.
pub fn try_parse_machine_json(bytes: &[u8]) -> Result<MachineJson, serde_json::Error> {
    let mut doc = serde_json::from_slice::<MachineJson>(bytes)?.normalise();
    doc.readable = true;
    Ok(doc)
}

/// [`qontinui_dir`] without the canary — the internal spelling the canaried
/// entry points and the fixture itself use, so a read that has ALREADY reported
/// its concrete source does not re-report a vaguer one.
fn qontinui_dir_unchecked() -> Option<PathBuf> {
    qontinui_dir_from(std::env::var_os(QONTINUI_HOME_ENV), dirs::home_dir())
}

// ============================================================================
// The canary
// ============================================================================

/// What an ambient read is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Read the real machine. Production, or a test holding a fixture.
    Proceed,
    /// A test process, no fixture: serve an EMPTY ambient home instead of the
    /// operator's real one.
    Deflect,
}

/// Decide whether an ambient read may see the machine.
///
/// # Why an unguarded read is DEFLECTED rather than fatal
///
/// The plan's D2 specified `panic!` here, sized against a ledger of ONE
/// known-bad test. Building it and running the suite under a poisoned home
/// measured the real population: **~100 tests across 10 modules take an
/// unguarded ambient read**, nearly all of them reaching it incidentally
/// through `coord_mcp`'s `resolve_tenant_pin()` while testing nonce and proxy
/// logic that has nothing to do with tenancy. Every one of them PASSED under
/// poison — they read ambient state without depending on its value.
///
/// Panicking would turn ~100 green tests red to report a latent fragility,
/// which is not shippable and would get the canary deleted (D2's own stated
/// failure mode). Guarding all ~100 is the deferred D5(c) refactor.
///
/// Deflection is strictly stronger than either, on the plan's own ranking:
///
/// - **#1 capability** — panicking *reports* the class; deflection *removes*
///   it. A test that cannot reach the machine cannot depend on it, so
///   "passes on CI, fails on your box" stops being expressible. The empty
///   home is exactly what a clean CI runner presents, so every test now sees
///   CI's answer on every box.
/// - **#3 robustness** — there is no allowlist to drift and no migration to
///   half-finish. A NEW ambient reader is hermetic the day it is written.
///
/// A test that WANTS ambient data supplies it through
/// [`test_support::IsolatedAmbient::write_machine_json`] — the file becomes a
/// fixture input, which is the dossier's exit criterion.
///
/// Loudness is kept where it costs nothing: the first deflection per source
/// prints a line NAMING that source, so a test surprised by an empty read is
/// told why in one line rather than debugging a mystery. And
/// [`test_support::strict_canary`] restores the hard panic for the thread that
/// asks — which is how this module's own tests, and the bin crate's, prove the
/// canary reaches them at all.
///
/// Allow arms, in order:
///
/// 1. not a test harness — see [`test_support::canary_armed`]. A shipped or
///    dev runner reads ambient state because that is its job;
/// 2. a live [`test_support::IsolatedAmbient`] on THIS thread;
/// 3. else any live guard anywhere in the process. Deliberately soft: a test
///    that hands work to a helper thread or a tokio worker still reads through
///    its own fixture, and that fixture's env IS the isolated one, so the
///    machine is not reached either way.
///
/// `pub` because two resolvers outside this module read ambient state the
/// seam does not own and answer a [`Verdict::Deflect`] themselves:
/// `workspace_paths::runner_workspace_root` and `profiles::connected_coord_base`.
#[cfg(any(test, debug_assertions))]
pub fn canary(source: &str) -> Verdict {
    if !test_support::canary_armed() {
        return Verdict::Proceed;
    }
    if test_support::thread_is_guarded() || test_support::live_guard_count() > 0 {
        return Verdict::Proceed;
    }

    let home = match std::env::var_os(QONTINUI_HOME_ENV) {
        Some(v) if !v.is_empty() => format!("QONTINUI_HOME={}", v.to_string_lossy()),
        _ => "QONTINUI_HOME unset".to_string(),
    };

    if test_support::thread_is_strict() {
        panic!(
            "ambient read of {source} ({home}) from a test with no isolated_ambient() guard \
             — see plan 2026-09-03-runner-tests-read-ambient-machine-state"
        );
    }

    test_support::warn_once(source, &home);
    Verdict::Deflect
}

/// Release builds carry no canary at all — `debug_assertions` is off there, and
/// `test_support` (which holds the state this reads) is not compiled either.
#[cfg(not(any(test, debug_assertions)))]
#[inline(always)]
pub fn canary(_source: &str) -> Verdict {
    Verdict::Proceed
}

/// The empty directory an unguarded test read is served instead of
/// `~/.qontinui`.
///
/// One per process, created on first deflection and never removed — it must
/// EXIST (rather than merely be a path that does not resolve) so that a test
/// which *writes* to the ambient home still succeeds, writing into the void
/// instead of into the operator's real `~/.qontinui`. That write-redirection
/// is a second defect this closes: an unguarded test that wrote there was
/// mutating the machine it ran on.
#[cfg(any(test, debug_assertions))]
fn deflected_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("qontinui-ambient-deflected-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    })
    .clone()
}

#[cfg(not(any(test, debug_assertions)))]
fn deflected_dir() -> PathBuf {
    unreachable!("release builds never deflect")
}

/// The empty WORKSPACE ROOT an unguarded test read of `$QONTINUI_ROOT` is
/// served — the deflection twin of [`test_support::IsolatedAmbient::root`],
/// for `workspace_paths::runner_workspace_root`. Created, for the same reason
/// [`deflected_dir`] is: a resolver that existence-checks its candidates must
/// accept it, and a test that materialises something under it lands in the
/// void rather than in the operator's checkout.
pub fn deflected_workspace_root() -> PathBuf {
    let root = deflected_dir().join("root");
    let _ = std::fs::create_dir_all(&root);
    root
}

// ============================================================================
// The fixture
// ============================================================================

/// The isolation fixture, plus the ONE process-wide env lock this rlib owns.
///
/// Gated on `any(test, debug_assertions)` rather than `cfg(test)` so the runner
/// *binary* crate's tests can use it too: `cargo test` builds the bin's
/// dependencies (this rlib included) without `cfg(test)` but WITH
/// `debug_assertions`. A release build has neither and compiles none of this.
#[cfg(any(test, debug_assertions))]
pub mod test_support {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    /// A profile name no real `profiles.json` can carry, so the profile arm of
    /// `profiles::resolve_coord_base()` misses deterministically everywhere.
    pub const NO_SUCH_PROFILE: &str = "__qontinui_test_no_such_profile__";

    /// Keys the fixture points at its temp dir: the ambient home (both the
    /// seam's override and what `dirs::home_dir()` reads), the runner's
    /// config dir (so the tier a `profiles` read infers comes from OUR
    /// `settings.json`) and its secure-storage dir (empty ⇒ not paired).
    pub const KEYS_SET_TO_DIR: &[&str] = &[
        "QONTINUI_HOME",
        "QONTINUI_CONFIG_DIR",
        "QONTINUI_SECURE_STORAGE_DIR",
        "HOME",
        "USERPROFILE",
    ];

    /// Keys the fixture removes, so the process looks unconfigured — what a
    /// configured developer box exports and a clean CI runner does not.
    pub const KEYS_REMOVED: &[&str] = &[
        "CLAUDE_CONFIG_DIR",
        "COORD_HTTP_URL",
        "QONTINUI_WORKSPACE_ROOT",
        "QONTINUI_SERVER_MODE",
        "QONTINUI_RUNNER_TOKEN",
        "QONTINUI_RUNNER_TIER",
        "DATABASE_URL",
    ];

    /// A single process-wide lock that serializes every test which reads or
    /// mutates a `std::env` variable.
    ///
    /// `std::env` is process-global, so two tests touching the same var in
    /// parallel race — one clobbers the value mid-read, the code-under-test
    /// sees the wrong value, and CI reddens non-deterministically (the flake
    /// class fixed 2026-07-11; cf. `qontinui_shim::resolve_real_in`).
    ///
    /// It lives HERE, in the rlib, rather than once per crate root, because the
    /// runner-bin test binary links this rlib: a lock defined in `main.rs` and
    /// a lock defined in `lib.rs` are two different statics in that one
    /// process, so a bin test holding one would not exclude an
    /// [`IsolatedAmbient`] holding the other. `lib.rs::test_env` and
    /// `main.rs::test_env` both re-export this one.
    ///
    /// Poison-recovering so a panicking test can't cascade-fail the rest.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// How many [`EnvLockGuard`]s this thread currently holds. See
        /// [`env_lock`].
        static ENV_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// The guard [`env_lock`] returns. Opaque on purpose — see that function
    /// for why it is not a bare `MutexGuard`.
    pub struct EnvLockGuard {
        /// `Some` only for the OUTERMOST acquisition on this thread; a nested
        /// one holds nothing and so releases nothing when it drops.
        _inner: Option<MutexGuard<'static, ()>>,
    }

    impl Drop for EnvLockGuard {
        fn drop(&mut self) {
            ENV_LOCK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            // `_inner` drops after this, releasing the mutex at depth 0.
        }
    }

    /// Acquire the shared env lock. Hold the returned guard for the whole body
    /// of any test that touches `std::env`.
    ///
    /// **Reentrant per thread.** A plain `std::sync::Mutex` is not, and that
    /// mattered the moment [`IsolatedAmbient`] started taking this same lock:
    /// ~115 runner-bin tests reach ambient machine state, a dozen of them from
    /// bodies that already hold `env_lock()`, and a non-reentrant lock would
    /// have turned each of those into a silent hang rather than a failure.
    /// Nesting is counted per thread and the mutex is released only when the
    /// outermost guard drops; RAII gives the LIFO drop order that requires.
    ///
    /// Cross-thread exclusion is unchanged — that is the property the lock
    /// exists for.
    pub fn env_lock() -> EnvLockGuard {
        let depth = ENV_LOCK_DEPTH.with(|d| d.get());
        let inner = if depth == 0 {
            Some(ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner()))
        } else {
            None
        };
        ENV_LOCK_DEPTH.with(|d| d.set(depth + 1));
        EnvLockGuard { _inner: inner }
    }

    /// RAII guard that restores the captured env vars to their pre-capture
    /// values on drop (including the panic path). Use for tests that mutate a
    /// process-global var which may already be set in the environment (e.g.
    /// `DATABASE_URL` in dev / DB-gated CI) so the test can't leak its value —
    /// or its removal — to sibling tests in the same binary.
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

    /// How many [`IsolatedAmbient`] guards are alive in this process.
    static LIVE_GUARDS: AtomicUsize = AtomicUsize::new(0);

    /// Explicit override for [`canary_armed`], set by [`arm_canary`].
    static EXPLICIT_ARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Force the canary on for this process, regardless of what
    /// [`canary_armed`]'s heuristic concludes.
    ///
    /// Exists so a test binary that the heuristic cannot recognise (a custom
    /// harness, a binary copied out of `deps/`) can still arm it, and so the
    /// arming does not depend on some other test having constructed a fixture
    /// first. There is no disarm: a process that has ever declared itself a
    /// test harness stays one.
    pub fn arm_canary() {
        EXPLICIT_ARM.store(true, Ordering::SeqCst);
    }

    /// Whether the canary should evaluate at all in THIS process.
    ///
    /// # Why this is not simply the `cfg`
    ///
    /// `cfg(test)` is set only while compiling a crate's OWN test binary. When
    /// `cargo test` builds the runner *binary* crate, it links this rlib as an
    /// ordinary dependency compiled WITHOUT `cfg(test)` — so a `cfg(test)`
    /// canary is a no-op for every test defined in `main.rs`'s module tree,
    /// which is precisely where the one test this plan exists for lives
    /// (`session::tests::tenant_scope_of_distinguishes_owned_from_both_unknowns`).
    ///
    /// Widening the gate to `debug_assertions` alone would be worse: that is ON
    /// in an ordinary dev build of the runner, where no fixture is ever live,
    /// so the canary would panic on the first real `read_machine_json()` and
    /// brick dev runs. The gate has to be a *runtime* property of the process,
    /// not a compile-time property of the build.
    ///
    /// Three signals, cheapest first:
    ///
    /// 1. `cfg!(test)` — definitive for this rlib's own test binary.
    /// 2. [`arm_canary`] — an explicit declaration.
    /// 3. Otherwise: is this executable a cargo-built test binary? Cargo emits
    ///    every unit-test, integration-test and bench binary into
    ///    `<target>/<profile>/deps/`, and the runner binary is never RUN from
    ///    there — `cargo run`, `tauri dev`, the published build and the
    ///    installed build all live one directory up or somewhere else
    ///    entirely.
    ///
    /// The heuristic's failure modes are asymmetric on purpose. A false
    /// negative (a test binary the heuristic does not recognise) turns the
    /// canary off, which is exactly the pre-plan status quo and costs nothing.
    /// A false positive would panic a real runner — which requires launching
    /// the shipped binary from a directory literally named `deps`, and is
    /// additionally impossible in a release build where this whole module is
    /// `cfg`-ed out.
    ///
    /// Memoised: `current_exe()` is a syscall and `read_machine_json` is not
    /// hot, but it is called on paths that run per session.
    pub fn canary_armed() -> bool {
        if cfg!(test) || EXPLICIT_ARM.load(Ordering::SeqCst) {
            return true;
        }
        static DETECTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *DETECTED.get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.file_name() == Some("deps".as_ref())))
                .unwrap_or(false)
        })
    }

    thread_local! {
        /// Whether THIS thread currently holds an [`IsolatedAmbient`].
        static THREAD_ARMED: Cell<bool> = const { Cell::new(false) };
    }

    /// See [`LIVE_GUARDS`]. Read by the canary.
    pub fn live_guard_count() -> usize {
        LIVE_GUARDS.load(Ordering::SeqCst)
    }

    /// See [`THREAD_ARMED`]. Read by the canary.
    pub fn thread_is_guarded() -> bool {
        THREAD_ARMED.with(|c| c.get())
    }

    thread_local! {
        /// Whether an unguarded ambient read on THIS thread should panic
        /// instead of being deflected to an empty home. See [`strict_canary`].
        static THREAD_STRICT: Cell<bool> = const { Cell::new(false) };
    }

    /// See [`THREAD_STRICT`]. Read by the canary.
    pub fn thread_is_strict() -> bool {
        THREAD_STRICT.with(|c| c.get())
    }

    /// Make an unguarded ambient read on THIS thread panic, naming its source,
    /// instead of being deflected to an empty home.
    ///
    /// **Thread-local on purpose.** The obvious spelling — a process-global
    /// flag, or an env var — would be read by every test running in parallel,
    /// so a strict test would arm the panic under its siblings for the
    /// duration and redden whichever of them happened to take an unguarded
    /// ambient read at that instant. That is a flake generator. Scoping the
    /// strictness to the asking thread makes it observable only by the test
    /// that asked for it.
    ///
    /// Used by the tests that must prove the canary is WIRED — including the
    /// bin crate's, where `cfg(test)` does not reach and the runtime arming is
    /// the thing under test.
    pub fn strict_canary() -> StrictCanary {
        let prev = THREAD_STRICT.with(|c| c.replace(true));
        StrictCanary { prev }
    }

    /// RAII guard returned by [`strict_canary`].
    pub struct StrictCanary {
        prev: bool,
    }

    impl Drop for StrictCanary {
        fn drop(&mut self) {
            let prev = self.prev;
            THREAD_STRICT.with(|c| c.set(prev));
        }
    }

    /// Print ONE line per distinct ambient source that gets deflected, so a
    /// test surprised by an empty ambient read is told why without drowning a
    /// suite log in one line per read.
    pub fn warn_once(source: &str, home: &str) {
        use std::sync::Mutex;
        static SEEN: Mutex<Option<Vec<String>>> = Mutex::new(None);
        let mut guard = match SEEN.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let seen = guard.get_or_insert_with(Vec::new);
        if seen.iter().any(|s| s == source) {
            return;
        }
        seen.push(source.to_string());
        eprintln!(
            "[ambient] deflected an unguarded test read of {source} ({home}) to an empty home. \
             Hold `ambient::test_support::isolated_ambient()` and write the file you want \
             the test to see. Plan 2026-09-03-runner-tests-read-ambient-machine-state."
        );
    }

    /// An RAII fixture that makes every ambient read in its scope land inside a
    /// throwaway directory, and arms the canary for reads taken outside one.
    ///
    /// On construction it:
    ///
    /// - takes the process-wide [`env_lock`], so no sibling test can observe or
    ///   race the environment it is about to rewrite;
    /// - captures every [`super::AMBIENT_ENV_KEYS`] value for restoration on
    ///   drop, **including the panic path**;
    /// - creates a `tempfile::tempdir()` and points every [`KEYS_SET_TO_DIR`]
    ///   key at it — so [`super::qontinui_dir`], `dirs::home_dir()`, the
    ///   runner's config dir and its secure-storage dir all resolve inside the
    ///   fixture;
    /// - creates `<dir>/root` and points `QONTINUI_ROOT` at it, so the
    ///   workspace-root resolver answers a directory this test owns;
    /// - removes every [`KEYS_REMOVED`] key, which a configured developer box
    ///   exports and a clean CI runner does not;
    /// - sets `QONTINUI_ENV` to [`NO_SUCH_PROFILE`], so the profile arm of the
    ///   coord-base resolver misses deterministically;
    /// - clears the process-global runtime tier override
    ///   ([`crate::profiles::set_runtime_tier_override`]), which is not an env
    ///   var and so is outside [`EnvVarRestore`]'s reach.
    ///
    /// The directory starts EMPTY: there is no `machine.json` until a test
    /// writes one with [`IsolatedAmbient::write_machine_json`]. That is the
    /// exit criterion made testable — the file becomes a fixture INPUT rather
    /// than something the box happens to have.
    pub struct IsolatedAmbient {
        // Field order is drop order, and drop order matters: restore the
        // environment and release the arming BEFORE the temp dir is deleted,
        // so nothing can resolve `QONTINUI_HOME` to a path that no longer
        // exists. `_lock` is last so it outlives every restore.
        _restore: EnvVarRestore,
        dir: tempfile::TempDir,
        prev_thread_armed: bool,
        // The REENTRANT guard, not a bare `MutexGuard`: a test body that
        // already holds `env_lock()` and then builds a fixture must nest
        // rather than deadlock. See [`env_lock`].
        _lock: EnvLockGuard,
    }

    /// Construct an [`IsolatedAmbient`] — the spelling the plan names, and the
    /// one the canary's own message points at.
    pub fn isolated_ambient() -> IsolatedAmbient {
        IsolatedAmbient::new()
    }

    impl IsolatedAmbient {
        /// Construct the fixture. See the type docs for everything it does.
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            let lock = env_lock();
            let restore = EnvVarRestore::capture(super::AMBIENT_ENV_KEYS);
            let dir = tempfile::tempdir().expect("isolated ambient fixture needs a temp dir");
            let root = dir.path().join("root");
            std::fs::create_dir_all(&root).expect("isolated ambient fixture needs <dir>/root");

            let prev_thread_armed = THREAD_ARMED.with(|c| c.replace(true));
            LIVE_GUARDS.fetch_add(1, Ordering::SeqCst);

            for key in KEYS_SET_TO_DIR {
                std::env::set_var(key, dir.path());
            }
            std::env::set_var("QONTINUI_ROOT", &root);
            for key in KEYS_REMOVED {
                std::env::remove_var(key);
            }
            std::env::set_var("QONTINUI_ENV", NO_SUCH_PROFILE);
            crate::profiles::set_runtime_tier_override(None);

            Self {
                _restore: restore,
                dir,
                prev_thread_armed,
                _lock: lock,
            }
        }

        /// The fixture's root — the directory `QONTINUI_HOME` points at, and so
        /// the directory [`super::qontinui_dir`] answers.
        pub fn dir(&self) -> &Path {
            self.dir.path()
        }

        /// The created `$QONTINUI_ROOT` (`<dir>/root`) — what the workspace-root
        /// resolver answers inside this fixture.
        pub fn root(&self) -> PathBuf {
            self.dir.path().join("root")
        }

        /// Where [`super::read_machine_json`] will look inside this fixture.
        pub fn machine_json_path(&self) -> PathBuf {
            self.dir.path().join(super::MACHINE_JSON)
        }

        /// Write a `machine.json` into the fixture, verbatim.
        pub fn write_machine_json(&self, contents: &str) -> PathBuf {
            let path = self.machine_json_path();
            std::fs::write(&path, contents).expect("fixture machine.json must be writable");
            path
        }

        /// Write a `machine.json` stating `active_tenant_id`.
        pub fn write_active_tenant_id(&self, tenant: uuid::Uuid) -> PathBuf {
            self.write_machine_json(&format!(
                "{{\"device_id\":\"fixture-device\",\"active_tenant_id\":\"{tenant}\"}}"
            ))
        }

        /// Write a `settings.json` into the fixture — the runner tier document
        /// `QONTINUI_CONFIG_DIR` (already pointed here) resolves — so the tier
        /// a `profiles` read infers comes from the fixture rather than from
        /// the box. Re-pins the config and secure-storage dirs in case a test
        /// moved them.
        pub fn write_settings_json(&self, contents: &str) -> PathBuf {
            let path = self.dir.path().join("settings.json");
            std::fs::write(&path, contents).expect("fixture settings.json must be writable");
            std::env::set_var("QONTINUI_CONFIG_DIR", self.dir.path());
            std::env::set_var("QONTINUI_SECURE_STORAGE_DIR", self.dir.path());
            path
        }
    }

    impl Drop for IsolatedAmbient {
        fn drop(&mut self) {
            LIVE_GUARDS.fetch_sub(1, Ordering::SeqCst);
            let prev = self.prev_thread_armed;
            THREAD_ARMED.with(|c| c.set(prev));
            crate::profiles::set_runtime_tier_override(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use proc_macro2::{Delimiter, TokenStream, TokenTree};
    use std::collections::BTreeSet;

    /// The containment that keeps [`AMBIENT_ENV_KEYS`] from drifting away from
    /// the lib's own declaration of the coord-base surface. The hand-maintained
    /// version of this list already drifted once — `QONTINUI_SERVER_MODE`
    /// became a tier signal and two of three copies never learned about it.
    #[test]
    fn ambient_env_keys_cover_coord_base() {
        for key in crate::profiles::COORD_BASE_ENV_KEYS {
            assert!(
                AMBIENT_ENV_KEYS.contains(key),
                "AMBIENT_ENV_KEYS is missing coord-base key {key}"
            );
        }
    }

    #[test]
    fn ambient_env_keys_have_no_duplicates() {
        let mut seen: Vec<&str> = AMBIENT_ENV_KEYS.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "AMBIENT_ENV_KEYS has a duplicate");
    }

    /// Sorted so an addition has one obvious place, and covering every key the
    /// fixture itself steers — a key the fixture sets or removes but does not
    /// declare would not be restored on drop.
    #[test]
    fn ambient_env_keys_are_sorted_and_cover_the_fixture() {
        let mut sorted = AMBIENT_ENV_KEYS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, AMBIENT_ENV_KEYS, "AMBIENT_ENV_KEYS must be sorted");
        let set: BTreeSet<&str> = AMBIENT_ENV_KEYS.iter().copied().collect();
        for k in fixture_steered_keys() {
            assert!(set.contains(k), "{k} must be in AMBIENT_ENV_KEYS");
        }
    }

    // ---- the pure resolver, reachable without touching `$HOME` ----

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

    // ---- the canary ----

    /// THE deliverable, negatively: an ambient read with no fixture does not
    /// return a value the box happens to have — it fails, naming what it read.
    ///
    /// Holds [`test_support::env_lock`] so the canary's soft process-global arm
    /// cannot be satisfied by a sibling test's guard running concurrently: an
    /// `IsolatedAmbient` holds that same lock for its whole life.
    #[test]
    #[should_panic(expected = "ambient read of")]
    fn unguarded_qontinui_dir_read_names_its_ambient_source() {
        let _lock = env_lock();
        let _strict = strict_canary();
        let _ = qontinui_dir();
    }

    #[test]
    #[should_panic(expected = "ambient read of ~/.qontinui/machine.json")]
    fn unguarded_machine_json_read_names_the_file() {
        let _lock = env_lock();
        let _strict = strict_canary();
        let _ = read_machine_json();
    }

    /// THE deliverable, in its default posture: an unguarded read does not see
    /// the machine.
    ///
    /// This box HAS a populated `~/.qontinui/machine.json` (that is the whole
    /// premise of the plan), so before the seam this read returned the
    /// operator's real device id. It must now return the same empty answer a
    /// clean CI runner gives — deterministically, without a fixture, and
    /// without the test having to know it was at risk.
    #[test]
    fn an_unguarded_read_is_deflected_away_from_the_machine() {
        let _lock = env_lock();

        let doc = read_machine_json();
        assert!(
            !doc.readable,
            "an unguarded test read must not reach the machine's machine.json"
        );
        assert!(
            doc.device_id.is_none(),
            "the box's device id leaked into a test"
        );
        // The fallible twin says WHY, and the why is the clean-box answer.
        let err = try_read_machine_json().unwrap_err();
        assert!(err.is_missing(), "{err}");

        // And the directory it would have used is the deflected one, not the
        // real home.
        let dir = qontinui_dir().expect("deflection always yields a directory");
        assert!(
            dir.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("qontinui-ambient-deflected-")),
            "unguarded reads must resolve to the deflected home, got {dir:?}"
        );
        assert!(
            dir.exists(),
            "the deflected home must exist so writes land harmlessly"
        );
        let root = deflected_workspace_root();
        assert!(root.starts_with(&dir) && root.is_dir());
    }

    /// Strictness is scoped to the thread that asks, so a strict test cannot
    /// redden a sibling running in parallel. If this ever regresses, the
    /// `should_panic` tests above become a flake generator for the whole suite.
    #[test]
    fn strictness_does_not_leak_past_its_guard() {
        let _lock = env_lock();
        {
            let _strict = strict_canary();
            assert!(thread_is_strict());
        }
        assert!(!thread_is_strict());
        // And the read is deflected again rather than panicking.
        assert!(!read_machine_json().readable);
    }

    // ---- the fixture ----

    #[test]
    fn guarded_reads_land_inside_the_fixture() {
        let amb = isolated_ambient();
        assert_eq!(qontinui_dir().as_deref(), Some(amb.dir()));
        assert_eq!(runner_dir(), Some(amb.dir().join("runner")));
        assert_eq!(machine_json_path(), Some(amb.machine_json_path()));
        assert!(amb.root().is_dir(), "QONTINUI_ROOT must be a CREATED dir");
        assert_eq!(
            std::env::var_os("QONTINUI_ROOT").as_deref(),
            Some(amb.root().as_os_str())
        );
    }

    /// The bare case the poisoned-home CI run reddened: with a fixture, the
    /// absence of a `machine.json` is a property of the FIXTURE, not of the
    /// machine the test happens to run on.
    #[test]
    fn fixture_starts_with_no_machine_json() {
        let _amb = isolated_ambient();
        let doc = read_machine_json();
        assert!(!doc.readable, "a fresh fixture must carry no machine.json");
        assert!(doc.active_tenant_id.is_none());
        assert!(doc.device_id.is_none());
        assert!(try_read_machine_json().unwrap_err().is_missing());
    }

    #[test]
    fn fixture_machine_json_is_an_input() {
        let amb = isolated_ambient();
        let tenant = uuid::Uuid::from_u128(0xA11CE);
        amb.write_active_tenant_id(tenant);

        let doc = read_machine_json();
        assert!(doc.readable);
        assert_eq!(doc.device_id.as_deref(), Some("fixture-device"));
        assert_eq!(
            doc.active_tenant_id.as_ref().and_then(|v| v.as_str()),
            Some(tenant.to_string().as_str())
        );
        assert_eq!(doc.active_tenant_uuid(), Some(tenant));
    }

    #[test]
    fn qontinui_home_override_beats_the_home_dir() {
        let amb = isolated_ambient();
        let elsewhere = amb.dir().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::env::set_var("QONTINUI_HOME", &elsewhere);
        assert_eq!(qontinui_dir().as_deref(), Some(elsewhere.as_path()));
    }

    /// An EMPTY `QONTINUI_HOME` is not an override — it is the shape a
    /// `systemd` unit's `Environment=FOO=` produces, and treating it as a path
    /// would resolve every ambient read to the process cwd.
    #[test]
    fn empty_qontinui_home_falls_through_to_the_home_dir() {
        let _amb = isolated_ambient();
        std::env::set_var("QONTINUI_HOME", "");
        // Only the LEAF is asserted, not the whole path: `dirs::home_dir()`
        // reads `$HOME` on unix but the FOLDERID_Profile known folder on
        // Windows, so the fixture's `HOME` does not steer it there. What is
        // under test is that an empty value is not treated as a path — which
        // would otherwise resolve every ambient read to the process cwd (the
        // `systemd Environment=FOO=` shape).
        let dir = qontinui_dir().expect("a home directory must resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some(".qontinui"));
        assert!(dir.parent().is_some_and(|p| !p.as_os_str().is_empty()));
    }

    /// The generalisation of the bin's old `isolate_coord_env_pins_every_declared_key`:
    /// the fixture must capture EVERY declared key (restore on drop), set the
    /// ones it says it sets, remove the ones it says it removes, and leave the
    /// rest exactly as captured.
    ///
    /// Holds the (reentrant) env lock across the fixture's whole life so the
    /// restored environment can be inspected without a sibling test getting
    /// in between.
    /// Every key the fixture STEERS — sets to its dir, removes, or sets to a
    /// fixed value. Not the whole of [`AMBIENT_ENV_KEYS`]: the rest is
    /// captured and restored by `EnvVarRestore` without ever being written,
    /// which is true by construction and needs no sentinel to prove.
    fn fixture_steered_keys() -> Vec<&'static str> {
        KEYS_SET_TO_DIR
            .iter()
            .chain(KEYS_REMOVED)
            .chain(&["QONTINUI_ROOT", "QONTINUI_ENV", QONTINUI_HOME_ENV])
            .copied()
            .collect()
    }

    /// The fixture writes exactly the keys it documents, and puts every one
    /// of them back on drop.
    ///
    /// Only the STEERED keys are set to the sentinel. An earlier version set
    /// all ~190 declared keys: readers of the other ~170 in parallel tests do
    /// not take the env lock — a reader cannot corrupt a sibling, so the guard
    /// does not ask it to — and observed the sentinel mid-test.
    #[test]
    fn isolated_ambient_pins_every_declared_key() {
        const SENTINEL: &str = "__qontinui_ambient_sentinel__";
        let steered = fixture_steered_keys();
        let _lock = env_lock();
        let _outer = EnvVarRestore::capture(AMBIENT_ENV_KEYS);
        for k in &steered {
            std::env::set_var(k, SENTINEL);
        }
        crate::profiles::set_runtime_tier_override(Some(crate::profiles::LOCAL_TIER));

        {
            let amb = isolated_ambient();
            let dir = amb.dir().as_os_str().to_owned();
            for k in &steered {
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
                    panic!("{k} is steered by the fixture but this test does not know how");
                }
            }
            assert_eq!(crate::profiles::runtime_tier_override(), None);
        }

        // Restore-on-drop, checked while STILL holding the lock.
        for k in &steered {
            assert_eq!(
                std::env::var(k).ok().as_deref(),
                Some(SENTINEL),
                "{k} was not restored when the fixture dropped"
            );
        }
        // The override is process-global and NOT an env var: the fixture
        // clears it on drop rather than restoring a value some earlier test
        // may have leaked.
        assert_eq!(crate::profiles::runtime_tier_override(), None);
    }

    // ---- the pure parser, reachable without touching `$HOME` ----

    #[test]
    fn unparseable_machine_json_is_not_readable() {
        let doc = parse_machine_json(b"{ this is not json");
        assert!(!doc.readable);
        assert!(try_parse_machine_json(b"[1,2]").is_err(), "not an object");
    }

    #[test]
    fn readable_document_missing_the_field_is_distinguishable_from_an_unreadable_one() {
        let present = parse_machine_json(br#"{"device_id":"d","hostname":"msi"}"#);
        assert!(present.readable);
        assert!(present.active_tenant_id.is_none());
        assert_eq!(present.device_id.as_deref(), Some("d"));
        assert_eq!(present.hostname.as_deref(), Some("msi"));
        // The key the struct does not name is still reachable.
        assert!(present.extra.is_empty());
        let odd = parse_machine_json(br#"{"paired_at":"2026-01-01"}"#);
        assert_eq!(
            odd.extra.get("paired_at").and_then(|v| v.as_str()),
            Some("2026-01-01")
        );

        let absent = parse_machine_json(b"");
        assert!(!absent.readable);
    }

    /// The legacy `machine_id` spelling still loads, and a file carrying BOTH
    /// spellings — what `pair::ensure_device_id_persisted` writes — reads the
    /// canonical one. A serde `alias` rejects that file as a duplicate field;
    /// the seam must not.
    #[test]
    fn reads_canonical_and_legacy_identity_spellings() {
        let canonical = parse_machine_json(br#"{"device_id":" abc ","hostname":" h "}"#);
        assert_eq!(canonical.device_id.as_deref(), Some("abc"));
        assert_eq!(canonical.hostname.as_deref(), Some("h"));

        let legacy = parse_machine_json(br#"{"machine_id":"def"}"#);
        assert!(legacy.readable);
        assert_eq!(legacy.device_id.as_deref(), Some("def"));

        let both = parse_machine_json(br#"{"machine_id":"old","device_id":"new"}"#);
        assert_eq!(both.device_id.as_deref(), Some("new"));
    }

    #[test]
    fn blank_or_non_string_identity_is_absent() {
        assert_eq!(parse_machine_json(br#"{"device_id":"  "}"#).device_id, None);
        assert_eq!(
            parse_machine_json(br#"{"machine_id":"  "}"#).device_id,
            None
        );
        // A non-string `device_id` is a parse error for the typed field, which
        // the total reader folds into "unreadable" — a machine that wrote a
        // number there did not state an identity.
        assert!(!parse_machine_json(br#"{"device_id":7}"#).readable);
        let empty = parse_machine_json(b"{}");
        assert!(empty.readable);
        assert_eq!(empty.device_id, None);
        assert_eq!(empty.device_uuid(), None);
    }

    #[test]
    fn active_tenant_id_accessors_trim_and_validate() {
        let id = uuid::Uuid::new_v4();
        let pinned = parse_machine_json(format!(r#"{{"active_tenant_id":" {id} "}}"#).as_bytes());
        assert_eq!(pinned.active_tenant_id_str(), Some(id.to_string().as_str()));
        assert_eq!(pinned.active_tenant_uuid(), Some(id));

        let malformed = parse_machine_json(br#"{"active_tenant_id":"not-a-uuid"}"#);
        assert_eq!(malformed.active_tenant_id_str(), Some("not-a-uuid"));
        assert_eq!(malformed.active_tenant_uuid(), None);

        let blank = parse_machine_json(br#"{"active_tenant_id":"   "}"#);
        assert!(blank.active_tenant_id.is_some(), "stated, raw, preserved");
        assert_eq!(blank.active_tenant_id_str(), None);
    }

    /// An explicit `null` folds to `None`, exactly as an absent key does — and
    /// that fold is SAFE, which is the half worth pinning.
    ///
    /// Serde maps a JSON `null` onto `None` for an `Option<T>` field, so this
    /// struct cannot distinguish "absent" from "explicitly null" and does not
    /// try to. The assertion that matters is the one below it: both spellings
    /// reach the same `Unpinned` verdict, so nothing downstream can tell them
    /// apart either. If a caller ever DOES need the difference, this test is
    /// where the `Option<Option<_>>` + `deserialize_with` change announces
    /// itself.
    #[test]
    fn explicit_null_and_absent_tenant_both_fold_to_none_and_to_unpinned() {
        let explicit_null = parse_machine_json(br#"{"active_tenant_id":null}"#);
        assert!(explicit_null.readable);
        assert_eq!(explicit_null.active_tenant_id, None);

        let absent = parse_machine_json(br#"{"device_id":"d"}"#);
        assert!(absent.readable);
        assert_eq!(absent.active_tenant_id, None);

        // The load-bearing half: indistinguishable is CORRECT here, because
        // the classifier gives both the same answer anyway.
        for doc in [&explicit_null, &absent] {
            assert_eq!(
                crate::tenant_pin::pin_from_active_tenant_id(doc.active_tenant_id.as_ref()),
                crate::tenant_pin::TenantPin::Unpinned,
            );
        }
    }

    /// The distinction the raw `Value` DOES buy: a stated-but-unparseable
    /// tenant stays `Some(_)` through the parse, so it can be told apart from
    /// "never stated" and refused rather than silently ignored.
    #[test]
    fn a_stated_but_garbage_tenant_survives_the_parse_as_some() {
        let doc = parse_machine_json(br#"{"active_tenant_id":"not-a-uuid"}"#);
        assert!(doc.readable);
        assert!(doc.active_tenant_id.is_some());
        assert_eq!(
            crate::tenant_pin::pin_from_active_tenant_id(doc.active_tenant_id.as_ref()),
            crate::tenant_pin::TenantPin::Unresolvable,
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
        assert!(err.to_string().starts_with("parse "), "{err}");
    }

    // ---- source-scan drift guards ----

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

    /// One leaf of a file's token stream, with the source line it starts on.
    ///
    /// The tokens are proc-macro2's — the same lexer `syn` uses, and the one
    /// `env_write_lock_guard` (in the bin crate) walks — so comments are gone
    /// before the scan looks, doc comments are attributes it never descends
    /// into, and a `'"'` char literal is a `Literal` rather than something a
    /// byte scanner mistakes for the opening of a string: the hand-rolled
    /// `strip_comments` this replaced inverted its in-string state on exactly
    /// that, for the rest of the file, in 68 files. A group's delimiters are
    /// leaves too, so a walk can still see the `(` of a call.
    #[derive(Debug)]
    enum Tok {
        Ident(String),
        Punct(char),
        Lit(String),
        Open(Delimiter),
        Close(Delimiter),
    }

    struct Leaf {
        tok: Tok,
        line: usize,
    }

    impl Leaf {
        fn is_ident(&self, name: &str) -> bool {
            matches!(&self.tok, Tok::Ident(i) if i == name)
        }
        fn is_punct(&self, ch: char) -> bool {
            matches!(&self.tok, Tok::Punct(p) if *p == ch)
        }
        fn is_open(&self, d: Delimiter) -> bool {
            matches!(&self.tok, Tok::Open(x) if *x == d)
        }
        fn is_close(&self, d: Delimiter) -> bool {
            matches!(&self.tok, Tok::Close(x) if *x == d)
        }
        fn lit(&self) -> Option<&str> {
            match &self.tok {
                Tok::Lit(s) => Some(s),
                _ => None,
            }
        }
    }

    /// A file's PRODUCTION tokens, flattened.
    ///
    /// Everything an item-level `#[cfg(test)]` / `#[cfg(all(test, …))]` or a
    /// `#[test]`-style attribute (`#[test]`, `#[tokio::test(flavor = …)]`)
    /// governs is dropped: the attributed item's body (its next brace group)
    /// or, for a braceless item, everything up to its `;`. A file-level
    /// `#![cfg(test)]` drops the rest of the file. An out-of-line
    /// `#[cfg(test)] mod name;` is recorded in `test_only_mods` so
    /// [`production_sources`] can drop that FILE — the guards are about the
    /// production surface, and a key or a path that exists only inside a
    /// test module is not part of it.
    #[derive(Default)]
    struct ProdTokens {
        leaves: Vec<Leaf>,
        test_only_mods: Vec<String>,
    }

    fn prod_tokens(src: &str, rel: &str) -> ProdTokens {
        let stream: TokenStream = src
            .parse()
            .unwrap_or_else(|e| panic!("{rel} does not tokenize: {e}"));
        let mut out = ProdTokens::default();
        flatten(stream, &mut out);
        out
    }

    fn flatten(stream: TokenStream, out: &mut ProdTokens) {
        let trees: Vec<TokenTree> = stream.into_iter().collect();
        let mut i = 0;
        // Set by a test-only attribute; cleared by the item it governed.
        let mut skipping = false;
        while i < trees.len() {
            let tree = &trees[i];
            // An attribute — `#` [`!`] `[ … ]` — is classified, never entered.
            if matches!(tree, TokenTree::Punct(p) if p.as_char() == '#') {
                let inner =
                    matches!(trees.get(i + 1), Some(TokenTree::Punct(b)) if b.as_char() == '!');
                let at = if inner { i + 2 } else { i + 1 };
                if let Some(TokenTree::Group(g)) = trees.get(at) {
                    if g.delimiter() == Delimiter::Bracket {
                        if is_test_only_attr(g.stream()) {
                            if inner {
                                // `#![cfg(test)]`: the rest of this scope.
                                return;
                            }
                            skipping = true;
                        }
                        i = at + 1;
                        continue;
                    }
                }
            }
            if skipping {
                match tree {
                    TokenTree::Ident(id) if id == "mod" => {
                        if let (Some(TokenTree::Ident(name)), Some(TokenTree::Punct(semi))) =
                            (trees.get(i + 1), trees.get(i + 2))
                        {
                            if semi.as_char() == ';' {
                                out.test_only_mods.push(name.to_string());
                            }
                        }
                    }
                    TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => skipping = false,
                    TokenTree::Punct(p) if p.as_char() == ';' => skipping = false,
                    _ => {}
                }
                i += 1;
                continue;
            }
            let line = tree.span().start().line;
            match tree {
                TokenTree::Group(g) => {
                    out.leaves.push(Leaf {
                        tok: Tok::Open(g.delimiter()),
                        line,
                    });
                    flatten(g.stream(), out);
                    out.leaves.push(Leaf {
                        tok: Tok::Close(g.delimiter()),
                        line: g.span_close().start().line,
                    });
                }
                TokenTree::Ident(id) => out.leaves.push(Leaf {
                    tok: Tok::Ident(id.to_string()),
                    line,
                }),
                TokenTree::Punct(p) => out.leaves.push(Leaf {
                    tok: Tok::Punct(p.as_char()),
                    line,
                }),
                TokenTree::Literal(l) => out.leaves.push(Leaf {
                    tok: Tok::Lit(l.to_string()),
                    line,
                }),
            }
            i += 1;
        }
    }

    /// The inside of an attribute's `[ … ]`: is it `cfg(test)`,
    /// `cfg(all(test, …))`, or a path ending in `test` with an optional
    /// argument group (`test`, `tokio::test`, `tokio::test(flavor = …)`)?
    /// `cfg(any(test, debug_assertions))` is NOT test-only — a dev build of
    /// the runner compiles it — and `cfg(not(test))` is the opposite arm.
    fn is_test_only_attr(attr: TokenStream) -> bool {
        let trees: Vec<TokenTree> = attr.into_iter().collect();
        if let [TokenTree::Ident(cfg), TokenTree::Group(pred)] = trees.as_slice() {
            if cfg == "cfg" && pred.delimiter() == Delimiter::Parenthesis {
                let pred: Vec<TokenTree> = pred.stream().into_iter().collect();
                return match pred.as_slice() {
                    [TokenTree::Ident(t)] => t == "test",
                    [TokenTree::Ident(all), TokenTree::Group(g)] if all == "all" => g
                        .stream()
                        .into_iter()
                        .any(|t| matches!(t, TokenTree::Ident(id) if id == "test")),
                    _ => false,
                };
            }
        }
        // `Ident (:: Ident)*` then at most one trailing group.
        let path_end = trees
            .iter()
            .position(|t| matches!(t, TokenTree::Group(_)))
            .unwrap_or(trees.len());
        if path_end + 1 < trees.len() {
            return false;
        }
        let mut last: Option<String> = None;
        for (k, t) in trees[..path_end].iter().enumerate() {
            match (k % 3, t) {
                (0, TokenTree::Ident(id)) => last = Some(id.to_string()),
                (1 | 2, TokenTree::Punct(p)) if p.as_char() == ':' => {}
                _ => return false,
            }
        }
        last.as_deref() == Some("test")
    }

    /// [`rust_sources`] tokenized to their production surface, with every
    /// file an out-of-line `#[cfg(test)] mod name;` names dropped —
    /// `<dir>/name.rs` and everything under `<dir>/name/`, where `<dir>` is
    /// the declaring file's own module directory.
    fn production_sources() -> Vec<(String, Vec<Leaf>)> {
        let scanned: Vec<(String, ProdTokens)> = rust_sources()
            .into_iter()
            .map(|(rel, src)| {
                let tokens = prod_tokens(&src, &rel);
                (rel, tokens)
            })
            .collect();
        let mut test_only_stems: Vec<String> = Vec::new();
        for (rel, tokens) in &scanned {
            let (parent, file) = rel.rsplit_once('/').unwrap_or(("", rel));
            let dir = if matches!(file, "mod.rs" | "main.rs" | "lib.rs") {
                parent.to_string()
            } else {
                rel.strip_suffix(".rs").unwrap().to_string()
            };
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            for m in &tokens.test_only_mods {
                test_only_stems.push(format!("{prefix}{m}"));
            }
        }
        // Measured 2026-09-13: 11 (`main.rs` alone declares four). A walk
        // that finds none has stopped seeing the attribute, not the modules.
        assert!(
            test_only_stems.len() >= 5,
            "only {} out-of-line `#[cfg(test)] mod …;` found: {test_only_stems:?}",
            test_only_stems.len()
        );
        scanned
            .into_iter()
            .filter(|(rel, _)| {
                !test_only_stems.iter().any(|stem| {
                    *rel == format!("{stem}.rs") || rel.starts_with(&format!("{stem}/"))
                })
            })
            .map(|(rel, tokens)| (rel, tokens.leaves))
            .collect()
    }

    /// The key of an `env::var("…")` / `env::var_os("…")` read at `leaves[i]`,
    /// when that is where one starts.
    fn env_read_key(leaves: &[Leaf], i: usize) -> Option<&str> {
        let [env, c1, c2, var, open, lit] = leaves.get(i..i + 6)? else {
            return None;
        };
        (env.is_ident("env")
            && c1.is_punct(':')
            && c2.is_punct(':')
            && (var.is_ident("var") || var.is_ident("var_os"))
            && open.is_open(Delimiter::Parenthesis))
        .then(|| lit.lit())
        .flatten()
        .and_then(|s| s.strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
    }

    /// Does a literal spell the `.qontinui` DIRECTORY — `".qontinui"`,
    /// `"{}/.qontinui/machine.json"` — as opposed to the bundle identifier
    /// `com.qontinui.runner`, where `.qontinui` is a segment of a dotted name?
    fn spells_the_qontinui_dir(lit: &str) -> bool {
        lit.match_indices(".qontinui").any(|(at, _)| {
            let before = lit[..at].chars().next_back();
            let after = lit[at + ".qontinui".len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric())
                && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
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
    ];

    /// Drift guard (a): outside this module, nobody builds `<home>/.qontinui`.
    #[test]
    fn no_qontinui_dir_is_built_outside_the_seam() {
        let mut violations = Vec::new();
        let mut allowlist_used: BTreeSet<&str> = BTreeSet::new();
        for (rel, leaves) in production_sources() {
            if rel == "ambient.rs" {
                continue;
            }
            for (i, leaf) in leaves.iter().enumerate() {
                // `home_dir()` with `.qontinui` spelled anywhere in the next
                // ~60 tokens — the `format!("{}/.qontinui/…")` shape the
                // exact-literal check below cannot see.
                let is_home_dir_call = leaf.is_ident("home_dir")
                    && leaves
                        .get(i + 1)
                        .is_some_and(|l| l.is_open(Delimiter::Parenthesis))
                    && leaves
                        .get(i + 2)
                        .is_some_and(|l| l.is_close(Delimiter::Parenthesis));
                if is_home_dir_call
                    && leaves[i..leaves.len().min(i + 60)]
                        .iter()
                        .any(|l| l.lit().is_some_and(spells_the_qontinui_dir))
                {
                    violations.push(format!(
                        "{rel}:{} — a `home_dir()` chained into `.qontinui`; use ambient::qontinui_dir()",
                        leaf.line
                    ));
                }
                if leaf.lit() != Some("\".qontinui\"") {
                    continue;
                }
                match PROJECT_LOCAL_QONTINUI_DIRS.iter().find(|(f, _)| *f == rel) {
                    Some((f, _)) => {
                        allowlist_used.insert(f);
                    }
                    None => violations.push(format!(
                        "{rel}:{} — the literal \".qontinui\" belongs to ambient::qontinui_dir() \
                         (or, for a PROJECT-local .qontinui, to PROJECT_LOCAL_QONTINUI_DIRS)",
                        leaf.line
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
    /// in the crate's PRODUCTION code is declared in [`AMBIENT_ENV_KEYS`], so
    /// the fixture pins it.
    #[test]
    fn every_literal_qontinui_or_coord_env_read_is_declared() {
        let declared: BTreeSet<&str> = AMBIENT_ENV_KEYS.iter().copied().collect();
        let mut violations = BTreeSet::new();
        for (rel, leaves) in production_sources() {
            for i in 0..leaves.len() {
                let Some(key) = env_read_key(&leaves, i) else {
                    continue;
                };
                if (key.starts_with("QONTINUI_") || key.starts_with("COORD_"))
                    && !declared.contains(key)
                {
                    violations.insert(format!("{key} ({rel}:{})", leaves[i].line));
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
