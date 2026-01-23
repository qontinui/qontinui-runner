//! Test Orchestrator Module
//!
//! AI-driven multi-step API test orchestration system that enables:
//! - Automatic execution planning from a set of saved API requests
//! - Variable chaining between steps (extract from response, use in next request)
//! - AI-powered verification test code generation from execution results
//!
//! ## Architecture
//!
//! ```text
//! User Input (selected requests + test description)
//!     │
//!     ▼
//! ┌─────────────┐
//! │   Planner   │ ← AI creates execution plan with variable mappings
//! └─────────────┘
//!     │
//!     ▼
//! ┌─────────────┐
//! │  Executor   │ ← Runs HTTP requests, chains variables
//! └─────────────┘
//!     │
//!     ▼
//! ┌─────────────┐
//! │  Generator  │ ← AI generates verification test code
//! └─────────────┘
//! ```

pub mod executor;
pub mod planner;
pub mod types;

pub use executor::TestOrchestrator;
pub use planner::{create_orchestration_plan, generate_test_from_execution};
pub use types::*;
