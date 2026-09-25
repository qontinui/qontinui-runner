//! Totality corpus for the typed step dispatch.
//!
//! `execute_single_step` parses every `ExecutionStepConfig` into
//! `FullRunnerStep` via [`to_full_runner_step`]; a registry-served type that
//! fails that parse still reaches its handler by the raw step type, but with a
//! `warn!` (Phase 3), so the parse must be total for every live shape for that
//! warning to mean a genuine schema/handler disagreement. This corpus feeds every step
//! shape a LIVE producer emits through the same path the executor takes —
//! `parse_step_value` (canonical-key normalization, then
//! `serde_json::from_value::<ExecutionStepConfig>`) → `to_full_runner_step` →
//! [`handler_lookup_key`] — and asserts:
//!
//! - every step whose type the handler registry serves parses `Ok`, and
//!   `handler_lookup_key` sends it to the handler registered under that type;
//! - every registry-served type appears in the corpus at least once, so a new
//!   registered handler cannot land without a shape here;
//! - the legacy string-dispatched types (`shell_command`, `check`,
//!   `check_group`, `shell`, `log_watch`, `gate`) are NOT typed and parse
//!   `Err` — they are served by the legacy `match` after the registry lookup;
//! - the shapes whose refusal matches the handler's own verdict stay refused
//!   (`KNOWN_REFUSALS`, each with the reason).
//!
//! Sources, in order: `examples/workflows/*.json`; the AI generator's
//! meta-workflow and its canonical step examples (`meta_workflow.rs`); the
//! fixer / follow-up / reflection / meta-optimizer step builders; one
//! Builder-shaped step per `AddStepDropdown` entry (the built-in skill
//! catalog plus the Wrapper Action button) and the runner's
//! `buildSpecWorkflow.ts` step shapes; and the vet probe's shapes.
//!
//! Plan `2026-09-24-typed-step-dispatch-covers-every-registered-handler`,
//! Phase 2 — this test is what keeps Phase 3's fallback warning a signal
//! rather than noise.

use super::{
    handler_lookup_key, resolve_dispatch, to_full_runner_step, DispatchRoute,
    LEGACY_STRING_DISPATCH,
};
use crate::step_executor::handlers::HandlerRegistry;
use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::step_conversion::{
    convert_json_steps_with_phase, parse_step_value,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// One corpus entry: where it came from, and the step as JSON.
struct Shape {
    source: String,
    step: Value,
}

fn shape(source: impl Into<String>, step: Value) -> Shape {
    Shape {
        source: source.into(),
        step,
    }
}

/// Serialize a producer's `ExecutionStepConfig` exactly as the executor
/// receives it (task runs persist and reload `execution_steps_json`).
fn esc_shape(source: impl Into<String>, step: &ExecutionStepConfig) -> Shape {
    shape(
        source,
        serde_json::to_value(step).expect("serialize ExecutionStepConfig"),
    )
}

fn phase_arrays(source: &str, wf: &Value, out: &mut Vec<Shape>) {
    for key in [
        "setup_steps",
        "verification_steps",
        "agentic_steps",
        "completion_steps",
    ] {
        if let Some(steps) = wf.get(key).and_then(Value::as_array) {
            for (i, s) in steps.iter().enumerate() {
                out.push(shape(format!("{source} {key}[{i}]"), s.clone()));
            }
        }
    }
}

/// `examples/workflows/*.json` — every file, so a new fixture joins the
/// corpus without editing this test.
fn example_workflows(out: &mut Vec<Shape>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/workflows");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no example workflows under {}",
        dir.display()
    );
    for f in files {
        let wf: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap())
            .unwrap_or_else(|e| panic!("parse {}: {e}", f.display()));
        let name = format!("examples/{}", f.file_name().unwrap().to_string_lossy());
        phase_arrays(&name, &wf, out);
    }
}

/// The AI generator: the meta-workflow it runs, and the canonical step
/// examples it teaches the builder model to emit.
fn generator(out: &mut Vec<Shape>) {
    use crate::workflow_generation::meta_workflow::{
        build_meta_workflow_template, build_spec_brief_recognition_prompt,
    };
    use crate::workflow_generation::GenerateWorkflowRequest;

    for simple in [false, true] {
        let request = GenerateWorkflowRequest {
            description: "Add a findings panel to the runs page".to_string(),
            simple_mode: Some(simple),
            ..Default::default()
        };
        let wf = build_meta_workflow_template(&request, "", None, None);
        for (key, steps) in [
            ("setup_steps", &wf.setup_steps),
            ("verification_steps", &wf.verification_steps),
            ("agentic_steps", &wf.agentic_steps),
            ("completion_steps", &wf.completion_steps),
        ] {
            for (i, s) in steps.iter().enumerate() {
                out.push(shape(
                    format!("meta_workflow(simple={simple}) {key}[{i}]"),
                    s.clone(),
                ));
            }
        }
    }

    // Every ```json fence in the spec-brief prompt is a canonical step the
    // builder model is told to copy.
    let prompt = build_spec_brief_recognition_prompt(9876, "");
    let mut fences = 0;
    for block in prompt.split("```json").skip(1) {
        let body = block.split("```").next().unwrap();
        let step: Value = serde_json::from_str(body)
            .unwrap_or_else(|e| panic!("canonical example is not JSON ({e}):\n{body}"));
        fences += 1;
        out.push(shape(
            format!("meta_workflow canonical example #{fences}"),
            step,
        ));
    }
    assert!(
        fences >= 5,
        "expected the canonical step examples, found {fences}"
    );
}

/// The fixer / follow-up / reflection / meta-optimizer builders. Their
/// `build_setup_steps` need a live `AppState` only for the base URL, so the
/// step-shape helpers they are made of are called directly.
fn builders(out: &mut Vec<Shape>) {
    let url = "http://localhost:9876/task-runs/x/output";
    let api = [
        (
            "fixer",
            crate::fixer::workflow::build_api_step("Load", "GET", url, None, Some("v")),
        ),
        (
            "follow_up",
            crate::follow_up::workflow::build_api_step("Load", "GET", url, None, Some("v")),
        ),
        (
            "meta_optimizer::architecture",
            crate::meta_optimizer::architecture_optimizer::build_api_step(
                "Load",
                "POST",
                url,
                Some("{}"),
                None,
            ),
        ),
        (
            "meta_optimizer::meta_prompt",
            crate::meta_optimizer::meta_prompt_optimizer::build_api_step(
                "Load",
                "GET",
                url,
                None,
                Some("v"),
            ),
        ),
        (
            "meta_optimizer::generation_template",
            crate::meta_optimizer::generation_template_optimizer::build_api_step(
                "Load", "GET", url, None, None,
            ),
        ),
        (
            "meta_optimizer::pipeline_prompt",
            crate::meta_optimizer::pipeline_prompt_optimizer::build_api_step(
                "Load",
                "GET",
                url,
                None,
                Some("v"),
            ),
        ),
        (
            "reflection (setup)",
            crate::reflection::workflow::build_api_step("Load", "GET", url, None, Some("v"), true),
        ),
        (
            "reflection (completion)",
            crate::reflection::workflow::build_api_step(
                "Save",
                "POST",
                url,
                Some("{}"),
                None,
                false,
            ),
        ),
        (
            "reflection prompt",
            crate::reflection::workflow::build_prompt_step("Analyze", "Analyze the run."),
        ),
        (
            "reflection verification prompt",
            crate::reflection::workflow::build_verification_prompt_step("Verify", "Verify it."),
        ),
    ];
    for (source, step) in &api {
        out.push(esc_shape(format!("builder {source}"), step));
    }
    let verification = [
        ("fixer", crate::fixer::workflow::build_verification_steps()),
        (
            "follow_up",
            crate::follow_up::workflow::build_verification_steps(),
        ),
        (
            "meta_optimizer::architecture",
            crate::meta_optimizer::architecture_optimizer::build_verification_steps(),
        ),
        (
            "meta_optimizer::meta_prompt",
            crate::meta_optimizer::meta_prompt_optimizer::build_verification_steps(),
        ),
        (
            "meta_optimizer::generation_template",
            crate::meta_optimizer::generation_template_optimizer::build_verification_steps(),
        ),
        (
            "meta_optimizer::pipeline_prompt",
            crate::meta_optimizer::pipeline_prompt_optimizer::build_verification_steps(),
        ),
    ];
    for (source, steps) in &verification {
        for (i, step) in steps.iter().enumerate() {
            out.push(esc_shape(
                format!("builder {source} verification[{i}]"),
                step,
            ));
        }
    }
    // The completion sweep (`loop_controller.rs`): an id-less agentic prompt.
    out.push(esc_shape(
        "loop_controller completion sweep",
        &ExecutionStepConfig {
            name: Some("Completion Sweep 1".into()),
            step_type: "prompt".into(),
            phase: Some("agentic".into()),
            prompt_content: Some("sweep".into()),
            ..Default::default()
        },
    ));
}

/// What `instantiateSkill` (qontinui-workflow-utils
/// `src/skills/skill-instantiation.ts`) produces: `{id, name, phase,
/// skill_origin, ...template}` with every `{{param}}` resolved and an
/// unresolved exact-match param dropped.
fn builder_step(skill: &str, phase: &str, template: Value) -> Shape {
    let mut step = json!({
        "id": format!("{skill}-id"),
        "name": skill,
        "phase": phase,
        "skill_origin": {"skill_id": format!("builtin:{skill}"), "skill_slug": skill, "parameter_values": {}}
    });
    for (k, v) in template.as_object().unwrap() {
        step[k.as_str()] = v.clone();
    }
    shape(format!("AddStepDropdown {skill}"), step)
}

/// One step per `AddStepDropdown` entry: the built-in skills
/// (qontinui-workflow-utils `src/skills/builtin-skills.ts`) plus the Wrapper
/// Action button (`AddStepDropdown.tsx`). The five ui_bridge skills are
/// [`BUILDER_UI_BRIDGE_TEMPLATES`], shared with
/// `builder_ui_bridge_steps_carry_action_and_url`, which checks their fields
/// arrive.
///
/// HAND-COPIED SNAPSHOT of those TypeScript producers, not live-loaded: a
/// change to the templates does not reach this test. Only
/// `examples/workflows/*.json`, `meta_workflow.rs` and the Rust step builders
/// are loaded live.
fn add_step_dropdown(out: &mut Vec<Shape>) {
    let skills = [
        (
            "shell-command",
            "setup",
            json!({"type": "command", "mode": "shell", "command": "cargo build", "working_directory": ".", "fail_on_error": true}),
        ),
        (
            "lint-project",
            "verification",
            json!({"type": "command", "mode": "check", "check_type": "lint", "working_directory": "."}),
        ),
        (
            "format-check",
            "verification",
            json!({"type": "command", "mode": "check", "check_type": "format"}),
        ),
        (
            "type-check",
            "verification",
            json!({"type": "command", "mode": "check", "check_type": "typecheck", "working_directory": "."}),
        ),
        (
            "run-check-group",
            "setup",
            json!({"type": "command", "mode": "check_group", "check_group_id": "cg-1"}),
        ),
        (
            "security-scan",
            "verification",
            json!({"type": "command", "mode": "check", "check_type": "security"}),
        ),
        (
            "run-tests",
            "verification",
            json!({"type": "command", "mode": "test", "test_type": "custom_command", "command": "pytest", "working_directory": "."}),
        ),
        (
            "playwright-test",
            "verification",
            json!({"type": "command", "mode": "test", "test_type": "playwright", "code": "await page.goto('/')"}),
        ),
        (
            "ci-cd-status",
            "verification",
            json!({"type": "command", "mode": "check", "check_type": "ci_cd", "repository": "qontinui/qontinui-runner", "workflow_name": "ci.yml", "branch": "main"}),
        ),
        (
            "api-health-check",
            "setup",
            json!({"type": "command", "mode": "check", "check_type": "http_status", "check_url": "http://localhost:8000/health", "expected_status": 200, "timeout_seconds": 30}),
        ),
        (
            "ai-task",
            "agentic",
            json!({"type": "prompt", "content": "Fix the failing test."}),
        ),
        (
            "ai-verification",
            "verification",
            json!({"type": "prompt", "content": "Is the page correct?"}),
        ),
        (
            "run-sub-workflow",
            "setup",
            json!({"type": "workflow", "workflow_id": "wf-1", "workflow_name": ""}),
        ),
        (
            "state-exploration",
            "setup",
            json!({"type": "command", "mode": "shell", "command": "explore"}),
        ),
    ];
    for (skill, phase, template) in skills {
        out.push(builder_step(skill, phase, template));
    }
    for (skill, phase, template) in BUILDER_UI_BRIDGE_TEMPLATES {
        out.push(builder_step(
            skill,
            phase,
            serde_json::from_str(template).unwrap(),
        ));
    }
    // The Wrapper Action button, as constructed, and after the pickers are
    // filled in.
    out.push(shape(
        "AddStepDropdown wrapper_action (new)",
        json!({"id": "w1", "type": "wrapper_action", "name": "Wrapper Action", "phase": "setup",
               "wrapperId": "", "actionId": "", "params": {}, "resultVariable": ""}),
    ));
    out.push(shape(
        "AddStepDropdown wrapper_action (configured)",
        json!({"id": "w2", "type": "wrapper_action", "name": "Wrapper Action", "phase": "verification",
               "wrapperId": "notepad", "actionId": "open_file", "params": {"path": "{{ file }}"},
               "resultVariable": "opened"}),
    ));
}

/// `src/lib/workflow-builder/buildSpecWorkflow.ts` step shapes.
///
/// HAND-COPIED SNAPSHOT of that TypeScript producer, not live-loaded.
fn build_spec_workflow(out: &mut Vec<Shape>) {
    let ui = |action: &str, target: &str| {
        json!({"id": format!("sw-{action}"), "type": "ui_bridge", "phase": "setup",
               "name": format!("group: {action}"), "ui_bridge_action": action,
               "ui_bridge_target": target, "ui_bridge_snapshot_target": "sdk"})
    };
    out.push(shape("buildSpecWorkflow navigate", {
        let mut s = ui("navigate", "");
        s["ui_bridge_url"] = json!("http://localhost:3001/runs");
        s
    }));
    out.push(shape(
        "buildSpecWorkflow element_action",
        ui("element_action", r#"{"elementId":"btn","action":"click"}"#),
    ));
    out.push(shape(
        "buildSpecWorkflow wait_for_element",
        ui(
            "wait_for_element",
            r#"{"criteria":{"role":"main"},"timeout":10000}"#,
        ),
    ));
    out.push(shape("buildSpecWorkflow wait", ui("wait", "500")));
    out.push(shape("buildSpecWorkflow snapshot_assert", {
        let mut s = ui(
            "snapshot_assert",
            r#"[{"id":"a","assertionType":"exists","criteria":{"role":"button"}}]"#,
        );
        s["phase"] = json!("verification");
        s
    }));
    out.push(shape(
        "buildSpecWorkflow prompt",
        json!({"id": "sw-p", "type": "prompt", "phase": "verification", "name": "semantic",
               "content": "Check it.", "response_mode": true}),
    ));
}

/// The vet probe's shapes: every one ran only through the string fallback
/// before qontinui-types 3.1.
///
/// HAND-COPIED SNAPSHOT of the plan vet's scratch probe, not live-loaded.
fn probe(out: &mut Vec<Shape>) {
    let cases = [
        (
            "probe command id-less",
            json!({"type": "command", "name": "b", "phase": "setup", "command": "ls"}),
        ),
        (
            "probe command name-less",
            json!({"type": "command", "id": "a", "phase": "setup", "command": "ls"}),
        ),
        (
            "probe command phase-less",
            json!({"type": "command", "id": "a", "name": "b", "command": "ls"}),
        ),
        (
            "probe command bare",
            json!({"type": "command", "command": "ls"}),
        ),
        (
            "probe prompt phase-less",
            json!({"type": "prompt", "id": "a", "name": "b", "content": "x"}),
        ),
        (
            "probe prompt id-less",
            json!({"type": "prompt", "name": "b", "phase": "agentic", "content": "x"}),
        ),
        (
            "probe code_execution name-less",
            json!({"type": "code_execution", "id": "a", "code": "print(1)"}),
        ),
        (
            "probe vga_automate",
            json!({"type": "vga_automate", "id": "a", "name": "b", "phase": "verification",
                   "stateMachineId": "sm", "targetProcess": "np.exe",
                   "actionSequence": [{"kind": "click", "elementId": "e1"}]}),
        ),
        (
            "probe vga_automate runner names",
            json!({"type": "vga_automate", "id": "a", "name": "b",
                   "vga_state_machine_id": "sm", "vga_target_process": "np.exe"}),
        ),
        (
            "probe ui_bridge click",
            json!({"type": "ui_bridge", "id": "a", "name": "b", "ui_bridge_action": "click", "ui_bridge_target": r#"{"role":"button"}"#}),
        ),
        (
            "probe ui_bridge wait_for_element",
            json!({"type": "ui_bridge", "id": "a", "name": "b", "ui_bridge_action": "wait_for_element"}),
        ),
        (
            "probe ui_bridge element_action",
            json!({"type": "ui_bridge", "id": "a", "name": "b", "ui_bridge_action": "element_action"}),
        ),
        (
            "probe spec_check",
            json!({"type": "spec_check", "id": "a", "name": "b", "spec_check_app_id": "qontinui-web", "spec_check_page_id": "runs"}),
        ),
        (
            // No producer emits effect_check; hand-written JSON is how it is
            // reached (executor_types.rs field names).
            "probe effect_check",
            json!({"type": "effect_check", "id": "a", "name": "b", "effect_check_element_id": "btn",
                   "effect_check_action": "click", "effect_check_expected_outcome": "Confirmed"}),
        ),
        (
            "probe execute_playbook",
            json!({"type": "execute_playbook", "id": "a", "name": "b", "content": "# playbook"}),
        ),
        (
            "probe native_accessibility",
            json!({"type": "native_accessibility", "id": "a", "name": "b", "action": "capture"}),
        ),
        (
            "probe restart_process",
            json!({"type": "restart_process", "id": "a", "name": "b", "restart_process_name": "backend"}),
        ),
        (
            "probe ui_bridge_design_audit",
            json!({"type": "ui_bridge_design_audit", "id": "a", "name": "b"}),
        ),
        (
            "probe ui_bridge_visual_assertion",
            json!({"type": "ui_bridge_visual_assertion", "id": "a", "name": "b", "visual_assertion_type": "text"}),
        ),
        (
            "probe workflow_ref",
            json!({"type": "workflow_ref", "id": "a", "name": "b", "workflow_id": "w"}),
        ),
        (
            "probe dag_cancel",
            json!({"type": "dag_cancel", "id": "a", "name": "b"}),
        ),
        (
            "probe dag_approval",
            json!({"type": "dag_approval", "id": "a", "name": "b"}),
        ),
        (
            "probe dag_loop",
            json!({"type": "dag_loop", "id": "a", "name": "b"}),
        ),
    ];
    for (source, step) in cases {
        out.push(shape(source, step));
    }
}

/// The five ui_bridge skill templates in qontinui-workflow-utils
/// `builtin-skills.ts` (HAND-COPIED SNAPSHOT, checked against `origin/master`
/// on 2026-09-26). They write the canonical bare keys (`action`, `url`,
/// `target`, `assert_type`, ...), which `normalize_step_value` maps onto the
/// `ui_bridge_*` fields. `(skill, phase, template JSON)`.
const BUILDER_UI_BRIDGE_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "navigate-to-url",
        "setup",
        r#"{"type": "ui_bridge", "action": "navigate", "url": "http://localhost:3001"}"#,
    ),
    (
        "assert-element",
        "verification",
        r##"{"type": "ui_bridge", "action": "assert", "target": "#save", "assert_type": "exists", "expected": ""}"##,
    ),
    (
        "take-snapshot",
        "completion",
        r#"{"type": "ui_bridge", "action": "snapshot"}"#,
    ),
    (
        "ui-execute",
        "setup",
        r##"{"type": "ui_bridge", "action": "execute", "instruction": "click save", "target": "#save"}"##,
    ),
    (
        "ui-compare",
        "verification",
        r#"{"type": "ui_bridge", "action": "compare", "comparison_mode": "structural", "reference_snapshot_id": "snap-1"}"#,
    ),
];

/// Shapes a producer can emit that the typed parse refuses ON PURPOSE: the
/// handler fails each of them too, so the fallback reaches the same verdict.
/// `(source, step, why)`.
fn known_refusals() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "probe command agentic",
            json!({"type": "command", "id": "a", "name": "b", "phase": "agentic", "command": "ls"}),
            "no producer puts a command step in the agentic phase; only prompt steps are agentic",
        ),
        (
            "probe workflow_fixup unknown mode",
            json!({"type": "workflow_fixup", "id": "a", "name": "b", "fixupMode": "zzz"}),
            "WorkflowFixupHandler fails an unknown mode (\"Unknown fixup mode\")",
        ),
    ]
}

fn corpus() -> Vec<Shape> {
    let mut out = Vec::new();
    example_workflows(&mut out);
    generator(&mut out);
    builders(&mut out);
    add_step_dropdown(&mut out);
    build_spec_workflow(&mut out);
    probe(&mut out);
    out
}

fn step_type(v: &Value) -> String {
    v.get("type")
        .or_else(|| v.get("step_type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn every_live_producer_shape_parses_to_its_registered_handler() {
    let registry = HandlerRegistry::with_standard_handlers();
    let served: BTreeSet<&str> = registry.step_types().into_iter().collect();

    let mut covered = BTreeSet::new();
    let mut failures = Vec::new();
    let mut legacy_seen = Vec::new();

    let corpus = corpus();
    let total = corpus.len();
    for Shape { source, step } in corpus {
        let ty = step_type(&step);
        // "test" is normalised to "command" before the typed parse.
        let ty = if ty == "test" {
            "command".to_string()
        } else {
            ty
        };
        let esc: ExecutionStepConfig = match parse_step_value(&step) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{source}: ExecutionStepConfig refused it: {e}"));
                continue;
            }
        };
        if LEGACY_STRING_DISPATCH.contains(&ty.as_str()) {
            legacy_seen.push(format!("{source} ({ty})"));
            continue;
        }
        assert!(
            served.contains(ty.as_str()),
            "{source}: type {ty:?} is neither registry-served nor legacy — add it to one"
        );
        match to_full_runner_step(&esc) {
            Ok(typed) => {
                let key = handler_lookup_key(&typed);
                if key != ty {
                    failures.push(format!("{source}: type {ty:?} dispatched to {key:?}"));
                }
                covered.insert(ty);
            }
            Err(e) => failures.push(format!("{source}: {e}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} live producer shape(s) fail the typed parse:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    let uncovered: Vec<_> = served.iter().filter(|t| !covered.contains(**t)).collect();
    assert!(
        uncovered.is_empty(),
        "registry-served types with no corpus shape: {uncovered:?}"
    );
    eprintln!(
        "typed-dispatch corpus: {total} shapes, {} registry types covered",
        covered.len()
    );
    // Informational: which corpus shapes ride the legacy path.
    eprintln!("legacy string-dispatched shapes in the corpus: {legacy_seen:?}");
}

#[test]
fn legacy_string_types_are_not_typed() {
    for ty in LEGACY_STRING_DISPATCH {
        let esc: ExecutionStepConfig =
            serde_json::from_value(json!({"type": ty, "id": "a", "name": "b"})).unwrap();
        assert!(
            to_full_runner_step(&esc).is_err(),
            "{ty:?} parsed as a FullRunnerStep; it is served by the legacy match"
        );
        assert!(
            HandlerRegistry::with_standard_handlers().get(ty).is_none(),
            "{ty:?} is registered; it should not also be in LEGACY_STRING_DISPATCH"
        );
    }
}

#[test]
fn known_refusals_stay_refused() {
    for (source, step, why) in known_refusals() {
        let esc = parse_step_value(&step).unwrap();
        assert!(
            to_full_runner_step(&esc).is_err(),
            "{source}: expected a typed-parse refusal ({why})"
        );
    }
}

/// Which types still need a direct constructor in `to_full_runner_step`:
/// `ui_bridge` and `workflow` do (their typed fields are bare `action` /
/// `target` / `workflowId`, while `ExecutionStepConfig` serializes
/// `ui_bridge_*` / `ref_workflow_id`); the three new variants do not. Proven by
/// the plain JSON round-trip `to_full_runner_step` uses for every other type.
#[test]
fn which_types_need_a_direct_constructor() {
    use qontinui_types::workflow_step::FullRunnerStep;
    let round_trip = |v: Value| -> Result<FullRunnerStep, String> {
        let esc: ExecutionStepConfig = serde_json::from_value(v).unwrap();
        serde_json::from_value(serde_json::to_value(&esc).unwrap()).map_err(|e| e.to_string())
    };
    // No direct constructor needed: the plain round-trip already parses and
    // keeps every field.
    let FullRunnerStep::SpecCheck(s) = round_trip(json!({"type": "spec_check", "id": "a", "name": "b",
        "spec_check_app_id": "qontinui-web", "spec_check_page_id": "runs", "spec_check_fail_on": ["timeout"]}))
    .unwrap() else {
        panic!("spec_check")
    };
    assert_eq!(s.spec_check_app_id.as_deref(), Some("qontinui-web"));
    assert_eq!(s.spec_check_fail_on, Some(vec!["timeout".to_string()]));

    let FullRunnerStep::WrapperAction(w) =
        round_trip(json!({"type": "wrapper_action", "id": "a", "name": "b",
        "wrapperId": "notepad", "actionId": "open", "params": {"p": 1}, "resultVariable": "r"}))
        .unwrap()
    else {
        panic!("wrapper_action")
    };
    assert_eq!(w.wrapper_id.as_deref(), Some("notepad"));
    assert_eq!(w.action_id.as_deref(), Some("open"));
    assert_eq!(
        w.params,
        Some(json!({"p": 1})),
        "Builder params must survive"
    );
    assert_eq!(w.result_variable.as_deref(), Some("r"));

    let FullRunnerStep::EffectCheck(e) =
        round_trip(json!({"type": "effect_check", "id": "a", "name": "b",
        "effect_check_element_id": "btn", "effect_check_action": "click"}))
        .unwrap()
    else {
        panic!("effect_check")
    };
    assert_eq!(e.effect_check_element_id.as_deref(), Some("btn"));

    // Still needed: the plain round-trip fails both — `UiBridgeStep.action`
    // is a required bare `action` while the fat struct serializes
    // `ui_bridge_action`, and `WorkflowStep` wants `workflowId` /
    // `workflowName` while the fat struct serializes `ref_workflow_id`. That is
    // why those two keep their direct constructors (and the constructors are
    // what the corpus above exercises).
    assert!(round_trip(
        json!({"type": "ui_bridge", "id": "a", "name": "b", "ui_bridge_action": "click"})
    )
    .is_err());
    assert!(
        round_trip(json!({"type": "workflow", "id": "a", "name": "b", "workflow_id": "w"}))
            .is_err()
    );
}

#[test]
fn builder_wrapper_params_reach_the_handler_field() {
    // `params` is what the Builder writes; the handler reads
    // `ExecutionStepConfig::wrapper_params`.
    let esc: ExecutionStepConfig = serde_json::from_value(json!({
        "type": "wrapper_action", "wrapperId": "w", "actionId": "a", "params": {"path": "x"}
    }))
    .unwrap();
    assert_eq!(esc.wrapper_params, Some(json!({"path": "x"})));
}

/// The step the executor receives for `step`, through the unified-workflow
/// conversion seam (`convert_json_steps_with_phase`), in `phase`.
fn converted(step: &Value, phase: &str) -> ExecutionStepConfig {
    let mut steps = convert_json_steps_with_phase(std::slice::from_ref(step), 0, Some(phase));
    assert_eq!(steps.len(), 1, "conversion dropped {step}");
    steps.remove(0)
}

/// Builder ui_bridge steps carry their configured action, URL, target and
/// the rest to the handler — flipped from the pinned defect
/// `builder_ui_bridge_steps_lose_action_and_url` (plan
/// `2026-09-25-builder-ui-bridge-steps-lose-action-and-url`, Phase 1).
#[test]
fn builder_ui_bridge_steps_carry_action_and_url() {
    use qontinui_types::workflow_step::{FullRunnerStep, UiBridgeAction};
    assert_eq!(BUILDER_UI_BRIDGE_TEMPLATES.len(), 5);
    let registry = HandlerRegistry::with_standard_handlers();
    for (skill, phase, template) in BUILDER_UI_BRIDGE_TEMPLATES {
        let template: Value = serde_json::from_str(template).unwrap();
        let Shape { step, .. } = builder_step(skill, phase, template.clone());
        let esc = converted(&step, phase);
        let written = |k: &str| template.get(k).and_then(Value::as_str).map(str::to_string);

        assert_eq!(esc.ui_bridge_action, written("action"), "{skill}: action");
        assert_eq!(esc.ui_bridge_url, written("url"), "{skill}: url");
        assert_eq!(esc.ui_bridge_target, written("target"), "{skill}: target");
        assert_eq!(
            esc.ui_bridge_instruction,
            written("instruction"),
            "{skill}: instruction"
        );
        assert_eq!(
            esc.ui_bridge_assert_type,
            written("assert_type"),
            "{skill}: assert_type"
        );
        assert_eq!(
            esc.ui_bridge_expected,
            written("expected"),
            "{skill}: expected"
        );
        assert_eq!(
            esc.ui_bridge_compare_mode,
            written("comparison_mode"),
            "{skill}: comparison_mode"
        );
        assert_eq!(
            esc.ui_bridge_reference_snapshot_id,
            written("reference_snapshot_id"),
            "{skill}: reference_snapshot_id"
        );
        // The bare keys no longer leak into the native_accessibility fields.
        assert_eq!(esc.a11y_action, None, "{skill}: a11y_action");
        assert_eq!(esc.a11y_target, None, "{skill}: a11y_target");

        let FullRunnerStep::UiBridge(u) = to_full_runner_step(&esc).unwrap() else {
            panic!("{skill}: expected UiBridge")
        };
        let action: UiBridgeAction = serde_json::from_value(template["action"].clone()).unwrap();
        assert_eq!(u.action, action, "{skill}: typed action");
        assert_eq!(u.url, written("url"), "{skill}: typed url");
        assert_eq!(u.target, written("target"), "{skill}: typed target");
        assert_eq!(
            resolve_dispatch(&esc, &registry),
            DispatchRoute::Registry("ui_bridge"),
            "{skill}: dispatch"
        );
    }
}

/// Editor-shaped ui_bridge steps (runner `UiBridgeConfig.tsx`, qontinui-web
/// `UiBridgeStepConfig.tsx`): bare `action` / `url` / `target`, camelCase
/// `assertType` / `comparisonMode` / `referenceSnapshotId` /
/// `severityThreshold` / `timeoutMs`. `timeoutMs` must reach
/// `ui_bridge_timeout_ms`, not the VGA field it also aliases.
#[test]
fn editor_ui_bridge_steps_carry_camel_case_fields() {
    let assert = converted(
        &json!({"id": "e1", "type": "ui_bridge", "name": "check save", "phase": "verification",
                "action": "assert", "target": "#save", "assertType": "text_equals",
                "expected": "Saved", "timeoutMs": 7000, "uiBridgeSnapshotTarget": "sdk"}),
        "verification",
    );
    assert_eq!(assert.ui_bridge_action.as_deref(), Some("assert"));
    assert_eq!(assert.ui_bridge_target.as_deref(), Some("#save"));
    assert_eq!(assert.ui_bridge_assert_type.as_deref(), Some("text_equals"));
    assert_eq!(assert.ui_bridge_expected.as_deref(), Some("Saved"));
    assert_eq!(assert.ui_bridge_timeout_ms, Some(7000));
    assert_eq!(assert.vga_timeout_ms, None, "timeoutMs leaked into VGA");
    assert_eq!(assert.ui_bridge_snapshot_target.as_deref(), Some("sdk"));
    assert_eq!(assert.a11y_action, None);

    let compare = converted(
        &json!({"id": "e2", "type": "ui_bridge", "name": "compare", "action": "compare",
                "comparisonMode": "visual", "referenceSnapshotId": "snap-9",
                "severityThreshold": "minor"}),
        "completion",
    );
    assert_eq!(compare.ui_bridge_action.as_deref(), Some("compare"));
    assert_eq!(compare.ui_bridge_compare_mode.as_deref(), Some("visual"));
    assert_eq!(
        compare.ui_bridge_reference_snapshot_id.as_deref(),
        Some("snap-9")
    );
    assert_eq!(
        compare.ui_bridge_severity_threshold.as_deref(),
        Some("minor")
    );
    assert_eq!(compare.phase.as_deref(), Some("completion"));

    let navigate = converted(
        &json!({"id": "e3", "type": "ui_bridge", "name": "go", "action": "navigate",
                "url": "http://localhost:3001/runs"}),
        "setup",
    );
    assert_eq!(navigate.ui_bridge_action.as_deref(), Some("navigate"));
    assert_eq!(
        navigate.ui_bridge_url.as_deref(),
        Some("http://localhost:3001/runs")
    );
}

/// A prefixed key already on the step wins over its canonical spelling, and
/// a serialized `ExecutionStepConfig` passes through unchanged.
#[test]
fn prefixed_ui_bridge_keys_win_and_normalization_is_idempotent() {
    let esc = parse_step_value(&json!({"type": "ui_bridge", "action": "snapshot",
        "ui_bridge_action": "navigate", "url": "http://a", "uiBridgeUrl": "http://b"}))
    .unwrap();
    assert_eq!(esc.ui_bridge_action.as_deref(), Some("navigate"));
    assert_eq!(esc.ui_bridge_url.as_deref(), Some("http://b"));
    assert_eq!(esc.a11y_action, None);

    let round = parse_step_value(&serde_json::to_value(&esc).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&round).unwrap(),
        serde_json::to_value(&esc).unwrap()
    );

    // Not a ui_bridge step: bare `action` keeps its native_accessibility meaning.
    let a11y =
        parse_step_value(&json!({"type": "native_accessibility", "action": "capture"})).unwrap();
    assert_eq!(a11y.a11y_action.as_deref(), Some("capture"));
    assert_eq!(a11y.ui_bridge_action, None);

    // A structured canonical target is carried as its JSON text.
    let esc = parse_step_value(&json!({"type": "ui_bridge", "action": "click",
        "target": {"role": "button"}}))
    .unwrap();
    assert_eq!(
        esc.ui_bridge_target.as_deref(),
        Some(r#"{"role":"button"}"#)
    );
}

/// A ui_bridge step that still fails the parse is refused by the conversion
/// seam rather than rebuilt without its action.
#[test]
fn unparseable_ui_bridge_step_is_refused_not_rebuilt() {
    let bad = json!({"type": "ui_bridge", "name": "bad", "action": "navigate",
                     "timeoutMs": "not-a-number"});
    assert!(parse_step_value(&bad).is_err());
    assert!(convert_json_steps_with_phase(std::slice::from_ref(&bad), 0, Some("setup")).is_empty());
    // Other types keep the command-field fallback.
    let cmd = json!({"type": "command", "name": "c", "command": "ls", "timeoutMs": "x"});
    let steps = convert_json_steps_with_phase(std::slice::from_ref(&cmd), 0, Some("setup"));
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].shell_command.as_deref(), Some("ls"));
}

/// An action-less ui_bridge step is a `snapshot` on every side: the typed
/// view, the schemas default, and the action the handler runs (plan
/// `2026-09-25-builder-ui-bridge-steps-lose-action-and-url`, Phase 2).
#[test]
fn action_less_ui_bridge_step_is_a_snapshot() {
    use crate::step_executor::handlers::ui_bridge::DEFAULT_UI_BRIDGE_ACTION;
    use qontinui_types::workflow_step::{FullRunnerStep, UiBridgeAction};
    let esc = converted(
        &json!({"id": "a", "type": "ui_bridge", "name": "look"}),
        "verification",
    );
    assert_eq!(esc.ui_bridge_action, None);
    let FullRunnerStep::UiBridge(u) = to_full_runner_step(&esc).unwrap() else {
        panic!("expected UiBridge")
    };
    assert_eq!(u.action, UiBridgeAction::Snapshot, "typed default");
    let handler_default: UiBridgeAction =
        serde_json::from_value(json!(DEFAULT_UI_BRIDGE_ACTION)).unwrap();
    assert_eq!(u.action, handler_default, "typed view vs handler default");
    assert_eq!(
        UiBridgeAction::default(),
        UiBridgeAction::Snapshot,
        "schemas #[default]"
    );
    assert_eq!(
        resolve_dispatch(&esc, &HandlerRegistry::with_standard_handlers()),
        DispatchRoute::Registry("ui_bridge")
    );
}

/// Field-level checks for every shape this change newly types or fixes:
/// `Ok` plus the lookup key (the corpus test) does not prove the fields
/// survived.
#[test]
fn newly_typed_shapes_keep_their_fields() {
    use qontinui_types::workflow_step::{
        CommandStepPhase, FullRunnerStep, PromptStepPhase, UiBridgeAction, VgaAction,
    };
    let typed = |v: Value| -> FullRunnerStep {
        let esc = parse_step_value(&v).unwrap();
        to_full_runner_step(&esc).unwrap_or_else(|e| panic!("{v}: {e}"))
    };

    // vga_automate: the runner's own field names, and the camelCase ones.
    for v in [
        json!({"type": "vga_automate", "id": "a", "name": "b",
               "vga_state_machine_id": "sm", "vga_target_process": "np.exe",
               "vga_action_sequence": [{"kind": "click", "elementId": "e1"}]}),
        json!({"type": "vga_automate", "id": "a", "name": "b", "phase": "verification",
               "stateMachineId": "sm", "targetProcess": "np.exe",
               "actionSequence": [{"kind": "click", "elementId": "e1"}]}),
    ] {
        let FullRunnerStep::VgaAutomate(g) = typed(v) else {
            panic!("vga_automate")
        };
        assert_eq!(g.state_machine_id, "sm");
        assert_eq!(g.target_process, "np.exe");
        assert_eq!(
            g.action_sequence,
            vec![VgaAction::Click {
                element_id: "e1".into(),
                timeout_ms: None
            }]
        );
    }

    // wrapper_action exactly as the Builder writes it (`params`).
    let FullRunnerStep::WrapperAction(w) = typed(json!({"id": "w2", "type": "wrapper_action",
        "name": "Wrapper Action", "phase": "verification", "wrapperId": "notepad",
        "actionId": "open_file", "params": {"path": "{{ file }}"}, "resultVariable": "opened"}))
    else {
        panic!("wrapper_action")
    };
    assert_eq!(w.wrapper_id.as_deref(), Some("notepad"));
    assert_eq!(w.action_id.as_deref(), Some("open_file"));
    assert_eq!(w.params, Some(json!({"path": "{{ file }}"})));
    assert_eq!(w.result_variable.as_deref(), Some("opened"));

    // spec_check: every field.
    let FullRunnerStep::SpecCheck(c) = typed(json!({"type": "spec_check", "id": "a", "name": "b",
        "phase": "verification", "spec_check_app_id": "qontinui-web", "spec_check_page_id": "runs",
        "spec_check_policy": {"minMatchRate": 0.9}, "spec_check_fail_when_no_app": true,
        "spec_check_fail_when_no_spec": false, "spec_check_fail_on": ["timeout", "network"]}))
    else {
        panic!("spec_check")
    };
    assert_eq!(c.spec_check_app_id.as_deref(), Some("qontinui-web"));
    assert_eq!(c.spec_check_page_id.as_deref(), Some("runs"));
    assert_eq!(c.spec_check_policy, Some(json!({"minMatchRate": 0.9})));
    assert_eq!(c.spec_check_fail_when_no_app, Some(true));
    assert_eq!(c.spec_check_fail_when_no_spec, Some(false));
    assert_eq!(
        c.spec_check_fail_on,
        Some(vec!["timeout".to_string(), "network".to_string()])
    );

    // effect_check: its four fields.
    let FullRunnerStep::EffectCheck(e) =
        typed(json!({"type": "effect_check", "id": "a", "name": "b",
        "effect_check_element_id": "btn", "effect_check_action": "type",
        "effect_check_params": {"value": "hi"}, "effect_check_expected_outcome": "Confirmed"}))
    else {
        panic!("effect_check")
    };
    assert_eq!(e.effect_check_element_id.as_deref(), Some("btn"));
    assert_eq!(e.effect_check_action.as_deref(), Some("type"));
    assert_eq!(e.effect_check_params, Some(json!({"value": "hi"})));
    assert_eq!(
        e.effect_check_expected_outcome.as_deref(),
        Some("Confirmed")
    );

    // ui_bridge actions carried in `ui_bridge_action`.
    for (wire, expected) in [
        ("click", UiBridgeAction::Click),
        ("wait_for_element", UiBridgeAction::WaitForElement),
        ("element_action", UiBridgeAction::ElementAction),
        ("wait", UiBridgeAction::Wait),
    ] {
        let FullRunnerStep::UiBridge(u) =
            typed(json!({"type": "ui_bridge", "id": "a", "name": "b",
            "phase": "setup", "ui_bridge_action": wire, "ui_bridge_target": "500"}))
        else {
            panic!("ui_bridge {wire}")
        };
        assert_eq!(u.action, expected, "ui_bridge_action {wire}");
        assert_eq!(u.target.as_deref(), Some("500"));
    }

    // id-less / name-less / phase-less.
    let FullRunnerStep::Command(c) =
        typed(json!({"type": "command", "name": "b", "phase": "verification", "command": "ls"}))
    else {
        panic!("command id-less")
    };
    assert_eq!(c.base.id, "");
    assert_eq!(c.base.name, "b");
    assert_eq!(c.phase, CommandStepPhase::Verification);
    let FullRunnerStep::Command(c) = typed(json!({"type": "command", "command": "ls"})) else {
        panic!("command bare")
    };
    assert_eq!((c.base.id.as_str(), c.base.name.as_str()), ("", ""));
    assert_eq!(c.phase, CommandStepPhase::Setup);
    let FullRunnerStep::Prompt(p) =
        typed(json!({"type": "prompt", "id": "a", "name": "b", "content": "x"}))
    else {
        panic!("prompt phase-less")
    };
    assert_eq!(p.phase, PromptStepPhase::Setup);
    assert_eq!(p.content, "x");
    // The fixer's id-less builder step, exactly as built.
    let esc = crate::fixer::workflow::build_api_step("Load", "GET", "http://x", None, Some("v"));
    assert_eq!(esc.id, None);
    let FullRunnerStep::Command(c) = to_full_runner_step(&esc).unwrap() else {
        panic!("fixer step")
    };
    assert_eq!(c.base.id, "");
    assert_eq!(c.base.name, "Load");
    assert_eq!(c.phase, CommandStepPhase::Setup);
}
