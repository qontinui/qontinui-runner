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
//! This crate emits **source files as strings** ([`GeneratedBackend`]). It does NOT
//! run the backend, migrate a database, or hit a live endpoint — that runtime
//! verification (docker-compose postgres + uvicorn) is the integration tier and is
//! deferred (see the crate's `tests/` and the `run/` scaffold). Coverage observed
//! "from a running server" (the plan's premise) is therefore NOT proven here; only
//! the deterministic generation core is.

pub mod route_map;
pub mod scaffold;

pub use route_map::{openapi_document, route_for, RouteBinding};
pub use scaffold::{generate_phase1, EmittedModel, EmittedRoute, GeneratedBackend};
