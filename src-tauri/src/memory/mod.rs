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
//! - `tenant_sync` — Consent-gated outbox emitter + drain mirroring high-value
//!   memories into the tenant agentic-memory web API

pub mod consolidation;
pub mod contradiction;
pub mod decay;
pub mod entity_profiles;
pub mod importance;
pub mod memory_synthesis;
pub mod scheduler;
pub mod tenant_sync;
pub mod unified_query;
pub mod working_representation;
