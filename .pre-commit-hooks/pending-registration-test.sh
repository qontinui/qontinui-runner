#!/usr/bin/env bash
#
# Tests for `lib/pending-registration.sh`: the local drift hook's tolerance for
# bindings qontinui-schemas declares as waiting on an open runner PR.
#
# Toolchain-free: throwaway git repos under $TMPDIR, and a STUB
# `scripts/pending_registration.py` standing in for qontinui-schemas' module,
# so no network, no GitHub token and no schemas checkout are needed. The stub
# fakes only the network half (`metadata_verdict`, from STUB_* variables); its
# shape helpers (`py_module_for`, `ts_export_line`, `py_all_line`,
# `lines_of`) are CI's, copied verbatim, so the committed-shape cases exercise
# the semantics CI applies. The real module's ACTIVE rules are tested where
# they live (qontinui-schemas `tests/test_pending_registration.py`).
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
has() { case "$2" in *"$1"*) echo yes ;; *) echo no ;; esac; }

ROOT="$(mktemp -d -t pending-registration-test-XXXXXXXX)"
trap 'chmod -R u+w "$ROOT" 2>/dev/null; rm -rf "$ROOT"' EXIT

# ── the ACTIVE + committed-shape decision (stub module, real git) ──────────

echo "pending-registration: reading the module's answer"

commit_all() {
    local repo="$1" msg="${2:-fixture}"
    git -C "$repo" add -A >/dev/null
    git -C "$repo" -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false \
        -c core.hooksPath=/nonexistent commit --quiet --allow-empty -m "$msg"
}

# A schemas checkout with Alpha and Beta declared and committed in CI's shape.
fake_schemas() {
    local dir="$ROOT/schemas-$1" t lower
    mkdir -p "$dir/codegen" "$dir/scripts" "$dir/ts/src/generated" "$dir/src/qontinui_schemas/generated/per_type"
    git init --quiet "$dir"
    git -C "$dir" config core.autocrlf false
    printf '[[pending]]\ntype = "Alpha"\n\n[[pending]]\ntype = "Beta"\n' > "$dir/codegen/pending-registration.toml"
    : > "$dir/ts/src/generated/index.ts"
    : > "$dir/src/qontinui_schemas/generated/__init__.py"
    for t in Alpha Beta; do
        lower="$(printf '%s' "$t" | tr 'A-Z' 'a-z')"
        printf 'export type %s = string;\n' "$t" > "$dir/ts/src/generated/$t.d.ts"
        printf "export type { %s } from './%s';\n" "$t" "$t" >> "$dir/ts/src/generated/index.ts"
        printf 'class %s: ...\n' "$t" > "$dir/src/qontinui_schemas/generated/per_type/$lower.py"
        printf 'from .per_type.%s import %s\n' "$lower" "$t" >> "$dir/src/qontinui_schemas/generated/__init__.py"
    done
    printf '__all__ = [\n    "Alpha",\n    "Beta",\n]\n' >> "$dir/src/qontinui_schemas/generated/__init__.py"
    cat > "$dir/scripts/pending_registration.py" <<'PY'
import os
import re
from pathlib import Path

TS_DIR = "ts/src/generated"
TS_BARREL = "ts/src/generated/index.ts"
PY_DIR = "src/qontinui_schemas/generated"
PY_BARREL = "src/qontinui_schemas/generated/__init__.py"


class ModuleError(Exception):
    pass


class _E:
    def __init__(self, t):
        self.type = t


def load_entries(root):
    if os.environ.get("STUB_RAISE"):
        raise ModuleError("stub: toml unreadable")
    text = (Path(root) / "codegen" / "pending-registration.toml").read_text()
    return [_E(t) for t in re.findall(r'type = "(\w+)"', text)]


def make_reader():
    return None


def _today():
    return None


def metadata_verdict(entry, reader, today):
    if os.environ.get("PENDING_REGISTRATION_FIXTURE"):
        return True, ""  # what the real module's fixture seam can do
    if entry.type in os.environ.get("STUB_ACTIVE", "").split(","):
        return True, ""
    return False, os.environ.get("STUB_WHY", "runner PR closed")


# ── CI's own shape helpers, verbatim from qontinui-schemas ──
def ts_export_line(t: str) -> str:
    return f"export type {{ {t} }} from './{t}';"


def py_all_line(t: str) -> str:
    return f'    "{t}",'


def py_module_for(t, head_py_barrel):
    if head_py_barrel is None:
        return None
    m = re.search(
        r"^from \.per_type\.([A-Za-z0-9_]+) import " + re.escape(t) + r"$",
        head_py_barrel,
        re.M,
    )
    return m.group(1) if m else None


def lines_of(text):
    return [] if text is None else text.splitlines()
PY
    commit_all "$dir"
    # What CI runs is the module on the remote default branch; the fixture's
    # "remote" is its first commit.
    git -C "$dir" update-ref refs/remotes/origin/main HEAD
    printf '%s' "$dir"
}
HEAD_N=0
active() {  # against a fresh scratch dir each time, as the hook does per run
    HEAD_N=$((HEAD_N + 1))
    pending_registration_active_types "$1" "$ROOT/head-$HEAD_N"
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
    S="$(fake_schemas mixed)"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "only the ACTIVE entry is returned" "ok|Alpha" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"
    check "  and the INERT one is reported with the module's reason" \
        "Alpha: ACTIVE"$'\n'"Beta: INERT (runner PR closed)" "$PENDING_ROWS"
    check "  without writing anything into the schemas checkout" "" "$(git -C "$S" status --porcelain -uall)"
    check "  read at the checkout's HEAD commit" "$(git -C "$S" rev-parse HEAD)" "$PENDING_HEAD_SHA"

    S="$(fake_schemas none)"
    rm -f "$S/codegen/pending-registration.toml"; commit_all "$S"
    active "$S"
    check "no toml in HEAD: nothing is declared" "none" "$PENDING_STATE"

    S="$(fake_schemas spelled)"
    printf '[[ pending ]]\ntype = "Alpha"\n' > "$S/codegen/pending-registration.toml"; commit_all "$S"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a table spelled [[ pending ]] is still read (no grep pre-filter)" "ok|Alpha" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas uncommitted)"
    rm -f "$S/codegen/pending-registration.toml"; commit_all "$S"
    printf '[[pending]]\ntype = "Alpha"\n' > "$S/codegen/pending-registration.toml"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "an UNCOMMITTED toml is not honoured (HEAD is read, as CI reads it)" "none|" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas dirtyrow)"
    printf '[[pending]]\ntype = "Gamma"\n' >> "$S/codegen/pending-registration.toml"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha,Beta,Gamma active "$S"
    check "a row added to the toml but not committed is not read" "ok|Alpha"$'\n'"Beta" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas dirtymodule)"
    printf '\n\ndef metadata_verdict(entry, reader, today):\n    return True, ""\n' >> "$S/scripts/pending_registration.py"
    GITHUB_TOKEN=t STUB_ACTIVE= active "$S"
    check "an uncommitted edit to the MODULE is never run" "ok|" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas localmodule)"
    printf '

def metadata_verdict(entry, reader, today):
    return True, ""
' >> "$S/scripts/pending_registration.py"
    commit_all "$S" "a local, unpushed module change"
    GITHUB_TOKEN=t STUB_ACTIVE= active "$S"
    check "a COMMITTED module that differs from origin's is not run: UNKNOWN" "unknown||yes" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES|$(has "is not the one on refs/remotes/origin/main" "$PENDING_UNKNOWN_REASON")"

    S="$(fake_schemas shadowref)"
    printf '
# an unpushed edit
' >> "$S/scripts/pending_registration.py"
    commit_all "$S" "a local module change"
    git -C "$S" branch origin/main HEAD  # a LOCAL branch shadowing the remote's short name
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a local branch named origin/main does not vouch for an unpushed module" "unknown|"         "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas localother)"
    printf 'local note\n' > "$S/NOTES"
    commit_all "$S" "a local commit that leaves the module alone"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a local commit whose module equals origin's still runs" "ok|Alpha" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas behindmain)"
    printf '\n# a later main edit\n' >> "$S/scripts/pending_registration.py"
    commit_all "$S" "main moves the module on"
    git -C "$S" update-ref refs/remotes/origin/main HEAD
    git -C "$S" reset --quiet --hard HEAD~1
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a pinned commit BEHIND origin's main runs its own (reviewed) module" "ok|Alpha" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas noend)"
    printf '
import sys
sys.exit(0)
' >> "$S/scripts/pending_registration.py"
    commit_all "$S"; git -C "$S" update-ref refs/remotes/origin/main HEAD
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a module that exits 0 without its END line is UNKNOWN (incomplete answer)" "unknown|yes"         "$PENDING_STATE|$(has "incomplete answer" "$PENDING_UNKNOWN_REASON")"

    S="$(fake_schemas importerror)"
    printf '
raise RuntimeError("boom at import")
' >> "$S/scripts/pending_registration.py"
    commit_all "$S"; git -C "$S" update-ref refs/remotes/origin/main HEAD
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a module that fails at import is UNKNOWN, naming the failure" "unknown|yes"         "$PENDING_STATE|$(has "boom at import" "$PENDING_UNKNOWN_REASON")"

    S="$(fake_schemas xtrace)"
    # The token is exported BEFORE xtrace starts, so only the library's own
    # trace can carry it.
    XT="$( ( export GITHUB_TOKEN=sekrit-token-value STUB_ACTIVE=Alpha; set -x; active "$S"; set +x ) 2>&1 )"
    HEAD_N=$((HEAD_N + 1))  # the subshell's increment does not come back
    check "under bash -x the token never reaches the trace" "no" "$(has "sekrit-token-value" "$XT")"

    S="$(fake_schemas nomodule)"
    rm -f "$S/scripts/pending_registration.py"; commit_all "$S"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "declared waits but no module: UNKNOWN, never a grant" "unknown|" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas nested)"
    mkdir -p "$S/plain/codegen" && cp "$S/codegen/pending-registration.toml" "$S/plain/codegen/"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S/plain"
    check "a plain dir nested in another checkout is UNKNOWN (never that repo's HEAD)" "unknown|yes" \
        "$PENDING_STATE|$(has "not the top of a git checkout" "$PENDING_UNKNOWN_REASON")"

    S="$(fake_schemas notgit)"
    rm -rf "$S/.git"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha active "$S"
    check "a schemas dir that is not a git checkout is UNKNOWN, never a grant" "unknown|" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas noactive)"
    GITHUB_TOKEN=t STUB_ACTIVE= active "$S"
    check "every entry INERT (with a token): an answer, absorbing nothing" "ok|" "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas raise)"
    GITHUB_TOKEN=t STUB_RAISE=1 active "$S"
    check "a module error is UNKNOWN, naming the module's own reason" "unknown|yes" \
        "$PENDING_STATE|$(has "toml unreadable" "$PENDING_UNKNOWN_REASON")"

    S="$(fake_schemas fixture)"
    PENDING_REGISTRATION_FIXTURE=/some/fixture.json GITHUB_TOKEN=t STUB_ACTIVE= active "$S"
    check "a leftover PENDING_REGISTRATION_FIXTURE never reaches the module" "ok|" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"

    S="$(fake_schemas notoken)"
    PATH="$NO_GH:$PATH" GITHUB_TOKEN= STUB_ACTIVE= STUB_WHY="unknown(no token)" active "$S"
    check "no token anywhere: UNKNOWN naming the token, not 'nothing pending'" "unknown|yes" \
        "$PENDING_STATE|$(has "no GitHub token" "$PENDING_UNKNOWN_REASON")"

    echo "pending-registration: CI's committed shape (absorb_set's HEAD half)"

    TSB=ts/src/generated/index.ts
    PYB=src/qontinui_schemas/generated/__init__.py
    shape_case() {  # <label> <want-reason-fragment>; S already edited and committed
        GITHUB_TOKEN=t STUB_ACTIVE=Alpha,Beta active "$S"
        check "$1" "ok|Beta|yes" "$PENDING_STATE|$PENDING_ACTIVE_TYPES|$(has "$2" "$PENDING_ROWS")"
    }

    S="$(fake_schemas noimport)"
    sed -i '/import Alpha$/d' "$S/$PYB"; commit_all "$S"
    shape_case "no committed per_type import: INERT, as CI says" "Alpha: INERT (shape: no \`from .per_type.<m> import Alpha\`"

    S="$(fake_schemas noall)"
    sed -i '/^    "Alpha",$/d' "$S/$PYB"; commit_all "$S"
    shape_case "no committed __all__ entry: INERT" '`"Alpha",` is in the committed'

    S="$(fake_schemas alltwice)"
    printf '    "Alpha",\n' >> "$S/$PYB"; commit_all "$S"
    shape_case "an __all__ entry committed twice: INERT" '2 times, not once'

    S="$(fake_schemas nopymodule)"
    rm -f "$S/src/qontinui_schemas/generated/per_type/alpha.py"; commit_all "$S"
    shape_case "the per_type module not committed: INERT" "per_type/alpha.py is not committed"

    S="$(fake_schemas nodts)"
    git -C "$S" rm --cached --quiet ts/src/generated/Alpha.d.ts
    git -C "$S" -c user.name=t -c user.email=t@example.com -c commit.gpgsign=false \
        -c core.hooksPath=/nonexistent commit --quiet -m "drop Alpha.d.ts from HEAD only"
    shape_case "a .d.ts on disk but not committed: INERT" "ts/src/generated/Alpha.d.ts is not committed"

    S="$(fake_schemas tstwice)"
    printf "export type { Alpha } from './Alpha';\n" >> "$S/$TSB"; commit_all "$S"
    sed -i "0,/{ Alpha }/{/{ Alpha }/d}" "$S/$TSB"
    shape_case "an export line COMMITTED twice: INERT, though the working copy has it once" "2 times, not once"

    S="$(fake_schemas interiorcr)"
    printf "junk\rexport type { Alpha } from './Alpha';\n" >> "$S/$TSB"; commit_all "$S"
    shape_case "an interior CR is a line break, as CI's splitlines sees it (2 lines, not 1)" "2 times, not once"

    S="$(fake_schemas dirtyshape)"
    sed -i '/import Alpha$/d' "$S/$PYB"
    GITHUB_TOKEN=t STUB_ACTIVE=Alpha,Beta active "$S"
    check "an uncommitted barrel edit does not change the verdict" "ok|Alpha"$'\n'"Beta" \
        "$PENDING_STATE|$PENDING_ACTIVE_TYPES"
fi

# ── the absorption (pure: the working trees only) ──────────────────────────

echo "pending-registration: absorbing a declared binding from the on-disk baseline"

# baseline: Alpha, Beta, Gamma exported; the regeneration lacks Alpha.
mk_trees() {
    local name="$1" eol="${2:-}" t
    BASE="$ROOT/$name/base"; REGEN="$ROOT/$name/regen"; OUT="$ROOT/$name/out"
    mkdir -p "$BASE" "$REGEN"
    for t in Alpha Beta Gamma; do printf 'export type %s = string;\n' "$t" > "$BASE/$t.d.ts"; done
    for t in Beta Gamma; do cp "$BASE/$t.d.ts" "$REGEN/$t.d.ts"; done
    printf "export type { Alpha } from './Alpha';$eol\nexport type { Beta } from './Beta';$eol\nexport type { Gamma } from './Gamma';$eol\n" > "$BASE/index.ts"
    printf "export type { Beta } from './Beta';$eol\nexport type { Gamma } from './Gamma';$eol\n" > "$REGEN/index.ts"
}
# autocrlf off: a CRLF-vs-LF difference must count, not be normalised away.
same() { git -c core.autocrlf=false diff --no-index --quiet -- "$1" "$2" && echo same || echo differs; }

mk_trees plain
check "precondition: the baseline differs from the regeneration" differs "$(same "$BASE" "$REGEN")"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "a binding the regeneration deleted is absorbed" "Alpha|same" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"
check "  and the baseline itself is untouched" "yes|yes" \
    "$([ -f "$BASE/Alpha.d.ts" ] && echo yes || echo no)|$(grep -qF "{ Alpha }" "$BASE/index.ts" && echo yes || echo no)"

mk_trees nofinalnl
printf "export type { Alpha } from './Alpha';\nexport type { Beta } from './Beta';\nexport type { Gamma } from './Gamma';" > "$BASE/index.ts"
printf "export type { Beta } from './Beta';\nexport type { Gamma } from './Gamma';" > "$REGEN/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "an index.ts with no final newline is absorbed without gaining one" "Alpha|same" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees nofinalnl-last
printf "export type { Beta } from './Beta';
export type { Gamma } from './Gamma';
export type { Alpha } from './Alpha';" > "$BASE/index.ts"
printf "export type { Beta } from './Beta';
export type { Gamma } from './Gamma';" > "$REGEN/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "dropping the LAST line of a no-final-newline index.ts leaves no stray newline" "Alpha|same" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees nodecl
pending_registration_absorb "$BASE" "$REGEN" "$OUT"
check "with nothing ACTIVE, nothing is absorbed and the drift stands" "|differs" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees other
printf 'export type Delta = number;\n' > "$REGEN/Delta.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "absorbing one binding leaves unrelated drift visible" "Alpha|differs" "$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"

mk_trees produced
printf 'export type Alpha = number;\n' > "$REGEN/Alpha.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "a binding the regeneration PRODUCED is never absorbed (it is compared)" "|yes" \
    "$PENDING_ABSORBED|$(has "Alpha: the regeneration produced it" "$PENDING_NOT_ABSORBED")"

mk_trees absent
rm -f "$BASE/Alpha.d.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "a type absent from the on-disk baseline is not absorbed" "|Alpha: not in the baseline on disk" \
    "$PENDING_ABSORBED|$PENDING_NOT_ABSORBED"

mk_trees stillexported
printf "export type { Alpha } from './Alpha';\n" >> "$REGEN/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "a regenerated index.ts that still exports it is not absorbed" "" "$PENDING_ABSORBED"

mk_trees twice
printf "export type { Alpha } from './Alpha';\n" >> "$BASE/index.ts"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "an export line twice in the on-disk index.ts is not absorbed" "" "$PENDING_ABSORBED"

mk_trees crlf '\r'
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha
check "a CRLF working copy of index.ts is absorbed byte-for-byte" "Alpha|same" \
    "$PENDING_ABSORBED|$(cmp -s "$OUT/index.ts" "$REGEN/index.ts" && echo same || echo differs)"

mk_trees badname
pending_registration_absorb "$BASE" "$REGEN" "$OUT" "../Alpha"
check "a type name that is not an identifier is refused, never used as a path" "|../Alpha: not a type name" \
    "$PENDING_ABSORBED|$PENDING_NOT_ABSORBED"

mk_trees symlink
LINKED="$ROOT/symlink/linked-base"
if ln -s "$BASE" "$LINKED" 2>/dev/null && [ -L "$LINKED" ]; then
    pending_registration_absorb "$LINKED" "$REGEN" "$OUT" Alpha
    check "a symlinked baseline is copied, never written through" "Alpha|yes|yes" \
        "$PENDING_ABSORBED|$([ -f "$BASE/Alpha.d.ts" ] && echo yes || echo no)|$(grep -qF '{ Alpha }' "$BASE/index.ts" && echo yes || echo no)"
else
    skip "symlinked baseline: 'ln -s' made no symlink here (a copy, or refused)"
fi

mk_trees readonly
chmod -R a-w "$BASE"
pending_registration_absorb "$BASE" "$REGEN" "$OUT" Alpha; rc=$?
check "a read-only baseline (the sibling store) still absorbs" "0|Alpha|same" "$rc|$PENDING_ABSORBED|$(same "$OUT" "$REGEN")"
chmod -R u+w "$BASE"

echo "$pass passed, $failn failed, $skipn skipped"
[ "$failn" -eq 0 ]
