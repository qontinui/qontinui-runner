//! The host build-admission broker (plan
//! `2026-10-06-builds-are-admitted-per-invocation-against-measured-host-memory`,
//! Phase 2): the cargo wrappers open a ticket per build over loopback, and the
//! broker decides — with the pure core in `qontinui_types::build_admission` —
//! when it may start.
//!
//! **This build is OBSERVE-ONLY.** Every ticket is granted at once and nothing
//! about the build changes; the broker records what enforcement WOULD have
//! done (a shadow schedule), which is the evidence Phase 4's shadow week reads.
//!
//! | Module | Owns |
//! |---|---|
//! | [`broker`] | the ledger: tickets, leases, per-ticket secrets, the shadow schedule |
//! | [`facts`] | host facts with provenance: MemAvailable, PSI, build-tree attribution, non-build p95, frozen state from `cgroup.events` |
//! | [`ci_reservation`] | the CI slices' reservation, descending an unlimited top-level slice |
//! | [`policy`] | level + parameters by the plan's local precedence |
//! | [`persist`] | the files under `~/.qontinui/build-admission/` |
//! | [`routes`] | the loopback routes and the 2 s tick |
//!
//! Activation is the runner's own: a box runs this broker once its runner runs
//! a build containing it. Nothing restarts a runner for it.

pub mod broker;
pub mod ci_reservation;
pub mod facts;
pub mod persist;
pub mod policy;
pub mod routes;
