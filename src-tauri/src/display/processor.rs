use std::collections::HashMap;
use std::sync::Arc;
use serde::Serialize;
use crate::display::{DisplayProfile, EventLog, ViewType};

/// Central processor that applies display profiles to event data
pub struct DisplayProcessor {
    /// Event log storage
    event_log: EventLog,

    /// Registered display profiles
    profiles: HashMap<String, Arc<dyn DisplayProfile<Output = Box<dyn erased_serde::Serialize + Send>>>>,
}

impl DisplayProcessor {
    pub fn new() -> Self {
        Self {
            event_log: EventLog::new(),
            profiles: HashMap::new(),
        }
    }

    /// Register a display profile
    pub fn register_profile<P>(&mut self, profile: P) -> &mut Self
    where
        P: DisplayProfile + 'static,
        P::Output: Serialize + Send + 'static,
    {
        let name = profile.name().to_string();

        // Wrap the profile to erase the Output type
        struct ProfileWrapper<P>(P);

        impl<P> DisplayProfile for ProfileWrapper<P>
        where
            P: DisplayProfile,
            P::Output: Serialize + Send + 'static,
        {
            type Output = Box<dyn erased_serde::Serialize + Send>;

            fn process(&self, events: &[crate::display::RawEvent]) -> Self::Output {
                let output = self.0.process(events);
                Box::new(output)
            }

            fn name(&self) -> &str {
                self.0.name()
            }

            fn view_type(&self) -> ViewType {
                self.0.view_type()
            }

            fn can_process(&self, events: &[crate::display::RawEvent]) -> bool {
                self.0.can_process(events)
            }
        }

        self.profiles.insert(name, Arc::new(ProfileWrapper(profile)));
        self
    }

    /// Get the event log (mutable)
    pub fn event_log_mut(&mut self) -> &mut EventLog {
        &mut self.event_log
    }

    /// Get the event log (immutable)
    #[allow(dead_code)]
    pub fn event_log(&self) -> &EventLog {
        &self.event_log
    }

    /// Get view data for a specific profile
    pub fn get_view(&self, profile_name: &str) -> Result<serde_json::Value, String> {
        let profile = self.profiles
            .get(profile_name)
            .ok_or_else(|| format!("Profile not found: {}", profile_name))?;

        let output = profile.process(self.event_log.events());

        serde_json::to_value(&output)
            .map_err(|e| format!("Failed to serialize view data: {}", e))
    }

    /// Get view data by view type
    #[allow(dead_code)]
    pub fn get_view_by_type(&self, view_type: ViewType) -> Result<serde_json::Value, String> {
        let profile = self.profiles
            .values()
            .find(|p| p.view_type() == view_type)
            .ok_or_else(|| format!("No profile found for view type: {:?}", view_type))?;

        let output = profile.process(self.event_log.events());

        serde_json::to_value(&output)
            .map_err(|e| format!("Failed to serialize view data: {}", e))
    }

    /// List all registered profiles
    #[allow(dead_code)]
    pub fn list_profiles(&self) -> Vec<(String, ViewType)> {
        self.profiles
            .iter()
            .map(|(name, profile)| (name.clone(), profile.view_type()))
            .collect()
    }

    /// Clear the event log
    pub fn clear_events(&mut self) {
        self.event_log.clear();
    }
}

impl Default for DisplayProcessor {
    fn default() -> Self {
        Self::new()
    }
}
