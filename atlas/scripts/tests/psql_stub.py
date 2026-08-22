"""Stub `psql` for exercising regen_exclude.ps1 without a database.

Mode is taken from the PSQL_STUB_MODE env var:
  ok      -- return a small, realistic object set
  fail    -- exit 1 (simulates DB unreachable / bad auth)
  empty   -- return zero rows for every query (silent zero-exit failure)
"""
import os
import re
import sys

mode = os.environ.get("PSQL_STUB_MODE", "ok")

if mode == "fail":
    sys.stderr.write("psql: error: connection to server failed\n")
    sys.exit(1)

# The SQL is the argument after -c
argv = sys.argv[1:]
sql = ""
if "-c" in argv:
    sql = argv[argv.index("-c") + 1]

if mode == "empty":
    sys.exit(0)

is_enum = "typtype" in sql

# Whichever schema the query names.
schema = None
for s in ("project", "coord", "orchestration"):
    if "'%s'" % s in sql:
        schema = s
        break

# Relations that exist in the fake DB, per schema. Includes the runner-authored
# pilot tables (spec_proposals, proposal_events, orchestration.*) so this
# reproduces the LOCAL canonical-PG shape -- the one the old hardcoded carve-out
# mishandled -- rather than CI's alembic-only shape.
RELATIONS = {
    "project": [
        "alembic_version",
        "apps",
        "coordinator_shadow_decisions",
        "proposal_events",
        "regression_assertion_executions",
        "regression_diagnoses",
        "regression_runs",
        "regression_suites",
        "spec_proposals",
        "workflows",
    ],
    "coord": [
        "agents",
        "gates",
    ],
    "orchestration": [
        "runs",
        "subtasks",
    ],
}
ENUMS = {
    "project": ["workflow_status"],
    "coord": ["gate_state"],
    "orchestration": [],
}

rows = list((ENUMS if is_enum else RELATIONS).get(schema, []))

# Honour the two SQL-side name predicates the script has historically used, so
# a comparison between script versions reflects the script and not the stub:
#   AND relname NOT LIKE 'regression_%'
#   AND relname <> 'coordinator_shadow_decisions'
for pat in re.findall(r"relname\s+NOT\s+LIKE\s+'([^']*)'", sql, re.I):
    # SQL LIKE: % -> .*  (underscore is left literal; every real pattern here
    # uses it as a literal separator, e.g. 'regression_%').
    rx = re.compile("^" + ".*".join(re.escape(p) for p in pat.split("%")) + "$")
    rows = [r for r in rows if not rx.match(r)]
for name in re.findall(r"relname\s*<>\s*'([^']*)'", sql, re.I):
    rows = [r for r in rows if r != name]

for r in sorted(rows):
    sys.stdout.write(r + "\n")
sys.exit(0)
