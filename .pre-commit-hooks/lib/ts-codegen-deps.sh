#!/usr/bin/env bash
#
# ts-codegen-deps.sh — make the TypeScript codegen's Node dependencies
# resolvable, WITHOUT writing into the qontinui-schemas checkout.
#
# ── THE PROBLEM ─────────────────────────────────────────────────────────────
#
# `qontinui-schemas/scripts/compile_typescript.mjs` — the script
# `src-tauri/scripts/generate_types.sh` runs to emit the per-type `.d.ts`
# files — imports three BARE specifiers:
#
#     import { compile }       from 'json-schema-to-typescript';
#     import { toSafeString }  from 'json-schema-to-typescript/dist/src/utils.js';
#     import { format }        from 'prettier';
#
# Node resolves a bare specifier in an ES module by walking `node_modules`
# directories UP FROM THE IMPORTING FILE'S OWN LOCATION. There is no
# `NODE_PATH` for ESM. So the resolution root is fixed by where that file
# sits: `qontinui-schemas/node_modules`, and nowhere else.
#
# `node_modules/` is gitignored in qontinui-schemas, so NO checkout of that
# repo ever carries it — a checkout gets one only if somebody runs
# `npm install` in it. CI does exactly that, as an explicit step
# (`.github/workflows/qontinui-types-drift.yml`, "Install qontinui-schemas npm
# deps"). A coord-allocated agent worktree does not: `POST /agents/allocate`
# provisions the declared sibling CHECKOUT, pinned to a SHA, and a checkout is
# not a build environment.
#
# The result, before this library existed: every `git push` from a freshly
# allocated worktree died inside `gen-events-drift.sh` with
#
#     Error [ERR_MODULE_NOT_FOUND]: Cannot find package 'json-schema-to-typescript'
#
# reported as "generate_types.sh failed (exit 1)" — indistinguishable from a
# codegen break, and leaving the agent only `--no-verify` (forbidden, and it
# disables the cargo gate too) or not pushing.
#
# ── THE SHAPE OF THE FIX, AND THE ONE IT IS NOT ─────────────────────────────
#
# It is NOT "skip the drift check when node_modules is missing". That is
# absence-reads-as-OK: it would clear genuine codegen drift in precisely the
# environment agents work in. This library makes the dependency AVAILABLE so
# the gate actually runs, and reports honestly when it could not.
#
# It is also not `npm install` inside the schemas checkout. `gen-events-drift.sh`
# holds one invariant above all others — *** ZERO writes inside the
# qontinui-schemas checkout *** — because on a dev box that tree is SHARED and
# holds other sessions' uncommitted work. `node_modules` is gitignored, so an
# install there would slip past both halves of that hook's write tripwire
# (`git status --porcelain -uall` never lists it; neither does the
# `ts/src/generated` content snapshot) — the exact class of write that goes
# unnoticed until it races a peer's concurrent install. Everything here is
# built inside a scratch directory the CALLER owns and removes.
#
# ── THE RUNGS ───────────────────────────────────────────────────────────────
#
#   sibling      the sibling's OWN `node_modules` satisfies this checkout's
#                pins AND the specifiers resolve from `$SCHEMAS_DIR/scripts`.
#                Run the sibling's own script, unchanged. This is the normal
#                dev box and the CI lane; nothing below it executes there.
#   donor        another checkout of the same repo has a `node_modules` whose
#                installed versions EQUAL this checkout's exact pins. Copy the
#                compile script into a hook-owned shim and symlink that
#                `node_modules` beside it.
#   npm          copy `package.json` + the script into the shim and
#                `npm install` there (measured 592 ms with a warm cache;
#                22 MB). Mirrors what CI does, in a directory we own.
#   unavailable  none of the above. The caller reports it loudly; it never
#                passes silently.
#
# EVERY rung is confirmed by the SAME probe — a real `node --input-type=module`
# import of all three specifiers, run with the candidate directory as the cwd,
# which is exactly how Node will resolve for the real script. A rung that does
# not actually work falls through instead of producing a half-built shim. That
# is also why the symlink needs no per-platform special-casing: if `ln -s`
# degrades on a Windows shell, the probe fails and the `npm` rung runs.
#
# EVERY rung is version-gated, not layout-gated, and the SIBLING rung is
# gated too — which is not obvious and is the reason it is spelled out. Node's
# walk-up does not stop at the schemas checkout: it continues to `/`. On this
# box `<workspace-root>/qontinui-runner/node_modules` — an ancestor of an
# allocated `agent-worktrees/<id>/qontinui-schemas` — holds `prettier@3.8.3`
# against a pinned `3.9.6`, and it fails to satisfy rung 1 today only because
# it happens to carry no `json-schema-to-typescript`. Since prettier's version
# decides `.d.ts` formatting, an ancestor supplying it would produce FABRICATED
# drift verdicts. So rung 1 requires the sibling's own `node_modules` to match
# the pins before the resolvability probe is even consulted; a hoisted or
# stale tree falls through to `donor`/`npm` rather than answering with an
# unpinned toolchain. The donor's checkout is derived from git itself
# (`--git-common-dir` names the primary checkout a linked worktree belongs to)
# and is subject to the same comparison. A mismatch falls through; the gate
# never silently generates with a different toolchain than the one pinned.
#
# ── CONTRACT ────────────────────────────────────────────────────────────────
#
#   ts_codegen_deps_resolve <schemas_dir> <shim_parent_dir>
#
# sets, and never returns non-zero:
#
#   TS_CODEGEN_DEPS_STATE   sibling | donor | npm | unavailable
#   TS_CODEGEN_DEPS_SCRIPT  the compile_typescript.mjs to run ("" when
#                           unavailable)
#   TS_CODEGEN_DEPS_DETAIL  one line naming the rung that answered
#   TS_CODEGEN_DEPS_TRIED   newline-joined log of every rung that did not, and
#                           why — this is what makes the `unavailable` message
#                           actionable rather than opaque
#
# It creates <shim_parent_dir> only when it needs one, and writes nothing
# outside it.
#
# One environment knob, and it is the manual escape hatch for a layout the
# git-derived donor cannot reach:
#
#   QONTINUI_TS_CODEGEN_NODE_MODULES   a `node_modules` directory to try as the
#                                      donor BEFORE the git-derived one. It is
#                                      version-gated exactly like any other
#                                      donor, so naming a mismatched tree makes
#                                      the ladder fall through rather than
#                                      accept it.

# The specifiers, spelled exactly as compile_typescript.mjs spells them. The
# deep subpath is included deliberately: a package can resolve while its
# `exports` map refuses that path, and the real script would then fail after
# this library reported success.
TS_CODEGEN_DEPS_IMPORTS="import 'json-schema-to-typescript'; import 'json-schema-to-typescript/dist/src/utils.js'; import 'prettier';"

# The direct dependencies whose versions the donor rung compares. Transitive
# packages are not compared — the pins are exact and npm's resolution of them
# is deterministic enough that this is a guardrail, not a lockfile. There IS no
# lockfile at the schemas root to be stricter with (which is also why CI runs
# `npm install` and not `npm ci`).
TS_CODEGEN_DEPS_PACKAGES="json-schema-to-typescript prettier"

# Can Node resolve all three specifiers when the importing file sits in $1?
#
# Uses the same walk-up rule as the real import, by running node with $1 as the
# cwd: for `--input-type=module -e`, the resolution base is the cwd. Cheap
# (~80 ms measured) because resolution failure throws before any of the
# packages' own work happens.
ts_codegen_deps_resolvable_from() {
    local dir="$1"
    [ -n "$dir" ] && [ -d "$dir" ] || return 1
    command -v node >/dev/null 2>&1 || return 1
    ( cd "$dir" && node --input-type=module -e "$TS_CODEGEN_DEPS_IMPORTS" ) \
        >/dev/null 2>&1
}

# Version string a package.json PINS for a dependency (dev or prod), or "".
ts_codegen_deps_pinned() {
    local pkg_json="$1" name="$2"
    [ -f "$pkg_json" ] || return 0
    node -e '
        const fs = require("fs");
        let p = {};
        try { p = JSON.parse(fs.readFileSync(process.argv[1], "utf8")); } catch { }
        const d = Object.assign({}, p.dependencies, p.devDependencies);
        process.stdout.write(String(d[process.argv[2]] || ""));
    ' "$pkg_json" "$name" 2>/dev/null || true
}

# Version a node_modules tree actually has INSTALLED for a package, or "".
ts_codegen_deps_installed() {
    local node_modules="$1" name="$2"
    local pkg_json="$node_modules/$name/package.json"
    [ -f "$pkg_json" ] || return 0
    node -e '
        const fs = require("fs");
        try {
            process.stdout.write(String(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).version || ""));
        } catch { }
    ' "$pkg_json" 2>/dev/null || true
}

# Does $1 (a node_modules dir) satisfy every pin declared by $2 (a package.json)?
#
# Fails when a pin is absent, when the package is not installed, or when the
# two differ as strings. String equality is right BECAUSE the pins are exact
# ("15.0.4", not "^15.0.4"); a range would simply never match and the caller
# falls through to a real install, which is the safe direction.
ts_codegen_deps_donor_matches() {
    local node_modules="$1" pkg_json="$2" name pinned installed
    [ -d "$node_modules" ] || return 1
    for name in $TS_CODEGEN_DEPS_PACKAGES; do
        pinned="$(ts_codegen_deps_pinned "$pkg_json" "$name")"
        installed="$(ts_codegen_deps_installed "$node_modules" "$name")"
        [ -n "$pinned" ] || return 1
        [ -n "$installed" ] || return 1
        [ "$pinned" = "$installed" ] || return 1
    done
    return 0
}

# The `node_modules` of the PRIMARY checkout that $1 belongs to, or "".
#
# `--git-common-dir` is the whole mechanism: from a linked worktree it names
# the primary checkout's `.git`, so its parent is the shared checkout — the one
# a human has actually run `npm install` in. From the primary checkout it names
# that same directory, and the candidate is then identical to the sibling rung
# that already declined, which the caller skips.
ts_codegen_deps_primary_node_modules() {
    local dir="$1" common
    [ -d "$dir" ] || return 0
    command -v git >/dev/null 2>&1 || return 0
    common="$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 0
    [ -n "$common" ] || return 0
    printf '%s/node_modules' "$(dirname "$common")"
}

ts_codegen_deps_note() {
    if [ -n "$TS_CODEGEN_DEPS_TRIED" ]; then
        TS_CODEGEN_DEPS_TRIED="$TS_CODEGEN_DEPS_TRIED
  - $1"
    else
        TS_CODEGEN_DEPS_TRIED="  - $1"
    fi
}

# Lay the compile script into the shim. Copied from the checkout the CALLER
# validated and is watching with its write tripwire, so the shim runs that
# checkout's generator — the property `QONTINUI_SCHEMAS_DIR` forwarding exists
# to preserve — just from a directory where Node can resolve its imports.
ts_codegen_deps_stage_script() {
    local schemas_dir="$1" shim="$2"
    mkdir -p "$shim" 2>/dev/null || return 1
    cp "$schemas_dir/scripts/compile_typescript.mjs" "$shim/compile_typescript.mjs" 2>/dev/null || return 1
    return 0
}

ts_codegen_deps_resolve() {
    local schemas_dir="$1" shim_parent="$2"
    local scripts_dir="$schemas_dir/scripts"
    local shim="$shim_parent/shim"
    local candidate npm_log rc

    TS_CODEGEN_DEPS_STATE="unavailable"
    TS_CODEGEN_DEPS_SCRIPT=""
    TS_CODEGEN_DEPS_DETAIL=""
    TS_CODEGEN_DEPS_TRIED=""

    if ! command -v node >/dev/null 2>&1; then
        ts_codegen_deps_note "node is not on PATH — no rung can run without it"
        return 0
    fi

    # ── Rung 1: the sibling checkout already has what it needs ──────────────
    #
    # BOTH tests, in this order. Resolvability alone would accept an ANCESTOR
    # `node_modules` — Node's walk-up runs to `/` — and that tree is under no
    # obligation to match this checkout's pins. See the header.
    if ts_codegen_deps_donor_matches "$schemas_dir/node_modules" "$schemas_dir/package.json"; then
        if ts_codegen_deps_resolvable_from "$scripts_dir"; then
            TS_CODEGEN_DEPS_STATE="sibling"
            TS_CODEGEN_DEPS_SCRIPT="$scripts_dir/compile_typescript.mjs"
            TS_CODEGEN_DEPS_DETAIL="resolved from $schemas_dir/node_modules, at this checkout's pinned versions"
            return 0
        fi
        ts_codegen_deps_note "sibling: $schemas_dir/node_modules matches the pins but '$TS_CODEGEN_DEPS_IMPORTS' still does not resolve from $scripts_dir"
    else
        ts_codegen_deps_note "sibling: $schemas_dir/node_modules is absent, or does not hold the versions pinned in $schemas_dir/package.json (an ANCESTOR node_modules is deliberately not accepted — it is under no obligation to match the pins)"
    fi

    # ── Rung 2: borrow an already-installed, version-matching node_modules ──
    # The command substitution below is `set -e`-safe only because
    # ts_codegen_deps_primary_node_modules returns 0 on EVERY path (it prints
    # nothing when it cannot resolve). Keep that property if you edit it: a
    # non-zero return here would abort the sourcing hook mid-list.
    for candidate in \
        "${QONTINUI_TS_CODEGEN_NODE_MODULES:-}" \
        "$(ts_codegen_deps_primary_node_modules "$schemas_dir")"
    do
        [ -n "$candidate" ] || continue
        # The sibling's own tree already declined above; re-testing it here
        # would report a confusing second failure for the same reason.
        # Spelled as an `if` rather than `[ ... ] && continue`: this file is
        # SOURCED into a hook running under `set -e`, where a trailing `&&`
        # list whose left side is false is itself a failed statement.
        if [ "$candidate" = "$schemas_dir/node_modules" ]; then
            continue
        fi
        if [ ! -d "$candidate" ]; then
            ts_codegen_deps_note "donor: $candidate does not exist"
            continue
        fi
        if ! ts_codegen_deps_donor_matches "$candidate" "$schemas_dir/package.json"; then
            ts_codegen_deps_note "donor: $candidate has versions that do not match the pins in $schemas_dir/package.json"
            continue
        fi
        if ! ts_codegen_deps_stage_script "$schemas_dir" "$shim"; then
            ts_codegen_deps_note "donor: could not stage compile_typescript.mjs into $shim"
            continue
        fi
        # `|| true` on every `rm`: this file is SOURCED into a hook running
        # `set -euo pipefail`, so a bare failing statement aborts it with a raw
        # bash error — the one exit the hook header promises never to make.
        rm -rf "$shim/node_modules" || true
        if ! ln -s "$candidate" "$shim/node_modules" 2>/dev/null; then
            ts_codegen_deps_note "donor: could not link $candidate into the shim (symlinks unavailable?)"
            continue
        fi
        if ts_codegen_deps_resolvable_from "$shim"; then
            TS_CODEGEN_DEPS_STATE="donor"
            TS_CODEGEN_DEPS_SCRIPT="$shim/compile_typescript.mjs"
            TS_CODEGEN_DEPS_DETAIL="borrowed the version-matched node_modules at $candidate (nothing was written there)"
            return 0
        fi
        ts_codegen_deps_note "donor: linked $candidate but the imports still do not resolve from the shim"
        rm -rf "$shim/node_modules" || true
    done

    # ── Rung 3: install the pinned deps into a directory we own ─────────────
    if ! command -v npm >/dev/null 2>&1; then
        ts_codegen_deps_note "npm: not on PATH"
        return 0
    fi
    if [ ! -f "$schemas_dir/package.json" ]; then
        ts_codegen_deps_note "npm: $schemas_dir/package.json is missing, so there is nothing to install from"
        return 0
    fi
    if ! ts_codegen_deps_stage_script "$schemas_dir" "$shim"; then
        ts_codegen_deps_note "npm: could not stage compile_typescript.mjs into $shim"
        return 0
    fi
    rm -rf "$shim/node_modules" || true
    cp "$schemas_dir/package.json" "$shim/package.json" 2>/dev/null || {
        ts_codegen_deps_note "npm: could not copy $schemas_dir/package.json into the shim"
        return 0
    }
    npm_log="$shim_parent/npm-install.log"
    # `--include=dev` is LOAD-BEARING, not belt-and-braces. Both pins live in
    # `devDependencies`, and npm omits those whenever `NODE_ENV=production` is
    # exported (measured: `NODE_ENV=production npm config get omit` -> `dev`,
    # npm 11.17.0) or an `.npmrc` sets `omit=dev` / `production=true`. Without
    # it this rung installs nothing, the probe below fails, and the last rung
    # is silently unavailable in any production-flavoured shell.
    #
    # Announce BEFORE installing: everything below is redirected to a log, and
    # a cold cache can sit here for up to the timeout with the terminal showing
    # nothing at all. `gen-events-drift` carries no `stages:`, so this can fire
    # at pre-commit as well as pre-push — a silent frozen `git commit` is worse
    # than a slow one.
    printf '[ts-codegen-deps] installing the pinned codegen deps into %s (first run on this worktree; up to 300s on a cold npm cache)\n' "$shim"
    # Bounded: this runs inside a pre-push hook, and an npm reaching a
    # network that neither answers nor refuses would otherwise hang the push
    # with no output at all. `timeout` is absent on some Windows shells, so it
    # is used when present rather than required.
    rc=0
    if command -v timeout >/dev/null 2>&1; then
        ( cd "$shim" && timeout 300 npm install --include=dev --no-audit --no-fund --prefer-offline --loglevel=error ) \
            >"$npm_log" 2>&1 || rc=$?
    else
        ( cd "$shim" && npm install --include=dev --no-audit --no-fund --prefer-offline --loglevel=error ) \
            >"$npm_log" 2>&1 || rc=$?
    fi
    if [ "$rc" -ne 0 ]; then
        ts_codegen_deps_note "npm: 'npm install' in the shim exited $rc (log: $npm_log; usually no network and a cold npm cache)"
        return 0
    fi
    if ts_codegen_deps_resolvable_from "$shim"; then
        TS_CODEGEN_DEPS_STATE="npm"
        TS_CODEGEN_DEPS_SCRIPT="$shim/compile_typescript.mjs"
        TS_CODEGEN_DEPS_DETAIL="installed the pinned codegen deps into $shim (nothing was written to $schemas_dir)"
        return 0
    fi
    ts_codegen_deps_note "npm: 'npm install' succeeded but the imports still do not resolve from the shim"
    return 0
}
