#!/bin/bash
# Regenerate schema.pg.sql.generated from the canonical alembic-managed schema.
#
# Phase 5 deliverable of the migration consolidation
# (D:/qontinui-root/tmp_migration_consolidation_plan.md §"Phase 5").
#
# Purpose: produce the source-of-truth file that Clorinde reads to
# validate queries/*.sql against, sourced from a real Postgres dump
# rather than the hand-authored schema.pg.sql. Once Phase 4 of the
# consolidation plan deletes the hand-authored schema.pg.sql, this
# generated file takes over.
#
# Modes
# -----
# Default (no flag): dump from a running canonical Postgres container.
#   Expects the qontinui-canonical-postgres container to be up with
#   schemas project/coord/agent/auth/cloud/public created and
#   populated by the alembic chain (which post-transplant includes
#   the consolidation revisions formerly in _staged_consolidation/).
#
# --fresh-temp-db: create a throwaway database inside the same container,
#   apply alembic upgrade head into it from qontinui-web/backend, dump
#   the temp DB, drop it. This is the post-transplant CI mode — it
#   guarantees the dump reflects the ALEMBIC-AUTHORITATIVE state,
#   independent of any drift in the long-lived dev DB.
#
# Output: src-tauri/schema.pg.sql.generated.
#
# Determinism: pg_dump headers that contain timestamps or runtime info
# (Started on / Completed on) are stripped post-dump so two consecutive
# runs produce byte-equal output. The pg_dump version line is kept (it
# changes only when the container is rebuilt with a newer Postgres,
# which is a reviewable event).
#
# Environment overrides:
#   CLORINDE_PG_CONTAINER (default: qontinui-canonical-postgres)
#                         If set to empty string, native pg_dump is used.
#   CLORINDE_PG_HOST      (default: localhost)
#   CLORINDE_PG_PORT      (default: 5433)
#   CLORINDE_PG_DB        (default: qontinui_db)
#   CLORINDE_PG_USER      (default: qontinui_user)
#   PGPASSWORD            (read by native pg_dump; ignored in docker-exec mode)
#   QONTINUI_WEB_DIR      (default: ../../../qontinui-web/backend)
#                         Used by --fresh-temp-db to find alembic.
#
# Container vs native pg_dump:
# By default the script uses `docker exec <container> pg_dump` so a host
# without pg_dump installed can still run it. If CLORINDE_PG_CONTAINER
# is set to the empty string, the script falls back to host `pg_dump`
# (the CI workflow uses this path with a postgres-client install).

set -euo pipefail

CONTAINER="${CLORINDE_PG_CONTAINER-qontinui-canonical-postgres}"
HOST="${CLORINDE_PG_HOST:-localhost}"
PORT="${CLORINDE_PG_PORT:-5433}"
DB="${CLORINDE_PG_DB:-qontinui_db}"
PG_USER="${CLORINDE_PG_USER:-qontinui_user}"
SCHEMAS=(project coord agent auth cloud public)

# Default to relative path; can be overridden by env.
QONTINUI_WEB_DIR="${QONTINUI_WEB_DIR:-../../../qontinui-web/backend}"

cd "$(dirname "$0")/.."  # src-tauri/

OUTPUT="schema.pg.sql.generated"
TMP="${OUTPUT}.tmp"

mode="default"
if [[ "${1:-}" == "--fresh-temp-db" ]]; then
    mode="fresh-temp-db"
fi

# ---------------------------------------------------------------------
# Build the pg_dump arg list (shared between modes).
# ---------------------------------------------------------------------
PG_DUMP_ARGS=(--schema-only --no-owner --no-privileges)
for s in "${SCHEMAS[@]}"; do
    PG_DUMP_ARGS+=(--schema="$s")
done

# ---------------------------------------------------------------------
# Source DB selection (default vs --fresh-temp-db).
# ---------------------------------------------------------------------
if [[ "$mode" == "fresh-temp-db" ]]; then
    echo "[regen] Mode: fresh-temp-db. Creating temp DB inside $CONTAINER." >&2
    TEMP_DB="clorinde_regen_$(date +%s)_$$"
    cleanup_temp_db() {
        echo "[regen] Dropping temp DB $TEMP_DB" >&2
        docker exec "$CONTAINER" psql -U "$PG_USER" -d "$DB" \
            -c "DROP DATABASE IF EXISTS $TEMP_DB" >/dev/null 2>&1 || true
    }
    trap cleanup_temp_db EXIT

    docker exec "$CONTAINER" psql -U "$PG_USER" -d "$DB" \
        -c "CREATE DATABASE $TEMP_DB" >/dev/null

    # Apply alembic upgrade head against the temp DB. This requires the
    # alembic chain in qontinui-web (post-transplant, that includes the
    # consolidation revisions).
    if [[ ! -d "$QONTINUI_WEB_DIR" ]]; then
        echo "[regen] ERROR: QONTINUI_WEB_DIR not found at $QONTINUI_WEB_DIR" >&2
        echo "        Set QONTINUI_WEB_DIR env var to qontinui-web/backend path." >&2
        exit 2
    fi
    (
        cd "$QONTINUI_WEB_DIR"
        # Container is on host network port 5433; assume that.
        DATABASE_URL="postgresql://${PG_USER}:qontinui_dev_password@localhost:5433/${TEMP_DB}" \
            python -m alembic upgrade head
    )
    DUMP_DB="$TEMP_DB"
else
    echo "[regen] Mode: default. Dumping $DB from $CONTAINER." >&2
    DUMP_DB="$DB"
fi

# ---------------------------------------------------------------------
# Header. Single quotes prevent shell expansion of the body.
# ---------------------------------------------------------------------
cat > "$TMP" <<'EOF'
-- GENERATED FILE — do not hand-edit.
-- Regenerate via: src-tauri/scripts/regenerate_schema_pg_sql.sh
--
-- Source: alembic-managed schema, dumped via pg_dump with
--   --schema=project --schema=coord --schema=agent --schema=auth
--   --schema=cloud --schema=public --no-owner --no-privileges.
--
-- Consumers: Clorinde (validates queries/*.sql against this file).
--
-- Determinism: timestamp/runtime-context lines are stripped so two
-- consecutive runs produce byte-equal output. CI gate fails if
-- `git diff schema.pg.sql.generated` is non-empty after a fresh run.

EOF

# ---------------------------------------------------------------------
# pg_dump → strip non-deterministic lines → append.
# ---------------------------------------------------------------------
# Pick docker-exec vs native pg_dump based on whether CLORINDE_PG_CONTAINER
# is set to a non-empty value.
#
# Filter notes:
# - `\restrict` / `\unrestrict` are pg_dump session tokens (PG 17+) used to
#   scope restoration privileges. The token is regenerated on every run,
#   so stripping them is required for byte-stable output. They're psql-meta
#   commands, not DDL, so Clorinde doesn't need them.
# - `-- Started on` / `-- Completed on` are pg_dump runtime timestamps;
#   not present in every PG version but defensively stripped.
if [[ -n "$CONTAINER" ]]; then
    docker exec "$CONTAINER" pg_dump -U "$PG_USER" -d "$DUMP_DB" "${PG_DUMP_ARGS[@]}" \
        | sed -e '/^-- Started on /d' \
              -e '/^-- Completed on /d' \
              -e '/^\\restrict /d' \
              -e '/^\\unrestrict /d' \
        >> "$TMP"
else
    pg_dump -h "$HOST" -p "$PORT" -U "$PG_USER" -d "$DUMP_DB" "${PG_DUMP_ARGS[@]}" \
        | sed -e '/^-- Started on /d' \
              -e '/^-- Completed on /d' \
              -e '/^\\restrict /d' \
              -e '/^\\unrestrict /d' \
        >> "$TMP"
fi

# pg_dump finishes with two trailing blank lines after "PostgreSQL
# database dump complete". The repo's end-of-file-fixer pre-commit hook
# strips trailing blank lines, leaving exactly one final newline. To
# stay byte-stable across `regenerate` + `git commit` cycles, normalize
# here: strip trailing blank lines, ensure exactly one final newline.
python3 -c '
import sys
data = open(sys.argv[1], "rb").read()
data = data.rstrip(b"\n") + b"\n"
open(sys.argv[1], "wb").write(data)
' "$TMP"

mv "$TMP" "$OUTPUT"
echo "[regen] Wrote: $(pwd)/$OUTPUT ($(wc -l < "$OUTPUT") lines)" >&2
