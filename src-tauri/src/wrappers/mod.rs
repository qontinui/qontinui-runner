//! Wrappers — install, manage, and dispatch to user-facing wrapper
//! subprocesses.
//!
//! Phase 1 of the wrapper-runner integration plan
//! (`C:/tmp/wrapper-runner-integration-plan.md`). The runner is the host;
//! wrappers are plugins exposing typed actions over the existing UI Bridge
//! relay.
//!
//! ## Module layout
//!
//! - [`manifest`] — types + parsing of the `qontinui.wrapper` package.json
//!   field and the `--manifest-only` stdout payload.
//! - [`registry`] — filesystem scan + JSON-file cache + `notify` watcher.
//! - [`manager`] — subprocess lifecycle (spawn / stop / health / idle reap).
//! - [`dispatch`] — `POST /wrappers/:id/dispatch` with per-(wrapper,
//!   action) serialization for `exclusive: true` actions.
//! - [`credentials`] — per-wrapper keyring namespace.
//! - [`install`] — `pnpm install` / update / uninstall pipeline.
//! - [`routes`] — single axum router that wires every Phase 1 HTTP route.
//!
//! ## State plumbing
//!
//! [`WrapperState`] bundles the shared registry / manager / dispatch
//! context so application bootstrap can construct it once and stash it on
//! `commands::AppState`.

pub mod clients;
pub mod credentials;
pub mod dispatch;
pub mod events;
pub mod install;
pub mod launcher;
pub mod manager;
pub mod manifest;
pub mod primary_proxy;
pub mod registry;
pub mod routes;

use std::sync::Arc;

/// Run `f` with every `tracing` event (TRACE and up) captured on this
/// thread, and return what was logged (tests only): proves what a log line
/// does — and does not — carry.
#[cfg(test)]
pub(crate) fn capture_logs<F: FnOnce()>(f: F) -> String {
    #[derive(Clone)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Capture(Arc::default());
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.0.lock().unwrap().clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub use dispatch::DispatchContext;
pub use manager::WrapperManager;
pub use registry::WrapperRegistry;

/// Top-level state for the wrappers subsystem. Construct once at runner
/// startup and store on `AppState::wrapper_dispatch`.
pub struct WrapperState {
    pub registry: Arc<WrapperRegistry>,
    pub manager: Arc<WrapperManager>,
    pub dispatch: Arc<DispatchContext>,
}

impl WrapperState {
    /// Build the full wrapper subsystem rooted at the platform-default
    /// `<APP_DATA>/qontinui-runner/wrappers/` directory.
    ///
    /// Triggers an initial filesystem rescan, starts the file watcher,
    /// spawns the health-check loop, and spawns the idle reaper.
    pub async fn new_default() -> Arc<Self> {
        let registry = WrapperRegistry::new(WrapperRegistry::default_root()).await;
        registry.start_watcher();
        let manager = WrapperManager::new(registry.clone());
        manager.spawn_health_loop();
        manager.spawn_idle_reaper();
        let dispatch = DispatchContext::new(registry.clone(), manager.clone());
        Arc::new(Self {
            registry,
            manager,
            dispatch,
        })
    }
}
