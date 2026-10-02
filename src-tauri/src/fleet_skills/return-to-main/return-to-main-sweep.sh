#!/usr/bin/env bash
# return-to-main-sweep.sh — return primary checkouts to their default branch
# when the branch they are parked on holds nothing that is not already upstream.
#
# Plan: 2026-09-04-primary-checkouts-pinned-behind-main-by-work-that-already-landed
# (Phase 5). Depends STRICTLY on Phase 1 (`scripts/classify-branch-state.sh`).
# Hardened for UNATTENDED runs by plan 2026-09-13-nightly-return-to-main-sweep
# (Phase 1: run-from-copy, --fetch, honest ages, --adjudicated-landed,
# --restore-residue) -- see "UNATTENDED HARDENING" below.
#
# WHY THIS EXISTS, AND WHY THE ORDER MATTERED
#
# On a fleet whose merge authority REBASE-lands, a branch that shipped keeps its
# commits locally under different SHAs. Every cheap test then reads "unpushed
# peer work", every tool refuses, and the primary checkout stays parked on a
# ghost — measured 2026-09-04: `qontinui-runner` 315 commits behind for 11 days,
# held there by a byte-identical duplicate of PR #1115; `qontinui-web` 151
# behind, held by a 2-line residue of PR #1178. Both PRs carry coord's own
# comment saying they landed. Nothing local read it.
#
# THE ORDER IS LOAD-BEARING AND THIS PARAGRAPH IS THE RECORD OF IT. Installed
# BEFORE Phase 1, this sweeper would have classified those four parked repos
# with the only evidence a pre-Phase-1 fleet had — ancestry and a commit count —
# refused all four exactly as `/pull-scoped` does today, written a log full of
# abstentions, and looked like it was working. A sweeper that abstains on 100 %
# of its cases is indistinguishable from a correct one until you check what it
# abstained on. Phase 1 shipped first, so the discrimination this job acts on is
# real: the fleet-scale measurement from that phase (`scan-worktree-wip.sh
# --classify --max 400`, 206 checkouts in 18 s) found **136 LANDED_DUPLICATE**
# against 69 UNIQUE_WIP, 1 MIXED, 1 INCOMPLETE. Roughly two thirds of what the
# fleet treats as untouchable peer work is a ghost. That ratio is what makes
# this job worth scheduling, and it was not measurable before Phase 1 existed.
#
# THE JOB'S WORK IS RESTORING THE DEFAULT BRANCH. THE PULL IS A CONSEQUENCE.
# This is not "pull-all with a safety check". It never merges, never rebases,
# never resolves a conflict, and contacts the network ONLY under `--fetch` —
# and then only to refresh origin's remote-tracking refs. It moves a checkout
# back onto its default branch and fast-forwards that branch to the upstream
# ref (freshly fetched under `--fetch`, as last fetched otherwise). If the
# fast-forward is not a fast-forward, it stops.
#
# ---------------------------------------------------------------------------
# WHAT IT MAY TOUCH, AND THE SAFETY ORDER
#
# The commissioning plan's §6 names a wrong LANDED_DUPLICATE as the ONLY way
# this plan can destroy work. So the safety properties are stated as an order,
# and each one is checked before the next:
#
#   1. PRIMARY CHECKOUTS ONLY, depth 1 under the workspace root, and only where
#      `.git` is a DIRECTORY. A linked worktree's `.git` is a FILE; agent
#      worktrees and the `-wt-*` siblings are therefore never candidates. They
#      are allocated through coord (`POST /agents/allocate`), owned by a
#      session, and reclaimed by machinery that is not this.
#   2. THE VERDICT IS PHASE 1'S, NOT THIS SCRIPT'S. Only exit 0
#      (LANDED_DUPLICATE) is actionable. UNIQUE_WIP, MIXED and INCOMPLETE are
#      all abstentions, and INCOMPLETE is NEVER collapsed toward
#      LANDED_DUPLICATE — that is precisely the move that would authorise a
#      destructive action on an unknown. Uncommitted content (tracked OR
#      untracked) forces UNIQUE_WIP inside the classifier, before any commit is
#      examined, so a dirty tree can never reach the acting arm at all.
#      The ONE override is an explicit, evidenced `--adjudicated-landed
#      REPO=SHA` (see 1d below): it can turn a UNIQUE_WIP or MIXED on a CLEAN
#      tree whose HEAD is exactly SHA into an actionable verdict. It can never
#      override INCOMPLETE, a usage error, or uncommitted content.
#   3. THE VERDICT IS RE-VERIFIED IMMEDIATELY BEFORE THE SWITCH. Classification
#      happens at T and the switch at T+ε, and this fleet runs ~9 concurrent
#      sessions in shared checkouts. Between the two, this script re-reads HEAD
#      (abstain `raced_head_moved`), re-reads `git status --porcelain` (abstain
#      `raced_dirty`), and refuses on an in-progress rebase / merge / cherry-pick
#      / bisect / revert (abstain `operation_in_progress`) or a present
#      `index.lock` (abstain `index_locked`). A stale clean reading is not a
#      licence to act on a tree that has since moved.
#   4. A refs/wip/ SNAPSHOT IS WRITTEN BEFORE ANY BRANCH SWITCH AND BEFORE ANY
#      RESIDUE RESTORE, and a snapshot that cannot be written is an abstention,
#      not a warning. See below.
#   5. A RESTORER NEVER DELETES A BRANCH, and this script is a restorer: only the
#      gated reaper `.claude/skills/return-to-main/reap-restore-snapshot.sh`
#      may, 14 days later and after verifying the branch holds nothing a ref or
#      a landed patch does not (the contract:
#      knowledge-base/qontinui-specific/checkout-restore-contract.md, plan
#      2026-09-13-one-recovery-rule-for-both-checkout-restorers). So:
#      NEVER stash (no `git stash push`/`save`/`pop`/`apply` — the user's work
#      is never moved into or out of the stash). NEVER `reset --hard`. NEVER
#      `checkout -f`, `-B`, `--force`, `clean`, `branch -D`, or any `--force*`
#      flag. The complete set of mutating commands this script runs is:
#        * `git update-ref` (into refs/wip/, never over an existing ref name it
#          did not just compute);
#        * `git checkout --no-overwrite-ignore <branch>` /
#          `git checkout --no-overwrite-ignore -b <branch> origin/<branch>`;
#        * `git merge --ff-only --no-overwrite-ignore <upstream>`;
#      and before the switch the sweep abstains `ignored_path_overwrite: <p>`
#      when a path the switch or the fast-forward ADDS is already occupied on
#      disk by something untracked -- an ignored file or symlink, a directory
#      holding any untracked or ignored entry (a tracked directory upstream
#      turns into a file), or an untracked file / symlink at a leading
#      component -- git overwrites ignored files by default, and no snapshot
#      holds them. "Tracked" is decided case-insensitively when
#      core.ignorecase is true (an upstream case-only rename on NTFS/APFS is
#      git's to handle). Under --restore-residue the same check runs BEFORE
#      anything is restored. `--no-overwrite-ignore` is the backstop: git then
#      refuses instead, and the row is FAILED with git's message.
#        * ONLY under --fetch: `git fetch --prune --refmap= origin
#          '+refs/heads/*:refs/remotes/origin/*'` — an EXPLICIT refspec AND an
#          EMPTY refmap, so it writes refs/remotes/origin/* and FETCH_HEAD and
#          nothing else. Both halves are needed: an explicit refspec alone
#          still lets git apply the configured `remote.origin.fetch` mapping
#          to the refs it fetches, so a clone configured with the mirror
#          refspec `+refs/heads/*:refs/heads/*` would have a local branch
#          FORCE-overwritten (unpushed commits lost) or the fetch aborted when
#          the checked-out branch exists upstream; `--refmap=` makes git ignore
#          the configured refspecs entirely (see 1b);
#        * ONLY under --restore-residue, and only after the snapshot ref is
#          written: `git --literal-pathspecs restore --source=HEAD --staged
#          --worktree --pathspec-from-file=- ...` over EXACTLY the tracked
#          files proven restorable — literal, because a glob pathspec like
#          `f[1].txt` also matches `f1.txt` and would revert a file nobody
#          proved anything about (measured 2026-09-13) — and then `rm -f --`
#          of EXACTLY the untracked files proven byte-identical to the
#          upstream tip, one path at a time (never `git clean`).
#      Each of those REFUSES rather than discards. `git stash create`,
#      `commit-tree`, and `update-index`/`write-tree` on a SCRATCH index
#      (`GIT_INDEX_FILE` in the run's temp dir, never the checkout's) appear
#      as snapshot WRITERS of dangling objects that touch neither the tree nor
#      the index — see 1e.
#   6. EVERY action AND EVERY abstention is logged. An abstention that leaves no
#      trace is indistinguishable from a sweeper that never ran, which is §2.4's
#      finding about `nightly-pull-all.ps1` reproduced one layer up.
#
# WHY LIVENESS IS NOT RESOLVED HERE, AND WHY THAT IS NOT A GAP. Phase 1 refuses
# to answer "is anyone working in this checkout?" and served policy
# `coordination` `overlap-read-is-not-proof-of-a-free-file` records why: a
# reader's own `git status` rewrites `.git/index`, so an abandoned tree reads as
# seconds old. This script does not need that answer, and the reason is
# specific rather than convenient: the actionable case is LANDED_DUPLICATE ON A
# CLEAN TREE, which means every byte in that checkout is already on the upstream
# default branch. There is nothing for a live session to lose. What a live
# session can lose is an edit it is ABOUT to write, and that is what step 3's
# re-read plus step 4's snapshot cover. This script emits no liveness judgement
# and none of its abstentions should be read as one. (The unattended caller —
# `/return-to-main`, plan 2026-09-13 Phase 4 — gates the whole run on a
# machine-wide quiet check BEFORE invoking this script; that is its job, not
# this one's.)
#
# ---------------------------------------------------------------------------
# THE refs/wip/ SNAPSHOT — WHAT IT IS AND HOW A WRONG VERDICT IS RECOVERED
#
# The idiom is `scripts/wip-custody-record.sh`'s, reused rather than reinvented:
# `git update-ref` into the `refs/wip/` namespace the runner's
# `drain.rs::capture_wip_ref` established, with a reflog message carrying the
# context a bare sha cannot. It is deliberately NOT a second snapshot
# mechanism — one namespace, one recovery index (`git for-each-ref refs/wip`).
#
# WHAT IS SNAPSHOTTED. The actionable tree is clean by construction, so there is
# no uncommitted content for `git stash create` to capture; the thing at risk is
# the BRANCH TIP this script is about to stop pointing HEAD at. So the ref is
# written to the HEAD COMMIT:
#
#     refs/wip/return-to-main/<UTCstamp>-<repo>-<sha7>  ->  <HEAD before the switch>
#
# `git stash create` still runs first, as a READ and as the third race check: it
# writes a commit object and touches neither the working tree nor the index
# (which is what makes it safe on a hot path — `wip-custody-record.sh` documents
# the same property), and a NON-EMPTY result means the tree became dirty since
# step 3, so the sweep abstains `raced_dirty` rather than acting. On the acting
# path its output is empty, which is the expected and the only accepted answer.
#
# THE RESIDUE SNAPSHOT (1e) is the other shape: there the tree IS dirty, so the
# `git stash create` commit is the snapshot itself —
#
#     refs/wip/return-to-main/<UTCstamp>-<repo>-residue  ->  <stash-create commit>
#
# — written and verified BEFORE a single file is restored. `git stash apply
# <sha>` (or `git checkout <sha> -- <file>`) puts the pre-restore content back.
#
# TWO INDEPENDENT RECOVERY PATHS, and it is worth being explicit that neither
# depends on this log surviving:
#
#   a. THE BRANCH REF IS NEVER DELETED BY THIS SCRIPT. `git checkout main`
#      leaves `refs/heads/<branch>` exactly where it was. `git checkout
#      <branch>` puts the checkout back, and that is the whole undo for the
#      common case. It stays that way for the retention window: every snapshot
#      gets ONE 14-day `time_elapsed` gate (the /return-to-main skill registers
#      it; this script stays offline and marks the row `retention_gate:
#      "pending"`), and only the reaper that gate runs may delete the branch
#      and the snapshot, after its six checks (checkout-restore-contract.md).
#   b. THE SNAPSHOT REF, for the case where (a) is gone — someone later deleted
#      the branch, or the switch itself half-failed:
#         git for-each-ref --format='%(refname) %(objectname)' refs/wip/return-to-main
#         git reflog refs/wip/return-to-main/<...>     # names the repo and branch
#         git branch <branch> <sha>                    # restore the branch ref
#      The reflog message is `return-to-main-sweep: <repo> leaving <branch>
#      (verdict LANDED_DUPLICATE [adjudicated], upstream <ref> last fetched
#      <time>, tip committed <age>)`, so the ref is self-describing even with
#      the log lost.
#
# READ `wip_ref` OUT OF THE LOG RECORD, do not rebuild it — the same rule
# `wip-custody-record.sh` states for its own ref, for the same reason: `__` and
# `-` are separators, not delimiters guaranteed absent from either part.
#
# NAMESPACE. `refs/wip/return-to-main/...` cannot collide with the custody
# recorder's `refs/wip/<session-id>[__<worktree>]` unless a Claude session id is
# literally the string `return-to-main`, which is not a UUID. Stated rather than
# assumed because a git D/F conflict there would make BOTH writers fail.
#
# ---------------------------------------------------------------------------
# UNATTENDED HARDENING — plan 2026-09-13-nightly-return-to-main-sweep, Phase 1
#
# Run by hand on 2026-09-13 this sweep worked only because a session had first
# fetched every primary, cleaned `qontinui-claude-config`, and adjudicated a
# false UNIQUE_WIP (finding a911a385). Scheduled, none of that happens. So:
#
#   1a RUN FROM A COPY. This script's own checkout is one of its candidates and
#      sorts early. Fast-forwarding it mid-run would replace the classifier
#      every LATER candidate is judged by — and, since bash reads a script
#      incrementally, could replace lines of this script not yet read. So the
#      first thing a run does is copy this script, the classifier,
#      `dirty-provenance.sh` (when present) and `lib/` into a `mktemp -d`
#      directory and `exec` from there (guard env QONTINUI_RTM_REEXEC=1). One
#      run, one version. The copy is removed on exit — only a directory that is
#      provably the copy (it is the one this process runs from AND it carries
#      the parent's marker file), so a stray env var can never aim that `rm` at
#      anything else. A copy that cannot be built is a REFUSAL (exit 2).
#   1b --fetch. Before classifying each candidate, `git fetch` (the explicit
#      refspec + empty `--refmap=` above, so a configured mirror refspec can
#      neither overwrite a local branch nor abort the fetch) under a
#      wall-clock timeout, with terminal and credential
#      prompts disabled so an unattended run cannot hang on a password prompt.
#      Logged per candidate as `fetched` true|false plus `fetch_error`. A
#      failed fetch changes nothing about the decision rules: the ref is merely
#      older, which can only ADD abstentions, never fabricate a return.
#   1c HONEST AGES. The classifier reports `upstream_tip_age` (how old the tip
#      COMMIT is) and `upstream_fetched_at` (when the ref was last fetched) as
#      two fields, and every reason line below names them separately. Until
#      this phase the tip's age was printed as "as last fetched, <age>".
#   1d --adjudicated-landed REPO=SHA (repeatable) + --evidence FILE. The one
#      case finding a911a385 shows a script gets wrong — a rebase-land with
#      conflict resolution, then further upstream edits to the same lines — is
#      settled by a judgment step OUTSIDE this script (`land-evidence.sh` plus
#      a reader). That judgment is passed in as an argument, never inferred:
#      the repo is treated as LANDED_DUPLICATE only while HEAD STILL EQUALS
#      SHA, the tree is clean, the classifier said UNIQUE_WIP or MIXED, AND
#      the branch is ahead of its upstream by ZERO merge commits. That last
#      clause is held here independently of the adjudicator: an EVIL MERGE (a
#      merge whose conflict resolution adds content no patch carries) is
#      invisible to `git cherry`, so to any patch-id judgment, and a
#      valid-looking evidence file does not lift it
#      (`adjudication_refused_ahead_merges`; a count that could not be taken
#      is `adjudication_refused_merge_count_unknown`).
#      Every other safety step is unchanged (re-verify, snapshot, --ff-only,
#      branch kept). Logged as `verdict_source: adjudicated`, with the evidence
#      file's path and sha256.
#   1d' --adjudicated-superseded REPO=SHA (repeatable) + --evidence FILE. The
#      SIBLING of 1d, holding EVERY safety property above unchanged and
#      differing in exactly one thing: the word it records. It is for the
#      adjacent class `land-evidence.sh` reports as SUPERSEDED (exit 5) — a
#      branch whose files upstream carried forward by later DIRECT edits, under
#      no PR, so nothing ever landed and nothing is owed. Logged as
#      `verdict_source: adjudicated_superseded`, verdict word
#      SUPERSEDED_BY_UPSTREAM.
#      WHY A SECOND ARGUMENT RATHER THAN A WIDER FIRST ONE: until this existed,
#      `--adjudicated-landed` was the ONLY argument that could move a MIXED
#      branch, so a session that correctly concluded "superseded" had to assert
#      "landed" to act at all. On 2026-09-16 that is exactly what happened: a
#      decision row on a plans repository reads `verdict_source: adjudicated`
#      for 8 commits that demonstrably never landed, while the evidence file
#      beside it says SUPERSEDED_BY_UPSTREAM. A ledger that cannot distinguish
#      "landed" from "superseded" will eventually be used to prove something
#      false. Naming the same repo BOTH ways is refused as incoherent.
#      (plan 2026-09-16-land-evidence-has-no-superseded-arm, D1.)
#   1e --restore-residue REPO (repeatable). A tree dirtied only by content
#      ALREADY ON the upstream default branch, or by provisioner residue, is
#      restored to HEAD and then classified as normal. `dirty-provenance.sh`
#      decides it by exact blob: UPSTREAM_CURRENT (every version equals the
#      upstream TIP's blob and mode — restoring and fast-forwarding reproduces
#      the same bytes) and UPSTREAM_HISTORICAL (the blob is in upstream
#      history) are restorable ANYWHERE; EOL_ONLY and RUNNER_BUNDLE only inside
#      the runner provisioner's footprint `.claude/commands/**` /
#      `.claude/skills/**` (plan 2026-09-29, the sweep never clears WIP that is
#      already on main). It acts ONLY when `dirty-provenance.sh --json
#      <checkout>` exits 0, every file in its payload is a restorable class
#      carrying `"restorable":true`, that set equals this script's own census
#      of changed TRACKED entries EXACTLY (staged and unstaged: index side
#      `.`/`M`/`A`/`D` with worktree side `.`/`M`/`D`; a type change, rename,
#      copy, conflict or submodule still refuses), its `untracked_on_upstream`
#      list (untracked files byte-identical to the tip) equals this script's
#      census of UNTRACKED files EXACTLY (any other untracked file keeps the
#      repo abstaining, `untracked_present`), and a path HEAD lacks (a staged
#      add) is admitted only as UPSTREAM_CURRENT — its removal is undone by
#      the fast-forward, byte for byte.
#      THREE RULES THIS SCRIPT HOLDS ITSELF, whatever the prover says (defence
#      in depth -- a prover bug must not be enough to lose bytes):
#        * RAW BYTES. Every working-tree file (tracked and untracked) is hashed
#          twice, as git stores it (clean filters, eol conversion) and with
#          `--no-filters`. If the two differ, OR any conversion is in effect
#          for the path (`check-attr` text/eol/filter/ident/
#          working-tree-encoding set, the legacy `crlf` attribute in any
#          state, or core.autocrlf `input` or any boolean true (yes/on/1; a
#          malformed value fails closed) -- equal
#          hashes do not prove the checkout writes the same bytes: an LF file
#          under `eol=crlf` comes back CRLF), the stored blob is checked back
#          out at that path (`cat-file --filters`); if that does not reproduce
#          the raw bytes -- a lossy `filter=` driver, a mixed-EOL file, that LF
#          file -- neither the proof nor the snapshot can give them back:
#          abstain `filtered_content: <path>`. A pure CRLF<->LF conversion
#          round-trips and is restored. Checked at every look.
#        * STABLE ATTRIBUTES. That round trip is only meaningful under the
#          attributes the restore and the fast-forward will use. So when a
#          `.gitattributes` is itself dirty, or `git diff --name-only HEAD
#          <upstream> -- '**/.gitattributes'` is non-empty (or unreadable),
#          and ANY file the restore would write or unlink (deletions exempt:
#          the fast-forward re-deletes them) has a conversion in effect under
#          the current attributes OR the tip's (`check-attr --source`), abstain
#          `attributes_unstable`. A repo whose attributes change upstream is
#          thereby held only for converted files, not for good.
#        * OCCUPIED PARENTS. A leading component of ANY path the restore
#          touches that is a non-directory or a symlink on disk (an ignored
#          file `a` where git has `a/b`) would be replaced by the restore, and
#          no snapshot holds it: abstain `parent_path_occupied: <path>`.
#        * OCCUPIED DELETIONS. A deletion (`?D`, `D?`) is a deletion only when
#          nothing is at the path: an ignored file, or a directory holding only
#          ignored files, is invisible to status and would be overwritten or
#          removed by the restore -- abstain `deleted_path_occupied: <path>`.
#        * THE DEFAULT BRANCH. "Restorable anywhere" presumes that a
#          fast-forward reproducing the bytes CAN follow. That holds only on
#          the upstream default branch: on a feature branch a deliberate
#          `git checkout <old> -- file` revert would be discarded and, 14 days
#          later, reaped. So when ANY file to be restored or unlinked lies
#          outside the provisioner footprint (.claude/commands/**,
#          .claude/skills/**), the checkout must be ON the default branch
#          (origin/HEAD, else origin/main, else origin/master -- the
#          classifier's order) -- else abstain `not_on_default_branch` -- AND
#          HEAD must be an ancestor of origin/<default> (`merge-base
#          --is-ancestor`), since a local default branch carrying commits
#          upstream lacks gets no fast-forward either -- else abstain
#          `no_fast_forward_follows`. That makes a fast-forward POSSIBLE, not
#          certain: the classifier runs after the restore and may still
#          abstain (or the fast-forward may be refused). A row whose restore
#          ran but which did not end FAST_FORWARDED, RETURNED or NO_CHANGE
#          (level with upstream: the restored bytes are the tip's) carries
#          `residue_restored_no_ff: true`, and its reason names the snapshot
#          ref that holds the pre-restore bytes.
#        * SYMLINKS. A tracked entry whose index or working-tree mode is a
#          symlink (120000), or an untracked symlink, abstains
#          `symlink_entry: <path>`: the residue reaper refuses every symlink
#          entry, so a restored one would sit in a snapshot nothing can
#          retire. (The fingerprint hashes a symlink as its LINK TEXT, as
#          `stash create` and `update-index` store it, should one appear
#          between looks.)
#      Each restored file's pre-restore working-tree blob id is logged
#      (`residue_blobs`, "<blob> <path>", "deleted <path>" for a deletion), so
#      the working state can be rebuilt from the object store even after the
#      snapshot ref is reaped (while the objects survive gc).
#      Order: census + fingerprint (every tracked record with its INDEX blob,
#      every working-tree and untracked blob), provenance, re-census +
#      re-fingerprint (unchanged), snapshot = `git stash create` (HEAD ^1,
#      index ^2) plus, when untracked files are to be removed, an untracked
#      parent ^3 built in a scratch index — the `git stash push -u` shape —
#      verified to hold exactly the proven working-tree, index and untracked
#      blobs, `update-ref` the snapshot, re-census + re-fingerprint once more,
#      then ONE `git restore --source=HEAD --staged --worktree` over exactly the
#      tracked paths and an explicit unlink of exactly the untracked ones.
#      The fingerprint covers the untracked NAMES (sorted) as well as their
#      blob ids, so a rename a -> b between two looks is a changed tree.
#      THE WINDOWS THE SNAPSHOT CANNOT CLOSE, both guarded ONLY by the
#      caller's QUIET quiesce gate (/return-to-main runs this under
#      machine-quiesce-check.sh's QUIET verdict, or the per-repo override's idle
#      test) -- no check inside this script can reach either:
#        * anything written between the last look (the final re-census +
#          re-fingerprint) and the restore/unlink is NOT in the snapshot, and
#          the restore overwrites it;
#        * an ABA change DURING the prover run -- a file changed and changed
#          back between the first look and the second -- leaves h1 == h2, so
#          the proof may have read content the fingerprints never saw. The
#          proof then describes bytes that are no longer there, but the
#          restore still acts only on bytes the fingerprints DID see.
#      No ref, no restore. Exit 1 (a UNIQUE file), 3 (UNKNOWN), a timeout or
#      anything else is an abstention with the reason logged.
#
# ---------------------------------------------------------------------------
# THE DECISION TABLE — the interface, and the whole of the behaviour
#
#   classifier exit / state                     action        log `action`
#   ------------------------------------------  ------------  ------------------
#   0 LANDED_DUPLICATE, on a feature branch     switch + FF   RETURNED
#   0 LANDED_DUPLICATE, already on default,     FF only       FAST_FORWARDED
#     behind > 0
#   0 LANDED_DUPLICATE, already on default,     nothing       NO_CHANGE
#     behind 0
#   0 but HEAD moved / tree dirty / op in       nothing       ABSTAINED
#     progress / index.lock / snapshot failed
#   0 but the default branch is checked out     nothing       ABSTAINED
#     in another worktree of this repo
#   0 but the upstream ref is not origin/<b>    nothing       ABSTAINED
#   1 UNIQUE_WIP                                nothing       ABSTAINED
#   1/2 + --adjudicated-landed REPO=HEAD, clean as for 0      (as for 0),
#     and ZERO merge commits ahead                            verdict_source
#                                                             adjudicated
#   1/2 + --adjudicated-landed REPO=<not HEAD>  nothing       ABSTAINED
#     or a dirty tree                                         (adjudicated_*)
#   1/2 + --adjudicated-landed REPO=HEAD, but   nothing       ABSTAINED
#     >= 1 merge commit ahead, or uncounted                   (adjudication_refused_*)
#   2 MIXED                                     nothing       ABSTAINED
#   3 INCOMPLETE (adjudication or not)          nothing       ABSTAINED
#   4 usage error from the classifier           nothing       ABSTAINED
#   --restore-residue: provenance exit != 0,    no restore;   (then classified;
#     list != census, unproven untracked/...    logged        dirty => ABSTAINED)
#   --restore-residue: the restore itself       nothing more  FAILED
#     errored after the snapshot
#   `.git` is a FILE, or absent                 nothing       (not a candidate)
#   any of the above, under --dry-run           nothing       WOULD_*
#
# ---------------------------------------------------------------------------
# THE OS THIS JOB REQUIRES
#
# THE SWEEPER ITSELF IS OS-AGNOSTIC BY SUBSTANCE: bash 4+ and git, nothing else.
# It runs on Linux, on macOS, and under Git Bash on Windows. It reads no Windows
# registry, calls no service manager, and spawns no PowerShell. Declared here
# the way the other operator tooling declares its OS, because the honest
# declaration is "any" and a reader must be able to see that it was decided
# rather than skipped. (The fetch timeout uses `timeout`, then Homebrew's
# `gtimeout`, then a pure-bash watchdog, because macOS ships neither.)
#
# THE SCHEDULE LIVES INSIDE QONTINUI — the runner owns it, on every OS.
# Operator ruling 2026-09-13 (plan 2026-09-13-nightly-return-to-main-sweep): no
# Windows Task Scheduler job, systemd timer or cron entry. Users may run Windows,
# macOS or Linux, and nothing in Qontinui may need an external system to work.
# The previous arm here — a systemd/cron installer plus a doctor probe for a
# Windows scheduled task nobody had registered — reproduced §2.4's finding one
# layer up (a remediation installed on one OS and silent on the others), and the
# installer is deleted. So:
#
#   * The nightly run is a runner scheduler `RemoteAgent` task named
#     `return-to-main`, registered and inspected by
#     `scripts/schedule-return-to-main.sh` (`--install` / `--check` /
#     `--uninstall`, driven by `/return-to-main --install`). Its session runs the
#     `/return-to-main` skill, which proves the machine quiet, calls THIS script,
#     and adjudicates what this script abstains on. This script stays the only
#     thing that mutates a checkout.
#   * `scripts/capability-doctor.sh` carries this as the mechanism
#     `return_to_main_sweep` and probes the RUNNER's scheduler for that task, so
#     a box that has no registered task says INOPERATIVE-ON-THIS-MACHINE naming
#     `/return-to-main --install`, and an unreachable runner reads UNKNOWN —
#     never exiting 0 into nothing.
#
# RELATIONSHIP TO `scripts/nightly-pull-all.ps1` — IT SITS BESIDE IT.
# That script runs `claude -p "/pull-all"`: a model in the loop, able to resolve
# merge conflicts, costing tokens, and with a blast radius this job deliberately
# does not have. This one is deterministic, model-free, free, and never merges
# anything that is not a fast-forward. Three reasons not to call it or replace
# it: (a) they answer different questions — "reconcile divergent repos" versus
# "stop treating a ghost as work"; (b) §2.4 established `nightly-pull-all.ps1`
# has NEVER run on the operator box, so a sweeper that delegated to it would
# inherit its zero-install status, which is the exact failure this phase guards
# against; (c) it is PowerShell, which would put a Windows-shaped dependency
# back into the middle of the one arm this phase exists to give Linux. Nothing
# here deletes or edits it, and installing this sweeper does not install it.
#
# ---------------------------------------------------------------------------
# THE LOG
#
# JSON Lines, append-only, one object per line, at
# `<workspace-root>/.dev-logs/return-to-main-sweep.log` by default (the
# `.dev-logs/` convention §2.4 itself cites when it looks for
# `nightly-pull-all.log` and does not find it). `--log` overrides. Schema 2
# (2026-09-13): `upstream_age` was renamed `upstream_tip_age` and the fetch,
# adjudication and residue fields were added.
#
#   {"ts":…,"event":"run_start","schema":2,"host":…,"root":…,
#    "mode":"act"|"dry_run","fetch":bool,"run_from":…,"source_dir":…,
#    "copy":bool,"adjudicated":[{"repo":…,"sha":…}],"evidence":…,
#    "evidence_sha256":…,"restore_residue":[…],"dirty_provenance":…,…}
#   {"ts":…,"event":"decision","checkout":…,"repo":…,"branch":…,
#    "verdict":…,"verdict_source":"classifier"|"adjudicated"|"adjudicated_superseded",
#    "classifier_exit":N,"action":…,"reason":…,
#    "default_branch":…,"upstream_ref":…,"upstream_tip_age":…,
#    "upstream_fetched_at":…,"upstream_fetched_at_source":…,"upstream_tip":…,
#    "ahead":N,"behind":N,"head_before":…,"head_after":…,
#    "wip_ref":…,"wip_commit":…,"floor":true,
#    "fetch_requested":bool,"fetched":bool,"fetch_error":…,
#    "adjudicated_sha":…,"evidence":…,"evidence_sha256":…,
#    "residue_outcome":"not_requested"|"nothing_to_restore"|"abstained"|
#                      "would_restore"|"restored"|"failed",
#    "residue_reason":…,"residue_provenance_exit":N,"residue_files":[…],
#    "residue_untracked":[…],"residue_blobs":["<blob> <path>"|"deleted <path>",…],
#    "residue_restored_no_ff":bool,
#    "residue_wip_ref":…,"residue_wip_commit":…,
#    "retention_gate":"pending"|null}
#   `retention_gate` is "pending" exactly when the row names a snapshot that
#   exists (`wip_ref` or `residue_wip_ref` non-null): this script never talks
#   to coord, so the retention gate the contract requires is registered by the
#   caller (/return-to-main Step 4a) or, failing that, by its reconciliation (Step 4b).
#   {"ts":…,"event":"run_end","examined":N,"returned":N,"fast_forwarded":N,
#    "no_change":N,"abstained":N,"failed":N,"fetched":N,"fetch_failed":N,
#    "adjudicated_acted":N,"residue_restored":N,"unmatched_repo_args":[…],
#    "elapsed_sec":N}
#
# Every candidate produces exactly one `decision` line, whatever happened —
# that is safety property 6. The log is rotated to `<log>.1` past
# $QONTINUI_RTM_LOG_MAX_BYTES (default 2 MiB) so an unattended timer cannot fill
# a volume; one generation is kept, because the durable record of an ACTION is
# the refs/wip/ ref and the branch ref, not this file.
#
# USAGE
#   return-to-main-sweep.sh [options]
#
# OPTIONS
#   --dry-run          decide and log, touch no working tree, index, branch or
#                      refs/wip/ ref. Every action becomes WOULD_RETURN /
#                      WOULD_FAST_FORWARD, and a residue restore WOULD_RESTORE.
#                      Combined with --fetch it still FETCHES (a fetch writes
#                      only refs/remotes/origin/* and FETCH_HEAD), so a shadow
#                      run judges against the same refs a live run would.
#   --fetch            fetch each candidate's origin before classifying it
#                      (explicit refspec, prompts disabled; see 1b).
#   --fetch-timeout N  per-candidate fetch wall-clock limit in seconds
#                      (default 120).
#   --adjudicated-landed REPO=SHA
#                      treat REPO as LANDED_DUPLICATE while its HEAD is exactly
#                      SHA (full 40- or 64-hex), its tree is clean and it is
#                      ahead of its upstream by no merge commit, when the
#                      classifier said UNIQUE_WIP or MIXED. Repeatable. Requires
#                      --evidence. See 1d.
#   --adjudicated-superseded REPO=SHA
#                      as --adjudicated-landed, with every identical safety
#                      check, but records SUPERSEDED_BY_UPSTREAM rather than
#                      claiming a land that never happened. This is the correct
#                      argument for `land-evidence.sh` exit 5. Repeatable;
#                      requires --evidence; a repo may not be given both ways.
#                      See 1d'.
#   --evidence FILE    the evidence the adjudication was made on (e.g. the
#                      `land-evidence.sh` output). Must be a readable file; its
#                      path and sha256 are logged with every adjudicated row.
#   --restore-residue REPO
#                      restore REPO's changed tracked files (staged or not)
#                      and remove its untracked files when
#                      `dirty-provenance.sh` proves every one of them already
#                      on the upstream default branch, or residue.
#                      Repeatable. See 1e.
#   --root DIR         workspace root (default: resolved; $QONTINUI_ROOT wins).
#   --log FILE         JSONL log path (default <root>/.dev-logs/
#                      return-to-main-sweep.log). `--log -` disables the log —
#                      for tests only; a scheduled run with no log is the shape
#                      safety property 6 forbids.
#   --only REPO        restrict to this checkout's directory name. Repeatable.
#   --max N            cap on candidates examined (default 100). A cap that
#                      bites is reported in the run_end record and on stdout.
#   --json             machine summary on stdout instead of the table.
#   -h, --help         this text.
#
# ENVIRONMENT
#   QONTINUI_CLASSIFY_BIN            the classifier (default: beside this script)
#   QONTINUI_DIRTY_PROVENANCE_BIN    dirty-provenance.sh (default: beside this
#                                    script; absent => every residue restore
#                                    abstains)
#   QONTINUI_RTM_PROVENANCE_TIMEOUT  seconds allowed for one dirty-provenance.sh
#                                    run (default 300)
#   QONTINUI_RTM_LOG_MAX_BYTES       log rotation threshold (default 2 MiB)
#   QONTINUI_RTM_REEXEC=1            INTERNAL: set by the run-from-copy re-exec.
#                                    Setting it yourself runs IN PLACE, which is
#                                    exactly the hazard 1a removes.
#
# EXIT CODES
#   0  ran to completion. Abstentions are NORMAL and do not affect this — a
#      sweep that abstained on everything still exits 0, which is why the log,
#      not the exit code, is the thing to read.
#   1  ran, but at least one INTENDED action failed (a switch, fast-forward or
#      residue restore that was attempted and errored). The repositories are
#      left as git left them; nothing is retried or forced.
#   2  refused to run at all: no workspace root, no git, no classifier, bad
#      option (including an --adjudicated-landed without --evidence), or the
#      private copy to run from could not be built. Nothing was examined.
# ---- END HELP

set -u

_rtm_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_rtm_self="$_rtm_dir/$(basename "${BASH_SOURCE[0]}")"
# The ORIGINAL arguments, kept for the run-from-copy re-exec (the option loop
# below consumes "$@").
_RTM_ARGV=("$@")

# -- Exit cleanup: the private copy (1a) and the scratch dir ------------------
# Registered FIRST, so every exit path below -- a refusal included -- removes
# what this process made. The copy is removed only when it is provably the copy
# the parent made: it must be the directory this script runs from AND carry the
# parent's marker. A stray QONTINUI_RTM_COPY_DIR can never aim this at anything
# else.
_RTM_SOURCE_DIR="$_rtm_dir"
_RTM_IS_COPY=0
_RTM_COPY_TO_REMOVE=""
RTM_SCRATCH=""
if [ "${QONTINUI_RTM_REEXEC:-}" = 1 ]; then
  [ -n "${QONTINUI_RTM_SOURCE_DIR:-}" ] && _RTM_SOURCE_DIR="$QONTINUI_RTM_SOURCE_DIR"
  if [ -n "${QONTINUI_RTM_COPY_DIR:-}" ] && [ "$QONTINUI_RTM_COPY_DIR" = "$_rtm_dir" ] \
     && [ -f "$_rtm_dir/.qontinui-rtm-copy" ]; then
    _RTM_IS_COPY=1
    _RTM_COPY_TO_REMOVE="$_rtm_dir"
  fi
fi
_rtm_on_exit() {
  [ -n "$RTM_SCRATCH" ] && rm -rf -- "$RTM_SCRATCH" 2>/dev/null
  [ -n "$_RTM_COPY_TO_REMOVE" ] && rm -rf -- "$_RTM_COPY_TO_REMOVE" 2>/dev/null
  return 0
}
trap _rtm_on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# -- Isolate the scoped git queries ------------------------------------------
# Same reasoning as `classify-branch-state.sh` and `wip-custody-record.sh`, and
# it matters more here than in either: an INHERITED GIT_DIR makes `git -C
# <path>` answer about a DIFFERENT repository, and this script does not merely
# report — it would then write a ref into, and CHECK OUT A BRANCH IN, a
# repository it was never pointed at. git exports GIT_DIR into every hook's
# environment and this workspace installs per-clone hooks, so the poison is
# ordinary rather than exotic.
if [ -r "$_rtm_dir/lib/git-scope.sh" ]; then
  # shellcheck source=lib/git-scope.sh
  . "$_rtm_dir/lib/git-scope.sh"
fi
if declare -F git_scope_strip >/dev/null 2>&1; then
  git_scope_strip
elif [ -n "${GIT_DIR+s}${GIT_WORK_TREE+s}${GIT_COMMON_DIR+s}" ]; then
  echo "return-to-main-sweep: FATAL - lib/git-scope.sh is not usable ($_rtm_dir/lib/git-scope.sh) AND this process carries GIT_DIR / GIT_WORK_TREE / GIT_COMMON_DIR, so every query below would answer about ANOTHER repository -- and the switch would happen there. Refusing." >&2
  exit 2
fi

# -- Native path for `git -C` ------------------------------------------------
# Same MSYS boundary as the sibling scripts: candidates are POSIX-spelled and a
# native git.exe cannot open `/d/<root>/...` under MSYS_NO_PATHCONV=1.
if [ -r "$_rtm_dir/lib/native-path.sh" ]; then
  # shellcheck source=lib/native-path.sh
  . "$_rtm_dir/lib/native-path.sh"
fi
if ! declare -F native_path_w >/dev/null 2>&1; then
  if command -v cygpath >/dev/null 2>&1; then
    echo "return-to-main-sweep: FATAL - lib/native-path.sh is not usable ($_rtm_dir/lib/native-path.sh) AND cygpath is present, so this is an MSYS box: the checkout would reach git.exe in a spelling it cannot open. Refusing." >&2
    exit 2
  fi
  native_path_w() { printf '%s\n' "$1"; }
fi

# ---------------------------------------------------------------------------
# args

DRY_RUN=0
ROOT_ARG=""
LOG_ARG=""
MAX=100
JSON_OUT=0
ONLY=()
FETCH=0
FETCH_TIMEOUT=120
ADJ_REPOS=()
ADJ_SHAS=()
ADJ_KINDS=()   # "landed" | "superseded" -- the WORD the row will record
EVIDENCE=""
RESIDUE_REPOS=()

_usage_err() { echo "return-to-main-sweep: $1 (see --help)" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --root) shift; [ $# -gt 0 ] || _usage_err "--root needs a directory"
            ROOT_ARG="$1"; shift ;;
    --log)  shift; [ $# -gt 0 ] || _usage_err "--log needs a path"
            LOG_ARG="$1"; shift ;;
    --only) shift; [ $# -gt 0 ] || _usage_err "--only needs a repo name"
            ONLY+=("$1"); shift ;;
    --max)  shift; [ $# -gt 0 ] || _usage_err "--max needs a number"
            MAX="$1"; shift
            case "$MAX" in ''|*[!0-9]*) _usage_err "--max needs a number" ;; esac ;;
    --json) JSON_OUT=1; shift ;;
    --fetch) FETCH=1; shift ;;
    --fetch-timeout) shift; [ $# -gt 0 ] || _usage_err "--fetch-timeout needs a number of seconds"
            FETCH_TIMEOUT="$1"; shift
            case "$FETCH_TIMEOUT" in ''|*[!0-9]*|0) _usage_err "--fetch-timeout needs a positive number of seconds" ;; esac ;;
    # Two spellings, ONE code path and ONE set of safety checks. They differ in
    # exactly one thing: the word the decision row records. `--adjudicated-landed`
    # asserts the branch's content REACHED the upstream; `--adjudicated-superseded`
    # asserts the upstream MOVED PAST it. Those are different claims about the
    # world, and a ledger that cannot tell them apart will eventually be used to
    # prove something false -- which is what happened on 2026-09-16, when the only
    # argument able to move a superseded branch was the one that says "landed"
    # (plan 2026-09-16-land-evidence-has-no-superseded-arm, D1/Defect 2).
    --adjudicated-landed|--adjudicated-superseded)
            _a_flag="$1"; _a_kind=landed
            [ "$_a_flag" = --adjudicated-superseded ] && _a_kind=superseded
            shift; [ $# -gt 0 ] || _usage_err "$_a_flag needs REPO=SHA"
            case "$1" in *=*) ;; *) _usage_err "$_a_flag needs REPO=SHA, got '$1'" ;; esac
            _a_repo="${1%%=*}"; _a_sha="${1#*=}"; _a_sha="${_a_sha,,}"
            case "$_a_repo" in ''|*/*|*\\*) _usage_err "$_a_flag: '$_a_repo' is not a checkout directory name" ;; esac
            # A FULL object name only. An abbreviation can be ambiguous, and a
            # prefix match against HEAD would let a short string authorise an
            # act on a commit nobody named.
            case "${#_a_sha}" in 40|64) ;; *) _usage_err "$_a_flag: '$_a_sha' is not a full 40- or 64-hex object name" ;; esac
            case "$_a_sha" in *[!0-9a-f]*) _usage_err "$_a_flag: '$_a_sha' is not hexadecimal" ;; esac
            for _i in ${ADJ_REPOS[@]+"${!ADJ_REPOS[@]}"}; do
              if [ "${ADJ_REPOS[$_i]}" = "$_a_repo" ] && [ "${ADJ_SHAS[$_i]}" != "$_a_sha" ]; then
                _usage_err "$_a_flag names $_a_repo twice with different SHAs"
              fi
              # A repo adjudicated BOTH ways is an incoherent claim, not a
              # precedence question: refuse it rather than pick a winner.
              if [ "${ADJ_REPOS[$_i]}" = "$_a_repo" ] && [ "${ADJ_KINDS[$_i]}" != "$_a_kind" ]; then
                _usage_err "$_a_repo is adjudicated both landed and superseded; those are contradictory claims about the same branch"
              fi
            done
            ADJ_REPOS+=("$_a_repo"); ADJ_SHAS+=("$_a_sha"); ADJ_KINDS+=("$_a_kind"); shift ;;
    --evidence) shift; [ $# -gt 0 ] || _usage_err "--evidence needs a file"
            [ -z "$EVIDENCE" ] || _usage_err "--evidence given twice; one evidence file per run"
            EVIDENCE="$1"; shift ;;
    --restore-residue) shift; [ $# -gt 0 ] || _usage_err "--restore-residue needs a repo name"
            RESIDUE_REPOS+=("$1"); shift ;;
    # Print the header up to the sentinel rather than a hard-coded line range,
    # which silently truncated on every header edit in the sibling scripts.
    -h|--help) sed -n '2,/^# ---- END HELP/p' "$0" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'; exit 0 ;;
    *) _usage_err "unknown option $1" ;;
  esac
done

# An adjudication is a judgment made OUTSIDE this script; the evidence it was
# made on is what makes it auditable. One without the other is refused.
if [ "${#ADJ_REPOS[@]}" -gt 0 ] && [ -z "$EVIDENCE" ]; then
  _usage_err "--adjudicated-landed / --adjudicated-superseded needs --evidence <file>: an adjudication with no recorded evidence cannot be audited"
fi
if [ -n "$EVIDENCE" ]; then
  [ "${#ADJ_REPOS[@]}" -gt 0 ] || _usage_err "--evidence is only meaningful with --adjudicated-landed or --adjudicated-superseded"
  [ -f "$EVIDENCE" ] && [ -r "$EVIDENCE" ] || _usage_err "--evidence '$EVIDENCE' is not a readable file"
  EVIDENCE="$(cd "$(dirname "$EVIDENCE")" && pwd)/$(basename "$EVIDENCE")"
fi

command -v git >/dev/null 2>&1 || {
  echo "return-to-main-sweep: git is not on PATH -- nothing was examined." >&2
  exit 2
}

# -- The Phase 1 dependency, checked explicitly ------------------------------
# Not a convenience check. This job's entire licence to act is the classifier's
# verdict; without it the only classification available is the pre-Phase-1 one
# that reads every landed duplicate as peer work. A missing classifier must be a
# REFUSAL, never a fallback to a cheaper test.
CLASSIFIER="${QONTINUI_CLASSIFY_BIN:-$_rtm_dir/classify-branch-state.sh}"
[ -f "$CLASSIFIER" ] || {
  echo "return-to-main-sweep: the Phase 1 classifier is not at $CLASSIFIER. This job's only licence to act is its verdict; there is no cheaper test to fall back to (that is the whole finding). Refusing -- nothing was examined." >&2
  exit 2
}

# The residue prover (plan 2026-09-13 Phase 3b), located exactly as the
# classifier is. OPTIONAL: its absence disables only --restore-residue, and
# every requested restore then abstains with that reason.
DIRTY_PROV="${QONTINUI_DIRTY_PROVENANCE_BIN:-$_rtm_dir/dirty-provenance.sh}"
[ -f "$DIRTY_PROV" ] || DIRTY_PROV=""

# -- Run from a copy (1a) — the parent half ----------------------------------
# See "UNATTENDED HARDENING" 1a. Everything the run EXECUTES is copied before
# anything is examined; after the exec, a fast-forward of this script's own
# checkout changes files this process no longer reads.
if [ "${QONTINUI_RTM_REEXEC:-}" != 1 ]; then
  _rtm_tmp=""
  _rtm_fail_copy() {
    echo "return-to-main-sweep: cannot build the private copy to run from ($1). Running in place would let a fast-forward of this script's own checkout change the code mid-run, so the run is refused -- nothing was examined." >&2
    if [ -n "$_rtm_tmp" ] && [ -f "$_rtm_tmp/.qontinui-rtm-copy" ]; then rm -rf -- "$_rtm_tmp" 2>/dev/null; fi
    exit 2
  }
  _rtm_tmp="$(mktemp -d "${TMPDIR:-/tmp}/rtm-copy.XXXXXX" 2>/dev/null)" || _rtm_fail_copy "mktemp -d failed"
  [ -n "$_rtm_tmp" ] || _rtm_fail_copy "mktemp -d returned nothing"
  _rtm_tmp="$(cd "$_rtm_tmp" && pwd)" || _rtm_fail_copy "cannot enter the temp dir"
  printf 'return-to-main-sweep run-from-copy; parent pid %s; source %s\n' "$$" "$_rtm_dir" \
    > "$_rtm_tmp/.qontinui-rtm-copy" || _rtm_fail_copy "cannot write the marker"
  cp -p "$_rtm_self" "$_rtm_tmp/return-to-main-sweep.sh" || _rtm_fail_copy "copying the sweep $_rtm_self"
  cp -p "$CLASSIFIER" "$_rtm_tmp/classify-branch-state.sh" || _rtm_fail_copy "copying the classifier $CLASSIFIER"
  if [ -n "$DIRTY_PROV" ]; then
    cp -p "$DIRTY_PROV" "$_rtm_tmp/dirty-provenance.sh" || _rtm_fail_copy "copying $DIRTY_PROV"
  fi
  if [ -d "$_rtm_dir/lib" ]; then
    cp -Rp "$_rtm_dir/lib" "$_rtm_tmp/lib" || _rtm_fail_copy "copying lib/"
  fi
  export QONTINUI_RTM_REEXEC=1 QONTINUI_RTM_COPY_DIR="$_rtm_tmp" QONTINUI_RTM_SOURCE_DIR="$_rtm_dir"
  # The copies ARE the classifier and the prover now; an override must not
  # send the child back to the originals it was copied from.
  unset QONTINUI_CLASSIFY_BIN QONTINUI_DIRTY_PROVENANCE_BIN
  exec bash "$_rtm_tmp/return-to-main-sweep.sh" ${_RTM_ARGV[@]+"${_RTM_ARGV[@]}"}  # skill-self-path-ok: the run-from-copy re-exec (Phase 1a) -- $_rtm_tmp is the mktemp -d copy of THIS directory ($_rtm_dir, from BASH_SOURCE), not a rooted path
  _rtm_fail_copy "exec failed"
fi

# ---------------------------------------------------------------------------
# workspace-root resolution
#
# Identical probe to `scan-worktree-wip.sh` and `.claude/hooks/worktree-create.sh`
# — see the former's header for why neither a ${BASH_SOURCE[0]}-relative nor a
# git-toplevel derivation works here (<root>/.claude is a SYMLINK into this
# repo, and the workspace root is a multi-repo umbrella that is not itself a git
# repo). Kept as a copy rather than sourced so this script runs standalone from
# any cwd, including from a runner-scheduled session with no shell profile. Probed from the
# SOURCE directory, never the private copy under /tmp.
_rtm_is_root() {
  case "$1" in ""|/|//) return 1 ;; esac
  [ -e "$1/qontinui-claude-config/.git" ] && [ -f "$1/.claude/settings.json" ]
}
_rtm_resolve_root() {
  local d
  if [ -n "$ROOT_ARG" ]; then
    # An EXPLICIT --root is honoured without the marker probe: the hermetic test
    # builds a fixture workspace that has no .claude/settings.json, and a
    # scheduled unit may be pointed at a non-standard layout. An explicit path
    # that is not a directory is still a refusal.
    [ -d "$ROOT_ARG" ] || return 1
    (cd "$ROOT_ARG" && pwd); return 0
  fi
  if [ -n "${QONTINUI_ROOT:-}" ] && _rtm_is_root "$QONTINUI_ROOT"; then
    (cd "$QONTINUI_ROOT" && pwd); return 0
  fi
  for d in "$PWD" "${CLAUDE_PROJECT_DIR:-}" "$_RTM_SOURCE_DIR"; do
    while [ -n "$d" ]; do
      _rtm_is_root "$d" && { (cd "$d" && pwd); return 0; }
      case "$d" in ""|/|//) break ;; esac
      d="${d%/*}"; [ -z "$d" ] && d=/
    done
  done
  return 1
}

ROOT="$(_rtm_resolve_root)" || {
  echo "return-to-main-sweep: cannot resolve the workspace root (pass --root, or set \$QONTINUI_ROOT) -- nothing was examined." >&2
  exit 2
}

# ---------------------------------------------------------------------------
# the log

LOG=""
if [ "$LOG_ARG" = "-" ]; then
  LOG=""
elif [ -n "$LOG_ARG" ]; then
  LOG="$LOG_ARG"
else
  LOG="$ROOT/.dev-logs/return-to-main-sweep.log"
fi

LOG_MAX="${QONTINUI_RTM_LOG_MAX_BYTES:-2097152}"
case "$LOG_MAX" in ''|*[!0-9]*) LOG_MAX=2097152 ;; esac

LOG_STATE="ok"
if [ -n "$LOG" ]; then
  mkdir -p "$(dirname "$LOG")" 2>/dev/null || true
  # One generation only. The durable record of an ACTION is the refs/wip/ ref
  # and the surviving branch ref; this file is the record of the DECISIONS,
  # including the abstentions, which is what a next session needs to read.
  if [ -f "$LOG" ]; then
    _sz="$(wc -c < "$LOG" 2>/dev/null | tr -d ' ')"
    case "$_sz" in ''|*[!0-9]*) _sz=0 ;; esac
    [ "$_sz" -gt "$LOG_MAX" ] && mv -f "$LOG" "$LOG.1" 2>/dev/null
  fi
  # Fail LOUDLY rather than silently: a sweep whose decisions are unrecorded is
  # the shape safety property 6 exists to forbid, so if the log is unwritable
  # the run degrades to --dry-run rather than acting unobserved.
  if ! : >> "$LOG" 2>/dev/null; then
    echo "return-to-main-sweep: cannot append to $LOG -- an unlogged sweep cannot be audited, so this run is downgraded to --dry-run." >&2
    LOG_STATE="unwritable"
    LOG=""
    DRY_RUN=1
  fi
fi

PROV_TIMEOUT="${QONTINUI_RTM_PROVENANCE_TIMEOUT:-300}"
case "$PROV_TIMEOUT" in ''|*[!0-9]*|0) PROV_TIMEOUT=300 ;; esac

# Scratch space for NUL-delimited git output (command substitution drops NUL
# bytes, which would collapse a `-z` status into one unparseable blob -- the
# classifier records the same trap). Removed by the EXIT trap. Its absence
# disables only --restore-residue, which then abstains.
RTM_SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/rtm-scratch.XXXXXX" 2>/dev/null)" || RTM_SCRATCH=""

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || printf 'unknown'; }

# JSON string escaping. Values reaching here are branch names, paths, git
# stderr and classifier reasons; git stderr is the one that can carry a newline
# or a tab, and an unescaped one would split a JSONL record into two.
json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/\t/\\t/g' | tr '\n\r' '  '
}
js()  { printf '"%s"' "$(json_escape "$1")"; }
# Numbers, and `null` for anything that is not one -- never a bare empty value,
# which would make the record unparseable.
jn()  { case "${1:-}" in ''|*[!0-9]*) printf 'null' ;; *) printf '%s' "$1" ;; esac; }
jsn() { [ -n "${1:-}" ] && js "$1" || printf 'null'; }
jb()  { [ "${1:-0}" = 1 ] || [ "${1:-}" = true ] && printf 'true' || printf 'false'; }
ja()  { # <items...> -> JSON array of strings
  local first=1 e
  printf '['
  for e in "$@"; do
    [ "$first" = 1 ] || printf ','
    first=0
    js "$e"
  done
  printf ']'
}

log_line() { [ -n "$LOG" ] && printf '%s\n' "$1" >> "$LOG"; return 0; }

sha256_of() { # <file> -> hex digest, or empty when no tool is available
  local out=""
  if command -v sha256sum >/dev/null 2>&1; then out="$(sha256sum < "$1" 2>/dev/null)"
  elif command -v shasum >/dev/null 2>&1; then out="$(shasum -a 256 < "$1" 2>/dev/null)"
  fi
  printf '%s' "${out%% *}"
}

in_list() { # <needle> <items...>
  local n="$1" e; shift
  for e in "$@"; do [ "$e" = "$n" ] && return 0; done
  return 1
}

adj_sha_for() { # <repo> -> the adjudicated SHA, or return 1
  local i
  for i in ${ADJ_REPOS[@]+"${!ADJ_REPOS[@]}"}; do
    [ "${ADJ_REPOS[$i]}" = "$1" ] && { printf '%s' "${ADJ_SHAS[$i]}"; return 0; }
  done
  return 1
}
# A SEPARATE printing lookup, deliberately. An earlier draft had adj_sha_for set
# ADJ_KIND as a side effect -- but its only caller is
# `ADJ_SHA="$(adj_sha_for "$REPO")"`, so that assignment ran in a command
# substitution SUBSHELL and never reached the caller. The row then recorded
# `adjudicated_kind: null` and the bare word `adjudicated`: the exact defect this
# argument exists to remove, reintroduced silently. Never return this through an
# assignment.
adj_kind_for() { # <repo> -> "landed" | "superseded" | ""
  local i
  for i in ${ADJ_REPOS[@]+"${!ADJ_REPOS[@]}"}; do
    [ "${ADJ_REPOS[$i]}" = "$1" ] && { printf '%s' "${ADJ_KINDS[$i]}"; return 0; }
  done
  return 1
}
adj_flag_of() { # "landed"|"superseded" -> the flag that asked for it
  if [ "$1" = superseded ]; then printf -- '--adjudicated-superseded'
  else printf -- '--adjudicated-landed'; fi
}

# Run "$@" under a wall-clock limit; exit 124 on timeout. GNU/MSYS `timeout`
# first, then Homebrew's `gtimeout`, then a pure-bash watchdog (macOS ships
# neither). The watchdog's own output goes to /dev/null so a lingering `sleep`
# cannot hold a caller's command substitution open until the limit.
_rtm_timeout() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then timeout "$secs" "$@"; return $?; fi
  if command -v gtimeout >/dev/null 2>&1; then gtimeout "$secs" "$@"; return $?; fi
  local pid w rc
  "$@" &
  pid=$!
  ( sleep "$secs"; kill -TERM "$pid" 2>/dev/null ) >/dev/null 2>&1 &
  w=$!
  wait "$pid"; rc=$?
  if kill -0 "$w" 2>/dev/null; then
    kill "$w" 2>/dev/null; wait "$w" 2>/dev/null
  else
    rc=124
  fi
  return "$rc"
}

# ---------------------------------------------------------------------------
# reading JSON (the classifier's, and dirty-provenance.sh's)
#
# bash regex, not a `sed 's/.*"key".*/'`: sed's leading `.*` is greedy and takes
# the LAST occurrence of a key, so a key name appearing inside an earlier value
# (`settled_by` is prose) would shadow the real field. Bash's =~ is leftmost.
jget_s() { # <json> <key> -> string value, or empty
  local re="\"$2\"[[:space:]]*:[[:space:]]*\"([^\"]*)\""
  [[ "$1" =~ $re ]] && printf '%s' "${BASH_REMATCH[1]}"
  return 0
}
jget_n() { # <json> <key> -> bare token (number, true/false, null), or empty
  local re="\"$2\"[[:space:]]*:[[:space:]]*([^,}\"]*)"
  [[ "$1" =~ $re ]] && printf '%s' "${BASH_REMATCH[1]}"
  return 0
}

# ---------------------------------------------------------------------------
# candidate enumeration — PRIMARY checkouts only
#
# Depth 1 under the workspace root, and `.git` must be a DIRECTORY. That single
# test is the whole primary-vs-worktree discrimination and it is exact: git
# writes a `.git` FILE containing `gitdir: …` in every linked worktree. Measured
# on this box 2026-09-04 — the workspace root holds 15 primaries alongside ~80
# `<repo>-wt-*` linked siblings, and the test separates them with no name
# matching and no `git worktree list` spawn.
#
# It is also the safety boundary: an agent worktree is allocated through coord
# (`POST /agents/allocate`), belongs to a session, and is reclaimed by machinery
# that is not this. Nothing here may switch a branch in one.
CANDIDATES=()
shopt -s dotglob nullglob
for _d in "$ROOT"/*; do
  [ -d "$_d" ] || continue
  [ -d "$_d/.git" ] || continue
  if [ "${#ONLY[@]}" -gt 0 ]; then
    _name="${_d##*/}"
    _hit=0
    for _o in "${ONLY[@]}"; do [ "$_o" = "$_name" ] && _hit=1; done
    [ "$_hit" = 1 ] || continue
  fi
  CANDIDATES+=("$_d")
done
shopt -u dotglob nullglob
if [ "${#CANDIDATES[@]}" -gt 1 ]; then
  mapfile -t CANDIDATES < <(printf '%s\n' "${CANDIDATES[@]}" | LC_ALL=C sort)
fi

TOTAL=${#CANDIDATES[@]}
CAPPED=0
if [ "$TOTAL" -gt "$MAX" ]; then
  CAPPED=$((TOTAL - MAX))
  CANDIDATES=("${CANDIDATES[@]:0:$MAX}")
fi

MODE_WORD="act"; [ "$DRY_RUN" = 1 ] && MODE_WORD="dry_run"
START_EPOCH="$(date -u +%s 2>/dev/null || printf 0)"

EVIDENCE_SHA=""
[ -n "$EVIDENCE" ] && EVIDENCE_SHA="$(sha256_of "$EVIDENCE")"

_adj_json="["
for _i in ${ADJ_REPOS[@]+"${!ADJ_REPOS[@]}"}; do
  [ "$_adj_json" = "[" ] || _adj_json+=","
  _adj_json+="{\"repo\":$(js "${ADJ_REPOS[$_i]}"),\"sha\":$(js "${ADJ_SHAS[$_i]}"),\"kind\":$(js "${ADJ_KINDS[$_i]}")}"
done
_adj_json+="]"

log_line "{\"ts\":$(js "$(now_iso)"),\"event\":\"run_start\",\"tool\":\"return-to-main-sweep.sh\",\"schema\":2,\"host\":$(js "$(hostname 2>/dev/null || printf unknown)"),\"root\":$(js "$ROOT"),\"mode\":$(js "$MODE_WORD"),\"candidates\":$(jn "$TOTAL"),\"capped_out\":$(jn "$CAPPED"),\"classifier\":$(js "$CLASSIFIER"),\"dirty_provenance\":$(jsn "$DIRTY_PROV"),\"run_from\":$(js "$_rtm_dir"),\"source_dir\":$(js "$_RTM_SOURCE_DIR"),\"copy\":$(jb "$_RTM_IS_COPY"),\"fetch\":$(jb "$FETCH"),\"fetch_timeout_sec\":$(jn "$FETCH_TIMEOUT"),\"adjudicated\":$_adj_json,\"evidence\":$(jsn "$EVIDENCE"),\"evidence_sha256\":$(jsn "$EVIDENCE_SHA"),\"restore_residue\":$(ja ${RESIDUE_REPOS[@]+"${RESIDUE_REPOS[@]}"}),\"session_id\":$(jsn "${CLAUDE_CODE_SESSION_ID:-}")}"

# ---------------------------------------------------------------------------
# per-checkout work

N_RETURNED=0; N_FF=0; N_NOCHANGE=0; N_ABSTAIN=0; N_FAILED=0; N_EXAMINED=0
N_FETCHED=0; N_FETCH_FAILED=0; N_ADJ_ACTED=0; N_RES_RESTORED=0
ROWS=()
EXAMINED_REPOS=()

# Every read goes through here; every WRITE is spelled out at its call site so
# that `grep -n 'gw '` enumerates the complete set of mutating commands (the
# fetch is the one write with its own spelled-out `git -C` call, because it
# carries its own environment and timeout).
# `--no-optional-locks` on the reads matches `classify-branch-state.sh:_git_c`
# and `scan-worktree-wip.sh:_git_c`, so classification and re-verification never
# fight a peer's index.lock and never rewrite the stat cache in a shared
# checkout.
gr() { git --no-optional-locks -C "$NATIVE" "$@" 2>/dev/null; }
gw() { git -C "$NATIVE" "$@"; }

# One decision record, from the current candidate's variables. Called exactly
# once per candidate, on every path.
emit_decision() {
  log_line "{\"ts\":$(js "$(now_iso)"),\"event\":\"decision\",\"checkout\":$(js "$CO"),\"repo\":$(js "$REPO"),\"branch\":$(jsn "$BRANCH"),\"verdict\":$(jsn "$VERDICT"),\"verdict_source\":$(js "$VERDICT_SOURCE"),\"classifier_exit\":$(jn "$CRC"),\"action\":$(js "$ACTION"),\"reason\":$(js "$REASON"),\"default_branch\":$(jsn "$DEFAULT_BRANCH"),\"upstream_ref\":$(jsn "$UPSTREAM"),\"upstream_tip_age\":$(jsn "$UP_TIP_AGE"),\"upstream_fetched_at\":$(jsn "$UP_FETCHED_AT"),\"upstream_fetched_at_source\":$(jsn "$UP_FETCHED_SRC"),\"upstream_tip\":$(jsn "$UP_TIP"),\"ahead\":$(jn "$AHEAD"),\"behind\":$(jn "$BEHIND"),\"head_before\":$(jsn "$HEAD_BEFORE"),\"head_after\":$(jsn "$HEAD_AFTER"),\"wip_ref\":$(jsn "$WIP_REF"),\"wip_commit\":$(jsn "$WIP_COMMIT"),\"floor\":true,\"fetch_requested\":$(jb "$FETCH"),\"fetched\":$(jb "$FETCHED"),\"fetch_error\":$(jsn "$FETCH_ERR"),\"adjudicated_sha\":$(jsn "$ADJ_SHA"),\"adjudicated_kind\":$(jsn "$ADJ_KIND"),\"evidence\":$(jsn "$ADJ_EVIDENCE"),\"evidence_sha256\":$(jsn "$ADJ_EVIDENCE_SHA"),\"residue_outcome\":$(js "$RES_OUTCOME"),\"residue_reason\":$(jsn "$RES_REASON"),\"residue_provenance_exit\":$(jn "$RES_PROV_RC"),\"residue_files\":$(ja ${RES_FILES[@]+"${RES_FILES[@]}"}),\"residue_untracked\":$(ja ${RES_UNTRACKED[@]+"${RES_UNTRACKED[@]}"}),\"residue_blobs\":$(ja ${RES_BLOBS[@]+"${RES_BLOBS[@]}"}),\"residue_restored_no_ff\":$(jb "$RES_NO_FF"),\"residue_wip_ref\":$(jsn "$RES_WIP_REF"),\"residue_wip_commit\":$(jsn "$RES_WIP_COMMIT"),\"retention_gate\":$( { [ -n "$WIP_REF" ] || [ -n "$RES_WIP_REF" ]; } && js pending || printf 'null')}"
}

# -- 1b: fetch ---------------------------------------------------------------
# The explicit refspec PLUS an empty `--refmap=` is the safety property, not a
# detail. The refspec alone is NOT enough: git still applies the configured
# `remote.origin.fetch` mapping to every ref it fetches ("opportunistic
# remote-tracking update"), so a clone configured with `+refs/heads/*:refs/heads/*`
# had its local branches FORCE-updated by this exact command -- an unpushed
# commit overwritten -- and aborted rc=128 when the checked-out branch existed
# upstream (measured, git 2.55 on Windows, pre-PR review 2026-09-13). The empty
# refmap tells git to ignore the configured refspecs and use only the one on
# the command line, so this writes refs/remotes/origin/* and FETCH_HEAD and
# nothing else, and `--prune` prunes only refs/remotes/origin/*. Prompts are
# disabled at every layer that can raise one (terminal, Git Credential Manager,
# git 2.46+ credential.interactive) so an unattended run fails fast instead of
# waiting on a password nobody will type.
_rtm_fetch() {
  local out rc
  out="$(GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never \
         _rtm_timeout "$FETCH_TIMEOUT" git -C "$NATIVE" -c credential.interactive=never \
           fetch --quiet --prune --no-recurse-submodules --refmap= origin \
           '+refs/heads/*:refs/remotes/origin/*' 2>&1)"; rc=$?
  if [ "$rc" = 0 ]; then
    FETCHED=true; FETCH_ERR=""
    N_FETCHED=$((N_FETCHED + 1))
  else
    FETCHED=false
    if [ "$rc" = 124 ]; then
      FETCH_ERR="timed out after ${FETCH_TIMEOUT}s"
    else
      FETCH_ERR="git fetch exited $rc: ${out:-<no output>}"
    fi
    FETCH_ERR="${FETCH_ERR:0:400}"
    N_FETCH_FAILED=$((N_FETCH_FAILED + 1))
  fi
}

# -- 1e: residue restore -----------------------------------------------------
# The census of the dirty tree, from `status --porcelain=v2 -z -uall`:
#   _RES_MOD   every changed TRACKED path (index side, worktree side or both);
#   _RES_XY    its two-character XY, parallel to _RES_MOD;
#   _RES_WT    the _RES_MOD paths that still have a working-tree file;
#   _RES_UNTR  every untracked path;
#   _RES_SIG   the tracked records themselves (XY, modes, HEAD and INDEX blob
#              ids, path) -- so a staged blob that moves is a changed census.
# Returns 1 with _RES_WHY on any entry a residue restore does not handle. The
# admitted XY shapes are the ones dirty-provenance.sh classifies by blob: an
# index side of `.` `M` `A` `D` combined with a worktree side of `.` `M` `D`.
# A type change, a rename or copy, an unmerged entry, a submodule or a path
# carrying a newline or a carriage return is refused -- no blob proof covers it.
_res_census() {
  _RES_MOD=(); _RES_XY=(); _RES_WT=(); _RES_UNTR=(); _RES_SIG=""; _RES_WHY=""
  local st="$RTM_SCRATCH/status" rec expect_orig=0 xy sub path _mi _mw
  if ! gr status --porcelain=v2 -z -uall > "$st"; then
    _RES_WHY="status_failed: git status failed in this checkout"; return 1
  fi
  while IFS= read -r -d '' rec; do
    if [ "$expect_orig" = 1 ]; then expect_orig=0; continue; fi
    case "$rec" in
      '# '*|'! '*) ;;
      '? '*)
        path="${rec#? }"
        # A newline or a CR: the snapshot check's `cat-file --batch-check`
        # reads line-wise and strips a trailing CR, so neither can be verified.
        case "$path" in *$'\n'*|*$'\r'*) _RES_WHY="unsupported_path: an untracked path contains a newline or a carriage return"; return 1 ;; esac
        if _res_parent_blocked "$path"; then
          _RES_WHY="parent_path_occupied: $path -- $_RES_BLOCKED is a file or symlink on disk where the path needs a directory"; return 1
        fi
        if [ -L "$CO/$path" ]; then
          _RES_WHY="symlink_entry: $path is an untracked symlink; a symlink is never restored as residue (the reaper refuses every symlink entry)"; return 1
        fi
        _RES_UNTR+=("$path") ;;
      '1 '*)
        xy="${rec:2:2}"; sub="${rec:5:4}"
        # `1 XY sub mH mI mW hH hI <path>`: strip the eight leading fields
        # rather than split, because a path may contain spaces.
        path="${rec#* * * * * * * * }"
        case "$path" in *$'\n'*|*$'\r'*) _RES_WHY="unsupported_path: a modified path contains a newline or a carriage return"; return 1 ;; esac
        case "$sub" in N*) ;; *) _RES_WHY="submodule_entry: $path is a submodule, which a residue restore never touches"; return 1 ;; esac
        case "$xy" in
          .M|.D|M.|MM|MD|A.|AM|AD|D.) ;;
          *T*) _RES_WHY="type_changed: $path changed type (file/symlink, status '$xy'); not a content modification"; return 1 ;;
          *)   _RES_WHY="unsupported_status: $path has status '$xy', which no blob-provenance proof covers"; return 1 ;;
        esac
        # A leading component that is a non-directory or a symlink (an ignored
        # file `a` where git has `a/b`) would be replaced by the restore, and
        # no snapshot holds it -- for a deletion and for every other entry.
        if _res_parent_blocked "$path"; then
          _RES_WHY="parent_path_occupied: $path -- $_RES_BLOCKED is a file or symlink on disk where git has a directory, which a restore would replace"; return 1
        fi
        read -r _ _ _ _ _mi _mw _ <<< "${rec%%$'\n'*}"
        case " $_mi $_mw " in *" 120000 "*)
          _RES_WHY="symlink_entry: $path is a symlink in the index or the working tree; a symlink is never restored as residue (the reaper refuses every symlink entry)"; return 1 ;;
        esac
        [ -L "$CO/$path" ] && { _RES_WHY="symlink_entry: $path is a symlink on disk; a symlink is never restored as residue"; return 1; }
        _RES_MOD+=("$path"); _RES_XY+=("$xy")
        # No working-tree file when the worktree side deletes it (`?D`) or the
        # index side does and the worktree agrees (`D.`) -- and a deletion is a
        # deletion only when NOTHING is at the path. An ignored file (a `D.`
        # whose file is back) or a directory of ignored files (`.D`) is
        # invisible to status; the restore would overwrite or remove it.
        case "$xy" in
          ?D|D?)
            if [ -e "$CO/$path" ] || [ -L "$CO/$path" ]; then
              _RES_WHY="deleted_path_occupied: $path is deleted in git but something (an ignored file or a directory) occupies the path on disk, which a restore would overwrite or remove"; return 1
            fi ;;
          *) _RES_WT+=("$path") ;;
        esac
        _RES_SIG+="$rec"$'\n' ;;
      '2 '*) _RES_WHY="rename_or_copy: a renamed or copied entry is not a blob modification"; return 1 ;;
      'u '*) _RES_WHY="unmerged_entry: the index holds a conflict"; return 1 ;;
      *) _RES_WHY="unrecognised_status_record: '${rec:0:40}'"; return 1 ;;
    esac
  done < "$st"
  return 0
}

# _res_parent_blocked <path> -> 0 (_RES_BLOCKED set) when a leading component
# of <path> is a non-directory or a symlink on disk.
_res_parent_blocked() {
  local rest="$1" pre=""
  _RES_BLOCKED=""
  while [ "${rest#*/}" != "$rest" ]; do
    pre="${pre:+$pre/}${rest%%/*}"; rest="${rest#*/}"
    if [ -L "$CO/$pre" ] || { [ -e "$CO/$pre" ] && [ ! -d "$CO/$pre" ]; }; then
      _RES_BLOCKED="$pre"; return 0
    fi
  done
  return 1
}
# _res_conv_read <source rev or ""> <path>... -> _RC[path]=1 for every path with
# a conversion in effect (text, eol, filter, ident or working-tree-encoding set
# to anything but unspecified/unset), under the working tree's attributes or,
# given a rev, under that tree's (`check-attr --source`). _RC_ALL=1 when
# core.autocrlf is true/input (every text file converts) or the read failed
# (fail closed).
declare -A _RC=()
_RC_ALL=0
# _res_autocrlf_converts -> 0 when core.autocrlf makes every text file convert:
# `input`, or ANY boolean true git accepts (true/yes/on/1). Unset -> 1; a read
# that errors (a malformed value) -> 0, fail closed.
_res_autocrlf_converts() {
  local raw rc b
  raw="$(gr config --get core.autocrlf)"; rc=$?
  [ "$rc" = 1 ] && return 1
  [ "$rc" = 0 ] || return 0
  case "$raw" in [Ii][Nn][Pp][Uu][Tt]) return 0 ;; esac
  b="$(gr config --type=bool --get core.autocrlf)" || return 0
  [ "$b" = true ]
}
_res_conv_read() {
  local src="$1" p a v f="$RTM_SCRATCH/checkattr"
  shift
  _RC=(); _RC_ALL=0
  if _res_autocrlf_converts; then _RC_ALL=1; return 0; fi
  [ $# -gt 0 ] || return 0
  if [ -n "$src" ]; then
    printf '%s\0' "$@" | git --no-optional-locks -C "$NATIVE" check-attr -z --stdin --source="$src" text eol crlf filter ident working-tree-encoding > "$f" 2>/dev/null \
      || { _RC_ALL=1; return 0; }
  else
    printf '%s\0' "$@" | git --no-optional-locks -C "$NATIVE" check-attr -z --stdin text eol crlf filter ident working-tree-encoding > "$f" 2>/dev/null \
      || { _RC_ALL=1; return 0; }
  fi
  while IFS= read -r -d '' p && IFS= read -r -d '' a && IFS= read -r -d '' v; do
    # The legacy `crlf` attribute counts in ANY state but unspecified
    # (`-crlf` included: conservative).
    case "$a:$v" in *:unspecified) continue ;; crlf:*) ;; *:unset) continue ;; esac
    _RC["$p"]=1
  done < "$f"
}
# _res_attrs_stable -> 1 (with _RES_WHY) when the attributes the round trip
# was judged under are not the ones the restore / fast-forward will use (a
# dirty .gitattributes, or one HEAD and origin/<default> disagree on) AND a
# file the restore writes or unlinks has a conversion under either.
_res_attrs_stable() {
  local why="" defb up d p
  local -a paths
  _RES_WHY=""
  for p in ${_RES_MOD[@]+"${_RES_MOD[@]}"} ${_RES_UNTR[@]+"${_RES_UNTR[@]}"}; do
    case "$p" in .gitattributes|*/.gitattributes) why="$p is itself dirty"; break ;; esac
  done
  defb="$(_res_default_branch)"; up=""
  [ -n "$defb" ] && up="refs/remotes/origin/$defb"
  if [ -z "$why" ] && [ -n "$up" ]; then
    d="$(gr diff --name-only HEAD "$up" -- ':(glob)**/.gitattributes')" || d="<diff failed>"
    [ -z "$d" ] || why="a .gitattributes differs between HEAD and origin/$defb (${d%%$'\n'*})"
  fi
  [ -n "$why" ] || return 0
  paths=(${_RES_WT[@]+"${_RES_WT[@]}"} ${_RES_UNTR[@]+"${_RES_UNTR[@]}"})
  [ "${#paths[@]}" -gt 0 ] || return 0
  _res_conv_read "" "${paths[@]}"
  for p in "${paths[@]}"; do
    if [ "$_RC_ALL" = 1 ] || [ -n "${_RC[$p]+s}" ]; then
      _RES_WHY="attributes_unstable: $why, and $p has a conversion in effect under the current attributes, so whether its bytes round-trip was decided under attributes the restore / fast-forward will not use"; return 1
    fi
  done
  if [ -z "$up" ]; then
    _RES_WHY="attributes_unstable: $why, and no origin/<default> resolves to read the tip's attributes"; return 1
  fi
  _res_conv_read "$up" "${paths[@]}"
  for p in "${paths[@]}"; do
    if [ "$_RC_ALL" = 1 ] || [ -n "${_RC[$p]+s}" ]; then
      _RES_WHY="attributes_unstable: $why, and $p has a conversion in effect under origin/$defb's attributes, so the fast-forward may write it differently from what the round trip was judged under"; return 1
    fi
  done
  return 0
}

# The fingerprint every race check compares: the tracked census records (which
# carry each INDEX blob id and path), then the untracked NAMES sorted (so a
# rename a -> b between looks changes it), then the blob id of every
# working-tree file the census names -- tracked ones still on disk, then
# untracked ones -- one per line, clean filters applied exactly as
# `git stash create` / `update-index --add` store them. The blob ids are LAST:
# the snapshot check reads them with `tail`.
# Every file is ALSO hashed with `--no-filters`. Where the raw hash differs
# from the stored one (a clean filter, eol conversion), OR a conversion is in
# effect for the path (_res_conv_read -- equal hashes do not prove the checkout
# writes the same bytes: an LF file under eol=crlf comes back CRLF), the stored
# blob is
# checked back out at that path (_res_smudge_reproduces: what the restore, the
# fast-forward and a snapshot apply all write); only when THAT does not
# reproduce the raw bytes -- a lossy filter, a mixed-EOL file -- does it return
# 2 with the path in $RTM_SCRATCH/filtered: neither the proof nor the snapshot
# could give those bytes back. A pure CRLF<->LF conversion round-trips and
# passes, so a core.autocrlf box stays restorable. Returns 1 (and no usable
# fingerprint) on a failed hash.
_res_hashes() {
  local out raw n i
  local -a ol rl al
  printf '%s' "$_RES_SIG"
  [ ${#_RES_UNTR[@]} -eq 0 ] || printf 'untracked-name %s\n' "${_RES_UNTR[@]}" | LC_ALL=C sort
  n=$(( ${#_RES_WT[@]} + ${#_RES_UNTR[@]} ))
  [ "$n" -eq 0 ] && return 0
  al=(${_RES_WT[@]+"${_RES_WT[@]}"} ${_RES_UNTR[@]+"${_RES_UNTR[@]}"})
  # `--stdin-paths` C-unquotes a line starting with `"` (so `"a"` would hash
  # the file `a`) and follows a symlink, while `stash create` / `update-index`
  # store its LINK TEXT; it also strips a trailing CR from each line. Those
  # paths are hashed one at a time: a quote- or backslash-leading name, or one
  # holding a CR, on argv; a symlink as its link text.
  local -a bl=() bi=()
  for i in "${!al[@]}"; do
    if [ -L "$CO/${al[$i]}" ]; then ol[$i]="L"
    else case "${al[$i]}" in '"'*|'\'*|*$'\r'*) ol[$i]="A" ;; *) bl+=("${al[$i]}"); bi+=("$i") ;; esac; fi
  done
  if [ "${#bl[@]}" -gt 0 ]; then
    local -a bo br
    mapfile -t bo < <(printf '%s\n' "${bl[@]}" | git --no-optional-locks -C "$NATIVE" hash-object --stdin-paths 2>/dev/null)
    mapfile -t br < <(printf '%s\n' "${bl[@]}" | git --no-optional-locks -C "$NATIVE" hash-object --no-filters --stdin-paths 2>/dev/null)
    [ "${#bo[@]}" = "${#bl[@]}" ] && [ "${#br[@]}" = "${#bl[@]}" ] || return 1
    for i in "${!bi[@]}"; do ol[${bi[$i]}]="${bo[$i]}"; rl[${bi[$i]}]="${br[$i]}"; done
  fi
  local lt
  for i in "${!al[@]}"; do
    case "${ol[$i]:-}" in
      L) lt="$(readlink -- "$CO/${al[$i]}")" || return 1
         ol[$i]="$(printf '%s' "$lt" | git --no-optional-locks -C "$NATIVE" hash-object --stdin 2>/dev/null)" || return 1
         rl[$i]="${ol[$i]}" ;;
      A) ol[$i]="$(git --no-optional-locks -C "$NATIVE" hash-object -- "${al[$i]}" 2>/dev/null)" || return 1
         rl[$i]="$(git --no-optional-locks -C "$NATIVE" hash-object --no-filters -- "${al[$i]}" 2>/dev/null)" || return 1 ;;
    esac
  done
  out="$(printf '%s\n' "${ol[@]}")"
  _res_conv_read "" "${al[@]}"
  for i in "${!al[@]}"; do
    [ -n "${ol[$i]}" ] || return 1
    [ -L "$CO/${al[$i]}" ] && continue
    if { [ "${ol[$i]}" != "${rl[$i]}" ] || [ "$_RC_ALL" = 1 ] || [ -n "${_RC[${al[$i]}]+s}" ]; } && ! _res_smudge_reproduces "${al[$i]}" "${ol[$i]}" "${rl[$i]}"; then
      printf '%s' "${al[$i]}" > "$RTM_SCRATCH/filtered" 2>/dev/null
      return 2
    fi
  done
  printf '%s\n' "$out"
}
# _res_smudge_reproduces <path> <stored blob> <raw hash> -> 0 when git's
# checkout of <blob> at <path> (`cat-file --filters`: smudge + eol conversion)
# is byte-identical to the raw bytes. Per file, and only for files whose two
# hashes differ or that have a conversion in effect: `cat-file --batch --filters` frames its output with the
# PRE-filter size, so it cannot be parsed. A blob the object store lacks fails
# closed (it is never written here).
_res_smudge_reproduces() {
  local sm
  [ -n "$2" ] && [ -n "$3" ] || return 1
  git --no-optional-locks -C "$NATIVE" cat-file -e "$2" 2>/dev/null || return 1
  sm="$(set -o pipefail; git --no-optional-locks -C "$NATIVE" cat-file --filters --path="$1" "$2" 2>/dev/null \
        | git --no-optional-locks -C "$NATIVE" hash-object --no-filters --stdin 2>/dev/null)" || return 1
  [ -n "$sm" ] && [ "$sm" = "$3" ]
}
# _res_hash_why <rc> -> the abstention reason for a failed/filtered fingerprint
_res_hash_why() {
  if [ "$1" = 2 ]; then
    printf 'filtered_content: %s: checking the stored blob back out (clean filter / eol conversion) does not reproduce the on-disk bytes, so a restore would lose bytes that neither the proof nor the snapshot can give back' "$(cat "$RTM_SCRATCH/filtered" 2>/dev/null)"
  else
    printf 'hash_failed: could not read the blob id of every changed or untracked file'
  fi
}
# The upstream default branch, resolved as the classifier resolves it:
# refs/remotes/origin/HEAD, else origin/main, else origin/master. Empty when
# none resolves.
_res_default_branch() {
  local r
  r="$(gr symbolic-ref -q --short refs/remotes/origin/HEAD)"
  if [ -n "$r" ] && gr rev-parse -q --verify "$r^{commit}" >/dev/null; then
    case "$r" in origin/*) printf '%s' "${r#origin/}"; return 0 ;; esac
  fi
  for r in main master; do
    gr rev-parse -q --verify "refs/remotes/origin/$r^{commit}" >/dev/null && { printf '%s' "$r"; return 0; }
  done
  return 0
}

# JSON string unescape for a dirty-provenance path: only \\ \" and \/ are
# accepted. Any other escape (\uXXXX, \t, ...) returns 1 -- such a path cannot
# be matched against the census with confidence, so the restore abstains.
_json_unescape_path() {
  local s="$1"
  case "$s" in *\\*) ;; *) printf '%s' "$s"; return 0 ;; esac
  s="${s//\\\\/$'\x01'}"
  s="${s//\\\"/\"}"
  s="${s//\\\//\/}"
  case "$s" in *\\*) return 1 ;; esac
  printf '%s' "${s//$'\x01'/\\}"
}

# `"untracked_on_upstream": ["p", ...]` from the provenance payload, into
# _RES_UOU. Returns 2 when the key is absent (a prover predating it, which
# therefore proves no untracked file), 1 when the array cannot be read -- a
# string at a time, anchored, so a `]` or `,` inside a path is not a delimiter.
_res_parse_untracked_on_upstream() {
  _RES_UOU=()
  local rest re_key re_str re_end p
  re_key='"untracked_on_upstream"[[:space:]]*:[[:space:]]*\['
  [[ "$1" =~ $re_key ]] || return 2
  rest="${1#*"${BASH_REMATCH[0]}"}"
  re_end='^[[:space:]]*\]'
  re_str='^[[:space:]]*"(([^"\\]|\\.)*)"[[:space:]]*(,|\])'
  [[ "$rest" =~ $re_end ]] && return 0
  while [[ "$rest" =~ $re_str ]]; do
    p="$(_json_unescape_path "${BASH_REMATCH[1]}")" || return 1
    _RES_UOU+=("$p")
    [ "${BASH_REMATCH[3]}" = "]" ] && return 0
    rest="${rest:${#BASH_REMATCH[0]}}"
  done
  return 1
}

# _rtm_ignored_collision <branch> <default branch> <upstream ref> [detached]
#   -> 0 (IGN_HIT set) when a path the branch switch or the fast-forward would
# ADD is already occupied on disk by something untracked -- an ignored file or
# symlink, a DIRECTORY holding any untracked or ignored entry (a tracked
# directory that upstream turns into a file), or an untracked file / symlink at
# one of its leading components. git overwrites ignored files on checkout and
# merge by default, and no snapshot holds them. A diff that cannot be read
# counts as a collision (fail closed).
# "Tracked" is decided case-INsensitively when core.ignorecase is true: on
# NTFS/APFS an upstream case-only rename (readme.md -> README.md) makes the
# tracked readme.md answer `-e README.md`, and it is git's to replace.
# Paths in IGN_EXCLUDE (the untracked files a residue restore is about to
# unlink) are not occupants.
declare -A IGN_EXCLUDE=()
_rtm_ignored_collision() {
  local br="$1" defb="$2" up="$3" det="${4:-false}" t p rest pre magic="literal" u
  local -a targets=("$up")
  IGN_HIT=""
  [ "$(gr config --type=bool --get core.ignorecase)" = true ] && magic="literal,icase"
  if [ "$br" != "$defb" ] || [ "$det" = true ]; then
    gr rev-parse --verify --quiet "refs/heads/$defb" >/dev/null && targets+=("refs/heads/$defb")
  fi
  [ -n "$RTM_SCRATCH" ] && [ -d "$RTM_SCRATCH" ] || { IGN_HIT="<no scratch dir to read the incoming paths into>"; return 0; }
  for t in "${targets[@]}"; do
    if ! gr diff --name-only -z --no-renames --diff-filter=A HEAD "$t" > "$RTM_SCRATCH/incoming"; then
      IGN_HIT="<git diff HEAD $t failed>"; return 0
    fi
    while IFS= read -r -d '' p; do
      if [ -d "$CO/$p" ] && [ ! -L "$CO/$p" ]; then
        # A directory where the target has a file: anything untracked or
        # ignored inside it is removed with it.
        gr ls-files -z --others -- ":($magic)$p/" > "$RTM_SCRATCH/occupants" || { IGN_HIT="<git ls-files --others $p failed>"; return 0; }
        while IFS= read -r -d '' u; do
          [ -n "${IGN_EXCLUDE[$u]+s}" ] && continue
          IGN_HIT="$p (a directory holding the untracked or ignored $u)"; return 0
        done < "$RTM_SCRATCH/occupants"
      elif { [ -e "$CO/$p" ] || [ -L "$CO/$p" ]; } && [ -z "${IGN_EXCLUDE[$p]+s}" ] \
           && [ -z "$(gr ls-files -- ":($magic)$p")" ]; then
        IGN_HIT="$p"; return 0
      fi
      rest="$p"; pre=""
      while [ "${rest#*/}" != "$rest" ]; do
        pre="${pre:+$pre/}${rest%%/*}"; rest="${rest#*/}"
        if { [ -L "$CO/$pre" ] || { [ -e "$CO/$pre" ] && [ ! -d "$CO/$pre" ]; }; } \
           && [ -z "${IGN_EXCLUDE[$pre]+s}" ] && [ -z "$(gr ls-files -- ":($magic)$pre")" ]; then
          IGN_HIT="$pre (a leading component of the incoming $p)"; return 0
        fi
      done
    done < "$RTM_SCRATCH/incoming"
  done
  return 1
}

# _res_same_list <array name> <array name> -> 0 when the two IN-MEMORY arrays
# hold the same elements in the same order. Element by element, so it never
# compares two command substitutions (check #73).
_res_same_list() {
  local -n _sl_a="$1" _sl_b="$2"
  local i
  [ "${#_sl_a[@]}" -eq "${#_sl_b[@]}" ] || return 1
  for i in "${!_sl_a[@]}"; do
    [ -n "${_sl_b[$i]+s}" ] && [ "${_sl_a[$i]}" = "${_sl_b[$i]}" ] || return 1
  done
  return 0
}

_rtm_restore_residue() {
  local gd pj prc h1 h2 h3 hrc stash n nu i p b msg ref err re_p re_c re_r rest uou_rc outside defb
  local base_tree base_head base_index utree ucommit uidx snap chk expect got rec xy hidx wi
  local -a pp cc rr census_sorted prov_sorted untr_sorted uou_sorted absent wth rb
  local -A class_of=()
  RES_OUTCOME="abstained"
  if [ -z "$DIRTY_PROV" ]; then
    RES_REASON="dirty_provenance_unavailable: no dirty-provenance.sh beside this sweep (or at \$QONTINUI_DIRTY_PROVENANCE_BIN), so no file can be PROVEN residue"
    return 0
  fi
  if [ -z "$RTM_SCRATCH" ] || [ ! -d "$RTM_SCRATCH" ]; then
    RES_REASON="no_scratch_dir: mktemp -d failed, so the NUL-delimited census cannot be read safely"
    return 0
  fi
  gd="$(gr rev-parse --path-format=absolute --git-dir)"; [ -n "$gd" ] || gd="$CO/.git"
  if [ -e "$gd/rebase-merge" ] || [ -e "$gd/rebase-apply" ] || [ -e "$gd/MERGE_HEAD" ] \
     || [ -e "$gd/CHERRY_PICK_HEAD" ] || [ -e "$gd/REVERT_HEAD" ] || [ -e "$gd/BISECT_LOG" ]; then
    RES_REASON="operation_in_progress: a sequencer operation is in flight in $gd"
    return 0
  fi
  if [ -e "$gd/index.lock" ]; then
    RES_REASON="index_locked: $gd/index.lock exists"
    return 0
  fi

  # -- census #1 and the blob ids the proof must be about --------------------
  if ! _res_census; then RES_REASON="$_RES_WHY"; return 0; fi
  n=${#_RES_MOD[@]}; nu=${#_RES_UNTR[@]}
  if [ "$n" -eq 0 ] && [ "$nu" -eq 0 ]; then
    RES_OUTCOME="nothing_to_restore"
    RES_REASON="no tracked file is changed and no untracked file is present"
    return 0
  fi
  mapfile -t census_sorted < <(printf '%s\n' ${_RES_MOD[@]+"${_RES_MOD[@]}"} | LC_ALL=C sort -u)
  mapfile -t untr_sorted < <(printf '%s\n' ${_RES_UNTR[@]+"${_RES_UNTR[@]}"} | LC_ALL=C sort -u)
  [ "$n" -eq 0 ] && census_sorted=()
  [ "$nu" -eq 0 ] && untr_sorted=()
  # A path both changed-tracked and untracked (a staged deletion re-created on
  # disk) would be restored from HEAD and then unlinked: refuse the shape.
  if [ "$n" -gt 0 ] && [ "$nu" -gt 0 ] \
     && [ -n "$(LC_ALL=C comm -12 <(printf '%s\n' "${census_sorted[@]}") <(printf '%s\n' "${untr_sorted[@]}"))" ]; then
    RES_REASON="path_collision: a path is both a changed tracked entry and an untracked file"
    return 0
  fi
  h1="$(_res_hashes)"; hrc=$?
  if [ "$hrc" != 0 ]; then
    RES_REASON="$(_res_hash_why "$hrc")"
    return 0
  fi
  if ! _res_attrs_stable; then RES_REASON="$_RES_WHY"; return 0; fi

  # -- the proof -------------------------------------------------------------
  pj="$(_rtm_timeout "$PROV_TIMEOUT" bash "$DIRTY_PROV" --json "$CO" 2>/dev/null)"; prc=$?
  RES_PROV_RC="$prc"
  case "$prc" in
    0) ;;
    1) RES_REASON="residue_unique: dirty-provenance.sh exited 1 -- $(jget_n "$pj" unique_count) changed tracked file(s) are UNIQUE content (never on the upstream default branch) and $(jget_n "$pj" outside_footprint_count) EOL-only/runner-bundle file(s) are outside the provisioner footprint (.claude/commands/**, .claude/skills/**); none of those is ever restored, and one of them keeps the whole repo as it is (run \`dirty-provenance.sh --summary $CO\` for the list)"; return 0 ;;
    3) RES_REASON="residue_unknown: dirty-provenance.sh exited 3 -- a probe could not decide (UNKNOWN/INCOMPLETE), and an undecided file is never restored"; return 0 ;;
    4) RES_REASON="residue_usage: dirty-provenance.sh reported a usage error on this checkout"; return 0 ;;
    124) RES_REASON="residue_timeout: dirty-provenance.sh did not finish within ${PROV_TIMEOUT}s"; return 0 ;;
    *) RES_REASON="residue_exit_out_of_range: dirty-provenance.sh exited $prc, outside its documented 0/1/3/4"; return 0 ;;
  esac

  # Exit 0 is necessary, not sufficient: the payload must agree with itself and
  # with THIS script's census, file for file. Parsing is by leftmost regex; any
  # disagreement -- including a payload this parser cannot read -- abstains.
  if [ "$(jget_n "$pj" all_residue)" != true ] || [ "$(jget_n "$pj" unique_count)" != 0 ]; then
    RES_REASON="provenance_inconsistent: exit 0 but all_residue=$(jget_n "$pj" all_residue) unique_count=$(jget_n "$pj" unique_count)"
    return 0
  fi
  # The untracked list comes first and is cut out of the payload, so the
  # files[] walk below never reads into it.
  _res_parse_untracked_on_upstream "$pj"; uou_rc=$?
  if [ "$uou_rc" = 1 ]; then
    RES_REASON="provenance_unparseable: the payload's untracked_on_upstream array could not be read"
    return 0
  fi
  re_p='"path"[[:space:]]*:[[:space:]]*"(([^"\\]|\\.)*)"'
  re_c='"class"[[:space:]]*:[[:space:]]*"([^"]*)"'
  rest="$pj"; pp=()
  while [[ "$rest" =~ $re_p ]]; do
    p="$(_json_unescape_path "${BASH_REMATCH[1]}")" || {
      RES_REASON="provenance_unparseable_path: a path in the provenance payload uses a JSON escape this script does not decode, so it cannot be matched to the census"; return 0; }
    pp+=("$p")
    rest="${rest#*"${BASH_REMATCH[0]}"}"
  done
  rest="$pj"; cc=()
  while [[ "$rest" =~ $re_c ]]; do
    cc+=("${BASH_REMATCH[1]}")
    rest="${rest#*"${BASH_REMATCH[0]}"}"
  done
  # `restorable` is per file and bare (true/false); the leftmost-regex walk
  # pairs the k-th flag with the k-th path, like the classes.
  re_r='"restorable"[[:space:]]*:[[:space:]]*(true|false)'
  rest="$pj"; rr=()
  while [[ "$rest" =~ $re_r ]]; do
    rr+=("${BASH_REMATCH[1]}")
    rest="${rest#*"${BASH_REMATCH[0]}"}"
  done
  if [ "${#pp[@]}" != "${#cc[@]}" ] || [ "${#pp[@]}" != "${#rr[@]}" ]; then
    RES_REASON="provenance_unparseable: ${#pp[@]} path(s), ${#cc[@]} class(es) and ${#rr[@]} restorable flag(s) in the payload"
    return 0
  fi
  for i in ${cc[@]+"${!cc[@]}"}; do
    case "${cc[$i]}" in
      UPSTREAM_CURRENT|UPSTREAM_HISTORICAL|RUNNER_BUNDLE|EOL_ONLY) ;;
      *) RES_REASON="provenance_not_residue: ${pp[$i]:-a file} is classed '${cc[$i]}', which is neither already-on-main nor a residue class"; return 0 ;;
    esac
    # The restore list must be EXACTLY the restorable set: one non-restorable
    # file stops it all.
    if [ "${rr[$i]}" != true ]; then
      RES_REASON="provenance_not_restorable: ${pp[$i]:-a file} is '${cc[$i]}' but restorable=${rr[$i]}, so nothing is restored"; return 0
    fi
    class_of["${pp[$i]}"]="${cc[$i]}"
  done
  mapfile -t prov_sorted < <(printf '%s\n' ${pp[@]+"${pp[@]}"} | LC_ALL=C sort -u)
  [ "${#pp[@]}" -eq 0 ] && prov_sorted=()
  if [ "${#pp[@]}" != "$n" ] || [ "${#prov_sorted[@]}" != "$n" ] \
     || ! _res_same_list census_sorted prov_sorted; then
    RES_REASON="provenance_census_mismatch: dirty-provenance.sh proved ${#pp[@]} tracked file(s) restorable but this checkout has $n changed tracked file(s), and the two lists are not identical; only an exact match is restored"
    return 0
  fi
  # Untracked: the prover's tip-equal list must be EXACTLY this census's
  # untracked list. Any other untracked file is content nothing upstream has
  # at that path; it is never removed, and it forces UNIQUE_WIP on its own.
  mapfile -t uou_sorted < <(printf '%s\n' ${_RES_UOU[@]+"${_RES_UOU[@]}"} | LC_ALL=C sort -u)
  [ "${#_RES_UOU[@]}" -eq 0 ] && uou_sorted=()
  if [ "${#_RES_UOU[@]}" != "$nu" ] || [ "${#uou_sorted[@]}" != "$nu" ] \
     || ! _res_same_list untr_sorted uou_sorted; then
    RES_REASON="untracked_present: $nu untracked file(s) are present and dirty-provenance.sh proved ${#_RES_UOU[@]} of them byte-identical to the upstream tip$([ "$uou_rc" = 2 ] && printf ' (its payload carries no untracked_on_upstream list at all)'); an untracked file is removed only when EVERY one is proven, since any other forces UNIQUE_WIP on its own and the restore would change the tree without enabling a return"
    return 0
  fi
  # A path HEAD does not have (a staged add) is admitted ONLY when it is
  # UPSTREAM_CURRENT: restoring it deletes it, and only a tip-equal file is
  # guaranteed to come back, byte for byte, with the fast-forward.
  absent=()
  if [ "$n" -gt 0 ]; then
    while IFS= read -r chk; do
      case "$chk" in *' missing') p="${chk#HEAD:}"; absent+=("${p% missing}") ;; esac
    done < <(printf 'HEAD:%s\n' "${_RES_MOD[@]}" | gr cat-file --batch-check='%(objectname)')
    for p in ${absent[@]+"${absent[@]}"}; do
      if [ "${class_of[$p]:-}" != UPSTREAM_CURRENT ]; then
        RES_REASON="absent_from_head_not_current: $p is not in HEAD and is classed '${class_of[$p]:-?}'; a path HEAD lacks is removed only when it is byte-identical to the upstream tip"
        return 0
      fi
    done
  fi

  # "Restorable anywhere" presumes a fast-forward that reproduces the bytes,
  # which only the default branch gets. Outside the provisioner footprint a
  # restore is therefore allowed only there (see 1e in the header).
  # The switch / fast-forward that may follow must not overwrite an ignored
  # file either -- checked HERE, before anything is restored, so a collision
  # abstains with the tree untouched (the classifier's arm checks again). The
  # untracked files this restore unlinks are not occupants.
  defb="$(_res_default_branch)"
  if [ -n "$defb" ]; then
    IGN_EXCLUDE=()
    for p in ${_RES_UNTR[@]+"${_RES_UNTR[@]}"}; do IGN_EXCLUDE["$p"]=1; done
    if _rtm_ignored_collision "${BRANCH_PRE:-HEAD}" "$defb" "refs/remotes/origin/$defb" "$([ "${BRANCH_PRE:-HEAD}" = HEAD ] && echo true || echo false)"; then
      RES_REASON="ignored_path_overwrite: $IGN_HIT -- the switch to $defb or the fast-forward to origin/$defb that would follow this restore adds a tracked path where an ignored (or untracked) file sits on disk; nothing restored"
      IGN_EXCLUDE=(); return 0
    fi
    IGN_EXCLUDE=()
  fi
  outside=""
  for p in ${_RES_MOD[@]+"${_RES_MOD[@]}"} ${_RES_UNTR[@]+"${_RES_UNTR[@]}"}; do
    case "$p" in .claude/commands/?*|.claude/skills/?*) ;; *) outside="$p"; break ;; esac
  done
  if [ -n "$outside" ]; then
    if [ -z "$defb" ] || [ "${BRANCH_PRE:-}" != "$defb" ]; then
      RES_REASON="not_on_default_branch: residue outside the provisioner footprint is restored only on ${defb:-<the default branch, which does not resolve>}, where the fast-forward reproduces it ($outside is outside .claude/commands/** and .claude/skills/**; this checkout is on ${BRANCH_PRE:-<unknown>})"
      return 0
    fi
    # On the default branch by NAME is not enough: a local default carrying
    # commits upstream lacks gets no fast-forward either.
    if ! gr merge-base --is-ancestor HEAD "refs/remotes/origin/$defb"; then
      RES_REASON="no_fast_forward_follows: HEAD is not an ancestor of origin/$defb (local $defb carries commits upstream lacks, or the check failed), so no fast-forward would reproduce residue outside the provisioner footprint ($outside)"
      return 0
    fi
  fi

  RES_FILES=(${_RES_MOD[@]+"${_RES_MOD[@]}"})
  RES_UNTRACKED=(${_RES_UNTR[@]+"${_RES_UNTR[@]}"})
  if [ "$DRY_RUN" = 1 ]; then
    RES_OUTCOME="would_restore"
    RES_REASON="all $n changed tracked file(s) and $nu untracked file(s) proven already on the upstream default branch or residue by dirty-provenance.sh; a live run would snapshot them to refs/wip/return-to-main/<stamp>-<repo>-residue, restore the tracked ones to HEAD (index and working tree) and unlink the untracked ones"
    return 0
  fi

  # -- the tree must not have moved while the proof ran ----------------------
  # Re-census (its status checked), then compare the in-memory lists element by
  # element -- no probe output is compared, so two failed reads cannot agree.
  local -a mod_now=() untr_now=()
  local census_ok=0
  if _res_census; then
    census_ok=1
    mapfile -t mod_now < <(printf '%s\n' ${_RES_MOD[@]+"${_RES_MOD[@]}"} | LC_ALL=C sort -u)
    mapfile -t untr_now < <(printf '%s\n' ${_RES_UNTR[@]+"${_RES_UNTR[@]}"} | LC_ALL=C sort -u)
    [ "${#_RES_MOD[@]}" -eq 0 ] && mod_now=()
    [ "${#_RES_UNTR[@]}" -eq 0 ] && untr_now=()
  fi
  if [ "$census_ok" != 1 ] || [ "${#mod_now[@]}" != "$n" ] || [ "${#untr_now[@]}" != "$nu" ] \
     || ! _res_same_list mod_now census_sorted || ! _res_same_list untr_now untr_sorted; then
    RES_REASON="raced_dirty: the set of changed or untracked files changed while dirty-provenance.sh ran${_RES_WHY:+ ($_RES_WHY)}"
    return 0
  fi
  h2="$(_res_hashes)"; hrc=$?
  if [ "$hrc" = 2 ]; then RES_REASON="$(_res_hash_why 2)"; return 0; fi
  if [ "$hrc" != 0 ] || [ "$h2" != "$h1" ]; then
    RES_REASON="raced_dirty: a changed file's index or working-tree content, or an untracked file's content, changed while dirty-provenance.sh ran, so the proof is about content that is no longer there"
    return 0
  fi

  # -- the snapshot, verified, BEFORE anything is restored ---------------------
  # `git stash create` records HEAD (^1), the index (^2) and the tracked
  # working tree (its own tree), and writes nothing but objects. It omits
  # untracked files, so when there are any the snapshot gains the untracked
  # parent `git stash push -u` writes (^3): a parentless commit whose tree is
  # exactly those files, built in a scratch index that is never the checkout's.
  base_head="$(gr rev-parse --verify --quiet 'HEAD^{commit}')"
  if [ -z "$base_head" ]; then
    RES_REASON="snapshot_failed: HEAD does not resolve to a commit"; return 0
  fi
  stash=""
  if [ "$n" -gt 0 ]; then
    stash="$(gr stash create "return-to-main-sweep residue snapshot: $REPO")"; stash="${stash//[$'\r'$'\n']/}"
    if [ -z "$stash" ]; then
      RES_REASON="snapshot_failed: \`git stash create\` produced nothing although $n tracked file(s) are changed"
      return 0
    fi
  fi
  snap="$stash"
  if [ "$nu" -gt 0 ]; then
    if [ -n "$stash" ]; then
      base_tree="$stash^{tree}"; base_index="$stash^2"
    else
      base_tree="$base_head^{tree}"
      base_index="$(gw commit-tree -p "$base_head" -m "index on ${BRANCH_PRE:-HEAD}: return-to-main-sweep residue snapshot" "$base_head^{tree}" 2>/dev/null)"
      [ -n "$base_index" ] || { RES_REASON="snapshot_failed: could not write the index commit of the untracked snapshot"; return 0; }
    fi
    uidx="$(native_path_w "$RTM_SCRATCH/untracked.index")"
    rm -f "$RTM_SCRATCH/untracked.index"
    if ! printf '%s\0' "${_RES_UNTR[@]}" | GIT_INDEX_FILE="$uidx" gw update-index --add -z --stdin 2>/dev/null \
       || ! utree="$(GIT_INDEX_FILE="$uidx" gw write-tree 2>/dev/null)" || [ -z "$utree" ] \
       || ! ucommit="$(gw commit-tree -m "untracked files on ${BRANCH_PRE:-HEAD}: return-to-main-sweep residue snapshot" "$utree" 2>/dev/null)" || [ -z "$ucommit" ] \
       || ! snap="$(gw commit-tree -p "$base_head" -p "$base_index" -p "$ucommit" \
                     -m "On ${BRANCH_PRE:-HEAD}: return-to-main-sweep residue snapshot: $REPO" "$base_tree" 2>/dev/null)" \
       || [ -z "$snap" ]; then
      rm -f "$RTM_SCRATCH/untracked.index"
      RES_REASON="snapshot_failed: could not write the untracked parent of the snapshot, and an untracked file is never removed without one"
      return 0
    fi
    rm -f "$RTM_SCRATCH/untracked.index"
  fi
  # Verify, in one batch, that the snapshot holds exactly the proven content:
  # every working-tree file (and every working-tree deletion) in its tree, every
  # index blob (and every staged deletion) in ^2, every untracked blob in ^3.
  expect=""; chk=""; wi=0; rb=()
  mapfile -t wth < <(printf '%s' "$h1" | tail -n "$(( ${#_RES_WT[@]} + nu ))")
  [ $(( ${#_RES_WT[@]} + nu )) -eq 0 ] && wth=()
  while IFS= read -r rec; do
    [ -n "$rec" ] || continue
    xy="${rec:2:2}"; p="${rec#* * * * * * * * }"
    read -r _ _ _ _ _ _ _ hidx _ <<< "$rec"
    case "$xy" in
      ?D|D?) chk+="$snap:$p"$'\n'; expect+="$snap:$p missing"$'\n'; rb+=("deleted $p") ;;
      *)  chk+="$snap:$p"$'\n'; expect+="${wth[$wi]}"$'\n'; rb+=("${wth[$wi]} $p"); wi=$((wi + 1)) ;;
    esac
    case "$xy" in
      D?) chk+="$snap^2:$p"$'\n'; expect+="$snap^2:$p missing"$'\n' ;;
      *)  chk+="$snap^2:$p"$'\n'; expect+="$hidx"$'\n' ;;
    esac
  done <<< "$_RES_SIG"
  for p in ${_RES_UNTR[@]+"${_RES_UNTR[@]}"}; do
    chk+="$snap^3:$p"$'\n'; expect+="${wth[$wi]}"$'\n'; rb+=("${wth[$wi]} $p"); wi=$((wi + 1))
  done
  if [ -n "$chk" ]; then
    got="$(printf '%s' "$chk" | gr cat-file --batch-check='%(objectname)')"
    if [ "$got"$'\n' != "$expect" ]; then
      RES_REASON="snapshot_mismatch: the snapshot $snap does not hold exactly the proven working-tree, index and untracked content; nothing restored"
      return 0
    fi
  fi
  ref="refs/wip/return-to-main/$(date -u +%Y%m%dT%H%M%SZ 2>/dev/null || printf 'nostamp')-$(printf '%s' "$REPO" | tr -c 'A-Za-z0-9._-' '_')-residue"
  msg="return-to-main-sweep: $REPO residue restore on ${BRANCH_PRE:-<unknown branch>} -- $n tracked + $nu untracked file(s) proven on-main/residue by dirty-provenance.sh (exit 0); undo with: git stash apply --index $snap"
  if ! gw update-ref --create-reflog -m "$msg" "$ref" "$snap" 2>/dev/null \
     && ! gw update-ref -m "$msg" "$ref" "$snap" 2>/dev/null; then
    RES_REASON="snapshot_failed: could not write $ref -> $snap, and nothing is restored without a recoverable ref"
    return 0
  fi
  RES_WIP_REF="$ref"; RES_WIP_COMMIT="$snap"
  RES_BLOBS=(${rb[@]+"${rb[@]}"})

  # -- last look, then restore EXACTLY those files -----------------------------
  # The fingerprint carries the untracked NAMES, so a rename between the
  # snapshot and here (a -> b) fails this check rather than unlinking b.
  if ! _res_census; then
    RES_REASON="raced_dirty: the tree changed shape after the snapshot was written${_RES_WHY:+ ($_RES_WHY)}; nothing restored (the snapshot $ref is kept)"
    return 0
  fi
  h3="$(_res_hashes)"; hrc=$?
  if [ "$hrc" = 2 ]; then RES_REASON="$(_res_hash_why 2); nothing restored (the snapshot $ref is kept)"; return 0; fi
  if [ "$hrc" != 0 ] || [ "$h3" != "$h1" ]; then
    RES_REASON="raced_dirty: a proven file changed (or an untracked file was renamed) after the snapshot was written; nothing restored (the snapshot $ref is kept)"
    return 0
  fi
  # ONE mutation for every tracked path, staged or not: HEAD's version into the
  # index AND the working tree. A staged add (absent from HEAD, admitted above
  # only as UPSTREAM_CURRENT) leaves both; a deletion comes back from HEAD.
  if [ "$n" -gt 0 ] \
     && ! err="$(printf '%s\0' "${_RES_MOD[@]}" | gw --literal-pathspecs restore --source=HEAD --staged --worktree --pathspec-from-file=- --pathspec-file-nul 2>&1)"; then
    RES_OUTCOME="failed"
    RES_REASON="restore_failed: git restore of the $n proven tracked file(s) errored: ${err:0:300} (nothing forced or retried; $ref holds the pre-restore content)"
    return 0
  fi
  # Untracked: an explicit unlink of exactly the proven paths, never `git clean`.
  for p in ${_RES_UNTR[@]+"${_RES_UNTR[@]}"}; do
    if ! err="$(rm -f -- "$CO/$p" 2>&1)"; then
      RES_OUTCOME="failed"
      RES_REASON="restore_failed: could not unlink the proven untracked file $p: ${err:0:300} ($ref holds its content at ^3)"
      return 0
    fi
  done
  RES_OUTCOME="restored"
  RES_REASON="restored $n tracked file(s) to HEAD and unlinked $nu untracked file(s), all proven already on the upstream default branch or residue by dirty-provenance.sh; snapshot $ref -> $snap"
  N_RES_RESTORED=$((N_RES_RESTORED + 1))
  return 0
}

for CO in ${CANDIDATES[@]+"${CANDIDATES[@]}"}; do
  N_EXAMINED=$((N_EXAMINED + 1))
  REPO="${CO##*/}"
  EXAMINED_REPOS+=("$REPO")
  NATIVE="$(native_path_w "$CO")"
  ACTION=""; REASON=""; WIP_REF=""; WIP_COMMIT=""; HEAD_AFTER=""; HEAD_BEFORE=""
  VERDICT=""; CRC=""; BRANCH=""; DEFAULT_BRANCH=""; UPSTREAM=""
  UP_TIP_AGE=""; UP_FETCHED_AT=""; UP_FETCHED_SRC=""; UP_TIP=""; AHEAD=""; BEHIND=""
  FETCHED=false; FETCH_ERR=""
  VERDICT_SOURCE="classifier"; ADJ_SHA=""; ADJ_REASON=""; ADJ_EVIDENCE=""; ADJ_EVIDENCE_SHA=""; ADJ_KIND=""
  RES_OUTCOME="not_requested"; RES_REASON=""; RES_WIP_REF=""; RES_WIP_COMMIT=""; RES_FILES=(); RES_UNTRACKED=(); RES_BLOBS=(); RES_PROV_RC=""; RES_NO_FF=false
  PRE_ACTION=""; PRE_REASON=""

  # An EMPTY conversion must never reach git: `git -C ""` is a documented NO-OP
  # that leaves git in the CURRENT directory and exits 0 -- here that would
  # switch a branch in the CALLER's repository under the target's name.
  if [ -z "$NATIVE" ]; then
    ACTION="ABSTAINED"; REASON="path conversion produced an empty path for $CO"
    N_ABSTAIN=$((N_ABSTAIN + 1))
    ROWS+=("$REPO|?|?|$ACTION|$REASON")
    emit_decision
    continue
  fi

  # -- 1b: refresh the upstream ref first, so every read below sees it ------
  [ "$FETCH" = 1 ] && _rtm_fetch

  # -- 1e: restore proven residue before classifying ----------------------
  if [ "${#RESIDUE_REPOS[@]}" -gt 0 ] && in_list "$REPO" "${RESIDUE_REPOS[@]}"; then
    BRANCH_PRE="$(gr rev-parse --abbrev-ref HEAD)"
    _rtm_restore_residue
    if [ "$RES_OUTCOME" = failed ]; then
      # A restore that errored mid-way leaves a tree nobody has classified.
      # Nothing further is attempted on it this run.
      PRE_ACTION="FAILED"; PRE_REASON="$RES_REASON"
    fi
  fi

  # HEAD is read BEFORE classification and again after, so the window this
  # script is responsible for is the window it actually measures.
  HEAD_BEFORE="$(gr rev-parse HEAD)"

  if [ -n "$PRE_ACTION" ]; then
    CJSON=""; CRC=""
  else
    CJSON="$("$CLASSIFIER" --json "$CO" 2>/dev/null)"; CRC=$?
  fi

  VERDICT="$(jget_s "$CJSON" verdict)"
  BRANCH="$(jget_s "$CJSON" branch)"
  UPSTREAM="$(jget_s "$CJSON" upstream_ref)"
  UP_TIP_AGE="$(jget_s "$CJSON" upstream_tip_age)"
  UP_FETCHED_AT="$(jget_s "$CJSON" upstream_fetched_at)"
  UP_FETCHED_SRC="$(jget_s "$CJSON" upstream_fetched_at_source)"
  UP_TIP="$(jget_s "$CJSON" upstream_tip)"
  AHEAD="$(jget_n "$CJSON" ahead)"
  BEHIND="$(jget_n "$CJSON" behind)"
  DETACHED="$(jget_n "$CJSON" detached)"
  SETTLED="$(jget_s "$CJSON" settled_by)"
  INCOMPLETE_WHY="$(jget_s "$CJSON" incomplete_reason)"
  DIRTY_T="$(jget_n "$CJSON" dirty_tracked)"
  DIRTY_U="$(jget_n "$CJSON" dirty_untracked)"
  if [ -n "$PRE_ACTION" ]; then VERDICT="NOT_CLASSIFIED"; fi
  [ -n "$VERDICT" ] || VERDICT="UNPARSEABLE"
  # `ahead` / `behind` are JSON null whenever the classifier settled before it
  # counted them (uncommitted content does exactly that). Normalise to
  # digits-or-EMPTY here, so that below "" means UNKNOWN and never accidentally
  # compares equal to 0 -- an unknown behind-count must not read as "level".
  case "$AHEAD"  in ''|*[!0-9]*) AHEAD=""  ;; esac
  case "$BEHIND" in ''|*[!0-9]*) BEHIND="" ;; esac
  # The ages, in words, for every reason line: WHEN the ref was fetched and how
  # old its tip commit is are different facts and are never merged (1c).
  AGES="upstream last fetched ${UP_FETCHED_AT:-at an UNKNOWN time}; its tip committed ${UP_TIP_AGE:-at an unknown time}"

  # -- 1d: an adjudication can make a CLEAN UNIQUE_WIP / MIXED actionable ----
  ACT_RC="$CRC"
  if ADJ_SHA="$(adj_sha_for "$REPO")"; then
    ADJ_KIND="$(adj_kind_for "$REPO")"
    ADJ_EVIDENCE="$EVIDENCE"; ADJ_EVIDENCE_SHA="$EVIDENCE_SHA"
    case "$CRC" in
      0) : ;;   # the classifier's own verdict already licenses the act; the adjudication is logged, not used
      1|2)
        if [ -z "$HEAD_BEFORE" ] || [ "$HEAD_BEFORE" != "$ADJ_SHA" ]; then
          ADJ_REASON="adjudicated_sha_mismatch: $(adj_flag_of "$ADJ_KIND") named $ADJ_SHA but HEAD is ${HEAD_BEFORE:-<unreadable>}, so the adjudication describes a state this checkout has left. The classifier's $VERDICT stands"
        elif [ "${DIRTY_T:-x}" != 0 ] || [ "${DIRTY_U:-x}" != 0 ]; then
          ADJ_REASON="adjudicated_but_dirty: the tree holds ${DIRTY_T:-?} tracked / ${DIRTY_U:-?} untracked uncommitted change(s); an adjudication is about COMMITS and never overrides uncommitted content"
        else
          # The merge guard, held HERE and not only by the adjudicator. An EVIL
          # MERGE (a merge commit whose conflict resolution adds content no
          # patch carries) is invisible to `git cherry`, so a judgment built on
          # patch-ids can call such a branch landed while the merge holds the
          # only copy of that content. This script therefore honours an
          # adjudication only on a branch ahead of its upstream by ZERO merge
          # commits, whatever the evidence file says. The classifier's own
          # count is used when it reported one (it counts the same range);
          # when it settled before counting, the count is taken here, pinned
          # to the adjudicated SHA. A count nobody could take is a refusal.
          ADJ_MERGES="$(jget_n "$CJSON" ahead_merge_commits)"; ADJ_MERGES_SRC="the classifier's ahead_merge_commits"
          case "$ADJ_MERGES" in
            ''|*[!0-9]*)
              ADJ_MERGES=""; ADJ_MERGES_SRC="counted by this script (the classifier reported no count)"
              if [ -n "$UPSTREAM" ]; then
                ADJ_MERGES="$(gr rev-list --count --merges "${UPSTREAM}..${ADJ_SHA}")"
                ADJ_MERGES="${ADJ_MERGES//[$'\r'$'\n']/}"
                case "$ADJ_MERGES" in ''|*[!0-9]*) ADJ_MERGES="" ;; esac
              fi ;;
          esac
          if [ -z "$ADJ_MERGES" ]; then
            ADJ_REASON="adjudication_refused_merge_count_unknown: the number of merge commits in ${UPSTREAM:-<no upstream>}..$ADJ_SHA could not be established, and an adjudication is honoured only on a branch with ZERO ahead merges. The classifier's $VERDICT stands"
          elif [ "$ADJ_MERGES" != 0 ]; then
            ADJ_REASON="adjudication_refused_ahead_merges: the branch is ahead of $UPSTREAM by $ADJ_MERGES merge commit(s) ($ADJ_MERGES_SRC). A merge's conflict resolution can carry content no patch holds, which git cherry -- and so any patch-id adjudication -- cannot see; an adjudication is honoured only with ZERO ahead merges. The classifier's $VERDICT stands"
          else
            # The ONE thing the two spellings do differently: the word recorded.
            ACT_RC=0
            if [ "$ADJ_KIND" = superseded ]; then VERDICT_SOURCE="adjudicated_superseded"; else VERDICT_SOURCE="adjudicated"; fi
          fi
        fi ;;
      *) ADJ_REASON="adjudication_not_applicable: the classifier answered ${VERDICT} (exit ${CRC:-none}); an adjudication overrides only UNIQUE_WIP or MIXED, and INCOMPLETE is never collapsed toward LANDED_DUPLICATE" ;;
    esac
  else
    ADJ_SHA=""; ADJ_KIND=""
  fi

  if [ "$ACT_RC" != 0 ]; then
    # THE ABSTENTION ARM. Everything that is not (effectively) exit 0 lands
    # here, INCLUDING INCOMPLETE -- which is never collapsed toward
    # LANDED_DUPLICATE, because that is the single move §6 names as able to
    # destroy work.
    if [ -n "$PRE_ACTION" ]; then
      ACTION="$PRE_ACTION"; REASON="$PRE_REASON"
      if [ "$ACTION" = FAILED ]; then N_FAILED=$((N_FAILED + 1)); else N_ABSTAIN=$((N_ABSTAIN + 1)); fi
    else
      ACTION="ABSTAINED"
      if [ -n "$ADJ_REASON" ]; then
        REASON="$ADJ_REASON"
      else
        case "$CRC" in
          1) REASON="UNIQUE_WIP: ${SETTLED:-content exists that the upstream does not have}" ;;
          2) REASON="MIXED: some ahead-commits landed and some did not; neither returning it nor treating it as peer work is right" ;;
          3) REASON="INCOMPLETE: ${INCOMPLETE_WHY:-the classifier could not answer}. NOT collapsed toward LANDED_DUPLICATE." ;;
          4) REASON="the classifier reported a usage error on this checkout" ;;
          *) REASON="the classifier exited $CRC, which is outside its documented 0-4 range" ;;
        esac
      fi
      case "$RES_OUTCOME" in
        abstained)     REASON="residue restore abstained ($RES_REASON); then $REASON" ;;
        would_restore) REASON="residue WOULD be restored in a live run ($RES_REASON); in this dry run the tree is still dirty, so $REASON" ;;
      esac
      N_ABSTAIN=$((N_ABSTAIN + 1))
    fi
  else
    # -- The acting arm. Everything below re-verifies rather than trusts. -----
    GITDIR="$(gr rev-parse --path-format=absolute --git-dir)"
    [ -n "$GITDIR" ] || GITDIR="$CO/.git"
    ADJ_NOTE=""
    case "$VERDICT_SOURCE" in adjudicated|adjudicated_superseded)
      ADJ_NOTE=" [adjudicated ${ADJ_KIND}: classifier said $VERDICT; HEAD $ADJ_SHA matches $(adj_flag_of "$ADJ_KIND"); evidence $EVIDENCE${EVIDENCE_SHA:+ sha256 $EVIDENCE_SHA}]" ;;
    esac

    if [ "${DIRTY_T:-0}" != 0 ] || [ "${DIRTY_U:-0}" != 0 ]; then
      # Belt and braces: the classifier forces UNIQUE_WIP on any uncommitted
      # content, so a LANDED_DUPLICATE carrying a non-zero count would mean the
      # two halves disagree. Refuse and say so rather than pick one.
      ACTION="ABSTAINED"
      REASON="the classifier said LANDED_DUPLICATE but its own record reports ${DIRTY_T} tracked / ${DIRTY_U} untracked uncommitted -- the two disagree, so nothing is touched"
      N_ABSTAIN=$((N_ABSTAIN + 1))
    elif [ -e "$GITDIR/rebase-merge" ] || [ -e "$GITDIR/rebase-apply" ] \
      || [ -e "$GITDIR/MERGE_HEAD" ] || [ -e "$GITDIR/CHERRY_PICK_HEAD" ] \
      || [ -e "$GITDIR/REVERT_HEAD" ] || [ -e "$GITDIR/BISECT_LOG" ]; then
      # A half-finished sequencer operation can leave a CLEAN tree between
      # steps, so cleanliness is not evidence that nothing is in flight, and a
      # branch switch here destroys the operation's state.
      ACTION="ABSTAINED"
      REASON="operation_in_progress: a rebase / merge / cherry-pick / revert / bisect is in flight in $GITDIR; a clean tree between its steps is not evidence that nothing is happening"
      N_ABSTAIN=$((N_ABSTAIN + 1))
    elif [ -e "$GITDIR/index.lock" ]; then
      ACTION="ABSTAINED"
      REASON="index_locked: $GITDIR/index.lock exists, so another git process is mid-write in this checkout"
      N_ABSTAIN=$((N_ABSTAIN + 1))
    else
      case "$UPSTREAM" in
        origin/*) DEFAULT_BRANCH="${UPSTREAM#origin/}" ;;
        *) DEFAULT_BRANCH="" ;;
      esac
      HEAD_NOW="$(gr rev-parse HEAD)"
      STATUS_NOW="$(gr status --porcelain)"

      if [ -z "$DEFAULT_BRANCH" ]; then
        # An --upstream override or a non-origin remote. This job restores a
        # checkout to the branch its REMOTE declares as default; it has no
        # licence to invent one from an arbitrary ref.
        ACTION="ABSTAINED"
        REASON="upstream_not_origin: the classifier compared against '${UPSTREAM:-<none>}', which does not name a branch on origin, so there is no default branch to return to"
        N_ABSTAIN=$((N_ABSTAIN + 1))
      elif [ -n "$STATUS_NOW" ]; then
        ACTION="ABSTAINED"
        REASON="raced_dirty: the tree was clean when it was classified and is not now -- a peer wrote to it during the sweep"
        N_ABSTAIN=$((N_ABSTAIN + 1))
      elif [ -z "$HEAD_NOW" ] || [ "$HEAD_NOW" != "$HEAD_BEFORE" ]; then
        # For an adjudicated row this is also the act-time re-check of
        # HEAD == SHA: HEAD_BEFORE was required to equal the adjudicated SHA.
        ACTION="ABSTAINED"
        REASON="raced_head_moved: HEAD was ${HEAD_BEFORE:-<unreadable>} before classification and is ${HEAD_NOW:-<unreadable>} now, so the verdict describes a state this checkout has left"
        N_ABSTAIN=$((N_ABSTAIN + 1))
      elif [ "$BRANCH" = "$DEFAULT_BRANCH" ] && [ "${DETACHED:-false}" != true ] && [ "$BEHIND" = "0" ]; then
        ACTION="NO_CHANGE"
        REASON="already on $DEFAULT_BRANCH and level with $UPSTREAM ($AGES)"
        N_NOCHANGE=$((N_NOCHANGE + 1))
        HEAD_AFTER="$HEAD_NOW"
      elif IGN_EXCLUDE=(); _rtm_ignored_collision "$BRANCH" "$DEFAULT_BRANCH" "$UPSTREAM" "${DETACHED:-false}"; then
        # git overwrites IGNORED files on checkout and merge; nothing snapshots
        # them. Refused before any ref is written.
        ACTION="ABSTAINED"
        REASON="ignored_path_overwrite: $IGN_HIT -- the switch to $DEFAULT_BRANCH or the fast-forward to $UPSTREAM adds a tracked path where an ignored (or untracked) file sits on disk, and git would overwrite it with nothing holding its bytes"
        N_ABSTAIN=$((N_ABSTAIN + 1))
      else
        # -- THE SNAPSHOT, before anything mutates ------------------------------
        # `git stash create` runs first as a READ and as the third race check:
        # it writes a commit object and touches neither the working tree nor the
        # index, and a NON-EMPTY answer means the tree is dirty after all.
        _stash="$(gr stash create)"; _stash="${_stash//[$'\r'$'\n']/}"
        if [ -n "$_stash" ]; then
          ACTION="ABSTAINED"
          REASON="raced_dirty: \`git stash create\` produced $_stash, so the tree holds uncommitted content despite reading clean a moment ago; the snapshot is kept at no ref and nothing is touched"
          N_ABSTAIN=$((N_ABSTAIN + 1))
        else
          _stamp="$(date -u +%Y%m%dT%H%M%SZ 2>/dev/null || printf 'nostamp')"
          _reposafe="$(printf '%s' "$REPO" | tr -c 'A-Za-z0-9._-' '_')"
          WIP_REF="refs/wip/return-to-main/${_stamp}-${_reposafe}-${HEAD_NOW:0:7}"
          _vword="LANDED_DUPLICATE"
          case "$VERDICT_SOURCE" in
            adjudicated)             _vword="LANDED_DUPLICATE adjudicated from $VERDICT, evidence ${EVIDENCE_SHA:-$EVIDENCE}" ;;
            adjudicated_superseded)  _vword="SUPERSEDED_BY_UPSTREAM adjudicated from $VERDICT, evidence ${EVIDENCE_SHA:-$EVIDENCE}" ;;
          esac
          _msg="return-to-main-sweep: $REPO leaving ${BRANCH:-<detached>} (verdict $_vword, upstream $UPSTREAM last fetched ${UP_FETCHED_AT:-at an unknown time}, tip committed ${UP_TIP_AGE:-at an unknown time})"

          if [ "$DRY_RUN" = 1 ]; then
            # `wip_ref` in the record stays NULL: a consumer reading that field
            # must get a ref that exists or nothing at all. The ref this run
            # WOULD have written is named in the reason instead.
            WIP_COMMIT=""
            if [ "$BRANCH" = "$DEFAULT_BRANCH" ] && [ "${DETACHED:-false}" != true ]; then
              ACTION="WOULD_FAST_FORWARD"
              REASON="on $DEFAULT_BRANCH, ${BEHIND:-an unknown number of commits} behind $UPSTREAM ($AGES); would fast-forward$ADJ_NOTE"
              N_FF=$((N_FF + 1))
            else
              ACTION="WOULD_RETURN"
              REASON="parked on ${BRANCH:-<detached HEAD>} (${AHEAD:-?} ahead, ${BEHIND:-?} behind) whose content is already on $UPSTREAM ($AGES); would snapshot $HEAD_NOW to $WIP_REF, switch to $DEFAULT_BRANCH and fast-forward$ADJ_NOTE"
              N_RETURNED=$((N_RETURNED + 1))
            fi
            case "$VERDICT_SOURCE" in adjudicated|adjudicated_superseded) N_ADJ_ACTED=$((N_ADJ_ACTED + 1)) ;; esac
            WIP_REF=""
          # `--create-reflog` is not decoration. git only keeps a reflog for
          # refs under refs/heads, refs/remotes, refs/notes and HEAD unless
          # `core.logAllRefUpdates=always`, so a plain `update-ref -m` into
          # refs/wip/ writes the ref and SILENTLY DISCARDS the message --
          # measured here 2026-09-04, and it is the message that makes the ref
          # self-describing once the log is gone. The flag is git 2.9+ (2016);
          # an older git is retried without it, and then the record's own
          # `branch` field is the only index, which is why the retry is a
          # fallback rather than the first spelling.
          elif ! gw update-ref --create-reflog -m "$_msg" "$WIP_REF" "$HEAD_NOW" 2>/dev/null \
            && ! gw update-ref -m "$_msg" "$WIP_REF" "$HEAD_NOW" 2>/dev/null; then
            # A snapshot that cannot be written is an ABSTENTION, not a
            # warning. §6 makes the ref the thing that makes a wrong verdict
            # survivable; acting without it removes the mitigation the whole
            # phase is predicated on.
            ACTION="ABSTAINED"
            REASON="snapshot_failed: could not write $WIP_REF -> $HEAD_NOW, and this job never switches a branch without a recoverable ref"
            WIP_REF=""
            N_ABSTAIN=$((N_ABSTAIN + 1))
          else
            WIP_COMMIT="$HEAD_NOW"
            _switch_err=""
            _switched=1
            if [ "$BRANCH" = "$DEFAULT_BRANCH" ] && [ "${DETACHED:-false}" != true ]; then
              _switched=0   # already there; fast-forward only
            elif gr rev-parse --verify --quiet "refs/heads/$DEFAULT_BRANCH" >/dev/null; then
              # Plain `checkout`: no -f, no -B. It REFUSES on a tree it would
              # have to discard rather than discarding it.
              _switch_err="$(gw checkout --quiet --no-overwrite-ignore "$DEFAULT_BRANCH" 2>&1)" || _switched=-1
            else
              _switch_err="$(gw checkout --quiet --no-overwrite-ignore -b "$DEFAULT_BRANCH" "$UPSTREAM" 2>&1)" || _switched=-1
            fi

            if [ "$_switched" = -1 ]; then
              case "$_switch_err" in
                *"already used by worktree"*|*"already checked out"*)
                  # Not a defect and not a failure: another worktree of this
                  # repo holds the default branch, which git forbids sharing.
                  # The snapshot ref stays -- it costs nothing and it records
                  # that this checkout was a candidate.
                  ACTION="ABSTAINED"
                  REASON="default_branch_checked_out_elsewhere: $DEFAULT_BRANCH is checked out in another worktree of this repo, so git will not check it out here ($_switch_err)"
                  N_ABSTAIN=$((N_ABSTAIN + 1))
                  ;;
                *)
                  ACTION="FAILED"
                  REASON="switch to $DEFAULT_BRANCH failed: $_switch_err (nothing was forced or retried; the checkout is as git left it, and $WIP_REF holds $HEAD_NOW)"
                  N_FAILED=$((N_FAILED + 1))
                  ;;
              esac
            else
              # -- The fast-forward. `--ff-only` REFUSES a non-fast-forward
              # rather than creating a merge commit, which is the property that
              # keeps this job from ever resolving a conflict.
              # No `behind == 0` arm here: that case is settled above, before
              # any ref is written. Reaching this point means either the branch
              # changed or the count is non-zero or UNKNOWN, and `--ff-only`
              # against an already-level upstream is a documented no-op success.
              _ff_err=""
              if _ff_err="$(gw merge --ff-only --no-overwrite-ignore "$UPSTREAM" 2>&1)"; then
                if [ "$_switched" = 0 ]; then
                  ACTION="FAST_FORWARDED"
                  REASON="on $DEFAULT_BRANCH, fast-forwarded to $UPSTREAM ($AGES)$ADJ_NOTE"
                  N_FF=$((N_FF + 1))
                else
                  ACTION="RETURNED"
                  REASON="left ${BRANCH:-<detached HEAD>} (${AHEAD:-0} ahead, all already on $UPSTREAM) for $DEFAULT_BRANCH and fast-forwarded ${BEHIND:-0} commits ($AGES); snapshot $WIP_REF -> $HEAD_NOW$ADJ_NOTE"
                  N_RETURNED=$((N_RETURNED + 1))
                fi
                case "$VERDICT_SOURCE" in adjudicated|adjudicated_superseded) N_ADJ_ACTED=$((N_ADJ_ACTED + 1)) ;; esac
              else
                # Switched but could not fast-forward. Reported as a FAILURE
                # with both halves named, because the checkout is now on the
                # default branch and still behind -- a state a reader must not
                # have to infer.
                ACTION="FAILED"
                if [ "$_switched" = 0 ]; then
                  REASON="on $DEFAULT_BRANCH, but the fast-forward to $UPSTREAM was refused: $_ff_err (no merge was created; snapshot $WIP_REF -> $HEAD_NOW)"
                else
                  REASON="switched to $DEFAULT_BRANCH but the fast-forward to $UPSTREAM was refused: $_ff_err (no merge was created; snapshot $WIP_REF -> $HEAD_NOW)"
                fi
                N_FAILED=$((N_FAILED + 1))
              fi
            fi
            HEAD_AFTER="$(gr rev-parse HEAD)"
          fi
        fi
      fi
    fi
    # A residue restore that ran, and then an acting arm that did not act, is
    # still recorded on the reason line (the restore itself is in residue_*).
    case "$RES_OUTCOME" in
      restored) [ "$ACTION" = ABSTAINED ] && REASON="residue restored first ($RES_REASON); then $REASON" ;;
    esac
  fi

  # A restore that ran but was not followed by a fast-forward: the restored
  # files hold HEAD's bytes, and only the snapshot holds what was there.
  if [ "$RES_OUTCOME" = restored ]; then
    case "$ACTION" in
      FAST_FORWARDED|RETURNED|NO_CHANGE) ;;
      *) RES_NO_FF=true
         REASON="$REASON; residue_restored_no_ff: the residue restore ran but no fast-forward followed (action $ACTION), so the pre-restore bytes are held only by the snapshot ${RES_WIP_REF:-<none>}" ;;
    esac
  fi
  [ -n "$HEAD_AFTER" ] || HEAD_AFTER="$(gr rev-parse HEAD)"
  _vcol="$VERDICT"
  case "$VERDICT_SOURCE" in
    adjudicated)            _vcol="$VERDICT>ADJUDICATED" ;;
    adjudicated_superseded) _vcol="$VERDICT>ADJUDICATED_SUPERSEDED" ;;
  esac
  ROWS+=("$REPO|${BRANCH:-?}|$_vcol|$ACTION|$REASON")
  emit_decision
done

# Repo names passed to --adjudicated-landed / --restore-residue that were never
# examined (a typo, an --only that excluded them, the --max cap). Reported, not
# silently dropped: an adjudication that matched nothing did nothing.
UNMATCHED=()
for _r in ${ADJ_REPOS[@]+"${ADJ_REPOS[@]}"} ${RESIDUE_REPOS[@]+"${RESIDUE_REPOS[@]}"}; do
  if ! in_list "$_r" ${EXAMINED_REPOS[@]+"${EXAMINED_REPOS[@]}"} && ! in_list "$_r" ${UNMATCHED[@]+"${UNMATCHED[@]}"}; then
    UNMATCHED+=("$_r")
  fi
done

END_EPOCH="$(date -u +%s 2>/dev/null || printf 0)"
ELAPSED=$((END_EPOCH - START_EPOCH))
[ "$ELAPSED" -ge 0 ] || ELAPSED=0

log_line "{\"ts\":$(js "$(now_iso)"),\"event\":\"run_end\",\"mode\":$(js "$MODE_WORD"),\"examined\":$(jn "$N_EXAMINED"),\"returned\":$(jn "$N_RETURNED"),\"fast_forwarded\":$(jn "$N_FF"),\"no_change\":$(jn "$N_NOCHANGE"),\"abstained\":$(jn "$N_ABSTAIN"),\"failed\":$(jn "$N_FAILED"),\"fetched\":$(jn "$N_FETCHED"),\"fetch_failed\":$(jn "$N_FETCH_FAILED"),\"adjudicated_acted\":$(jn "$N_ADJ_ACTED"),\"residue_restored\":$(jn "$N_RES_RESTORED"),\"unmatched_repo_args\":$(ja ${UNMATCHED[@]+"${UNMATCHED[@]}"}),\"capped_out\":$(jn "$CAPPED"),\"elapsed_sec\":$(jn "$ELAPSED")}"

# ---------------------------------------------------------------------------
# report

if [ "$JSON_OUT" = 1 ]; then
  printf '{"tool":"return-to-main-sweep.sh","schema":2,"mode":%s,"root":%s,"log":%s,"log_state":%s,"fetch":%s,' \
    "$(js "$MODE_WORD")" "$(js "$ROOT")" "$(jsn "$LOG")" "$(js "$LOG_STATE")" "$(jb "$FETCH")"
  printf '"examined":%s,"returned":%s,"fast_forwarded":%s,"no_change":%s,"abstained":%s,"failed":%s,"fetched":%s,"fetch_failed":%s,"adjudicated_acted":%s,"residue_restored":%s,"unmatched_repo_args":%s,"capped_out":%s,"elapsed_sec":%s}\n' \
    "$(jn "$N_EXAMINED")" "$(jn "$N_RETURNED")" "$(jn "$N_FF")" "$(jn "$N_NOCHANGE")" \
    "$(jn "$N_ABSTAIN")" "$(jn "$N_FAILED")" "$(jn "$N_FETCHED")" "$(jn "$N_FETCH_FAILED")" \
    "$(jn "$N_ADJ_ACTED")" "$(jn "$N_RES_RESTORED")" "$(ja ${UNMATCHED[@]+"${UNMATCHED[@]}"})" \
    "$(jn "$CAPPED")" "$(jn "$ELAPSED")"
else
  printf 'return-to-main-sweep: %s under %s\n' "$([ "$DRY_RUN" = 1 ] && printf 'DRY RUN (nothing was touched)' || printf 'sweep')" "$ROOT"
  printf '\n  %-28s %-42s %-18s %s\n' REPO BRANCH VERDICT ACTION
  for r in ${ROWS[@]+"${ROWS[@]}"}; do
    IFS='|' read -r _r _b _v _a _why <<< "$r"
    printf '  %-28s %-42s %-18s %s\n' "$_r" "$_b" "$_v" "$_a"
    [ -n "$_why" ] && printf '  %-28s   %s\n' '' "$_why"
  done
  printf '\n  examined %s: %s %s, %s %s, %s already current, %s abstained, %s failed (%ss)\n' \
    "$N_EXAMINED" "$N_RETURNED" "$([ "$DRY_RUN" = 1 ] && printf 'WOULD be returned' || printf 'returned')" \
    "$N_FF" "$([ "$DRY_RUN" = 1 ] && printf 'WOULD be fast-forwarded' || printf 'fast-forwarded')" \
    "$N_NOCHANGE" "$N_ABSTAIN" "$N_FAILED" "$ELAPSED"
  [ "$N_ADJ_ACTED" -gt 0 ] && printf '  %s of those acted on an ADJUDICATED verdict (evidence %s)\n' "$N_ADJ_ACTED" "$EVIDENCE"
  [ "$N_RES_RESTORED" -gt 0 ] && printf '  %s checkout(s) had proven residue restored first (snapshots under refs/wip/return-to-main/*-residue)\n' "$N_RES_RESTORED"
  [ "${#UNMATCHED[@]}" -gt 0 ] && printf '  NOT examined, though named by --adjudicated-landed / --adjudicated-superseded / --restore-residue: %s\n' "${UNMATCHED[*]}"
  [ "$CAPPED" -gt 0 ] && printf '  %s further candidate(s) were NOT examined: --max %s. That is a cap, not an all-clear.\n' "$CAPPED" "$MAX"
  if [ -n "$LOG" ]; then
    printf '  log: %s (every action AND every abstention, one JSON object per line)\n' "$LOG"
  else
    printf '  log: NONE for this run (%s) -- decisions above are not durable.\n' "$LOG_STATE"
  fi
  if [ "$FETCH" = 1 ]; then
    printf '\n  FETCHED %s of %s checkout(s) before classifying them. %s fetch(es) FAILED; those\n' "$N_FETCHED" "$N_EXAMINED" "$N_FETCH_FAILED"
    printf '  verdicts are FLOORS relative to the upstream ref AS LAST FETCHED (see each row'"'"'s\n'
    printf '  upstream_fetched_at). A stale ref inflates the abstentions; it can never fabricate a return.\n'
  else
    printf '\n  FLOOR, not a measurement of the remote. This run did not fetch (--fetch does), so\n'
    printf '  every verdict and every count is relative to the upstream ref AS LAST FETCHED --\n'
    printf '  each row names when that was. A stale ref inflates the abstentions; it can never\n'
    printf '  fabricate a return.\n'
  fi
  printf '  Recovery for any RETURNED row: `git checkout <branch>` (the branch ref is never\n'
  printf '  deleted), or `git for-each-ref refs/wip/return-to-main` for the snapshot.\n\n'
fi

[ "$N_FAILED" -gt 0 ] && exit 1
exit 0
