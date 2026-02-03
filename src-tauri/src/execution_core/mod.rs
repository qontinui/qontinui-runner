//! Execution Core Module
//!
//! Shared execution primitives used by both Flow Designer and Unified Workflow systems.
//! This module extracts common functionality to enable code reuse while maintaining
//! the unique orchestration models of each system:
//!
//! - **Unified Workflow**: Phase-based (Setup → [Verification ↔ Agentic]* → Completion)
//! - **Flow Designer**: Graph-based (arbitrary node connections, branching, loops, parallel)
//!
//! ## Modules
//!
//! - `builtin_tools`: Built-in tool implementations (JSON, string, array, timestamp, etc.)
//! - `conditions`: Condition evaluation for flow control
//! - `expressions`: Expression evaluation for variable resolution
//! - `events`: Execution event types and emission
//! - `results`: Unified step result types
//! - `unified_tools`: Unified Workflow step types exposed as Flow Designer tools

pub mod builtin_tools;
pub mod conditions;
pub mod events;
pub mod expressions;
pub mod results;
pub mod unified_tools;

// Re-exports for convenience - used by external crates consuming this API
#[allow(unused_imports)]
pub use builtin_tools::*;
#[allow(unused_imports)]
pub use conditions::*;
#[allow(unused_imports)]
pub use events::*;
#[allow(unused_imports)]
pub use expressions::*;
#[allow(unused_imports)]
pub use results::*;
#[allow(unused_imports)]
pub use unified_tools::*;
