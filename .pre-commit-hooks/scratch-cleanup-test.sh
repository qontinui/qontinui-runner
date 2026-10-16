#!/usr/bin/env bash
#
# Tests for `lib/scratch-cleanup.sh`: removing a scratch tree that contains a
# READ-ONLY copy (the shape `cp -R` of the SHA-keyed sibling store produces) must
# succeed, and must never fail the caller.
#
# Run: bash .pre-commit-hooks/scratch-cleanup-test.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/scratch-cleanup.sh
. "$HERE/lib/scratch-cleanup.sh"

pass=0; failn=0
ok()  { echo "  ok   $*"; pass=$((pass + 1)); }
bad() { echo "  FAIL $*"; failn=$((failn + 1)); }

# A read-only source tree, then a `cp -R` snapshot of it — exactly the hook's path.
mk_readonly_snapshot() {
    local root="$1"
    mkdir -p "$root/src/nested"
    echo a > "$root/src/a.d.ts"; echo b > "$root/src/nested/b.d.ts"
    chmod -R a-w "$root/src"
    cp -R "$root/src" "$root/snap"
}

echo "scratch-cleanup"
# root ignores file modes, so the read-only precondition can never hold there:
# say so and skip rather than report a failure that is about the runner.
if [ "$(id -u)" -eq 0 ]; then
    echo "  SKIP running as root: a read-only tree cannot be observed (root bypasses modes)"
    exit 0
fi
root="$(mktemp -d -t scratch-cleanup-test-XXXXXXXX)"
mk_readonly_snapshot "$root"
# Sanity: the snapshot really is read-only, or this test proves nothing.
if rm -rf "$root/snap" 2>/dev/null && [ ! -e "$root/snap" ]; then
    bad "precondition: a plain rm -rf removed the read-only snapshot, so the test cannot observe the fix (running as root?)"
else
    ok "precondition: a plain rm -rf cannot remove a read-only cp -R snapshot"
fi

scratch_dir_remove "$root"; rc=$?
[ "$rc" -eq 0 ] && ok "returns 0 on a tree with read-only directories" || bad "returned $rc"
[ ! -e "$root" ] && ok "removes the whole tree, read-only parts included" || bad "left $root behind"

# The EXIT-trap shape, under the hook's own `set -euo pipefail`: a command that
# fails inside an EXIT trap under `set -e` replaces the script's exit status, so
# a passing run must still exit 0. (Without `set -e` the bare `rm -rf` would not
# have failed the hook, and this case would observe nothing.)
out="$(bash -c 'set -euo pipefail; . "$1"; d="$(mktemp -d)"; mkdir -p "$d/x"; echo z > "$d/x/z"; chmod -R a-w "$d/x"; trap "scratch_dir_remove \"$d\"" EXIT; echo OK' _ "$HERE/lib/scratch-cleanup.sh")"; rc=$?
[ "$rc" -eq 0 ] && [ "$out" = "OK" ] && ok "an EXIT trap over a read-only tree leaves the exit status 0" || bad "EXIT-trap run exited $rc (out=$out)"

scratch_dir_remove ""; [ $? -eq 0 ] && ok "an empty argument is a no-op" || bad "empty argument failed"
scratch_dir_remove "/nonexistent/scratch-cleanup-$$"; [ $? -eq 0 ] && ok "a missing dir is a no-op" || bad "missing dir failed"

echo "$pass passed, $failn failed"
[ "$failn" -eq 0 ]
