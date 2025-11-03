// Display Profile Framework
//
// This module provides a framework for processing raw execution events
// into structured view data for different frontend displays.
//
// Architecture:
// - EventLog: Stores raw events from the executor
// - DisplayProfile: Trait for transforming events into view data
// - DisplayProcessor: Applies profiles to events

pub mod event_log;
pub mod profile;
pub mod processor;
pub mod profiles;

// Tests
#[cfg(test)]
mod event_log_test;

pub use event_log::{EventLog, RawEvent};
pub use profile::{DisplayProfile, ViewType};
pub use processor::DisplayProcessor;
