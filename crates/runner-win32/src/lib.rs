//! Low-level Win32 process-containment primitives for the runner.
//!
//! Carved out of the `qontinui-runner` binary crate's module tree (plan
//! `2026-08-21-runner-extract-crates-frontier-first`, Phase 1, crate 2).
//!
//! **Why this is the SECOND crate and not the last.** The predecessor plan's
//! Phase 0 ended on a Windows failure that a green `cargo check` cannot see:
//! `0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND`, raised at LOAD time, after a clean
//! 39m59s compile. Raw `extern "system"` blocks resolved by symbol name are the
//! construct that produces it, and `job_object` carried one. Retiring that risk
//! on a one-module crate is far cheaper than discovering it on forty — so this
//! crate is deliberately sequenced immediately after the trivial proof crate.
//!
//! The raw extern is gone (see `job_object`): this crate declares
//! `Win32_Security` in its own feature list and calls `CreateJobObjectW`
//! through `windows-sys` like every other Win32 entry point here, so nothing
//! links by hand-declared name.

// `windows-sys` is a TARGET-GATED dependency (see Cargo.toml), so the module
// that uses it must be gated to match. `ci.yml` builds on ubuntu-latest as well
// as windows-latest; without this the crate does not compile there at all.
//
// The binary crate gated its `mod job_object;` the same way. That gate is easy
// to drop on the way out -- removing the `mod` line alone leaves the attribute
// orphaned onto whatever declaration follows it, silently cfg-ing OUT an
// unrelated module on non-Windows. It happened once while writing this commit
// and only a Linux build would have caught it.
#[cfg(windows)]
pub mod job_object;

#[cfg(windows)]
pub use job_object::{
    assign_process_to_job, current_job_pid_saturation, init_job_object, ScopedKillOnCloseJob,
};
