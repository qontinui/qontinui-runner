//! `qontinui-layers.toml` — the **authored declared side** for `Ξ_Layering`
//! (Phase 4b). When a repo ships this manifest, `layer_observer` labels modules
//! from it (instead of the `Ξ_Arch` kernel) and the verdict becomes **gate-grade**:
//! the declared side is authored intent (the spec), so per the construct's
//! credibility-gates-the-gate rule the breach detection — deterministic over the
//! resolved `Ξ_AST` graph — earns a gate recommendation, not just an advisory hint.
//!
//! Schema (see `plans/2026-06-07-twin-layering-architecture-drift.md` Appendix A):
//! ```toml
//! [order]                       # optional; defaults to the built-in downward order
//! allowed = [["ui","api"],["ui","service"],["api","service"],["service","data"]]
//! forbid_skip = true
//! [layers]                      # glob → layer; MOST-SPECIFIC (longest prefix) wins
//! "src-tauri/src/commands/**" = "api"
//! "src-tauri/src/**"          = "service"
//! [exempt]                      # composition roots/barrels: never judged, excluded from cycles
//! modules = ["src-tauri/src"]
//! [carve_out]                   # the construct's C — edges expected to differ (accepted)
//! edges = [ { from_glob = "src-tauri/src/database/**", to_layer = "service", reason = "…" } ]
//! ```
//! Resolution: a `P/**` glob matches a module `M` iff `M == P` or `M` starts with
//! `P/`; an exact (non-glob) pattern matches `M == pattern`. Among matching `[layers]`
//! patterns the longest prefix wins (exact beats glob). Everything is deterministic
//! and parses with the already-present `toml` crate — no model call, no I/O beyond
//! the one file read.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

/// The built-in downward allowed-edge relation (used when `[order].allowed` is empty).
const DEFAULT_ALLOWED: &[(&str, &str)] = &[
    ("ui", "api"),
    ("ui", "service"),
    ("api", "service"),
    ("service", "data"),
];

// ===========================================================================
// Raw (serde) shape
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
struct RawManifest {
    #[serde(default)]
    order: RawOrder,
    #[serde(default)]
    layers: BTreeMap<String, String>,
    #[serde(default)]
    exempt: RawExempt,
    #[serde(default)]
    carve_out: RawCarve,
}

#[derive(Debug, Deserialize)]
struct RawOrder {
    #[serde(default)]
    allowed: Vec<Vec<String>>,
    #[serde(default = "default_true")]
    #[allow(dead_code)] // documents intent; non-listed pairs are breaches regardless
    forbid_skip: bool,
}
impl Default for RawOrder {
    fn default() -> Self {
        RawOrder {
            allowed: Vec::new(),
            forbid_skip: true,
        }
    }
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
struct RawExempt {
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    globs: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawCarve {
    #[serde(default)]
    edges: Vec<RawCarveRule>,
}

#[derive(Debug, Deserialize)]
struct RawCarveRule {
    from_glob: Option<String>,
    to_layer: Option<String>,
}

// ===========================================================================
// Resolved manifest
// ===========================================================================

/// A glob/exact pattern reduced to its literal prefix + whether it is a glob.
#[derive(Debug, Clone)]
struct Pat {
    prefix: String,
    is_glob: bool,
}

impl Pat {
    fn parse(pattern: &str) -> Pat {
        if let Some(p) = pattern
            .strip_suffix("/**")
            .or_else(|| pattern.strip_suffix("/*"))
        {
            Pat {
                prefix: p.to_string(),
                is_glob: true,
            }
        } else {
            Pat {
                prefix: pattern.to_string(),
                is_glob: false,
            }
        }
    }

    fn matches(&self, module: &str) -> bool {
        if self.is_glob {
            module == self.prefix || module.starts_with(&format!("{}/", self.prefix))
        } else {
            module == self.prefix
        }
    }

    /// Specificity for "most-specific wins": prefix length, with an exact match
    /// beating a glob of the same length.
    fn specificity(&self) -> (usize, bool) {
        (self.prefix.len(), !self.is_glob)
    }
}

/// A carve-out rule: an edge whose `from` matches `from_glob` and whose `to` layer
/// equals `to_layer` is accepted (never a breach), recorded in the verdict.
#[derive(Debug, Clone)]
struct CarveRule {
    from: Pat,
    to_layer: String,
}

/// The parsed, resolved manifest.
#[derive(Debug, Clone, Default)]
pub(super) struct LayerManifest {
    /// `[layers]` patterns sorted most-specific-first.
    layers: Vec<(Pat, String)>,
    /// `[exempt]` patterns (modules + globs).
    exempt: Vec<Pat>,
    carve: Vec<CarveRule>,
    allowed: Vec<(String, String)>,
}

impl LayerManifest {
    /// Load `<project_dir>/qontinui-layers.toml`. Returns `None` if the file is
    /// absent or unparseable (the caller then falls back to the `Ξ_Arch` kernel
    /// path). The manifest lives at the repo ROOT — NOT under `.qontinui/`, which is
    /// a runtime-state dir gitignored in some repos (an authored declared side must
    /// be committable).
    pub(super) fn load(project_dir: &Path) -> Option<LayerManifest> {
        let path = project_dir.join("qontinui-layers.toml");
        let content = std::fs::read_to_string(&path).ok()?;
        Self::parse(&content)
    }

    /// Parse manifest text (pure — the unit tests drive this directly).
    pub(super) fn parse(content: &str) -> Option<LayerManifest> {
        let raw: RawManifest = toml::from_str(content).ok()?;

        let mut layers: Vec<(Pat, String)> = raw
            .layers
            .into_iter()
            .map(|(pat, layer)| (Pat::parse(&pat), layer))
            .collect();
        // Most-specific first: longer prefix wins; exact beats glob on a tie.
        layers.sort_by_key(|(pat, _)| std::cmp::Reverse(pat.specificity()));

        let mut exempt: Vec<Pat> = Vec::new();
        for m in raw.exempt.modules {
            exempt.push(Pat::parse(&m));
        }
        for g in raw.exempt.globs {
            exempt.push(Pat::parse(&g));
        }

        let carve: Vec<CarveRule> = raw
            .carve_out
            .edges
            .into_iter()
            .filter_map(|r| match (r.from_glob, r.to_layer) {
                (Some(from), Some(to_layer)) => Some(CarveRule {
                    from: Pat::parse(&from),
                    to_layer,
                }),
                _ => None,
            })
            .collect();

        let allowed: Vec<(String, String)> = if raw.order.allowed.is_empty() {
            DEFAULT_ALLOWED
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        } else {
            raw.order
                .allowed
                .into_iter()
                .filter_map(|pair| match pair.as_slice() {
                    [a, b] => Some((a.clone(), b.clone())),
                    _ => None,
                })
                .collect()
        };

        Some(LayerManifest {
            layers,
            exempt,
            carve,
            allowed,
        })
    }

    /// The declared layer for a module, by the most-specific matching `[layers]`
    /// pattern. `None` if no pattern matches (a manifest coverage gap — the caller
    /// treats it as an `unknown`/unlabelled endpoint, lowering coverage honestly).
    pub(super) fn layer_of(&self, module: &str) -> Option<&str> {
        self.layers
            .iter()
            .find(|(pat, _)| pat.matches(module))
            .map(|(_, layer)| layer.as_str())
    }

    /// Is this module exempt (a composition root / barrel)? Exempt modules are never
    /// judged by Φ and are excluded from cycle detection.
    pub(super) fn is_exempt(&self, module: &str) -> bool {
        self.exempt.iter().any(|p| p.matches(module))
    }

    /// Is `a → b` an allowed layer edge? Intra-layer (`a == b`) is always allowed;
    /// otherwise membership in the (possibly overridden) allowed set.
    pub(super) fn is_allowed(&self, a: &str, b: &str) -> bool {
        a == b
            || self
                .allowed
                .iter()
                .any(|(x, y)| x.as_str() == a && y.as_str() == b)
    }

    /// Is the edge `from` (→ a module of layer `to_layer`) an accepted carve-out?
    pub(super) fn is_carved(&self, from: &str, to_layer: &str) -> bool {
        self.carve
            .iter()
            .any(|r| r.to_layer == to_layer && r.from.matches(from))
    }
}

// ===========================================================================
// Tests (pure)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[order]
allowed = [["ui","api"],["ui","service"],["api","service"],["service","data"],["api","data"]]
forbid_skip = true

[layers]
"src-tauri/src/commands/**" = "api"
"src-tauri/src/mcp/**" = "api"
"src-tauri/src/database/**" = "data"
"src-tauri/src/util/**" = "utility"
"src-tauri/src/**" = "service"
"src/**" = "ui"

[exempt]
modules = ["src-tauri/src"]

[carve_out]
edges = [ { from_glob = "src-tauri/src/database/**", to_layer = "service", reason = "persist domain types" } ]
"#;

    #[test]
    fn parses_and_resolves_most_specific_glob() {
        let m = LayerManifest::parse(SAMPLE).expect("parses");
        // most-specific wins: commands/** (api) beats the broader src/** (service)
        assert_eq!(m.layer_of("src-tauri/src/commands"), Some("api"));
        assert_eq!(m.layer_of("src-tauri/src/commands/recap"), Some("api"));
        assert_eq!(m.layer_of("src-tauri/src/mcp"), Some("api"));
        assert_eq!(m.layer_of("src-tauri/src/database/pg"), Some("data"));
        assert_eq!(m.layer_of("src-tauri/src/util"), Some("utility"));
        // falls back to the broad service glob
        assert_eq!(m.layer_of("src-tauri/src/orchestrator"), Some("service"));
        // the TS frontend
        assert_eq!(m.layer_of("src/components"), Some("ui"));
        // no glob covers this → a manifest gap (unknown)
        assert_eq!(m.layer_of("vendor/thirdparty"), None);
    }

    #[test]
    fn exempt_matches_root_only() {
        let m = LayerManifest::parse(SAMPLE).unwrap();
        assert!(m.is_exempt("src-tauri/src"));
        // a child of the exempt root is NOT itself exempt (exact match)
        assert!(!m.is_exempt("src-tauri/src/commands"));
    }

    #[test]
    fn order_override_sanctions_api_to_data() {
        let m = LayerManifest::parse(SAMPLE).unwrap();
        assert!(m.is_allowed("ui", "api"));
        assert!(m.is_allowed("service", "data"));
        assert!(
            m.is_allowed("api", "data"),
            "the [order] override sanctions api→data"
        );
        assert!(
            m.is_allowed("service", "service"),
            "intra-layer always allowed"
        );
        // still forbidden:
        assert!(!m.is_allowed("data", "service"));
        assert!(!m.is_allowed("ui", "data"));
        assert!(!m.is_allowed("service", "api"));
    }

    #[test]
    fn carve_out_matches_from_glob_and_to_layer() {
        let m = LayerManifest::parse(SAMPLE).unwrap();
        // database/** → a service-layer target is carved (accepted)
        assert!(m.is_carved("src-tauri/src/database/pg", "service"));
        // database/** → a non-service target is NOT carved
        assert!(!m.is_carved("src-tauri/src/database/pg", "api"));
        // a non-database source is not carved
        assert!(!m.is_carved("src-tauri/src/orchestrator", "service"));
    }

    #[test]
    fn default_order_when_omitted() {
        let m = LayerManifest::parse("[layers]\n\"a/**\" = \"api\"\n").unwrap();
        assert!(m.is_allowed("api", "service"));
        assert!(
            !m.is_allowed("api", "data"),
            "no override → api→data is a breach"
        );
    }

    #[test]
    fn empty_or_garbage() {
        assert!(LayerManifest::parse("").is_some()); // empty manifest is valid (all defaults)
        assert!(LayerManifest::parse("not = [valid").is_none());
    }
}
