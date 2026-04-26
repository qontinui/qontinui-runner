//! Build-id reporting command.
//!
//! Returns the value of the `RUNNER_BUILD_ID` env var that `build.rs` baked
//! into the binary at compile time. The value is read by `build.rs` from
//! `dist/build-id.txt`, which `vite.config.ts` writes during `npm run build`
//! — so the binary's compile-time stamp matches the `<meta name="build-id">`
//! tag in the embedded `index.html` exactly. This lets the React refresh
//! banner detect "binary swapped while my webview was still open" without
//! firing on every cold spawn.
//!
//! Registered through the central `tauri::generate_handler!` in
//! `main.rs`. Per `proj_tauri_plugin_naming_trap` we deliberately do NOT
//! introduce a per-module `PluginBuilder::new("qontinui_*")` here: Tauri 2
//! rejects underscore-named plugin prefixes and the runner's IPC contract
//! depends on bare `invoke("get_build_id")` resolving without a plugin
//! prefix.

/// Return the build-id of the currently-running runner binary.
///
/// The value is fixed at compile time by `build.rs` (which reads
/// `dist/build-id.txt`, written by `vite.config.ts`). For any runner
/// produced by the supervisor's standard build sequence (`npm run build`
/// then `cargo build`), this string matches the `<meta name="build-id">`
/// tag served from the embedded HTML — the React refresh banner relies on
/// that invariant so it only fires when the binary on disk has been
/// replaced behind a still-open webview.
#[tauri::command]
pub fn get_build_id() -> String {
    env!("RUNNER_BUILD_ID").to_string()
}
