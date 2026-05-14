//! IR + Legacy wire types are now canonical in `qontinui-schemas`
//! (`qontinui_types::ir`). This shim preserves the existing import path
//! (`crate::spec_api::types::*`) so runner-internal callers keep compiling
//! without churn. New code should import from `qontinui_types::ir`
//! directly.

pub use qontinui_types::ir::*;
