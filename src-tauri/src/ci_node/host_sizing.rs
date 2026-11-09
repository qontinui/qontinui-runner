//! Host-derived parallelism sizing for one CI dispatch — re-exported from
//! [`qontinui_types::host_sizing`].
//!
//! The sizing rule (3 GiB per cargo build token, 1 GiB per test process, the
//! 8 GiB small-host floor, the per-slot suggestion and the 1/N `share`) used to
//! live here. It moved to `qontinui-schemas/rust` so the runner, the supervisor
//! and the bash build wrappers (through a golden table pinned there) all size
//! cargo from ONE formula. Its rationale and its unit tests live with it; this
//! module keeps only the CI lane's import path. Plan
//! `2026-10-02-cargo-jobs-supervisor-build-pool-and-physical-disk-floors-follow-the-machine`,
//! design decision D2.
//!
//! `probe` comes from the `host-probe` feature, which the runner enables on
//! its `qontinui-types` dependency.

pub(crate) use qontinui_types::host_sizing::{
    derive, probe, share, suggested_concurrent_builds, suggestion_with_limit, HostCapacity,
    HostSizing,
};
