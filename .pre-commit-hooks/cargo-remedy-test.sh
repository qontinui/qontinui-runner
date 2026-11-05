#!/usr/bin/env bash
#
# Tests for `lib/cargo-remedy.sh`. When cargo is installed under
# ${CARGO_HOME:-$HOME/.cargo}/bin but that directory is not on PATH, the hook
# must name that directory as the remedy, not "install Rust via rustup". In
# every other case it must keep the rustup remedy.
#
# Run: bash .pre-commit-hooks/cargo-remedy-test.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/cargo-remedy.sh
. "$HERE/lib/cargo-remedy.sh"

pass=0; failn=0
ok()  { echo "  ok   $*"; pass=$((pass + 1)); }
bad() { echo "  FAIL $*"; failn=$((failn + 1)); }

root="$(mktemp -d -t cargo-remedy-test-XXXXXXXX)"
trap 'rm -rf -- "$root"' EXIT

# An installed toolchain: an executable `cargo` stub under <home>/bin.
mk_cargo_home() {
    mkdir -p "$1/bin"
    printf '#!/bin/sh\nexit 0\n' > "$1/bin/cargo"
    chmod +x "$1/bin/cargo"
}
# An empty directory to use as PATH, so no system cargo can be found. The
# helper uses only builtins, so an empty PATH is all it needs.
empty="$root/empty-path"; mkdir -p "$empty"

# Run the helper in a subshell, so its PATH, HOME and CARGO_HOME stay there.
# Arguments: PATH, then CARGO_HOME ("-" means unset), then HOME ("-" means unset).
remedy() {
    (
        export PATH="$1"
        if [ "$2" = "-" ]; then unset CARGO_HOME; else export CARGO_HOME="$2"; fi
        if [ "$3" = "-" ]; then unset HOME; else export HOME="$3"; fi
        cargo_unrunnable_remedy
    )
}

echo "cargo-remedy"

# 1. Installed under CARGO_HOME, missing from PATH: name the directory.
ch="$root/cargo-home"; mk_cargo_home "$ch"
out="$(remedy "$empty" "$ch" "$root/no-home")"; rc=$?
[ "$rc" -eq 0 ] && ok "returns 0" || bad "returned $rc"
case "$out" in
    *"Rust IS installed"*"$ch/bin"*) ok "CARGO_HOME install off PATH: names $ch/bin" ;;
    *) bad "CARGO_HOME install off PATH: expected the PATH remedy, got: $out" ;;
esac
case "$out" in
    *rustup*) bad "CARGO_HOME install off PATH: still recommends rustup: $out" ;;
    *) ok "CARGO_HOME install off PATH: does not recommend rustup" ;;
esac

# 2. Following the printed remedy really does make cargo resolvable. This
#    checks the advice, not just the wording.
suggested="$(printf '%s\n' "$out" | sed -n 's/.*PATH="\([^:"]*\):.*/\1/p')"
if [ -n "$suggested" ] && (export PATH="$suggested:$empty"; type -P cargo >/dev/null 2>&1); then
    ok "the suggested PATH prefix ($suggested) resolves cargo"
else
    bad "the suggested PATH prefix '${suggested}' does not resolve cargo"
fi

# 3. No CARGO_HOME: fall back to $HOME/.cargo/bin, as rustup and cargo-verify do.
h="$root/home"; mk_cargo_home "$h/.cargo"
out="$(remedy "$empty" - "$h")"
case "$out" in
    *"Rust IS installed"*"$h/.cargo/bin"*) ok "no CARGO_HOME: names \$HOME/.cargo/bin" ;;
    *) bad "no CARGO_HOME: expected $h/.cargo/bin, got: $out" ;;
esac

# 4. Nothing installed anywhere: keep the rustup remedy.
out="$(remedy "$empty" "$root/absent-cargo-home" "$root/absent-home")"
case "$out" in
    *rustup*) ok "nothing installed: keeps the rustup remedy" ;;
    *) bad "nothing installed: expected rustup, got: $out" ;;
esac

# 5. cargo IS on PATH (so it was found and failed to execute): that is a
#    toolchain fault, not a PATH one, so keep the rustup remedy.
out="$(remedy "$ch/bin:$empty" "$ch" "$root/no-home")"
case "$out" in
    *rustup*) ok "cargo on PATH but failing: keeps the rustup remedy" ;;
    *) bad "cargo on PATH but failing: expected rustup, got: $out" ;;
esac

# 6. A cargo file that is not executable is not an install: keep rustup.
nx="$root/not-exec"; mkdir -p "$nx/bin"; : > "$nx/bin/cargo"; chmod -x "$nx/bin/cargo"
out="$(remedy "$empty" "$nx" "$root/no-home")"
case "$out" in
    *rustup*) ok "non-executable cargo file: keeps the rustup remedy" ;;
    *) bad "non-executable cargo file: expected rustup, got: $out" ;;
esac

# 7. Under the hook's own `set -euo pipefail`, with HOME and CARGO_HOME both
#    unset: no unbound-variable abort, and the exit status stays 0.
out="$(env -u HOME -u CARGO_HOME PATH="$empty" /bin/bash -c \
    'set -euo pipefail; . "$1"; cargo_unrunnable_remedy; echo DONE' _ "$HERE/lib/cargo-remedy.sh")"; rc=$?
case "$rc:$out" in
    0:*DONE) ok "set -euo pipefail with HOME and CARGO_HOME unset: exits 0" ;;
    *) bad "set -euo pipefail with HOME and CARGO_HOME unset: rc=$rc out=$out" ;;
esac

echo "$pass passed, $failn failed"
[ "$failn" -eq 0 ]
