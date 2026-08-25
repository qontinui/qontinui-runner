# Publish Runner

Cut and publish a new **qontinui-runner** desktop release (Tauri v2, Windows NSIS installer) with a working auto-updater. Bumps the version everywhere, tags a CI-green commit, lets the release workflow build + sign + auto-publish, then verifies the release, the updater endpoint, and the web download actually flipped.

Argument: the target version, e.g. `/publish-runner 1.0.5`. If omitted, read the current version (Phase 1) and propose the next patch bump for confirmation.

Repo: `qontinui/qontinui-runner`. Working tree: `<workspace-root>/qontinui-runner` (path-dep on sibling `../../qontinui-schemas` — never clone the runner elsewhere or the build won't resolve).

---

## How the release actually ships (mental model — read once)

- **Everything is done in GitHub — you never need a private key locally.** The build, the minisign signing, and the publish all happen inside GitHub Actions on the tag push. Running this command is: bump the version, land it, push a tag. That's it. The signing key exists **only as the `TAURI_SIGNING_PRIVATE_KEY` repo secret** (already configured on `qontinui/qontinui-runner`) — CI reads it to sign; nobody running `/publish-runner` holds, sets, or supplies a private key on their machine. Phase 0's secret check is a **read-only confirmation that the repo secret still exists**, not a step where you provide a key.
- **Trigger is a tag push**, not a branch push. `.github/workflows/release.yml` fires on `push: tags: v*`. Pushing a `vX.Y.Z` tag is what starts a release; nothing else does.
- The workflow **creates a DRAFT release, builds every platform, and auto-un-drafts** to `--latest` **only after the Windows `-setup.exe` uploads** (`publish-update-json` job, hard-gated on the Windows asset via `always() && create-release==success`). macOS/Linux legs are `continue-on-error` — a red mac/linux leg does NOT block publish.
- **The macOS-arm64 leg is slow** (~much longer than Windows). Auto-publish waits on the whole matrix. If Windows is green and its assets are up but the release is still a draft because mac-arm64 is dragging, you can **publish by hand immediately** (Phase 5 fallback) — the Windows asset gate is the only thing that actually matters.
- **Auto-update is real as of v1.0.3.** The Windows leg signs each bundle (minisign, `createUpdaterArtifacts: true`) using repo secrets `TAURI_SIGNING_PRIVATE_KEY` + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, emits `<installer>.sig`, and assembles a **real-signed `latest.json`**. The updater endpoint is `releases/latest/download/latest.json`. The public key is embedded in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). **DO NOT change the pubkey** — rotating it breaks auto-update for every existing install and forces everyone to reinstall.

---

## Phase 0: Preconditions (fail fast)

```bash
cd <workspace-root>/qontinui-runner
# 1. Confirm the signing secrets still exist ON THE REPO (GitHub-side; CI reads
#    them to sign). This is a read-only presence check — you do NOT supply a key.
#    NOTE: the row for the private key is tab-delimited, so `grep " "` can miss it;
#    just eyeball the full list if the grep looks short.
gh secret list -R qontinui/qontinui-runner | grep -E "TAURI_SIGNING_PRIVATE_KEY( |	|_PASSWORD)"
# 2. main must be GREEN — a red required check (usually a fresh RUSTSEC advisory
#    on the `security` cargo-audit job) blocks the merge train fleet-wide AND
#    means you'd be tagging on top of red.
gh run list -R qontinui/qontinui-runner --branch main --limit 3 --json workflowName,conclusion,headSha
```

Both secrets MUST be present **on the repo** (that's all this checks — GitHub-side presence). They were set once and persist; signing happens in CI, so there is nothing to supply locally when publishing. If one has genuinely gone missing, STOP — this is the rare case where the operator (who holds the key material out-of-band) must re-set it: PowerShell `gh secret set NAME -R qontinui/qontinui-runner --body (Get-Content path -Raw).Trim()` — NOT `< file`, PowerShell doesn't support `<`. Do NOT block a normal publish on generating or having a private key yourself; you never need one.

**If `security` (cargo-audit) is red on main**, it HOLDS the merge train — but it does not deadlock every merge. `main-red` is checked ONLY at ENQUEUE (Tier 4 of `pr_merge::predicate::is_simple_green_path`) and is never re-consulted at land; two further routes (`POST /merge/propose`, `engine::enqueue_merge_proposal_for_pr`) enqueue without the predicate at all. So a PR can still land under a red main: measured 2026-08-20, `qontinui-runner#1076` got `block_reason_code: "main-red"` from a live `/reevaluate` and **landed anyway** at 08:41:31Z with `mergedBy = app/qontinui-merge-orchestrator` — coord itself, no human, no `--admin`; which route had enqueued it was NOT established. ⚠️ **Before assuming a cause at all, check that the job actually failed.** `gh run list --json conclusion` reads the RUN level, where an infrastructure kill is indistinguishable from a real red: fetch the failed job's steps (`gh api repos/qontinui/qontinui-runner/actions/runs/<run_id>/jobs?per_page=100`) and if it has **no step whose `conclusion` is `"failure"`** (or no steps at all), it is an infra kill — re-run it and publish, do not go hunting an advisory that does not exist. Scope this honestly: `qontinui-runner` has **zero self-hosted runners** (`GET /repos/qontinui/qontinui-runner/actions/runners` → `total_count: 0`), so the common self-hosted shape cannot occur here and the exposure is the GitHub-hosted one, of which the 2026-08-20 fleet sweep saw **one** — rare, not absent, and cheap to rule out. Predicate and derivation: `.claude/commands/babysit-prs.md` Step 3 / `.claude/commands/merge-train-steward.md` → “The `failure`-side discriminator is STEP-LEVEL”. Once the job is confirmed genuinely red, two distinct causes:
- **A new RustSec advisory** against a runner dependency: bump the flagged crate (`cargo update -p <crate> --precise <patched>`), verify `cargo audit` exits 0. ⚠️ **Do NOT route this "via the `coord:red-main-fix` recovery lane" — there is no such lane to route through, and for a security-class fix least of all.** Three independent reasons: (a) the label is **convenience/intent only and is NOT an input to the predicate** — `policies::evaluator::is_recovery_candidate` says so verbatim ("a mislabeled (or unlabeled) PR is judged purely on these facts"), which is deliberate — a mislabeled PR "can never force-land unless it truly fixes main", and labelling yours changes nothing; (b) the in-predicate waiver it names is **INERT in prod** — `is_recovery_candidate` requires `rebased_candidate_green`, whose only producer is `pr_merge::engine::head_has_green_speculative_candidate` (a green, non-invalidated `coord.speculative_chains` row), and speculative candidate CI is OFF (`deploy/taskdef.json` sets `COORD_SPECULATIVE_DISABLED="1"` against an inverted-sense read, `!= Ok("0")`), so no such row is ever produced — coord's own `fixer_arm_readiness::adjacent_breakages` carries the entry `red_main_recovery_merge_lane_inert`; (c) even if it fired, the predicate requires `!security_class_touched`, so it refuses security-class PRs **by design** — a cargo-audit bump is exactly that. **So never wait for the waiver to fire; that wait never ends.** Open the fix PR green and non-draft and let coord's ordinary merge path land it; never `--admin`. Applying the label as intent signalling is still fine, but set it with `gh pr edit --add-label` — `pr_merge::labels_routes::validate_label` **rejects** `coord:red-main-fix`, so `/coord-pr-label` cannot. Full derivation of all three points is in `.claude/commands/merge-train-steward.md`. See `reference_runner_rustsec_redmain_cargoaudit_coord_recovery`.
- **`cargo install cargo-audit` fails to COMPILE** (exit 101, ~20s fast-fail, log says e.g. "kstring@X requires rustc 1.96.0 / Try --locked"): this is upstream *tooling* MSRV drift, NOT an advisory and NOT a runner vuln. CI pins rustc 1.95.0 (`rust-toolchain.toml` + ci.yml dtolnay); the unpinned install pulled a too-new transitive dep. **FIX = add `--locked` to `cargo install cargo-audit` in `.github/workflows/ci.yml`** (installs cargo-audit against its own tested lockfile). Do NOT bump the toolchain — huge blast radius and it still edits a gated workflow. This happened on the v1.0.5 cut (2026-07-14, kstring 2.0.3).

**Editing `ci.yml` (or any gating workflow) trips the `ci-integrity.yml` guard** ("Guard gating workflows from self-edits") which reds CI by design. BUT that guard is NOT a required context in the ruleset — so an armed auto-merge STILL lands the PR once the actually-required checks (security + test ubuntu/windows) pass. It's an advisory red, not a hard block; don't assume you need `--admin` for a workflow-edit PR — check `mergeStateStatus` / whether it merges on its own first. (On v1.0.5, folding the `--locked` fix into the release-bump PR #772 auto-merged with no admin override.)

## Phase 1: Read current version + decide target

```bash
cd <workspace-root>/qontinui-runner
grep -m1 '"version"' src-tauri/tauri.conf.json
gh release list -R qontinui/qontinui-runner --limit 3
```

The published GitHub "latest" is the real production version — trust it over local files (local may be mid-bump on a feature branch). Target = next version per the user's arg, or propose `latest + patch`.

## Phase 2: Bump the version in ALL FOUR places

The version lives in four files and they MUST match, or the build is inconsistent (Cargo.lock mismatch fails `--locked`; a stale tauri.conf serves the wrong updater version):

1. `src-tauri/tauri.conf.json` → `"version"` (top-level)
2. `src-tauri/Cargo.toml` → `version = "…"` (package, near line 3)
3. `package.json` → `"version"`
4. `Cargo.lock` → the `[[package]] name = "qontinui-runner"` entry's `version`

Edit files 1–3 with Edit. For `Cargo.lock`, prefer `cargo update -p qontinui-runner --precise <version>` (or edit the one entry by hand). Do NOT run a bare `cargo update` — it churns unrelated crates and can pull in a fresh RustSec advisory that reds `security`.

**Do these bumps on a normal feature branch and land them through the merge train** — the tag must point at a commit that already exists and is CI-green on main. Do not tag an unmerged local commit.

## Phase 3: Land the bump, then tag a green commit

1. Open a PR with the four version bumps (plus any release-note edits) and let coord's merge train land it once green. **Do not run `gh pr merge` or `--admin`** — coord is the sole merge authority for `qontinui/*` repos (CLAUDE.md; coord-served policy `git-operations` `merge-authority`). The tag in Phase 3 must point at a commit that is already on `main` and green, so waiting for coord is a real dependency here, not a formality. **Gotcha:** if you re-point a PR's base, `pull_request` CI does NOT re-fire — `gh pr close <n> && gh pr reopen <n>` to force a `reopened` event.
2. Confirm the merge commit is green on main, then tag **that exact SHA**:

```bash
cd <workspace-root>/qontinui-runner
git fetch origin main
SHA=$(git rev-parse origin/main)          # or the specific merge SHA you verified green
git tag v<version> $SHA
git push origin v<version>                 # <-- this is what launches the release
```

The tag push is the release trigger. Watch it:

```bash
gh run list -R qontinui/qontinui-runner --workflow=release.yml --limit 3
gh run watch -R qontinui/qontinui-runner <run-id>
```

**Local-push clippy gotcha:** `cargo-prepush` runs `clippy --all-targets` (stricter than CI) and pre-existing test-file lints can block an unrelated push. Bypass with `QONTINUI_PREPUSH_SKIP=1 git push …`. (Tag pushes usually don't trip this, but branch pushes for the bump PR can.)

## Phase 4: Let it build + auto-publish

The workflow: creates the draft → builds the matrix → Windows leg signs + writes `latest.json` → `publish-update-json` verifies the Windows `-setup.exe` is present → un-drafts to `--latest`.

Green Windows leg + assets uploaded ⇒ it auto-publishes. If you're watching and Windows is done but the release is still a draft because mac-arm64 is slow, go to Phase 5's manual publish — don't wait it out.

**Do NOT re-diagnose an `os error 2` NSIS failure as the resources glob or the NSIS toolchain** — that was resolved (real cause was the `--target` flag, PR #684, long merged). If the Windows leg genuinely fails now, read the actual error; it's something new. See memory `reference_runner_release_nsis_os_error2_not_resources_glob`.

## Phase 5: Publish manually (fallback — only if auto-publish is gated)

If Windows is green + its assets are up but the release is still a draft (slow/failed non-blocking leg):

```bash
# Confirm the Windows installer + signed manifest are actually on the release first:
gh release view v<version> -R qontinui/qontinui-runner --json assets --jq '.assets[].name'
# Expect: Qontinui.Runner_<version>_x64-setup.exe, .exe.sig, latest.json, checksums-windows-x64.txt
gh release edit v<version> -R qontinui/qontinui-runner --draft=false --prerelease=false --latest
```

Never publish a release missing the `-setup.exe` or `latest.json` — an assetless "latest" breaks both the web download and the updater.

## Phase 6: Verify the release is genuinely live (do NOT skip)

Every pre-v1.0.3 release *looked* configured but shipped empty signatures. Verify for real:

```bash
# 1. GitHub "latest" is the new version
gh release list -R qontinui/qontinui-runner --limit 3   # top row should show "Latest" on v<version>

# 2. Updater endpoint serves the new version with a REAL (non-empty) signature
curl -sL https://github.com/qontinui/qontinui-runner/releases/latest/download/latest.json | head -c 500
#    -> "version":"v<version>" and a long base64 "signature" that decodes to
#       "signature from tauri secret key" (NOT empty, NOT the pubkey).

# 3. The manifest signature matches the uploaded .sig byte-for-byte
#    (download both; the "signature" field in latest.json must equal the .sig file contents)

# 4. Web download flips to the new version (may lag GitHub by a bit / needs auth on the page route)
curl -sI https://github.com/qontinui/qontinui-runner/releases/latest/download/Qontinui.Runner_<version>_x64-setup.exe | grep -i location
```

All four must pass. Only then is the release real.

## Phase 7: Report + record

Tell the user:
- New version published + verified (GitHub latest, updater endpoint, signature match, web download).
- **Whether existing users must reinstall or auto-update carries them.** Auto-update works v1.0.3 → onward. Any user on a release that shipped *without* a working updater (≤ v1.0.2) must reinstall **once**; from a working-updater version they auto-update.
- If anything in Phase 6 failed, say exactly which check and stop — do not claim success.

Update memory `reference_runner_device_autoinit_and_autoupdate_v103` (or a successor) with the new version + any new gotcha.

---

## Notes / invariants

- **Four version files, always in lockstep:** tauri.conf.json, src-tauri/Cargo.toml, package.json, Cargo.lock.
- **Never touch `plugins.updater.pubkey`** in tauri.conf.json. Signing-key custody is load-bearing and irreversible-ish — losing/rotating the private key forces a fleet-wide manual reinstall.
- **Tag = release trigger.** No tag, no build. Tag only a CI-green commit that's already on main.
- **Windows is the sole hard gate;** mac/linux legs are non-blocking (`continue-on-error`). If mac-arm64 stalls auto-publish, publish by hand once Windows assets are up.
- **The web download endpoints query GitHub's "latest"** — that's why the un-draft step passes `--latest`. If a release isn't marked latest, the website keeps serving the old one.
- **`os error 2` is NOT the NSIS glob/toolchain** (resolved via #684 `--target`). Read the real error before theorizing.
- **cargo-audit / RUSTSEC** can red `security` on main from an upstream advisory, holding new proposals (incl. the release bump) at ENQUEUE — fix the crate first, then open the fix PR normally and let coord land it. **There is no working `coord:red-main-fix` recovery lane**: the label is not a predicate input, the waiver it names is inert in prod, and the predicate excludes security-class changes anyway (`!security_class_touched`). Never `--admin`; detail in the `security`-red section above.
- **Base re-point doesn't re-fire CI** — `gh pr close && gh pr reopen`.
- **PowerShell has no `<`** — `gh secret set … --body (Get-Content path -Raw).Trim()`.
- **prepush clippy is stricter than CI** — `QONTINUI_PREPUSH_SKIP=1 git push` to bypass pre-existing unrelated lints.
