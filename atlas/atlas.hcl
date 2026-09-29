// Atlas project config — the runner's Atlas-owned schemas.
//
// Usage:
//   docker run --rm --network host -v "${PWD}/atlas:/work" -w /work \
//     -e ATLAS_LIVE_URL -e ATLAS_DEV_URL \
//     arigaio/atlas:1.3.3-community schema apply --env runner_pilot --dry-run
//
// Atlas owns `atlas_managed` and `orchestration` WHOLLY: every object in them
// is declared in schema.hcl, and nothing else lives there. Every other schema
// (`project`, `coord`, `agent`, `auth`, ...) is alembic-owned and simply NOT
// listed below. A schema outside `schemas` is invisible to `schema diff` and
// `schema apply`, so none of its objects can become a DROP candidate -- which
// is why there is no exclude list (plan `2026-05-14-atlas-wave-6-triage`).

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
// creates nor reconciles them. It must list NOTHING else: a schema in scope
// that also holds objects another system authors turns each of those objects
// into a DROP candidate. .github/workflows/atlas-schema-check.yml asserts both
// against a Postgres with the alembic chain applied.
env "runner_pilot" {
  src     = "file://schema.hcl"
  url     = var.live_url
  dev     = var.dev_url
  schemas = ["atlas_managed", "orchestration"]
}
