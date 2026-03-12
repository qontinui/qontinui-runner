//! Constraint Engine for pre-verification validation.
//!
//! Evaluates declarative constraints against the state of the working directory
//! after the agentic phase applies changes but before the next verification cycle.
//! Violations are fed back to the AI as high-priority context — they never terminate
//! the workflow.
//!
//! ## How it differs from verification
//!
//! - **Verification** tests the application's behavior (does the filter work?)
//! - **Constraints** guard the repair process itself (is the fix safe? scoped? clean?)
//!
//! Constraints catch issues that verification might miss (secrets in code, scope
//! creep to unrelated directories) or that would waste a verification cycle to
//! discover (compilation errors in modified files).
//!
//! ## Flow
//!
//! ```text
//! AI applies fix (agentic phase)
//!     ↓
//! Constraint engine checks applied changes
//!     ↓ (violations found)
//! Inject violations into next iteration's failure context as Priority 0
//!     ↓
//! Verification still runs (constraints don't skip it)
//!     ↓
//! AI gets both constraint violations AND verification results
//! ```

mod builtins;
pub mod config;
mod evaluator;
pub mod proposals;
mod types;

pub use evaluator::ConstraintEngine;

// Public API types — used by consumers of this module (workflow config, MCP API)
#[allow(unused_imports)]
pub use types::{Constraint, ConstraintCheck, ConstraintResult, ConstraintSeverity};
