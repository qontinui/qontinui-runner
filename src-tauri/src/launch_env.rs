//! Typed snapshot of all `QONTINUI_*` / `WEBVIEW2_*` env vars that influence
//! runner startup.
//!
//! Read once in `main.rs::main()` via [`RunnerLaunchEnv::read`], stored as
//! `Arc<RunnerLaunchEnv>` on Tauri app state. Every other site that needs a
//! launch env var pulls it from here instead of re-parsing.
//!
//! # Why
//!
//! Before this module the runner re-parsed roughly a dozen `std::env::var(...)`
//! calls scattered across `main.rs`, `commands/auth.rs`, `mcp/misc.rs`,
//! `heartbeat.rs`, `settings.rs`, etc. — each with its own `.ok().and_then(parse)`
//! pattern, no central documentation, and no way for tests to construct a
//! known-good environment without `serial_test`-guarded `set_var` calls.
//!
//! With this module:
//!
//! * Adding a new launch env var is one struct field + one parsing line.
//! * Tests construct a `RunnerLaunchEnv` directly — no global mutation.
//! * The set of env vars the runner cares about is documented in one place.
//!
//! # Migration policy
//!
//! Item 3 of the runner-supervisor modularity plan migrates *consumers*
//! that read launch-time env vars. It does not touch:
//!
//! * **Setters** — code that writes env vars onto child `Command`s (the
//!   supervisor's `process/manager.rs`, the runner's `instance_manager.rs`).
//!   Those are about *producing* the env, not consuming it.
//! * **`crate::instance::*`** — those helpers stay as the canonical
//!   parsing point for `QONTINUI_INSTANCE_NAME` / `QONTINUI_PRIMARY_PORT`.
//!   `RunnerLaunchEnv::read()` *delegates* to them so there is still one
//!   parsing point per var.
//! * **Runtime-only env vars** — things like `QONTINUI_PYTHON_PATH` or
//!   `QONTINUI_BROWSE_DIRS` that are read inside request handlers and may
//!   change between runs. Those stay as ad-hoc reads.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use qontinui_types::wire::runner_kind::RunnerKind;

/// Window-placement hints injected by the supervisor on spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowEnvHints {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `Some(true)` = decorations on, `Some(false)` = borderless,
    /// `None` = use Tauri default (which is currently "decorations on").
    pub decorations: Option<bool>,
}

/// Restate runtime overrides injected by the supervisor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestateEnvHints {
    pub external_admin_url: Option<String>,
    pub external_ingress_url: Option<String>,
}

/// Snapshot of all `QONTINUI_*` / `WEBVIEW2_*` env vars read at startup.
///
/// Constructed once via [`RunnerLaunchEnv::read`] in `main.rs::main()`,
/// then cloned via `Arc` onto Tauri app state. Tests construct one directly
/// via field literals or [`RunnerLaunchEnv::default`] + overrides — no
/// `std::env::set_var` mutation required.
///
/// `Default` produces a `Primary` runner with no overrides — equivalent to
/// "this process was launched with no `QONTINUI_*` / `WEBVIEW2_*` vars set."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerLaunchEnv {
    /// Runner classification — `Primary` if `QONTINUI_INSTANCE_NAME` is unset,
    /// `Named { name }` otherwise. The runner can't distinguish `Named` vs
    /// `Temp` from env alone (the supervisor sets `QONTINUI_INSTANCE_NAME`
    /// for both), so `Temp` never appears here. See `runner_kind` module
    /// doc in `qontinui-types::wire`.
    pub kind: RunnerKind,

    /// Raw instance name, for sites that prefer `Option<String>` over
    /// matching on `RunnerKind`. Mirrors `crate::instance::instance_name()`.
    pub instance_name: Option<String>,

    /// Own API port override (`QONTINUI_PORT`). When unset, the runner
    /// falls back to `MCP_API_PORT` (9876).
    pub port: Option<u16>,

    /// Backend API URL override (`QONTINUI_API_URL`).
    pub api_url: Option<String>,

    /// Primary runner's port (`QONTINUI_PRIMARY_PORT`), set on secondaries
    /// so they can register / heartbeat. Mirrors `crate::instance::primary_port()`.
    pub primary_port: Option<u16>,

    /// Server-mode flag (`QONTINUI_SERVER_MODE=1` or `=true`).
    pub server_mode: bool,

    /// Window placement hints (`QONTINUI_WINDOW_X/Y/WIDTH/HEIGHT/DECORATIONS`).
    pub window: WindowEnvHints,

    /// Override for the panic-log directory (`QONTINUI_PANIC_LOG_DIR`).
    /// Note: `startup_panic.rs` historically reads `QONTINUI_RUNNER_LOG_DIR`
    /// for the same purpose — both are accepted; the runner-log-dir wins.
    pub panic_log_dir: Option<PathBuf>,

    /// Per-runner log dir set by the supervisor (`QONTINUI_RUNNER_LOG_DIR`).
    pub runner_log_dir: Option<PathBuf>,

    /// Isolated WebView2 user-data folder (`WEBVIEW2_USER_DATA_FOLDER`),
    /// set by the supervisor for secondaries to avoid profile-lock contention.
    pub webview2_user_data_dir: Option<PathBuf>,

    /// Restate URL overrides forwarded by the supervisor.
    pub restate: RestateEnvHints,
}

impl Default for RunnerLaunchEnv {
    fn default() -> Self {
        Self {
            kind: RunnerKind::Primary,
            instance_name: None,
            port: None,
            api_url: None,
            primary_port: None,
            server_mode: false,
            window: WindowEnvHints::default(),
            panic_log_dir: None,
            runner_log_dir: None,
            webview2_user_data_dir: None,
            restate: RestateEnvHints::default(),
        }
    }
}

/// The FIRST [`RunnerLaunchEnv::read`] of this process, and when it happened.
///
/// # Why this exists (plan `2026-08-20-effective-config-provenance-and-env-generation`, Phase 3)
///
/// "Read once in `main()`" is the property that makes this snapshot a
/// *generation*: every consumer pulls the typed value from here, so the runner
/// acts on the environment as it stood at boot for as long as it lives. A
/// config report that wants to say "your flag flip has not reached the runner
/// yet" has to compare that snapshot against a fresh read — and it cannot,
/// because the snapshot lives on Tauri app state, reachable only from a command
/// that threads `State<SharedLaunchEnv>` through every caller.
///
/// A `OnceLock` set by `read()` itself is the smaller mechanism: the FIRST call
/// is `main`'s, so this captures exactly the value `main` stored, and every
/// later call (the report's re-read) leaves it untouched. Nothing about the
/// existing Tauri-state path changes.
///
/// It is `None` in any process that never called `read()` — a test binary, or
/// the headless `config_report` bin, which does not link this module at all.
/// The report renders that as UNKNOWN, never as "no drift".
static FIRST_READ: OnceLock<(RunnerLaunchEnv, DateTime<Utc>)> = OnceLock::new();

/// The launch snapshot `main()` took, with its capture time — or `None` if
/// [`RunnerLaunchEnv::read`] has never run in this process.
///
/// Deliberately returns the STORED value rather than re-reading: the whole
/// point of the accessor is to expose the generation, and a helpful re-read
/// here would silently make every staleness check pass.
pub fn first_read() -> Option<&'static (RunnerLaunchEnv, DateTime<Utc>)> {
    FIRST_READ.get()
}

impl RunnerLaunchEnv {
    /// Read every relevant env var from the current process environment.
    ///
    /// Called exactly once in `main.rs::main()`. Uses `crate::instance::*`
    /// helpers as the canonical parsing point for instance-related vars
    /// so there is still a single source of truth per var.
    ///
    /// The first call in a process is also recorded in [`FIRST_READ`] so the
    /// config report can compare the launch generation against a later re-read
    /// (see that static's docs). Recording is `set`-once and side-effect free
    /// for every caller.
    pub fn read() -> Self {
        let value = Self::read_uncached();
        let _ = FIRST_READ.set((value.clone(), Utc::now()));
        value
    }

    /// The read itself, with no snapshot recording — so the config report can
    /// take a FRESH reading to compare against the launch snapshot without
    /// racing to become the launch snapshot itself.
    pub fn read_uncached() -> Self {
        let instance_name = crate::instance::instance_name();
        let primary_port = crate::instance::primary_port();
        let kind = crate::instance::runner_kind();

        let port = std::env::var("QONTINUI_PORT")
            .ok()
            .and_then(|s| s.parse().ok());
        let api_url = std::env::var("QONTINUI_API_URL")
            .ok()
            .filter(|s| !s.is_empty());
        let server_mode = std::env::var("QONTINUI_SERVER_MODE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let window = WindowEnvHints {
            x: parse_env::<i32>("QONTINUI_WINDOW_X"),
            y: parse_env::<i32>("QONTINUI_WINDOW_Y"),
            width: parse_env::<u32>("QONTINUI_WINDOW_WIDTH"),
            height: parse_env::<u32>("QONTINUI_WINDOW_HEIGHT"),
            decorations: std::env::var("QONTINUI_WINDOW_DECORATIONS")
                .ok()
                .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false"))),
        };

        let panic_log_dir = std::env::var("QONTINUI_PANIC_LOG_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let runner_log_dir = std::env::var("QONTINUI_RUNNER_LOG_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        // Delegate to crate::instance::webview2_data_dir() so this field
        // honors the env-var-or-fallback resolution from Item 5: prefer
        // WEBVIEW2_USER_DATA_FOLDER (set by the supervisor) and fall back
        // to qontinui_types::wire::webview2_data_dir for standalone
        // launches. Returns None on non-Windows.
        let webview2_user_data_dir = crate::instance::webview2_data_dir();

        let restate = RestateEnvHints {
            external_admin_url: std::env::var("QONTINUI_RESTATE_EXTERNAL_ADMIN_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            external_ingress_url: std::env::var("QONTINUI_RESTATE_EXTERNAL_INGRESS_URL")
                .ok()
                .filter(|s| !s.is_empty()),
        };

        Self {
            kind,
            instance_name,
            port,
            api_url,
            primary_port,
            server_mode,
            window,
            panic_log_dir,
            runner_log_dir,
            webview2_user_data_dir,
            restate,
        }
    }

    /// True when this runner is a non-primary instance (any `RunnerKind`
    /// other than `Primary`). Convenience over matching the enum.
    pub fn is_secondary(&self) -> bool {
        !matches!(self.kind, RunnerKind::Primary)
    }
}

/// Type alias used on Tauri app state. Stored as `Arc` so handlers can
/// hold a cheap clone across `await` boundaries without re-parsing.
pub type SharedLaunchEnv = Arc<RunnerLaunchEnv>;

fn parse_env<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_primary_with_no_overrides() {
        let env = RunnerLaunchEnv::default();
        assert_eq!(env.kind, RunnerKind::Primary);
        assert!(env.instance_name.is_none());
        assert!(env.port.is_none());
        assert!(!env.server_mode);
        assert!(!env.is_secondary());
        assert_eq!(env.window, WindowEnvHints::default());
    }

    #[test]
    fn constructed_named_runner_is_secondary() {
        let env = RunnerLaunchEnv {
            kind: RunnerKind::Named {
                name: "test-1".to_string(),
            },
            instance_name: Some("test-1".to_string()),
            ..Default::default()
        };
        assert!(env.is_secondary());
    }

    #[test]
    fn window_hints_round_trip() {
        let hints = WindowEnvHints {
            x: Some(100),
            y: Some(200),
            width: Some(1400),
            height: Some(800),
            decorations: Some(false),
        };
        let env = RunnerLaunchEnv {
            window: hints.clone(),
            ..Default::default()
        };
        assert_eq!(env.window, hints);
    }

    #[test]
    fn restate_hints_default_empty() {
        let env = RunnerLaunchEnv::default();
        assert!(env.restate.external_admin_url.is_none());
        assert!(env.restate.external_ingress_url.is_none());
    }

    /// Regression: `RunnerLaunchEnv::read()` must succeed even with no env
    /// vars set (i.e. produce a default-equivalent instance for the primary
    /// runner). This is the codepath every non-test process hits.
    #[test]
    fn read_in_clean_env_does_not_panic() {
        // We can't easily clear env in a parallel test environment, so we
        // just call read() and assert basic invariants. The unit value of
        // this test is "doesn't panic" — full env-driven tests would need
        // serial_test, which is exactly what `RunnerLaunchEnv` is meant to
        // make unnecessary at consumer sites.
        let env = RunnerLaunchEnv::read();
        let _ = env.is_secondary();
        let _ = env.kind;
    }

    /// The launch snapshot is recorded on the FIRST `read()` and never moves —
    /// which is the property that makes it a *generation* the config report can
    /// compare a fresh read against. `read_uncached()` must agree on the same
    /// env while recording nothing.
    ///
    /// Order-independent by construction: whichever test in this binary calls
    /// `read()` first sets the lock, and every reading here is taken from the
    /// same (unchanged) process env, so the assertions hold either way.
    #[test]
    fn first_read_records_the_launch_generation_and_never_moves() {
        let at_launch = RunnerLaunchEnv::read();
        let (snapshot, first_ts) = first_read()
            .expect("read() must record the first call")
            .clone();
        assert_eq!(snapshot, at_launch);

        // A second read leaves the recorded generation alone — otherwise the
        // report would compare the snapshot against itself and every staleness
        // check would pass vacuously.
        let _ = RunnerLaunchEnv::read();
        let (again, ts_again) = first_read().expect("still recorded");
        assert_eq!(*again, snapshot);
        assert_eq!(*ts_again, first_ts);

        // The uncached read is the same parser, recording nothing.
        assert_eq!(RunnerLaunchEnv::read_uncached(), at_launch);
        assert_eq!(first_read().expect("unchanged").1, first_ts);
    }
}
