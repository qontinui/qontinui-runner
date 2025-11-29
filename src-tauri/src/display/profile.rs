use crate::display::RawEvent;
use serde::Serialize;

/// Type of view this profile generates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ViewType {
    ActionLog,
    StateTimeline,
    TransitionFlow,
    ExecutionStats,
}

/// Trait for display profiles that transform raw events into view-specific data
pub trait DisplayProfile: Send + Sync {
    /// The output type for this profile (must be serializable)
    type Output: Serialize;

    /// Process raw events into structured view data
    fn process(&self, events: &[RawEvent]) -> Self::Output;

    /// Get the name of this profile
    fn name(&self) -> &str;

    /// Get the view type this profile generates
    #[allow(dead_code)]
    fn view_type(&self) -> ViewType;

    /// Optional: Check if this profile can handle the given events
    #[allow(dead_code)]
    fn can_process(&self, _events: &[RawEvent]) -> bool {
        true
    }
}
