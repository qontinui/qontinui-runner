//! How far this process's boot has got on the road to binding the HTTP
//! listener — a process-global the health monitor's OS thread can read.
//!
//! Plan `2026-11-04-runner-wedge-detector-reads-a-boot-as-a-wedge-and-never-bound-is-silent`,
//! Phase 1. The health monitor starts at `main.rs` `run_app` long before the
//! listener binds, and everything in between (the Postgres arm, the embedded
//! bootstrap, the dev-app bootstrap, the Tauri builder and `.setup`, the main
//! window, the router build) can delay the bind. When `/livez` has never
//! answered, the one question a breadcrumb must answer is "where did the boot
//! stop?", and before this module nothing the monitor could reach knew.
//!
//! # Furthest stage, not latest stamp
//!
//! The API server runs on the async runtime, concurrently with the rest of
//! `.setup`, so stamps do not arrive in a single order. [`stamp`] therefore
//! records the FURTHEST stage reached (`fetch_max` over the declaration order
//! below), never the latest one written: `.setup` completing after the router
//! build began must not hide that the router build is where the bind stalled.
//! The declaration order is the order of progress toward the bind, which is why
//! [`BootStage::SetupComplete`] sits between the server spawn and the router
//! build — setup returning is later than the spawn, but it is not on the
//! bind's own path.
//!
//! Every stamp also records its first-reached instant (milliseconds since the
//! health monitor's process clock), so latencies between stages can be logged
//! without a second clock, and logs one line — the boot already logged most of
//! these points, but not as one greppable family with a common time base.

use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::time::Duration;

use tracing::info;

/// One point on the road from process start to a bound listener, in order of
/// progress toward the bind. `#[repr(u8)]` so the furthest stage fits one
/// lock-free atomic the monitor thread can read while every other thread is
/// stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum BootStage {
    /// `main()` entered. Also anchors the process clock.
    ProcessStart = 0,
    /// `health_monitor::start_health_monitor()` called (`run_app`).
    HealthMonitorStarted,
    /// `select_db_arm` entered — choosing external vs embedded Postgres.
    DatabaseArmSelect,
    /// The bundled embedded-Postgres bootstrap (`boot_embedded_pg`).
    EmbeddedPostgresBootstrap,
    /// Connecting to the configured (external) Postgres.
    ConfiguredPostgresConnect,
    /// `bootstrap_dev_apps` and `online_learning::initialize`.
    DevAppsAndOnlineLearning,
    /// The Tauri builder is being assembled.
    TauriBuilder,
    /// The `.setup` closure has been entered.
    TauriSetup,
    /// The global `AppHandle` is set — from here a toast can be raised.
    AppHandleSet,
    /// `webview_recovery::build_main_window` is about to run.
    MainWindowBuild,
    /// The MCP API server task is about to be spawned.
    ApiServerSpawn,
    /// `.setup` returned (`Tauri application setup complete`).
    SetupComplete,
    /// `mcp_api::start_server` is building the router.
    ApiRouterBuild,
    /// `mcp_api::start_server` has entered its bind loop.
    ApiBindLoop,
    /// The listener is bound (`set_bound_port` ran).
    ApiListenerBound,
}

impl BootStage {
    /// Every stage, in declaration order. The index of a stage here is its
    /// discriminant — pinned by a unit test.
    const ALL: [BootStage; 15] = [
        BootStage::ProcessStart,
        BootStage::HealthMonitorStarted,
        BootStage::DatabaseArmSelect,
        BootStage::EmbeddedPostgresBootstrap,
        BootStage::ConfiguredPostgresConnect,
        BootStage::DevAppsAndOnlineLearning,
        BootStage::TauriBuilder,
        BootStage::TauriSetup,
        BootStage::AppHandleSet,
        BootStage::MainWindowBuild,
        BootStage::ApiServerSpawn,
        BootStage::SetupComplete,
        BootStage::ApiRouterBuild,
        BootStage::ApiBindLoop,
        BootStage::ApiListenerBound,
    ];

    /// Stable, greppable token written into logs and `wedge-incidents.log`.
    /// Do not reword: a reader greps these after the fact.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BootStage::ProcessStart => "process_start",
            BootStage::HealthMonitorStarted => "health_monitor_started",
            BootStage::DatabaseArmSelect => "database_arm_select",
            BootStage::EmbeddedPostgresBootstrap => "embedded_postgres_bootstrap",
            BootStage::ConfiguredPostgresConnect => "configured_postgres_connect",
            BootStage::DevAppsAndOnlineLearning => "dev_apps_and_online_learning",
            BootStage::TauriBuilder => "tauri_builder",
            BootStage::TauriSetup => "tauri_setup",
            BootStage::AppHandleSet => "app_handle_set",
            BootStage::MainWindowBuild => "main_window_build",
            BootStage::ApiServerSpawn => "api_server_spawn",
            BootStage::SetupComplete => "setup_complete",
            BootStage::ApiRouterBuild => "api_router_build",
            BootStage::ApiBindLoop => "api_bind_loop",
            BootStage::ApiListenerBound => "api_listener_bound",
        }
    }

    fn from_u8(raw: u8) -> BootStage {
        Self::ALL
            .get(raw as usize)
            .copied()
            .unwrap_or(BootStage::ProcessStart)
    }
}

impl std::fmt::Display for BootStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The furthest stage reached. `0` = [`BootStage::ProcessStart`].
static FURTHEST: AtomicU8 = AtomicU8::new(0);

/// Per-stage first-reached time, ms since the process clock's epoch, floored
/// at 1. `0` = never reached.
static REACHED_AT_MS: [AtomicI64; BootStage::ALL.len()] =
    [const { AtomicI64::new(0) }; BootStage::ALL.len()];

/// Record that the boot reached `stage`, and log it once with its offset from
/// process start. Lock-free and idempotent: a second stamp of the same stage
/// keeps the first instant and logs nothing.
pub(crate) fn stamp(stage: BootStage) {
    let ms = since_process_start_ms(crate::health_monitor::process_elapsed());
    let first = REACHED_AT_MS[stage as usize]
        .compare_exchange(0, ms, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok();
    FURTHEST.fetch_max(stage as u8, Ordering::SeqCst);
    if first {
        info!(
            boot_stage = stage.as_str(),
            since_process_start_ms = ms,
            "boot stage reached: {stage} (+{ms} ms since process start)"
        );
    }
}

/// The furthest stage this boot has reached.
pub(crate) fn furthest() -> BootStage {
    BootStage::from_u8(FURTHEST.load(Ordering::SeqCst))
}

/// When `stage` was first reached, as an offset from process start; `None`
/// when it has not been reached.
pub(crate) fn reached_at(stage: BootStage) -> Option<Duration> {
    match REACHED_AT_MS[stage as usize].load(Ordering::SeqCst) {
        0 => None,
        ms => Some(Duration::from_millis(ms as u64)),
    }
}

/// Milliseconds for a stamp, floored at 1 so a real stamp is never the `0`
/// "never reached" sentinel.
fn since_process_start_ms(elapsed: Duration) -> i64 {
    (elapsed.as_millis().min(i64::MAX as u128) as i64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_table_is_indexed_by_discriminant() {
        for (i, stage) in BootStage::ALL.iter().enumerate() {
            assert_eq!(*stage as usize, i, "{stage} is out of place in ALL");
            assert_eq!(BootStage::from_u8(i as u8), *stage);
        }
    }

    #[test]
    fn the_tokens_are_pinned() {
        assert_eq!(BootStage::DatabaseArmSelect.as_str(), "database_arm_select");
        assert_eq!(BootStage::ApiRouterBuild.as_str(), "api_router_build");
        assert_eq!(BootStage::ApiListenerBound.as_str(), "api_listener_bound");
        assert_eq!(BootStage::SetupComplete.to_string(), "setup_complete");
    }

    /// The router build is concurrent with the tail of `.setup`; setup
    /// completing afterwards must not mask where the bind stalled.
    #[test]
    fn the_furthest_stage_wins_over_the_latest_stamp() {
        assert!(BootStage::ApiRouterBuild > BootStage::SetupComplete);
        assert!(BootStage::SetupComplete > BootStage::ApiServerSpawn);
        assert!(BootStage::ApiListenerBound > BootStage::ApiBindLoop);
        // Process-global, so only assert monotonicity of what this test adds.
        stamp(BootStage::ApiRouterBuild);
        stamp(BootStage::SetupComplete);
        assert!(furthest() >= BootStage::ApiRouterBuild);
        assert!(reached_at(BootStage::ApiRouterBuild).is_some());
    }

    #[test]
    fn a_stamp_is_never_the_unreached_sentinel() {
        assert_eq!(since_process_start_ms(Duration::ZERO), 1);
        assert_eq!(since_process_start_ms(Duration::from_millis(1490)), 1490);
    }
}
