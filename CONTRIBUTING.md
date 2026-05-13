<!-- BEGIN License & CLA preamble — added 2026-04-29 during AGPL rollout -->
## License & CLA

This project is licensed under the **GNU Affero General Public License v3.0 or later** (`AGPL-3.0-or-later`). See [`LICENSE`](LICENSE) for the full text. Contributors should be aware:

- AGPL is a strong copyleft license. Anyone who runs a modified version of this project as a network service must publish their modifications under AGPL too.
- For typical self-hosting, internal use, forking, or contributing back, AGPL behaves like GPL.

All non-trivial contributions require signing the qontinui Contributor License Agreement (CLA). The CLA is administered via [cla-assistant.io](https://cla-assistant.io/) — when you open a pull request, the CLA bot will comment with a one-click sign link, and signing applies across all qontinui repositories. The CLA text lives in [`CLA.md`](CLA.md). It grants Joshua Spinak the right to relicense your contribution under any future license; you retain copyright in your contributions.

The remainder of this document covers contribution mechanics specific to this repository.

<!-- END License & CLA preamble -->

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

### Python Bridge

- Follow PEP 8
- Use type hints
- Format with `ruff format`
- Minimal code - delegate to qontinui library

## Testing

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

### What "main is green" means here

This repo has five workflows in `.github/workflows/`. They split into two tiers by trigger type:

**Merge gates** (must be green on your PR before merge):

- `ci.yml` — runs on PR + push to `main` and `develop` (`.github/workflows/ci.yml:3-7`). Two jobs:
  - `test` matrix (`ci.yml:13-189`) on `ubuntu-22.04`, `macos-latest`, `windows-latest`: clones sibling repos (`qontinui-schemas`, `jspinak/qontinui-web`), `pnpm install` + `pnpm run lint` + `pnpm run build`, then `cargo fmt -- --check`, `cargo clippy -- -D warnings`, `cargo test`, and finally `pnpm run tauri build --debug --no-bundle`. Each platform leg must be either green **or** linked to a tracked open issue documenting an upstream-runner block (see "Platform escape valve" below). The escape valve is for hosted-runner pathologies you can't fix in the PR (e.g. rustc-LLVM crashing on the GitHub `windows-latest` image), not for "tests are flaky, ignore."
  - `security` job (`ci.yml:191-228`) on `ubuntu-latest` only: `cargo audit --file Cargo.lock --ignore RUSTSEC-2023-0071` + `pnpm audit --audit-level=moderate`. Runs in parallel with the matrix; treated as part of the same `ci.yml` gate. Don't ignore it just because it's separate from the platform legs.
- `forbid-runner-schema.yml` — runs on PR + push to `main` and `develop` (`.github/workflows/forbid-runner-schema.yml:21-25`). Cheap, fast, no excuse for letting it go red.
- `schema-pg-sql-fresh.yml` — `pull_request: [main]` + `workflow_dispatch`, paths-filtered to `src-tauri/queries/**` and `src-tauri/schema.pg.sql.generated` (`.github/workflows/schema-pg-sql-fresh.yml:18-25`). Required when your PR touches those paths; otherwise it doesn't run and isn't a gate for that PR. Confirms the checked-in `schema.pg.sql.generated` matches a fresh `alembic upgrade head + pg_dump` against the current `qontinui-web` alembic chain. If it goes red, regenerate locally via `bash src-tauri/scripts/regenerate_schema_pg_sql.sh` and commit the result.

> **Note on `spec-pairing.yml`**: a previous draft of this section claimed `spec-pairing.yml` is a path-triggered gate in this repo. It isn't — `spec-pairing.yml` lives in `jspinak/qontinui-web`, not in `qontinui-runner`. Don't expect to see it in this repo's PR checks.

**Not merge gates** (validated at release time, not PR time):

- `release.yml` — `push: tags: ['v*']` + `workflow_dispatch`. Won't run on a PR. Verify when cutting a tag.
- `build-python-executor.yml` — `workflow_dispatch` + `workflow_call` only. Called from `release.yml`. Verify when invoking manually or via release.

If `release.yml` is red on `windows-latest`, that's a release-time problem, not a merge-time problem — but file an issue so it isn't a surprise on the next tag.

### Platform escape valve

`ci.yml` runs on three hosted GitHub runners, and a platform leg can sometimes fail for reasons you can't fix inside your PR — either a genuine upstream issue (a runner-image regression, a third-party action breaking change) or an in-progress project-side fight that's already being worked on a different branch. Strict-on-every-platform-no-matter-what would block all merges during those windows, which punishes contributors for problems being tracked elsewhere.

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

- `forbid-runner-schema`
- `security`
- `test (ubuntu-22.04)`
- `test (macos-latest)`
- `test (windows-latest)`
- `schema-fresh` — required *when run*, i.e. only on PRs touching `src-tauri/queries/**` or `src-tauri/schema.pg.sql.generated`

Required-when-run is the rulesets default: checks that didn't trigger on a PR don't show as `pending` and don't block merge. PRs also have to go through a pull request — direct push to `main` is blocked.

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
