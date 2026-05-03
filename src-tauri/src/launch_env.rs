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
use std::sync::Arc;

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

/// Test auto-login credentials, populated for temp test runners only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestAutoLogin {
    pub email: String,
    pub password: String,
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

    /// Test auto-login credentials (`QONTINUI_TEST_AUTO_LOGIN_EMAIL/PASSWORD`).
    /// Both must be set and non-empty for `Some` to be returned.
    pub auto_login: Option<TestAutoLogin>,

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
            auto_login: None,
            panic_log_dir: None,
            runner_log_dir: None,
            webview2_user_data_dir: None,
            restate: RestateEnvHints::default(),
        }
    }
}

impl RunnerLaunchEnv {
    /// Read every relevant env var from the current process environment.
    ///
    /// Called exactly once in `main.rs::main()`. Uses `crate::instance::*`
    /// helpers as the canonical parsing point for instance-related vars
    /// so there is still a single source of truth per var.
    pub fn read() -> Self {
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

        let auto_login = match (
            std::env::var("QONTINUI_TEST_AUTO_LOGIN_EMAIL").ok(),
            std::env::var("QONTINUI_TEST_AUTO_LOGIN_PASSWORD").ok(),
        ) {
            (Some(email), Some(password)) if !email.is_empty() && !password.is_empty() => {
                Some(TestAutoLogin { email, password })
            }
            _ => None,
        };

        let panic_log_dir = std::env::var("QONTINUI_PANIC_LOG_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let runner_log_dir = std::env::var("QONTINUI_RUNNER_LOG_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let webview2_user_data_dir = std::env::var("WEBVIEW2_USER_DATA_FOLDER")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

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
            auto_login,
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
        assert!(env.auto_login.is_none());
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
    fn auto_login_struct_construction() {
        let creds = TestAutoLogin {
            email: "test@example.com".into(),
            password: "secret".into(),
        };
        let env = RunnerLaunchEnv {
            auto_login: Some(creds.clone()),
            ..Default::default()
        };
        assert_eq!(env.auto_login.as_ref().unwrap(), &creds);
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
}
