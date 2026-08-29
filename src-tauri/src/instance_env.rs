//! Runner instance identity **as read from the process env** — the one
//! canonical `QONTINUI_INSTANCE_NAME` reader.
//!
//! In the LIB crate for the same reason as `runner_breadcrumb` / `mcp_spill`:
//! a second bin cannot import from the runner bin's module tree, and
//! `bin/qontinui_profile.rs` needs the SAME primary/secondary predicate the
//! runner bin's tier-persist guard uses ([`crate::profiles::promote_tier_to_account`]).
//! One module ⇒ one predicate ⇒ the two doors cannot drift.
//!
//! The rest of `crate::instance` (path scoping, `RunnerKind`, the WebView2
//! data dir, primary registration) stays in the runner bin: it reaches into
//! `crate::mcp::types` and `crate::session`, which are bin-only. `instance.rs`
//! re-exports these two functions, so every `crate::instance::is_secondary()`
//! call site in the bin resolves here.

/// The raw instance name from the env, if set and non-empty.
///
/// The supervisor sets `QONTINUI_INSTANCE_NAME` when spawning non-primary
/// runners (temp runners, named runners).
pub fn instance_name() -> Option<String> {
    std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// True when this runner was launched as a non-primary instance.
///
/// This is the CONSERVATIVE side of the primary/secondary distinction, and it
/// is deliberately conservative for the settings-write guard: a secondary
/// launched with only `QONTINUI_INSTANCE_NAME` (no `QONTINUI_CONFIG_DIR`)
/// resolves the primary's SHARED `settings.json`, so any writer that could
/// demote the primary must refuse on this predicate alone.
///
/// Note this is a WEAKER check than
/// `process_capture::primary_proxy::is_secondary` (which additionally requires
/// a primary port to proxy to) and than `instance::data_subdir`'s fail-closed
/// `resolve_data_subdir` (which additionally detects a NAMELESS secondary by
/// port). Those exist for different questions — path isolation and request
/// proxying — and both build on this one.
pub fn is_secondary() -> bool {
    instance_name().is_some()
}
