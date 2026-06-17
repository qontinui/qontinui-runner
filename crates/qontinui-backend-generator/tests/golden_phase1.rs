//! Phase 1 golden test — the deterministic spec→backend seam.
//!
//! Proves, WITHOUT running anything:
//!   (1) the emitted `pairConfirm` route's method+path is byte-equal to the shared
//!       `endpoint_for(pairConfirm, profile)` (== `POST /api/v1/devices/pair-confirm`)
//!       — this is the #1↔#2 agreement proof (both sides derive routes from the same
//!       function), and
//!   (2) the emitted `Device` model has one column per spec `Device` field.
//!
//! These two assertions are the falsifiable core of Phase 1's deterministic tier.
//! Runtime verification (compose postgres + uvicorn, hit the live endpoint) is a
//! separate, deferred integration tier — see `tests/runtime_integration.rs`.

use std::path::PathBuf;

use qontinui_backend_generator::generate_phase1;
use qontinui_types::endpoint_for::endpoint_for;
use qontinui_types::functional_spec::FunctionalSpec;
use qontinui_types::priorities_profile::Profile;

/// The connect-runner fixtures live in the sibling schemas crate (read-only).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../qontinui-schemas/rust/tests/fixtures/functional_spec")
}

fn load_spec() -> FunctionalSpec {
    let p = fixtures_dir().join("connect-runner.functional_spec.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read spec fixture {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("parse connect-runner functional_spec")
}

fn load_profile() -> Profile {
    let p = fixtures_dir().join("connect-runner.profile.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("read profile fixture {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("parse connect-runner profile")
}

#[test]
fn pair_confirm_route_method_and_path_equal_shared_endpoint_for() {
    let spec = load_spec();
    let profile = load_profile();

    let pair_confirm = spec
        .operations
        .iter()
        .find(|o| o.name == "pairConfirm")
        .expect("fixture has a pairConfirm operation");

    // The canonical derivation — the one #1 (app-gen) also calls.
    let canonical = endpoint_for(pair_confirm, &profile);

    let backend = generate_phase1(&spec, &profile);
    let route = backend
        .route("pairConfirm")
        .expect("generator emitted a pairConfirm route");

    // (1) The emitted route is byte-equal to the shared endpoint_for result.
    assert_eq!(
        route.method, canonical.method,
        "emitted method must equal endpoint_for"
    );
    assert_eq!(
        route.path, canonical.path,
        "emitted path must equal endpoint_for"
    );

    // And both equal the observed reality the fixture provenance records.
    assert_eq!(route.method, "POST");
    assert_eq!(route.path, "/api/v1/devices/pair-confirm");

    // The emitted FastAPI source actually carries that method+path in its decorator.
    assert!(
        route
            .source
            .contains("@router.post(\"/api/v1/devices/pair-confirm\""),
        "emitted route source must wire POST /api/v1/devices/pair-confirm; got:\n{}",
        route.source
    );

    // The OpenAPI aid (Fork C) lists the same path under the same lowercased method.
    let path_obj = &backend.openapi["paths"]["/api/v1/devices/pair-confirm"];
    assert!(
        path_obj.get("post").is_some(),
        "openapi aid must list POST /api/v1/devices/pair-confirm; got:\n{}",
        serde_json::to_string_pretty(&backend.openapi).unwrap()
    );
}

#[test]
fn device_model_has_a_column_per_spec_field() {
    let spec = load_spec();
    let profile = load_profile();

    let device = spec
        .entities
        .iter()
        .find(|e| e.name == "Device")
        .expect("fixture has a Device entity");
    let expected_cols: Vec<String> = device.fields.iter().map(|f| f.name.clone()).collect();
    // Sanity: the fixture's four observed/inferred fields.
    assert_eq!(
        expected_cols,
        vec![
            "deviceName".to_string(),
            "deviceId".to_string(),
            "state".to_string(),
            "callback".to_string()
        ]
    );

    let backend = generate_phase1(&spec, &profile);
    let model = backend
        .model("Device")
        .expect("generator emitted a Device model");

    // Table name from the profile conventions (pluralize + snake_case).
    assert_eq!(model.table, "devices");

    // Every spec field is present as a snake_case column (plus the synthetic id PK).
    for field in &device.fields {
        let col = snake_local(&field.name);
        assert!(
            model.columns.contains(&col),
            "model must have a column for spec field `{}` (expected `{}`); columns = {:?}",
            field.name,
            col,
            model.columns
        );
        // The emitted SQLAlchemy source carries the mapped_column too.
        assert!(
            model.source.contains(&format!("{col}: Mapped[")),
            "model source must declare `{col}` as a Mapped column;\nsource:\n{}",
            model.source
        );
    }

    // One column per field + one synthetic id PK.
    assert_eq!(
        model.columns.len(),
        device.fields.len() + 1,
        "exactly one column per field plus the id PK; columns = {:?}",
        model.columns
    );
    assert_eq!(model.columns.first().map(String::as_str), Some("id"));
}

#[test]
fn dropping_a_field_drops_its_column_deterministic_gap_proof() {
    // Deterministic analogue of the plan's "broken variant surfaces a gap": at the
    // generation tier, removing a spec field must remove exactly its column — proving
    // the emitted schema reflects the spec, not a hardcoded shape. (The *runtime*
    // gap-from-live-server proof is the deferred integration tier.)
    let mut spec = load_spec();
    let profile = load_profile();

    let full = generate_phase1(&spec, &profile);
    let full_cols = full.model("Device").unwrap().columns.clone();
    assert!(full_cols.contains(&"callback".to_string()));

    // Drop the `callback` field.
    let device = spec
        .entities
        .iter_mut()
        .find(|e| e.name == "Device")
        .unwrap();
    device.fields.retain(|f| f.name != "callback");

    let dropped = generate_phase1(&spec, &profile);
    let dropped_cols = dropped.model("Device").unwrap().columns.clone();

    assert!(
        !dropped_cols.contains(&"callback".to_string()),
        "dropping the spec field must drop its column"
    );
    assert_eq!(
        dropped_cols.len(),
        full_cols.len() - 1,
        "exactly the dropped field's column disappears"
    );
}

/// Local copy of the generator's snake_case rule, kept in the test so the assertion
/// is independent of the crate's private helper.
fn snake_local(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
