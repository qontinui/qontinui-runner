#!/usr/bin/env bash
# stage-coord-bearer.sh -- stage a coord bearer for exec-equivalence.py, or say
# why none could be. A sidecar of the follow-up sweep skill: it reaches its one
# dependency, coord-tenant-credential.sh, beside itself, never through a
# checkout path, so it works wherever the skill directory was copied.
#
# USAGE (exec-equivalence.py is the caller; nothing else needs it)
#   bash stage-coord-bearer.sh owner <owner/repo> [<candidate-tenant>...]
#   bash stage-coord-bearer.sh any [<candidate-tenant>...]
#
#   owner  the bearer must be for the tenant PROVEN to own <owner/repo>
#          (coord's author-session ownership door answered 200 under it):
#          the tenant whose declared code_paths are being read. When no tenant
#          is proven, it falls back to `any` and says so in owner_state.
#   any    the first candidate tenant a bearer can be staged for: the coord
#          route it is for is auth-only, so any tenant's credential serves.
#          Candidates are tried in argument order, then the device's bindings.
#
# CALLER CONTRACT
#   CTC_TMP        a private directory; on success the bearer is staged at
#                  $CTC_TMP/bearer.hdr as `Authorization: Bearer <jwt>`, which
#                  the caller reads and the caller deletes.
#   CTC_COORD_URL, CTC_PY, CTC_NO_MINT, CTC_NO_RUNNER and the rest: read by
#                  coord-tenant-credential.sh (see its header).
#
# OUTPUT: `key=value` lines on stdout, never the token.
#   state=proven|any|none   tenant=<uuid>   source=<where the bearer came from>
#   owner_state=proven|refuted|unknown (owner mode only)   owner_note=<why>
#   transport=1 when a not-proven answer met an unreachable coord
#   note=<why nothing was staged>
# Always exits 0: the verdict is the state line, and a caller reading an exit
# status alone could not tell "no credential" from "the script did not run".

set -u

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ctc="$here/coord-tenant-credential.sh"
if [ ! -r "$ctc" ]; then
  printf 'state=none\nnote=coord-tenant-credential.sh is not beside stage-coord-bearer.sh\n'
  exit 0
fi
if [ -z "${CTC_TMP:-}" ] || [ ! -d "$CTC_TMP" ]; then
  printf 'state=none\nnote=CTC_TMP is not a directory\n'
  exit 0
fi
# shellcheck source=coord-tenant-credential.sh
. "$ctc"

mode="${1:-}"
[ "$#" -gt 0 ] && shift

case "$mode" in
  owner)
    slug="${1:-}"
    [ "$#" -gt 0 ] && shift
    ctc_owner_tenant "$slug"
    if [ "$CTC_STATE" = proven ]; then
      printf 'state=proven\ntenant=%s\nsource=%s\nowner_state=proven\n' \
        "$CTC_TENANT" "$(ctc_sanitize "$CTC_BEARER_SRC")"
      exit 0
    fi
    printf 'owner_state=%s\nowner_note=%s\ntransport=%s\n' \
      "$CTC_STATE" "$(ctc_sanitize "$CTC_NOTE")" "$CTC_TRANSPORT"
    ;;
  any) ;;
  *)
    printf 'state=none\nnote=mode must be owner or any, got %s\n' "$(ctc_sanitize "${mode:-<empty>}")"
    exit 0
    ;;
esac

tried=" "
why=""
stage_one() { # <tenant> -> 0 when a bearer is now staged
  local t="${1,,}" src
  ctc_is_uuid "$t" || return 1
  case "$tried" in *" $t "*) return 1 ;; esac
  tried="$tried$t "
  src="$(ctc_stage_for_tenant "$t")"
  case "$src" in
    rejected*) why="${why}$t: ${src#rejected}; "; return 1 ;;
  esac
  printf 'state=any\ntenant=%s\nsource=%s\n' "$t" "$(ctc_sanitize "$src")"
  return 0
}

for cand in "$@"; do
  stage_one "$cand" && exit 0
done
ctc_bindings
bindings=()
[ -n "$CTC_BINDINGS" ] && read -r -a bindings <<<"$CTC_BINDINGS"
for cand in ${bindings[@]+"${bindings[@]}"}; do
  stage_one "$cand" && exit 0
done
[ -n "$CTC_BINDINGS" ] || why="${why}bindings: ${CTC_BINDINGS_NOTE:-none}; "
printf 'state=none\nnote=%s\n' "$(ctc_sanitize "no tenant credential could be staged (${why%; })")"
exit 0
