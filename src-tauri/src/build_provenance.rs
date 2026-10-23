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

/// A stamped value (a hash, a profile dir, an opt-level), or `null` when the
/// build script could not measure it.
fn stamp_or_null(raw: &str) -> Value {
    if raw.is_empty() || raw == UNKNOWN {
        Value::Null
    } else {
        Value::String(raw.to_string())
    }
}

/// The `provenance.build` object: the build's MEASURED profile knobs (plan
/// `2026-10-19-primary-runner-is-a-debug-build-and-nothing-shows-it`, Phase 2).
///
/// - `profile` — the cargo profile DIRECTORY the binary was built into
///   (`debug` for cargo's `dev` profile, else the profile name), from
///   `OUT_DIR`; `null` when the build script could not read it.
/// - `optLevel` — the opt-level cargo passed rustc, from `OPT_LEVEL`.
/// - `debugAssertions` — `cfg!(debug_assertions)` of this crate.
/// - `optimized` — `optLevel` other than `0`; `null` when `optLevel` is
///   unknown. Never derived from the profile's NAME: a profile called
///   `release` with `opt-level = 0` is not optimized, and an unmeasured build
///   is UNKNOWN, never `debug`.
fn build_from(profile_dir: &str, opt_level: &str, debug_assertions: bool) -> Value {
    let opt = stamp_or_null(opt_level);
    let optimized = match opt.as_str() {
        None => Value::Null,
        Some("0") => Value::Bool(false),
        Some(_) => Value::Bool(true),
    };
    json!({
        "profile": stamp_or_null(profile_dir),
        "optLevel": opt,
        "debugAssertions": debug_assertions,
        "optimized": optimized,
    })
}

/// The one-line startup description of the build (`runner-lifecycle.log`):
/// which knobs this binary was compiled with, each named for what it is. It
/// replaces a `Debug build: <cfg!(debug_assertions)>` line, which would read
/// `true` on an optimized binary that keeps its debug assertions.
fn lifecycle_line_from(profile_dir: &str, opt_level: &str, debug_assertions: bool) -> String {
    format!(
        "debug-assertions: {debug_assertions}, profile: {}, opt-level: {}",
        if profile_dir.is_empty() {
            UNKNOWN
        } else {
            profile_dir
        },
        if opt_level.is_empty() {
            UNKNOWN
        } else {
            opt_level
        },
    )
}

fn provenance_from(
    git_sha: &str,
    git_dirty: &str,
    tree_hash: &str,
    rust_src_hash: &str,
    frontend_src_hash: &str,
    halves_agree: &str,
    build_id: &str,
    build: Value,
) -> Value {
    json!({
        "gitSha": git_sha,
        // gitDirty, treeHash and rustSrcHash are as of the LAST build-script
        // run. `pnpm run build:exe` always re-runs it; a bare `cargo build`
        // after a Rust edit does not (see build.rs, "What these stamps can and
        // cannot see", for what that can and cannot mislead).
        "gitDirty": tri(git_dirty),
        "treeHash": stamp_or_null(tree_hash),
        "rustSrcHash": stamp_or_null(rust_src_hash),
        "frontendSrcHash": stamp_or_null(frontend_src_hash),
        "buildId": build_id,
        // Did the embedded dist still match its recorded frontend sources when
        // cargo last ran the build script? `null` = no verdict (no
        // `dist/provenance.json`, or git unavailable) — never "agree".
        "halvesAgree": tri(halves_agree),
        "unstamped": build_id.starts_with("unstamped-"),
        // Which cargo profile, at which opt-level, this binary was built
        // with — see `build_from`.
        "build": build,
    })
}

/// The `provenance.build` object for THIS binary.
pub fn build_json() -> Value {
    build_from(
        env!("QONTINUI_BUILD_PROFILE_DIR"),
        env!("QONTINUI_BUILD_OPT_LEVEL"),
        cfg!(debug_assertions),
    )
}

/// The `/health` `buildProfile` string for THIS binary: the cargo profile
/// directory it was built into (`debug`, `release`, `release-checked`, …), or
/// `null` when the build script could not read it — never a defaulted
/// `"debug"`. Mirrors `provenance.build.profile`.
pub fn build_profile_json() -> Value {
    stamp_or_null(env!("QONTINUI_BUILD_PROFILE_DIR"))
}

/// The startup-log description of THIS binary's build knobs.
pub fn lifecycle_line() -> String {
    lifecycle_line_from(
        env!("QONTINUI_BUILD_PROFILE_DIR"),
        env!("QONTINUI_BUILD_OPT_LEVEL"),
        cfg!(debug_assertions),
    )
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
        build_json(),
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
            build_from("unknown", "unknown", false),
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
        let p = provenance_from(
            "abc",
            "true",
            "t1",
            "r1",
            "f1",
            "false",
            "abc-1",
            build_from("debug", "0", true),
        );
        assert_eq!(p["build"]["profile"], "debug");
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
            "build",
        ] {
            assert!(p.get(key).is_some(), "provenance.{key} missing: {p}");
        }
        for key in ["profile", "optLevel", "debugAssertions", "optimized"] {
            assert!(
                p["build"].get(key).is_some(),
                "provenance.build.{key} missing: {p}"
            );
        }
        // A real cargo build always sets OUT_DIR and OPT_LEVEL for the build
        // script, so this binary's stamps are measured, not UNKNOWN — and the
        // top-level `buildProfile` mirrors `provenance.build.profile`.
        assert!(p["build"]["profile"].is_string(), "{p}");
        assert!(p["build"]["optLevel"].is_string(), "{p}");
        assert_eq!(build_profile_json(), p["build"]["profile"]);
        assert_eq!(
            p["build"]["optimized"],
            Value::Bool(p["build"]["optLevel"] != "0"),
            "{p}"
        );
    }

    /// The primary's intended profile: optimized, assertions kept.
    #[test]
    fn release_checked_at_opt_level_2_is_optimized() {
        let b = build_from("release-checked", "2", true);
        assert_eq!(b["profile"], "release-checked");
        assert_eq!(b["optLevel"], "2");
        assert_eq!(b["debugAssertions"], Value::Bool(true));
        assert_eq!(b["optimized"], Value::Bool(true));
    }

    /// Today's primary: cargo's dev profile, opt-level 0.
    #[test]
    fn debug_at_opt_level_0_is_not_optimized() {
        let b = build_from("debug", "0", true);
        assert_eq!(b["profile"], "debug");
        assert_eq!(b["optLevel"], "0");
        assert_eq!(b["optimized"], Value::Bool(false));
    }

    /// `optimized` follows the MEASURED opt-level, not the profile's name.
    #[test]
    fn optimized_reads_the_opt_level_not_the_profile_name() {
        assert_eq!(
            build_from("release", "0", false)["optimized"],
            Value::Bool(false)
        );
        assert_eq!(
            build_from("debug", "1", true)["optimized"],
            Value::Bool(true)
        );
        assert_eq!(
            build_from("release", "s", false)["optimized"],
            Value::Bool(true)
        );
    }

    /// An unmeasured build is UNKNOWN on every axis the stamps feed — never
    /// rendered as `"debug"` and never as `optimized: false`.
    #[test]
    fn unknown_stamps_render_null_never_debug() {
        let b = build_from("unknown", "unknown", true);
        assert_eq!(b["profile"], Value::Null);
        assert_ne!(b["profile"], "debug");
        assert_eq!(b["optLevel"], Value::Null);
        assert_eq!(b["optimized"], Value::Null);
        // `debugAssertions` is a cfg of this crate, so it is always known.
        assert_eq!(b["debugAssertions"], Value::Bool(true));
    }

    #[test]
    fn the_lifecycle_line_names_each_knob() {
        assert_eq!(
            lifecycle_line_from("release-checked", "2", true),
            "debug-assertions: true, profile: release-checked, opt-level: 2"
        );
        assert_eq!(
            lifecycle_line_from("unknown", "", false),
            "debug-assertions: false, profile: unknown, opt-level: unknown"
        );
    }
}
