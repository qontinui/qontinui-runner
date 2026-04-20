//! Rathole-based reverse tunnel for reaching the runner from off-network devices.
//!
//! Plan 1B replaces the ephemeral Cloudflare quick-tunnel with a self-hosted
//! rathole relay. Because rathole is distributed as a binary and does not
//! publish a library crate, `rathole_client` spawns the rathole executable as
//! a child process with a generated TOML config.
//!
//! Layout:
//! - [`config`] — `RatholeConfig` / `TunnelService` + TOML generation
//! - [`rathole_client`] — child-process supervisor (start / stop / is_connected)

pub mod config;
pub mod rathole_client;

pub use config::{RatholeConfig, TunnelService};
pub use rathole_client::RatholeClient;
