//! Shared routing helpers for UI Bridge submodules.
//!
//! The UI Bridge HTTP surface predates the control/ai namespace
//! consolidation: several agent-facing endpoints must remain reachable
//! under BOTH `/ui-bridge/control/<tail>` and `/ui-bridge/ai/<tail>`.
//! Historically every submodule's `routes()` spelled this pairing out
//! by hand — two separate `.route(...)` calls — which made renames
//! fragile (changing one side silently 404s the other).
//!
//! [`add_dual!`] makes the aliasing visible-by-design: registers the
//! same handler under both namespaces in one call. Use it inside a
//! submodule's `routes()` function.
//!
//! Only use it for routes where the control-side and ai-side point at
//! the SAME handler function. Endpoints with namespace-specific logic
//! (e.g. `/control/snapshot` vs `/ai/snapshot`, different handlers)
//! must stay split.
//!
//! **Manifest entries:** the matching static tuples in
//! `route_entries()` stay as two explicit literals — slice literals
//! cannot accept a macro that expands to multiple elements. The drift
//! test in `mod.rs::manifest_drift_tests` is aware of `add_dual!(...)`
//! invocations and synthesises both `/control/<tail>` and `/ai/<tail>`
//! registrations from them, so adding a new alias through the macro
//! will still be cross-checked against the manifest.

/// Register the given handler at both `/ui-bridge/control/<tail>` and
/// `/ui-bridge/ai/<tail>` in a single chained call.
///
/// `$method` is a method constructor identifier from `axum::routing`
/// (e.g. `get`, `post`, `put`) already in scope at the call site.
/// `$tail` must be a string literal and must NOT include the
/// `/ui-bridge/` prefix or either namespace — e.g.
/// `"wait-for-navigation"`, not `"/ui-bridge/control/wait-for-navigation"`.
///
/// Expansion:
/// ```ignore
/// add_dual!(router, post, "wait-for-navigation", handler)
/// // =>
/// router
///     .route("/ui-bridge/control/wait-for-navigation", post(handler))
///     .route("/ui-bridge/ai/wait-for-navigation",      post(handler))
/// ```
macro_rules! add_dual {
    ($router:expr, $method:ident, $tail:literal, $handler:expr) => {{
        $router
            .route(concat!("/ui-bridge/control/", $tail), $method($handler))
            .route(concat!("/ui-bridge/ai/", $tail), $method($handler))
    }};
}

pub(super) use add_dual;
