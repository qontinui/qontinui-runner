//! Shared `{module → architectural-layer}` classify path for the stochastic-kernel
//! observers.
//!
//! Extracted from `Ξ_Arch` (`arch_observer.rs`) so it has an in-process seam: both
//! `Ξ_Arch` (which renders the labels into its own advisory envelope) and
//! `Ξ_Layering` (which uses the labels to evaluate the layering-allowed-edge
//! relation Φ over the resolved dependency digraph) call [`classify_layers`].
//!
//! This is the LLM-coupled classify step only — prompt build, the forced-CLI call,
//! and the tolerant parse. It is observer-agnostic about envelopes: it returns the
//! raw [`LayerAssignment`]s and lets each observer assemble its own kernel envelope.
//! On any model failure it returns an empty vec (the honest "I couldn't read it"
//! signal — every caller maps that to coverage 0, never a confident false answer).

use serde_json::Value;

use super::module_graph::{self, ModuleSummary};

/// The architectural-layer taxonomy (UA's architecture-analyzer set + `unknown`).
pub(super) const LAYERS: &[&str] = &["api", "service", "data", "ui", "utility", "unknown"];

/// One module's layer assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LayerAssignment {
    pub(super) module: String,
    pub(super) layer: String,
    pub(super) rationale: String,
}

/// Classify each module into an architectural layer via the forced Claude-CLI path.
///
/// `build_prompt` → [`module_graph::run_cli_structured`] → [`parse_layers`]. Returns
/// an empty vec on model failure (honest degrade → the caller maps that to coverage
/// 0). The model sees only the rendered module structure, never raw source.
pub(super) async fn classify_layers(modules: &[ModuleSummary]) -> Vec<LayerAssignment> {
    let prompt = build_prompt(modules);
    match module_graph::run_cli_structured(prompt).await {
        Some(output) => parse_layers(&output),
        None => Vec::new(),
    }
}

// ===========================================================================
// Prompt construction (pure)
// ===========================================================================

/// Build the classification prompt: the arch-layer instruction header + the
/// shared module-list block. The model sees only structure (no source) and must
/// return strict JSON.
fn build_prompt(modules: &[ModuleSummary]) -> String {
    let mut s = String::from(
        "You are classifying the ARCHITECTURAL LAYER of each MODULE in a codebase.\n\
         A module is a directory. For each module you are given its files, its exported\n\
         symbols, the other in-repo modules it imports from, and the external packages\n\
         it uses. You do NOT see source code — classify from this structure alone.\n\n\
         Assign each module EXACTLY ONE layer from this taxonomy:\n\
         - api: HTTP/RPC route handlers, controllers, transport/request-response edges.\n\
         - service: business logic, orchestration, use-cases, domain operations.\n\
         - data: persistence, models, repositories, DB/ORM/migrations, schema.\n\
         - ui: user-interface components, views, widgets, frontend rendering.\n\
         - utility: cross-cutting helpers, shared types, config, logging, pure utils.\n\
         - unknown: the structure is insufficient to decide.\n\n\
         Return ONLY a JSON object, no prose, no code fences:\n\
         {\"modules\":[{\"module\":\"<exact module key>\",\"layer\":\"<taxonomy value>\",\"rationale\":\"<=12 words\"}]}\n\n\
         Modules:\n",
    );
    s.push_str(&module_graph::render_modules(modules));
    s
}

// ===========================================================================
// Response parsing (pure, tolerant)
// ===========================================================================

/// Parse `{"modules":[{module,layer,rationale}]}` (or a bare array) out of the
/// model's output, tolerant of surrounding prose / ```json fences. An unknown or
/// missing layer normalizes to `"unknown"`; entries without a `module` are
/// dropped. Returns an empty vec on any failure (the honest "I couldn't read it"
/// signal — the caller maps that to coverage 0, never a confident false answer).
fn parse_layers(output: &str) -> Vec<LayerAssignment> {
    let json_str = match module_graph::extract_json(output) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    // Accept either `{"modules":[...]}` or a bare `[...]`.
    let arr = match &value {
        Value::Object(map) => map.get("modules").and_then(|v| v.as_array()).cloned(),
        Value::Array(a) => Some(a.clone()),
        _ => None,
    };
    let arr = match arr {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in arr {
        let module = entry
            .get("module")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());
        let module = match module {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let layer = entry
            .get("layer")
            .and_then(|v| v.as_str())
            .map(normalize_layer)
            .unwrap_or_else(|| "unknown".to_string());
        let rationale = entry
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        out.push(LayerAssignment {
            module,
            layer,
            rationale,
        });
    }
    out
}

/// Normalize a model-supplied layer string to the taxonomy; anything off-taxonomy
/// (or empty) becomes `"unknown"` rather than leaking an invented label.
fn normalize_layer(raw: &str) -> String {
    let l = raw.trim().to_lowercase();
    if LAYERS.contains(&l.as_str()) {
        l
    } else {
        "unknown".to_string()
    }
}

// ===========================================================================
// Tests (pure — no LLM call)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_modules_taxonomy_and_json_instruction() {
        let mods = vec![ModuleSummary {
            module: "src/api".into(),
            files: vec!["routes.ts".into()],
            exports: vec!["registerRoutes".into()],
            internal_deps: vec!["src/services".into()],
            external_pkgs: vec!["express".into()],
            export_count: 1,
        }];
        let p = build_prompt(&mods);
        assert!(p.contains("src/api"));
        assert!(p.contains("registerRoutes"));
        assert!(p.contains("imports-from: src/services"));
        assert!(p.contains("external: express"));
        // taxonomy + strict-JSON instruction present
        assert!(p.contains("service:"));
        assert!(p.contains("\"modules\""));
    }

    #[test]
    fn parse_clean_json_object() {
        let out = r#"{"modules":[{"module":"src/api","layer":"api","rationale":"route handlers"},{"module":"src/db","layer":"data","rationale":"models"}]}"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].module, "src/api");
        assert_eq!(parsed[0].layer, "api");
        assert_eq!(parsed[1].layer, "data");
    }

    #[test]
    fn parse_tolerates_prose_and_code_fences() {
        let out = "Sure — here is the classification:\n```json\n{\"modules\":[{\"module\":\"src/ui\",\"layer\":\"UI\",\"rationale\":\"views\"}]}\n```\nLet me know if you need more.";
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].module, "src/ui");
        // layer is normalized to lowercase taxonomy.
        assert_eq!(parsed[0].layer, "ui");
    }

    #[test]
    fn parse_normalizes_offtaxonomy_layer_to_unknown() {
        let out = r#"{"modules":[{"module":"src/x","layer":"controller","rationale":"?"}]}"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].layer, "unknown");
    }

    #[test]
    fn parse_accepts_bare_array_and_drops_module_less_entries() {
        let out = r#"[{"module":"src/a","layer":"service"},{"layer":"data"}]"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1, "entry without a module is dropped");
        assert_eq!(parsed[0].module, "src/a");
        // missing rationale defaults to empty.
        assert_eq!(parsed[0].rationale, "");
    }

    #[test]
    fn parse_garbage_returns_empty() {
        assert!(parse_layers("I cannot help with that.").is_empty());
        assert!(parse_layers("").is_empty());
        assert!(parse_layers("{not json").is_empty());
    }
}
