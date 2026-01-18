//! Step Types Module
//!
//! Defines step types for the automation system.
//! Steps are organized by category:
//! - GUI automation (workflow, state, action)
//! - Web automation (playwright)
//! - AWAS automation (discover, execute, check_support, list_actions, extract_elements)

pub mod awas;

#[allow(unused_imports)]
pub use awas::*;
