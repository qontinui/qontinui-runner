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
# Atlas-owned schemas (both modes)
# --------------------------------
# alembic does NOT create the Atlas-owned tables: `atlas_managed` (and
# `orchestration`) are declared in atlas/schema.hcl and applied by Atlas, and
# no alembic revision creates or moves them. So after the alembic chain this
# script applies atlas/schema.hcl to the database it dumps
# (atlas/scripts/apply_to.sh), then dumps `atlas_managed` alongside the
# alembic schemas. The four `project.regression_*` tables alembic still
# creates (frozen revision f9d3e8a4c1b6) are legacy copies superseded by
# `atlas_managed` and are EXCLUDED from the dump, so the output carries all
# six Atlas-owned tables in `atlas_managed` and none in `project`.
#
# PREREQUISITE: docker (apply_to.sh runs Atlas and psql from pinned images),
# unless ATLAS_BIN names a native atlas binary and `psql` is on PATH.
# REGEN_SKIP_ATLAS=1 skips the apply, for a caller that has just applied it
# itself (the CI workflows do, as their own step). In DEFAULT mode (a
# long-lived database) the apply is opt-in via REGEN_APPLY_ATLAS=1, because it
# writes to shared state; without it the script only checks that all six
# atlas_managed tables are already there and refuses otherwise.
#
# Output: src-tauri/schema.pg.sql.generated.
#
# Determinism: pg_dump headers that contain timestamps, runtime info,
# and the pg_dump-build version are stripped post-dump so two consecutive
# runs produce byte-equal output regardless of pg_dump build origin
# (apt-installed Ubuntu vs docker-exec'd Debian — these embed different
# distro tags in the `-- Dumped by pg_dump version ...` header even at
# the same Postgres patch level). Stripping the Dumped-version lines is
# necessary for the CI gate, where the workflow uses Ubuntu's apt
# postgresql-client-16 but `pg_dump`s a Debian-based pgvector container.
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
#   REGEN_SKIP_ATLAS      (default: unset) 1 = do not apply atlas/schema.hcl
#                         (the DB already has it applied).
#   REGEN_APPLY_ATLAS     (default: unset) 1 = in default mode, apply
#                         atlas/schema.hcl to the long-lived DB before dumping.
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
SCHEMAS=(project coord agent auth cloud public atlas_managed)

# Default to relative path; can be overridden by env.
QONTINUI_WEB_DIR="${QONTINUI_WEB_DIR:-../../../qontinui-web/backend}"

cd "$(dirname "$0")/.."  # src-tauri/

# Shared Atlas helpers (side-effect free to source): the declared table list
# and the container networking apply_to.sh uses.
# shellcheck source=atlas/scripts/lib.sh
. ../atlas/scripts/lib.sh

# One source for credentials. PGPASSWORD is exported for pg_dump/psql and the
# helper containers. The user (and, where a URL must carry it, the password) is
# percent-encoded, so a character like @ : / # ? cannot break a URL; an IPv6
# host is bracketed.
PG_PASSWORD="${PGPASSWORD:-qontinui_dev_password}"
export PGPASSWORD="$PG_PASSWORD"
urlenc() {
    python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"
}
# pg_url <db> [nopass]. `nopass` leaves the password out of the URL, for a URL
# that travels in argv: every consumer of such a URL (psql, Atlas) reads the
# exported PGPASSWORD instead.
pg_url() {
    local host="$HOST" secret=""
    case "$host" in
        \[*) ;;
        *:*) host="[$host]" ;;
    esac
    if [[ "${2:-}" != "nopass" ]]; then
        secret=":$(urlenc "$PG_PASSWORD")"
    fi
    printf 'postgresql://%s%s@%s:%s/%s\n' "$(urlenc "$PG_USER")" "$secret" "$host" "$PORT" "$1"
}

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
# Legacy copies from frozen alembic history (revision f9d3e8a4c1b6),
# superseded by atlas_managed.<t>; their indexes and constraints go with them.
LEGACY_ATLAS_TABLES=(regression_suites regression_runs regression_diagnoses regression_assertion_executions)
for t in "${LEGACY_ATLAS_TABLES[@]}"; do
    PG_DUMP_ARGS+=(--exclude-table="project.$t")
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
        # The container publishes Postgres on $HOST:$PORT (default localhost:5433).
        DATABASE_URL="$(pg_url "$TEMP_DB")" python -m alembic upgrade head
    )
    DUMP_DB="$TEMP_DB"
else
    echo "[regen] Mode: default. Dumping $DB from $CONTAINER." >&2
    DUMP_DB="$DB"
fi

# ---------------------------------------------------------------------
# Atlas-owned schemas: apply atlas/schema.hcl to the DB being dumped.
# ---------------------------------------------------------------------
if [[ "${REGEN_SKIP_ATLAS:-}" == "1" ]]; then
    echo "[regen] REGEN_SKIP_ATLAS=1: assuming atlas/schema.hcl is already applied to $DUMP_DB." >&2
elif [[ "$mode" != "fresh-temp-db" && "${REGEN_APPLY_ATLAS:-}" != "1" ]]; then
    # Default mode dumps a LONG-LIVED database (the shared dev DB). Applying
    # Atlas there is a write to shared state, so it is opt-in: a runner that
    # booted against that DB has already moved the Atlas-owned tables into
    # atlas_managed via its self-heal. Refuse rather than dump a file that
    # silently lacks them.
    echo "[regen] Default mode: NOT applying atlas/schema.hcl to the long-lived $DUMP_DB (set REGEN_APPLY_ATLAS=1 to allow that write)." >&2
    # The expected set is whatever atlas/schema.hcl declares in atlas_managed.
    expected="$(atlas_declared_tables atlas_managed | sed 's/^atlas_managed\.//' | sort)"
    list_sql="SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'atlas_managed' AND c.relkind IN ('r','p') ORDER BY 1"
    if [[ -n "$CONTAINER" ]]; then
        present="$(docker exec "$CONTAINER" psql -U "$PG_USER" -d "$DUMP_DB" -tAc "$list_sql")" \
            || { echo "[regen] ERROR: could not read atlas_managed from $DUMP_DB." >&2; exit 1; }
    else
        present="$(psql -h "$HOST" -p "$PORT" -U "$PG_USER" -d "$DUMP_DB" -tAc "$list_sql")" \
            || { echo "[regen] ERROR: could not read atlas_managed from $DUMP_DB." >&2; exit 1; }
    fi
    missing="$(comm -23 <(printf '%s\n' "$expected") <(printf '%s\n' "$present" | sort) | tr '\n' ' ')"
    missing="${missing% }"
    if [[ -z "$expected" || -n "$missing" ]]; then
        echo "[regen] ERROR: $DUMP_DB lacks atlas_managed table(s) schema.hcl declares: ${missing:-<none parsed from schema.hcl>}. Boot a current runner against it, re-run with REGEN_APPLY_ATLAS=1, or use --fresh-temp-db." >&2
        exit 1
    fi
else
    echo "[regen] Applying atlas/schema.hcl to $DUMP_DB (Atlas owns atlas_managed)." >&2
    # With a named container, the helper containers join its network and reach
    # Postgres on its own port (works on Docker Desktop, which has no host
    # networking); without one they use the host network.
    ATLAS_PG_CONTAINER="$CONTAINER" bash ../atlas/scripts/apply_to.sh "$(pg_url "$DUMP_DB" nopass)"
fi

# ---------------------------------------------------------------------
# Header. Single quotes prevent shell expansion of the body.
# ---------------------------------------------------------------------
cat > "$TMP" <<'EOF'
-- GENERATED FILE — do not hand-edit.
-- Regenerate via: src-tauri/scripts/regenerate_schema_pg_sql.sh
--
-- Source: alembic head plus atlas/schema.hcl (Atlas owns atlas_managed),
-- dumped via pg_dump with
--   --schema=project --schema=coord --schema=agent --schema=auth
--   --schema=cloud --schema=public --schema=atlas_managed --no-owner
--   --no-privileges, excluding the legacy project.regression_* copies.
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
              -e '/^-- Dumped from database version /d' \
              -e '/^-- Dumped by pg_dump version /d' \
              -e '/^\\restrict /d' \
              -e '/^\\unrestrict /d' \
        >> "$TMP"
else
    pg_dump -h "$HOST" -p "$PORT" -U "$PG_USER" -d "$DUMP_DB" "${PG_DUMP_ARGS[@]}" \
        | sed -e '/^-- Started on /d' \
              -e '/^-- Completed on /d' \
              -e '/^-- Dumped from database version /d' \
              -e '/^-- Dumped by pg_dump version /d' \
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
