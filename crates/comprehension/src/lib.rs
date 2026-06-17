//! Frontend comprehension → `FunctionalSpec` (component **#3** of the
//! website→mobile regeneration program).
//!
//! Comprehension is the INVERSE of app-gen: it turns an *observed live frontend*
//! (a UI Bridge snapshot + a discovery `StateDiscoveryResult`, optionally an
//! AWAS manifest) into a populated [`qontinui_types::functional_spec::FunctionalSpec`]
//! with **honest** `SpecProvenance` — the artifact #1/#2 consume and the verify
//! phase scores via `evaluate_completeness`.
//!
//! ## Phase 1 scope (this crate, today)
//!
//! The pipeline is deliberately split into a deterministic, golden-testable core
//! and a runtime-deferred LLM/capture shell:
//!
//! - **Deterministic (tested here):** [`mapping`] (snapshot/discovery →
//!   `IrState`/`IrAssertion`/`IrTransition` + the evidence-class map), [`clamp`]
//!   (the honest-provenance clamp + ledger collation), and [`worker::assemble_spec`]
//!   (overlay IR onto the inferred spec, clamp, collate). These have NO LLM and
//!   NO clock — they are locked down by the oracle + anti-overpromise golden
//!   tests in `tests/`.
//! - **Runtime-deferred (NOT faked):** [`llm`] wires the `claude` CLI
//!   structured-output subprocess (the inference step) and [`worker::comprehend`]
//!   /the live UI Bridge + discovery capture. These are exercised only by
//!   `#[ignore]`d tests because their output is non-deterministic (LLM) or
//!   requires a live page.
//!
//! ## The honesty invariant
//!
//! A node's `SpecProvenance` is never higher than its weakest evidence justifies.
//! The LLM may over-claim; [`clamp::clamp_provenance`] deterministically
//! downgrades any node whose known evidence class forbids the claim, and forces
//! every `OperationEffect` to `Assumed`. This is what makes comprehension's
//! "measured, not assumed" coverage premise machine-checkable.

pub mod clamp;
pub mod input;
pub mod llm;
pub mod mapping;
pub mod worker;
