// The runner's sidecar roster: the ONE list both build paths read.
//
//   - scripts/bundle-profile-sidecar.mjs (the `tauri build` bundle step) builds
//     SIDECAR_BINS in release and copies them to src-tauri/binaries/ for the
//     bundler's externalBin.
//   - scripts/build-exe.mjs (`pnpm run build:exe`, the primary's build) builds
//     PRIMARY_BINS beside qontinui-runner in the SAME cargo invocation and
//     profile, because the runner resolves every one of them from
//     `current_exe().parent()` at run time. A fresh profile dir (e.g. the
//     primary's `release-checked/`) otherwise holds no sidecar at all, and the
//     runner publishes a missing or 0-byte `qontinui-pr` onto PATH -- the
//     `qontinui-pr-shim-zero-byte-opens-no-pr` failure.
//
// Keep SIDECAR_BINS in sync with `tauri.conf.json` `bundle.externalBin` and
// `src-tauri/build.rs` `EXTERNAL_BIN_SIDECARS`; a build.rs test pins the
// SIDECAR_BINS line below to that list, so edit it as one line.

export const SIDECAR_BINS = ["qontinui_profile", "qontinui-pr", "qontinui-pty-holder"];

// The cargo PACKAGES those bins live in. `qontinui-pty-holder` (plan
// 2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions, Phase 2:
// the per-pane PTY holder the runner spawns from beside its own exe) is a bin
// of `crates/pty-holder`, not of this crate, so a build selects both packages;
// cargo applies the `--bin` filters across every selected package.
export const SIDECAR_PACKAGES = ["qontinui-runner", "qontinui-pty-holder"];

// Bins the PRIMARY needs beside its exe that are not bundle externalBins:
// `qontinui-shim` is materialized onto session PATHs from
// `current_exe().parent()` (install_effects_producer/intercept/shim_materializer.rs).
export const PRIMARY_EXTRA_BINS = ["qontinui-shim"];

// Everything build:exe builds beside qontinui-runner.
export const PRIMARY_BINS = [...SIDECAR_BINS, ...PRIMARY_EXTRA_BINS];
