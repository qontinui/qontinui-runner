//! Build-id reporting command.
//!
//! Returns the value of the `RUNNER_BUILD_ID` env var that `build.rs` baked
//! into the binary at compile time (format `<git-sha-short>-<unix-ms>`).
//! Paired with the `<meta name="build-id">` tag injected into the embedded
//! `index.html` at Vite-build time, this lets the React refresh banner
//! detect "binary swapped while my webview was still open" — the only
//! divergence vector for a runner whose frontend is fully embedded.
//!
//! Registered through the central `tauri::generate_handler!` in
//! `main.rs`. Per `proj_tauri_plugin_naming_trap` we deliberately do NOT
//! introduce a per-module `PluginBuilder::new("qontinui_*")` here: Tauri 2
//! rejects underscore-named plugin prefixes and the runner's IPC contract
//! depends on bare `invoke("get_build_id")` resolving without a plugin
//! prefix.

/// Return the build-id of the currently-running runner binary.
///
/// The value is fixed at compile time by `build.rs`. Two runner binaries
/// produced by separate `cargo build` runs will always report different
/// values (the unix-millis suffix changes), even when the git SHA is
/// unchanged.
#[tauri::command]
pub fn get_build_id() -> String {
    env!("RUNNER_BUILD_ID").to_string()
}
