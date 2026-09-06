//! MCP API Module
//!
//! This module provides the HTTP API for the qontinui-runner.
//! Handlers are being incrementally extracted from mcp_api.rs into focused submodules.
//!
//! ## Module structure
//!
//! - `types` - Authoritative ApiState, ApiResponse, and request/response types
//! - `shared` - Cross-cutting utilities (emit_ai_output, FindingContext, etc.)
//! - `goals` - Goal verification logic
//! - `server` - HTTP routing and server initialization (delegates to mcp_api)
//! - `awas` - AWAS (Application Web Automation Specification) handlers
//! - `awas_bridge` - Bridge between AWAS and ui-bridge systems

pub mod accessibility;
pub mod action_plan_cache;
pub mod adb_helper;
// `GET /agent-tokens/health` — publishes the per-agent JWT refresh loop's
// own output, so a latched-off refresh (silent by design) is legible
// somewhere other than the log file.
pub mod agent_tokens;
pub mod agent_worktrees;
pub mod ai_generation;
pub mod ai_network_probe;
pub mod ai_session;
pub mod ai_wait_for;
pub mod api_requests;
pub mod api_spec_verify;
pub mod api_surface;
pub mod api_surface_diff;
pub mod app_discovery;
pub mod app_dispatch;
pub mod app_registry;
pub mod auto_continue;
pub mod automation_runs;
pub mod awas;
pub mod awas_bridge;
pub mod backend_relay;
pub mod backup_restore;
pub mod blind_spots_api;
pub mod bridges;
pub mod canvas;
pub mod cascade;
pub mod checks;
pub mod code_semantics;
pub mod command_relay;
pub mod comparison_api;
pub mod completion_reports;
pub mod completion_sources;
pub mod configs;
pub mod constraints_api;
pub mod container_status;
pub mod contexts;
pub mod continuation_verdict;
pub mod coordinator;
pub mod debug_builder_prompt;
/// Debug-only UI-thread wedge affordance. Absent from release builds — see the
/// module docs for why the gate is load-bearing.
#[cfg(debug_assertions)]
pub mod debug_wedge;
pub mod decision_trail_api;
pub mod development_intelligence;
pub mod device_jwt_refresher;
pub mod discovery;
// `GET /disk/reclaimable` — the runner-local disk-reclaim preview (INV-D1:
// answers with a build in flight, never gated on the deletion gates).
pub mod disk_reclaim;
pub mod dom_capture;
pub mod entity_profiles_api;
pub mod envelope;
#[cfg(debug_assertions)]
pub mod envelope_audit;
pub mod error_monitor;
pub mod executor;
pub mod extraction;
pub mod file_browser;
pub mod file_registry;
pub mod findings_api;
pub mod fleet_policy_poller;
pub mod generation_rules_api;
pub mod generator_eval;
pub mod git_supervision_api;
pub mod github_budget_api;
pub mod goals;
pub mod graph_api;
pub mod gui_config;
pub mod gui_execution;
pub mod headless_browser;
pub mod hitl;
pub mod hooks;
pub mod image_quality_tests;
pub mod inngest;
pub mod interaction_recording;
pub mod knowledge;
pub mod knowledge_acquisition_api;
pub mod log_sources;
pub mod macros;
pub mod mcp_servers;
pub mod memory_consolidation_api;
pub mod meta_optimizer_api;
pub mod misc;
pub mod models;
pub mod monitors;
pub mod observations_api;
pub mod online_learning_api;
pub mod orchestration_loop_api;
pub mod orchestration_report;
pub mod orchestration_run_api;
pub mod otel_status;
pub mod pg_guard;
pub mod physical_device;
pub mod physical_device_api;
pub mod plan_library;
pub mod playwright;
pub mod playwright_collection;
pub mod policy_context;
pub mod prm_export;
pub mod probe_executor;
pub mod processes;
pub mod prompt_home;
pub mod prompt_snippets;
pub mod prompts;
pub mod provider_health;
pub mod query_memory_tool;
pub mod query_tool;
pub mod queue;
pub mod rag;
pub mod recordings;
pub mod reflection;
pub mod reflection_api;
pub mod relay_routable;
pub mod restart_readiness;
pub mod restate_api;
pub mod reviews;
pub mod saved_api_requests;
pub mod scheduler;
pub mod sdk_client;
pub mod sdk_terminal_buffer;
pub mod security_audit;
pub mod server;
pub mod session_briefing;
pub mod session_compliance;
pub mod session_message_poller;
pub mod session_recap;
pub mod session_repository;
pub mod sessions;
pub mod settings;
pub mod shared;
pub mod shell_commands;
pub mod skills;
pub mod snapshots;
pub mod state_explorer;
pub mod state_machine;
pub mod step_evaluation_api;
pub mod step_type_knowledge_api;
pub mod step_type_metadata_api;
pub mod steward;
pub mod streaming;
pub mod subagent_api;
pub mod task_run_inspection;
pub mod task_run_queries;
pub mod task_run_structured_output;
pub mod task_run_workflow_state;
pub mod task_runs;
pub mod task_supervisor;
pub mod tauri_proxy;
pub mod terminals;
pub mod testing;
// Phase 5.1 of the UI Bridge discoverability/effectiveness plan:
// debug-only `/ui-bridge/test/inject-session` + `/ui-bridge/test/clear-sessions`
// for manual SessionCard-rendering tests. The cfg gate keeps the module
// (and its routes) out of production release builds unless `test-fixtures`
// is explicitly enabled.
#[cfg(any(debug_assertions, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod token_analytics;
pub mod trace_verification;
pub mod transport;
pub mod triggers;
pub mod tunnel_api;
pub mod types;
pub mod ui_bridge;
pub mod ui_bridge_integration;
pub mod ui_bridge_invoke_handlers;
pub mod unified_workflows;
pub mod verification_tests;
pub mod web_backend_workflows;
pub mod websocket;
pub mod window_manager;
pub mod worktrees;
pub mod ws_bridge_dispatch;
pub mod ws_relay;

// ===========================================================================
// The route-registration control
// ===========================================================================

/// A module that publishes an HTTP surface must actually be MOUNTED on one.
///
/// # The control this replaces could not observe the thing it named
///
/// `session_briefing::route_entries` and `plan_library::route_entries` each
/// claim, in their own doc comments, that the table plus its count test is
/// "what catches a route added to `routes()` and forgotten in `mcp_api`'s
/// `.merge(…)`". Neither test can observe a `.merge`. They assert the table's
/// length and that `routes()` compiles — and BOTH stay true when the `.merge`
/// line is deleted and every endpoint in the family starts answering 404.
///
/// That case is constructible, which is what makes this a defect rather than a
/// suspicion: delete `.merge(crate::mcp::session_briefing::routes())` from
/// `mcp_api.rs` and `the_route_table_is_in_lockstep_with_routes` still passes,
/// because nothing it looks at changed. `GET /session-briefing` — the route the
/// settings panel and `/whereami` read provenance from — is simply gone.
///
/// qontinui-runner#1226 named the gap and deferred it, on the reasoning that
/// closing it properly means asserting against the assembled router. That is
/// genuinely blocked: `types::ApiState` holds a `tauri::AppHandle`, so the
/// `Router<Arc<ApiState>>` the families return cannot be given state and driven
/// with `tower::ServiceExt::oneshot` from a unit test. But "cannot drive the
/// router" is not "cannot test the property" — the registration itself is a
/// `.merge(…)` line in the source, and reading it is exactly what
/// `ui_bridge::manifest_drift_tests` already does one level down for `.route(…)`
/// calls.
///
/// # Bound, stated rather than papered over
///
/// A module is counted as mounted when some OTHER file references
/// `<name>::routes()`. So a family merged only into a parent that is itself
/// unmounted reads as mounted here. The parent is still flagged, so the
/// condition surfaces — but it surfaces under the parent's name, not the
/// child's. Chasing the reference graph transitively would need a real module
/// resolver; the flat check is what catches the failure this control exists
/// for, which is a family nobody wired up at all.
///
/// Comments are stripped before the scan, because prose about a `.merge` is not
/// a `.merge` — this module's own doc comment named one and satisfied the
/// control for the family it was describing. STRING LITERALS are not stripped,
/// so a literal containing `<name>::routes()` would still read as a call site.
/// That is a known residual, kept because stripping literals means handling raw
/// and nested-raw strings for a case that has arisen exactly once: inside this
/// module's own tests, where the needle is composed at runtime to avoid it.
#[cfg(test)]
mod route_registration_tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// The name a module is spelled with at a call site: the file stem, or the
    /// DIRECTORY name for a `mod.rs`. `ui_bridge/mod.rs` is merged as
    /// `ui_bridge::routes()` and never as `mod::routes()`.
    fn module_name(path: &Path) -> String {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if stem == "mod" {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        } else {
            stem.to_string()
        }
    }

    /// Every `.rs` under `src/`, as `path -> contents`. The whole tree, not
    /// just `src/mcp/`: `trace_api` lives at `src/trace_api/` and is merged
    /// from `mcp_api.rs` like any other family.
    fn crate_sources() -> BTreeMap<PathBuf, String> {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            out.insert(path.to_path_buf(), text);
        }
        assert!(!out.is_empty(), "no sources under {}", src.display());
        out
    }

    /// Comments are PROSE, not registrations.
    ///
    /// This module's own doc comment names
    /// `.merge(crate::mcp::session_briefing::routes())` while explaining the
    /// counter-case. Scanning raw text, the control read that explanation as
    /// the registration and passed with the real `.merge` line deleted —
    /// measured, not feared. A control satisfied by its own documentation is
    /// the decoration this one replaces.
    ///
    /// Whole-line comments (`//`, `///`, `//!`) are dropped entirely. A
    /// TRAILING comment is cut only when no string literal opens before it, so
    /// a line carrying a `"http://…"` is left intact rather than truncated at
    /// the `//` inside the URL.
    fn strip_comments(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            if line.trim_start().starts_with("//") {
                out.push('\n');
                continue;
            }
            match line.find("//") {
                Some(i) if !line[..i].contains('"') => out.push_str(&line[..i]),
                _ => out.push_str(line),
            }
            out.push('\n');
        }
        out
    }

    /// The modules that define `routes()` and that NOTHING else calls.
    ///
    /// Pure over a source map so the control can be driven against a MUTATED
    /// tree and shown to fail, rather than only against the healthy one where
    /// a broken control and a working one look identical.
    fn unmounted_modules(sources: &BTreeMap<PathBuf, String>) -> Vec<String> {
        // Anchored to a line start so a doc comment QUOTING `pub fn routes()`
        // cannot register a module that defines no such thing.
        let defines = regex::Regex::new(r"(?m)^\s*pub(?:\(crate\)|\(super\))?\s+fn\s+routes\s*\(")
            .expect("static pattern");
        // Any `<module>::routes()` call site, however it is qualified:
        // `ai::routes()`, `crate::mcp::tauri_proxy::routes()`, and the arm of a
        // conditional merge alike — `trace_api` is mounted inside an `if`, so
        // matching only `.merge(<name>::routes())` would call it unmounted.
        let references =
            regex::Regex::new(r"\b([a-z_][a-z_0-9]*)::routes\s*\(\s*\)").expect("static pattern");

        let mut definitions: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut referenced_in: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for (path, text) in sources {
            let code = strip_comments(text);
            if defines.is_match(&code) {
                definitions
                    .entry(module_name(path))
                    .or_default()
                    .push(path.clone());
            }
            for cap in references.captures_iter(&code) {
                referenced_in
                    .entry(cap[1].to_string())
                    .or_default()
                    .push(path.clone());
            }
        }

        // A module is MOUNTED when some OTHER file calls its `routes()`. Its
        // own file does not count: every family's lockstep test says
        // `let _r: Router<Arc<ApiState>> = routes();`, and a module must not be
        // able to mount itself.
        definitions
            .iter()
            .filter(|(name, defined_in)| {
                !referenced_in
                    .get(name.as_str())
                    .is_some_and(|files| files.iter().any(|f| !defined_in.contains(f)))
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// How many modules define `routes()`. A sanity floor: a regex that
    /// silently matched nothing would leave every assertion below passing
    /// forever while testing nothing at all.
    fn definition_count(sources: &BTreeMap<PathBuf, String>) -> usize {
        let defines = regex::Regex::new(r"(?m)^\s*pub(?:\(crate\)|\(super\))?\s+fn\s+routes\s*\(")
            .expect("static pattern");
        sources
            .iter()
            .filter(|(_, text)| defines.is_match(&strip_comments(text)))
            .map(|(path, _)| module_name(path))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// The property the two `route_entries` doc comments state: a family's
    /// `routes()` is reachable from the assembled router, not merely defined.
    #[test]
    fn every_module_that_defines_routes_is_mounted() {
        let sources = crate_sources();
        assert!(
            definition_count(&sources) > 100,
            "expected >100 route-defining modules, found {} — the definition \
             regex has drifted and this control can no longer fail",
            definition_count(&sources)
        );

        let unmounted = unmounted_modules(&sources);
        assert!(
            unmounted.is_empty(),
            "these modules define `routes()` that NOTHING mounts, so every \
             endpoint they declare answers 404:\n  {}\n\nMerge each into the \
             router that should carry it — `mcp_api.rs` for a top-level \
             family, `mcp/ui_bridge/mod.rs` for a UI Bridge one.",
            unmounted.join("\n  ")
        );
    }

    /// **The control can fail**, driven against a tree with the registration
    /// actually removed.
    ///
    /// This is the case the count test in `session_briefing` cannot see and
    /// the reason this module exists: delete the `.merge(…)` line and
    /// `the_route_table_is_in_lockstep_with_routes` still passes, because
    /// nothing it looks at changed, while `GET /session-briefing` 404s.
    ///
    /// A control only ever exercised against a healthy tree is
    /// indistinguishable from one that cannot fail at all — which is exactly
    /// what the first draft of this module turned out to be.
    #[test]
    fn the_control_flags_a_family_whose_merge_was_deleted() {
        let mut sources = crate_sources();
        let api = sources
            .keys()
            .find(|p| p.ends_with("mcp_api.rs"))
            .expect("mcp_api.rs is in the tree")
            .clone();
        // Composed at runtime on purpose. Spelled as one literal, this needle
        // would ITSELF be a `session_briefing::routes()` occurrence in this
        // file, and would mount the very family the test is trying to unmount
        // — the string-literal twin of the comment case below, and a mistake
        // this test caught being made in its own first draft.
        let family = "session_briefing";
        let merge = format!(".merge(crate::mcp::{family}::routes())");

        let text = sources.get_mut(&api).expect("just found it");
        assert!(
            text.contains(&merge),
            "the registration this test deletes must exist to begin with"
        );
        *text = text.replace(&merge, "");

        assert!(
            unmounted_modules(&sources).contains(&family.to_string()),
            "deleting the only `.merge` of a family must flag it"
        );
    }

    /// A module is mounted by a `.merge`, never by PROSE about one.
    ///
    /// This module's own doc comment names
    /// `.merge(crate::mcp::session_briefing::routes())`, and scanning raw text
    /// made that sentence satisfy the control for the very family it was
    /// describing.
    #[test]
    fn a_comment_naming_a_family_does_not_mount_it() {
        let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
        sources.insert(
            PathBuf::from("src/mcp/lonely.rs"),
            "pub fn routes() -> Router {\n    Router::new()\n}\n".to_string(),
        );
        sources.insert(
            PathBuf::from("src/mcp_api.rs"),
            "// merge crate::mcp::lonely::routes() one day\n/// see `lonely::routes()`\nfn build() {}\n"
                .to_string(),
        );
        assert_eq!(
            unmounted_modules(&sources),
            vec!["lonely".to_string()],
            "prose naming a family must not register it"
        );

        // …and a real call site DOES mount it, so the assertion above is not
        // passing because the reference scan matches nothing at all.
        sources.insert(
            PathBuf::from("src/mcp_api.rs"),
            "fn build() { let _ = crate::mcp::lonely::routes(); }\n".to_string(),
        );
        assert!(unmounted_modules(&sources).is_empty());
    }
}
