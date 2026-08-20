//! Shim argv **classification** — re-export shim over the lib-crate canonical
//! source ([`crate::intercept_core::classify`]).
//!
//! The classify logic moved into the lib crate (plan §4 Phase 4 option (a)) so
//! the standalone `qontinui-shim` Windows `.exe` stub can share it — a second
//! binary cannot import from this bin's module tree. The producer code keeps
//! referring to `intercept::classify::…` unchanged via this re-export; the unit
//! tests + the canonical doc live with the source in
//! `intercept_core::classify`.

pub use crate::intercept_core::classify::*;
