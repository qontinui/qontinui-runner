//! Accessibility event system.
//!
//! Defines event types emitted by platform adapters and consumed by the
//! `TreeCache` for incremental updates and by the frontend for real-time
//! accessibility state notifications.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::model::Ref;

/// Accessibility events produced by platform adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum A11yEvent {
    /// An element gained keyboard focus.
    FocusChanged {
        ref_id: Ref,
        node_name: Option<String>,
    },

    /// Tree structure changed (children added, removed, or reordered).
    StructureChanged {
        parent_ref: Ref,
        change_type: StructureChangeType,
    },

    /// An element's property changed (name, value, or state).
    PropertyChanged {
        ref_id: Ref,
        property: String,
        old_value: Option<String>,
        new_value: Option<String>,
    },

    /// Full tree was re-captured (e.g., after page navigation).
    TreeReplaced {
        generation: u64,
        total_nodes: u32,
    },

    /// Connection status changed.
    ConnectionChanged {
        connected: bool,
        backend: String,
    },
}

/// Type of structural change in the accessibility tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructureChangeType {
    ChildAdded,
    ChildRemoved,
    ChildrenReordered,
    Subtree,
}

/// Sender half of the accessibility event channel.
pub type EventSender = broadcast::Sender<A11yEvent>;

/// Receiver half of the accessibility event channel.
pub type EventReceiver = broadcast::Receiver<A11yEvent>;

/// Default dirty batch interval in milliseconds.
pub const DEFAULT_DIRTY_BATCH_MS: u64 = 200;

/// Create an event channel with the given capacity.
pub fn create_event_channel(capacity: usize) -> (EventSender, EventReceiver) {
    broadcast::channel(capacity)
}
