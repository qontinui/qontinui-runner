#!/usr/bin/env bash
# Pre-push guard: abort a push FAST when the pre-commit framework has reverted
# this worktree's unstaged edits for the hook run.
#
# THE DEFECT (coord findings 690aaeec-9a4d-4bc2-9938-ef7d302d7ba0 and
# d0958551-c245-4fdf-af53-933447c13517). `pre-commit hook-impl --hook-type
# pre-push` resolves from_ref/to_ref from git's stdin, and pre-commit's
# `commands/run.py` stashes whenever it is NOT given --all-files or --files:
#
#     stash = not args.all_files and not args.files
#     ...
#     if stash: exit_stack.enter_context(staged_files_only(store.directory))
#
# `staged_files_only` writes every unstaged change to
# `<store>/patch<epoch>-<pid>` and runs `git checkout -- .` — the working tree
# is reverted to the index — and only re-applies the patch when the LAST hook
# finishes. This stage holds gen-events-drift (a release build) and
# cargo-prepush (fmt + a whole-workspace clippy), so in the measured case the
# tree sat reverted for 13 minutes, and every cargo-guard / cargo-verify build
# in that worktree compiled HEAD instead of the caller's edits. Worse, an edit
# made to that tree DURING the window is at risk: when the patch no longer
# applies, the framework runs `git checkout -- .` again before re-applying it.
#
# WHY A HOOK CANNOT PREVENT THE STASH, AND WHAT THIS ONE DOES INSTEAD. The
# stash is entered before the config's hooks are even loaded, and pre-commit
# has no config key that turns it off. So no hook in this file can stop it;
# the only fix that removes it is arming the stage with a mechanism that never
# stashes (qontinui-claude-config's install-repo-git-hooks.sh shim — see the
# NOTE this script prints). What a hook CAN do is bound the window: this one
# is the FIRST hook of the pre-push stage, declared `fail_fast: true`, so when
# it detects that the framework stashed it fails immediately, pre-commit skips
# every remaining hook, and the framework restores the edits within seconds of
# reverting them instead of after the whole cargo gate. Nothing is lost: the
# framework's own restore runs, and the patch file stays in its store.
#
# DETECTION. The stash is observed, not guessed: under the framework
# (`PRE_COMMIT=1`, which `run.py` exports to every hook) a
# `patch<epoch>-<pid>` file in the framework's store is THIS run's stash of
# THIS tree's unstaged edits when all three hold:
#   * its epoch is recent (the window below);
#   * the process that wrote it is the pre-commit process running THIS hook.
#     A finished run's patch stays in the store forever, and after the caller
#     discards those edits it applies cleanly again; and every worktree on a
#     box shares one store, so another worktree's live run can leave a patch
#     that happens to apply here. On Linux/macOS a `language: system` hook is
#     spawned directly by the pre-commit process, so the pid must equal $PPID
#     (falling back to "alive" only when $PPID is unusable). On Git Bash /
#     MSYS $PPID of a bash under native Python is 1, so the pid — a Windows
#     pid — is only required to be ALIVE (`ps -W`); a live foreign run's patch
#     that applies here, or a reused pid, can still refuse a clean push there.
#     That residual false REFUSAL is accepted and named in the message: it
#     costs a retry or the override, never an edit. When liveness cannot be
#     read the patch is treated as live, for the same reason;
#   * `git apply --check` accepts it against this tree right now — a patch whose
#     edits are present (already restored) does not apply.
# No such patch, or a store that cannot be read, passes: that is today's
# behaviour, never worse. Run outside the framework (the shim, a hand run) it
# does nothing at all, because nothing stashed.
#
#   QONTINUI_PREPUSH_ALLOW_STASH=1        let the push proceed under the stash
#                                         (you are certain no build reads this
#                                         tree for the next few minutes)
#   QONTINUI_PREPUSH_STASH_WINDOW_SECS=N  how old a patch may be and still be
#                                         this run's (default 900)
#   QONTINUI_PREPUSH_STASH_STORE=<dir>    the store to search (tests); default
#                                         the framework's own resolution:
#                                         $PRE_COMMIT_HOME, else
#                                         ${XDG_CACHE_HOME:-~/.cache}/pre-commit
#                                         — and on Windows also
#                                         %USERPROFILE%/.cache/pre-commit, since
#                                         Python's `~` reads USERPROFILE, not
#                                         Git Bash's $HOME.
#
# SCOPE. This reads pre-commit's store layout (`staged_files_only.py`, checked
# against 4.6.0). prek keeps a different store; under prek the guard finds no
# patch and passes, i.e. today's behaviour.

set -uo pipefail

[[ "${PRE_COMMIT:-}" == "1" ]] || exit 0

if [[ -n "${QONTINUI_PREPUSH_STASH_STORE:-}" ]]; then
  stores=("$QONTINUI_PREPUSH_STASH_STORE")
elif [[ -n "${PRE_COMMIT_HOME:-}" ]]; then
  stores=("$PRE_COMMIT_HOME")
else
  stores=("${XDG_CACHE_HOME:-$HOME/.cache}/pre-commit")
  if [[ -z "${XDG_CACHE_HOME:-}" && -n "${USERPROFILE:-}" ]] && command -v cygpath >/dev/null 2>&1; then
    _up="$(cygpath -u "$USERPROFILE" 2>/dev/null)/.cache/pre-commit"
    [[ "$_up" -ef "${stores[0]}" ]] || stores+=("$_up")
  fi
fi

window="${QONTINUI_PREPUSH_STASH_WINDOW_SECS:-900}"
[[ "$window" =~ ^[0-9]+$ ]] || window=900
now="$(date +%s)"
root="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0

# Windows pids of live processes, read once, only on MSYS/Cygwin (`ps -W`'s
# WINPID column; a leading status flag such as `I` shifts it by one). Empty
# when `ps -W` cannot be read, which pid_is_this_run treats as "alive".
WIN_PIDS=""
WIN_PIDS_OK=0
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    if _ps="$(ps -W 2>/dev/null)"; then
      WIN_PIDS="$(printf '%s\n' "$_ps" | awk 'NR > 1 { i = ($1 ~ /^[0-9]+$/) ? 4 : 5; print $i }')"
      [[ -n "$WIN_PIDS" ]] && WIN_PIDS_OK=1
    fi
    IS_WINDOWS=1
    ;;
  *) IS_WINDOWS=0 ;;
esac

pid_is_this_run() {
  local pid="$1"
  if [[ "$IS_WINDOWS" == "1" ]]; then
    [[ "$WIN_PIDS_OK" == "1" ]] || return 0
    printf '%s\n' "$WIN_PIDS" | grep -qx -- "$pid"
    return
  fi
  # The writer is this hook's parent; see DETECTION above.
  if [[ "${PPID:-}" =~ ^[0-9]+$ && "$PPID" -gt 1 ]]; then
    [[ "$pid" == "$PPID" ]]
    return
  fi
  if [[ -d /proc/self ]]; then
    [[ -d "/proc/$pid" ]]
    return
  fi
  kill -0 "$pid" 2>/dev/null && return 0
  # EPERM still means the process exists.
  [[ "$(kill -0 "$pid" 2>&1)" == *ermitted* ]]
}

applies_here() {
  local patch="$1"
  [[ -s "$patch" ]] || return 1
  git -C "$root" apply --check --whitespace=nowarn "$patch" >/dev/null 2>&1 && return 0
  git -C "$root" -c core.autocrlf=false apply --check --whitespace=nowarn "$patch" >/dev/null 2>&1
}

found=""
for store in "${stores[@]}"; do
  [[ -d "$store" ]] || continue
  for patch in "$store"/patch*-*; do
    [[ -f "$patch" ]] || continue
    name="${patch##*/}"
    epoch="${name#patch}"
    epoch="${epoch%%-*}"
    pid="${name##*-}"
    [[ "$epoch" =~ ^[0-9]+$ && "$pid" =~ ^[0-9]+$ ]] || continue
    (( now - epoch <= window && epoch - now <= 60 )) || continue
    pid_is_this_run "$pid" || continue
    if applies_here "$patch"; then
      found="$patch"
      break 2
    fi
  done
done

[[ -n "$found" ]] || exit 0

changed="$(git -C "$root" apply --numstat "$found" 2>/dev/null | wc -l | tr -d ' ')"

if [[ "${QONTINUI_PREPUSH_ALLOW_STASH:-}" == "1" ]]; then
  echo "[pre-push] WARNING — pre-commit reverted ${changed:-?} unstaged file(s) in this worktree for"
  echo "[pre-push]   the hook run (patch: $found). QONTINUI_PREPUSH_ALLOW_STASH=1 — proceeding;"
  echo "[pre-push]   any build in this tree compiles the INDEX, not your edits, until it ends."
  exit 0
fi

cat >&2 <<EOF
[pre-push] REFUSED — pre-commit reverted this worktree's unstaged edits for the hook run.
[pre-push]   ${changed:-?} file(s) were stashed to $found
[pre-push]   and the tree was checked out to the index. The rest of this stage (the
[pre-push]   gen-events release build, cargo fmt + clippy) would keep it reverted for
[pre-push]   minutes, and any cargo-guard / cargo-verify build here would compile the
[pre-push]   index instead of your edits (findings 690aaeec, d0958551). The push is
[pre-push]   aborted NOW so pre-commit restores the edits immediately; nothing is lost
[pre-push]   (the patch above is pre-commit's own copy).
[pre-push]   If this tree has NO unstaged edits, the patch is another live pre-commit
[pre-push]   run's (the store is shared by every worktree): retry once it finishes.
[pre-push]   To push:
[pre-push]     * commit the edits (a WIP commit is fine), then push again; or
[pre-push]     * QONTINUI_PREPUSH_ALLOW_STASH=1 git push, only if no build reads this
[pre-push]       tree until the push finishes.
[pre-push]   Permanent fix for this machine: arm the stage with the stash-free shim —
[pre-push]   install-repo-git-hooks.sh --shim (qontinui-claude-config/scripts).
EOF
exit 1
