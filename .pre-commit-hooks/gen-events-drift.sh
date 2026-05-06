#!/usr/bin/env bash
#
# gen-events-drift.sh — pre-commit drift guard for Tauri event TS bindings.
#
# Runs the schemars→TypeScript pipeline that produces the typed bindings
# under `qontinui-schemas/ts/src/{generated,tauri-events}` and the bundled
# `dist/tauri-events.*`. If the regenerated output differs from what's
# checked in, the commit is aborted with instructions to run
# `npm run gen-events` from `qontinui-runner/`.
#
# Triggered by the pre-commit hook in `.pre-commit-config.yaml` whenever
# one of the canonical Rust source files changes (terminal payloads,
# AppEvent, runner-local Finding, schema_export registry).
#
# This guards against the silent class of bugs fixed in commits
# ed1b45d56 / 16e6fb4ff / b0cefef89: Rust serde renames or shape changes
# (e.g. `terminal_id` → camelCase `terminalId`, flat → tagged
# `{event_type, data}` envelope) that previously slipped past CI because
# the TS interfaces were hand-written. Now they break the runner build.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SCHEMAS_DIR="$(cd "$RUNNER_DIR/../qontinui-schemas" && pwd)"

cd "$RUNNER_DIR"

echo "[gen-events-drift] regenerating Rust JSON Schemas + per-type TS bindings..."
# Run only the source-of-truth steps (schemars export + per-type d.ts
# emission). Skip the tsup bundle build because tsup's chunk filenames
# aren't deterministic across runs (e.g. `Region.d-XXXXXXXX.d.cts`),
# which would cause spurious failures here.
if ! bash src-tauri/scripts/generate_types.sh --ts-only >/dev/null 2>&1; then
    echo "[gen-events-drift] ERROR: generate_types.sh failed. Re-run it manually to see the error."
    exit 1
fi

# Drift only matters for the per-type generated definitions and the
# tauri-events re-export module. The bundled `ts/dist/tauri-events.*`
# files are derived; agents regenerate them with `npm run gen-events`
# before committing API changes.
DRIFT_PATHS=(
    ts/src/generated
    ts/src/tauri-events
)

if ! git -C "$SCHEMAS_DIR" diff --quiet -- "${DRIFT_PATHS[@]}" 2>/dev/null; then
    echo "[gen-events-drift] ERROR: Generated Tauri event bindings are stale."
    echo "[gen-events-drift] Run \`npm run gen-events\` from qontinui-runner/ and commit the result."
    git -C "$SCHEMAS_DIR" --no-pager diff --stat -- "${DRIFT_PATHS[@]}" 2>/dev/null || true
    exit 1
fi

echo "[gen-events-drift] OK — generated bindings match checked-in output."
