# Publishing the Runner

**Publishing is fully automated by CI.** You do **not** build or upload anything by hand.
Pushing a `v*` tag triggers the `Release` workflow (`.github/workflows/release.yml`),
which builds Windows / macOS / Linux on GitHub's runners, signs the updater artifacts,
uploads the installers, and un-drafts the release as "latest".

**Time:** ~2 minutes of your effort, then CI runs. | **Tool:** any machine with a clean checkout.

---

## Why you can (and should) publish from a clean machine

Because nothing is built locally, publishing only needs a clean, up-to-date checkout of
`main` — no local build, no local secrets. The signing keys
(`TAURI_SIGNING_PRIVATE_KEY` + password) live in **GitHub Actions secrets**, not on any
dev machine. This means you can cut a release from a machine that has no work-in-progress,
avoiding the "finish and push my WIP first" delay entirely.

---

## Prerequisites (one-time sanity check)

- The version in these three files must be **in sync** and equal to the release you're cutting:
  - `package.json` → `"version"`
  - `src-tauri/tauri.conf.json` → `"version"`
  - `src-tauri/Cargo.toml` → `[package] version`
- The sibling repo `../qontinui-schemas` must be up to date (it's a Rust path-dependency).
  A stale sibling silently downgrades `qontinui-types` in `Cargo.lock`.

---

## Step 1: Make sure your checkout is clean and current

```bash
cd /path/to/qontinui-runner
git checkout main
git pull --ff-only
(cd ../qontinui-schemas && git pull --ff-only)   # keep the path-dep sibling fresh
```

---

## Step 2: Bump the version (e.g. 1.0.5 → 1.0.6)

Edit the version in all three files, then refresh the lockfile:

- `package.json`
- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.toml`

```bash
(cd src-tauri && cargo update -p qontinui-runner)   # updates Cargo.lock to the new version
```

Confirm the lockfile diff is *only* the version bump (and `qontinui-types` stays at its
current release version — if it changed, your `qontinui-schemas` sibling is stale):

```bash
git diff Cargo.lock
```

---

## Step 3: Commit, push, tag, push the tag

```bash
git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml Cargo.lock
git commit -m "chore: release v1.0.6"
git push origin main

git tag v1.0.6
git push origin v1.0.6
```

The tag push is what triggers CI. (A tag only triggers the workflow if the commit it
points to already contains `.github/workflows/release.yml` — always tag from `main` after
pushing, never an older commit.)

---

## Step 4: Watch CI (optional)

```bash
gh run watch                      # follow the latest run
gh release view v1.0.6            # inspect the release once CI finishes
```

CI creates the release as a **draft** first, builds every platform, uploads the
installers + checksums + `latest.json` updater manifest, then flips it to
**published / latest** — but only if the Windows installer built (the sole hard gate),
so a release is never published empty.

---

## What CI produces

- **Windows:** `Qontinui.Runner_<ver>_x64-setup.exe` (NSIS) + `.sig` + `latest.json` (auto-updater)
- **macOS:** `.dmg` for x86_64 and aarch64 (non-blocking until fully verified)
- **Linux:** `.deb` + `.AppImage` (non-blocking until fully verified)
- SHA-256 checksums for each platform

---

## Manual trigger (rare)

You can also run it without a tag from the Actions tab → **Release** →
**Run workflow** (`workflow_dispatch`), which additionally offers the opt-in
bundled-Python executor. Tag pushes intentionally skip that to reduce build-failure surface.

---

## Troubleshooting

- **Nothing happened after pushing the tag** — the tagged commit predates
  `release.yml`, or Actions is disabled. Tag from current `main`.
- **`No .sig produced`** — `TAURI_SIGNING_PRIVATE_KEY` isn't set in repo secrets.
- **Release stuck as a draft** — the Windows leg failed (the hard gate). Check the run logs;
  macOS/Linux failing is non-blocking and won't hold back the release.
- **`Cargo.lock` shows a `qontinui-types` downgrade** — your `../qontinui-schemas` checkout
  is stale; `git pull` it and re-run `cargo update -p qontinui-runner`.
