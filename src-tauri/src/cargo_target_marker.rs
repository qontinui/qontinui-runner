//! What marks a directory as a cargo target directory.
//!
//! One predicate, shared by the orphan-target reaper (bin crate) and the
//! embedded-PG test harness (lib crate). Plan
//! `2026-11-20-runner-test-harness-target-discovery-under-cargo-guard`.

use std::path::Path;

/// True iff `dir` carries a co-authoritative cargo target marker: a
/// `CACHEDIR.TAG` (newer cargo) OR a `.rustc_info.json` (present in every cargo
/// target root, including the many that predate or lack the tag —
/// e.g. `_cargo-targets/<slug>`, `_targets/<slug>`, `.wt-targets/<slug>`,
/// `cargo-target-coord-wt-<slug>` all have `.rustc_info.json + debug/` but NO
/// tag). Either marker positively identifies a cargo build dir; a dir with
/// neither (a real source/data dir) is never a target root.
///
/// The tag alone is not enough because cargo writes `CACHEDIR.TAG` only into a
/// target directory cargo itself creates: a directory that already existed
/// when cargo first ran in it (a pre-made `CARGO_TARGET_DIR`, or one a wrapper
/// re-created under `.cargo-verify/` after a sweep removed it) never gets one.
/// `qontinui-claude-config/scripts/cargo-sweep-all.ps1` discovers targets by
/// `.rustc_info.json` for the same reason.
///
/// Presence is read with `symlink_metadata`, so any entry of that name counts,
/// a directory or a dangling symlink included, and a symlink is never
/// followed. Before this predicate was shared, the test harness required
/// `CACHEDIR.TAG` to be a regular file; the reaper's looser reading is the one
/// kept. Only cargo writes `.rustc_info.json`. A stray `CACHEDIR.TAG` (other
/// caches write one too: `.ruff_cache`, `.mypy_cache`, `.venv`) can at worst
/// place the harness's `qontinui-test-pg` cache in that ancestor, which
/// `QONTINUI_TEST_PG_DIR` overrides, and the reaper additionally requires a
/// profile directory beside the marker.
pub fn has_cargo_marker(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join("CACHEDIR.TAG")).is_ok()
        || std::fs::symlink_metadata(dir.join(".rustc_info.json")).is_ok()
}
