# Build / run qontinui-runner with the DEV-ONLY `debug-tokio-console` feature so
# the `tokio-console` client can attach to the runtime's task graph.
#
# Phase 5 of `2026-08-30-runner-blocking-pool-exhaustion-and-wedge-diagnostics`.
# The native OS-level census (threads, handles, child processes) proves the
# process is wedged; only tokio-console shows WHICH async task is stalled, for
# how long, and on what. See src-tauri/docs/tokio-console.md.
#
# WHY THIS SCRIPT EXISTS AT ALL
# -----------------------------
# `console-subscriber` needs `--cfg tokio_unstable`, which is a BUILD-WIDE rustc
# flag: Cargo features cannot set it conditionally. Putting it in
# `src-tauri/.cargo/config.toml` would therefore apply it to every build of this
# crate, shipped release bundles included -- exactly what the plan forbids. So
# the flag is set NOWHERE in the repository and is passed at invocation time
# instead. `src-tauri/build.rs` fails the build with a one-line message if the
# feature is enabled without it.
#
# Two traps this script exists to avoid:
#
#  1. `RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS` *REPLACE* the `[target.*]
#     rustflags` in `src-tauri/.cargo/config.toml` -- Cargo does not merge them.
#     Setting RUSTFLAGS by hand therefore silently drops `/STACK:8388608` (needed
#     to link the large test binary on MSVC), `/Brepro`, and the sccache path
#     remaps. This script re-states them.
#  2. Plain `RUSTFLAGS` is split on whitespace with no quoting, so
#     `-C link-args=/STACK:8388608 /Brepro` cannot survive it. We use
#     `CARGO_ENCODED_RUSTFLAGS` (0x1f-separated), which can.
#
# Changing RUSTFLAGS invalidates the build cache: the first build after this
# switch (and the first one back) is a full rebuild of the dependency graph.
# That is expected, not a fault.
#
# Usage:
#   scripts\dev-tokio-console.ps1 -Action run
#   scripts\dev-tokio-console.ps1 -Action check -CargoArgs '--lib'
#
# Then, in a second terminal:
#   cargo install --locked tokio-console     # once
#   tokio-console http://127.0.0.1:6669      # or $env:TOKIO_CONSOLE_BIND

[CmdletBinding()]
param(
    [ValidateSet('run', 'check', 'build', 'test', 'clippy')]
    [string]$Action = 'run',

    # Extra arguments appended verbatim to the cargo invocation.
    [string[]]$CargoArgs = @()
)

$ErrorActionPreference = 'Stop'

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$srcTauri = Join-Path (Split-Path -Parent $scriptDir) 'src-tauri'

$hostTriple = (& rustc -vV | Select-String -Pattern '^host: ').ToString() -replace '^host: ', ''

# Keep in sync with src-tauri/.cargo/config.toml [target.*] rustflags.
if ($hostTriple -like '*windows-msvc*') {
    $flags = @(
        '-C', 'link-args=/STACK:8388608 /Brepro',
        '--remap-path-prefix=D:/qontinui-root.wt=/qontinui',
        '--remap-path-prefix=D:/qontinui-root=/qontinui'
    )
} elseif ($hostTriple -like '*linux-gnu*') {
    $flags = @(
        '-C', 'link-arg=-Wl,--build-id=none',
        '--remap-path-prefix=/home/runner/qontinui-root.wt=/qontinui',
        '--remap-path-prefix=/home/runner/qontinui-root=/qontinui'
    )
} else {
    # No target block in .cargo/config.toml for this host -- nothing to re-state.
    $flags = @()
}
$flags += @('--cfg', 'tokio_unstable')

# 0x1f (unit separator) is the encoding Cargo defines for
# CARGO_ENCODED_RUSTFLAGS; unlike RUSTFLAGS it preserves flags containing spaces.
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join ([char]0x1f)
# CARGO_ENCODED_RUSTFLAGS wins over RUSTFLAGS, but an inherited RUSTFLAGS would
# be confusing in logs -- drop it so there is exactly one source of truth.
Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue

$bind = if ($env:TOKIO_CONSOLE_BIND) { $env:TOKIO_CONSOLE_BIND } else { '127.0.0.1:6669' }
Write-Host "==> cargo $Action --features debug-tokio-console  (--cfg tokio_unstable)"
Write-Host "==> host: $hostTriple"
Write-Host "==> tokio-console will listen on $bind  ->  tokio-console http://$bind"
Write-Host "==> NOTE: changing RUSTFLAGS invalidates the build cache; expect a full rebuild."

Push-Location $srcTauri
try {
    & cargo $Action '--features' 'debug-tokio-console' @CargoArgs
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
