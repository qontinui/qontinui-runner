#!/usr/bin/env bash
#
# Tests for `lib/cargo-remedy.sh`, and for its wiring into `cargo-prepush.sh`.
# When cargo is installed under $CARGO_HOME/bin or $HOME/.cargo/bin but that
# directory is not on PATH, the hook must name that directory as the remedy,
# not "install Rust via rustup". In every other case it must keep the rustup
# remedy.
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

# An installed toolchain: an executable `cargo` stub under <dir>/bin.
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

# Check that pasting the printed line would make cargo resolvable. It takes
# the `PATH=<quoted dir>:"$PATH" git push` assignment from the output,
# evaluates it the way a shell would, and looks cargo up under the result.
# This checks the advice, not just the wording.
pasted_line_resolves_cargo() {
    local line q
    line="$(printf '%s\n' "$1" | sed -n 's/.*Prepend it and push again:  //p')"
    case "$line" in
        'PATH='*':"$PATH" git push') ;;
        *) return 1 ;;
    esac
    q="${line#PATH=}"; q="${q%:\"\$PATH\" git push}"
    (eval "export PATH=$q:$empty"; type -P cargo >/dev/null 2>&1)
}

echo "cargo-remedy"

# 1. Installed under CARGO_HOME, missing from PATH: name the directory.
ch="$root/cargo-home"; mk_cargo_home "$ch"
out="$(remedy "$empty" "$ch" "$root/no-home")"; rc=$?
[ "$rc" -eq 0 ] && ok "returns 0" || bad "returned $rc"
case "$out" in
    *"cargo IS installed at $ch/bin/cargo"*) ok "CARGO_HOME install off PATH: names $ch/bin" ;;
    *) bad "CARGO_HOME install off PATH: expected the PATH remedy, got: $out" ;;
esac
case "$out" in
    *rustup*) bad "CARGO_HOME install off PATH: still recommends rustup: $out" ;;
    *) ok "CARGO_HOME install off PATH: does not recommend rustup" ;;
esac

# 2. The printed line is copy-pasteable: $PATH is printed literally (not
#    expanded), and pasting it resolves cargo.
case "$out" in
    *':"$PATH" git push'*) ok "prints \$PATH literally, not this shell's expanded PATH" ;;
    *) bad "the remedy line does not end in a literal :\"\$PATH\" git push: $out" ;;
esac
pasted_line_resolves_cargo "$out" && ok "pasting the printed line resolves cargo" \
    || bad "pasting the printed line does not resolve cargo: $out"

# 3. A directory with spaces and shell metacharacters is quoted, so the pasted
#    line still resolves cargo.
odd="$root/odd dir \$x \"q\""; mk_cargo_home "$odd"
out="$(remedy "$empty" "$odd" "$root/no-home")"
pasted_line_resolves_cargo "$out" && ok "a directory with spaces and \$/\" is quoted correctly" \
    || bad "a directory with spaces and \$/\" is not pasteable: $out"

# 4. No CARGO_HOME: fall back to $HOME/.cargo/bin, as rustup and cargo-verify do.
h="$root/home"; mk_cargo_home "$h/.cargo"
out="$(remedy "$empty" - "$h")"
case "$out" in
    *"cargo IS installed at $h/.cargo/bin/cargo"*) ok "no CARGO_HOME: names \$HOME/.cargo/bin" ;;
    *) bad "no CARGO_HOME: expected $h/.cargo/bin, got: $out" ;;
esac

# 5. CARGO_HOME set as a cache with no cargo in it, while rustup's proxies are
#    in $HOME/.cargo/bin: still name the $HOME install rather than send the
#    pusher to rustup.
out="$(remedy "$empty" "$root/cache-only-cargo-home" "$h")"
case "$out" in
    *"cargo IS installed at $h/.cargo/bin/cargo"*) ok "cache-only CARGO_HOME: falls back to \$HOME/.cargo/bin" ;;
    *) bad "cache-only CARGO_HOME: expected $h/.cargo/bin, got: $out" ;;
esac

# 6. Nothing installed anywhere: keep the rustup remedy.
out="$(remedy "$empty" "$root/absent-cargo-home" "$root/absent-home")"
case "$out" in
    *rustup*) ok "nothing installed: keeps the rustup remedy" ;;
    *) bad "nothing installed: expected rustup, got: $out" ;;
esac

# 7. cargo IS on PATH (so it was found and failed to execute): that is a
#    toolchain fault, not a PATH one, so keep the rustup remedy.
out="$(remedy "$ch/bin:$empty" "$ch" "$root/no-home")"
case "$out" in
    *rustup*) ok "cargo on PATH but failing: keeps the rustup remedy" ;;
    *) bad "cargo on PATH but failing: expected rustup, got: $out" ;;
esac

# 8. A cargo file that is not executable is not an install: keep rustup.
nx="$root/not-exec"; mkdir -p "$nx/bin"; : > "$nx/bin/cargo"; chmod -x "$nx/bin/cargo"
out="$(remedy "$empty" "$nx" "$root/no-home")"
case "$out" in
    *rustup*) ok "non-executable cargo file: keeps the rustup remedy" ;;
    *) bad "non-executable cargo file: expected rustup, got: $out" ;;
esac

# 9. Under the hook's own `set -euo pipefail`, with HOME and CARGO_HOME both
#    unset: no unbound-variable abort, and the exit status stays 0.
out="$(env -u HOME -u CARGO_HOME -u BASH_ENV PATH="$empty" "$BASH" -c \
    'set -euo pipefail; . "$1"; cargo_unrunnable_remedy; echo DONE' _ "$HERE/lib/cargo-remedy.sh")"; rc=$?
case "$rc:$out" in
    0:*DONE) ok "set -euo pipefail with HOME and CARGO_HOME unset: exits 0" ;;
    *) bad "set -euo pipefail with HOME and CARGO_HOME unset: rc=$rc out=$out" ;;
esac

# 10. Wiring: cargo-prepush.sh's `cargo metadata` exit-127 arm must source this
#     lib and call the helper. Otherwise a deleted call or a misspelled path
#     quietly brings back the old rustup line, and every case above still
#     passes. The arm runs from its `metadata_rc" -eq 127` test to its `exit 1`.
arm="$(awk '/metadata_rc" -eq 127/{f=1} f{print} f&&/exit 1/{exit}' "$HERE/cargo-prepush.sh")"
case "$arm" in
    *'. "$SCRIPT_DIR/lib/cargo-remedy.sh"'*cargo_unrunnable_remedy*)
        ok "cargo-prepush.sh's metadata exit-127 arm sources the lib and calls the helper" ;;
    *) bad "cargo-prepush.sh's metadata exit-127 arm does not source lib/cargo-remedy.sh and call cargo_unrunnable_remedy" ;;
esac
[ -f "$HERE/lib/cargo-remedy.sh" ] && ok "the path that arm sources exists" \
    || bad "lib/cargo-remedy.sh is missing next to cargo-prepush.sh"

echo "$pass passed, $failn failed"
[ "$failn" -eq 0 ]
