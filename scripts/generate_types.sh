#!/usr/bin/env bash
#
# Cross-Language Type Generation from Rust JSON Schemas
#
# Exports JSON Schemas from Rust types and generates TypeScript (and optionally
# Python/Pydantic) type definitions.
#
# Usage:
#   ./scripts/generate_types.sh              # Generate TypeScript types
#   ./scripts/generate_types.sh --python     # Also generate Python types
#   ./scripts/generate_types.sh --check      # Export schemas only (dry run)
#
# Prerequisites:
#   - Rust toolchain (cargo)
#   - Node.js + npx (for json-schema-to-typescript)
#   - Optional: pip install datamodel-code-generator (for Python types)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC_TAURI="$PROJECT_ROOT/src-tauri"
SCHEMA_OUTPUT="$PROJECT_ROOT/src/types/generated"
SCHEMA_JSON="$SRC_TAURI/target/schemas.json"
PYTHON_OUTPUT="$PROJECT_ROOT/qontinui-schemas/src/qontinui_schemas/generated"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[info]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; }
error() { echo -e "${RED}[error]${NC} $1"; }

# --------------------------------------------------------------------------
# Step 1: Build and run the schema export binary
# --------------------------------------------------------------------------
info "Building export_schemas binary..."
(cd "$SRC_TAURI" && cargo build --bin export_schemas 2>&1) || {
    error "Failed to build export_schemas. Run 'cargo check --lib' in src-tauri/ for details."
    exit 1
}

info "Exporting JSON Schemas..."
(cd "$SRC_TAURI" && cargo run --bin export_schemas -- --pretty) > "$SCHEMA_JSON" || {
    error "Failed to run export_schemas binary."
    exit 1
}

SCHEMA_COUNT=$(python3 -c "import json; print(len(json.load(open('$SCHEMA_JSON'))))" 2>/dev/null || echo "?")
info "Exported $SCHEMA_COUNT schemas to $SCHEMA_JSON"

# If --check mode, stop here
if [[ "${1:-}" == "--check" ]]; then
    info "Check mode: schemas exported successfully. No types generated."
    cat "$SCHEMA_JSON"
    exit 0
fi

# --------------------------------------------------------------------------
# Step 2: Generate TypeScript types
# --------------------------------------------------------------------------
info "Generating TypeScript types..."

# Check for json-schema-to-typescript
if ! npx --yes json-schema-to-typescript --help > /dev/null 2>&1; then
    warn "json-schema-to-typescript not found. Install with:"
    warn "  npm install -g json-schema-to-typescript"
    warn "  # or it will be auto-installed via npx"
fi

# Create output directory
mkdir -p "$SCHEMA_OUTPUT"

# Generate TypeScript for each schema
# We split the single JSON object into per-type schema files, then convert each
python3 -c "
import json, sys, os

schemas = json.load(open('$SCHEMA_JSON'))
out_dir = '$SCHEMA_OUTPUT'
os.makedirs(out_dir, exist_ok=True)

# Write individual schema files for json-schema-to-typescript
for name, schema in schemas.items():
    schema_file = os.path.join(out_dir, f'{name}.schema.json')
    with open(schema_file, 'w') as f:
        json.dump(schema, f, indent=2)
    print(f'  Wrote {schema_file}')
" || {
    error "Failed to split schemas (requires python3)."
    exit 1
}

# Convert each schema to TypeScript
TS_INDEX="$SCHEMA_OUTPUT/index.ts"
echo "// Auto-generated from Rust JSON Schemas — do not edit manually" > "$TS_INDEX"
echo "// Generated at: $(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$TS_INDEX"
echo "" >> "$TS_INDEX"

GENERATED=0
for schema_file in "$SCHEMA_OUTPUT"/*.schema.json; do
    type_name=$(basename "$schema_file" .schema.json)
    ts_file="$SCHEMA_OUTPUT/${type_name}.d.ts"

    if npx --yes json-schema-to-typescript "$schema_file" --output "$ts_file" 2>/dev/null; then
        echo "export type { ${type_name} } from './${type_name}';" >> "$TS_INDEX"
        info "  Generated $ts_file"
        GENERATED=$((GENERATED + 1))
    else
        warn "  Failed to generate TypeScript for $type_name (non-fatal)"
    fi
done

if [ "$GENERATED" -eq 0 ]; then
    warn "No TypeScript types generated. Ensure npx and json-schema-to-typescript are available."
else
    info "Generated $GENERATED TypeScript type files in $SCHEMA_OUTPUT/"
fi

# --------------------------------------------------------------------------
# Step 3: Optionally generate Python/Pydantic types
# --------------------------------------------------------------------------
if [[ "${1:-}" == "--python" ]]; then
    info "Generating Python/Pydantic types..."

    if ! command -v datamodel-codegen > /dev/null 2>&1; then
        warn "datamodel-code-generator not found. Install with:"
        warn "  pip install datamodel-code-generator"
        warn "Skipping Python type generation."
    else
        mkdir -p "$PYTHON_OUTPUT"

        for schema_file in "$SCHEMA_OUTPUT"/*.schema.json; do
            type_name=$(basename "$schema_file" .schema.json)
            py_file="$PYTHON_OUTPUT/${type_name}.py"

            if datamodel-codegen \
                --input "$schema_file" \
                --output "$py_file" \
                --output-model-type pydantic_v2.BaseModel \
                --target-python-version 3.11 2>/dev/null; then
                info "  Generated $py_file"
            else
                warn "  Failed to generate Python for $type_name (non-fatal)"
            fi
        done

        # Generate __init__.py
        echo "# Auto-generated from Rust JSON Schemas" > "$PYTHON_OUTPUT/__init__.py"
        for py_file in "$PYTHON_OUTPUT"/*.py; do
            module=$(basename "$py_file" .py)
            if [ "$module" != "__init__" ]; then
                echo "from .${module} import *" >> "$PYTHON_OUTPUT/__init__.py"
            fi
        done
        info "Generated Python types in $PYTHON_OUTPUT/"
    fi
fi

# --------------------------------------------------------------------------
# Done
# --------------------------------------------------------------------------
info "Type generation complete."
info "Schemas: $SCHEMA_JSON"
info "TypeScript: $SCHEMA_OUTPUT/"
