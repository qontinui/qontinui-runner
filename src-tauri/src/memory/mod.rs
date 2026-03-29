//! Memory Module
//!
//! Unified memory retrieval with Reciprocal Rank Fusion (RRF), plus consolidation
//! services that synthesize raw observations into higher-level "mental models".
//!
//! ## Submodules
//!
//! - `unified_query` — Fans out queries to all memory stores in parallel and fuses via RRF
//! - `importance` — Importance scoring (type-based + confirmation/revision/access/task boosts)
//! - `decay` — Ebbinghaus-inspired forgetting curves with differential decay rates
//! - `consolidation` — 4-phase periodic consolidation: orient → gather → consolidate → prune

pub mod consolidation;
pub mod decay;
pub mod importance;
pub mod scheduler;
pub mod unified_query;
