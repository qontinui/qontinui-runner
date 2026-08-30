#!/bin/bash
#
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
#  1. `RUSTFLAGS` (and `CARGO_ENCODED_RUSTFLAGS`) *REPLACE* the `[target.*]
#     rustflags` in `src-tauri/.cargo/config.toml` -- Cargo does not merge them.
#     Setting RUSTFLAGS by hand therefore silently drops the Windows
#     `/STACK:8388608` link arg (needed to link the large test binary), `/Brepro`
#     and the sccache path remaps. This script re-states them.
#  2. Plain `RUSTFLAGS` is split on whitespace with no quoting, so the Windows
#     `-C link-args=/STACK:8388608 /Brepro` value cannot survive it. We use
#     `CARGO_ENCODED_RUSTFLAGS` (0x1f-separated), which does.
#
# Changing RUSTFLAGS invalidates the build cache: the first build after this
# switch (and the first one back) is a full rebuild of the dependency graph.
# That is expected, not a fault.
#
# Usage:
#   scripts/dev-tokio-console.sh [run|check|build|test] [extra cargo args...]
#
# Then, in a second terminal:
#   cargo install --locked tokio-console     # once
#   tokio-console http://127.0.0.1:6669      # or $TOKIO_CONSOLE_BIND

set -euo pipefail

ACTION="${1:-run}"
shift || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI="$SCRIPT_DIR/../src-tauri"

case "$ACTION" in
    run|check|build|test|clippy) ;;
    *)
        echo "usage: $(basename "$0") [run|check|build|test|clippy] [extra cargo args...]" >&2
        exit 2
        ;;
esac

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"

# Keep in sync with src-tauri/.cargo/config.toml [target.*] rustflags.
declare -a FLAGS
case "$HOST_TRIPLE" in
    *windows-msvc*)
        FLAGS=(
            "-C" "link-args=/STACK:8388608 /Brepro"
            "--remap-path-prefix=D:/qontinui-root.wt=/qontinui"
            "--remap-path-prefix=D:/qontinui-root=/qontinui"
        )
        ;;
    *linux-gnu*)
        FLAGS=(
            "-C" "link-arg=-Wl,--build-id=none"
            "--remap-path-prefix=/home/runner/qontinui-root.wt=/qontinui"
            "--remap-path-prefix=/home/runner/qontinui-root=/qontinui"
        )
        ;;
    *)
        # No target block in .cargo/config.toml for this host -- nothing to
        # re-state, so the cfg flag is all we add.
        FLAGS=()
        ;;
esac
FLAGS+=("--cfg" "tokio_unstable")

# 0x1f (unit separator) is the encoding Cargo defines for
# CARGO_ENCODED_RUSTFLAGS; unlike RUSTFLAGS it preserves flags containing spaces.
US=$'\x1f'
ENCODED=""
for flag in "${FLAGS[@]}"; do
    if [ -z "$ENCODED" ]; then ENCODED="$flag"; else ENCODED="$ENCODED$US$flag"; fi
done
export CARGO_ENCODED_RUSTFLAGS="$ENCODED"
# CARGO_ENCODED_RUSTFLAGS wins over RUSTFLAGS, but an inherited RUSTFLAGS would
# be confusing in logs -- drop it so there is exactly one source of truth.
unset RUSTFLAGS || true

BIND="${TOKIO_CONSOLE_BIND:-127.0.0.1:6669}"
echo "==> cargo $ACTION --features debug-tokio-console  (--cfg tokio_unstable)"
echo "==> host: $HOST_TRIPLE"
echo "==> tokio-console will listen on $BIND  ->  tokio-console http://$BIND"
echo "==> NOTE: changing RUSTFLAGS invalidates the build cache; expect a full rebuild."

cd "$SRC_TAURI"
exec cargo "$ACTION" --features debug-tokio-console "$@"
