#!/usr/bin/env bash
#
# cargo-remedy.sh — name the right remedy when cargo could not run at all.
#
# Sourced by `.pre-commit-hooks/cargo-prepush.sh`; tested by
# `cargo-remedy-test.sh`.
#
# THE FAILURE THIS FIXES (coord memories 6edb2fb5 and 1d9aa03e; it happened
# again on 2026-11-05 while pushing qontinui-runner#2636)
#
# An agent's Bash tool runs a non-interactive shell, so it never sources the
# `. "$HOME/.cargo/env"` line that rustup adds to ~/.profile and ~/.bashrc.
# `cargo` is then missing from PATH even though ~/.cargo/bin/cargo exists. The
# hook's exit-127 arm told the pusher to "install Rust via rustup", which is a
# remedy for a toolchain that is not installed. Three sessions in five weeks
# worked out the real cause by hand.
#
# The hook does NOT put cargo on PATH itself. A session may leave cargo off
# PATH on purpose (one did, to keep a clippy build off a box under memory
# pressure). `cargo-verify.sh` handles the same case by refusing and naming the
# directory rather than prepending it. This helper only names the right remedy.

# cargo_unrunnable_remedy — print the remedy lines for "cargo could not run".
#
# When `cargo` is not on PATH, it looks for an executable cargo in
# $CARGO_HOME/bin and then in $HOME/.cargo/bin. Checking both covers a
# CARGO_HOME used only as a cache while rustup's proxies stay in ~/.cargo/bin.
# If it finds one, it prints the PATH remedy naming that directory,
# shell-quoted so the line can be pasted. Otherwise it prints the rustup
# remedy. That includes a cargo that IS on PATH and still failed to execute,
# which is a toolchain fault, not a PATH one. It uses only shell builtins, so
# it works under an empty PATH, and it always returns 0.
cargo_unrunnable_remedy() {
    local dir found=""
    if ! type -P cargo >/dev/null 2>&1; then
        for dir in "${CARGO_HOME:+$CARGO_HOME/bin}" "${HOME:+$HOME/.cargo/bin}"; do
            if [ -n "$dir" ] && [ -x "$dir/cargo" ]; then
                found="$dir"
                break
            fi
        done
    fi
    if [ -n "$found" ]; then
        echo "          cargo IS installed at $found/cargo, but $found is not on"
        echo "          this shell's PATH (a non-interactive shell never sources ${found%/bin}/env)."
        echo "          Prepend it and push again:  PATH=$(printf '%q' "$found"):\"\$PATH\" git push"
    else
        echo "          Check the toolchain: install Rust via rustup."
    fi
    return 0
}
