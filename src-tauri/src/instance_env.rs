//! Runner LAUNCH identity **as read from the process env** — the one canonical
//! reader of `QONTINUI_INSTANCE_NAME` (which instance is this?) and
//! `QONTINUI_SERVER_MODE` (was it launched headless?).
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

/// `QONTINUI_SERVER_MODE` — was this process launched headless (`1` / `true`,
/// case-insensitive)? The ONE parse of that variable in the tree.
///
/// In the LIB for the same reason as [`is_secondary`], and then one more: the
/// runner bin's `launch_env::server_mode_from_env` re-exports it (so
/// `RunnerLaunchEnv` and `settings::load_settings_full` keep their call sites),
/// AND `profiles::read_runner_tier` needs it. That reader is the tier answer
/// every in-process coord consumer gets, and it has to agree with the tier
/// `settings::load_settings` resolves — a hardcoded `false` there meant a
/// headless NAMED secondary ran its relay as Tier 2 while
/// `profiles::connected_coord_base` returned `None` for the same process.
///
/// # Why a free function and not a field on the launch snapshot
///
/// The typed `RunnerLaunchEnv` snapshot lives on Tauri app state and is `None`
/// until `main()` has taken it, but settings (and the tier) are read from paths
/// that run before, beside and entirely outside `main()`'s setup — the
/// `config_report` path, the `qontinui_profile` bin, tests. A `None` there
/// would silently read as "not headless", i.e. the exact defect this shared
/// accessor exists to prevent. So the accessor is shared, not the value.
pub fn server_mode() -> bool {
    std::env::var("QONTINUI_SERVER_MODE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}
