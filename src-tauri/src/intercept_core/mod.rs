//! **Pure** install-interception core — the lib-crate home of the classify +
//! gate logic shared by two binaries (plan §4 Phase 4).
//!
//! ## Why this lives in the lib crate
//! `install_effects_producer` is rooted in the `qontinui-runner` BIN target
//! (`mod install_effects_producer;` in `main.rs`), so a SECOND binary (the
//! Windows `.exe` shadow stub `qontinui-shim`, plan §6) cannot import from its
//! module tree — a bin can't `use` another bin's modules. The plan (§4 Phase 4
//! option (a)) calls for moving the PURE pieces — argv classification and the
//! gate truth table, plus the small wire types they need — into the lib crate so
//! BOTH the runner bin and the shim bin share one source of truth.
//!
//! What moved here (pure, no bin-only state, no async, no coord/keyring deps):
//! - [`types`] — `PackageManager` + `PackageSpecInput` (the coord-wire mirrors).
//! - [`classify`] — install-verb tables + argv → `(packages, dev, lockfile_sync)`.
//! - [`gate`] — the `should_block` truth table + `InterceptMode` parse.
//!
//! What stayed in the bin (`install_effects_producer`): everything that touches
//! the coord HTTP client, the keyring, the producer route handlers, the
//! `PreContext` TTL stash, FS observation, and the per-terminal shim
//! materializer — none of which the stub binary needs. The bin's
//! `install_effects_producer::intercept::{classify,gate}` are now thin
//! re-export shims over these modules (the producer code reads them unchanged).
//!
//! ## Registry credentials (plan §4 Phase 4)
//! Interception does NOT inject registry secrets into agent shells. The agent's
//! shell carries the operator's normal registry config (the `.npmrc` /
//! `~/.cargo/credentials` / pip index auth it already has); the shim only
//! observes. The producer's pre-call dry-run uses the OS keyring as it does for
//! the explicit route (parent plan #441 Phase 4). Interception OBSERVES; it does
//! not re-auth. This module is therefore credential-free by construction — there
//! is nothing here that reads or forwards a secret.

pub mod classify;
pub mod gate;
pub mod types;
