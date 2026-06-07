//! Pure wire types shared by the install-interception classify/gate logic
//! (plan §4 Phase 4 — the lib-crate move).
//!
//! These are the SINGLE definitions of `PackageManager` + `PackageSpecInput`:
//! the bin-target `install_effects_producer::types` re-exports them so the wide
//! producer code is unchanged, and the standalone `qontinui-shim` binary (the
//! Windows `.exe` shadow stub) imports them from the lib crate. Both consumers
//! therefore (de)serialize the SAME coord-wire shapes — no drift.

use serde::{Deserialize, Serialize};

/// Mirror of coord `install_effects::PackageManager`
/// (`#[serde(rename_all = "snake_case")]`). NO `pipenv` variant by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Cargo,
    Pip,
    Poetry,
}

impl PackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Yarn => "yarn",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Cargo => "cargo",
            PackageManager::Pip => "pip",
            PackageManager::Poetry => "poetry",
        }
    }
}

/// A requested spec on the route request (mirror of coord's `PackageSpec` with a
/// friendlier route field name). `name` + optional `version_req`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageSpecInput {
    pub name: String,
    #[serde(default)]
    pub version_req: Option<String>,
}
