//! Pure parsers over captured tool output for the install-effects producer
//! (Phase 2). Every function in this module is **pure** — no subprocess, no FS
//! mutation, no network — and **total over garbage** (malformed input ⇒
//! `None`/empty, never a panic, never a fabricated-but-wrong result). The
//! orchestration ([`super`]) owns the subprocess invocation and feeds the
//! captured text/JSON here.
//!
//! Layout (mirrors coord's pure-parser posture in
//! `qontinui-coord::install_effects` + the runner's tested-collector posture in
//! `agent_worktree::fs_observer`):
//!
//! - [`dry_run`] — `<pm> --dry-run` output → [`super::types::DryRunData`]
//!   (npm/pnpm/cargo/pip). The predicted-plan side.
//! - [`lockfile`] — `package-lock.json` / `pnpm-lock.yaml` / `Cargo.lock` /
//!   `pip list` → an `observed_pinned` map. Phase 3 calls these post-install.
//! - [`audit`] — `npm audit` / `cargo audit` / `pip-audit` JSON → `Vec<Cve>` +
//!   a `severity_max` derivation. The security side.
//! - [`semver`] — a tiny pure semver core for the `from → to` delta
//!   classification (runner has no `semver` crate dep).

pub mod audit;
pub mod dry_run;
pub mod lockfile;
pub mod semver;
