//! Pure dry-run parsers → [`DryRunData`].
//!
//! Each ecosystem has a different "what would this install do without doing it"
//! probe and a different output shape:
//!
//! | pm    | probe                                   | shape parsed here          |
//! |-------|-----------------------------------------|----------------------------|
//! | npm   | `npm install --dry-run --json`          | JSON (`added`/`changed`/`removed` or the v7 action list) |
//! | pnpm  | `pnpm add --lockfile-only` + lock diff  | before/after `pnpm-lock.yaml` (delta) |
//! | cargo | `cargo add --dry-run`                   | **stderr text** (`Adding x v1.2.3`)   |
//! | pip   | `pip install --dry-run --report -`      | JSON report (`install[].metadata`)    |
//!
//! Every parser is **total over garbage**: malformed / unexpected input ⇒
//! `None` (the caller keeps `dry_run` absent — honest unknown, never a
//! fabricated empty plan). A successfully-parsed-but-empty plan (`Some(default)`)
//! is reserved for the rare genuine "nothing to do" probe; in practice the
//! callers below return `None` on a structurally-unrecognized payload.
//!
//! `version_bumped[].delta` is NOT computed here — coord recomputes it from the
//! `from`/`to` strings we ship ([`VersionBump`]). The pnpm differ supplies the
//! `from`/`to`; the [`super::semver`] helper is available for the producer's own
//! pre-checks.

use std::collections::BTreeMap;

use crate::install_effects_producer::types::{DryRunData, PackageId, VersionBump};

use super::lockfile;

// ---------------------------------------------------------------------------
// npm — `npm install --dry-run --json`
// ---------------------------------------------------------------------------

/// Parse `npm install <pkg> --dry-run --json` stdout into a [`DryRunData`].
///
/// npm's `--json` dry-run reports the resolution it *would* apply. Across npm 7-10
/// the stable summary fields are `added` / `removed` / `changed` (counts in some
/// versions, arrays in `--json` with `{ name, version }` entries). We read the
/// array forms: `added[]` ⇒ `direct_added` + `predicted_pinned`,
/// `changed[]`/`updated[]` ⇒ `version_bumped` (when a `previousVersion`/`from`
/// is present) and `predicted_pinned`. `direct_names` distinguishes the
/// explicitly-requested adds from transitive ones.
///
/// Returns `None` when the payload is not valid JSON OR carries none of the
/// recognized arrays (honest unknown).
pub fn parse_npm_dry_run(stdout: &str, direct_names: &[String]) -> Option<DryRunData> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let mut data = DryRunData::default();
    let mut saw_known_array = false;

    // `added` (+ legacy `updated`) — new/installed packages.
    for key in ["added", "updated"] {
        if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
            saw_known_array = true;
            for item in arr {
                let (Some(name), Some(ver)) = (
                    item.get("name").and_then(|x| x.as_str()),
                    item.get("version").and_then(|x| x.as_str()),
                ) else {
                    continue;
                };
                data.predicted_pinned
                    .insert(name.to_string(), ver.to_string());
                let id = PackageId {
                    name: name.to_string(),
                    version: ver.to_string(),
                };
                if direct_names.iter().any(|d| d == name) {
                    data.direct_added.push(id);
                } else {
                    data.transitive_added.push(id);
                }
            }
        }
    }

    // `changed` — existing packages whose pinned version moved.
    if let Some(arr) = v.get("changed").and_then(|x| x.as_array()) {
        saw_known_array = true;
        for item in arr {
            let Some(name) = item.get("name").and_then(|x| x.as_str()) else {
                continue;
            };
            let to = item
                .get("version")
                .or_else(|| item.get("newVersion"))
                .and_then(|x| x.as_str());
            let from = item
                .get("previousVersion")
                .or_else(|| item.get("from"))
                .and_then(|x| x.as_str());
            if let Some(to) = to {
                data.predicted_pinned
                    .insert(name.to_string(), to.to_string());
                if let Some(from) = from {
                    data.version_bumped.push(VersionBump {
                        name: name.to_string(),
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
            }
        }
    }

    if saw_known_array {
        Some(data)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// pnpm — `pnpm add --lockfile-only` then diff the lockfile
// ---------------------------------------------------------------------------

/// Compute a [`DryRunData`] from the pnpm lockfile **before** and **after** a
/// `pnpm add --lockfile-only` run. This is the lockfile-diff path the plan
/// calls for: the dry-run mutates only the lockfile, and the delta is the
/// predicted effect. Pure over the two lockfile texts.
///
/// `direct_names` distinguishes the explicitly-requested adds (`direct_added`)
/// from transitively-pulled ones (`transitive_added`). A name present in both
/// before & after with a changed version ⇒ a [`VersionBump`]. The full
/// post-state is `predicted_pinned`.
///
/// An unparseable *after* lockfile ⇒ `None` (we can't predict). An unparseable
/// *before* (e.g. first install, no prior lockfile) is treated as an empty
/// pre-state — every post entry is then an add.
pub fn parse_pnpm_dry_run(
    before_lock: &str,
    after_lock: &str,
    direct_names: &[String],
) -> Option<DryRunData> {
    let after = lockfile::parse_pnpm_lock(after_lock);
    if after.is_empty() {
        // No post-state we can read ⇒ honest unknown.
        return None;
    }
    let before = lockfile::parse_pnpm_lock(before_lock); // empty on first install
    Some(diff_pinned_to_dryrun(&before, &after, direct_names))
}

/// Shared pinned-set differ → [`DryRunData`]. Mirrors coord's
/// `dry_run::diff_pinned`, but assembles the full [`DryRunData`] (including
/// `predicted_pinned`) so any lockfile-diff ecosystem (pnpm now, npm/cargo
/// post-install in Phase 3) can reuse it. Pure.
pub fn diff_pinned_to_dryrun(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    direct_names: &[String],
) -> DryRunData {
    let mut data = DryRunData {
        predicted_pinned: after.clone(),
        ..DryRunData::default()
    };
    for (name, ver) in after {
        match before.get(name) {
            None => {
                let id = PackageId {
                    name: name.clone(),
                    version: ver.clone(),
                };
                if direct_names.iter().any(|d| d == name) {
                    data.direct_added.push(id);
                } else {
                    data.transitive_added.push(id);
                }
            }
            Some(old) if old != ver => data.version_bumped.push(VersionBump {
                name: name.clone(),
                from: old.clone(),
                to: ver.clone(),
            }),
            _ => {}
        }
    }
    data
}

// ---------------------------------------------------------------------------
// cargo — `cargo add --dry-run` (stderr text)
// ---------------------------------------------------------------------------

/// Parse `cargo add --dry-run` **stderr** into a [`DryRunData`].
///
/// `cargo add` writes its plan to stderr as human text, e.g.:
/// ```text
///     Updating crates.io index
///       Adding serde v1.0.197 to dependencies
///              Features: ...
///       Adding regex v1.10.2 to dev-dependencies (dry run)
///        Locking 3 packages to latest compatible versions
///         Adding memchr v2.7.1 (latest: ...)
/// ```
/// The `  Adding <name> v<version>` lines are the resolution. A line ending in
/// `to dependencies` / `to dev-dependencies` / `to build-dependencies` is a
/// **direct** add (it touches `Cargo.toml`); a bare `Adding <name> v<ver>`
/// (under a `Locking` section) is a transitive lock add. Every parsed crate
/// lands in `predicted_pinned`.
///
/// Returns `None` when no `Adding` line is found (honest unknown — the probe
/// produced nothing we recognize).
pub fn parse_cargo_dry_run(stderr: &str) -> Option<DryRunData> {
    let mut data = DryRunData::default();
    let mut saw_adding = false;

    for raw in stderr.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("Adding ") else {
            continue;
        };
        // `rest` = `serde v1.0.197 to dependencies` | `memchr v2.7.1 (latest: ...)`
        let mut parts = rest.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(ver_tok) = parts.next() else {
            continue;
        };
        // version token is `v1.0.197`; strip the leading `v`.
        let Some(ver) = ver_tok.strip_prefix('v') else {
            continue;
        };
        if ver.is_empty() {
            continue;
        }
        saw_adding = true;
        data.predicted_pinned
            .insert(name.to_string(), ver.to_string());
        let id = PackageId {
            name: name.to_string(),
            version: ver.to_string(),
        };
        // "to (dev-/build-)dependencies" anywhere in the tail ⇒ a direct add.
        if rest.contains("to dependencies")
            || rest.contains("to dev-dependencies")
            || rest.contains("to build-dependencies")
        {
            data.direct_added.push(id);
        } else {
            data.transitive_added.push(id);
        }
    }

    if saw_adding {
        Some(data)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// pip — `pip install --dry-run --report -` (JSON report)
// ---------------------------------------------------------------------------

/// Parse `pip install --dry-run --report -` JSON (the PEP 668-era install
/// report) into a [`DryRunData`].
///
/// The report shape (report version 1) is
/// `{ "version": "1", "install": [ { "metadata": { "name", "version" },
/// "requested": true|false, "is_direct": true|false } ] }`. Each `install[]`
/// entry is a package pip *would* install. `requested: true` (or `direct_names`
/// membership) ⇒ a direct add; else transitive. pip resolves the full graph, so
/// every entry lands in `predicted_pinned`. pip has no "previous version" in the
/// report (it is a clean resolution), so `version_bumped` is left empty here —
/// bumps surface post-install in Phase 3 via a `pip list` diff.
///
/// Returns `None` when the payload is not valid JSON or carries no `install`
/// array (honest unknown).
pub fn parse_pip_dry_run(stdout: &str, direct_names: &[String]) -> Option<DryRunData> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let install = v.get("install").and_then(|x| x.as_array())?;
    let mut data = DryRunData::default();
    for entry in install {
        let meta = entry.get("metadata");
        let (Some(name), Some(ver)) = (
            meta.and_then(|m| m.get("name")).and_then(|x| x.as_str()),
            meta.and_then(|m| m.get("version")).and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        data.predicted_pinned
            .insert(name.to_string(), ver.to_string());
        let requested = entry
            .get("requested")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
            || direct_names.iter().any(|d| d.eq_ignore_ascii_case(name));
        let id = PackageId {
            name: name.to_string(),
            version: ver.to_string(),
        };
        if requested {
            data.direct_added.push(id);
        } else {
            data.transitive_added.push(id);
        }
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_dry_run_added_and_changed() {
        let stdout = r#"{
            "added": [
                { "name": "left-pad", "version": "1.3.0" },
                { "name": "transitive-dep", "version": "0.0.1" }
            ],
            "changed": [
                { "name": "lodash", "version": "4.17.21", "previousVersion": "4.17.20" }
            ]
        }"#;
        let data = parse_npm_dry_run(stdout, &["left-pad".to_string()]).unwrap();
        assert_eq!(data.direct_added.len(), 1);
        assert_eq!(data.direct_added[0].name, "left-pad");
        assert_eq!(data.transitive_added.len(), 1);
        assert_eq!(data.transitive_added[0].name, "transitive-dep");
        assert_eq!(data.version_bumped.len(), 1);
        assert_eq!(data.version_bumped[0].from, "4.17.20");
        assert_eq!(data.version_bumped[0].to, "4.17.21");
        assert_eq!(
            data.predicted_pinned.get("left-pad"),
            Some(&"1.3.0".to_string())
        );
        assert_eq!(
            data.predicted_pinned.get("lodash"),
            Some(&"4.17.21".to_string())
        );
    }

    #[test]
    fn npm_dry_run_unrecognized_shape_is_none() {
        // valid JSON but no added/changed/updated arrays ⇒ honest unknown
        assert!(parse_npm_dry_run(r#"{"removed": 2}"#, &[]).is_none());
        assert!(parse_npm_dry_run("not json", &[]).is_none());
    }

    #[test]
    fn pnpm_lockfile_diff_add_and_bump() {
        let before = r#"
lockfileVersion: '6.0'
packages:
  /lodash/4.17.20:
    resolution: {integrity: sha512-x}
"#;
        let after = r#"
lockfileVersion: '6.0'
packages:
  /lodash/4.17.21:
    resolution: {integrity: sha512-y}
  /left-pad/1.3.0:
    resolution: {integrity: sha512-z}
  /transitive/0.0.1:
    resolution: {integrity: sha512-w}
"#;
        let data = parse_pnpm_dry_run(before, after, &["left-pad".to_string()]).unwrap();
        // lodash 4.17.20 -> 4.17.21 is a bump
        assert_eq!(data.version_bumped.len(), 1);
        assert_eq!(data.version_bumped[0].name, "lodash");
        assert_eq!(data.version_bumped[0].from, "4.17.20");
        assert_eq!(data.version_bumped[0].to, "4.17.21");
        // left-pad is a direct add, transitive is transitive
        assert_eq!(data.direct_added.len(), 1);
        assert_eq!(data.direct_added[0].name, "left-pad");
        assert_eq!(data.transitive_added.len(), 1);
        assert_eq!(data.transitive_added[0].name, "transitive");
        assert_eq!(data.predicted_pinned.len(), 3);
    }

    #[test]
    fn pnpm_first_install_empty_before() {
        let after = r#"
lockfileVersion: '9.0'
packages:
  requests@2.31.0:
    resolution: {integrity: sha512-x}
"#;
        let data = parse_pnpm_dry_run("", after, &["requests".to_string()]).unwrap();
        assert_eq!(data.direct_added.len(), 1);
        assert_eq!(data.version_bumped.len(), 0);
    }

    #[test]
    fn pnpm_unreadable_after_is_none() {
        assert!(parse_pnpm_dry_run("packages: {}", "garbage : : :", &[]).is_none());
    }

    #[test]
    fn cargo_dry_run_stderr() {
        let stderr = "\
    Updating crates.io index
      Adding serde v1.0.197 to dependencies
      Adding regex v1.10.2 to dev-dependencies (dry run)
       Locking 2 packages to latest compatible versions
        Adding memchr v2.7.1 (latest: v2.7.4)
";
        let data = parse_cargo_dry_run(stderr).unwrap();
        assert_eq!(
            data.predicted_pinned.get("serde"),
            Some(&"1.0.197".to_string())
        );
        assert_eq!(
            data.predicted_pinned.get("memchr"),
            Some(&"2.7.1".to_string())
        );
        // serde + regex are direct (to (dev-)dependencies); memchr is transitive
        let direct: Vec<&str> = data.direct_added.iter().map(|p| p.name.as_str()).collect();
        assert!(direct.contains(&"serde"));
        assert!(direct.contains(&"regex"));
        assert_eq!(data.transitive_added.len(), 1);
        assert_eq!(data.transitive_added[0].name, "memchr");
    }

    #[test]
    fn cargo_dry_run_no_adding_lines_is_none() {
        assert!(parse_cargo_dry_run("    Updating crates.io index\n").is_none());
        assert!(parse_cargo_dry_run("").is_none());
    }

    #[test]
    fn pip_dry_run_report() {
        let stdout = r#"{
            "version": "1",
            "install": [
                { "metadata": { "name": "requests", "version": "2.31.0" }, "requested": true },
                { "metadata": { "name": "urllib3", "version": "2.2.1" }, "requested": false },
                { "metadata": { "name": "certifi", "version": "2024.2.2" } }
            ]
        }"#;
        let data = parse_pip_dry_run(stdout, &[]).unwrap();
        assert_eq!(data.predicted_pinned.len(), 3);
        assert_eq!(data.direct_added.len(), 1);
        assert_eq!(data.direct_added[0].name, "requests");
        assert_eq!(data.transitive_added.len(), 2);
    }

    #[test]
    fn pip_dry_run_direct_names_fallback() {
        // when `requested` is absent, direct_names membership marks a direct add
        let stdout = r#"{
            "install": [
                { "metadata": { "name": "Flask", "version": "3.0.0" } }
            ]
        }"#;
        let data = parse_pip_dry_run(stdout, &["flask".to_string()]).unwrap();
        // case-insensitive match (pip normalizes case)
        assert_eq!(data.direct_added.len(), 1);
    }

    #[test]
    fn pip_dry_run_unrecognized_is_none() {
        assert!(parse_pip_dry_run(r#"{"version":"1"}"#, &[]).is_none());
        assert!(parse_pip_dry_run("not json", &[]).is_none());
    }
}
