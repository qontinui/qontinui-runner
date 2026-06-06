//! Pure native-audit parsers → `Vec<Cve>` (+ a `severity_max` derivation).
//!
//! These read the JSON emitted by each ecosystem's native auditor. The audit
//! tool may be absent on a given host — that is the **caller's** concern (it
//! sets `security_audited: false` and supplies no CVEs); the PARSER only ever
//! sees real audit JSON and turns it into coord's [`Cve`] wire shape.
//!
//! Supported formats:
//! - `npm audit --json` (the v7+ `vulnerabilities` map; also the legacy
//!   `advisories` map as a fallback).
//! - `cargo audit --json` (RustSec's `{ vulnerabilities: { list: [...] } }`).
//! - `pip-audit -f json` (the `dependencies[].vulns[]` array).
//!
//! Severity is normalized to coord's snake_case token set
//! (`none | low | moderate | high | critical`) so it round-trips into coord's
//! `Severity` enum and feeds the `severity_max` autonomy trigger. An
//! unrecognized severity ⇒ `None` on the [`Cve`] (coord's `severity_none`
//! default reads it as `none`), never a fabricated severity.
//!
//! Every parser is **total over garbage**: malformed / unexpected input ⇒ an
//! empty vec (never a panic, never a partial-but-wrong fabrication).

use crate::install_effects_producer::types::Cve;

/// Coord's ordered severity floor, lowest → highest. Used only for the
/// `severity_max` derivation (the rank). Mirrors coord's
/// `install_effects::Severity` `Ord`.
const SEVERITY_RANK: &[&str] = &["none", "low", "moderate", "high", "critical"];

/// Normalize an ecosystem-specific severity token to coord's set. npm uses
/// `info|low|moderate|high|critical`; cargo/RustSec uses CVSS-derived
/// `low|medium|high|critical` (sometimes a numeric score → handled by the
/// caller); pip-audit often has no severity. Unknown ⇒ `None` (honest).
fn normalize_severity(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "info" | "informational" => Some("none".to_string()),
        "low" => Some("low".to_string()),
        "moderate" | "medium" => Some("moderate".to_string()),
        "high" => Some("high".to_string()),
        "critical" => Some("critical".to_string()),
        _ => None,
    }
}

/// The maximum severity across a CVE set as coord's snake_case token. An empty
/// set (or all-unknown severities) ⇒ `"none"`. This is the runner-side mirror
/// of coord's `severity_max` derivation (coord recomputes it, but the producer
/// uses this for logging + the dry-run gate pre-check).
pub fn severity_max(cves: &[Cve]) -> &'static str {
    let mut best = 0usize; // index into SEVERITY_RANK; 0 == "none"
    for c in cves {
        if let Some(sev) = &c.severity {
            if let Some(rank) = SEVERITY_RANK.iter().position(|s| *s == sev.as_str()) {
                best = best.max(rank);
            }
        }
    }
    SEVERITY_RANK[best]
}

// ---------------------------------------------------------------------------
// npm audit --json
// ---------------------------------------------------------------------------

/// Parse `npm audit --json`. npm v7+ emits a `vulnerabilities` object keyed by
/// package name, each `{ severity, via: [...] }` where `via` entries are either
/// a transitive package name (string) or an advisory object
/// `{ source, name, title, severity, url }`. We emit one [`Cve`] per advisory
/// object found (id from `source`/`url`/`title`), tagged with the affected
/// package. Falls back to the legacy v6 `advisories` map. Garbage ⇒ empty.
pub fn parse_npm_audit(text: &str) -> Vec<Cve> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // npm v7+ `vulnerabilities` map.
    if let Some(vulns) = v.get("vulnerabilities").and_then(|x| x.as_object()) {
        for (pkg, entry) in vulns {
            let pkg_sev = entry
                .get("severity")
                .and_then(|s| s.as_str())
                .and_then(normalize_severity);
            if let Some(via) = entry.get("via").and_then(|x| x.as_array()) {
                for v in via {
                    // Only `via` objects are real advisories; bare strings are
                    // transitive package names (covered by their own entry).
                    if let Some(obj) = v.as_object() {
                        let id = obj
                            .get("source")
                            .map(|s| s.to_string())
                            .filter(|s| s != "null")
                            .or_else(|| obj.get("url").and_then(|u| u.as_str()).map(String::from))
                            .or_else(|| obj.get("title").and_then(|t| t.as_str()).map(String::from))
                            .unwrap_or_else(|| format!("npm:{pkg}"));
                        let severity = obj
                            .get("severity")
                            .and_then(|s| s.as_str())
                            .and_then(normalize_severity)
                            .or_else(|| pkg_sev.clone());
                        out.push(Cve {
                            id,
                            severity,
                            package: Some(pkg.clone()),
                        });
                    }
                }
            }
            // A vulnerability entry with no advisory `via` objects still
            // represents a real finding — record it keyed by the package.
            if !entry
                .get("via")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().any(|e| e.is_object()))
                .unwrap_or(false)
            {
                out.push(Cve {
                    id: format!("npm:{pkg}"),
                    severity: pkg_sev,
                    package: Some(pkg.clone()),
                });
            }
        }
        return out;
    }

    // Legacy npm v6 `advisories` map.
    if let Some(adv) = v.get("advisories").and_then(|x| x.as_object()) {
        for (id, entry) in adv {
            let severity = entry
                .get("severity")
                .and_then(|s| s.as_str())
                .and_then(normalize_severity);
            let package = entry
                .get("module_name")
                .and_then(|m| m.as_str())
                .map(String::from);
            out.push(Cve {
                id: id.clone(),
                severity,
                package,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// cargo audit --json
// ---------------------------------------------------------------------------

/// Parse `cargo audit --json` (RustSec). The report is
/// `{ "vulnerabilities": { "list": [ { advisory: { id, title }, package:
/// { name, version }, ... } ] } }`. RustSec advisories carry a CVSS string in
/// `advisory.cvss` (a vector, not a category) — we map the numeric base score
/// to coord's bands when present, else leave severity `None`. Garbage ⇒ empty.
pub fn parse_cargo_audit(text: &str) -> Vec<Cve> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Some(list) = v
        .get("vulnerabilities")
        .and_then(|x| x.get("list"))
        .and_then(|x| x.as_array())
    else {
        return out;
    };
    for item in list {
        let advisory = item.get("advisory");
        let id = advisory
            .and_then(|a| a.get("id"))
            .and_then(|i| i.as_str())
            .map(String::from)
            .unwrap_or_else(|| "RUSTSEC-UNKNOWN".to_string());
        // RustSec puts a category in `advisory.cvss` (a vector) — derive a band
        // from the base score if the report carries a numeric `score`, else
        // honest `None`.
        let severity = advisory
            .and_then(|a| a.get("severity"))
            .and_then(|s| s.as_str())
            .and_then(normalize_severity)
            .or_else(|| {
                advisory
                    .and_then(|a| a.get("cvss"))
                    .and_then(|c| c.as_f64())
                    .map(cvss_band)
            });
        let package = item
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(String::from);
        out.push(Cve {
            id,
            severity,
            package,
        });
    }
    out
}

/// Map a CVSS v3 base score to coord's severity band (the standard NVD ranges).
fn cvss_band(score: f64) -> String {
    let band = if score >= 9.0 {
        "critical"
    } else if score >= 7.0 {
        "high"
    } else if score >= 4.0 {
        "moderate"
    } else if score > 0.0 {
        "low"
    } else {
        "none"
    };
    band.to_string()
}

// ---------------------------------------------------------------------------
// pip-audit -f json
// ---------------------------------------------------------------------------

/// Parse `pip-audit -f json`. The report is
/// `{ "dependencies": [ { "name", "version", "vulns": [ { "id", "fix_versions",
/// "aliases", "description" } ] } ] }`. pip-audit does NOT emit a severity in
/// its default output, so severity is honest `None` unless a `severity`/`fix`
/// hint is present. Also tolerates the older top-level array form. Garbage ⇒
/// empty.
pub fn parse_pip_audit(text: &str) -> Vec<Cve> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    // Modern object form: { "dependencies": [ { name, vulns: [...] } ] }.
    let deps = v
        .get("dependencies")
        .and_then(|d| d.as_array())
        .cloned()
        // Older form: a bare top-level array of dependency objects.
        .or_else(|| v.as_array().cloned());

    let Some(deps) = deps else {
        return out;
    };
    for dep in deps {
        let pkg = dep.get("name").and_then(|n| n.as_str()).map(String::from);
        if let Some(vulns) = dep.get("vulns").and_then(|x| x.as_array()) {
            for vuln in vulns {
                let id = vuln
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| "PYSEC-UNKNOWN".to_string());
                // pip-audit has no severity field by default ⇒ honest None.
                let severity = vuln
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .and_then(normalize_severity);
                out.push(Cve {
                    id,
                    severity,
                    package: pkg.clone(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_audit_v7_vulnerabilities_map() {
        let text = r#"{
            "vulnerabilities": {
                "minimist": {
                    "name": "minimist",
                    "severity": "critical",
                    "via": [
                        {
                            "source": 1179,
                            "name": "minimist",
                            "title": "Prototype Pollution",
                            "url": "https://github.com/advisories/GHSA-xvch",
                            "severity": "critical"
                        }
                    ]
                }
            }
        }"#;
        let cves = parse_npm_audit(text);
        assert_eq!(cves.len(), 1);
        assert_eq!(cves[0].id, "1179");
        assert_eq!(cves[0].severity.as_deref(), Some("critical"));
        assert_eq!(cves[0].package.as_deref(), Some("minimist"));
        assert_eq!(severity_max(&cves), "critical");
    }

    #[test]
    fn npm_audit_via_string_only_records_package_finding() {
        // a vuln whose `via` is only transitive strings still records a finding
        let text = r#"{
            "vulnerabilities": {
                "lodash": { "severity": "high", "via": ["minimist"] }
            }
        }"#;
        let cves = parse_npm_audit(text);
        assert_eq!(cves.len(), 1);
        assert_eq!(cves[0].package.as_deref(), Some("lodash"));
        assert_eq!(cves[0].severity.as_deref(), Some("high"));
    }

    #[test]
    fn npm_audit_legacy_v6_advisories() {
        let text = r#"{
            "advisories": {
                "1065": { "severity": "moderate", "module_name": "hoek" }
            }
        }"#;
        let cves = parse_npm_audit(text);
        assert_eq!(cves.len(), 1);
        assert_eq!(cves[0].id, "1065");
        assert_eq!(cves[0].severity.as_deref(), Some("moderate"));
        assert_eq!(cves[0].package.as_deref(), Some("hoek"));
    }

    #[test]
    fn cargo_audit_list() {
        let text = r#"{
            "vulnerabilities": {
                "found": true,
                "count": 1,
                "list": [
                    {
                        "advisory": { "id": "RUSTSEC-2021-0079", "title": "x", "severity": "high" },
                        "package": { "name": "hyper", "version": "0.14.0" }
                    }
                ]
            }
        }"#;
        let cves = parse_cargo_audit(text);
        assert_eq!(cves.len(), 1);
        assert_eq!(cves[0].id, "RUSTSEC-2021-0079");
        assert_eq!(cves[0].severity.as_deref(), Some("high"));
        assert_eq!(cves[0].package.as_deref(), Some("hyper"));
    }

    #[test]
    fn cargo_audit_cvss_numeric_band() {
        let text = r#"{
            "vulnerabilities": { "list": [
                { "advisory": { "id": "RUSTSEC-2020-0001", "cvss": 9.8 },
                  "package": { "name": "smallvec" } }
            ] }
        }"#;
        let cves = parse_cargo_audit(text);
        assert_eq!(cves[0].severity.as_deref(), Some("critical"));
    }

    #[test]
    fn pip_audit_dependencies_vulns() {
        let text = r#"{
            "dependencies": [
                {
                    "name": "jinja2",
                    "version": "2.4.1",
                    "vulns": [
                        { "id": "PYSEC-2019-217", "fix_versions": ["2.11.3"], "aliases": ["CVE-2019-10906"] }
                    ]
                },
                { "name": "safe-pkg", "version": "1.0.0", "vulns": [] }
            ]
        }"#;
        let cves = parse_pip_audit(text);
        assert_eq!(cves.len(), 1);
        assert_eq!(cves[0].id, "PYSEC-2019-217");
        assert_eq!(cves[0].package.as_deref(), Some("jinja2"));
        // pip-audit default has no severity ⇒ honest None ⇒ severity_max "none"
        assert!(cves[0].severity.is_none());
        assert_eq!(severity_max(&cves), "none");
    }

    #[test]
    fn severity_max_picks_highest() {
        let cves = vec![
            Cve {
                id: "a".into(),
                severity: Some("low".into()),
                package: None,
            },
            Cve {
                id: "b".into(),
                severity: Some("high".into()),
                package: None,
            },
            Cve {
                id: "c".into(),
                severity: Some("moderate".into()),
                package: None,
            },
        ];
        assert_eq!(severity_max(&cves), "high");
    }

    #[test]
    fn severity_max_empty_is_none() {
        assert_eq!(severity_max(&[]), "none");
    }

    #[test]
    fn unknown_severity_normalizes_to_none_token() {
        // an "info" npm severity normalizes to "none"; a truly unknown one ⇒ None
        assert_eq!(normalize_severity("info").as_deref(), Some("none"));
        assert_eq!(normalize_severity("medium").as_deref(), Some("moderate"));
        assert_eq!(normalize_severity("bogus"), None);
    }

    // ---- malformed input ⇒ empty vec, never panic --------------------------

    #[test]
    fn malformed_audit_inputs_are_empty_not_panic() {
        assert!(parse_npm_audit("not json {{").is_empty());
        assert!(parse_npm_audit("[]").is_empty()); // valid json, wrong shape
        assert!(parse_npm_audit("{}").is_empty());
        assert!(parse_cargo_audit("not json").is_empty());
        assert!(parse_cargo_audit(r#"{"vulnerabilities": {}}"#).is_empty());
        assert!(parse_pip_audit("not json").is_empty());
        assert!(parse_pip_audit("{}").is_empty());
        assert!(parse_pip_audit("").is_empty());
    }
}
