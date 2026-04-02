#!/bin/bash
# Schema drift checker: ensures schema.pg.sql stays in sync with
# ensure_tables() DDL, SQLite schema.sql, and migrations.rs.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 scripts/check_schema_drift.py "$@"
