//! termhost-probe — termhost-bench's outside-in instrument (plan
//! `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 2).
//!
//! It reads composited frames and OS process accounting from OUTSIDE every
//! terminal host, so the runner, VS Code, Windows Terminal and tmux are measured
//! by one instrument (D1):
//!
//! - [`codec`] / [`raster`]: the marker codec of Phase 1's `marker.mjs`, and a
//!   decoder that reads markers off captured pixels — robust to anti-aliasing
//!   and scaling, and rejecting (never misdecoding) altered colours;
//! - [`model`]: the pure engine — first appearances, update cadence, interval
//!   validity (foreground, uncloaked, unoccluded, no operator input), hangs,
//!   marker loss, the WGC/DDA cross-check — tested event by event;
//! - [`engine`] + [`platform`]: capture threads (WGC + DXGI duplication on
//!   Windows, X11 damage + shm on Linux), window state, the real-input listener,
//!   the 500 ms process sampler;
//! - [`doctor`], [`watch`], [`inject`], [`selftest`]: the subcommands.

pub mod cli;
pub mod clock;
pub mod clockcheck;
pub mod codec;
pub mod doctor;
pub mod engine;
pub mod feed;
pub mod inject;
pub mod model;
pub mod platform;
pub mod raster;
pub mod records;
pub mod sampler;
pub mod selftest;
pub mod spec;
pub mod stats;
pub mod watch;
