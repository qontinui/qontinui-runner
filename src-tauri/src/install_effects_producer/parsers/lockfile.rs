//! Pure lockfile parsers → an `observed_pinned` map (`name → version`).
//!
//! Phase 3 calls these post-install to read the **actual** pinned set from the
//! mutated lockfile and diff it against the dry-run prediction. They are also
//! reused by [`super::dry_run`]: a `--lockfile-only` dry-run mutates a lockfile
//! in place (or a copy), so the same parser reads both the pre- and post-state
//! to compute the delta.
//!
//! Supported formats:
//! - `package-lock.json` v2/v3 (the `packages` map; npm-shrinkwrap is identical).
//! - `pnpm-lock.yaml` (the `packages` map; v6/v9 key shapes).
//! - `Cargo.lock` (TOML `[[package]]` array).
//! - pip: NO universal lockfile — [`parse_pip_list`] reads `pip list
//!   --format=json` output instead (honest: this is post-install inventory, not
//!   a lockfile).
//!
//! Every parser is **total over garbage**: malformed / unexpected input ⇒ an
//! empty map (never a panic, never a partial-but-wrong fabrication). An empty
//! map is the honest "could not read" signal; the caller keeps the field absent
//! rather than sending a fabricated empty pinned set.

use std::collections::BTreeMap;

/// Parsed pinned set: dependency name → resolved (pinned) version.
pub type PinnedSet = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// npm — package-lock.json v2/v3
// ---------------------------------------------------------------------------

/// Parse `package-lock.json` (lockfileVersion 2 or 3) into the pinned set.
///
/// v2/v3 carry a top-level `packages` object keyed by install path
/// (`""` = root project, `"node_modules/<name>"`, nested
/// `"node_modules/a/node_modules/b"`). Each entry has a `version`. We key by the
/// **package name** (the trailing path segment after the last `node_modules/`),
/// skipping the root (`""`) and any entry without a `version` (e.g. a `link:`
/// workspace ref). On a duplicated name (diamond) the last-wins; this is an
/// inventory probe, not a graph, so that is acceptable.
///
/// Falls back to the legacy v1 `dependencies` tree when `packages` is absent.
/// Garbage / non-object ⇒ empty map.
pub fn parse_package_lock(text: &str) -> PinnedSet {
    let mut out = PinnedSet::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return out;
    };

    // v2/v3 `packages` map (preferred — flat + authoritative).
    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
        for (path, entry) in pkgs {
            if path.is_empty() {
                continue; // root project, not a dependency
            }
            let Some(name) = name_from_lock_path(path) else {
                continue;
            };
            if let Some(ver) = entry.get("version").and_then(|x| x.as_str()) {
                out.insert(name, ver.to_string());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Legacy v1 `dependencies` recursive tree.
    if let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) {
        collect_v1_dependencies(deps, &mut out);
    }
    out
}

/// The package name for a v2/v3 `packages` key: the segment after the final
/// `node_modules/`. `"node_modules/@scope/pkg"` ⇒ `"@scope/pkg"`.
fn name_from_lock_path(path: &str) -> Option<String> {
    let tail = path.rsplit("node_modules/").next().unwrap_or(path);
    let tail = tail.trim_matches('/');
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    }
}

/// Recursively collect `{ name: { version, dependencies? } }` (v1 lockfiles).
fn collect_v1_dependencies(deps: &serde_json::Map<String, serde_json::Value>, out: &mut PinnedSet) {
    for (name, entry) in deps {
        if let Some(ver) = entry.get("version").and_then(|x| x.as_str()) {
            out.insert(name.clone(), ver.to_string());
        }
        if let Some(nested) = entry.get("dependencies").and_then(|d| d.as_object()) {
            collect_v1_dependencies(nested, out);
        }
    }
}

// ---------------------------------------------------------------------------
// pnpm — pnpm-lock.yaml
// ---------------------------------------------------------------------------

/// Parse `pnpm-lock.yaml` into the pinned set. pnpm keys its `packages` map by a
/// path-like id whose shape changed across lockfile versions:
/// - v5/v6: `/<name>/<version>` or `/@scope/name/version(_peerhash)`
/// - v9:    `<name>@<version>` or `@scope/name@<version>(peerhash)`
///
/// We prefer the entry's own `version:` field when present (v9 emits it), else
/// parse the version out of the key. Garbage / non-mapping ⇒ empty map.
pub fn parse_pnpm_lock(text: &str) -> PinnedSet {
    let mut out = PinnedSet::new();
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return out;
    };
    let Some(pkgs) = doc.get("packages").and_then(|p| p.as_mapping()) else {
        return out;
    };
    for (key, entry) in pkgs {
        let Some(key) = key.as_str() else { continue };
        // Prefer an explicit `version:` field (pnpm v9), else parse the key.
        let explicit = entry
            .get("version")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        if let Some((name, ver)) = split_pnpm_key(key, explicit) {
            out.insert(name, ver);
        }
    }
    out
}

/// Split a pnpm `packages` key into `(name, version)`. Handles both the
/// leading-slash v5/v6 form (`/lodash/4.17.21`, `/@babel/core/7.0.0_react@18`)
/// and the v9 `name@version` form (`lodash@4.17.21`, `@babel/core@7.0.0(x)`).
/// `explicit_version` (the entry's `version:` field) overrides the key-derived
/// version when present.
fn split_pnpm_key(key: &str, explicit_version: Option<String>) -> Option<(String, String)> {
    if let Some(rest) = key.strip_prefix('/') {
        // v5/v6: `/name/version` or `/@scope/name/version`. The version is the
        // last `/`-segment; strip a `_peerhash` / `(peerhash)` suffix.
        let (name_part, ver_part) = rest.rsplit_once('/')?;
        let name = name_part.trim_matches('/').to_string();
        let ver = explicit_version.unwrap_or_else(|| strip_peer_suffix(ver_part).to_string());
        if name.is_empty() || ver.is_empty() {
            return None;
        }
        Some((name, ver))
    } else {
        // v9: `name@version` — but a scoped name itself starts with `@`, so
        // find the LAST `@` that isn't the leading scope `@`.
        let at = if let Some(stripped) = key.strip_prefix('@') {
            // scoped: leading @ is the scope; the version separator is the next @
            stripped.find('@').map(|i| i + 1)
        } else {
            key.find('@')
        }?;
        let (name, ver_with_at) = key.split_at(at);
        let ver_raw = ver_with_at.strip_prefix('@').unwrap_or(ver_with_at);
        let ver = explicit_version.unwrap_or_else(|| strip_peer_suffix(ver_raw).to_string());
        if name.is_empty() || ver.is_empty() {
            return None;
        }
        Some((name.to_string(), ver))
    }
}

/// Strip a pnpm peer-dependency hash suffix from a version: `7.0.0_react@18.0.0`
/// or `7.0.0(react@18.0.0)` ⇒ `7.0.0`.
fn strip_peer_suffix(ver: &str) -> &str {
    let ver = ver.split('(').next().unwrap_or(ver);
    ver.split('_').next().unwrap_or(ver).trim()
}

// ---------------------------------------------------------------------------
// cargo — Cargo.lock
// ---------------------------------------------------------------------------

/// Parse `Cargo.lock` (TOML) into the pinned set. The file is an array of
/// `[[package]]` tables each with `name` + `version`. A duplicated crate name
/// (two major versions in the graph) keeps the last seen — acceptable for an
/// inventory probe. Garbage / non-TOML ⇒ empty map.
pub fn parse_cargo_lock(text: &str) -> PinnedSet {
    let mut out = PinnedSet::new();
    let Ok(doc) = toml::from_str::<toml::Value>(text) else {
        return out;
    };
    let Some(pkgs) = doc.get("package").and_then(|p| p.as_array()) else {
        return out;
    };
    for pkg in pkgs {
        if let (Some(name), Some(ver)) = (
            pkg.get("name").and_then(|x| x.as_str()),
            pkg.get("version").and_then(|x| x.as_str()),
        ) {
            out.insert(name.to_string(), ver.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// pip — `pip list --format=json` (no universal lockfile)
// ---------------------------------------------------------------------------

/// Parse `pip list --format=json` output into the pinned set. pip has no
/// universal lockfile; the honest post-install inventory is the installed-set
/// listing: `[{"name": "...", "version": "..."}, …]`. Garbage / non-array ⇒
/// empty map.
pub fn parse_pip_list(text: &str) -> PinnedSet {
    let mut out = PinnedSet::new();
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return out;
    };
    for entry in arr {
        if let (Some(name), Some(ver)) = (
            entry.get("name").and_then(|x| x.as_str()),
            entry.get("version").and_then(|x| x.as_str()),
        ) {
            // pip normalizes names case-insensitively & with `-`/`_` collapsed;
            // keep the reported name verbatim (the audit/dry-run sides report
            // the same canonical form pip emits).
            out.insert(name.to_string(), ver.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_lock_v3_packages_map() {
        let text = r#"{
            "name": "widget",
            "lockfileVersion": 3,
            "packages": {
                "": { "name": "widget", "version": "1.0.0" },
                "node_modules/left-pad": { "version": "1.3.0" },
                "node_modules/@babel/core": { "version": "7.23.0" },
                "node_modules/a/node_modules/b": { "version": "2.1.0" }
            }
        }"#;
        let pins = parse_package_lock(text);
        assert_eq!(pins.get("left-pad"), Some(&"1.3.0".to_string()));
        assert_eq!(pins.get("@babel/core"), Some(&"7.23.0".to_string()));
        assert_eq!(pins.get("b"), Some(&"2.1.0".to_string()));
        // root project ("") is excluded
        assert_eq!(pins.get("widget"), None);
    }

    #[test]
    fn package_lock_v1_dependencies_tree() {
        let text = r#"{
            "lockfileVersion": 1,
            "dependencies": {
                "left-pad": {
                    "version": "1.3.0",
                    "dependencies": {
                        "nested-dep": { "version": "0.0.1" }
                    }
                }
            }
        }"#;
        let pins = parse_package_lock(text);
        assert_eq!(pins.get("left-pad"), Some(&"1.3.0".to_string()));
        assert_eq!(pins.get("nested-dep"), Some(&"0.0.1".to_string()));
    }

    #[test]
    fn pnpm_lock_v6_slash_keys() {
        let text = r#"
lockfileVersion: '6.0'
packages:
  /lodash/4.17.21:
    resolution: {integrity: sha512-x}
  /@babel/core/7.23.0_react@18.2.0:
    resolution: {integrity: sha512-y}
"#;
        let pins = parse_pnpm_lock(text);
        assert_eq!(pins.get("lodash"), Some(&"4.17.21".to_string()));
        // peer-hash suffix stripped, scope preserved
        assert_eq!(pins.get("@babel/core"), Some(&"7.23.0".to_string()));
    }

    #[test]
    fn pnpm_lock_v9_at_keys_with_version_field() {
        let text = r#"
lockfileVersion: '9.0'
packages:
  lodash@4.17.21:
    resolution: {integrity: sha512-x}
  '@babel/core@7.23.0(react@18.2.0)':
    resolution: {integrity: sha512-y}
    version: 7.23.0
"#;
        let pins = parse_pnpm_lock(text);
        assert_eq!(pins.get("lodash"), Some(&"4.17.21".to_string()));
        assert_eq!(pins.get("@babel/core"), Some(&"7.23.0".to_string()));
    }

    #[test]
    fn cargo_lock_packages() {
        let text = r#"
version = 3

[[package]]
name = "serde"
version = "1.0.197"

[[package]]
name = "left_pad"
version = "0.1.0"
"#;
        let pins = parse_cargo_lock(text);
        assert_eq!(pins.get("serde"), Some(&"1.0.197".to_string()));
        assert_eq!(pins.get("left_pad"), Some(&"0.1.0".to_string()));
    }

    #[test]
    fn pip_list_json() {
        let text = r#"[
            {"name": "requests", "version": "2.31.0"},
            {"name": "urllib3", "version": "2.2.1"}
        ]"#;
        let pins = parse_pip_list(text);
        assert_eq!(pins.get("requests"), Some(&"2.31.0".to_string()));
        assert_eq!(pins.get("urllib3"), Some(&"2.2.1".to_string()));
    }

    // ---- malformed input ⇒ empty map, never panic --------------------------

    #[test]
    fn malformed_inputs_are_empty_not_panic() {
        assert!(parse_package_lock("not json {{{").is_empty());
        assert!(parse_package_lock("[]").is_empty()); // valid json, wrong shape
        assert!(parse_pnpm_lock(": : not yaml : :").is_empty());
        assert!(parse_pnpm_lock("packages: 42").is_empty()); // packages not a map
        assert!(parse_cargo_lock("not = = toml").is_empty());
        assert!(parse_cargo_lock("[other]\nkey = 1").is_empty()); // no [[package]]
        assert!(parse_pip_list("not json").is_empty());
        assert!(parse_pip_list("{}").is_empty()); // object, not array
        assert!(parse_pip_list("").is_empty());
    }

    #[test]
    fn entries_missing_version_are_skipped() {
        // a package-lock entry without a version (e.g. a workspace link)
        let text = r#"{
            "packages": {
                "node_modules/linked": { "link": true },
                "node_modules/real": { "version": "1.0.0" }
            }
        }"#;
        let pins = parse_package_lock(text);
        assert_eq!(pins.len(), 1);
        assert_eq!(pins.get("real"), Some(&"1.0.0".to_string()));
    }
}
