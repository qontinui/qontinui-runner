//! Harness markdown -> work-unit adapter.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-06-18-harness-markdown-to-workunit-adapter.md`
//! (P2 of the plan-decoupling program).
//!
//! ## Why this lives in the runner
//!
//! coord used to auto-discover plans by scanning a filesystem root and
//! parsing each `*.md` into a `coord.plans` row
//! (`qontinui-coord/src/plan_ingest_worker.rs`). That coupling bakes an
//! operator-specific markdown convention into the shared coordination plane
//! and is *lossy* — it keeps only slug + a fixed-vocabulary status and throws
//! away the phase structure and dependency edges that are the real value of a
//! plan.
//!
//! The plan-decoupling program moves that parse OUT of coord (the shared
//! plane) and INTO the operator harness, where it can be as rich as we like
//! without coupling the shared plane. The markdown plan is the **source**, the
//! work-unit DAG is the **IR**, and coord is the **runtime** that executes the
//! IR — coord must not parse source.
//!
//! ## Phase 1 (this module today)
//!
//! [`parser`] is the pure, IO-free front-end of that compiler: operator plan
//! markdown -> [`parser::ParsedWorkUnit`]. It is *richer* than coord's old
//! projection — it also extracts the phase structure (-> sub-units) and the
//! declared dependency edges — and it treats status as **opaque** (it
//! tokenizes against an operator-supplied [`parser::PlanConvention`] but never
//! *rejects* an unknown status the way coord's fixed vocabulary did), matching
//! the opaque-status work-unit model of the P1 work-unit API.
//!
//! Later phases (the push client that calls coord's `/coord/work-units/*` API,
//! and the periodic trigger) slot in as siblings of [`parser`] under this
//! module; they are deliberately absent until the P1 work-unit API lands.

pub mod parser;

pub use parser::{
    parse_work_unit, slug_from_filename, ParsedPhase, ParsedWorkUnit, PlanConvention,
};
