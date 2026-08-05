#!/usr/bin/env bash
#
# generate_types.sh
#
# Exports JSON Schemas from the Rust crate and generates TypeScript and Python
# type definitions. Requires:
#   - cargo (Rust toolchain)
#   - npx + json-schema-to-typescript (npm install -g json-schema-to-typescript)
#   - datamodel-code-generator (pip install datamodel-code-generator) [optional]
#
# Usage:
#   ./scripts/generate_types.sh             # Generate all types
#   ./scripts/generate_types.sh --ts-only   # TypeScript only
#   ./scripts/generate_types.sh --py-only   # Python only
#   ./scripts/generate_types.sh --dry-run   # Print schemas without generating
#
# Environment overrides (all optional; defaults preserve the historical
# sibling-checkout layout that CI recreates):
#
#   QONTINUI_SCHEMAS_DIR   Root of the qontinui-schemas checkout to READ the
#                          codegen helper scripts from.
#   QONTINUI_TS_OUT_DIR    Where the per-type TypeScript .d.ts + index.ts go.
#   QONTINUI_PY_OUT_DIR    Where the Pydantic modules go.
#
# The output overrides exist so a caller that must NOT mutate the schemas
# checkout (notably `.pre-commit-hooks/gen-events-drift.sh`, which runs
# against a shared multi-agent working tree) can redirect every write into a
# scratch directory it owns and then compare, rather than regenerating in
# place. This script DELETES stale artifacts in the output directories
# (`rm -f "$TS_OUT_DIR"/*.d.ts`, `rm -f "$PER_TYPE_DIR"/*.py`), so pointing an
# output at a working tree that holds someone else's uncommitted work destroys
# it. Redirect, don't regenerate in place.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Resolve the cargo output directory the same way cargo itself does, instead of
# hardcoding `$PROJECT_ROOT/target`. Builds in this repo routinely do NOT use
# `target/`: `qontinui-claude-config/scripts/cargo-guard.sh` reroutes to
# `target-agent/` when the running runner holds `target/` locked, and honours a
# caller-supplied `CARGO_TARGET_DIR=../target-pool/slot-N`. With the old
# hardcoded path, `cargo build` would succeed into the *other* directory,
# `$PROJECT_ROOT/target/` would never be created, and the shell would then fail
# setting up the `> "$SCHEMAS_JSON"` redirect below with the thoroughly
# misleading `.../src-tauri/target/schemas.json: No such file or directory`.
#
# Precedence matches cargo: CARGO_TARGET_DIR, then CARGO_BUILD_TARGET_DIR (the
# env form of `[build] target-dir`), then `<crate>/target`. A relative value is
# interpreted relative to PROJECT_ROOT, which is where every cargo invocation
# in this script runs from.
CARGO_OUT_DIR="${CARGO_TARGET_DIR:-${CARGO_BUILD_TARGET_DIR:-$PROJECT_ROOT/target}}"
case "$CARGO_OUT_DIR" in
    /*|[A-Za-z]:[/\\]*) ;;                       # already absolute
    *) CARGO_OUT_DIR="$PROJECT_ROOT/$CARGO_OUT_DIR" ;;
esac
SCHEMAS_JSON="$CARGO_OUT_DIR/schemas.json"

SCHEMAS_DIR="${QONTINUI_SCHEMAS_DIR:-$PROJECT_ROOT/../../qontinui-schemas}"

TS_OUT_DIR="${QONTINUI_TS_OUT_DIR:-$SCHEMAS_DIR/ts/src/generated}"
PY_OUT_DIR="${QONTINUI_PY_OUT_DIR:-$SCHEMAS_DIR/src/qontinui_schemas/generated}"

# On Windows (Git Bash / MSYS), POSIX-style paths like /d/foo are not
# understood by native Windows binaries (node, npx). Convert to Windows paths
# when cygpath is available so that node require() and npx json2ts receive
# usable arguments.
to_native_path() {
    if command -v cygpath &> /dev/null; then
        cygpath -w "$1"
    else
        echo "$1"
    fi
}

# Convert CamelCase / PascalCase to snake_case for filenames.
# e.g. `ConstraintCheck` -> `constraint_check`,
#      `HealthCheckUrl`  -> `health_check_url`,
#      `AppEvent`        -> `app_event`.
# Handles runs of uppercase letters followed by a lowercase letter
# (e.g. `URLPath` -> `url_path`).
to_snake_case() {
    # Step 1: insert `_` between a lowercase/digit and an uppercase letter.
    # Step 2: insert `_` between an uppercase run and a subsequent uppercase
    #         followed by a lowercase (e.g. `HTTPServer` -> `HTTP_Server`).
    # Step 3: lowercase the whole thing.
    echo "$1" \
        | sed -E 's/([a-z0-9])([A-Z])/\1_\2/g' \
        | sed -E 's/([A-Z]+)([A-Z][a-z])/\1_\2/g' \
        | tr '[:upper:]' '[:lower:]'
}

TS_ONLY=false
PY_ONLY=false
DRY_RUN=false

for arg in "$@"; do
    case "$arg" in
        --ts-only) TS_ONLY=true ;;
        --py-only) PY_ONLY=true ;;
        --dry-run) DRY_RUN=true ;;
        *) echo "Unknown argument: $arg"; exit 1 ;;
    esac
done

echo "==> Building export_schemas binary..."
cd "$PROJECT_ROOT"
# No `| tail -N` here: a genuine build failure must print in full. The previous
# `| tail -5` reliably truncated away the rustc diagnostic and left only the
# trailing `error: could not compile ...` summary, which says nothing about the
# cause.
cargo build --bin export_schemas --release 2>&1

# The redirect below cannot create missing parent directories, so make sure the
# resolved cargo output dir exists even on a cold tree.
mkdir -p "$CARGO_OUT_DIR"

echo "==> Exporting schemas to $SCHEMAS_JSON..."
# `$SCHEMAS_JSON` lives under src-tauri/target/, but cargo's default target dir
# for this workspace is qontinui-runner/target/ — so in a fresh worktree the
# parent directory does not exist and the redirection below dies with
# "No such file or directory" before anything is generated. (CI sidesteps this
# by overriding CARGO_TARGET_DIR to src-tauri/target.) Create it explicitly.
mkdir -p "$(dirname "$SCHEMAS_JSON")"
cargo run --bin export_schemas --release -- --pretty > "$SCHEMAS_JSON"

# schemars v1 emits `oneOf` for serde-tagged enums but no sibling
# `discriminator` keyword. datamodel-codegen needs that keyword to emit
# proper `Annotated[Union[...], Field(discriminator='...')]` tagged
# unions — without it, every variant is tried in turn (slow, poor errors).
# The post-processor is idempotent; safe to run unconditionally.
DISCR_SCRIPT="$SCHEMAS_DIR/scripts/add_discriminators.py"
if [ -f "$DISCR_SCRIPT" ] && command -v python >/dev/null 2>&1; then
    echo "==> Annotating oneOf unions with discriminator keyword..."
    # Convert MSYS paths to Windows form for native python.exe — same
    # rationale as the node + datamodel-codegen calls below.
    # Without this the pre-push gen-events-drift hook fails on Windows
    # worktrees with FileNotFoundError on the /d/... path. See
    # `proj_runner_gen_events_drift_windows_bug` for the symptom history.
    python "$(to_native_path "$DISCR_SCRIPT")" "$(to_native_path "$SCHEMAS_JSON")"
fi

if [ "$DRY_RUN" = true ]; then
    echo "==> Dry run: schema output:"
    cat "$SCHEMAS_JSON"
    exit 0
fi

# ── TypeScript generation ──

if [ "$PY_ONLY" = false ]; then
    echo "==> Generating TypeScript types..."
    mkdir -p "$TS_OUT_DIR"

    # Earlier versions of this script ran `json2ts` per-type. That produced
    # stub cross-type references (`{ type, [k:string]: unknown }`) because
    # each schema was processed in isolation with its own inlined `$defs`,
    # and tsup's DTS bundler then disambiguated duplicates with `$1`/`$2`
    # suffixes when the stubs collided with the full declarations from
    # sibling files.
    #
    # The Node script below does a single pass over all schemas: it rewrites
    # internal `#/$defs/X` refs to file-relative `./X.schema.json` when `X`
    # is itself a top-level type, strips those entries from the local
    # `$defs`, runs `json-schema-to-typescript` with
    # `declareExternallyReferenced: false`, and injects proper
    # `import type { X } from './X'` statements. Result: one canonical
    # declaration per type, no stubs, no `$1` aliases after tsup bundling.
    COMPILE_SCRIPT="$SCHEMAS_DIR/scripts/compile_typescript.mjs"
    if [ ! -f "$COMPILE_SCRIPT" ]; then
        echo "ERROR: $COMPILE_SCRIPT not found"
        exit 1
    fi
    if ! command -v node &> /dev/null; then
        echo "WARNING: node not found. Skipping TypeScript generation."
    else
        # Clean stale .d.ts files so removed types don't linger after
        # regeneration. Mirrors the Python-side `rm -f "$PER_TYPE_DIR"/*.py`.
        rm -f "$TS_OUT_DIR"/*.d.ts "$TS_OUT_DIR/index.ts"

        node "$(to_native_path "$COMPILE_SCRIPT")" \
            --input "$(to_native_path "$SCHEMAS_JSON")" \
            --output "$(to_native_path "$TS_OUT_DIR")"

        echo "  TypeScript types written to $TS_OUT_DIR/"
    fi
fi

# ── Python generation ──

if [ "$TS_ONLY" = false ]; then
    echo "==> Generating Python (Pydantic) types..."
    mkdir -p "$PY_OUT_DIR"
    PER_TYPE_DIR="$PY_OUT_DIR/per_type"
    mkdir -p "$PER_TYPE_DIR"

    if ! command -v datamodel-codegen &> /dev/null; then
        echo "WARNING: datamodel-codegen not found. Skipping Python generation."
        echo "  Install: pip install datamodel-code-generator"
    else
        # Per-type generation. datamodel-codegen collapses shared `$defs`
        # into `RootModel[Any]` aliases when multiple top-level types share
        # nested definitions (e.g. a combined schema wrapping 51 types).
        # Running the generator once per type produces proper `BaseModel`
        # subclasses with real fields, discriminated-union `RootModel`s,
        # and `StrEnum` classes.
        TYPES=$(cargo run --bin export_schemas --release -- --list)
        SCHEMAS_JSON_NATIVE="$(to_native_path "$SCHEMAS_JSON")"
        SCHEMAS_JSON_JS="${SCHEMAS_JSON_NATIVE//\\/\\\\}"
        TMP_SCHEMA_DIR="$(mktemp -d)"
        trap 'rm -rf "$TMP_SCHEMA_DIR"' EXIT

        # Clean any stale per-type files so removed types don't linger.
        rm -f "$PER_TYPE_DIR"/*.py

        SUCCESS_TYPES=()
        FAILED_TYPES=()
        for TYPE_NAME in $TYPES; do
            SNAKE_NAME="$(to_snake_case "$TYPE_NAME")"
            TMP_SCHEMA_FILE="$TMP_SCHEMA_DIR/${TYPE_NAME}.json"
            OUT_FILE="$PER_TYPE_DIR/${SNAKE_NAME}.py"

            # Extract the schema for this type into a temp file (same
            # node-based pattern used for TS generation).
            node -e "
                const schemas = require('$SCHEMAS_JSON_JS');
                const schema = schemas['$TYPE_NAME'];
                if (!schema) process.exit(2);
                require('fs').writeFileSync(process.argv[1], JSON.stringify(schema));
            " "$(to_native_path "$TMP_SCHEMA_FILE")" || {
                echo "  WARNING: Failed to extract schema for ${TYPE_NAME}"
                FAILED_TYPES+=("$TYPE_NAME")
                continue
            }

            if datamodel-codegen \
                --input "$(to_native_path "$TMP_SCHEMA_FILE")" \
                --input-file-type jsonschema \
                --output "$(to_native_path "$OUT_FILE")" \
                --output-model-type pydantic_v2.BaseModel \
                --target-python-version 3.11 \
                --use-schema-description \
                --use-annotated \
                --use-union-operator \
                >/dev/null 2>&1; then
                SUCCESS_TYPES+=("$TYPE_NAME")
            else
                echo "  WARNING: Python generation failed for ${TYPE_NAME}"
                FAILED_TYPES+=("$TYPE_NAME")
            fi
        done

        # per_type/__init__.py — empty marker, just a pragma comment.
        echo "# Auto-generated by generate_types.sh — do not edit" \
            > "$PER_TYPE_DIR/__init__.py"

        # generated/__init__.py — re-export the top-level type from each
        # per-type module. Callers do `from qontinui_schemas.generated
        # import Constraint, ScheduledTask, ...`.
        INIT_FILE="$PY_OUT_DIR/__init__.py"
        {
            echo "# Auto-generated by generate_types.sh — do not edit"
            echo "# Re-exports the top-level type from each per-type module."
            echo ""
        } > "$INIT_FILE"
        EXPORT_NAMES=()
        for TYPE_NAME in "${SUCCESS_TYPES[@]}"; do
            SNAKE_NAME="$(to_snake_case "$TYPE_NAME")"
            echo "from .per_type.${SNAKE_NAME} import ${TYPE_NAME}" >> "$INIT_FILE"
            EXPORT_NAMES+=("\"$TYPE_NAME\"")
        done
        echo "" >> "$INIT_FILE"
        {
            echo "__all__ = ["
            for NAME in "${EXPORT_NAMES[@]}"; do
                echo "    $NAME,"
            done
            echo "]"
        } >> "$INIT_FILE"

        # Delete any stale monolithic models.py from older runs. Option A
        # uses per-type modules exclusively; leaving the old file around
        # would shadow the fresh exports.
        if [ -f "$PY_OUT_DIR/models.py" ]; then
            rm -f "$PY_OUT_DIR/models.py"
            echo "  Removed stale $PY_OUT_DIR/models.py"
        fi

        echo "  Python types written to $PY_OUT_DIR/per_type/ (${#SUCCESS_TYPES[@]} types)"
        if [ "${#FAILED_TYPES[@]}" -gt 0 ]; then
            echo "  WARNING: Failed types (${#FAILED_TYPES[@]}): ${FAILED_TYPES[*]}"
        fi
    fi
fi

echo "==> Done. Schema JSON: $SCHEMAS_JSON"
