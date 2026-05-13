//! Criterion bench harness. Step 10d of Plan 02.
//!
//! Three benchmarks cover the crate's public hot paths:
//!
//! - `full_eval_100_elements` — small snapshot, 5 assertions. The
//!   compose-and-emit overhead floor.
//! - `full_eval_10000_elements` — large snapshot exercising the
//!   indexed-lookup + selectivity init at scale.
//! - `batch_eval_10_specs_x_1000_elements` — the §5.14 batch path:
//!   `IndexedSnapshot` is built once, then rayon fan-outs across specs.
//!
//! We intentionally do NOT add micro-benches for the internal
//! `narrow_candidates` / `compute_miss` entry points: exposing them
//! publicly just for benches would muddy the API surface, and the three
//! public-path benches above already hit those code paths via `evaluate()`
//! with criteria that succeed quickly (Phase 1) and criteria that miss
//! (Phase 2 is hit by the failure-driving spec in the batch bench).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use qontinui_spec_check::{evaluate, evaluate_batch, IrPageSpec, UIBridgeSnapshot};
use serde_json::json;

// ===========================================================================
// Fixture builders
// ===========================================================================

const ROLES: &[&str] = &["button", "link", "heading", "textbox", "checkbox", "img"];
const TAGS: &[&str] = &["button", "a", "h1", "input", "div", "img"];
const TEXTS: &[&str] = &[
    "Save",
    "Cancel",
    "Submit",
    "Edit profile",
    "Delete file",
    "Open settings",
];

fn make_snapshot(n: usize) -> UIBridgeSnapshot {
    let elements_json: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            let role = ROLES[i % ROLES.len()];
            let tag = TAGS[i % TAGS.len()];
            let text = TEXTS[i % TEXTS.len()];
            json!({
                "id": format!("e{i}"),
                "type": "div",
                "actions": [],
                "identifier": {
                    "xpath": format!("/html/body/div[{}]", i + 1),
                    "selector": format!("#e{i}"),
                },
                "state": {
                    "visible": true,
                    "enabled": true,
                    "focused": false,
                    "rect": {
                        "x": 0.0, "y": 0.0,
                        "width": 100.0, "height": 32.0,
                        "top": 0.0, "right": 100.0,
                        "bottom": 32.0, "left": 0.0,
                    },
                },
                "registeredAt": 1_700_000_000_000i64,
                "mounted": true,
                "role": role,
                "tagName": tag,
                "accessibleName": text,
                "text": text,
            })
        })
        .collect();

    let snap_json = json!({
        "timestamp": 1_700_000_000_000i64,
        "elements": elements_json,
        "components": [],
    });
    serde_json::from_value(snap_json).expect("fixture snapshot must parse")
}

/// Build a spec with `k` assertions, all targeting `(role, text)` pairs that
/// exist somewhere in `snap`. Used to drive the all-pass Phase 1 path.
fn make_spec_against(snap: &UIBridgeSnapshot, k: usize) -> IrPageSpec {
    let assertions_json: Vec<serde_json::Value> = (0..k)
        .map(|i| {
            let el = &snap.elements[i % snap.elements.len()];
            json!({
                "id": format!("a{i}"),
                "description": format!("assertion {i}"),
                "category": "element-presence",
                "severity": "error",
                "assertionType": "exists",
                "target": {
                    "type": "search",
                    "criteria": {
                        "role": el.role,
                        "text": el.text,
                    },
                    "label": format!("label-{i}"),
                },
                "source": "bench",
                "reviewed": true,
                "enabled": true,
            })
        })
        .collect();

    let spec_json = json!({
        "version": "1.0",
        "id": "bench-page",
        "name": "Bench page",
        "states": [
            {
                "id": "s1",
                "name": "Loaded",
                "isInitial": true,
                "assertions": assertions_json,
            }
        ],
        "transitions": [],
    });
    serde_json::from_value(spec_json).expect("fixture spec must parse")
}

/// Build a spec whose assertions deliberately miss — every criterion uses a
/// role/text pair that is NOT in the snapshot vocabulary. This drives Phase 2
/// (`compute_miss`) inside `evaluate()`.
fn make_failing_spec(k: usize) -> IrPageSpec {
    let assertions_json: Vec<serde_json::Value> = (0..k)
        .map(|i| {
            json!({
                "id": format!("a{i}"),
                "description": format!("assertion {i}"),
                "category": "element-presence",
                "severity": "error",
                "assertionType": "exists",
                "target": {
                    "type": "search",
                    "criteria": {
                        "role": "nonexistent-role",
                        "text": "nonexistent-text-string",
                    },
                    "label": format!("label-{i}"),
                },
                "source": "bench",
                "reviewed": true,
                "enabled": true,
            })
        })
        .collect();

    let spec_json = json!({
        "version": "1.0",
        "id": "bench-fail-page",
        "name": "Bench failing page",
        "states": [
            {
                "id": "s1",
                "name": "Loaded",
                "isInitial": true,
                "assertions": assertions_json,
            }
        ],
        "transitions": [],
    });
    serde_json::from_value(spec_json).expect("fixture spec must parse")
}

// ===========================================================================
// Benchmarks
// ===========================================================================

fn bench_full_eval_small(c: &mut Criterion) {
    let snap = make_snapshot(100);
    let spec = make_spec_against(&snap, 5);
    c.bench_function("full_eval_100_elements", |b| {
        b.iter(|| evaluate(black_box(&snap), black_box(&spec)))
    });
}

fn bench_full_eval_large(c: &mut Criterion) {
    let snap = make_snapshot(10_000);
    let spec = make_spec_against(&snap, 5);
    c.bench_function("full_eval_10000_elements", |b| {
        b.iter(|| evaluate(black_box(&snap), black_box(&spec)))
    });
}

fn bench_batch_eval(c: &mut Criterion) {
    let snap = make_snapshot(1_000);
    // Mix passing and failing specs to drive both Phase-1 and Phase-2 paths.
    let mut specs: Vec<IrPageSpec> = Vec::with_capacity(10);
    for _ in 0..7 {
        specs.push(make_spec_against(&snap, 5));
    }
    for _ in 0..3 {
        specs.push(make_failing_spec(5));
    }
    c.bench_function("batch_eval_10_specs_x_1000_elements", |b| {
        b.iter(|| evaluate_batch(black_box(&snap), black_box(&specs)))
    });
}

criterion_group!(
    benches,
    bench_full_eval_small,
    bench_full_eval_large,
    bench_batch_eval
);
criterion_main!(benches);
