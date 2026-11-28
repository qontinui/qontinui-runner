//! The memory knowledge graph: the in-memory graph engine, its PostgreSQL
//! builder, its node/edge types, unified search over it, and the trigram
//! fuzzy matcher the similar-errors endpoint uses.
//!
//! These lived in `reflection/` until plan
//! `2026-11-19-delete-the-runner-ai-workflow-loop-engine` Phase 1: they serve
//! the memory and graph APIs, not the reflection loop, so they move out
//! before `reflection/` is deleted.

pub mod fuzzy_matching;
pub mod graph_engine;
pub mod graph_engine_pg;
pub mod graph_types;
pub mod unified_search;
