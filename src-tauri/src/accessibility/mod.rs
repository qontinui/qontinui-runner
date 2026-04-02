//! Cross-platform desktop accessibility API layer.
//!
//! Provides Rust-native accessibility tree capture, querying, and interaction
//! for Windows (UIA), Linux (AT-SPI), and macOS (AX).
//! Replaces the Python HAL accessibility backends with a faster, more capable
//! Rust implementation.
//!
//! # Architecture
//!
//! ```text
//!                     Tauri Commands / MCP API
//!                             |
//!                     [AccessibilityManager]
//!                       |           |
//!               [QueryEngine]  [InteractionEngine]
//!                       |           |
//!                   [TreeCache + EventBus]
//!                       |
//!                   [FusionEngine]
//!                       |
//!               [NativeAdapter]
//!                       |
//!             +---------+---------+
//!             |         |         |
//!           [UIA]   [AT-SPI]   [AX]
//! ```

pub mod adapters;
pub mod cache;
pub mod events;
pub mod fusion;
#[cfg(test)]
mod integration_test;
pub mod interaction;
pub mod model;
pub mod query;
pub mod ref_manager;
pub mod traits;

use tracing::{debug, info, warn};

use self::adapters::create_platform_adapter;
use self::cache::TreeCache;
use self::events::{A11yEvent, EventReceiver};
use self::fusion::{FusionConfig, FusionEngine};
use self::interaction::InteractionEngine;
use self::model::{NodeSource, UnifiedNode, UnifiedSnapshot};
use self::query::QueryBuilder;
use self::ref_manager::RefManager;
use self::traits::{ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter};

/// Top-level facade for the accessibility system.
///
/// Manages platform adapters, tree cache, ref assignment, and interaction
/// dispatch. Provides a unified API for Tauri commands and MCP tools.
pub struct AccessibilityManager {
    /// Platform-native adapter (UIA, AT-SPI, or AX depending on OS).
    native_adapter: Box<dyn PlatformAdapter>,

    /// Cached accessibility tree with handle index.
    cache: TreeCache,

    /// Ref assignment and persistence.
    ref_manager: RefManager,

    /// Fusion engine for merging trees (reserved for future use, e.g., UI Bridge data).
    fusion_engine: FusionEngine,

    /// Event sender for broadcasting accessibility events.
    event_tx: events::EventSender,
}

impl AccessibilityManager {
    /// Create a new AccessibilityManager with default configuration.
    pub fn new() -> Self {
        let (event_tx, _) = events::create_event_channel(256);
        let cache = TreeCache::new(event_tx.clone());

        let mut ref_manager = RefManager::new();
        if let Some(home) = dirs::home_dir() {
            ref_manager.set_persistence_dir(home.join(".qontinui").join("a11y_refs"));
        }

        Self {
            native_adapter: create_platform_adapter(),
            cache,
            ref_manager,
            fusion_engine: FusionEngine::new(FusionConfig::default()),
            event_tx,
        }
    }

    /// Connect to an accessibility source.
    pub async fn connect(
        &mut self,
        target: ConnectionTarget,
        timeout_ms: u64,
    ) -> anyhow::Result<()> {
        info!(
            "Connecting to accessibility source via {}",
            self.native_adapter.backend_name()
        );
        self.native_adapter.connect(target, timeout_ms).await?;

        let _ = self.event_tx.send(A11yEvent::ConnectionChanged {
            connected: true,
            backend: self.native_adapter.backend_name().to_string(),
        });

        Ok(())
    }

    /// Disconnect from all sources.
    pub async fn disconnect(&mut self) -> anyhow::Result<()> {
        // Save refs before disconnecting
        if self.ref_manager.count() > 0 {
            let backend = self.native_adapter.backend_name();
            if let Err(e) = self.ref_manager.save(backend) {
                debug!("Failed to persist refs: {}", e);
            }
        }

        self.native_adapter.disconnect().await?;
        self.cache.clear().await;
        self.ref_manager.reset();

        let _ = self.event_tx.send(A11yEvent::ConnectionChanged {
            connected: false,
            backend: self.native_adapter.backend_name().to_string(),
        });

        info!("Disconnected from accessibility source");
        Ok(())
    }

    /// Whether connected to an accessibility source.
    pub fn is_connected(&self) -> bool {
        self.native_adapter.is_connected()
    }

    /// Capture the accessibility tree, assign refs, and update the cache.
    pub async fn capture(
        &mut self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedSnapshot> {
        // Capture native tree
        let mut native_tree = self
            .native_adapter
            .capture_tree(max_depth, include_hidden)
            .await?;

        let source = self.native_adapter_source();

        // Assign refs
        self.ref_manager
            .assign_refs_to_tree(&mut native_tree, false);

        // Build snapshot
        let generation = self.cache.next_generation();
        let snapshot = UnifiedSnapshot::from_root(
            native_tree,
            source,
            None, // URL populated by adapter when available
            None, // Title populated by adapter
            generation,
        );

        // Update cache
        self.cache.replace(snapshot.clone()).await;

        let _ = self.event_tx.send(A11yEvent::TreeReplaced {
            generation,
            total_nodes: snapshot.total_nodes,
        });

        info!(
            "Captured accessibility tree: {} nodes ({} interactive), generation {}",
            snapshot.total_nodes, snapshot.interactive_nodes, generation
        );

        Ok(snapshot)
    }

    /// Get the NodeSource for the native adapter.
    fn native_adapter_source(&self) -> NodeSource {
        match self.native_adapter.backend_name() {
            "uia" => NodeSource::Uia,
            "atspi" => NodeSource::Atspi,
            "ax" => NodeSource::Ax,
            _ => NodeSource::Uia,
        }
    }

    /// Create a query builder for searching the cached tree.
    pub fn query(&self) -> QueryBuilder {
        QueryBuilder::new()
    }

    /// Click an element by ref ID.
    pub async fn click(&self, ref_id: &str) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::click(&node, self.native_adapter.as_ref()).await
    }

    /// Type text into an element by ref ID.
    pub async fn type_text(
        &self,
        ref_id: &str,
        text: &str,
        clear_first: bool,
    ) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::type_text(&node, text, clear_first, self.native_adapter.as_ref()).await
    }

    /// Focus an element by ref ID.
    pub async fn focus(&self, ref_id: &str) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::focus(&node, self.native_adapter.as_ref()).await
    }

    /// Subscribe to accessibility events.
    pub fn subscribe(&self) -> EventReceiver {
        self.event_tx.subscribe()
    }

    /// Generate an AI-friendly text representation of the current tree.
    ///
    /// Matches the output format of the Python `IAccessibilityCapture.to_ai_context()`.
    pub async fn to_ai_context(&self, max_elements: usize, interactive_only: bool) -> String {
        let snapshot = match self.cache.snapshot().await {
            Some(s) => s,
            None => return "## Accessibility Tree\n\nNo tree captured.".to_string(),
        };

        let mut lines = Vec::new();
        lines.push("## Accessibility Tree".to_string());

        if let Some(url) = &snapshot.url {
            lines.push(format!("URL: {}", url));
        }
        if let Some(title) = &snapshot.title {
            lines.push(format!("Title: {}", title));
        }

        lines.push(String::new());
        lines.push("### Interactive Elements".to_string());

        let mut count = 0;

        fn walk(
            node: &UnifiedNode,
            interactive_only: bool,
            max_elements: usize,
            count: &mut usize,
            lines: &mut Vec<String>,
        ) {
            if *count >= max_elements {
                return;
            }

            if interactive_only && !node.is_interactive {
                for child in &node.children {
                    walk(child, interactive_only, max_elements, count, lines);
                }
                return;
            }

            let name_str = node
                .name
                .as_ref()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            let value_str = node
                .value
                .as_ref()
                .map(|v| format!(" = {}", v))
                .unwrap_or_default();

            let mut state_parts = Vec::new();
            if node.state.is_disabled {
                state_parts.push("disabled");
            }
            if node.state.is_focused {
                state_parts.push("focused");
            }
            if node.state.is_checked.is_true() {
                state_parts.push("checked");
            }
            if node.state.is_expanded.is_true() {
                state_parts.push("expanded");
            }

            let state_suffix = if state_parts.is_empty() {
                String::new()
            } else {
                format!(" [{}]", state_parts.join(", "))
            };

            lines.push(format!(
                "- {}: {}{}{}{}",
                node.ref_id,
                node.role.as_str(),
                name_str,
                value_str,
                state_suffix
            ));
            *count += 1;

            for child in &node.children {
                walk(child, interactive_only, max_elements, count, lines);
            }
        }

        walk(
            &snapshot.root,
            interactive_only,
            max_elements,
            &mut count,
            &mut lines,
        );

        if count >= max_elements {
            lines.push(format!("... (truncated, {} total)", snapshot.total_nodes));
        }

        lines.join("\n")
    }

    /// Get the current cached snapshot.
    pub async fn snapshot(&self) -> Option<UnifiedSnapshot> {
        self.cache.snapshot().await
    }

    /// Get the native adapter's backend name.
    pub fn backend_name(&self) -> &'static str {
        self.native_adapter.backend_name()
    }
}

impl Default for AccessibilityManager {
    fn default() -> Self {
        Self::new()
    }
}
