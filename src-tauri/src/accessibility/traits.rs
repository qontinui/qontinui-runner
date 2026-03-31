//! Platform adapter trait and shared types.
//!
//! Defines the contract that each platform-specific accessibility backend must
//! implement. The `PlatformAdapter` trait provides a uniform interface for
//! connecting, capturing trees, subscribing to events, and interacting with
//! elements across Windows UIA, Linux AT-SPI, and macOS AX.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::events::A11yEvent;
use super::model::{InteractionPattern, UnifiedNode};

/// Target specification for connecting to an accessibility source.
#[derive(Debug, Clone)]
pub enum ConnectionTarget {
    /// Match by window title (partial, case-insensitive).
    WindowTitle(String),
    /// Match by process ID.
    ProcessId(u32),
    /// Connect to the desktop root (all windows).
    Desktop,
}

/// Parameters for interaction commands.
#[derive(Debug, Clone)]
pub enum InteractionParams {
    /// No additional parameters (for Invoke, Toggle, CoordinateClick).
    None,
    /// Text input parameters (for Value).
    Text {
        value: String,
        clear_first: bool,
    },
    /// Scroll direction (for Scroll).
    ScrollDirection {
        horizontal: i32,
        vertical: i32,
    },
    /// Numeric value (for RangeValue, e.g. slider position).
    RangeValue {
        value: f64,
    },
    /// Expand or collapse (for ExpandCollapse).
    ExpandCollapse {
        expand: bool,
    },
}

/// Result of an interaction attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionResult {
    /// Whether the interaction succeeded.
    pub success: bool,
    /// Error message if the interaction failed.
    pub error: Option<String>,
    /// Which pattern was actually used.
    pub pattern_used: InteractionPattern,
}

impl InteractionResult {
    pub fn ok(pattern: InteractionPattern) -> Self {
        Self {
            success: true,
            error: None,
            pattern_used: pattern,
        }
    }

    pub fn err(pattern: InteractionPattern, msg: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(msg.into()),
            pattern_used: pattern,
        }
    }
}

/// Contract for platform-specific accessibility backends.
///
/// Each platform adapter (UIA, AT-SPI, AX) implements this trait.
/// The `AccessibilityManager` holds a `Box<dyn PlatformAdapter>` and
/// dispatches operations through it.
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Human-readable backend name (e.g., `"uia"`, `"atspi"`, `"ax"`).
    fn backend_name(&self) -> &'static str;

    /// Connect to an accessibility source.
    async fn connect(
        &mut self,
        target: ConnectionTarget,
        timeout_ms: u64,
    ) -> anyhow::Result<()>;

    /// Disconnect and release resources.
    async fn disconnect(&mut self) -> anyhow::Result<()>;

    /// Whether currently connected to a source.
    fn is_connected(&self) -> bool;

    /// Capture the full accessibility tree.
    ///
    /// The adapter normalizes platform-native roles and states to `UnifiedRole`
    /// and `UnifiedState`. Platform element handles are stored in `platform_handle`
    /// for later interaction dispatch.
    async fn capture_tree(
        &self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode>;

    /// Subscribe to accessibility events.
    ///
    /// Returns a channel receiver for events, or `None` if the platform does
    /// not support event streaming.
    async fn subscribe_events(
        &self,
    ) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>>;

    /// Perform a native interaction on an element.
    ///
    /// The `platform_handle` indexes into the adapter's internal handle table.
    async fn interact(
        &self,
        platform_handle: u64,
        pattern: InteractionPattern,
        params: InteractionParams,
    ) -> anyhow::Result<InteractionResult>;

    /// Get the interaction patterns supported by an element.
    async fn supported_patterns(
        &self,
        platform_handle: u64,
    ) -> Vec<InteractionPattern>;
}
