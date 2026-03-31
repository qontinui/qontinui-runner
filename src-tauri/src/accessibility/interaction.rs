//! Interaction engine for accessibility elements.
//!
//! Dispatches user actions (click, type, focus) through the best available
//! native interaction pattern, falling back to coordinate-based input when
//! no native pattern is available.

use tracing::{debug, warn};

use super::model::{InteractionPattern, NodeSource, UnifiedNode};
use super::traits::{InteractionParams, InteractionResult, PlatformAdapter};

/// Dispatches interactions to the appropriate adapter and pattern.
pub struct InteractionEngine;

impl InteractionEngine {
    /// Choose the best interaction pattern for a click action.
    pub fn best_click_pattern(node: &UnifiedNode) -> InteractionPattern {
        let patterns = &node.supported_patterns;

        // Prefer native invoke pattern
        if patterns.contains(&InteractionPattern::Invoke) {
            return InteractionPattern::Invoke;
        }

        // For toggle elements, use toggle pattern
        if patterns.contains(&InteractionPattern::Toggle) {
            return InteractionPattern::Toggle;
        }

        // Fallback to coordinate click
        InteractionPattern::CoordinateClick
    }

    /// Choose the best interaction pattern for typing text.
    pub fn best_type_pattern(node: &UnifiedNode) -> InteractionPattern {
        let patterns = &node.supported_patterns;

        // Prefer native value pattern
        if patterns.contains(&InteractionPattern::Value) {
            return InteractionPattern::Value;
        }

        // Fallback: click to focus + keyboard simulation
        InteractionPattern::CoordinateClick
    }

    /// Choose the best interaction pattern for focusing.
    pub fn best_focus_pattern(node: &UnifiedNode) -> InteractionPattern {
        // Focus is typically done via click at coordinates or native invoke
        if node.supported_patterns.contains(&InteractionPattern::Invoke) {
            InteractionPattern::Invoke
        } else {
            InteractionPattern::CoordinateClick
        }
    }

    /// Execute a click on a node via the appropriate adapter.
    pub async fn click(
        node: &UnifiedNode,
        adapter: &dyn PlatformAdapter,
    ) -> anyhow::Result<InteractionResult> {
        let pattern = Self::best_click_pattern(node);
        Self::dispatch(node, pattern, InteractionParams::None, adapter).await
    }

    /// Execute typing into a node via the appropriate adapter.
    pub async fn type_text(
        node: &UnifiedNode,
        text: &str,
        clear_first: bool,
        adapter: &dyn PlatformAdapter,
    ) -> anyhow::Result<InteractionResult> {
        let pattern = Self::best_type_pattern(node);
        let params = InteractionParams::Text {
            value: text.to_string(),
            clear_first,
        };
        Self::dispatch(node, pattern, params, adapter).await
    }

    /// Execute focus on a node via the appropriate adapter.
    pub async fn focus(
        node: &UnifiedNode,
        adapter: &dyn PlatformAdapter,
    ) -> anyhow::Result<InteractionResult> {
        let pattern = Self::best_focus_pattern(node);
        Self::dispatch(node, pattern, InteractionParams::None, adapter).await
    }

    /// Route an interaction to the native adapter.
    async fn dispatch(
        node: &UnifiedNode,
        pattern: InteractionPattern,
        params: InteractionParams,
        adapter: &dyn PlatformAdapter,
    ) -> anyhow::Result<InteractionResult> {
        let handle = node.platform_handle.ok_or_else(|| {
            anyhow::anyhow!(
                "Node {} has no platform handle for interaction",
                node.ref_id
            )
        })?;

        debug!(
            "Dispatching {:?} on {} ({:?}) via {}",
            pattern,
            node.ref_id,
            node.source,
            adapter.backend_name()
        );

        adapter.interact(handle, pattern, params).await
    }
}
