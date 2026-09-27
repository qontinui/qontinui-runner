# shellcheck shell=bash
# Shared helpers for the Atlas scripts (check_schema.sh, apply_to.sh). Source
# it; do not run it. Callers run under `set -euo pipefail`.
#
# Tooling, resolved so a laptop and CI need nothing but docker:
#   atlas  -- ATLAS_BIN (a native binary) when set, else the pinned Community
#             image ATLAS_IMAGE via `docker run`.
#   psql   -- `psql` from the postgres:16 image via `docker run`, unless a
#             native `psql` is on PATH and the target is not addressed through
#             a container network (ATLAS_PG_CONTAINER unset).
#
# How a container reaches the target Postgres (atlas_network):
#   ATLAS_DOCKER_NETWORK   explicit `--network` value; wins over the rest.
#   ATLAS_PG_CONTAINER     the target runs in this named container: join its
#                          network namespace (`--network container:<name>`)
#                          and address it at localhost:ATLAS_PG_CONTAINER_PORT
#                          (default 5432), whatever port the host published.
#                          Works the same on Linux and Docker Desktop.
#   otherwise, Linux       `--network host`; URLs are used as given.
#   otherwise (macOS/Win)  the default bridge network, with localhost /
#                          127.0.0.1 in the URL rewritten to
#                          host.docker.internal (Docker Desktop has no usable
#                          host networking).
# Callers pass the URL as the HOST sees it; atlas_container_url rewrites it.
# PGPASSWORD, when set, is forwarded to the containers, so a URL need not
# carry the password.
#
# The dev database. Atlas normalises the HCL in a scratch "dev" database, and
# that database must have NO `public` schema, or `schema apply` aborts its
# post-apply verification after an otherwise clean plan (spike Q1-1, plan
# 2026-05-14-atlas-wave-6-triage). atlas_provision_dev_db creates a throwaway
# one on the target's server and drops it again on exit.
#
# Sourcing this file has no side effects; a script that provisions a dev
# database or writes plan files calls atlas_init first.

ATLAS_IMAGE="${ATLAS_IMAGE:-arigaio/atlas:1.3.3-community}"
ATLAS_PSQL_IMAGE="${ATLAS_PSQL_IMAGE:-postgres:16}"
atlas_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
atlas_work=""
atlas_dev_db_name=""
atlas_dev_admin_url=""

atlas_init() {
  atlas_work="$(mktemp -d)"
  trap atlas_cleanup EXIT
}

atlas_cleanup() {
  if [ -n "$atlas_dev_db_name" ]; then
    atlas_psql "$atlas_dev_admin_url" -qc "DROP DATABASE IF EXISTS \"$atlas_dev_db_name\"" \
      >/dev/null 2>&1 || echo "warning: could not drop dev database $atlas_dev_db_name" >&2
  fi
  if [ -n "$atlas_work" ]; then rm -rf "$atlas_work"; fi
}

atlas_fail() {
  echo "::error::$*"
  exit 1
}

# The `--network` value for helper containers; empty = docker's default bridge.
atlas_network() {
  if [ -n "${ATLAS_DOCKER_NETWORK:-}" ]; then
    printf '%s\n' "$ATLAS_DOCKER_NETWORK"
  elif [ -n "${ATLAS_PG_CONTAINER:-}" ]; then
    printf 'container:%s\n' "$ATLAS_PG_CONTAINER"
  elif [ "$(uname -s)" = "Linux" ]; then
    printf 'host\n'
  else
    printf '\n'
  fi
}

# Rewrite a host-side postgres URL ($1) into the address a helper container on
# atlas_network sees. Anything that is not a postgres URL passes through.
atlas_container_url() {
  local url="$1" net re scheme userinfo host port rest
  re='^(postgres(ql)?://)([^@/]*@)?([^/:?]+)(:[0-9]+)?([/?].*)?$'
  if ! [[ "$url" =~ $re ]]; then
    printf '%s\n' "$url"
    return
  fi
  scheme="${BASH_REMATCH[1]}"
  userinfo="${BASH_REMATCH[3]}"
  host="${BASH_REMATCH[4]}"
  port="${BASH_REMATCH[5]}"
  rest="${BASH_REMATCH[6]}"
  net="$(atlas_network)"
  case "$net" in
    container:*)
      host="localhost"
      port=":${ATLAS_PG_CONTAINER_PORT:-5432}"
      ;;
    "")
      case "$host" in localhost | 127.0.0.1) host="host.docker.internal" ;; esac
      ;;
  esac
  printf '%s%s%s%s%s\n' "$scheme" "$userinfo" "$host" "$port" "$rest"
}

# docker run with the network and PGPASSWORD forwarding every helper needs.
atlas_docker_run() {
  local net args=(run --rm)
  net="$(atlas_network)"
  if [ -n "$net" ]; then args+=(--network "$net"); fi
  if [ -n "${PGPASSWORD:-}" ]; then args+=(-e PGPASSWORD); fi
  docker "${args[@]}" "$@"
}

atlas_run() {
  if [ -n "${ATLAS_BIN:-}" ]; then
    (cd "$atlas_dir" && "$ATLAS_BIN" "$@")
  else
    local live="" dev=""
    if [ -n "${ATLAS_LIVE_URL:-}" ]; then live="$(atlas_container_url "$ATLAS_LIVE_URL")"; fi
    if [ -n "${ATLAS_DEV_URL:-}" ]; then dev="$(atlas_container_url "$ATLAS_DEV_URL")"; fi
    atlas_docker_run -v "$atlas_dir:/work" -w /work \
      -e "ATLAS_LIVE_URL=$live" -e "ATLAS_DEV_URL=$dev" \
      "$ATLAS_IMAGE" "$@"
  fi
}

# psql never reads stdin here: -c only. </dev/null also stops `docker run -i`
# from swallowing a caller's here-string.
atlas_psql() {
  if [ -z "${ATLAS_PG_CONTAINER:-}" ] && command -v psql >/dev/null 2>&1; then
    psql -X -v ON_ERROR_STOP=1 "$@" </dev/null
  else
    local a args=()
    for a in "$@"; do args+=("$(atlas_container_url "$a")"); done
    atlas_docker_run -i "$ATLAS_PSQL_IMAGE" psql -X -v ON_ERROR_STOP=1 "${args[@]}" </dev/null
  fi
}

# The tables atlas/schema.hcl declares, one `schema.table` per line, in file
# order. With $1, only those in that schema. Pairs every `table "<name>" {`
# with the `schema = schema.<s>` line inside it.
atlas_declared_tables() {
  awk -v only="${1:-}" '
    /^table "[^"]+"[[:space:]]*\{/ { match($0, /"[^"]+"/); t = substr($0, RSTART + 1, RLENGTH - 2); next }
    t != "" && /^[[:space:]]*schema[[:space:]]*=[[:space:]]*schema\./ {
      sub(/.*schema\./, ""); gsub(/[[:space:]]/, "")
      if (only == "" || $0 == only) print $0 "." t
      t = ""
    }
  ' "$atlas_dir/schema.hcl"
}

# postgres(ql)://... -> postgres://...?sslmode=disable unless an sslmode is set.
atlas_normalize_url() {
  local url="$1"
  url="postgres://${url#*://}"
  case "$url" in
    *sslmode=*) ;;
    *\?*) url="$url&sslmode=disable" ;;
    *) url="$url?sslmode=disable" ;;
  esac
  printf '%s\n' "$url"
}

# The same server and credentials as $1, database $2.
atlas_url_with_db() {
  local url="$1" db="$2" base query=""
  base="${url%%\?*}"
  case "$url" in *\?*) query="?${url#*\?}" ;; esac
  printf '%s/%s%s\n' "${base%/*}" "$db" "$query"
}

# Create a throwaway dev database beside the database at $1, drop its public
# schema, and export ATLAS_DEV_URL. Dropped again by atlas_cleanup.
atlas_provision_dev_db() {
  atlas_dev_admin_url="$1"
  atlas_dev_db_name="atlas_dev_$$_${RANDOM}"
  atlas_psql "$atlas_dev_admin_url" -qc "CREATE DATABASE \"$atlas_dev_db_name\"" >/dev/null \
    || atlas_fail "could not create the Atlas dev database on the target's server"
  ATLAS_DEV_URL="$(atlas_url_with_db "$atlas_dev_admin_url" "$atlas_dev_db_name")"
  export ATLAS_DEV_URL
  atlas_psql "$ATLAS_DEV_URL" -qc "DROP SCHEMA public CASCADE" >/dev/null \
    || atlas_fail "could not drop the public schema of the Atlas dev database"
}

# SQL statement lines of an Atlas plan: drop the `--` comment lines, blanks, and
# the "Schema(s) is/are synced, no changes to be made" line Atlas prints on
# stdout when there is nothing to do.
atlas_statements() {
  grep -vE '^[[:space:]]*(--|$)|^Schemas? (is|are) synced, no changes to be made\.?$' "$1" || true
}

# Statement lines of plan file $1 that DROP anything.
atlas_plan_drops() {
  atlas_statements "$1" | grep -iwE 'DROP' || true
}
