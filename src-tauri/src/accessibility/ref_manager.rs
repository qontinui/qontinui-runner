//! Reference manager for accessibility nodes.
//!
//! Rust port of the Python `RefManager`. Provides stable identifiers (`@e1`,
//! `@e2`, etc.) for accessibility nodes, with optional JSON persistence for
//! ref→fingerprint mappings that survive reconnections.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use super::model::{UnifiedNode, UnifiedRole};

/// Fingerprint of an element used for cross-session ref re-resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefFingerprint {
    pub automation_id: Option<String>,
    pub role: Option<String>,
    pub name: Option<String>,
    pub class_name: Option<String>,
    pub bounds: Option<[i32; 4]>,
}

impl RefFingerprint {
    /// Create a fingerprint from a `UnifiedNode`.
    pub fn from_node(node: &UnifiedNode) -> Self {
        Self {
            automation_id: node.automation_id.clone(),
            role: Some(node.role.as_str().to_string()),
            name: node.name.clone(),
            class_name: node.class_name.clone(),
            bounds: node.bounds.as_ref().map(|b| [b.x, b.y, b.width, b.height]),
        }
    }
}

/// Manages ref assignment and lookup for accessibility nodes.
///
/// Refs are assigned sequentially (`@e1`, `@e2`, ...) during tree traversal.
/// The manager maintains a mapping from refs to fingerprints for persistence.
pub struct RefManager {
    counter: u64,
    fingerprints: HashMap<String, RefFingerprint>,
    persistence_dir: Option<PathBuf>,
}

impl RefManager {
    pub fn new() -> Self {
        Self {
            counter: 0,
            fingerprints: HashMap::new(),
            persistence_dir: None,
        }
    }

    /// Set the directory for ref persistence files.
    pub fn set_persistence_dir(&mut self, dir: PathBuf) {
        self.persistence_dir = Some(dir);
    }

    /// Reset the manager, clearing all refs and resetting the counter.
    pub fn reset(&mut self) {
        self.counter = 0;
        self.fingerprints.clear();
    }

    /// Assign the next sequential ref.
    pub fn assign_ref(&mut self) -> String {
        self.counter += 1;
        format!("@e{}", self.counter)
    }

    /// Register a node under a specific ref, storing its fingerprint.
    pub fn register_node(&mut self, ref_id: &str, node: &UnifiedNode) {
        self.fingerprints
            .insert(ref_id.to_string(), RefFingerprint::from_node(node));
    }

    /// Assign refs to all nodes in a tree (depth-first), registering fingerprints.
    pub fn assign_refs_to_tree(&mut self, node: &mut UnifiedNode, interactive_only: bool) {
        self.reset();
        self.assign_refs_recursive(node, interactive_only);
    }

    fn assign_refs_recursive(&mut self, node: &mut UnifiedNode, interactive_only: bool) {
        if !interactive_only || node.is_interactive {
            let ref_id = self.assign_ref();
            node.ref_id = ref_id.clone();
            self.register_node(&ref_id, node);
        } else {
            node.ref_id = String::new();
        }

        for child in &mut node.children {
            self.assign_refs_recursive(child, interactive_only);
        }
    }

    /// Number of registered refs.
    pub fn count(&self) -> usize {
        self.fingerprints.len()
    }

    /// Get all registered ref IDs.
    pub fn all_refs(&self) -> Vec<String> {
        self.fingerprints.keys().cloned().collect()
    }

    // =========================================================================
    // Persistence
    // =========================================================================

    /// Get the persistence file path for a given target name.
    fn persistence_path(&self, target_name: &str) -> Option<PathBuf> {
        self.persistence_dir.as_ref().map(|dir| {
            let safe_name: String = target_name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(80)
                .collect();
            dir.join(format!("{}.json", safe_name.trim()))
        })
    }

    /// Save ref fingerprints to a JSON file for the given target.
    pub fn save(&self, target_name: &str) -> anyhow::Result<usize> {
        let path = self
            .persistence_path(target_name)
            .ok_or_else(|| anyhow::anyhow!("No persistence directory configured"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(&self.fingerprints)?;
        std::fs::write(&path, json)?;
        debug!("Saved {} ref fingerprints to {:?}", self.fingerprints.len(), path);
        Ok(self.fingerprints.len())
    }

    /// Load persisted ref fingerprints from disk.
    ///
    /// Does NOT restore refs into the manager — caller must re-resolve
    /// fingerprints against a fresh tree and call `register_node`.
    pub fn load(&self, target_name: &str) -> HashMap<String, RefFingerprint> {
        let path = match self.persistence_path(target_name) {
            Some(p) if p.exists() => p,
            _ => return HashMap::new(),
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(fingerprints) => {
                    debug!("Loaded ref fingerprints from {:?}", path);
                    fingerprints
                }
                Err(e) => {
                    warn!("Failed to parse ref fingerprints from {:?}: {}", path, e);
                    HashMap::new()
                }
            },
            Err(e) => {
                warn!("Failed to read ref fingerprints from {:?}: {}", path, e);
                HashMap::new()
            }
        }
    }

    /// Attempt to re-resolve persisted fingerprints against a new tree.
    ///
    /// Matches by automation_id (most stable), then by role+name.
    /// Returns the number of successfully re-resolved refs.
    pub fn restore_from_fingerprints(
        &mut self,
        fingerprints: &HashMap<String, RefFingerprint>,
        root: &mut UnifiedNode,
    ) -> usize {
        let interactive_nodes = root.collect_interactive();
        let mut restored = 0;

        for (ref_id, fp) in fingerprints {
            let mut matched = false;

            // Strategy 1: automation_id (most stable)
            if let Some(auto_id) = &fp.automation_id {
                for node in &interactive_nodes {
                    if node.automation_id.as_deref() == Some(auto_id.as_str()) {
                        self.fingerprints
                            .insert(ref_id.clone(), RefFingerprint::from_node(node));
                        restored += 1;
                        matched = true;
                        break;
                    }
                }
            }

            // Strategy 2: role + name (fallback)
            if !matched {
                if let (Some(role_str), Some(name)) = (&fp.role, &fp.name) {
                    for node in &interactive_nodes {
                        if node.role.as_str() == role_str && node.name.as_deref() == Some(name.as_str())
                        {
                            self.fingerprints
                                .insert(ref_id.clone(), RefFingerprint::from_node(node));
                            restored += 1;
                            break;
                        }
                    }
                }
            }
        }

        if restored > 0 {
            debug!(
                "Restored {}/{} persisted refs",
                restored,
                fingerprints.len()
            );
        }
        restored
    }
}

impl Default for RefManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::model::*;

    fn make_test_node(name: &str, role: UnifiedRole, interactive: bool) -> UnifiedNode {
        UnifiedNode {
            ref_id: String::new(),
            role,
            name: Some(name.to_string()),
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: interactive,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: None,

            supported_patterns: vec![],
            generation: 0,
            children: vec![],
        }
    }

    #[test]
    fn test_sequential_ref_assignment() {
        let mut mgr = RefManager::new();
        assert_eq!(mgr.assign_ref(), "@e1");
        assert_eq!(mgr.assign_ref(), "@e2");
        assert_eq!(mgr.assign_ref(), "@e3");
    }

    #[test]
    fn test_assign_refs_to_tree() {
        let mut mgr = RefManager::new();
        let mut root = make_test_node("Window", UnifiedRole::Window, false);
        root.children.push(make_test_node("OK", UnifiedRole::Button, true));
        root.children.push(make_test_node("Label", UnifiedRole::StaticText, false));
        root.children.push(make_test_node("Cancel", UnifiedRole::Button, true));

        mgr.assign_refs_to_tree(&mut root, false);
        assert_eq!(root.ref_id, "@e1");
        assert_eq!(root.children[0].ref_id, "@e2");
        assert_eq!(root.children[1].ref_id, "@e3");
        assert_eq!(root.children[2].ref_id, "@e4");
        assert_eq!(mgr.count(), 4);
    }

    #[test]
    fn test_assign_refs_interactive_only() {
        let mut mgr = RefManager::new();
        let mut root = make_test_node("Window", UnifiedRole::Window, false);
        root.children.push(make_test_node("OK", UnifiedRole::Button, true));
        root.children.push(make_test_node("Label", UnifiedRole::StaticText, false));

        mgr.assign_refs_to_tree(&mut root, true);
        assert_eq!(root.ref_id, ""); // not interactive
        assert_eq!(root.children[0].ref_id, "@e1");
        assert_eq!(root.children[1].ref_id, ""); // not interactive
        assert_eq!(mgr.count(), 1);
    }

    #[test]
    fn test_fingerprint_creation() {
        let node = UnifiedNode {
            ref_id: "@e1".into(),
            role: UnifiedRole::Button,
            name: Some("Submit".into()),
            automation_id: Some("btn_submit".into()),
            bounds: Some(UnifiedBounds { x: 10, y: 20, width: 80, height: 30 }),
            ..make_test_node("Submit", UnifiedRole::Button, true)
        };

        let fp = RefFingerprint::from_node(&node);
        assert_eq!(fp.automation_id.as_deref(), Some("btn_submit"));
        assert_eq!(fp.role.as_deref(), Some("button"));
        assert_eq!(fp.name.as_deref(), Some("Submit"));
        assert_eq!(fp.bounds, Some([10, 20, 80, 30]));
    }

    #[test]
    fn test_reset() {
        let mut mgr = RefManager::new();
        mgr.assign_ref();
        mgr.assign_ref();
        assert_eq!(mgr.count(), 0); // assign_ref alone doesn't register
        let mut node = make_test_node("Test", UnifiedRole::Button, true);
        node.ref_id = "@e1".into();
        mgr.register_node("@e1", &node);
        assert_eq!(mgr.count(), 1);
        mgr.reset();
        assert_eq!(mgr.count(), 0);
    }
}
