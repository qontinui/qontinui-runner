//! Autoresearch — autonomous workflow optimization loop.
//!
//! Inspired by Karpathy's autoresearch pattern: an autonomous experiment loop that
//! modifies config → runs workflow → measures pass rate/iterations/duration → keeps
//! or discards → repeats. A meta-layer above the existing workflow execution system.
//!
//! # Architecture
//!
//! ```text
//! ResearchEngine (async loop)
//!   │
//!   ├─ PICK next experiment (mutation strategy)
//!   ├─ RUN trial via RunnerClient::start_workflow_with_overrides
//!   ├─ MEASURE aggregate metrics (pass_rate, iterations, duration)
//!   ├─ COMPARE to control
//!   ├─ DECIDE: accept or reject
//!   ├─ LOG to autoresearch_experiments table
//!   └─ REPEAT until stopped
//! ```
//!
//! # Workflow Architectures
//!
//! The `WorkflowArchitecture` search dimension allows comparing three execution strategies:
//!
//! - **Traditional**: Setup → [Deterministic Verification ↔ Agentic Fix]* → Completion
//! - **Agentic Verification**: [Verification Agent → Worker Agent]* loop
//! - **Multi-Agent Pipeline**: Specialized agents in a DAG-structured pipeline
//!   (Spec Analyst → DAG Builder → Locator → Implementers → Verifiers)
//!
//! See `agentic_verification` module for the alternative architecture designs.

pub mod agentic_verification;
pub mod commands;
pub mod engine;
pub mod metrics;
pub mod mutations;
pub mod types;
