//! Fusion engine for merging accessibility trees from multiple sources.
//!
//! Reserved for future use (e.g., merging UI Bridge data with native tree).
//! The engine can detect webview containers in the native tree and graft
//! additional tree data into them.

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::model::{NodeSource, UnifiedNode, UnifiedRole};

/// Configuration for the fusion engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionConfig {
    /// Whether fusion is enabled.
    pub enabled: bool,

    /// Class name patterns used to locate the webview container in the native tree.
    /// Platform-specific defaults are provided.
    pub webview_class_patterns: Vec<String>,

    /// AT-SPI role patterns for locating webview on Linux.
    pub webview_role_patterns: Vec<String>,

    /// How to merge additional children into the native tree.
    pub merge_strategy: MergeStrategy,
}

/// Strategy for merging an additional tree into the native tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Replace webview node's children with merged tree children (default).
    Replace,
    /// Append merged tree children after native children.
    Append,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webview_class_patterns: default_webview_class_patterns(),
            webview_role_patterns: default_webview_role_patterns(),
            merge_strategy: MergeStrategy::Replace,
        }
    }
}

/// Default webview class name patterns per platform.
fn default_webview_class_patterns() -> Vec<String> {
    vec![
        // Windows (WebView2 / CEF)
        "Chrome_RenderWidgetHostHWND".to_string(),
        "Chrome_WidgetWin_1".to_string(),
        "Internet Explorer_Server".to_string(),
        // Linux (WebKitGTK / CEF)
        "GtkDrawingArea".to_string(),
        // macOS (WKWebView)
        "AXWebArea".to_string(),
    ]
}

/// Default AT-SPI role patterns for webview detection on Linux.
fn default_webview_role_patterns() -> Vec<String> {
    vec!["document frame".to_string(), "document web".to_string()]
}

/// Fusion engine for merging accessibility trees from multiple sources.
pub struct FusionEngine {
    config: FusionConfig,
}

impl FusionEngine {
    pub fn new(config: FusionConfig) -> Self {
        Self { config }
    }

    /// Check if fusion is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Merge an additional tree into a native tree.
    ///
    /// Locates the webview container in the native tree by class name or role
    /// heuristics, grafts additional children into it, and marks provenance.
    ///
    /// If no webview container is found or no additional tree is provided,
    /// returns the native tree unchanged.
    pub fn fuse(
        &self,
        mut native_tree: UnifiedNode,
        additional_tree: Option<UnifiedNode>,
    ) -> UnifiedNode {
        let additional_tree = match additional_tree {
            Some(tree) => tree,
            None => return native_tree,
        };

        // Try to find the webview container in the native tree
        let grafted = self.graft_into_native(&mut native_tree, additional_tree.clone());

        if grafted {
            debug!("Fusion: grafted additional tree into native webview container");
        } else {
            // No webview container found -- append additional tree as a sibling
            // at the top level.
            debug!("Fusion: no webview container found, appending additional tree as child");
            let mut additional_root = additional_tree;
            additional_root.source = NodeSource::Fused;
            native_tree.children.push(additional_root);
        }

        native_tree
    }

    /// Attempt to graft additional children into the webview container.
    ///
    /// Returns true if a webview container was found and grafted.
    fn graft_into_native(&self, node: &mut UnifiedNode, additional_tree: UnifiedNode) -> bool {
        // Check if this node IS the webview container
        if self.is_webview_container(node) {
            self.perform_graft(node, additional_tree);
            return true;
        }

        // Recursively search children
        let webview_idx = node
            .children
            .iter()
            .position(|child| self.has_webview_container(child));

        if let Some(idx) = webview_idx {
            return self.graft_into_native(&mut node.children[idx], additional_tree);
        }

        false
    }

    /// Check if a subtree contains a webview container (read-only check).
    fn has_webview_container(&self, node: &UnifiedNode) -> bool {
        self.find_webview_container(node).is_some()
    }

    /// Check if a node is a webview container by class name or role patterns.
    fn is_webview_container(&self, node: &UnifiedNode) -> bool {
        // Check class_name patterns
        if let Some(class) = &node.class_name {
            if self
                .config
                .webview_class_patterns
                .iter()
                .any(|p| class.contains(p.as_str()))
            {
                return true;
            }
        }

        // Check role-based patterns (for AT-SPI where role names identify webviews)
        if node.role == UnifiedRole::Document {
            if let Some(name) = &node.name {
                let lower = name.to_ascii_lowercase();
                if self
                    .config
                    .webview_role_patterns
                    .iter()
                    .any(|p| lower.contains(p.as_str()))
                {
                    return true;
                }
            }
        }

        false
    }

    /// Perform the actual graft: replace or append children into the container.
    fn perform_graft(&self, container: &mut UnifiedNode, additional_tree: UnifiedNode) {
        // Mark the container as fused
        container.source = NodeSource::Fused;

        let additional_children = additional_tree.children;

        match self.config.merge_strategy {
            MergeStrategy::Replace => {
                container.children = additional_children;
            }
            MergeStrategy::Append => {
                container.children.extend(additional_children);
            }
        }
    }

    /// Locate the webview container node in a native tree (read-only).
    pub fn find_webview_container<'a>(&self, node: &'a UnifiedNode) -> Option<&'a UnifiedNode> {
        if self.is_webview_container(node) {
            return Some(node);
        }
        for child in &node.children {
            if let Some(found) = self.find_webview_container(child) {
                return Some(found);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::model::*;

    fn make_node(
        role: UnifiedRole,
        name: &str,
        source: NodeSource,
        class_name: Option<&str>,
    ) -> UnifiedNode {
        UnifiedNode {
            ref_id: String::new(),
            role,
            name: Some(name.to_string()),
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: false,
            level: None,
            automation_id: None,
            class_name: class_name.map(|s| s.to_string()),
            html_tag: None,
            url: None,
            source,
            platform_handle: None,
            supported_patterns: vec![],
            generation: 0,
            children: vec![],
        }
    }

    #[test]
    fn test_fuse_with_webview_container() {
        let config = FusionConfig {
            enabled: true,
            ..Default::default()
        };
        let engine = FusionEngine::new(config);

        // Native tree: Window > [Titlebar, WebView container]
        let mut native = make_node(UnifiedRole::Window, "App", NodeSource::Uia, None);
        native.children.push(make_node(
            UnifiedRole::Titlebar,
            "App Title",
            NodeSource::Uia,
            None,
        ));
        let mut webview = make_node(
            UnifiedRole::Pane,
            "WebView",
            NodeSource::Uia,
            Some("Chrome_RenderWidgetHostHWND"),
        );
        webview.children.push(make_node(
            UnifiedRole::StaticText,
            "Native placeholder",
            NodeSource::Uia,
            None,
        ));
        native.children.push(webview);

        // Additional tree: Document > [Button, Textbox]
        let mut additional = make_node(UnifiedRole::Document, "Web Page", NodeSource::Uia, None);
        let mut btn = make_node(UnifiedRole::Button, "Submit", NodeSource::Uia, None);
        btn.is_interactive = true;
        additional.children.push(btn);
        additional.children.push(make_node(
            UnifiedRole::Textbox,
            "Email",
            NodeSource::Uia,
            None,
        ));

        let fused = engine.fuse(native, Some(additional));

        // Window still native
        assert_eq!(fused.source, NodeSource::Uia);
        assert_eq!(fused.children.len(), 2);

        // Titlebar unchanged
        assert_eq!(fused.children[0].source, NodeSource::Uia);
        assert_eq!(fused.children[0].name.as_deref(), Some("App Title"));

        // Webview container marked as Fused
        let webview_node = &fused.children[1];
        assert_eq!(webview_node.source, NodeSource::Fused);

        // Webview children replaced with additional content
        assert_eq!(webview_node.children.len(), 2);
        assert_eq!(webview_node.children[0].name.as_deref(), Some("Submit"));
        assert_eq!(webview_node.children[1].name.as_deref(), Some("Email"));
    }

    #[test]
    fn test_fuse_no_webview_container() {
        let config = FusionConfig {
            enabled: true,
            ..Default::default()
        };
        let engine = FusionEngine::new(config);

        // Native tree with no webview
        let mut native = make_node(UnifiedRole::Window, "Notepad", NodeSource::Uia, None);
        native.children.push(make_node(
            UnifiedRole::Edit,
            "Editor",
            NodeSource::Uia,
            None,
        ));

        // Additional tree
        let additional = make_node(UnifiedRole::Document, "Web", NodeSource::Uia, None);

        let fused = engine.fuse(native, Some(additional));

        // Additional tree appended as child since no webview found
        assert_eq!(fused.children.len(), 2);
        assert_eq!(fused.children[0].name.as_deref(), Some("Editor"));
        assert_eq!(fused.children[1].source, NodeSource::Fused);
        assert_eq!(fused.children[1].name.as_deref(), Some("Web"));
    }

    #[test]
    fn test_fuse_no_additional_tree() {
        let engine = FusionEngine::new(FusionConfig::default());

        let native = make_node(UnifiedRole::Window, "App", NodeSource::Uia, None);
        let fused = engine.fuse(native.clone(), None);

        // Should return native unchanged
        assert_eq!(fused.name, native.name);
        assert_eq!(fused.source, NodeSource::Uia);
    }

    #[test]
    fn test_find_webview_container() {
        let engine = FusionEngine::new(FusionConfig::default());

        let mut root = make_node(UnifiedRole::Window, "App", NodeSource::Uia, None);
        root.children.push(make_node(
            UnifiedRole::Pane,
            "Sidebar",
            NodeSource::Uia,
            Some("NavigationPane"),
        ));
        root.children.push(make_node(
            UnifiedRole::Pane,
            "Content",
            NodeSource::Uia,
            Some("Chrome_RenderWidgetHostHWND"),
        ));

        let container = engine.find_webview_container(&root);
        assert!(container.is_some());
        assert_eq!(container.unwrap().name.as_deref(), Some("Content"));
    }

    #[test]
    fn test_append_merge_strategy() {
        let config = FusionConfig {
            enabled: true,
            merge_strategy: MergeStrategy::Append,
            ..Default::default()
        };
        let engine = FusionEngine::new(config);

        let mut native = make_node(UnifiedRole::Window, "App", NodeSource::Uia, None);
        let mut webview = make_node(
            UnifiedRole::Pane,
            "WebView",
            NodeSource::Uia,
            Some("Chrome_RenderWidgetHostHWND"),
        );
        webview.children.push(make_node(
            UnifiedRole::StaticText,
            "Native child",
            NodeSource::Uia,
            None,
        ));
        native.children.push(webview);

        let mut additional = make_node(UnifiedRole::Document, "Page", NodeSource::Uia, None);
        additional.children.push(make_node(
            UnifiedRole::Button,
            "Web Button",
            NodeSource::Uia,
            None,
        ));

        let fused = engine.fuse(native, Some(additional));
        let webview_node = &fused.children[0];

        // Both native and additional children should be present
        assert_eq!(webview_node.children.len(), 2);
        assert_eq!(
            webview_node.children[0].name.as_deref(),
            Some("Native child")
        );
        assert_eq!(webview_node.children[1].name.as_deref(), Some("Web Button"));
    }
}
