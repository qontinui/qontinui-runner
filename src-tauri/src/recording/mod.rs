//! Recording & Playback Module
//!
//! Enables recording user interactions in the browser and generating
//! executable scripts (Python, Playwright, pytest) for automation.
//!
//! # Architecture
//!
//! 1. **Browser Extension** captures user actions (clicks, typing, navigation)
//! 2. **WebSocket** sends RECORDING_SNAPSHOT messages to the runner
//! 3. **Storage** persists actions in SQLite for later script generation
//! 4. **Script Generator** produces executable automation scripts
//!
//! # Supported Export Formats
//!
//! - `python` - Pure Python using UI Bridge client
//! - `playwright` - Playwright TypeScript test
//! - `pytest` - pytest-playwright test structure

pub mod background_capture;
pub mod content_filter;
pub mod script_generator;
pub mod storage;
pub mod types;

pub use script_generator::ScriptGenerator;
pub use storage::RecordingStorage;
pub use types::*;
