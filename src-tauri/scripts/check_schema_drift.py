#!/usr/bin/env python3
"""
Schema drift detection: ensures schema.pg.sql stays in sync with
ensure_tables() DDL (pg/mod.rs), SQLite schema.sql, and migrations.rs.

Exit code 0 = PASS, 1 = FAIL (tables in SQLite but not PG).
"""

import os
import re
import subprocess
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
SRC_TAURI = os.path.dirname(SCRIPT_DIR)
REPO_ROOT = os.path.dirname(SRC_TAURI)

PG_SCHEMA   = os.path.join(SRC_TAURI, "schema.pg.sql")
SQLITE_SCHEMA = os.path.join(SRC_TAURI, "src", "database", "schema.sql")
MIGRATIONS  = os.path.join(SRC_TAURI, "src", "database", "migrations.rs")
PG_MOD      = os.path.join(SRC_TAURI, "src", "database", "pg", "mod.rs")

CREATE_RE = re.compile(r"CREATE\s+TABLE\s+IF\s+NOT\s+EXISTS\s+(\w+)", re.IGNORECASE)

# Tables created only as temporaries during migrations (renamed/dropped later)
MIGRATION_TEMP_TABLES = {"checks_new", "verification_tests_new", "workflow_step_checkpoints_new", "run_details"}


def _read_file(filepath):
    """Read a file from disk, falling back to `git show HEAD:<path>` if missing."""
    if os.path.isfile(filepath):
        with open(filepath, "r", encoding="utf-8") as f:
            return f.read()
    # Try git
    rel = os.path.relpath(filepath, REPO_ROOT).replace("\\", "/")
    try:
        result = subprocess.run(
            ["git", "show", f"HEAD:{rel}"],
            capture_output=True, text=True, cwd=REPO_ROOT,
        )
        if result.returncode == 0:
            return result.stdout
    except FileNotFoundError:
        pass
    raise FileNotFoundError(f"Cannot read {filepath} (not on disk or in git HEAD)")


def extract_tables(filepath):
    """Return set of table names from CREATE TABLE IF NOT EXISTS statements."""
    content = _read_file(filepath)
    return set(CREATE_RE.findall(content))


def extract_columns(filepath, table_name):
    """Extract column names for a given table from a SQL/Rust file.

    Handles both plain SQL and Rust string literals (with leading quotes/backslashes).
    Returns a sorted list of column names, or None if the table is not found.
    """
    content = _read_file(filepath)

    # Find the CREATE TABLE block
    pattern = re.compile(
        r"CREATE\s+TABLE\s+IF\s+NOT\s+EXISTS\s+" + re.escape(table_name) + r"\s*\(",
        re.IGNORECASE,
    )
    match = pattern.search(content)
    if not match:
        return None

    # Walk forward to find matching closing paren
    start = match.end()
    depth = 1
    pos = start
    while pos < len(content) and depth > 0:
        ch = content[pos]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        pos += 1

    block = content[start : pos - 1]

    # Extract column names: first word on lines that look like column defs
    columns = set()
    col_re = re.compile(r"^\s*\"?\s*(\w+)\s+(?:TEXT|INTEGER|BIGINT|BOOLEAN|REAL|BLOB|BYTEA|TIMESTAMPTZ|TIMESTAMP|SERIAL|VARCHAR|JSONB|JSON|DOUBLE|FLOAT|NUMERIC)", re.IGNORECASE | re.MULTILINE)
    for m in col_re.finditer(block):
        name = m.group(1).lower()
        # Skip SQL keywords that might appear
        if name.upper() not in ("CREATE", "TABLE", "IF", "NOT", "EXISTS", "PRIMARY", "UNIQUE", "CHECK", "CONSTRAINT", "FOREIGN"):
            columns.add(name)

    return sorted(columns) if columns else None


def main():
    exit_code = 0

    # --- Extract table sets ---
    pg_tables = extract_tables(PG_SCHEMA)
    try:
        sqlite_tables = extract_tables(SQLITE_SCHEMA)
    except FileNotFoundError:
        sqlite_tables = set()  # SQLite schema removed
    try:
        migration_tables = extract_tables(MIGRATIONS) - MIGRATION_TEMP_TABLES
    except FileNotFoundError:
        migration_tables = set()  # migrations.rs removed
    ensure_tables = extract_tables(PG_MOD)

    sqlite_all = sqlite_tables | migration_tables

    print("SCHEMA DRIFT CHECK")
    print("==================")
    print()

    # --- Tables in SQLite but not PG ---
    sqlite_only = sorted(sqlite_all - pg_tables)
    if sqlite_only:
        print(f"Tables in SQLite but not PG: {sqlite_only} -- ERROR")
        exit_code = 1
    else:
        print("Tables in SQLite but not PG: [] -- OK")

    # --- Tables in ensure_tables() but not schema.pg.sql ---
    ensure_only = sorted(ensure_tables - pg_tables)
    if ensure_only:
        print(f"Tables in ensure_tables but not schema.pg.sql: {ensure_only} -- WARNING")
    else:
        print("Tables in ensure_tables but not schema.pg.sql: [] -- OK")

    # --- Tables in schema.pg.sql but not ensure_tables() (informational) ---
    pg_only_vs_ensure = sorted(pg_tables - ensure_tables)
    if pg_only_vs_ensure:
        print(f"Tables in schema.pg.sql but not ensure_tables: [{len(pg_only_vs_ensure)} tables] -- INFO (expected)")
    else:
        print("Tables in schema.pg.sql but not ensure_tables: [] -- INFO")

    # --- Tables in PG but not SQLite (informational) ---
    pg_only_vs_sqlite = sorted(pg_tables - sqlite_all)
    if pg_only_vs_sqlite:
        print(f"Tables in PG but not SQLite: {pg_only_vs_sqlite} -- INFO")

    # --- Column parity checks ---
    print()
    print("COLUMN PARITY (key tables)")
    print("--------------------------")

    key_tables = ["task_runs", "unified_workflows", "settings"]
    col_issues = False

    for tbl in key_tables:
        pg_cols = extract_columns(PG_SCHEMA, tbl)
        try:
            sqlite_cols_base = extract_columns(SQLITE_SCHEMA, tbl)
        except FileNotFoundError:
            sqlite_cols_base = None
        try:
            sqlite_cols_mig = extract_columns(MIGRATIONS, tbl)
        except FileNotFoundError:
            sqlite_cols_mig = None

        # Merge SQLite base + migration columns
        if sqlite_cols_base and sqlite_cols_mig:
            sqlite_cols = sorted(set(sqlite_cols_base) | set(sqlite_cols_mig))
        else:
            sqlite_cols = sqlite_cols_base or sqlite_cols_mig

        if pg_cols is None:
            print(f"  {tbl}: not found in schema.pg.sql -- WARNING")
            col_issues = True
            continue
        if sqlite_cols is None:
            print(f"  {tbl}: not found in SQLite schemas -- WARNING")
            col_issues = True
            continue

        pg_set = set(pg_cols)
        sq_set = set(sqlite_cols)

        in_pg_not_sqlite = sorted(pg_set - sq_set)
        in_sqlite_not_pg = sorted(sq_set - pg_set)

        if in_pg_not_sqlite or in_sqlite_not_pg:
            col_issues = True
            if in_sqlite_not_pg:
                print(f"  {tbl}: in SQLite but not PG: {in_sqlite_not_pg} -- WARNING")
            if in_pg_not_sqlite:
                print(f"  {tbl}: in PG but not SQLite: {in_pg_not_sqlite} -- WARNING")
        else:
            print(f"  {tbl}: columns match ({len(pg_set)} cols) -- OK")

    # --- Summary ---
    print()
    print("==================")

    # Counts
    print(f"PG tables: {len(pg_tables)}, SQLite tables: {len(sqlite_all)}, ensure_tables: {len(ensure_tables)}")

    if exit_code != 0:
        print("Result: FAIL")
    elif col_issues or ensure_only:
        print("Result: PASS (with warnings)")
    else:
        print("Result: PASS")

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
