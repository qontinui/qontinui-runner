// Atlas project config — runner schema-half pilot (Row 3, Wave 1).
//
// Usage:
//   docker run --rm --network host -v "${PWD}/atlas:/atlas" -w /atlas \
//     arigaio/atlas:latest schema diff --env runner_pilot
//
// The --include filter would be the natural way to scope Atlas to just the
// runner-Rust-managed tables, but `--include` is Atlas Pro-only. Community
// edition is restricted to whole-schema management, so we use a long
// `--exclude` list covering every other table/sequence/view/MV/enum in the
// `project` and `coord` schemas (alembic-owned, not Atlas-owned).
//
// The exclude list is regenerated from the live canonical PG via
// `scripts/regen_exclude.ps1`. Run that whenever alembic adds a new
// non-Atlas-owned table — otherwise the next `schema apply` will try to
// drop it.

variable "live_url" {
  type    = string
  default = getenv("ATLAS_LIVE_URL")
}

variable "dev_url" {
  type    = string
  default = getenv("ATLAS_DEV_URL")
}

// `schemas` must list every schema schema.hcl declares a table in, or those
// declarations are inert -- Atlas never inspects the schema, so it neither
// creates nor reconciles them. `orchestration` was missing while schema.hcl
// declared `orchestration.runs` and `orchestration.subtasks`, so the Approach-D
// conductor ledger was declaratively described and never actually managed.
//
// It must equally match `$PilotSchemas` in scripts/regen_exclude.ps1: a schema
// in scope here but not scanned there contributes no exclusions, which makes
// every non-pilot object in it a DROP candidate. scripts/check_pilot_consistency.ps1
// asserts both directions on every CI run.
env "runner_pilot" {
  src     = "file://schema.hcl"
  url     = var.live_url
  dev     = var.dev_url
  schemas = ["project", "coord", "orchestration"]
  exclude = split("\n", trimspace(file("exclude.txt")))
}
