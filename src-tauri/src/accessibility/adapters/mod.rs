//! Platform-specific accessibility adapter implementations.
//!
//! Each adapter is gated behind a `cfg` flag so only the relevant platform
//! code is compiled. The `create_adapter` function returns the appropriate
//! adapter for the current platform.

use super::traits::PlatformAdapter;

// Platform-specific adapter modules (stubs for Phase 1).
// Full implementations added in Phases 2-6.

#[cfg(windows)]
pub mod uia;

#[cfg(windows)]
pub mod jab;

#[cfg(target_os = "linux")]
pub mod atspi_adapter;

#[cfg(target_os = "macos")]
pub mod ax;

/// Stub adapter that returns errors for all operations.
///
/// Used as a placeholder until platform-specific adapters are implemented.
pub struct StubAdapter {
    backend: &'static str,
}

impl StubAdapter {
    pub fn new(backend: &'static str) -> Self {
        Self { backend }
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for StubAdapter {
    fn backend_name(&self) -> &'static str {
        self.backend
    }

    async fn connect(
        &mut self,
        _target: super::traits::ConnectionTarget,
        _timeout_ms: u64,
    ) -> anyhow::Result<()> {
        anyhow::bail!("{} adapter not yet implemented (Phase 2-6)", self.backend)
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_connected(&self) -> bool {
        false
    }

    async fn capture_tree(
        &self,
        _max_depth: Option<u32>,
        _include_hidden: bool,
    ) -> anyhow::Result<super::model::UnifiedNode> {
        anyhow::bail!("{} adapter not yet implemented (Phase 2-6)", self.backend)
    }

    async fn subscribe_events(
        &self,
    ) -> anyhow::Result<Option<tokio::sync::mpsc::Receiver<super::events::A11yEvent>>> {
        Ok(None)
    }

    async fn interact(
        &self,
        _platform_handle: u64,
        pattern: super::model::InteractionPattern,
        _params: super::traits::InteractionParams,
    ) -> anyhow::Result<super::traits::InteractionResult> {
        Ok(super::traits::InteractionResult::err(
            pattern,
            format!("{} adapter not yet implemented", self.backend),
        ))
    }

    async fn supported_patterns(
        &self,
        _platform_handle: u64,
    ) -> Vec<super::model::InteractionPattern> {
        vec![]
    }
}

/// Create the appropriate platform adapter for the current OS.
///
/// Returns a stub adapter until platform-specific implementations are added.
pub fn create_platform_adapter() -> Box<dyn PlatformAdapter> {
    #[cfg(windows)]
    {
        Box::new(uia::UiaAdapter::new())
    }

    #[cfg(target_os = "linux")]
    {
        Box::new(atspi_adapter::AtspiAdapter::new())
    }

    #[cfg(target_os = "macos")]
    {
        Box::new(ax::AxAdapter::new())
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Box::new(StubAdapter::new("unsupported"))
    }
}
