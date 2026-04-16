//! Saved orchestration-loop config CRUD types — runner-side shim.
//!
//! The DTO shapes (`OlConfig`, `CreateOlConfigRequest`, `UpdateOlConfigRequest`)
//! live in `qontinui-types::orchestration_config` and are re-exported here via
//! `pub use`. This file is now a thin shim; no runner-local types remain.
//!
//! See `scheduler.rs` / `unified_workflows.rs` for the general
//! `pub use qontinui_types::…::*;` pattern used across the runner after the
//! DTO extractions.

pub use qontinui_types::orchestration_config::{
    CreateOlConfigRequest, OlConfig, UpdateOlConfigRequest,
};
