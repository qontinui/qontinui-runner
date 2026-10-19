#!/usr/bin/env bash
# Drop cached `postgresql_embedded` build units whose embedded PostgreSQL release
# is not the one pinned in src-tauri/.cargo/config.toml (POSTGRESQL_VERSION).
#
# WHY: the crate's `bundled` build script returns early when its OUT_DIR already
# holds an archive. Cargo does re-run the script when the pin changes, but
# OUT_DIR persists across runs, so a restored CI cache (or any warm target dir)
# keeps embedding the old release. Measured 2026-10-19: main's ubuntu-22.04
# rust-cache entry, saved from a failing run, held 18.6.1, whose initdb needs
# GLIBC 2.36. Removing the unit's build/ and .fingerprint/ dirs makes cargo
# rebuild the crate and run the script against an empty OUT_DIR.
#
# Usage: purge-stale-bundled-postgres.sh [target-dir]   (default: <repo>/target)
# Exit: 0 = done (purged or nothing to purge), 1 = no exact pin found.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CONFIG="$ROOT/src-tauri/.cargo/config.toml"
TARGET="${1:-$ROOT/target}"

pin="$(sed -nE 's/^[[:space:]]*POSTGRESQL_VERSION[[:space:]]*=[[:space:]]*"=([0-9]+\.[0-9]+\.[0-9]+)".*/\1/p' "$CONFIG" | head -n1)"
if [ -z "$pin" ]; then
  echo "::error::no exact POSTGRESQL_VERSION = \"=X.Y.Z\" pin in $CONFIG"
  exit 1
fi
echo "bundled PostgreSQL pin: $pin (target dir: $TARGET)"

shopt -s nullglob
kept=0
purged=0
# target/<profile>/build/... and target/<triple>/<profile>/build/...
for vf in "$TARGET"/*/build/postgresql_embedded-*/out/postgresql.version \
          "$TARGET"/*/*/build/postgresql_embedded-*/out/postgresql.version; do
  # An earlier iteration may already have purged this profile.
  [ -f "$vf" ] || continue
  have="$(tr -d '[:space:]' <"$vf")"
  unit="$(dirname "$(dirname "$vf")")"
  profile_dir="$(dirname "$(dirname "$unit")")"
  if [ "$have" = "$pin" ]; then
    echo "keep  $unit (embeds $have)"
    kept=$((kept + 1))
    continue
  fi
  echo "purge $profile_dir/{build,.fingerprint}/postgresql_embedded-* ($unit embeds $have)"
  rm -rf "$profile_dir"/build/postgresql_embedded-* "$profile_dir"/.fingerprint/postgresql_embedded-*
  purged=$((purged + 1))
done
echo "bundled PostgreSQL units: kept=$kept purged=$purged"
