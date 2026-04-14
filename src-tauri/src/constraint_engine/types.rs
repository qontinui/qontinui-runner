//! Types for the constraint engine.
//!
//! The DTO shape of these types lives in the `qontinui-types` crate
//! (`qontinui_types::constraints`); this module re-exports them so the rest
//! of the runner can continue to `use crate::constraint_engine::types::*`
//! without change. Business logic / evaluator code stays in the sibling
//! modules (`evaluator.rs`, `builtins.rs`, `config.rs`, `proposals.rs`).

pub use qontinui_types::constraints::{
    Constraint, ConstraintCheck, ConstraintResult, ConstraintSeverity, ConstraintViolation,
};
