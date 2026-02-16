//! Dynamic Step Injection System
//!
//! Allows agentic phase AI output to inject new verification steps into subsequent
//! loop iterations. Steps are parsed from `[INJECT_STEP]...[/INJECT_STEP]` markers
//! in the AI's output stream.
//!
//! ## Module structure
//!
//! - `types` - StepInjectionContext
//! - `parser` - InjectedStepParser state machine

#![allow(dead_code)]

pub mod parser;
pub mod types;
