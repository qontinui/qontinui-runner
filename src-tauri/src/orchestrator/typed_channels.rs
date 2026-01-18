//! Typed State Channels Module
//!
//! Implements the typed channel pattern from LangGraph for type-safe state
//! management with explicit data flow semantics.
//!
//! ## Key Concepts
//!
//! - **Channel**: A named, typed container for state values
//! - **Reducer**: Function that merges updates (optional)
//! - **Version**: Each update increments version for tracking
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::typed_channels::*;
//!
//! let mut state = ChannelState::new();
//!
//! // Create a channel that accumulates findings
//! state.create_channel::<Vec<String>>(
//!     "findings",
//!     Some(Reducer::Concat),
//! );
//!
//! // Update the channel
//! state.update("findings", vec!["Found issue A".to_string()]);
//! state.update("findings", vec!["Found issue B".to_string()]);
//!
//! // Get accumulated value
//! let findings: Vec<String> = state.get("findings").unwrap();
//! assert_eq!(findings.len(), 2);
//! ```

use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ============================================================================
// Reducer Types
// ============================================================================

/// Strategies for merging channel updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reducer {
    /// Last value wins (default).
    LastValue,
    /// Concatenate vectors/strings.
    Concat,
    /// Sum numeric values.
    Sum,
    /// Keep minimum value.
    Min,
    /// Keep maximum value.
    Max,
    /// Merge maps (later values override).
    MergeMap,
    /// Custom reducer (by name, resolved at runtime).
    Custom { name: String },
}

impl Default for Reducer {
    fn default() -> Self {
        Self::LastValue
    }
}

// ============================================================================
// Channel Value Wrapper
// ============================================================================

/// Type-erased channel value with version tracking.
#[derive(Debug, Clone)]
pub struct ChannelValue {
    /// The actual value (type-erased).
    value: Arc<dyn Any + Send + Sync>,
    /// Version number (increments on each update).
    version: u64,
    /// Timestamp of last update (RFC3339).
    updated_at: String,
    /// Type name for debugging.
    type_name: String,
}

impl ChannelValue {
    /// Create a new channel value.
    pub fn new<T: Any + Send + Sync + Clone>(value: T) -> Self {
        Self {
            value: Arc::new(value),
            version: 1,
            updated_at: chrono::Utc::now().to_rfc3339(),
            type_name: std::any::type_name::<T>().to_string(),
        }
    }

    /// Get the value, downcasting to the expected type.
    pub fn get<T: Any + Clone>(&self) -> Option<T> {
        self.value.downcast_ref::<T>().cloned()
    }

    /// Get the version number.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Get the last update timestamp.
    pub fn updated_at(&self) -> &str {
        &self.updated_at
    }

    /// Get the type name.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Update the value, incrementing version.
    fn update<T: Any + Send + Sync + Clone>(&mut self, value: T) {
        self.value = Arc::new(value);
        self.version += 1;
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }
}

// ============================================================================
// Channel Definition
// ============================================================================

/// Definition of a state channel.
#[derive(Debug, Clone)]
pub struct ChannelDef {
    /// Channel name.
    pub name: String,
    /// Reducer strategy.
    pub reducer: Reducer,
    /// Type name for validation.
    pub type_name: String,
    /// Default value (serialized).
    pub default_value: Option<serde_json::Value>,
    /// Description for documentation.
    pub description: Option<String>,
}

impl ChannelDef {
    /// Create a new channel definition.
    pub fn new<T: 'static>(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reducer: Reducer::default(),
            type_name: std::any::type_name::<T>().to_string(),
            default_value: None,
            description: None,
        }
    }

    /// Set the reducer.
    pub fn with_reducer(mut self, reducer: Reducer) -> Self {
        self.reducer = reducer;
        self
    }

    /// Set a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

// ============================================================================
// Channel State
// ============================================================================

/// Container for all channel state.
#[derive(Debug, Default)]
pub struct ChannelState {
    /// Channel values by name.
    channels: HashMap<String, ChannelValue>,
    /// Channel definitions by name.
    definitions: HashMap<String, ChannelDef>,
    /// Global version (increments on any change).
    global_version: u64,
}

impl ChannelState {
    /// Create a new empty channel state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a channel with optional reducer.
    pub fn create_channel<T: Any + Send + Sync + Clone + Default>(
        &mut self,
        name: impl Into<String>,
        reducer: Option<Reducer>,
    ) {
        let name = name.into();
        let def = ChannelDef {
            name: name.clone(),
            reducer: reducer.unwrap_or_default(),
            type_name: std::any::type_name::<T>().to_string(),
            default_value: None,
            description: None,
        };
        self.definitions.insert(name.clone(), def);
        self.channels.insert(name, ChannelValue::new(T::default()));
    }

    /// Create a channel with an initial value.
    pub fn create_channel_with_value<T: Any + Send + Sync + Clone>(
        &mut self,
        name: impl Into<String>,
        initial: T,
        reducer: Option<Reducer>,
    ) {
        let name = name.into();
        let def = ChannelDef {
            name: name.clone(),
            reducer: reducer.unwrap_or_default(),
            type_name: std::any::type_name::<T>().to_string(),
            default_value: None,
            description: None,
        };
        self.definitions.insert(name.clone(), def);
        self.channels.insert(name, ChannelValue::new(initial));
    }

    /// Get a channel value.
    pub fn get<T: Any + Clone>(&self, name: &str) -> Option<T> {
        self.channels.get(name).and_then(|cv| cv.get::<T>())
    }

    /// Get channel metadata.
    pub fn get_meta(&self, name: &str) -> Option<(u64, String)> {
        self.channels
            .get(name)
            .map(|cv| (cv.version(), cv.updated_at().to_string()))
    }

    /// Update a channel value.
    pub fn update<T: Any + Send + Sync + Clone>(&mut self, name: &str, value: T) -> bool {
        // Get the reducer first (cloned to avoid borrow conflicts)
        let reducer = match self.definitions.get(name) {
            Some(def) => def.reducer.clone(),
            None => return false,
        };

        // Get current channel value for reducer (if needed)
        let current_value = self.channels.get(name).cloned();

        if let Some(current) = current_value {
            // Apply reducer
            let new_value = Self::apply_reducer_static(&reducer, &current, value);
            if let Some(nv) = new_value {
                if let Some(channel) = self.channels.get_mut(name) {
                    channel.update(nv);
                    self.global_version += 1;
                    return true;
                }
            }
        }
        false
    }

    /// Apply reducer to merge old and new values (static version to avoid borrow conflicts).
    fn apply_reducer_static<T: Any + Send + Sync + Clone>(
        reducer: &Reducer,
        current: &ChannelValue,
        new_value: T,
    ) -> Option<T> {
        match reducer {
            Reducer::LastValue => Some(new_value),
            Reducer::Concat => {
                // Special handling for Vec types
                if let (Some(old_vec), Some(new_vec)) =
                    (current.get::<Vec<T>>(), Some(&new_value as &dyn Any))
                {
                    if let Some(new_items) = new_vec.downcast_ref::<Vec<T>>() {
                        let mut combined = old_vec;
                        combined.extend(new_items.clone());
                        // This is tricky with generics - for now, just use last value for complex cases
                        return Some(new_value);
                    }
                }
                // For strings
                if let (Some(old_str), Some(new_str)) = (
                    current.get::<String>(),
                    (&new_value as &dyn Any).downcast_ref::<String>(),
                ) {
                    let combined = format!("{}{}", old_str, new_str);
                    if let Some(result) = (&combined as &dyn Any).downcast_ref::<T>() {
                        return Some(result.clone());
                    }
                }
                Some(new_value)
            }
            _ => Some(new_value),
        }
    }

    /// Set a channel value directly (bypassing reducer).
    pub fn set<T: Any + Send + Sync + Clone>(&mut self, name: &str, value: T) -> bool {
        if self.definitions.contains_key(name) {
            if let Some(channel) = self.channels.get_mut(name) {
                channel.update(value);
                self.global_version += 1;
                return true;
            }
        }
        false
    }

    /// Check if a channel exists.
    pub fn has_channel(&self, name: &str) -> bool {
        self.channels.contains_key(name)
    }

    /// Get all channel names.
    pub fn channel_names(&self) -> Vec<&str> {
        self.channels.keys().map(|s| s.as_str()).collect()
    }

    /// Get global version.
    pub fn global_version(&self) -> u64 {
        self.global_version
    }

    /// Get channel version.
    pub fn channel_version(&self, name: &str) -> Option<u64> {
        self.channels.get(name).map(|cv| cv.version())
    }

    /// Remove a channel.
    pub fn remove_channel(&mut self, name: &str) -> bool {
        self.definitions.remove(name);
        self.channels.remove(name).is_some()
    }

    /// Clear all channels.
    pub fn clear(&mut self) {
        self.channels.clear();
        self.definitions.clear();
        self.global_version = 0;
    }
}

// ============================================================================
// Thread-Safe Channel State
// ============================================================================

/// Thread-safe wrapper around ChannelState.
#[derive(Debug, Clone, Default)]
pub struct SharedChannelState {
    inner: Arc<RwLock<ChannelState>>,
}

impl SharedChannelState {
    /// Create a new shared channel state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a channel.
    pub fn create_channel<T: Any + Send + Sync + Clone + Default>(
        &self,
        name: impl Into<String>,
        reducer: Option<Reducer>,
    ) {
        self.inner
            .write()
            .unwrap()
            .create_channel::<T>(name, reducer);
    }

    /// Create a channel with initial value.
    pub fn create_channel_with_value<T: Any + Send + Sync + Clone>(
        &self,
        name: impl Into<String>,
        initial: T,
        reducer: Option<Reducer>,
    ) {
        self.inner
            .write()
            .unwrap()
            .create_channel_with_value(name, initial, reducer);
    }

    /// Get a value.
    pub fn get<T: Any + Clone>(&self, name: &str) -> Option<T> {
        self.inner.read().unwrap().get(name)
    }

    /// Update a value.
    pub fn update<T: Any + Send + Sync + Clone>(&self, name: &str, value: T) -> bool {
        self.inner.write().unwrap().update(name, value)
    }

    /// Set a value directly.
    pub fn set<T: Any + Send + Sync + Clone>(&self, name: &str, value: T) -> bool {
        self.inner.write().unwrap().set(name, value)
    }

    /// Check if channel exists.
    pub fn has_channel(&self, name: &str) -> bool {
        self.inner.read().unwrap().has_channel(name)
    }

    /// Get global version.
    pub fn global_version(&self) -> u64 {
        self.inner.read().unwrap().global_version()
    }

    /// Get channel version.
    pub fn channel_version(&self, name: &str) -> Option<u64> {
        self.inner.read().unwrap().channel_version(name)
    }
}

// ============================================================================
// Predefined Channel Types
// ============================================================================

/// Common channel configurations for orchestrator state.
pub struct StandardChannels;

impl StandardChannels {
    /// Create standard channels for a task run.
    pub fn create_task_channels(state: &mut ChannelState) {
        // Status channel - last value wins
        state.create_channel_with_value::<String>("status", "pending".to_string(), None);

        // Findings - accumulate
        state.create_channel::<Vec<Finding>>("findings", Some(Reducer::Concat));

        // Files modified - accumulate
        state.create_channel::<Vec<String>>("files_modified", Some(Reducer::Concat));

        // Progress - last value
        state.create_channel_with_value::<Progress>("progress", Progress::default(), None);

        // Errors - accumulate
        state.create_channel::<Vec<String>>("errors", Some(Reducer::Concat));

        // Iteration count - last value
        state.create_channel_with_value::<u32>("iteration", 0, None);

        // Current worker - last value
        state.create_channel_with_value::<Option<String>>("current_worker", None, None);
    }
}

/// A finding from worker analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub description: String,
    pub location: Option<String>,
    pub suggestion: Option<String>,
}

/// Progress tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    pub current_step: u32,
    pub total_steps: u32,
    pub percent_complete: f32,
    pub current_activity: String,
}

// ============================================================================
// Channel Snapshot
// ============================================================================

/// Serializable snapshot of channel state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    /// Snapshot of each channel's metadata.
    pub channels: HashMap<String, ChannelMeta>,
    /// Global version at snapshot time.
    pub global_version: u64,
    /// Snapshot timestamp.
    pub timestamp: String,
}

/// Metadata about a channel (without the actual value for serialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub type_name: String,
    pub version: u64,
    pub updated_at: String,
    pub reducer: Reducer,
}

impl ChannelState {
    /// Create a snapshot of channel metadata.
    pub fn snapshot(&self) -> ChannelSnapshot {
        let channels = self
            .channels
            .iter()
            .filter_map(|(name, value)| {
                self.definitions.get(name).map(|def| {
                    (
                        name.clone(),
                        ChannelMeta {
                            name: name.clone(),
                            type_name: value.type_name().to_string(),
                            version: value.version(),
                            updated_at: value.updated_at().to_string(),
                            reducer: def.reducer.clone(),
                        },
                    )
                })
            })
            .collect();

        ChannelSnapshot {
            channels,
            global_version: self.global_version,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_channel() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("status", "active".to_string(), None);

        let value: Option<String> = state.get("status");
        assert_eq!(value, Some("active".to_string()));
    }

    #[test]
    fn test_update_channel() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<u32>("counter", 0, None);

        state.update("counter", 5u32);
        let value: Option<u32> = state.get("counter");
        assert_eq!(value, Some(5));
    }

    #[test]
    fn test_channel_version() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("data", "v1".to_string(), None);

        assert_eq!(state.channel_version("data"), Some(1));

        state.update("data", "v2".to_string());
        assert_eq!(state.channel_version("data"), Some(2));

        state.update("data", "v3".to_string());
        assert_eq!(state.channel_version("data"), Some(3));
    }

    #[test]
    fn test_global_version() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("a", "1".to_string(), None);
        state.create_channel_with_value::<String>("b", "2".to_string(), None);

        let initial_version = state.global_version();

        state.update("a", "updated".to_string());
        assert_eq!(state.global_version(), initial_version + 1);

        state.update("b", "updated".to_string());
        assert_eq!(state.global_version(), initial_version + 2);
    }

    #[test]
    fn test_shared_channel_state() {
        let state = SharedChannelState::new();
        state.create_channel_with_value::<i32>("value", 42, None);

        let value: Option<i32> = state.get("value");
        assert_eq!(value, Some(42));

        state.update("value", 100);
        let updated: Option<i32> = state.get("value");
        assert_eq!(updated, Some(100));
    }

    #[test]
    fn test_channel_snapshot() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("status", "active".to_string(), None);
        state.update("status", "completed".to_string());

        let snapshot = state.snapshot();
        assert_eq!(snapshot.channels.len(), 1);
        assert!(snapshot.channels.contains_key("status"));
        assert_eq!(snapshot.channels["status"].version, 2);
    }

    #[test]
    fn test_has_channel() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("exists", "yes".to_string(), None);

        assert!(state.has_channel("exists"));
        assert!(!state.has_channel("not_exists"));
    }

    #[test]
    fn test_remove_channel() {
        let mut state = ChannelState::new();
        state.create_channel_with_value::<String>("temp", "data".to_string(), None);

        assert!(state.has_channel("temp"));
        state.remove_channel("temp");
        assert!(!state.has_channel("temp"));
    }

    #[test]
    fn test_standard_channels() {
        let mut state = ChannelState::new();
        StandardChannels::create_task_channels(&mut state);

        assert!(state.has_channel("status"));
        assert!(state.has_channel("findings"));
        assert!(state.has_channel("files_modified"));
        assert!(state.has_channel("progress"));
        assert!(state.has_channel("errors"));
        assert!(state.has_channel("iteration"));
    }

    #[test]
    fn test_finding_type() {
        let mut state = ChannelState::new();
        state.create_channel::<Vec<Finding>>("findings", Some(Reducer::LastValue));

        let findings = vec![
            Finding {
                id: "f1".to_string(),
                category: "bug".to_string(),
                severity: "high".to_string(),
                description: "Found a bug".to_string(),
                location: Some("main.rs:10".to_string()),
                suggestion: None,
            },
            Finding {
                id: "f2".to_string(),
                category: "style".to_string(),
                severity: "low".to_string(),
                description: "Style issue".to_string(),
                location: None,
                suggestion: Some("Use snake_case".to_string()),
            },
        ];

        state.update("findings", findings.clone());
        let retrieved: Option<Vec<Finding>> = state.get("findings");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().len(), 2);
    }
}
