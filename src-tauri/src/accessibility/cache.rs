//! Tree cache for accessibility snapshots.
//!
//! Maintains a live `UnifiedSnapshot` with thread-safe read/write access
//! and a handle→ref index for O(1) lookups. Phase 1 implements full-replacement
//! mode; Phase 7 adds incremental patching via `apply_event()`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::events::{A11yEvent, EventSender, DEFAULT_DIRTY_BATCH_MS};
use super::model::{NodeSource, Ref, UnifiedNode, UnifiedSnapshot};

/// Thread-safe cache for the current accessibility tree snapshot.
pub struct TreeCache {
    /// The current snapshot (None until first capture).
    snapshot: Arc<RwLock<Option<UnifiedSnapshot>>>,

    /// Index from platform_handle → ref_id for O(1) interaction dispatch.
    handle_index: Arc<RwLock<HashMap<u64, Ref>>>,

    /// Monotonically increasing generation counter.
    generation: AtomicU64,

    /// Event sender for broadcasting cache changes.
    event_tx: EventSender,

    /// Set of ref IDs whose subtrees need re-capture (Phase 7).
    dirty_refs: Arc<RwLock<HashSet<String>>>,
}

impl TreeCache {
    /// Create a new empty tree cache.
    pub fn new(event_tx: EventSender) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(None)),
            handle_index: Arc::new(RwLock::new(HashMap::new())),
            generation: AtomicU64::new(0),
            event_tx,
            dirty_refs: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Get the current generation counter.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Increment and return the next generation number.
    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Replace the entire snapshot (full-replacement mode).
    ///
    /// Rebuilds the handle→ref index from the new tree.
    pub async fn replace(&self, snapshot: UnifiedSnapshot) {
        // Rebuild handle index
        let mut index = HashMap::new();
        Self::build_handle_index(&snapshot.root, &mut index);

        {
            let mut idx = self.handle_index.write().await;
            *idx = index;
        }

        {
            let mut snap = self.snapshot.write().await;
            *snap = Some(snapshot);
        }
    }

    /// Build the platform_handle → ref_id index from a tree.
    fn build_handle_index(node: &UnifiedNode, index: &mut HashMap<u64, Ref>) {
        if let Some(handle) = node.platform_handle {
            if !node.ref_id.is_empty() {
                index.insert(handle, node.ref_id.clone());
            }
        }
        for child in &node.children {
            Self::build_handle_index(child, index);
        }
    }

    /// Get a read-only reference to the current snapshot.
    pub async fn snapshot(&self) -> Option<UnifiedSnapshot> {
        self.snapshot.read().await.clone()
    }

    /// Find a node by ref ID in the current snapshot.
    pub async fn node_by_ref(&self, ref_id: &str) -> Option<UnifiedNode> {
        let snap = self.snapshot.read().await;
        snap.as_ref()
            .and_then(|s| s.root.find_by_ref(ref_id))
            .cloned()
    }

    /// Find a node by platform handle in the current snapshot.
    pub async fn node_by_handle(&self, handle: u64) -> Option<UnifiedNode> {
        let snap = self.snapshot.read().await;
        snap.as_ref()
            .and_then(|s| s.root.find_by_handle(handle))
            .cloned()
    }

    /// Look up the ref ID for a platform handle.
    pub async fn ref_for_handle(&self, handle: u64) -> Option<Ref> {
        self.handle_index.read().await.get(&handle).cloned()
    }

    /// Get all interactive nodes from the current snapshot.
    pub async fn interactive_nodes(&self) -> Vec<UnifiedNode> {
        let snap = self.snapshot.read().await;
        match snap.as_ref() {
            Some(s) => s.root.collect_interactive().into_iter().cloned().collect(),
            None => vec![],
        }
    }

    /// Check if a snapshot is currently cached.
    pub async fn has_snapshot(&self) -> bool {
        self.snapshot.read().await.is_some()
    }

    /// Clear the cache.
    pub async fn clear(&self) {
        *self.snapshot.write().await = None;
        self.handle_index.write().await.clear();
    }

    /// Get a reference to the event sender for broadcasting events.
    pub fn event_sender(&self) -> &EventSender {
        &self.event_tx
    }

    // ---- Phase 7: Incremental patching ----

    /// Apply a single accessibility event to the cached tree.
    ///
    /// - `FocusChanged` updates the `is_focused` flag on old/new nodes.
    /// - `PropertyChanged` updates the named property on the target node.
    /// - `StructureChanged` marks the parent ref as dirty for re-capture.
    /// - Other events are ignored (no-op).
    pub async fn apply_event(&self, event: &A11yEvent) {
        match event {
            A11yEvent::FocusChanged { ref_id, .. } => {
                let mut snap = self.snapshot.write().await;
                if let Some(snapshot) = snap.as_mut() {
                    // Clear old focus
                    Self::clear_focus(&mut snapshot.root);
                    // Set new focus
                    if let Some(node) = snapshot.root.find_by_ref_mut(ref_id) {
                        node.state.is_focused = true;
                        debug!("Focus moved to {}", ref_id);
                    } else {
                        warn!("FocusChanged target not found in tree: {}", ref_id);
                    }
                }
            }
            A11yEvent::PropertyChanged {
                ref_id,
                property,
                new_value,
                ..
            } => {
                let mut snap = self.snapshot.write().await;
                if let Some(snapshot) = snap.as_mut() {
                    if let Some(node) = snapshot.root.find_by_ref_mut(ref_id) {
                        match property.as_str() {
                            "name" => {
                                node.name = new_value.clone();
                                debug!("Updated name on {}", ref_id);
                            }
                            "value" => {
                                node.value = new_value.clone();
                                debug!("Updated value on {}", ref_id);
                            }
                            "description" => {
                                node.description = new_value.clone();
                                debug!("Updated description on {}", ref_id);
                            }
                            "is_focused" => {
                                node.state.is_focused = new_value.as_deref() == Some("true");
                            }
                            "is_disabled" => {
                                node.state.is_disabled = new_value.as_deref() == Some("true");
                            }
                            "is_hidden" => {
                                node.state.is_hidden = new_value.as_deref() == Some("true");
                            }
                            "is_readonly" => {
                                node.state.is_readonly = new_value.as_deref() == Some("true");
                            }
                            other => {
                                debug!("Unhandled property update '{}' on {}", other, ref_id);
                            }
                        }
                    } else {
                        warn!("PropertyChanged target not found in tree: {}", ref_id);
                    }
                }
            }
            A11yEvent::StructureChanged { parent_ref, .. } => {
                self.dirty_refs.write().await.insert(parent_ref.clone());
                debug!("Marked {} as dirty (structure changed)", parent_ref);
            }
            // TreeReplaced and ConnectionChanged are informational; no cache mutation needed.
            _ => {}
        }
    }

    /// Recursively clear the `is_focused` flag on all nodes in the subtree.
    fn clear_focus(node: &mut UnifiedNode) {
        node.state.is_focused = false;
        for child in &mut node.children {
            Self::clear_focus(child);
        }
    }

    /// Return and clear all dirty ref IDs.
    pub async fn drain_dirty(&self) -> HashSet<String> {
        let mut dirty = self.dirty_refs.write().await;
        std::mem::take(&mut *dirty)
    }

    /// Check whether any nodes are currently marked dirty.
    pub async fn has_dirty(&self) -> bool {
        !self.dirty_refs.read().await.is_empty()
    }

    /// Spawn a background task that listens for events and applies them.
    ///
    /// On a `DEFAULT_DIRTY_BATCH_MS` interval, if dirty nodes exist, emits a
    /// `TreeReplaced` event signaling the consumer should re-capture those subtrees.
    ///
    /// Returns a `JoinHandle` that can be aborted to stop the processor.
    pub fn start_event_processor(self: &Arc<Self>) -> JoinHandle<()> {
        let cache = Arc::clone(self);
        let mut rx = cache.event_tx.subscribe();
        let interval = Duration::from_millis(DEFAULT_DIRTY_BATCH_MS);

        tokio::spawn(async move {
            let mut dirty_timer = tokio::time::interval(interval);
            // The first tick completes immediately; consume it.
            dirty_timer.tick().await;

            loop {
                tokio::select! {
                    result = rx.recv() => {
                        match result {
                            Ok(event) => {
                                cache.apply_event(&event).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Event processor lagged, missed {} events", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                debug!("Event channel closed, stopping event processor");
                                break;
                            }
                        }
                    }
                    _ = dirty_timer.tick() => {
                        if cache.has_dirty().await {
                            let dirty = cache.drain_dirty().await;
                            let generation = cache.generation.load(Ordering::Relaxed);
                            debug!(
                                "Dirty batch: {} refs need re-capture (gen {})",
                                dirty.len(),
                                generation
                            );
                            let _ = cache.event_tx.send(A11yEvent::TreeReplaced {
                                generation,
                                total_nodes: 0, // consumer should re-capture to get actual count
                            });
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::events;
    use crate::accessibility::events::StructureChangeType;
    use crate::accessibility::model::*;

    fn make_test_snapshot() -> UnifiedSnapshot {
        let root = UnifiedNode {
            ref_id: "@e0".into(),
            role: UnifiedRole::Window,
            name: Some("Test Window".into()),
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: false,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: Some(100),
            supported_patterns: vec![],
            generation: 1,
            children: vec![UnifiedNode {
                ref_id: "@e1".into(),
                role: UnifiedRole::Button,
                name: Some("OK".into()),
                value: None,
                description: None,
                bounds: Some(UnifiedBounds {
                    x: 10,
                    y: 20,
                    width: 80,
                    height: 30,
                }),
                state: UnifiedState::default(),
                is_interactive: true,
                level: None,
                automation_id: None,
                class_name: None,
                html_tag: None,
                url: None,
                source: NodeSource::Uia,
                platform_handle: Some(101),

                supported_patterns: vec![InteractionPattern::Invoke],
                generation: 1,
                children: vec![],
            }],
        };

        UnifiedSnapshot {
            root,
            timestamp_ms: 1000,
            source: NodeSource::Uia,
            url: None,
            title: Some("Test Window".into()),
            total_nodes: 2,
            interactive_nodes: 1,
            generation: 1,
        }
    }

    #[tokio::test]
    async fn test_replace_and_query() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        assert!(!cache.has_snapshot().await);
        assert!(cache.node_by_ref("@e1").await.is_none());

        let snapshot = make_test_snapshot();
        cache.replace(snapshot).await;

        assert!(cache.has_snapshot().await);

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert_eq!(node.name.as_deref(), Some("OK"));
        assert_eq!(node.role, UnifiedRole::Button);

        assert!(cache.node_by_ref("@e99").await.is_none());
    }

    #[tokio::test]
    async fn test_handle_index() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        cache.replace(make_test_snapshot()).await;

        assert_eq!(cache.ref_for_handle(101).await.as_deref(), Some("@e1"));
        assert_eq!(cache.ref_for_handle(100).await.as_deref(), Some("@e0"));
        assert!(cache.ref_for_handle(999).await.is_none());
    }

    #[tokio::test]
    async fn test_interactive_nodes() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        cache.replace(make_test_snapshot()).await;

        let interactive = cache.interactive_nodes().await;
        assert_eq!(interactive.len(), 1);
        assert_eq!(interactive[0].name.as_deref(), Some("OK"));
    }

    #[tokio::test]
    async fn test_clear() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        cache.replace(make_test_snapshot()).await;
        assert!(cache.has_snapshot().await);

        cache.clear().await;
        assert!(!cache.has_snapshot().await);
    }

    #[test]
    fn test_generation_counter() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        assert_eq!(cache.generation(), 0);
        assert_eq!(cache.next_generation(), 1);
        assert_eq!(cache.next_generation(), 2);
        assert_eq!(cache.generation(), 2);
    }

    // ---- Phase 7 tests ----

    #[tokio::test]
    async fn test_apply_focus_changed() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        // Initially no node is focused
        let node = cache.node_by_ref("@e1").await.unwrap();
        assert!(!node.state.is_focused);

        // Focus the button
        cache
            .apply_event(&A11yEvent::FocusChanged {
                ref_id: "@e1".into(),
                node_name: Some("OK".into()),
            })
            .await;

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert!(node.state.is_focused);

        // Focus the window — button should lose focus
        cache
            .apply_event(&A11yEvent::FocusChanged {
                ref_id: "@e0".into(),
                node_name: Some("Test Window".into()),
            })
            .await;

        let button = cache.node_by_ref("@e1").await.unwrap();
        assert!(!button.state.is_focused);
        let window = cache.node_by_ref("@e0").await.unwrap();
        assert!(window.state.is_focused);
    }

    #[tokio::test]
    async fn test_apply_property_changed_name() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        cache
            .apply_event(&A11yEvent::PropertyChanged {
                ref_id: "@e1".into(),
                property: "name".into(),
                old_value: Some("OK".into()),
                new_value: Some("Submit".into()),
            })
            .await;

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert_eq!(node.name.as_deref(), Some("Submit"));
    }

    #[tokio::test]
    async fn test_apply_property_changed_value() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        cache
            .apply_event(&A11yEvent::PropertyChanged {
                ref_id: "@e1".into(),
                property: "value".into(),
                old_value: None,
                new_value: Some("42".into()),
            })
            .await;

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert_eq!(node.value.as_deref(), Some("42"));
    }

    #[tokio::test]
    async fn test_apply_property_changed_state() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        // Disable the button via property change
        cache
            .apply_event(&A11yEvent::PropertyChanged {
                ref_id: "@e1".into(),
                property: "is_disabled".into(),
                old_value: Some("false".into()),
                new_value: Some("true".into()),
            })
            .await;

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert!(node.state.is_disabled);
    }

    #[tokio::test]
    async fn test_apply_structure_changed_marks_dirty() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        assert!(!cache.has_dirty().await);

        cache
            .apply_event(&A11yEvent::StructureChanged {
                parent_ref: "@e0".into(),
                change_type: StructureChangeType::ChildAdded,
            })
            .await;

        assert!(cache.has_dirty().await);

        let dirty = cache.drain_dirty().await;
        assert_eq!(dirty.len(), 1);
        assert!(dirty.contains("@e0"));

        // After drain, should be empty
        assert!(!cache.has_dirty().await);
    }

    #[tokio::test]
    async fn test_drain_dirty_clears() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        // Add multiple dirty refs
        cache
            .apply_event(&A11yEvent::StructureChanged {
                parent_ref: "@e0".into(),
                change_type: StructureChangeType::ChildAdded,
            })
            .await;
        cache
            .apply_event(&A11yEvent::StructureChanged {
                parent_ref: "@e1".into(),
                change_type: StructureChangeType::ChildRemoved,
            })
            .await;
        // Same ref again — should deduplicate
        cache
            .apply_event(&A11yEvent::StructureChanged {
                parent_ref: "@e0".into(),
                change_type: StructureChangeType::Subtree,
            })
            .await;

        let dirty = cache.drain_dirty().await;
        assert_eq!(dirty.len(), 2);
        assert!(dirty.contains("@e0"));
        assert!(dirty.contains("@e1"));

        // Second drain should be empty
        let dirty2 = cache.drain_dirty().await;
        assert!(dirty2.is_empty());
    }

    #[tokio::test]
    async fn test_apply_event_no_snapshot_is_noop() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        // No snapshot loaded — focus/property events should not panic
        cache
            .apply_event(&A11yEvent::FocusChanged {
                ref_id: "@e1".into(),
                node_name: None,
            })
            .await;

        cache
            .apply_event(&A11yEvent::PropertyChanged {
                ref_id: "@e1".into(),
                property: "name".into(),
                old_value: None,
                new_value: Some("New".into()),
            })
            .await;

        // Structure changed still marks dirty even without snapshot
        cache
            .apply_event(&A11yEvent::StructureChanged {
                parent_ref: "@e0".into(),
                change_type: StructureChangeType::ChildAdded,
            })
            .await;
        assert!(cache.has_dirty().await);
    }

    #[tokio::test]
    async fn test_apply_event_unknown_ref_no_panic() {
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);
        cache.replace(make_test_snapshot()).await;

        // Unknown ref — should warn but not panic
        cache
            .apply_event(&A11yEvent::FocusChanged {
                ref_id: "@e99".into(),
                node_name: None,
            })
            .await;

        cache
            .apply_event(&A11yEvent::PropertyChanged {
                ref_id: "@e99".into(),
                property: "name".into(),
                old_value: None,
                new_value: Some("Ghost".into()),
            })
            .await;
    }

    #[tokio::test]
    async fn test_start_event_processor_applies_events() {
        let (tx, _rx) = events::create_event_channel(64);
        let cache = Arc::new(TreeCache::new(tx.clone()));
        cache.replace(make_test_snapshot()).await;

        let handle = cache.start_event_processor();

        // Send a focus event through the channel
        tx.send(A11yEvent::FocusChanged {
            ref_id: "@e1".into(),
            node_name: Some("OK".into()),
        })
        .unwrap();

        // Give the processor time to handle it
        tokio::time::sleep(Duration::from_millis(50)).await;

        let node = cache.node_by_ref("@e1").await.unwrap();
        assert!(node.state.is_focused);

        handle.abort();
    }

    #[tokio::test]
    async fn test_start_event_processor_dirty_batch() {
        let (tx, mut rx2) = events::create_event_channel(64);
        let cache = Arc::new(TreeCache::new(tx.clone()));
        cache.replace(make_test_snapshot()).await;

        let handle = cache.start_event_processor();

        // Send a structure changed event
        tx.send(A11yEvent::StructureChanged {
            parent_ref: "@e0".into(),
            change_type: StructureChangeType::ChildAdded,
        })
        .unwrap();

        // Wait for the dirty batch interval to fire
        tokio::time::sleep(Duration::from_millis(DEFAULT_DIRTY_BATCH_MS + 100)).await;

        // The processor should have emitted a TreeReplaced event
        // (we might also receive the StructureChanged event itself)
        let mut got_tree_replaced = false;
        // Drain available events
        loop {
            match rx2.try_recv() {
                Ok(A11yEvent::TreeReplaced { .. }) => {
                    got_tree_replaced = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert!(
            got_tree_replaced,
            "Expected TreeReplaced event from dirty batch"
        );

        // Dirty set should have been drained
        assert!(!cache.has_dirty().await);

        handle.abort();
    }

    #[tokio::test]
    async fn test_existing_methods_unchanged_after_phase7() {
        // Verify all Phase 1 methods still work identically.
        let (tx, _rx) = events::create_event_channel(16);
        let cache = TreeCache::new(tx);

        cache.replace(make_test_snapshot()).await;
        assert!(cache.has_snapshot().await);
        assert_eq!(cache.ref_for_handle(101).await.as_deref(), Some("@e1"));
        assert_eq!(cache.interactive_nodes().await.len(), 1);

        let snap = cache.snapshot().await.unwrap();
        assert_eq!(snap.total_nodes, 2);

        cache.clear().await;
        assert!(!cache.has_snapshot().await);
    }
}
