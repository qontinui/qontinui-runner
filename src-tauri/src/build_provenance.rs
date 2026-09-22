//! Build provenance for `/health` (plan `2026-08-23-build-provenance-assertion`,
//! Phase 4).
//!
//! The runner is built in two independent steps — Vite writes `dist/`, cargo
//! embeds it — and `gitSha` / `buildId` are two measurements of two separate
//! steps that nothing compared. `build.rs` now stamps what TREE STATE each half
//! came from as content hashes (see its "Build provenance" section), and this
//! module is the one place that reads those stamps back.
//!
//! Every field is an `env!()` read of a COMPILE-TIME constant. Nothing is
//! recomputed at runtime: a production install has no repo, and a dev worktree
//! has moved on since the build. These describe the build, not the present —
//! the same doctrine as `buildId` (plan
//! `2026-07-28-runner-build-id-banner-permanent-false-positive`). A caller
//! verifies by computing the same hashes in ITS worktree and comparing strings
//! (`scripts/frontend-provenance.mjs verify`; a qontinui-claude-config
//! temp-runner launcher is being added to shell it), which is exact for a dirty
//! tree where no SHA comparison can be.
//!
//! Three-valued throughout: the build script writes the literal `unknown` when
//! it could not measure (no git, no `dist/provenance.json`), and that surfaces
//! here as JSON `null` — "no verdict was computed" must never read as "agree"
//! or as "clean".

use serde_json::{json, Value};

const UNKNOWN: &str = "unknown";

/// `"true"` / `"false"` / anything else (the build script's `unknown`) →
/// `true` / `false` / `null`.
fn tri(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::Null,
    }
}

/// A stamped hash, or `null` when the build script could not compute it.
fn hash_or_null(raw: &str) -> Value {
    if raw.is_empty() || raw == UNKNOWN {
        Value::Null
    } else {
        Value::String(raw.to_string())
    }
}

fn provenance_from(
    git_sha: &str,
    git_dirty: &str,
    tree_hash: &str,
    rust_src_hash: &str,
    frontend_src_hash: &str,
    halves_agree: &str,
    build_id: &str,
) -> Value {
    json!({
        "gitSha": git_sha,
        // gitDirty, treeHash and rustSrcHash are as of the LAST build-script
        // run. `pnpm run build:exe` always re-runs it; a bare `cargo build`
        // after a Rust edit does not (see build.rs, "What these stamps can and
        // cannot see", for what that can and cannot mislead).
        "gitDirty": tri(git_dirty),
        "treeHash": hash_or_null(tree_hash),
        "rustSrcHash": hash_or_null(rust_src_hash),
        "frontendSrcHash": hash_or_null(frontend_src_hash),
        "buildId": build_id,
        // Did the embedded dist still match its recorded frontend sources when
        // cargo last ran the build script? `null` = no verdict (no
        // `dist/provenance.json`, or git unavailable) — never "agree".
        "halvesAgree": tri(halves_agree),
        "unstamped": build_id.starts_with("unstamped-"),
    })
}

/// The `/health` `provenance` block for THIS binary.
pub fn health_json() -> Value {
    provenance_from(
        env!("QONTINUI_GIT_SHA"),
        env!("QONTINUI_GIT_DIRTY"),
        env!("QONTINUI_TREE_HASH"),
        env!("QONTINUI_RUST_SRC_HASH"),
        env!("QONTINUI_FRONTEND_SRC_HASH"),
        env!("QONTINUI_HALVES_AGREE"),
        env!("RUNNER_BUILD_ID"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_stamps_surface_as_null_never_as_agree_or_clean() {
        let p = provenance_from(
            "abc",
            "unknown",
            "unknown",
            "unknown",
            "unknown",
            "unknown",
            "unstamped-abc",
        );
        assert_eq!(p["gitDirty"], Value::Null);
        assert_eq!(p["treeHash"], Value::Null);
        assert_eq!(p["rustSrcHash"], Value::Null);
        assert_eq!(p["frontendSrcHash"], Value::Null);
        assert_eq!(p["halvesAgree"], Value::Null);
        assert_eq!(p["unstamped"], Value::Bool(true));
    }

    #[test]
    fn measured_stamps_pass_through() {
        let p = provenance_from("abc", "true", "t1", "r1", "f1", "false", "abc-1");
        assert_eq!(p["gitDirty"], Value::Bool(true));
        assert_eq!(p["treeHash"], "t1");
        assert_eq!(p["rustSrcHash"], "r1");
        assert_eq!(p["frontendSrcHash"], "f1");
        assert_eq!(p["halvesAgree"], Value::Bool(false));
        assert_eq!(p["unstamped"], Value::Bool(false));
    }

    /// The real stamps compile in and every key is present on this binary.
    #[test]
    fn this_binary_carries_every_key() {
        let p = health_json();
        for key in [
            "gitSha",
            "gitDirty",
            "treeHash",
            "rustSrcHash",
            "frontendSrcHash",
            "buildId",
            "halvesAgree",
            "unstamped",
        ] {
            assert!(p.get(key).is_some(), "provenance.{key} missing: {p}");
        }
    }
}
