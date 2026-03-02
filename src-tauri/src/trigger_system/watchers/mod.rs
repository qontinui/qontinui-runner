//! Watchers that generate trigger events from external sources.
//!
//! Each watcher monitors a specific event source and sends TriggerEvent
//! messages to the TriggerService via its mpsc channel.

pub mod file_watcher;
pub mod git_watcher;
pub mod health_check;
pub mod schedule;
pub mod webhook;
pub mod workflow_chain;
