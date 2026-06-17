//! Phase 2 — whole-page inversion: `FunctionalSpec` -> a multi-screen, navigable,
//! data-layer-wired Expo app (deterministic core).
//!
//! Phase 1 ([`crate::screen::generate_screen`]) proved the load-bearing observability
//! seam for **one** `IrState`. Phase 2 lifts that to the whole page:
//!
//! - every [`IrState`] -> one instrumented `app/<id>.tsx` screen (Phase-1 renderer reused
//!   verbatim — the instrumentation stays byte-identical so the snapshot can never drift);
//! - every [`IrTransition`] -> a navigator edge + a deterministic `onPress` handler wired
//!   onto the transition's `action.target` element (the button the transition fires from),
//!   routing `from_states[0]` -> `activate_states[0]`;
//! - every [`Operation`] -> one typed data-layer service method whose URL is derived by
//!   the **shared** [`qontinui_types::endpoint_for::endpoint_for`] (the single #1<->#2
//!   contract — never re-derived here), wired into the matching transition handler.
//!
//! What stays out of Phase 2 (deferred, honestly): the **LLM presentational fill** (D2 —
//! layout/styling via `run_claude_cli`) and the **on-device render+snapshot** verify leg
//! (no generated-screen render harness exists — the mobile app on the device is a fixed
//! production build; see `tests/runtime_render_deferred.rs`). The deterministic
//! matcher-correctness proof — every emitted screen's instrumentation manifest scores a
//! `FullMatch` against the **real** `qontinui_spec_check::evaluate` — is what Phase 2
//! verifies, exactly as Phase 1 did for one state.

use qontinui_types::endpoint_for::{endpoint_for, Endpoint};
use qontinui_types::functional_spec::{FunctionalSpec, Operation};
use qontinui_types::ir::{IrTransition, IrTransitionAction};
use qontinui_types::priorities_profile::Profile;

use crate::screen::{generate_screen, ScreenArtifact};

/// One inverted navigation edge: the source screen, the destination screen, and the
/// element id on the source screen whose press fires the transition (resolved against the
/// source screen's instrumented elements so the handler is wired to a real, snapshot-able
/// button — the same element the matcher would find for the transition's `action.target`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavEdge {
    /// Transition id (`IrTransition.id`).
    pub transition_id: String,
    /// Source screen route (`from_states[0]`).
    pub from_state: String,
    /// Destination screen route (`activate_states[0]`).
    pub to_state: String,
    /// The instrumented element id on the source screen the press handler attaches to.
    /// `None` when no source-screen element matches the transition's `action.target`
    /// (a *dangling* transition — surfaced rather than silently wired to nothing).
    pub trigger_element_id: Option<String>,
    /// The operation (if any) this transition's handler also invokes, by op name.
    pub invokes_operation: Option<String>,
}

/// One inverted data-layer service method: the operation it serves and the endpoint it
/// calls (URL derived once via the shared [`endpoint_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceMethod {
    /// Operation name (`Operation.name`) — the method name on the generated client.
    pub operation: String,
    /// The derived `{ method, path }` the call targets.
    pub endpoint: Endpoint,
}

/// The whole-page inversion result: one screen per state, the nav graph, the data layer,
/// and the emitted data-layer client source. The `produced_refs` are the dotted spec refs
/// the generator produced (the D4 contract the verify phase consumes).
#[derive(Debug, Clone)]
pub struct GeneratedApp {
    /// One screen artifact per `IrState`, in spec order.
    pub screens: Vec<ScreenArtifact>,
    /// One nav edge per `IrTransition`, in spec order.
    pub nav_edges: Vec<NavEdge>,
    /// One service method per `Operation`, in spec order.
    pub services: Vec<ServiceMethod>,
    /// The emitted typed data-layer client (`api/client.ts`).
    pub data_layer_tsx: String,
    /// The dotted spec refs this generator produced — `uiStates.<id>`,
    /// `navigation.<id>`, `operations.<name>` (the `enumerate_nodes` convention).
    pub produced_refs: Vec<String>,
}

/// Invert a whole `FunctionalSpec` (+ `Profile` for endpoint derivation) into a
/// `GeneratedApp`. Deterministic; no LLM. Mirrors the per-screen instrumentation of
/// Phase 1 across every state, plus the navigation + data-layer wiring of Phase 2.
pub fn generate_app(spec: &FunctionalSpec, profile: &Profile) -> GeneratedApp {
    // 1. One instrumented screen per state (Phase-1 renderer, reused verbatim — the
    //    instrumentation bytes the snapshot depends on are produced HERE and never touched
    //    by the handler wiring below).
    let mut screens: Vec<ScreenArtifact> = spec.ui_states.iter().map(generate_screen).collect();

    // 2. One data-layer service method per operation (URL via the shared endpoint_for).
    let services: Vec<ServiceMethod> = spec
        .operations
        .iter()
        .map(|op| ServiceMethod {
            operation: op.name.clone(),
            endpoint: endpoint_for(op, profile),
        })
        .collect();

    // 3. One nav edge per transition, with its trigger element resolved against the
    //    SOURCE screen's instrumented elements + its operation invocation linked.
    let nav_edges: Vec<NavEdge> = spec
        .navigation
        .iter()
        .map(|t| invert_transition(t, &screens, &spec.operations))
        .collect();

    // 3b. Wire each resolved transition's navigation + (linked) data-layer call into its
    //     source screen's emitted `.tsx`, as a NON-INSTRUMENTATION annotation (a comment
    //     block documenting the handler the byte-stable Phase-1 placeholder stands in for).
    //     The instrumentation (hooks/host components) is left byte-identical so the
    //     snapshot — and therefore the matcher result — never drifts.
    for edge in &nav_edges {
        if let Some(trigger_id) = &edge.trigger_element_id {
            if let Some(screen) = screens.iter_mut().find(|s| s.state_id == edge.from_state) {
                screen
                    .tsx
                    .push_str(&render_handler_annotation(edge, trigger_id));
            }
        }
    }

    let data_layer_tsx = render_data_layer(&services);

    // 4. The dotted refs produced (the D4 verify contract; gated to covered downstream).
    let mut produced_refs =
        Vec::with_capacity(spec.ui_states.len() + spec.navigation.len() + spec.operations.len());
    for s in &spec.ui_states {
        produced_refs.push(format!("uiStates.{}", s.id));
    }
    for t in &spec.navigation {
        produced_refs.push(format!("navigation.{}", t.id));
    }
    for op in &spec.operations {
        produced_refs.push(format!("operations.{}", op.name));
    }

    GeneratedApp {
        screens,
        nav_edges,
        services,
        data_layer_tsx,
        produced_refs,
    }
}

/// Invert one transition into a [`NavEdge`], resolving the trigger element against the
/// source screen's instrumented elements (so the press handler is wired to a real,
/// snapshot-able button) and linking any operation the handler invokes.
fn invert_transition(
    t: &IrTransition,
    screens: &[ScreenArtifact],
    operations: &[Operation],
) -> NavEdge {
    let from_state = t.from_states.first().cloned().unwrap_or_default();
    let to_state = t.activate_states.first().cloned().unwrap_or_default();

    // Resolve the trigger element: find the source screen, then the instrumented element
    // whose role/text satisfies the first action's target criteria. This is the SAME
    // resolution the matcher performs for the transition's target — so a wired handler
    // implies the matcher would find the button (Phase-3 coverage gate prereq).
    let trigger_element_id = t
        .actions
        .first()
        .and_then(|action| resolve_trigger(&from_state, action, screens));

    // A click-action whose target text names an operation-bearing button is linked to the
    // operation whose handler the press should invoke. We link by the navigation effect:
    // a write/destructive transition that fires from a state invokes the page's operation.
    let invokes_operation = link_operation(t, operations);

    NavEdge {
        transition_id: t.id.clone(),
        from_state,
        to_state,
        trigger_element_id,
        invokes_operation,
    }
}

/// Find the instrumented element on the source screen that the transition's action
/// targets, returning its element id. The match mirrors the matcher's element search:
/// role must agree (when declared) and the target's text/textContains must be contained
/// in the element's visible text (case/space-insensitive).
fn resolve_trigger(
    from_state: &str,
    action: &IrTransitionAction,
    screens: &[ScreenArtifact],
) -> Option<String> {
    let screen = screens.iter().find(|s| s.state_id == from_state)?;
    let want_role = action.target.role.as_deref();
    let want_text = action
        .target
        .text
        .as_deref()
        .or(action.target.text_contains.as_deref())
        .map(norm);

    screen
        .elements
        .iter()
        .find(|el| {
            let role_ok = match want_role {
                Some(r) => el.accessibility_role.as_deref().map(norm) == Some(norm(r)),
                None => true,
            };
            let text_ok = match &want_text {
                Some(needle) => el
                    .visible_text
                    .as_deref()
                    .map(|t| norm(t).contains(needle))
                    .unwrap_or(false),
                None => true,
            };
            role_ok && text_ok
        })
        .map(|el| el.element_id.clone())
}

/// Link a transition to the operation its press handler invokes. v0 heuristic matching
/// the connect-runner shape: a write-effecting transition (or one whose only action is a
/// click) that activates a result state invokes the spec's single mutating operation. When
/// exactly one create/update/delete/custom operation exists, the transition is linked to
/// it; otherwise (ambiguous) no link is wired (surfaced, not guessed).
fn link_operation(t: &IrTransition, operations: &[Operation]) -> Option<String> {
    let is_click = t
        .actions
        .first()
        .map(|a| a.kind.eq_ignore_ascii_case("click"))
        .unwrap_or(false);
    if !is_click {
        return None;
    }
    let mutating: Vec<&Operation> = operations
        .iter()
        .filter(|op| !op.verb.eq_ignore_ascii_case("read"))
        .collect();
    match mutating.as_slice() {
        [single] => Some(single.name.clone()),
        _ => None,
    }
}

/// Render the deterministic typed data-layer client (`api/client.ts`): one method per
/// operation, each calling its derived endpoint through the `qontinui-mobile`
/// `HttpTransport` convention (base URL + bearer, per D3). Instrumentation-free (the data
/// layer isn't snapshot-matched); kept deterministic so the endpoint contract is stable.
fn render_data_layer(services: &[ServiceMethod]) -> String {
    let mut s = String::new();
    s.push_str("// AUTO-GENERATED by qontinui-app-generator (data layer, Phase 2).\n");
    s.push_str("// Endpoints derived via the shared qontinui_types::endpoint_for — the\n");
    s.push_str("// single #1<->#2 contract; do not hand-edit the method/path pairs.\n\n");
    s.push_str("import { HttpTransport } from './core/HttpTransport';\n\n");
    s.push_str("export class GeneratedApiClient {\n");
    s.push_str("  constructor(private readonly http: HttpTransport) {}\n\n");
    for m in services {
        let method_name = camel_case(&m.operation);
        let http_verb = m.endpoint.method.to_ascii_lowercase();
        s.push_str(&format!(
            "  /** {} -> {} {} */\n",
            m.operation, m.endpoint.method, m.endpoint.path
        ));
        s.push_str(&format!("  {method_name}(body?: unknown) {{\n",));
        s.push_str(&format!(
            "    return this.http.{http_verb}({}, body);\n",
            ts_string(&m.endpoint.path)
        ));
        s.push_str("  }\n\n");
    }
    s.push_str("}\n");
    s
}

/// Render the transition-handler annotation appended to a source screen's `.tsx`.
///
/// Phase 1 emitted a byte-stable `onPress` placeholder on the press element so the SDK's
/// dead-button backstop is satisfied and the instrumentation stays deterministic. Phase 2
/// records — as a trailing comment block keyed to the trigger element id — the concrete
/// navigation + data-layer call that placeholder stands in for, so the emitted source
/// documents the wired behavior WITHOUT mutating any instrumentation bytes the snapshot
/// depends on. (The executable handler body is filled by the Phase-2 LLM presentational
/// pass / a later codegen step; the deterministic core pins the contract.)
fn render_handler_annotation(edge: &NavEdge, trigger_id: &str) -> String {
    let mut s = String::new();
    s.push_str("\n// --- transition wiring (Phase 2, deterministic contract) ---\n");
    s.push_str(&format!(
        "// element {trigger_id}: onPress -> router.push('/{}')",
        edge.to_state
    ));
    if let Some(op) = &edge.invokes_operation {
        s.push_str(&format!(" after api.{}()", camel_case(op)));
    }
    s.push('\n');
    s.push_str(&format!("// transition: {}\n", edge.transition_id));
    s
}

/// Normalize text the way the matcher does (case-fold + whitespace-collapse) for the
/// generator's local trigger resolution. Mirrors `spec_check`'s `normalize_text` intent
/// without taking a dep on it (kept local + deterministic).
fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// `pair-confirm`/`pairConfirm` -> `pairConfirm` (camelCase method name).
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    let mut first = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if first {
                out.push(ch.to_ascii_lowercase());
                first = false;
            } else if upper_next {
                out.push(ch.to_ascii_uppercase());
                upper_next = false;
            } else {
                out.push(ch);
            }
        } else {
            upper_next = !first;
        }
    }
    if out.is_empty() {
        "call".to_string()
    } else {
        out
    }
}

/// Render a Rust string as a single-quoted TS string literal.
fn ts_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::ir::{IrElementCriteria, IrState};

    fn profile() -> Profile {
        serde_json::from_str(
            r#"{"profileVersion":"0","backend":{"language":"python","framework":"fastapi","datastore":"postgres","apiStyle":"rest"},"architecture":{"pattern":"layered","testingBar":"unit","minCoveragePct":80},"conventions":{"naming":"snake_case","entityToTable":"pluralize"}}"#,
        )
        .unwrap()
    }

    fn op(name: &str, verb: &str, entity: Option<&str>) -> Operation {
        serde_json::from_value(serde_json::json!({
            "name": name, "verb": verb, "entity": entity, "confidence": "observed"
        }))
        .unwrap()
    }

    fn click_transition(id: &str, from: &str, to: &str, role: &str, text: &str) -> IrTransition {
        IrTransition {
            id: id.into(),
            name: id.into(),
            from_states: vec![from.into()],
            activate_states: vec![to.into()],
            actions: vec![IrTransitionAction {
                kind: "click".into(),
                target: IrElementCriteria {
                    role: Some(role.into()),
                    text_contains: Some(text.into()),
                    ..Default::default()
                },
                params: None,
                wait_after: None,
            }],
            ..Default::default()
        }
    }

    fn state_with_button(id: &str) -> IrState {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id,
            "assertions": [{
                "id": format!("{id}-connect-button"),
                "description": "Connect",
                "category": "element-presence",
                "severity": "critical",
                "assertionType": "exists",
                "target": {"type":"search","criteria":{"role":"button","text":"Connect"},"label":"Connect"},
                "source":"discovery","reviewed":false,"enabled":true
            }]
        }))
        .unwrap()
    }

    #[test]
    fn transition_trigger_resolves_to_a_real_source_screen_element() {
        let from = state_with_button("pairing-confirm");
        let to = state_with_button("pairing-error");
        let spec = FunctionalSpec {
            spec_version: "0".into(),
            target: serde_json::from_value(serde_json::json!({"sourceUrl":"x"})).unwrap(),
            entities: vec![],
            operations: vec![op("pairConfirm", "create", Some("Device"))],
            ui_states: vec![from, to],
            navigation: vec![click_transition(
                "show-pairing-error",
                "pairing-confirm",
                "pairing-error",
                "button",
                "Connect",
            )],
            auth: None,
            assumptions: vec![],
        };
        let app = generate_app(&spec, &profile());
        assert_eq!(app.screens.len(), 2);
        assert_eq!(app.nav_edges.len(), 1);
        let edge = &app.nav_edges[0];
        assert_eq!(edge.from_state, "pairing-confirm");
        assert_eq!(edge.to_state, "pairing-error");
        // The handler wires to a REAL instrumented button on the source screen.
        assert!(
            edge.trigger_element_id.is_some(),
            "transition must wire to a real source-screen element, not nothing"
        );
        // The single mutating operation is linked to the click transition.
        assert_eq!(edge.invokes_operation.as_deref(), Some("pairConfirm"));
    }

    #[test]
    fn data_layer_derives_endpoint_via_shared_endpoint_for() {
        let spec = FunctionalSpec {
            spec_version: "0".into(),
            target: serde_json::from_value(serde_json::json!({"sourceUrl":"x"})).unwrap(),
            entities: vec![],
            operations: vec![op("pairConfirm", "create", Some("Device"))],
            ui_states: vec![],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        };
        let app = generate_app(&spec, &profile());
        assert_eq!(app.services.len(), 1);
        let m = &app.services[0];
        // The shared rule: custom op pairConfirm on Device -> POST /api/v1/devices/pair-confirm.
        assert_eq!(m.endpoint.method, "POST");
        assert_eq!(m.endpoint.path, "/api/v1/devices/pair-confirm");
        assert!(app
            .data_layer_tsx
            .contains("'/api/v1/devices/pair-confirm'"));
        assert!(app.data_layer_tsx.contains("pairConfirm(body?: unknown)"));
    }

    #[test]
    fn produced_refs_use_the_dotted_enumerate_convention() {
        let spec = FunctionalSpec {
            spec_version: "0".into(),
            target: serde_json::from_value(serde_json::json!({"sourceUrl":"x"})).unwrap(),
            entities: vec![],
            operations: vec![op("pairConfirm", "create", Some("Device"))],
            ui_states: vec![state_with_button("pairing-confirm")],
            navigation: vec![click_transition(
                "show-pairing-error",
                "pairing-confirm",
                "pairing-error",
                "button",
                "Connect",
            )],
            auth: None,
            assumptions: vec![],
        };
        let app = generate_app(&spec, &profile());
        assert!(app
            .produced_refs
            .contains(&"uiStates.pairing-confirm".to_string()));
        assert!(app
            .produced_refs
            .contains(&"navigation.show-pairing-error".to_string()));
        assert!(app
            .produced_refs
            .contains(&"operations.pairConfirm".to_string()));
    }

    #[test]
    fn dangling_transition_surfaces_as_unwired_not_silently_dropped() {
        // A transition whose target button does NOT exist on the source screen yields a
        // None trigger — surfaced, not silently wired to a nonexistent element.
        let from = state_with_button("pairing-confirm"); // has a Connect button
        let spec = FunctionalSpec {
            spec_version: "0".into(),
            target: serde_json::from_value(serde_json::json!({"sourceUrl":"x"})).unwrap(),
            entities: vec![],
            operations: vec![],
            ui_states: vec![from],
            navigation: vec![click_transition(
                "go-nowhere",
                "pairing-confirm",
                "elsewhere",
                "button",
                "Submit", // no "Submit" button on the source screen
            )],
            auth: None,
            assumptions: vec![],
        };
        let app = generate_app(&spec, &profile());
        assert_eq!(app.nav_edges[0].trigger_element_id, None);
    }

    #[test]
    fn camel_case_handles_kebab_and_camel() {
        assert_eq!(camel_case("pair-confirm"), "pairConfirm");
        assert_eq!(camel_case("pairConfirm"), "pairConfirm");
        assert_eq!(camel_case("create_invoice"), "createInvoice");
    }
}
