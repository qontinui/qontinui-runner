<!-- BEGIN License & DCO preamble — updated 2026-05-30 (CLA→DCO per open/closed license alignment) -->

## License & contributions

This project is licensed under the **GNU Affero General Public License v3.0 or later** (`AGPL-3.0-or-later`). See [`LICENSE`](LICENSE) for the full text. Contributors should be aware:

- AGPL is a strong copyleft license. Anyone who runs a modified version of this project as a network service must offer its users the Corresponding Source under AGPL too.
- For typical self-hosting, internal use, forking, or contributing back, AGPL behaves like GPL.

Contributions are accepted under the **Developer Certificate of Origin (DCO) 1.1** — *not* a CLA. The DCO text lives in [`DCO.txt`](DCO.txt). Certify that you wrote (or otherwise have the right to submit) your contribution by adding a `Signed-off-by` trailer to every commit:

    git commit -s -m "your message"

which appends `Signed-off-by: Your Name <your@email>` from your `git config user.name` / `user.email`. Your contributions are licensed inbound under the same `AGPL-3.0-or-later` as the project (inbound = outbound); you retain copyright in your contributions. No relicensing rights are granted — this repository is one of the apps where Qontinui does not need the dual-/commercial-license lever (that lever is retained only on the embeddable `ui-bridge` library via its CLA).

The remainder of this document covers contribution mechanics specific to this repository.

<!-- END License & DCO preamble -->

# Contributing to Qontinui Runner

Thank you for your interest in contributing to Qontinui Runner! This document provides guidelines for contributing to the desktop application.

## Code of Conduct

Be respectful, constructive, and collaborative. We're all here to build something useful together.

## How to Contribute

### Reporting Bugs

1. Check if the bug has already been reported in [Issues](https://github.com/yourusername/qontinui-runner/issues)
2. If not, create a new issue with:
   - Clear title describing the problem
   - Steps to reproduce
   - Expected vs actual behavior
   - Operating system and version
   - Screenshots if applicable
   - Console logs from dev tools (if available)

### Suggesting Features

1. Check existing [Issues](https://github.com/yourusername/qontinui-runner/issues)
2. Create a new issue describing:
   - The problem you're trying to solve
   - Your proposed solution
   - Example use cases
   - UI mockups if applicable

### Pull Requests

1. **Fork the repository** and create a branch from `main`
2. **Install dependencies:**

   ```bash
   # Install Node dependencies
   npm install

   # Install Rust (if not already installed)
   # https://rustup.rs/

   # Install Python dependencies for bridge
   cd python-bridge
   pip install -r requirements.txt
   cd ..
   ```

3. **Make your changes:**
   - Frontend (React/TypeScript): Follow existing patterns
   - Backend (Tauri/Rust): Follow Rust best practices
   - Python bridge: Follow Python style guide
   - Write clear, documented code
   - Add tests for new functionality

4. **Test your changes:**

   ```bash
   # Run in development mode
   npm run tauri dev

   # Run frontend tests
   npm test

   # Build for production
   npm run tauri build
   ```

5. **Commit your changes:**
   - Use clear commit messages
   - Reference issues when applicable

6. **Push to your fork** and submit a pull request

7. **Address review feedback** if requested

## Development Setup

### Prerequisites

- **Node.js** 18+ and npm
- **Rust** 1.70+ (via rustup)
- **Python** 3.10+
- **Qontinui library** installed (`poetry install` in qontinui repo)
- **MultiState library** installed (`poetry install` in multistate repo)

### Setup Steps

```bash
# Clone your fork
git clone https://github.com/yourusername/qontinui-runner.git
cd qontinui-runner

# Install frontend dependencies
npm install

# Install Python bridge dependencies
cd python-bridge
pip install -r requirements.txt
cd ..

# Run in development mode
npm run tauri dev
```

## Project Structure

```
qontinui-runner/
├── src/                    # React frontend (TypeScript)
│   ├── components/         # React components
│   ├── services/           # API services
│   └── App.tsx            # Main app component
├── src-tauri/             # Tauri backend (Rust)
│   ├── src/               # Rust source code
│   └── Cargo.toml         # Rust dependencies
├── python-bridge/         # Python bridge to qontinui
│   └── qontinui_bridge.py # Bridge implementation
└── public/                # Static assets
```

## Code Style

### Frontend (TypeScript/React)

- Use TypeScript for type safety
- Follow React hooks patterns
- Use functional components
- Format with Prettier
- Lint with ESLint

### Backend (Rust)

- Follow Rust naming conventions
- Use `cargo fmt` for formatting
- Run `cargo clippy` for linting
- Handle errors properly (Result types)

#### Subprocesses must be time-bounded — enforced in CI

`std::process::Command::output()` / `status()` and `Child::wait()` /
`wait_with_output()` have **no timeout**. A child that never exits parks the
calling thread forever, and when that thread came from tokio's blocking pool it
is never given back. tokio's default cap is 512 blocking threads; on 2026-08-30
eight independent *periodic* callers exhausted it, which starved the PG pool,
disabled `zombie_sweep`, and took `/livez` dark. The same defect shape had
already been found and fixed three times before that without review catching
the next one, so it is now a machine check.

Route the built command through one of the bounded wrappers in
[`src-tauri/src/process_helpers.rs`](src-tauri/src/process_helpers.rs):

| Wrapper | Use it when |
|---|---|
| `run_probe(cmd, timeout, label) -> ProbeOutcome` | "shell out, read stdout, degrade" — the overwhelmingly common shape |
| `output_with_timeout(cmd, timeout) -> io::Result<Output>` | drop-in for `.output()` where you already match on `Output` |
| `run_with_timeout(cmd, timeout) -> io::Result<TimedOutput>` | the base primitive, when you want to handle expiry yourself |

All three kill **and reap** the child on expiry, so a hung subprocess cannot
outlive the call. `.spawn()` is fine and is not gated — a deliberately-detached
long-lived child (the python sidecar, ffmpeg, rathole, a claude CLI session)
parks nothing. The defect is the unbounded *wait*.

`.github/workflows/forbid-untimed-subprocess.yml` fails the build on a new
untimed site. Run it yourself before pushing:

```bash
python3 scripts/check_untimed_subprocess.py          # Windows: python scripts\check_untimed_subprocess.py
python3 scripts/check_untimed_subprocess.py --list   # show every sync wait it can see
```

The surviving sites are enumerated with a written reason each in
[`scripts/untimed-subprocess-baseline.json`](scripts/untimed-subprocess-baseline.json).
It is a ratchet — the per-function count is checked in both directions, so it
can only shrink. Adding an entry for anything on a **timer or a hot path** is a
policy violation, not a lint fix: bound the call instead.

### Python Bridge

- Follow PEP 8
- Use type hints
- Format with `ruff format`
- Minimal code - delegate to qontinui library

## Testing

### Dev loop — test your change without restarting the primary

**Never restart the primary runner to test a code change — build it into an
isolated temp runner instead.** The supervisor (`:9875`) can compile any
worktree or git ref into its own throwaway runner (own port 9877–9899, own UI
Bridge) with **zero impact on the primary** (`:9876`), so a restart is never
the way to see your change.

The first-class way to do this from the runner UI: **Settings → "Test My
Change"**. Pick a detected worktree / a git ref / a worktree path, click
**Build & launch test runner**, then **Open** the spawned runner to verify.
**Stop** it when done. Under the hood this is a single
`POST /runners/spawn-test` with a provenance selector:

```bash
# build an isolated temp runner from a branch (clean checkout) and wait for health
curl -X POST localhost:9875/runners/spawn-test \
  -H 'content-type: application/json' \
  -d '{"rebuild":true,"git_ref":"feat/my-change","wait":true}'
# …or from an existing worktree (uncommitted edits included):
#   -d '{"rebuild":true,"worktree_path":"D:/.../qontinui-runner","wait":true}'
curl -X POST localhost:9875/runners/<id>/stop   # clean up when done
```

Always check the response `source` field (`worktree`/`worktree_path`/`git_ref`
vs `live_tree`) and `git_sha` to confirm what actually got built. `git_ref`
and `worktree_path` are mutually exclusive and both require `rebuild:true`.

### Diagnosing a wedged runtime — `tokio-console` (dev-only)

When the runner is alive but answering nothing (blocking-pool exhaustion, a
parked worker thread), a thread dump tells you the process is stuck but not
*which async task* is stuck or for how long. The `debug-tokio-console` Cargo
feature layers `console-subscriber` alongside the normal logging stack so the
`tokio-console` client can read the runtime's own task graph.

```bash
cargo install --locked tokio-console      # once
scripts/dev-tokio-console.sh run          # bash / WSL
scripts\dev-tokio-console.ps1 -Action run # PowerShell
tokio-console http://127.0.0.1:6669       # second terminal
```

It is **off by default and must not ship** — it requires the build-wide
`--cfg tokio_unstable` rustc flag, which is deliberately set nowhere in this
repository. Note that changing `RUSTFLAGS` invalidates the whole build cache.
Full write-up, including why you should use the wrapper scripts rather than
setting `RUSTFLAGS` by hand: [`src-tauri/docs/tokio-console.md`](src-tauri/docs/tokio-console.md).

### Frontend Tests

```bash
npm test
```

### End-to-End Testing

1. Build the app: `npm run tauri build`
2. Install and test the built application
3. Test on target platforms (Windows/Mac/Linux)

### Python Bridge Tests

```bash
cd python-bridge
pytest
```

### Rust unit tests — env-var hygiene

Cargo runs tests within a binary in parallel by default. Two tests in the same binary that touch the same process-wide env var will race: one mutates it, the other reads it expecting the unset (or differently-set) state. The race is OS-sensitive — macOS/Windows tend to finish the env-toggling test fast enough to hide the leak; Ubuntu CI exposes it routinely.

**Canonical pattern: [`src-tauri/src/startup_panic.rs::tests`](src-tauri/src/startup_panic.rs).** That module ships the full shape — module-local `static ENV_LOCK: Mutex<()>` for inter-test serialization, an `EnvGuard` RAII `Drop` that clears the touched vars on every exit path (including panics), and `.lock().unwrap_or_else(|e| e.into_inner())` poison recovery so a panicking test doesn't cascade-fail siblings. Copy that shape verbatim before reaching for `serial_test` or rolling your own. If your test mutates a different env var, name the guard accordingly (e.g. `QontinuiPortGuard` in `scheduler_service.rs::tests`); if multiple modules touch the same var, promote the lock to a shared module.

Avoid the half-pattern (lock without RAII, or RAII without poison recovery): the env state leaks across tests on every panic. See PRs #82 and #95 for examples of retrofitting the full shape onto modules that had one or both halves missing.

## CI & Merge Readiness

A PR is ready to merge when every required workflow is green on the PR's HEAD commit. Don't merge through red, and don't assume someone else's red is "fine" because `main` is also red — that's how `main` ended up with a 685-run failure streak going back to 2025-09-24.

That is the condition you can see. There is a second one you can't: coord scores `main` itself before it will **enqueue** your PR, and a red there blocks the PR with `block_reason_code: "main-red"` no matter how green your HEAD is. See "Advisory on a PR is not harmless on `main`" below — a workflow can be advisory at PR time and still hold the whole merge train shut from `main`.

### What "main is green" means here

Workflows in `.github/workflows/` split into three tiers. The authoritative list of what actually blocks a merge is the `main-merge-gates` ruleset (see "Branch protection" below) — it pins **check-context names**, so this section and the ruleset must be kept in sync whenever a job is renamed, added, or split out.

**Merge gates** (the ruleset's required contexts — must be green on your PR before merge):

- `ci.yml` — PR + push to `main`, `develop`, and the coord `merge-candidate/**` refs. Five jobs, six contexts (the `test` matrix reports one per platform):
  - `test (ubuntu-22.04)` / `test (windows-latest)` — the matrix leg: clones the sibling repos (`qontinui-schemas`, `qontinui-web`, `ui-bridge`), `pnpm install` + lint + typecheck + vitest + `pnpm run build`, then `cargo fmt -- --check`, `cargo test`, `pnpm run tauri build --debug --no-bundle`, plus the Windows-only SDK↔runner contract smoke. `cargo clippy` runs on the **ubuntu leg only** — the Windows clippy pass moved to its own job (next bullet). macOS is currently disabled in the matrix. Each platform leg must be either green **or** linked to a tracked open issue documenting an upstream-runner block (see "Platform escape valve" below). The escape valve is for hosted-runner pathologies you can't fix in the PR (e.g. rustc-LLVM crashing on the GitHub `windows-latest` image), not for "tests are flaky, ignore."
  - `Clippy (windows)` — the `clippy-windows` job, split out of the matrix so it runs in **parallel** with `test (windows-latest)` instead of sitting on its critical path (~12 min saved there). It is not redundant with the ubuntu clippy: this repo has substantial `#[cfg(windows)]` code that is only compiled — hence only linted — on a Windows host, `clippy-tiers.yml` is ubuntu-only and advisory, and plain `cargo test` does not evaluate `[lints.clippy]` levels. If this job is not required, Windows lint coverage exists but gates nothing.
  - `Frontend unit tests (vitest)` — the standalone `frontend-tests` job. The matrix leg runs `pnpm test` too, but wedged behind ~40-70 min of cargo; this job gives the same signal in ~1-2 min.
  - `seam-gate` — source-level assertion that the three `#[cfg(any(debug_assertions, feature = "test-fixtures"))]` anchors guarding the `/ui-bridge/test/*` injection seam are intact.
  - `security` — `cargo audit --file Cargo.lock` (with the documented `--ignore`s) + `pnpm audit --audit-level=moderate`. Runs in parallel with the matrix; don't ignore it just because it's a separate context.
- `forbid-runner-schema.yml` → `forbid-runner-schema`. Cheap, fast, no excuse for letting it go red.
- `secret-scan.yml` → `Gitleaks Secret Detection`.
- `schema-pg-sql-fresh.yml` → `schema-fresh`, and `clorinde-bindings-fresh.yml` → `clorinde-fresh`. Both are **always-run shim jobs** that reflect the verdict of a paths-filtered verify job, so the context reports on every PR (green as a no-op when your PR doesn't touch the watched paths) rather than going missing. `schema-fresh` confirms the checked-in `schema.pg.sql.generated` matches a fresh `alembic upgrade head + pg_dump` against the current `qontinui-web` alembic chain; if it goes red, regenerate locally via `bash src-tauri/scripts/regenerate_schema_pg_sql.sh` and commit the result.

> **Note on `spec-pairing.yml`**: a previous draft of this section claimed `spec-pairing.yml` is a path-triggered gate in this repo. It isn't — `spec-pairing.yml` lives in `qontinui-web`, not in `qontinui-runner`. Don't expect to see it in this repo's PR checks.

**Runs on PRs but is NOT a merge gate** (advisory signal — read it, don't be blocked by it):

- `ci-integrity.yml` → `Guard gating workflows from self-edits`. Goes **red by design** on any PR that edits one of the gating workflow files, so coord will not auto-land it and an operator reviews the diff. Do not "fix" this red and do not remove a file from its gating list.
- `clippy-tiers.yml` → `Clippy nightly (unscoped, all-targets)` + `Clippy diff-scoped (advisory)`. Ubuntu-only, advisory by design; not a substitute for either blocking clippy context above.
- `reproducibility-gate.yml`, `atlas-exclude-fresh.yml`, `page-spec-paths.yml`, `frontend-coverage-producer.yml`.
- `qontinui-types-drift.yml` — advisory **on your PR**, but read the next section before treating its `main` red as someone else's problem. Its `push` half is not advisory to anything.

### Advisory on a PR is not harmless on `main`

"Not a merge gate" above means *not a required context on your PR*. It does not mean the workflow cannot block merges, because coord — the actual merge authority — reads a different signal: the state of `main`. Two properties make that signal easy to misread, and the combination cost this repo ten days of partially-blocked merge train in August 2026 (PR #1107).

**Only a `push` run re-baselines coord.** coord scores `main` from the last run of each workflow, and `crates/coord/src/ci_baseline.rs` accepts the `push` event and nothing else:

```rust
assert!(establishes_main_baseline(Some("push")));
assert!(!establishes_main_baseline(Some("workflow_dispatch")));
assert!(!establishes_main_baseline(Some("schedule")));
```

So `gh workflow run <workflow> --ref main` — the obvious way to refresh a stale red — clears the red in the GitHub UI and changes nothing coord can see. For a paths-filtered workflow, the only thing that re-baselines it is a commit landing on `main` that touches one of its `paths:`. If it went red for a reason *outside* its own filter (the standing case: a sibling repo drifted), nothing in this repo re-runs it and the red freezes indefinitely.

**"PRs are landing" is not evidence the gate is clear.** `main-red` is consulted at **enqueue**, not at land. Already-queued PRs sail through a frozen red while every new one is refused, so the train looks alive from the outside the entire time.

The remedy, and the invariant that makes it available: every paths-filtered workflow lists **its own file** in its `paths:`, so editing the workflow is itself a valid thaw — landing that edit fires a fresh `push` run. That also means a PR editing a paths-filtered gate actually runs the gate, instead of landing unexercised. `src-tauri/tests/workflow_paths_self_inclusion.rs` enforces both halves; if you add a `paths:` filter, add the workflow's own path to it.

Use a dispatch to learn whether a drift is really fixed. Use a push to tell coord.

**Not PR-time at all** (validated at release time):

- `release.yml` — `push: tags: ['v*']` + `workflow_dispatch`. Won't run on a PR. Verify when cutting a tag.
- `build-python-executor.yml` — `workflow_dispatch` + `workflow_call` only. Called from `release.yml`. Verify when invoking manually or via release.

If `release.yml` is red on `windows-latest`, that's a release-time problem, not a merge-time problem — but file an issue so it isn't a surprise on the next tag.

### CI cache budget (10 GB, repo-wide)

GitHub caps Actions cache at **10 GB per repository** and evicts least-recently-used entries once you cross it. This repo lives close to that cap: a single `Swatinem/rust-cache` entry is ~2.3 GB and there are three of them from `main` alone (ubuntu `test`, windows `test`, `clippy-windows`). Two consequences worth knowing before you touch CI:

- **Only `main` saves.** Every rust-cache step sets `save-if: ${{ github.ref == 'refs/heads/main' }}`. Merge-candidate and PR refs are one-shot, so a per-ref save buys nothing and used to evict the `main` cache that everything else restores from (observed 2026-07-17: main went cold, 1h39m vs 1h17m warm). Restore is unrestricted — candidates and PRs still read main's cache. If you add a job with a cargo build, add the same `save-if`.
- **A reverted cache-producing feature leaves its entries behind.** Removing the wiring from `ci.yml` does not evict what it already wrote; those keys keep occupying the cap and evicting the caches that still matter. When you revert something that wrote to the Actions cache, delete its keys too:
  ```bash
  gh api repos/qontinui/qontinui-runner/actions/cache/usage
  gh api --paginate 'repos/qontinui/qontinui-runner/actions/caches?per_page=100' \
    --jq '.actions_caches[] | [.id, .size_in_bytes, .ref, .key] | @tsv'
  gh api -X DELETE repos/qontinui/qontinui-runner/actions/caches/<cache_id>
  ```

### Platform escape valve

`ci.yml`'s `test` matrix runs on hosted GitHub runners, and a platform leg can sometimes fail for reasons you can't fix inside your PR — either a genuine upstream issue (a runner-image regression, a third-party action breaking change) or an in-progress project-side fight that's already being worked on a different branch. Strict-on-every-platform-no-matter-what would block all merges during those windows, which punishes contributors for problems being tracked elsewhere.

Concrete current example: rustc-LLVM has been OOMing during codegen of the `qontinui_runner` test bin. On Windows the OOM surfaces as `STATUS_ILLEGAL_INSTRUCTION 0xc000001d` (rustc's allocator aborts; the OS reports the abort, not a real CPU instruction-set fault). On Linux the same root cause shows up as the runner agent receiving SIGTERM / exit 143 (the Linux OOM-killer takes the runner down before rustc can report). The mitigation lives in `Cargo.toml` profile overrides (`[profile.test] debug = 0`), `CARGO_BUILD_JOBS` caps, and pagefile / swap expansion — see `Cargo.toml:5-9` for the in-tree comment naming this exact symptom. Don't pin `target-cpu` or chase image-vintage theories; verify the OOM hypothesis first by grepping the log for `out of memory` and `Allocation failed`.

The rule:

- A platform leg may be temporarily exempted from the merge gate **if and only if** there's an open tracked issue or `_dev-notes-main/<slug>/SESSION_PROMPT.md` plan documenting the block, linked in the PR description. The block can be either an upstream-runner pathology *or* an in-progress project-side fix you can't land in your PR.
- Exemption applies to that platform leg only — the other two must still go green.
- Exemptions are not "permanent." Each one decays the moment the linked workstream closes; recheck before merging.

Don't add new exemptions casually. The escape valve exists so known-tracked blocks don't grind merges to zero — it isn't a free pass for "tests are flaky, ignore" or "I'll fix this later."

### Test locally first

For the platforms you can run locally, run the relevant test before pushing — the feedback loop is much faster than waiting on CI, and a local failure means CI failure too. The reverse isn't always true: local can pass while CI fails on something CI-environment-specific (smaller memory budget on the hosted runner, runner-image regression, action vendor break — see "Platform escape valve"). So local-first is a productivity practice, not a CI replacement.

```bash
# Frontend lint + build (CI uses pnpm — match the lockfile, don't mix npm/pnpm)
pnpm install --frozen-lockfile && pnpm run lint && pnpm run build

# Rust format + clippy + check (clippy is gated with `-D warnings` in CI)
cd src-tauri && cargo fmt -- --check && cargo clippy -- -D warnings && cargo check --bin qontinui-runner

# Rust tests (the slow one — only when relevant)
cd src-tauri && cargo test --bin qontinui-runner

# Untimed-subprocess gate (seconds; pure stdlib Python, same command CI runs)
python3 scripts/check_untimed_subprocess.py
```

If your local environment matches one of CI's platform legs (e.g. you're on Windows), green local runs are strong evidence the platform leg will go green in CI. They are not, however, a substitute for the CI run itself — push and verify.

### Hidden-red discipline

`main` has been red for months. That means a CI failure on your PR may be a layer of pre-existing breakage that was previously masked by an earlier-failing layer. Before you assume your PR caused a failure (or, worse, assume your PR is innocent because "CI is always red"), do this:

1. Pull up the latest run of the same workflow on `main`:

   ```bash
   gh run list --repo qontinui/qontinui-runner --branch main --workflow=<name> --limit 5
   gh run view <run-id> --log-failed
   ```

2. Compare your PR's failing job to `main`'s most recent failing job for the same workflow + platform.

   - **Symptom matches** → not your PR. Note this in the PR description, link the open issue or plan that owns the fix, and proceed.
   - **Symptom is new** → it's yours. Fix before merge.
   - **You can't tell** → check out a fresh `main`, push it to a throwaway branch, and see what CI does on a clean baseline. If the symptom appears there too, it's not yours.

Don't merge red without doing this comparison. "Same as main" is a real answer, but it has to be a verified answer.

### Active workstream awareness

CI is a shared surface. Before opening a PR that touches `.github/workflows/` or anything CI-adjacent, check what's already in flight:

```bash
gh pr list --repo qontinui/qontinui-runner --state open
gh api repos/qontinui/qontinui-runner/branches --jq '.[].name' | grep '^ci/'
```

There are usually several `ci/...` branches at any given time, some live and some stale. Don't accidentally re-do work that's already drafted on another branch. If you find a related open PR, coordinate (or rebase onto it) rather than opening a parallel attempt.

### Branch protection

The merge-gate set above is mechanically enforced by the `main-merge-gates` Repository Ruleset on `qontinui-runner` `main` (ruleset id `16044811`, [admin UI](https://github.com/qontinui/qontinui-runner/rules/16044811)). The rule blocks force-push, branch deletion, and any merge to `main` whose PR doesn't have these check contexts green:

- `test (ubuntu-22.04)`
- `test (windows-latest)`
- `Clippy (windows)`
- `Frontend unit tests (vitest)`
- `seam-gate`
- `security`
- `forbid-runner-schema`
- `clorinde-fresh`
- `schema-fresh`
- `Gitleaks Secret Detection`

Required-when-run is the rulesets default: checks that didn't trigger on a PR don't show as `pending` and don't block merge. `clorinde-fresh` and `schema-fresh` exploit that deliberately in the other direction — they are always-run shim jobs, so the context reports on every PR instead of going missing. The ruleset is **not strict** (`strict_required_status_checks_policy: false`), so a PR does not have to be up to date with `main` to merge; that is what lets coord's merge train validate a rebased candidate ref rather than forcing every PR to rebase. PRs also have to go through a pull request — direct push to `main` is blocked.

The escape-valve case ("merge with a red leg if a tracked plan documents the block") is intentionally **not** encoded in the ruleset. GitHub can't natively express "green OR linked open issue," so that part of the policy still lives in PR-review discipline, plus admin override (below) for the mechanical case.

#### Admin bypass

The ruleset has `OrganizationAdmin` as a `bypass_mode: always` actor. The org owner (currently jspinak) can override any rule — required checks, force-push block, deletion block — without going through the gate. This exists for two reasons:

1. **Solo-maintainer rescue.** With one admin, getting locked out by a misconfigured rule has no recovery path short of GitHub Support.
2. **Platform escape valve.** When a hosted-runner pathology blocks a leg and the project-side workstream tracking the fix is documented, an admin override is the mechanical answer for the gate that can't natively express "green OR linked open issue."

If you find yourself overriding routinely, the rule is wrong, not the override. Fix the rule.

#### How to override (admin runbook)

When you legitimately need to merge a red PR — documented hosted-runner block, in-flight project-side fix, etc. — and the escape-valve criteria in "Platform escape valve" above are satisfied:

1. Confirm the failure matches a tracked plan or open issue and link it in the PR description.
2. Click `Merge` on the PR. GitHub surfaces a "Bypass branch protections" prompt for org admins. Select "Bypass and merge."
3. Note in the merge commit message which rule was bypassed and why.

If a rule fires unexpectedly — e.g. a `.github/workflows/*.yml` job was renamed and the check context the ruleset pins no longer matches — update the ruleset, don't override repeatedly. Renaming a workflow job is a silent ruleset break: the ruleset references check contexts by name (`forbid-runner-schema`, `test (ubuntu-22.04)`, etc.), and those names follow the workflow file's `jobs.<id>` and matrix expansion. Sync the ruleset whenever those rename.

### Quick checklist before clicking merge

- [ ] Local build / test passed on whatever platform you're authoring on (`pnpm install && pnpm run lint && pnpm run build`, `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo check`, relevant `cargo test`)
- [ ] `ci.yml` `test` matrix green on each platform leg, OR red leg has a tracked exemption per "Platform escape valve" linked in the PR description
- [ ] `ci.yml` `security` job green (cargo-audit + pnpm audit on ubuntu-latest)
- [ ] `forbid-runner-schema.yml` green
- [ ] `schema-pg-sql-fresh.yml` green if it ran (or didn't run because no paths matched)
- [ ] Any new red compared against current `main` and either confirmed-not-yours-with-link or fixed
- [ ] No open `ci/...` branch is doing the same work

## Building for Release

```bash
# Build for current platform
npm run tauri build

# Output will be in src-tauri/target/release/bundle/
```

## Platform-Specific Notes

### Windows

- Requires MSVC toolchain
- May need to exclude .cargo directory from antivirus

### macOS

- Requires Xcode command line tools
- App needs to be signed for distribution

### Linux

- Requires additional system dependencies (see Tauri docs)
- Different package formats available (AppImage, deb, rpm)

## Areas for Contribution

### Good First Issues

- UI improvements
- Bug fixes
- Documentation
- Example automations

### Feature Development

- New UI components
- Configuration editor improvements
- Execution monitoring
- Error reporting improvements

### Platform Support

- Linux support
- macOS optimization
- Mobile platforms (future)

## Architecture

Qontinui Runner is a **Tauri application** with three layers:

1. **Frontend** (React/TypeScript): User interface
2. **Backend** (Rust/Tauri): Native OS integration
3. **Python Bridge**: Communication with qontinui library

The Python bridge is minimal - it delegates all automation logic to the qontinui library.

## Dependencies

- **[Qontinui](https://github.com/yourusername/qontinui)** - Core automation library
- **[MultiState](https://github.com/jspinak/multistate)** - State management
- **Tauri** - Desktop app framework
- **React** - UI framework

## Questions?

- Open an issue for questions
- Check Tauri documentation for framework questions
- Check qontinui docs for automation questions

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

Thank you for contributing! 🎉
