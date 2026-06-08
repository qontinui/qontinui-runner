//! `qontinui-layers.toml` parser — the **authored declared side** for `Ξ_Layering`
//! (Phase 4b). The parser itself now lives in the `qontinui-code-graph` crate's
//! `layering` module (extracted in coord-layering-enforcement R4a — it is pure
//! `serde`/`toml`, needed coord-side too). This module re-exports it so existing
//! `super::layer_manifest::LayerManifest` references resolve unchanged.

pub use qontinui_code_graph::layering::LayerManifest;
