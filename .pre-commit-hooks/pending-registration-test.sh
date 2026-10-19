#!/usr/bin/env bash
#
# Tests for `lib/pending-registration.sh`: the local drift hook's tolerance for
# bindings qontinui-schemas declares as waiting on an open runner PR.
#
# Toolchain-free: throwaway trees under $TMPDIR, and a STUB
# `scripts/pending_registration.py` standing in for qontinui-schemas' module,
# so no network, no GitHub token and no schemas checkout are needed. The real
# module's ACTIVE rules are tested where they live (qontinui-schemas
# `tests/test_pending_registration.py`); what is pinned here is this side of
# the seam — how its answers are read, and what gets absorbed.
#
# Run: bash .pre-commit-hooks/pending-registration-test.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/pending-registration.sh
. "$HERE/lib/pending-registration.sh"

pass=0; failn=0; skipn=0
ok()   { echo "  ok   $*"; pass=$((pass + 1)); }
bad()  { echo "  FAIL $*"; failn=$((failn + 1)); }
skip() { echo "  SKIP $*"; skipn=$((skipn + 1)); }
check() {
    local label="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then ok "$label"; else bad "$label"; printf '       want: %s\n       got : %s\n' "$want" "$got"; fi
}

ROOT="$(mktemp -d -t pending-registration-test-XXXXXXXX)"
trap 'chmod -R u+w "$ROOT" 2>/dev/null; rm -rf "$ROOT"' EXIT

# ── the ACTIVE decision (stub module) ──────────────────────────────────────

echo "pending-registration: reading the module's answer"

fake_schemas() {
    local dir="$ROOT/schemas-$1"
    mkdir -p "$dir/codegen" "$dir/scripts"
    printf '[[pending]]\ntype = "Alpha"\nrunner_pr = "qontinui/qontinui-runner#1"\nadded = 2026-10-13\n' \
        > "$dir/codegen/pending-registration.toml"
    cat > "$dir/scripts/pending_registration.py" <<'PY'
import os


class ModuleError(Exception):
    pass


class _E:
    def __init__(self, t):
        self.type = t


def load_entries(root):
    if os.environ.get("STUB_RAISE"):
        raise ModuleError("stub: toml unreadable")
    return [_E(t) for t in os.environ.get("STUB_TYPES", "").split(",") if t]


def make_reader():
    return None


def _today():
    return None


def metadata_verdict(entry, reader, today):
    if entry.type in os.environ.get("STUB_ACTIVE", "").split(","):
        return True, ""
    return False, os.environ.get("STUB_WHY", "runner PR closed")
PY
    printf '%s' "$dir"
}

# A `gh` that has no token to give, first on PATH, so the no-token case never
# depends on the developer's own sign-in.
NO_GH="$ROOT/no-gh"
mkdir -p "$NO_GH"
printf '#!/usr/bin/env bash\nexit 1\n' > "$NO_GH/gh"
chmod +x "$NO_GH/gh"

if ! pending_registration_python >/dev/null; then
    skip "no Python 3.11+ here: the module-reading arms cannot run (the hook then reports UNKNOWN and absorbs nothing)"
else
    S="$(fake_schemas none)"
    rm -f "$S/codegen/pending-registration.toml"
    pending_registration_active_types "$S"
    check "no toml: nothing is declared" "none" "$PENDING_STATE"

    S="$(fake_schemas nomodule)"
    rm -f "$S/scripts/pending_registration.py"
    pending_registration_active_types "$S"
    check "declared waits but no module: UNKNOWN, never a grant" "unknown|" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas mixed)"
    GITHUB_TOKEN=t STUB_TYPES=Alpha,Beta STUB_ACTIVE=Alpha pending_registration_active_types "$S"
    check "only the ACTIVE entry is returned" "ok|Alpha" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"
    check "  and the INERT one is reported with the module's reason" \
        "Alpha: ACTIVE"$'\n'"Beta: INERT (runner PR closed)" "$PENDING_ROWS"
    check "  without writing bytecode into the schemas checkout" "no" \
        "$([ -e "$S/scripts/__pycache__" ] && echo yes || echo no)"

    S="$(fake_schemas noactive)"
    GITHUB_TOKEN=t STUB_TYPES=Alpha STUB_ACTIVE= pending_registration_active_types "$S"
    check "every entry INERT (with a token): an answer, absorbing nothing" "ok|" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas raise)"
    GITHUB_TOKEN=t STUB_TYPES=Alpha STUB_RAISE=1 pending_registration_active_types "$S"
    check "a module error is UNKNOWN" "unknown" "$PENDING_STATE"
    check "  and names the module's own reason" yes \
        "$(case "$PENDING_UNKNOWN_REASON" in *"toml unreadable"*) echo yes ;; *) echo no ;; esac)"

    # The schemas module honours a fixture seam off GitHub Actions; a value
    # left in the environment must never reach it from this hook. The stub
    # stands in for the module, so it reports whether it SAW the variable.
    S="$(fake_schemas fixture)"
    cat >> "$S/scripts/pending_registration.py" <<'PY'


_real_metadata_verdict = metadata_verdict


def metadata_verdict(entry, reader, today):
    if os.environ.get("PENDING_REGISTRATION_FIXTURE"):
        return True, ""
    return _real_metadata_verdict(entry, reader, today)
PY
    PENDING_REGISTRATION_FIXTURE=/some/fixture.json GITHUB_TOKEN=t STUB_TYPES=Alpha STUB_ACTIVE= \
        pending_registration_active_types "$S"
    check "a leftover PENDING_REGISTRATION_FIXTURE never reaches the module" "ok|" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas notoken)"
    PATH="$NO_GH:$PATH" GITHUB_TOKEN= STUB_TYPES=Alpha STUB_ACTIVE= STUB_WHY="unknown(no token)" \
        pending_registration_active_types "$S"
    check "no token anywhere: UNKNOWN naming the token, not 'nothing pending'" "unknown|yes" \
        "$PENDING_STATE|$(case "$PENDING_UNKNOWN_REASON" in *"no GitHub token"*) echo yes ;; *) echo no ;; esac)"
fi

# ── the absorption (pure) ──────────────────────────────────────────────────

echo "pending-registration: absorbing a declared binding"

# baseline: Alpha, Beta, Gamma exported; the regeneration lacks Alpha.
mk_trees() {
    local name="$1" eol="${2:-}"
    BASE="$ROOT/$name/base"; REGEN="$ROOT/$name/regen"; OUT="$ROOT/$name/out"
    mkdir -p "$BASE" "$REGEN"
    local t
    for t in Alpha Beta Gamma; do printf 'export type %s = string;\n' "$t" > "$BASE/$t.d.ts"; done
    for t in Beta Gamma; do cp "$BASE/$t.d.ts" "$REGEN/$t.d.ts"; done
    printf "export type { Alpha } from './Alpha';$eol\nexport type { Beta } from './Beta';$eol\nexport type { Gamma } from './Gamma';$eol\n" > "$BASE/index.ts"
    printf "export type { Beta } from './Beta';$eol\nexport type { Gamma } from './Gamma';$eol\n" > "$REGEN/index.ts"
    # A schemas checkout whose COMMITTED Python generated tree has the shape
    # CI's `absorb_set` requires (one import, one `__all__` entry, the module
    # file). The library reads HEAD, as CI does; cases edit and re-commit.
    SCH="$ROOT/$name/schemas"
    PY="$SCH/src/qontinui_schemas/generated"
    mkdir -p "$PY/per_type"
    git init --quiet "$SCH"
    git -C "$SCH" config core.autocrlf false
    : > "$PY/__init__.py"
    for t in Alpha Beta Gamma; do
        printf 'from .per_type.%s import %s\n' "$(printf '%s' "$t" | tr 'A-Z' 'a-z')" "$t" >> "$PY/__init__.py"
        printf 'class %s: ...\n' "$t" > "$PY/per_type/$(printf '%s' "$t" | tr 'A-Z' 'a-z').py"
    done
    printf '__all__ = [\n    "Alpha",\n    "Beta",\n    "Gamma",\n]\n' >> "$PY/__init__.py"
    commit_py
}
commit_py() {
    git -C "$SCH" add -A >/dev/null
    git -C "$SCH" -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false \
        -c core.hooksPath=/nonexistent commit --quiet --allow-empty -m py
}
# autocrlf off: a CRLF-vs-LF difference must count, not be normalised away.
same() { git -c core.autocrlf=false diff --no-index --quiet -- "$1" "$2" && echo same || echo differs; }

mk_trees plain
check "precondition: the baseline differs from the regeneration" differs "$(same "$BASE" "$REGEN")"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "an ACTIVE binding the regeneration deleted is absorbed" "Alpha|same" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"
check "  and the baseline itself is untouched" "yes|yes" \
    "$([ -f "$BASE/Alpha.d.ts" ] && echo yes || echo no)|$(grep -qF "{ Alpha }" "$BASE/index.ts" && echo yes || echo no)"

mk_trees nodecl
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH"
check "with nothing ACTIVE, nothing is absorbed and the drift stands" "|differs" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees other
printf 'export type Delta = number;\n' > "$REGEN/Delta.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "absorbing one binding leaves unrelated drift visible" "Alpha|differs" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees produced
printf 'export type Alpha = number;\n' > "$REGEN/Alpha.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "a binding the regeneration PRODUCED is never absorbed (it is compared)" "|yes" \
    "$PENDING_ABSORBED|$(case "$PENDING_NOT_ABSORBED" in "Alpha: the regeneration produced it"*) echo yes ;; *) echo no ;; esac)"

mk_trees absent
rm -f "$BASE/Alpha.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "a declared type absent from the baseline is not absorbed" "|Alpha: not in the baseline" "$PENDING_ABSORBED|$PENDING_NOT_ABSORBED"

mk_trees stillexported
printf "export type { Alpha } from './Alpha';\n" >> "$REGEN/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "a regenerated index.ts that still exports it is not absorbed" "" "$PENDING_ABSORBED"

mk_trees twice
printf "export type { Alpha } from './Alpha';\n" >> "$BASE/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "an export line present twice in the baseline is not absorbed" "" "$PENDING_ABSORBED"

mk_trees crlf '\r'
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "a CRLF baseline index.ts is absorbed byte-for-byte" "Alpha|same"     "$PENDING_ABSORBED|$(cmp -s "$OUT/index.ts" "$REGEN/index.ts" && echo same || echo differs)"

echo "pending-registration: the Python half of CI's shape check"

mk_trees py-noimport
sed -i '/import Alpha$/d' "$PY/__init__.py"; commit_py
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "no per_type import of the type in the Python barrel: not absorbed (CI says INERT)" "|yes" \
    "$PENDING_ABSORBED|$(case "$PENDING_NOT_ABSORBED" in "Alpha: shape:"*) echo yes ;; *) echo no ;; esac)"

mk_trees py-noall
sed -i '/^    "Alpha",$/d' "$PY/__init__.py"; commit_py
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "no __all__ entry for the type: not absorbed" "" "$PENDING_ABSORBED"

mk_trees py-twice
printf '    "Alpha",\n' >> "$PY/__init__.py"; commit_py
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "an __all__ entry present twice: not absorbed" "" "$PENDING_ABSORBED"

mk_trees py-nomodule
rm -f "$PY/per_type/alpha.py"; commit_py
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "the per_type module file missing from HEAD: not absorbed" "|Alpha: shape: per_type/alpha.py is not in HEAD" \
    "$PENDING_ABSORBED|$PENDING_NOT_ABSORBED"

mk_trees py-crlf-committed
sed -i 's/$/\r/' "$PY/__init__.py"; commit_py
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "a barrel COMMITTED with CRLF is not absorbed (CI's ^...$ does not match it either)" "" "$PENDING_ABSORBED"

mk_trees py-dirty
sed -i '/import Alpha$/d' "$PY/__init__.py"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha
check "an uncommitted barrel edit does not change the verdict (HEAD is read, as CI reads it)" "Alpha" "$PENDING_ABSORBED"

echo "pending-registration: inputs that must never become paths or links"

mk_trees badname
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" "../Alpha"
check "a type name that is not an identifier is refused, never used as a path" "|../Alpha: not a type name" \
    "$PENDING_ABSORBED|$PENDING_NOT_ABSORBED"

mk_trees symlink
LINKED="$ROOT/symlink/linked-base"
if ln -s "$BASE" "$LINKED" 2>/dev/null && [ -L "$LINKED" ]; then
    pending_registration_absorb "$LINKED" "$REGEN" "$OUT" "$SCH" Alpha
    check "a symlinked baseline is copied, never written through" "Alpha|yes|yes" \
        "$PENDING_ABSORBED|$([ -f "$BASE/Alpha.d.ts" ] && echo yes || echo no)|$(grep -qF '{ Alpha }' "$BASE/index.ts" && echo yes || echo no)"
else
    skip "symlinked baseline: 'ln -s' made no symlink here (a copy, or refused)"
fi

mk_trees readonly
chmod -R a-w "$BASE"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "$SCH" Alpha; rc=$?
check "a read-only baseline (the sibling store) still absorbs" "0|Alpha|same" "$rc|$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"
chmod -R u+w "$BASE"

echo "$pass passed, $failn failed, $skipn skipped"
[ "$failn" -eq 0 ]
