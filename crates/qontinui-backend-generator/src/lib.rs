//! Backend generator (#2 of the website→mobile regeneration program) — Phase 1.
//!
//! Turns a frozen-v0 [`FunctionalSpec`] + [`Profile`] into the **source of a fresh,
//! runnable FastAPI backend**. This crate owns the *deterministic* core: the package
//! scaffold, the SQLAlchemy/Pydantic models, and the FastAPI routes. Per the plan's
//! Fork A the *business-logic bodies* are later authored by a `claude` CLI codegen
//! subprocess (`run_prompt_sync`); Phase 1 deliberately stops at a deterministic,
//! verifiable scaffold so the spec→routes/schema seam is proven before any LLM enters.
//!
//! ## The load-bearing seam this crate proves (Phase 1)
//!
//! Every route this generator emits derives its `{method, path}` from the **shared**
//! [`qontinui_types::endpoint_for::endpoint_for`] — the single function the app
//! generator (#1) and this backend generator (#2) both call so they agree with no
//! #1→#2 artifact dependency (cross-plan reconciliation §1). The golden test asserts
//! the emitted `pairConfirm` route's method+path is byte-equal to
//! `endpoint_for(pairConfirm, profile)` (`POST /api/v1/devices/pair-confirm`), and the
//! emitted `Device` model has a column per spec field. That is the deterministic
//! proof of the #1↔#2 agreement and of the spec→backend mapping.
//!
//! ## Honesty boundary
//!
//! This crate emits **source files as strings** ([`GeneratedBackend`]); it does not run
//! anything itself. Two generation tiers exist:
//!
//! - [`scaffold::generate_phase1`] — the deterministic spec→source seam (stub handler,
//!   no persistence); its golden test proves the route/schema mapping without running.
//! - [`runtime::generate_runnable`] — a **runnable** backend that really persists (sync
//!   SQLAlchemy/psycopg, `create_all` on startup, a `pairConfirm` handler that inserts a
//!   `Device` and returns a token). The gated integration test
//!   (`tests/runtime_integration.rs`, `#[ignore]`d — needs Docker) brings it up under
//!   docker-compose, hits the live endpoint, introspects the live schema, and assembles
//!   a `CoverageEvidence` from those observations. So "coverage observed from a running
//!   server" (the plan's premise) **is** proven — but only when that `--ignored` test is
//!   run on a host with a Docker daemon (CI has none).

pub mod route_map;
pub mod runtime;
pub mod scaffold;

pub use route_map::{openapi_document, route_for, RouteBinding};
pub use runtime::{generate_runnable, BreakMode};
pub use scaffold::{generate_phase1, EmittedModel, EmittedRoute, GeneratedBackend};
