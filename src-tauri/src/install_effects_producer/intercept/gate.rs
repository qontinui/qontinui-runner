//! The interception **gate decision** — re-export shim over the lib-crate
//! canonical source ([`qontinui_runner_lib::intercept_core::gate`]).
//!
//! The pure `should_block` truth table + `InterceptMode` parse moved into the
//! lib crate (plan §4 Phase 4 option (a)) so the standalone `qontinui-shim`
//! Windows `.exe` stub shares it. The producer + materializer keep referring to
//! `intercept::gate::…` unchanged via this re-export; the canonical truth-table
//! doc + unit tests live with the source in `intercept_core::gate`.

pub use qontinui_runner_lib::intercept_core::gate::*;
