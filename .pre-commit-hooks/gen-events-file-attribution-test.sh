#!/usr/bin/env bash
#
# Tests for `lib/gen-events-file-attribution.sh` and the registry reader it
# drives, `lib/gen-events-registry.mjs`: "which of the drifted FILES could
# this push have moved?"
#
# Run: bash .pre-commit-hooks/gen-events-file-attribution-test.sh
#
# Like gen-events-drift-attribution-test.sh, every case builds a throwaway git
# repo with a real "origin" under $TMPDIR, plus two small generated trees (the
# schemas-side BASELINE and the REGENERATED one). Nothing here needs a Rust
# toolchain, a qontinui-schemas checkout or the network. It does need `node`,
# which the drift hook itself cannot run without; without it every case is
# reported SKIPPED rather than passed.
#
# The two cases the per-file rule exists for, pinned first:
#
#   * drift already present at the merge-base, plus a push that touches runner
#     Rust but nothing that can move those files -> PRE-EXISTING (passes);
#   * the same, plus a push that registers a type the bindings do not carry
#     -> MINE, naming that type and its export line, and NOT the old drift.
#
# Then both directions of every rule, and every premise whose failure must fall
# back to the whole-push verdict rather than clear anyone.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/gen-events-attribution.sh
. "$SCRIPT_DIR/lib/gen-events-attribution.sh"
# shellcheck source=lib/gen-events-file-attribution.sh
. "$SCRIPT_DIR/lib/gen-events-file-attribution.sh"

# Before the first `git`: a hook's GIT_DIR would otherwise aim every fixture
# command at the real repository. Same reasoning, and the same stage-env and
# command-line-config scrub, as gen-events-drift-attribution-test.sh.
gen_events_clear_inherited_git_env
for v in $(compgen -v PRE_COMMIT_) $(compgen -v GIT_CONFIG); do unset "$v"; done
unset GEN_EVENTS_STAGE_HINT QONTINUI_GEN_EVENTS_DRIFT_STRICT
exec </dev/null

PASS=0
FAIL=0
SKIP=0
FIXTURE_ROOTS=()
cleanup() {
    local root
    for root in "${FIXTURE_ROOTS[@]:-}"; do
        [ -n "$root" ] || continue
        chmod -R u+w "$root" 2>/dev/null || true
        rm -rf "$root"
    done
}
trap cleanup EXIT

check() {
    local label="$1" want="$2" got="$3"
    if [ "$want" = "$got" ]; then
        printf '  ok   %s\n' "$label"
        PASS=$((PASS + 1))
    else
        printf '  FAIL %s\n       want: %s\n       got : %s\n' "$label" "$want" "$got"
        FAIL=$((FAIL + 1))
    fi
}
skip_note() {
    printf '  SKIP %s\n' "$1"
    SKIP=$((SKIP + 1))
}

# yes when the file $2 is listed in the `<file>\t<why>` list $1.
listed() {
    local f
    while IFS=$'\t' read -r f _; do
        [ "$f" = "$2" ] && { echo yes; return 0; }
    done <<< "$1"
    echo no
}
# The files of a `<file>\t<why>` list, sorted and space-joined.
files_of() {
    printf '%s\n' "$1" | cut -f1 | sed '/^$/d' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'
}

if ! command -v node >/dev/null 2>&1; then
    echo "gen-events-drift per-file attribution"
    skip_note "every case: node is not on PATH (the drift hook needs it too)"
    echo
    echo "$PASS passed, $FAIL failed, $SKIP SKIPPED"
    exit 0
fi

# ── the fixture ─────────────────────────────────────────────────────────────

# The registry a fixture starts from: two qontinui_types types, one generic
# qontinui_vision_core type in a nested block, and one runner-local type, with
# a test module after the function (as the real file has). $1 is spliced in as
# extra `add!` lines after Beta; $2, when `nobeta`, drops Beta.
registry_text() {
    local extra="${1:-}" beta='    add!("Beta", qb::Beta);'
    [ "${2:-}" = "nobeta" ] && beta=""
    cat <<EOF
//! Fixture registry. A comment naming add!("Fake", qa::Fake) is not a registration.
use schemars::schema_for;
use serde_json::{Map, Value};

pub fn export_all_schemas() -> Value {
    use qontinui_types::{alpha as qa, beta as qb};
    let mut m: Map<String, Value> = Map::new();

    macro_rules! add {
        (\$name:literal, \$ty:ty) => {
            m.insert(
                \$name.to_string(),
                serde_json::to_value(schema_for!(\$ty)).unwrap(),
            );
        };
    }

    add!("Alpha", qa::Alpha);
$beta
$extra
    {
        use qontinui_vision_core::observation as qob;
        add!("Gen", qob::Obs<serde_json::Value>);
    }
    use crate::local_payloads as lp;
    add!(
        "RunnerLocal",
        lp::RunnerLocal
    );

    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports() {
        let _ = export_all_schemas();
    }
}
EOF
}

REGISTERED="Alpha Beta Gen RunnerLocal"

# The known exporter, as the premise check requires it.
exporter_text() {
    cat <<'EOF'
// exports every schemars::JsonSchema type
fn main() {
    qontinui_runner_lib::instance_env::enforce_instance_root_or_exit(false);
    let args: Vec<String> = std::env::args().collect();

    let schemas = qontinui_runner_lib::schema_export::export_all_schemas();

    if args.iter().any(|a| a == "--list") {
        if let Some(obj) = schemas.as_object() {
            for key in obj.keys() {
                println!("{}", key);
            }
        }
        return;
    }

    let pretty = args.iter().any(|a| a == "--pretty");

    let output = if pretty {
        serde_json::to_string_pretty(&schemas).expect("Failed to serialize schemas")
    } else {
        serde_json::to_string(&schemas).expect("Failed to serialize schemas")
    };

    println!("{}", output);
}
EOF
}

fixture() {
    local root
    if ! root="$(mktemp -d -t gen-events-file-attr-XXXXXX)" || [ ! -d "$root" ]; then
        printf '  FATAL could not create a fixture directory\n' >&2
        exit 1
    fi
    FIXTURE_ROOTS+=("$root")
    UPSTREAM="$root/upstream"
    WORK="$root/work"
    BTREE="$root/baseline"
    RTREE="$root/regen"

    git init --quiet --initial-branch=main "$UPSTREAM"
    git -C "$UPSTREAM" config user.email t@example.com
    git -C "$UPSTREAM" config user.name t
    git -C "$UPSTREAM" config core.autocrlf false
    git -C "$UPSTREAM" config commit.gpgsign false
    git -C "$UPSTREAM" config core.hooksPath "$UPSTREAM/.git/no-hooks"
    mkdir -p "$UPSTREAM/src-tauri/src/bin" "$UPSTREAM/src-tauri/scripts" "$UPSTREAM/src" "$UPSTREAM/vendor/x"
    registry_text > "$UPSTREAM/src-tauri/src/schema_export.rs"
    printf '// lib\npub mod other;\npub mod schema_export;\n' > "$UPSTREAM/src-tauri/src/lib.rs"
    printf '// other\n' > "$UPSTREAM/src-tauri/src/other.rs"
    # The exporter mentions JsonSchema, which the markdown premise guard of
    # the whole-push attribution expects to find.
    exporter_text > "$UPSTREAM/src-tauri/src/bin/export_schemas.rs"
    printf '# gen\n' > "$UPSTREAM/src-tauri/scripts/generate_types.sh"
    printf '// build\n' > "$UPSTREAM/src-tauri/build.rs"
    printf '[package]\n' > "$UPSTREAM/Cargo.toml"
    printf '# lock\n' > "$UPSTREAM/Cargo.lock"
    printf '// v\n' > "$UPSTREAM/vendor/x/lib.rs"
    printf '// ui\n' > "$UPSTREAM/src/app.ts"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m base

    git clone --quiet --config core.autocrlf=false "$UPSTREAM" "$WORK"
    git -C "$WORK" config user.email t@example.com
    git -C "$WORK" config user.name t
    git -C "$WORK" config commit.gpgsign false
    git -C "$WORK" config core.hooksPath "$WORK/.git/no-hooks"

    # The fixture is only a fixture if git agrees (a leaked GIT_DIR would aim
    # every command at the real repository).
    local want got toplevel
    want="$(cd "$WORK" && pwd -P)"
    toplevel="$(git -C "$WORK" rev-parse --show-toplevel 2>/dev/null || true)"
    got=""
    [ -n "$toplevel" ] && got="$(cd "$toplevel" 2>/dev/null && pwd -P)"
    if [ -z "$got" ] || [ "$want" != "$got" ]; then
        printf '  FATAL fixture escaped: git -C %s resolves to %s\n' "$want" "${got:-<unresolvable>}" >&2
        exit 1
    fi
}

commit_change() {
    local path="$1" text="$2"
    mkdir -p "$WORK/$(dirname "$path")"
    printf '%s\n' "$text" >> "$WORK/$path"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "touch $path"
}

# Replace the registry in the work tree and commit it.
commit_registry() {
    registry_text "$@" > "$WORK/src-tauri/src/schema_export.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "registry"
}

# Commit a raw registry text (from stdin) to the BASE, then reset the work
# clone onto it, so a case can put an unusual registry at the merge-base.
seed_base_registry() {
    cat > "$UPSTREAM/src-tauri/src/schema_export.rs"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m "base: registry"
    git -C "$WORK" fetch --quiet origin
    git -C "$WORK" reset --hard --quiet origin/main
}

# A generated tree at $1 for the names that follow: one `<Name>.d.ts` each
# (content `// <Name> v1`, or the content in $TREE_CONTENT_<Name> when set),
# plus index.ts with one sorted export line per name.
mk_tree() {
    local dir="$1" name var; shift
    rm -rf "$dir"
    mkdir -p "$dir"
    for name in "$@"; do
        var="TREE_CONTENT_$name"
        printf '%s\n' "${!var:-// $name v1}" > "$dir/$name.d.ts"
    done
    {
        printf '%s\n' "${TREE_HEADER:-// Auto-generated by compile_typescript.mjs — do not edit}"
        printf '%s\n' "$@" | LC_ALL=C sort | while IFS= read -r name; do
            printf "export type { %s } from './%s';\n" "$name" "$name"
        done
    } > "$dir/index.ts"
}

decide_files() {
    unset FILE_ATTRIBUTION_STATE FILE_ATTRIBUTION_REASON FILE_ATTRIBUTION_MINE FILE_ATTRIBUTION_PRE
    gen_events_attribution "$WORK"
    gen_events_file_attribution "$WORK" "$BTREE" "$RTREE"
}

echo "gen-events-drift per-file attribution"
echo "  -- the two cases this exists for --"

# Pre-existing presence drift: the bindings carry Pending (a type the runner
# has not registered yet, at the merge-base or now). The push edits runner
# Rust that cannot reach the registry.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "the whole-push attribution says MINE (runner Rust touched)" "mine" "$ATTRIBUTION_STATE"
check "pre-existing drift + a clean diff is PRE-EXISTING per file" "pre-existing" "$FILE_ATTRIBUTION_STATE"
check "  Pending.d.ts is listed as pre-existing" yes "$(listed "$FILE_ATTRIBUTION_PRE" Pending.d.ts)"
check "  index.ts is listed as pre-existing" yes "$(listed "$FILE_ATTRIBUTION_PRE" index.ts)"
check "  nothing is held against the push" "" "$FILE_ATTRIBUTION_MINE"

# The same drift, plus a push that registers Gamma, which the bindings do not
# carry: the push's own drift fails it, and only that.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
commit_registry '    add!("Gamma", qa::Gamma);'
decide_files
check "a push that registers an unbound type is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  and names exactly Gamma.d.ts and index.ts" "Gamma.d.ts index.ts" "$(files_of "$FILE_ATTRIBUTION_MINE")"
check "  the export line names Gamma" yes \
    "$(printf '%s\n' "$FILE_ATTRIBUTION_MINE" | grep -F 'index.ts' | grep -F 'Gamma' >/dev/null && echo yes || echo no)"
check "  while the old drift stays pre-existing" "Pending.d.ts" "$(files_of "$FILE_ATTRIBUTION_PRE")"

echo "  -- presence: which names the push registers or unregisters --"

fixture
mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" Alpha Gen RunnerLocal
commit_registry "" nobeta
decide_files
check "unregistering a type the bindings still carry is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  Beta.d.ts is the push's" yes "$(listed "$FILE_ATTRIBUTION_MINE" Beta.d.ts)"

# Registering a type the bindings ALREADY carry (the runner half of a
# pending registration) removes drift: nothing is left to blame.
fixture
mk_tree "$BTREE" $REGISTERED Gamma Pending
mk_tree "$RTREE" $REGISTERED Gamma
commit_registry '    add!("Gamma", qa::Gamma);'
decide_files
check "registering a type the bindings already carry leaves only old drift" "pre-existing" "$FILE_ATTRIBUTION_STATE"
check "  which is Pending" "Pending.d.ts index.ts" "$(files_of "$FILE_ATTRIBUTION_PRE")"

# A binding the regeneration emits for a name registered at the merge-base
# too: the binding was never committed, which is not this push's doing.
fixture
mk_tree "$BTREE" Alpha Gen RunnerLocal
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a binding missing for a long-registered type is PRE-EXISTING" "pre-existing" "$FILE_ATTRIBUTION_STATE"
check "  Beta.d.ts is listed" yes "$(listed "$FILE_ATTRIBUTION_PRE" Beta.d.ts)"

# Registry edits in the WORKING TREE count: the regeneration compiled it.
fixture
mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED Gamma
registry_text '    add!("Gamma", qa::Gamma);' > "$WORK/src-tauri/src/schema_export.rs"
decide_files
check "an UNCOMMITTED registration is still MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  Gamma.d.ts is the push's" yes "$(listed "$FILE_ATTRIBUTION_MINE" Gamma.d.ts)"

echo "  -- registry-adjacent inputs move presence without an add! line --"

for adj in "src-tauri/src/bin/export_schemas.rs" "src-tauri/scripts/generate_types.sh"; do
    fixture
    mk_tree "$BTREE" $REGISTERED Pending
    mk_tree "$RTREE" $REGISTERED
    commit_change "$adj" "// changed"
    decide_files
    check "$adj makes presence drift MINE" "mine" "$FILE_ATTRIBUTION_STATE"
    check "  Pending.d.ts is held against it" yes "$(listed "$FILE_ATTRIBUTION_MINE" Pending.d.ts)"
done

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/lib.rs" "pub mod another;"
decide_files
check "a lib.rs that still declares pub mod schema_export; is a plain source" "pre-existing" "$FILE_ATTRIBUTION_STATE"

# A lib.rs that could point `schema_export` at another file means the
# registry the reader parses may not be the one that compiled: fall back.
for routed in \
    '#[path = "elsewhere.rs"]\npub mod schema_export;' \
    '#[cfg_attr(all(), path = "elsewhere.rs")]\npub mod schema_export;' \
    '#[cfg_attr(\n    all(),\n    path = "elsewhere.rs"\n)]\npub mod schema_export;' \
    '// no module declared'
do
    fixture
    mk_tree "$BTREE" $REGISTERED Pending
    mk_tree "$RTREE" $REGISTERED
    printf "// lib\npub mod other;\n$routed\n" > "$WORK/src-tauri/src/lib.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "route the registry"
    decide_files
    check "lib.rs '$(printf '%s' "$routed" | head -c 40 | tr '\n' ' ')' falls back to the whole push" "whole-push" "$FILE_ATTRIBUTION_STATE"
done

# A `#[path]` that is ALREADY on main redirects the registry with lib.rs
# untouched by the push: the working-tree check catches it all the same.
fixture
printf '// lib\npub mod other;\n#[path = "elsewhere.rs"]\npub mod schema_export;\n' > "$UPSTREAM/src-tauri/src/lib.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "base: route the registry"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a #[path] already on main, lib.rs untouched, still falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"

# The other half of the lib.rs premise: a push that REMOVES a #[path] leaves a
# plain working tree, while the merge-base registry was some other file.
fixture
printf '// lib\npub mod other;\n#[path = "elsewhere.rs"]\npub mod schema_export;\n' > "$UPSTREAM/src-tauri/src/lib.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "base: route the registry"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
printf '// lib\npub mod other;\npub mod schema_export;\n' > "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "unroute the registry"
decide_files
check "a push that removes a #[path] from lib.rs falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  naming the merge-base" yes \
    "$(case "$FILE_ATTRIBUTION_REASON" in *"at the merge-base"*) echo yes ;; *) echo no ;; esac)"

# lib.rs can rebind a trusted crate name for the whole crate.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
printf '// lib\nextern crate self as qontinui_types;\npub mod other;\npub mod schema_export;\n' > "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "rebind a trusted crate"
decide_files
check "extern crate self as qontinui_types in lib.rs falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
printf '// lib\nextern crate self as qontinui_runner_lib;\npub mod other;\npub mod schema_export;\n' > "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "the form the real lib.rs uses"
decide_files
check "  while the alias the real lib.rs declares does not" "pre-existing" "$FILE_ATTRIBUTION_STATE"

# Comments and raw identifiers cannot walk an alias past the allowlist.
for rebind in \
    'extern crate self /* c */ as qontinui_types;' \
    'extern crate self as r#qontinui_types;' \
    'extern crate self as serde_json;' \
    'extern crate self as schemars;'
do
    fixture
    mk_tree "$BTREE" $REGISTERED Pending
    mk_tree "$RTREE" $REGISTERED
    printf '// lib\n%s\npub mod other;\npub mod schema_export;\n' "$rebind" > "$WORK/src-tauri/src/lib.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "rebind a trusted crate"
    decide_files
    check "lib.rs '$rebind' falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
done

# A declaration that is not the one that compiles, beside one that is.
for decoy in \
    '#[cfg(any())]\npub mod schema_export;\nmod real_schema_export;' \
    '/* pub mod schema_export; */\nmod schema_export { }' \
    '// pub mod schema_export;\nmod schema_export { }' \
    'pub mod schema_export;\nmod schema_export { }'
do
    fixture
    mk_tree "$BTREE" $REGISTERED Pending
    mk_tree "$RTREE" $REGISTERED
    printf "// lib\npub mod other;\n$decoy\n" > "$WORK/src-tauri/src/lib.rs"
    git -C "$WORK" add -A >/dev/null
    git -C "$WORK" commit --quiet -m "a decoy declaration"
    decide_files
    check "lib.rs decoy '$(printf '%s' "$decoy" | head -c 34 | tr '\n' ' ')' falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
done

# A comment that merely mentions the words is not a reason to fall back.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
printf '// lib: path = x, extern crate y\npub mod other;\npub mod schema_export;\n' > "$WORK/src-tauri/src/lib.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "a comment naming the words"
decide_files
check "a lib.rs comment naming path = and extern does not trip the premise" "pre-existing" "$FILE_ATTRIBUTION_STATE"

# The exporter rewrites what the registry produced: already on main, untouched here.
fixture
exporter_text | sed 's/let schemas = \(.*\);/let schemas = qontinui_runner_lib::payloads::post(\1);/' \
    > "$UPSTREAM/src-tauri/src/bin/export_schemas.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "base: post-process"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/payloads.rs" "// changes every schema"
decide_files
check "an exporter that post-processes the map falls back, with that module edited" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  naming the exporter" yes \
    "$(case "$FILE_ATTRIBUTION_REASON" in *"known exporter"*) echo yes ;; *) echo no ;; esac)"

# A file under schema_export/ moves presence without an add! line changing.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/schema_export/more.rs" "// more"
decide_files
check "a schema_export/ file makes presence drift MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  Pending.d.ts is held against it" yes "$(listed "$FILE_ATTRIBUTION_MINE" Pending.d.ts)"

echo "  -- content: where the type is defined decides which inputs can move it --"

fixture
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "content drift on a qontinui_types type + a runner change is PRE-EXISTING" "pre-existing" "$FILE_ATTRIBUTION_STATE"
check "  Alpha.d.ts is listed" yes "$(listed "$FILE_ATTRIBUTION_PRE" Alpha.d.ts)"

fixture
TREE_CONTENT_Gen='// Gen schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a generic over external types (qob::Obs<serde_json::Value>) is external too" "pre-existing" "$FILE_ATTRIBUTION_STATE"

fixture
TREE_CONTENT_RunnerLocal='// RunnerLocal schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "content drift on a runner-LOCAL type + a runner change is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  RunnerLocal.d.ts is the push's" yes "$(listed "$FILE_ATTRIBUTION_MINE" RunnerLocal.d.ts)"

for shape_input in Cargo.lock Cargo.toml src-tauri/build.rs vendor/x/lib.rs crates/spec-check/src/lib.rs src-tauri/src/schema_export.rs; do
    fixture
    TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
    mk_tree "$RTREE" $REGISTERED
    if [ "$shape_input" = "src-tauri/src/schema_export.rs" ]; then
        commit_registry '    // a registry comment'
    else
        commit_change "$shape_input" "# changed"
    fi
    decide_files
    check "$shape_input shapes every binding: external content drift is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
done

# schema_export/ is a shape input as well as a presence one.
fixture
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/schema_export/more.rs" "// more"
decide_files
check "a schema_export/ file shapes every binding: external content drift is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  Alpha.d.ts is the push's" yes "$(listed "$FILE_ATTRIBUTION_MINE" Alpha.d.ts)"

# The false clear this reader once gave: an absolute path through this crate's
# own extern-crate alias reads as external unless its first segment is a root.
fixture
registry_text '    add!("Abs", ::qontinui_runner_lib::payloads::Abs);' | seed_base_registry
TREE_CONTENT_Abs='// Abs schemas-side' mk_tree "$BTREE" $REGISTERED Abs
mk_tree "$RTREE" $REGISTERED Abs
commit_change "src-tauri/src/payloads.rs" "// the type's own module"
decide_files
check "a ::-rooted path into this crate, with its module edited, is MINE" "mine" "$FILE_ATTRIBUTION_STATE"
check "  Abs.d.ts is the push's" yes "$(listed "$FILE_ATTRIBUTION_MINE" Abs.d.ts)"

# And its sibling: a schema_for that is really a runner module's.
fixture
registry_text | sed 's/^use schemars::schema_for;/use crate::schema_tools::schema_for;/' | seed_base_registry
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/schema_tools.rs" "// changes every schema"
decide_files
check "a schema_for imported from this crate falls back, with that module edited" "whole-push" "$FILE_ATTRIBUTION_STATE"

# A shape input that is only DIRTY, not committed: at push stage the reason
# must say the push does not carry it, never "this push changes it".
fixture
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $REGISTERED
mk_tree "$RTREE" $REGISTERED
commit_change "src/app.ts" "// ui only, committed"
printf '# dirty\n' >> "$WORK/Cargo.lock"
(
    GEN_EVENTS_STAGE_HINT=push
    decide_files
    printf '%s\n' "$FILE_ATTRIBUTION_STATE" > "$WORK/.st"
    printf '%s\n' "$FILE_ATTRIBUTION_MINE" > "$WORK/.mine"
)
check "a dirty Cargo.lock still holds external content drift (the regen read it)" "mine" "$(cat "$WORK/.st")"
check "  and the reason says the push does not carry it" yes \
    "$(grep -F 'Alpha.d.ts' "$WORK/.mine" | grep -F 'Cargo.lock (uncommitted changes — not part of this push)' >/dev/null && echo yes || echo no)"
check "  never 'this push changes'" no \
    "$(grep -qF 'this push changes' "$WORK/.mine" && echo yes || echo no)"

echo "  -- the barrel --"

fixture
mk_tree "$BTREE" $REGISTERED
TREE_HEADER='// a different header' mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a barrel whose non-export lines differ, with no writer touched, is PRE-EXISTING" "pre-existing" "$FILE_ATTRIBUTION_STATE"

fixture
mk_tree "$BTREE" $REGISTERED
TREE_HEADER='// a different header' mk_tree "$RTREE" $REGISTERED
commit_change "src-tauri/scripts/generate_types.sh" "# changed"
decide_files
check "  but MINE when generate_types.sh changed" "mine" "$FILE_ATTRIBUTION_STATE"

echo "  -- every doubt falls back to the whole push --"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Zeta
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a regenerated binding the registry does not list falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  and says why" yes \
    "$(case "$FILE_ATTRIBUTION_REASON" in *"does not account for"*Zeta*) echo yes ;; *) echo no ;; esac)"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
commit_registry '    #[cfg(feature = "x")]
    add!("Gamma", qa::Gamma);'
decide_files
check "a cfg in the registry falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  naming the cfg" yes "$(case "$FILE_ATTRIBUTION_REASON" in *cfg*) echo yes ;; *) echo no ;; esac)"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
commit_registry '    m.extend(other::more());
    add!("Gamma", qa::Gamma);'
decide_files
check "a second way into the map (m.extend) falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
registry_text '    add!("Gamma", qa::Gamma);' \
    | sed 's/\$name\.to_string()/format!("{}", $name)/' > "$WORK/src-tauri/src/schema_export.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "change the macro"
decide_files
check "a push that changes the add! macro itself falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"

# Both revisions parse, and their macros differ: the name sets are not comparable.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
registry_text '    add!("Gamma", qa::Gamma);' \
    | sed 's/\.unwrap(),$/.unwrap()/' > "$WORK/src-tauri/src/schema_export.rs"
git -C "$WORK" add -A >/dev/null
git -C "$WORK" commit --quiet -m "a macro that still has the one allowed shape"
decide_files
check "two readable registries with different macros fall back" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  naming the macro" yes \
    "$(case "$FILE_ATTRIBUTION_REASON" in *"macro differs"*) echo yes ;; *) echo no ;; esac)"

# A status with no rule (a typechange) must not be dropped from the verdict.
fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
rm -f "$RTREE/Alpha.d.ts"
if ln -s Beta.d.ts "$RTREE/Alpha.d.ts" 2>/dev/null && [ -L "$RTREE/Alpha.d.ts" ]; then
    commit_change "src-tauri/src/other.rs" "// a runner change"
    decide_files
    check "a typechange in the trees falls back rather than vanishing" "whole-push" "$FILE_ATTRIBUTION_STATE"
else
    skip_note "a typechange in the trees: this filesystem makes no symlinks"
fi

# The merge-base registry is unreadable (a cfg) and the push rewrites it.
fixture
registry_text '    #[cfg(feature = "x")]
    add!("Old", qa::Old);' | seed_base_registry
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_registry ""
decide_files
check "an unreadable registry at the merge-base falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  naming the merge-base" yes \
    "$(case "$FILE_ATTRIBUTION_REASON" in *"at the merge-base"*) echo yes ;; *) echo no ;; esac)"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
mkdir -p "$RTREE/nested"
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a directory inside a generated tree falls back" "whole-push" "$FILE_ATTRIBUTION_STATE"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
printf 'stray\n' > "$RTREE/notes.txt"
commit_change "src-tauri/src/other.rs" "// a runner change"
decide_files
check "a file that is not a per-type binding is held against the push" "mine" "$FILE_ATTRIBUTION_STATE"
check "  notes.txt is listed" yes "$(listed "$FILE_ATTRIBUTION_MINE" notes.txt)"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
commit_change "src/app.ts" "// ui only"
decide_files
check "a push that touches no codegen input never reaches the per-file rule" "whole-push" "$FILE_ATTRIBUTION_STATE"
check "  (the whole-push attribution already cleared it)" "pre-existing" "$ATTRIBUTION_STATE"

echo "  -- the hook's errexit context survives a large registry --"

# The hook runs under `set -euo pipefail`. A pipe whose reader exits early
# can SIGPIPE its writer there and end the hook with no verdict. Run one
# whole decision that way, over 12000 registrations: about 216 KB of registry
# rows, with the drifted type (Alpha) sorting first so an early-exiting reader
# leaves most of them unwritten. The size matters. A reader takes up to 96 KB
# before it stops and the pipe holds another 64 KB, so anything much under
# 160 KB lets the writer finish, and the case passes or fails by timing.
fixture
BIG_EXTRA="$(for i in $(seq 1 12000); do printf '    add!("Big%05d", qa::Big%05d);\n' "$i" "$i"; done)"
registry_text "$BIG_EXTRA" > "$UPSTREAM/src-tauri/src/schema_export.rs"
git -C "$UPSTREAM" add -A >/dev/null
git -C "$UPSTREAM" commit --quiet -m "base: a large registry"
git -C "$WORK" fetch --quiet origin
git -C "$WORK" reset --hard --quiet origin/main
BIG_NAMES="$REGISTERED $(seq -f 'Big%05.0f' 1 12000 | tr '\n' ' ')"
# shellcheck disable=SC2086
TREE_CONTENT_Alpha='// Alpha schemas-side' mk_tree "$BTREE" $BIG_NAMES Pending
# shellcheck disable=SC2086
mk_tree "$RTREE" $BIG_NAMES
commit_change "src-tauri/src/other.rs" "// a runner change"
# A plain statement, deliberately. `x="$(set -e; ...)" || ...` or an `if` around
# it switches errexit OFF inside, whatever the `set -e` says, and the case then
# cannot fail.
(
    set -euo pipefail
    decide_files >/dev/null 2>&1
    printf '%s' "$FILE_ATTRIBUTION_STATE" > "$WORK/.big-state"
)
BIG_RC=$?
check "a 12000-entry decision is not ended by errexit" 0 "$BIG_RC"
check "  and reaches its verdict" "pre-existing" "$(cat "$WORK/.big-state" 2>/dev/null)"

echo "  -- the registry reader, directly --"

PARSER="$SCRIPT_DIR/lib/gen-events-registry.mjs"
parse_text() {
    local f
    f="$(mktemp "${TMPDIR:-/tmp}/gen-events-registry-case.XXXXXX")"
    cat > "$f"
    node "$(_gen_events_native_path "$PARSER")" "$(_gen_events_native_path "$f")"
    local rc=$?
    rm -f "$f"
    return "$rc"
}

out="$(registry_text | parse_text)"; rc=$?
check "the fixture registry parses" 0 "$rc"
check "  names from add! only (the commented add!(\"Fake\") is not one)" "Alpha Beta Gen RunnerLocal" \
    "$(printf '%s\n' "$out" | sed '1d' | cut -f1 | tr '\n' ' ' | sed 's/ $//')"
check "  Alpha is external" "external" "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Alpha"{print $2}')"
check "  RunnerLocal (crate:: alias) is local" "local" "$(printf '%s\n' "$out" | awk -F'\t' '$1=="RunnerLocal"{print $2}')"

out="$(registry_text '    add!("Braces", qa::Braces); // "}" and '"'"'{'"'"' in a comment
    let _s = "{ add!(\"InString\", qa::X) }";
    let _c = '"'"'{'"'"';
    let _r = r#"} raw { "#;
    /* add!("InBlock", qa::Y) /* nested */ still a comment */' | parse_text)"; rc=$?
check "strings, chars, raw strings and nested comments do not confuse it" 0 "$rc"
check "  only the real add! lines count" "Alpha Beta Braces Gen RunnerLocal" \
    "$(printf '%s\n' "$out" | sed '1d' | cut -f1 | tr '\n' ' ' | sed 's/ $//')"

out="$(registry_text '    use crate::shadow as qa;
    add!("Twice", qa::Twice);' | parse_text)"; rc=$?
check "an alias declared twice, differently, is not external" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Twice"{print $2}')"

out="$(registry_text '    use crate::payloads::*;
    add!("Globbed", qa::Globbed);' | parse_text)"; rc=$?
check "a glob import in scope makes every type local" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Alpha"{print $2}')"

out="$(registry_text '    add!("Prim", String);' | parse_text)"
check "a prelude type is not provably external" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Prim"{print $2}')"

out="$(registry_text '    add!("Ref", &qa::Ref);' | parse_text)"
check "a type that is not a plain path is local" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Ref"{print $2}')"

out="$(registry_text | sed 's/\$name\.to_string()/format!("Q{}", $name)/' | parse_text)"; rc=$?
check "a macro that builds its key from a literal is refused" 1 "$rc"

out="$( { registry_text; printf '#[path = "x.rs"]\nmod elsewhere;\n'; } | parse_text)"; rc=$?
check "a path attribute at file level is refused" 1 "$rc"
check "  naming it" yes "$(case "$out" in *"path attribute"*) echo yes ;; *) echo no ;; esac)"

out="$(registry_text '    if crate::flags::on() { add!("Gated", qa::Gated); }' | parse_text)"; rc=$?
check "an add! under a runtime condition is refused" 1 "$rc"

out="$( { registry_text '    {
        use qontinui_vision_core::observation as qob2;
        add!("InBlock", qob2::InBlock);
    }
    add!("Shadowed", qob2::Shadowed);'; printf 'mod qob2 { pub struct Shadowed; }\n'; } | parse_text)"; rc=$?
check "a file-level mod makes a same-named block alias local" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Shadowed"{print $2}')"

out="$(registry_text '    add!("Dup", qa::Dup);
    add!("Dup", lp::Dup);' | parse_text)"; rc=$?
check "a name registered once external and once local is local" "local" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Dup"{print $2}')"

out="$(registry_text '    add!("Abs", ::qontinui_runner_lib::payloads::Abs);' | parse_text)"
check "a ::-rooted path is local" "local" "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Abs"{print $2}')"

out="$(registry_text '    add!("AbsArg", qa::W<::qontinui_runner_lib::payloads::C>);' | parse_text)"
check "  also as a generic argument" "local" "$(printf '%s\n' "$out" | awk -F'\t' '$1=="AbsArg"{print $2}')"

out="$(registry_text '    add!("Nested", qa::W<qb::X>);' | parse_text)"
check "  while an ordinary generic over external types stays external" "external" \
    "$(printf '%s\n' "$out" | awk -F'\t' '$1=="Nested"{print $2}')"

out="$(registry_text | sed 's/^use schemars::schema_for;/use crate::schema_tools::schema_for;/' | parse_text)"; rc=$?
check "a schema_for from this crate is refused" 1 "$rc"
check "  naming it" yes "$(case "$out" in *schema_for*) echo yes ;; *) echo no ;; esac)"

out="$(registry_text | sed '/^use schemars::schema_for;/d' | parse_text)"; rc=$?
check "a schema_for with no import is refused" 1 "$rc"

out="$( { registry_text; printf 'macro_rules! schema_for { ($t:ty) => { () }; }\n'; } | parse_text)"; rc=$?
check "a schema_for macro defined in the file is refused" 1 "$rc"

out="$(registry_text | sed 's/^use serde_json::{Map, Value};/use serde_json::{Map, Value};\nuse crate::j as serde_json;/' | parse_text)"; rc=$?
check "a rebound serde_json is refused" 1 "$rc"

out="$(registry_text | sed 's/^use serde_json::{Map, Value};/use serde_json::Value;\nuse crate::tracking::Map;/' | parse_text)"; rc=$?
check "an unqualified Map from this crate is refused" 1 "$rc"

out="$( { registry_text; printf 'extern crate self as qontinui_types;\n'; } | parse_text)"; rc=$?
check "an extern crate in the registry file is refused" 1 "$rc"

out="$(registry_text '    extern crate self as qontinui_types;' | parse_text)"; rc=$?
check "  also inside the function body" 1 "$rc"

out="$(registry_text | sed 's/^use schemars::schema_for;/use crate::m as schemars;\nuse schemars::schema_for;/' | parse_text)"; rc=$?
check "a rebound schemars is refused" 1 "$rc"

out="$(registry_text | sed 's/    Value::Object(m)/    crate::m::Object(m)/' | parse_text)"; rc=$?
check "a final Object(m) from this crate is refused" 1 "$rc"

out="$(registry_text | sed 's/^use serde_json::{Map, Value};/use serde_json::Map;\nuse crate::v::Value;/' | parse_text)"; rc=$?
check "an unqualified Value from this crate is refused" 1 "$rc"

out="$(registry_text | sed 's/    Value::Object(m)/    serde_json::Value::Object(m)/' | parse_text)"; rc=$?
check "  while a fully qualified serde_json::Value is fine" 0 "$rc"

out="$(registry_text '    type Alpha = crate::payloads::Alpha;' | parse_text)"; rc=$?
check "a type item in the function, shadowing an import, is refused" 1 "$rc"

out="$(registry_text '    bind!();' | parse_text)"; rc=$?
check "a macro call in the function other than add! is refused" 1 "$rc"
check "  naming it" yes "$(case "$out" in *bind*) echo yes ;; *) echo no ;; esac)"

out="$(registry_text | sed 's/^pub fn export_all_schemas/rewrite! { pub fn export_all_schemas/; s/^#\[cfg(test)\]/}\n#[cfg(test)]/' | parse_text)"; rc=$?
check "the function inside a macro call is refused" 1 "$rc"

out="$(registry_text | sed 's/^pub fn export_all_schemas/#[rewrite]\npub fn export_all_schemas/' | parse_text)"; rc=$?
check "an attribute on the function is refused" 1 "$rc"

out="$(registry_text | sed '1a #![rewrite]' | parse_text)"; rc=$?
check "an inner attribute in the registry file is refused" 1 "$rc"

for bad in \
    'include!("more.rs");' \
    'mod inner { }' \
    'macro_rules! other { () => {}; }' \
    'let _n = m.len();' \
    'add!(Unquoted, qa::X);' \
    'let _s = "unterminated;'
do
    out="$(registry_text "    $bad" | parse_text)"; rc=$?
    check "a violation is refused, not misread: $bad" 1 "$rc"
done

echo "  -- the hook itself, through every arm this adds --"

# The real gen-events-drift.sh, with a stand-in generate_types.sh that copies
# $RTREE into the scratch dir, and a stand-in schemas checkout. No cargo: with no
# tree-lock library beside this fixture, the guard reports itself absent.
hook_fixture() {
    SCHEMAS="$(dirname "$WORK")/schemas"
    mkdir -p "$SCHEMAS/scripts" "$SCHEMAS/ts/src/generated"
    mkdir -p "$SCHEMAS/node_modules/json-schema-to-typescript/dist/src" "$SCHEMAS/node_modules/prettier"
    printf '{"name":"s","devDependencies":{"json-schema-to-typescript":"1.0.0","prettier":"1.0.0"}}\n' > "$SCHEMAS/package.json"
    printf '{"name":"json-schema-to-typescript","version":"1.0.0","type":"module","main":"index.js"}\n' \
        > "$SCHEMAS/node_modules/json-schema-to-typescript/package.json"
    printf 'export const compile = () => {};\n' > "$SCHEMAS/node_modules/json-schema-to-typescript/index.js"
    printf 'export const toSafeString = (s) => s;\n' > "$SCHEMAS/node_modules/json-schema-to-typescript/dist/src/utils.js"
    printf '{"name":"prettier","version":"1.0.0","type":"module","main":"index.js"}\n' > "$SCHEMAS/node_modules/prettier/package.json"
    printf 'export const format = (s) => s;\n' > "$SCHEMAS/node_modules/prettier/index.js"
    printf '// stand-in\n' > "$SCHEMAS/scripts/compile_typescript.mjs"
    cp -R "$BTREE/." "$SCHEMAS/ts/src/generated/"
    git init --quiet --initial-branch=main "$SCHEMAS"
    # The stand-in generator ships on the base, so it is not an input the push touched.
    printf '#!/usr/bin/env bash\ncp -R "$FAKE_REGEN_TREE/." "$QONTINUI_TS_OUT_DIR/"\n' > "$UPSTREAM/src-tauri/scripts/generate_types.sh"
    git -C "$UPSTREAM" add -A >/dev/null
    git -C "$UPSTREAM" commit --quiet -m "base: stand-in generator"
    git -C "$WORK" fetch --quiet origin
    git -C "$WORK" reset --hard --quiet origin/main
}
# Sets HOOK_RC, and HOOK_OUT and HOOK_ERR to the two streams.
run_real_hook() {
    local strict="${1:-0}" out err
    out="$WORK/.hook.out"; err="$WORK/.hook.err"
    printf 'refs/heads/main %s refs/heads/main %s\n' "$(git -C "$WORK" rev-parse HEAD)" "$(git -C "$WORK" rev-parse origin/main)" \
        | ( cd "$WORK" && QONTINUI_HOOK_REPO_ROOT="$WORK" QONTINUI_SCHEMAS_DIR="$SCHEMAS" FAKE_REGEN_TREE="$RTREE" \
            QONTINUI_GEN_EVENTS_DRIFT_STRICT="$strict" QONTINUI_PREPUSH_NO_TTY=1 \
            bash "$SCRIPT_DIR/gen-events-drift.sh" ) > "$out" 2> "$err"
    HOOK_RC="${PIPESTATUS[1]}"
    HOOK_OUT="$(cat "$out")"; HOOK_ERR="$(cat "$err")"
}
says() { case "$1" in *"$2"*) echo yes ;; *) echo no ;; esac; }

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED
hook_fixture
commit_change "src-tauri/src/other.rs" "// a runner change"
run_real_hook
check "hook: pre-existing drift + a clean diff passes" 0 "$HOOK_RC"
check "  the verdict is pre-existing" yes "$(says "$HOOK_OUT" "PRE-EXISTING DRIFT")"
check "  and it lists the input with its label" yes "$(says "$HOOK_OUT" "src-tauri/src/other.rs  (committed in this push)")"
check "  and the drifted file with its reason" yes "$(says "$HOOK_OUT" "Pending.d.ts  (Pending is not registered at the merge-base either")"
check "  and warns that the remedy would delete it" yes "$(says "$HOOK_OUT" "would also DELETE the 1 binding(s)")"
check "  and never says the push changed nothing" no "$(says "$HOOK_OUT" "changes NONE of the sources")"

run_real_hook 1
check "hook: STRICT blocks that same drift" 1 "$HOOK_RC"
check "  saying it is pre-existing" yes "$(says "$HOOK_ERR" "none of the inputs that changed here can move")"
check "  and naming the file" yes "$(says "$HOOK_ERR" "Pending.d.ts")"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
hook_fixture
commit_registry '    add!("Gamma", qa::Gamma);'
run_real_hook
check "hook: registering an unbound type fails" 1 "$HOOK_RC"
check "  naming the type" yes "$(says "$HOOK_ERR" "Gamma.d.ts  (Gamma is registered here but not at the merge-base")"
check "  keeping the old drift apart" yes "$(says "$HOOK_ERR" "not held against this push")"
check "  and the labelled inputs" yes "$(says "$HOOK_ERR" "src-tauri/src/schema_export.rs  (committed in this push)")"
check "  and not the whole-push fallback" no "$(says "$HOOK_ERR" "Per-file attribution could not run")"

fixture
mk_tree "$BTREE" $REGISTERED Pending
mk_tree "$RTREE" $REGISTERED Gamma
hook_fixture
commit_registry '    #[cfg(feature = "x")]
    add!("Gamma", qa::Gamma);'
run_real_hook
check "hook: a premise failure fails the whole push" 1 "$HOOK_RC"
check "  and says which premise" yes "$(says "$HOOK_ERR" "Per-file attribution could not run: the registry in this tree cannot be read: a cfg inside")"

echo "  -- the real registry --"

# The premises hold on THIS tree's registry. A refactor that breaks one
# (a cfg, a second macro, registrations moved elsewhere) turns every push back
# into a whole-push verdict; this makes that loud at test time instead.
REAL="$SCRIPT_DIR/../src-tauri/src/schema_export.rs"
if [ -f "$REAL" ]; then
    out="$(node "$(_gen_events_native_path "$PARSER")" "$(_gen_events_native_path "$REAL")")"; rc=$?
    check "the real schema_export.rs parses" 0 "$rc"
    [ "$rc" -eq 0 ] || printf '       %s\n' "$out"
    check "  it registers runner-local types (RunnerFinding is local)" "local" \
        "$(printf '%s\n' "$out" | awk -F'\t' '$1=="RunnerFinding"{print $2}')"
    check "  and qontinui_types types (WorkerOutput is external)" "external" \
        "$(printf '%s\n' "$out" | awk -F'\t' '$1=="WorkerOutput"{print $2}')"
    if _gen_events_exporter_premise < "$SCRIPT_DIR/../src-tauri/src/bin/export_schemas.rs" 2>/dev/null; then
        check "the real export_schemas.rs is the known exporter" yes yes
    else
        check "the real export_schemas.rs is the known exporter (update EXPORTER_SHAPE)" yes no
    fi
    # The WORKING TREE copy, as the decision reads it: a commit that adds a
    # path attribute must fail here at that commit, not one commit later.
    if _gen_events_lib_rs_premise < "$SCRIPT_DIR/../src-tauri/src/lib.rs" 2>/dev/null; then
        check "the real lib.rs declares pub mod schema_export; plainly" yes yes
    else
        check "the real lib.rs declares pub mod schema_export; plainly" yes no
    fi
else
    skip_note "the real registry: $REAL is absent (a sparse checkout?)"
fi

echo
if [ "$SKIP" -gt 0 ]; then
    echo "$PASS passed, $FAIL failed, $SKIP SKIPPED (arms not exercised in this environment)"
else
    echo "$PASS passed, $FAIL failed"
fi
[ "$FAIL" -eq 0 ]
