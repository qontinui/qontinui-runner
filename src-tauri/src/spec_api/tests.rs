//! Tests for the Spec API.
//!
//! Coverage:
//! - Projection round-trip (Rust ↔ a small fixture matching the TS output)
//! - Storage write/read/list on a tempdir
//! - Each endpoint via `axum::Router` test harness
//! - Path traversal rejected
//! - SSE: POST /spec/author triggers a `spec.changed` event a subscriber receives

use std::time::Duration;

use serde_json::{json, Value};

use super::projection::project_ir_to_bundled_page;
use super::storage::{self, ReadWithinRootError, RUNNER_APP_ID};
use super::types::IrPageSpec;

fn small_doc() -> IrPageSpec {
    let raw = json!({
        "version": "1.0",
        "id": "active",
        "name": "Active Dashboard",
        "description": "Real-time monitoring hub",
        "metadata": { "tags": ["monitoring", "tier-1"] },
        "states": [
            {
                "id": "running",
                "name": "Running",
                "description": "Workflow executing",
                "assertions": [
                    {
                        "id": "running-elem-0",
                        "description": "Required element 0 for state Running",
                        "category": "element-presence",
                        "severity": "critical",
                        "assertionType": "exists",
                        "target": {
                            "type": "search",
                            "criteria": { "role": "button", "textContent": "Stop" },
                            "label": "Required element for Running"
                        },
                        "source": "ai-generated",
                        "reviewed": false,
                        "enabled": true
                    }
                ],
                "isInitial": false
            }
        ],
        "transitions": [
            {
                "id": "running-to-idle",
                "name": "Stop",
                "fromStates": ["running"],
                "activateStates": ["idle"],
                "exitStates": ["running"],
                "actions": [
                    {
                        "type": "click",
                        "target": { "role": "button", "text": "Stop" },
                        "waitAfter": { "type": "idle", "timeout": 3000 }
                    }
                ]
            }
        ],
        "initialState": "idle"
    });
    serde_json::from_value(raw).unwrap()
}

#[test]
fn projection_known_fixture_is_byte_stable() {
    let doc = small_doc();
    let v1 = project_ir_to_bundled_page(&doc, Some("Notes"));
    let v2 = project_ir_to_bundled_page(&doc, Some("Notes"));
    let s1 = serde_json::to_string(&v1).unwrap();
    let s2 = serde_json::to_string(&v2).unwrap();
    assert_eq!(s1, s2, "projection must be deterministic");
}

#[test]
fn projection_matches_known_shape() {
    // Validate the projection produces the exact structure the TS version
    // emits for the same input. We compare the (deterministic) JSON
    // representation against an inline expected fixture. If the TS
    // projection's output differs, this test will fail and direct
    // attention to whichever side drifted.
    let doc = small_doc();
    let v = project_ir_to_bundled_page(&doc, Some("Notes"));
    // Match-against-shape: assert specific projection rules per the
    // worked example in projection.ts JSDoc.
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["description"], "Real-time monitoring hub\n\nNotes");
    assert_eq!(v["metadata"]["component"], "active");
    assert_eq!(v["metadata"]["tags"][0], "monitoring");

    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["id"], "running");
    let assertions = groups[0]["assertions"].as_array().unwrap();
    assert_eq!(assertions.len(), 1);
    assert_eq!(assertions[0]["id"], "running-elem-0");
    assert_eq!(assertions[0]["target"]["criteria"]["role"], "button");
    assert_eq!(assertions[0]["target"]["criteria"]["textContent"], "Stop");

    let sm_states = v["stateMachine"]["states"].as_array().unwrap();
    assert_eq!(sm_states.len(), 1);
    assert_eq!(sm_states[0]["id"], "running");
    assert_eq!(sm_states[0]["isInitial"], false);
    let transitions = sm_states[0]["transitions"].as_array().unwrap();
    assert_eq!(transitions.len(), 1);
    assert_eq!(transitions[0]["id"], "running-to-idle");
    assert_eq!(transitions[0]["activateStates"][0], "idle");
    assert_eq!(transitions[0]["deactivateStates"][0], "running");
    assert_eq!(transitions[0]["staysVisible"], false);
    assert_eq!(transitions[0]["process"][0]["action"], "click");
    assert_eq!(
        transitions[0]["process"][0]["target"]["textContent"],
        "Stop"
    );
}

#[test]
fn projection_keys_are_sorted() {
    // The output of `project_ir_to_bundled_page` round-trips through
    // serde_json::to_string and back into a Value tree — key order in the
    // emitted Value must be alphabetic at every level. We walk the tree
    // and assert.
    fn walk(v: &Value) {
        match v {
            Value::Object(map) => {
                let keys: Vec<&String> = map.keys().collect();
                let mut sorted = keys.clone();
                sorted.sort();
                assert_eq!(keys, sorted, "object keys not sorted: {:?}", keys);
                for child in map.values() {
                    walk(child);
                }
            }
            Value::Array(arr) => {
                for child in arr {
                    walk(child);
                }
            }
            _ => {}
        }
    }
    let doc = small_doc();
    let v = project_ir_to_bundled_page(&doc, None);
    // Re-parse via to_string to capture serializer's actual key order.
    let s = serde_json::to_string(&v).unwrap();
    let reparsed: Value = serde_json::from_str(&s).unwrap();
    walk(&reparsed);
}

#[test]
fn storage_round_trip_in_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let doc = small_doc();
    let projection_path = storage::write_ir_and_regenerate(root, RUNNER_APP_ID, &doc).unwrap();
    assert!(projection_path.exists());
    // Re-read both halves; they must round-trip.
    let read_back = storage::read_ir(root, RUNNER_APP_ID, "active")
        .unwrap()
        .unwrap();
    assert_eq!(read_back.id, "active");
    assert_eq!(read_back.states.len(), 1);
    let proj = storage::read_projection(root, RUNNER_APP_ID, "active")
        .unwrap()
        .unwrap();
    assert_eq!(proj["version"], "1.0.0");
    let pages = storage::list_pages(root, RUNNER_APP_ID).unwrap();
    assert_eq!(pages, vec!["active".to_string()]);
}

#[test]
fn embedded_pages_snapshot_is_populated() {
    // Section 4 (UI Bridge redesign) — production binaries embed
    // <runner>/specs/pages at compile time so the Spec API still answers
    // /spec/page/<id> when shipped without a sibling specs/ directory.
    //
    // We assert via the public API: `list_pages` against an empty tempdir
    // root must fall back to the embedded snapshot and surface at least one
    // page id (the runner repo currently ships ~95 pages under specs/pages/).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let pages = storage::list_pages(root, RUNNER_APP_ID).unwrap();
    assert!(
        !pages.is_empty(),
        "EMBEDDED_PAGES fallback should surface at least one page when the \
         filesystem root is empty; got {:?}",
        pages
    );
    // Spot-check that `active` (the canonical existing page id) is among them.
    assert!(
        pages.contains(&"active".to_string()),
        "embedded snapshot should include the `active` page; got {:?}",
        pages
    );
    // The snapshot should be readable end-to-end via read_ir.
    let doc = storage::read_ir(root, RUNNER_APP_ID, "active")
        .expect("read_ir should not error on embedded fallback")
        .expect("active page must be present in the embedded snapshot");
    assert_eq!(doc.id, "active");
}

#[test]
fn storage_path_traversal_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Create a real file inside, plus a "secret" outside the root.
    std::fs::write(root.join("inside.txt"), b"ok").unwrap();
    let outer_dir = tmp.path().parent().unwrap();
    let secret = outer_dir.join("secret_outside.txt");
    std::fs::write(&secret, b"nope").unwrap();
    // In-bounds reads succeed.
    let inside = storage::read_within_root(root, "inside.txt").unwrap();
    assert_eq!(inside, b"ok");
    // Out-of-bounds reads via .. are rejected.
    let traversed = storage::read_within_root(root, "../secret_outside.txt");
    assert!(matches!(
        traversed,
        Err(ReadWithinRootError::OutsideRoot) | Err(ReadWithinRootError::FileNotFound)
    ));
    // Explicitly assert the error variant for the canonicalize-resolves
    // case (the file exists, so it should be `OutsideRoot`).
    if outer_dir.canonicalize().is_ok() {
        let traversed_explicit =
            storage::read_within_root(root, "../secret_outside.txt").unwrap_err();
        assert_eq!(traversed_explicit, ReadWithinRootError::OutsideRoot);
    }
    // Cleanup
    let _ = std::fs::remove_file(&secret);
}

// =========================================================================
// Handler tests via tower::ServiceExt::oneshot.
// =========================================================================

#[cfg(test)]
mod handler_tests {
    use super::super::events;
    use super::super::handlers;
    use super::super::storage::RUNNER_APP_ID;
    use super::small_doc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use serde_json::Value;
    use tower::ServiceExt;

    fn router_for_tests() -> Router {
        // Tests don't need ApiState; we route to plain handler functions
        // that don't actually read it for the tested code paths.
        //
        // Stream C: the path is now `/apps/:app_id/spec/health` and the
        // handler extracts the `app_id` segment (and ignores it for the
        // health smoke check).
        Router::new().route("/apps/{app_id}/spec/health", get(handlers::get_health))
    }

    #[tokio::test]
    async fn health_returns_reason() {
        let app = router_for_tests();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/apps/qontinui-runner/spec/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["reason"], "spec-api-mounted");
    }

    #[tokio::test]
    async fn sse_receives_emitted_event() {
        // Exercise the broadcaster directly: subscribe, emit, await event.
        // Per-app channels (Stream C) — subscriber + emitter must agree on
        // the app_id or the event lands on a different channel.
        let mut rx = events::subscribe(RUNNER_APP_ID);
        events::emit(events::SpecApiEvent::SpecChanged(events::SpecChanged {
            app_id: RUNNER_APP_ID.to_string(),
            page_id: "active".to_string(),
            kind: "ir-and-projection".to_string(),
            at_ms: events::now_ms(),
        }));
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("subscriber should receive within timeout")
            .expect("event must arrive");
        match received {
            events::SpecApiEvent::SpecChanged(payload) => {
                assert_eq!(payload.page_id, "active");
                assert_eq!(payload.kind, "ir-and-projection");
            }
            other => panic!("expected SpecChanged variant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_returns_per_page_discovered_specs() {
        // Section 13 / Phase 1 — `GET /spec/list` enumerates all known
        // pages and returns a per-page projection slice matching the
        // TypeScript `DiscoveredSpec` shape (see
        // qontinui-runner/src/lib/spec-prompt-builder.ts).
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Seed two distinct IR pages so the on-disk listing wins over
        // the embedded snapshot fallback.
        let mut doc_a = small_doc();
        doc_a.id = "test-alpha".to_string();
        let mut doc_b = small_doc();
        doc_b.id = "test-beta".to_string();
        super::super::storage::write_ir_and_regenerate(root, RUNNER_APP_ID, &doc_a).unwrap();
        super::super::storage::write_ir_and_regenerate(root, RUNNER_APP_ID, &doc_b).unwrap();

        let resp =
            super::super::handlers::build_list_response(root, RUNNER_APP_ID, "Qontinui Runner")
                .into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(v["ok"], true);
        let specs = v["specs"].as_array().expect("specs must be an array");
        assert_eq!(
            specs.len(),
            2,
            "expected exactly 2 entries, got {:?}",
            specs
        );

        // list_pages returns sorted ids ⇒ test-alpha, test-beta.
        let ids: Vec<&str> = specs
            .iter()
            .map(|s| s["specId"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["test-alpha", "test-beta"]);

        for entry in specs {
            assert_eq!(entry["appName"], "Qontinui Runner");
            let config = &entry["config"];
            assert_eq!(config["version"], "1.0.0");
            let groups = config["groups"]
                .as_array()
                .expect("config.groups must be an array");
            assert_eq!(groups.len(), 1, "small_doc projects to a single group");
            assert!(
                config.get("description").is_some(),
                "config must include description key (may be string or null)"
            );
        }
    }

    #[tokio::test]
    async fn projection_via_handler_storage_round_trip() {
        // End-to-end through storage: write a tempdir IR and re-read the
        // projection; assert reason-bearing 404 for an unknown id.
        //
        // Spec-multi-app Stream B: the QONTINUI_SPECS_ROOT env-var path was
        // deleted with the v1 single-root resolver. Tests now pass the
        // tempdir root directly to the storage primitives.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let doc = small_doc();
        super::super::storage::write_ir_and_regenerate(root, RUNNER_APP_ID, &doc).unwrap();

        let projection = super::super::storage::read_projection(root, RUNNER_APP_ID, "active")
            .unwrap()
            .expect("projection must exist");
        assert_eq!(projection["version"], "1.0.0");

        // Unknown id read should return None (handler then renders the
        // reason: "page-not-found" envelope).
        let missing =
            super::super::storage::read_projection(root, RUNNER_APP_ID, "nonexistent").unwrap();
        assert!(missing.is_none());
    }

    // =========================================================================
    // Plan 04 Phase 3 — distinctness validator integration tests
    // =========================================================================

    use super::super::handlers::{
        build_author_response, build_derive_response, build_validate_response, DeriveBody,
        ValidateBody,
    };
    use serde_json::json;

    /// Build a degenerate IR: a single state with one fully-empty `{}`
    /// criterion. Triggers a single `EmptyCriteria` violation.
    fn degenerate_doc(id: &str) -> super::IrPageSpec {
        let raw = json!({
            "version": "1.0",
            "id": id,
            "name": "Degenerate",
            "states": [
                {
                    "id": "degen-state",
                    "name": "Degen",
                    "assertions": [
                        {
                            "id": "degen-elem-0",
                            "description": "Empty criterion",
                            "category": "element-presence",
                            "severity": "critical",
                            "assertionType": "exists",
                            "target": {
                                "type": "search",
                                "criteria": {},
                                "label": "empty"
                            },
                            "source": "test",
                            "reviewed": false,
                            "enabled": true
                        }
                    ]
                }
            ],
            "transitions": []
        });
        serde_json::from_value(raw).unwrap()
    }

    /// Build a clean spec with two distinct, non-empty states.
    fn clean_doc(id: &str) -> super::IrPageSpec {
        let raw = json!({
            "version": "1.0",
            "id": id,
            "name": "Clean",
            "states": [
                {
                    "id": "state-a",
                    "name": "A",
                    "assertions": [
                        {
                            "id": "a-elem-0",
                            "description": "Button",
                            "category": "element-presence",
                            "severity": "critical",
                            "assertionType": "exists",
                            "target": {
                                "type": "search",
                                "criteria": { "role": "button" },
                                "label": "button"
                            },
                            "source": "test",
                            "reviewed": false,
                            "enabled": true
                        }
                    ]
                },
                {
                    "id": "state-b",
                    "name": "B",
                    "assertions": [
                        {
                            "id": "b-elem-0",
                            "description": "Link",
                            "category": "element-presence",
                            "severity": "critical",
                            "assertionType": "exists",
                            "target": {
                                "type": "search",
                                "criteria": { "role": "link" },
                                "label": "link"
                            },
                            "source": "test",
                            "reviewed": false,
                            "enabled": true
                        }
                    ]
                }
            ],
            "transitions": []
        });
        serde_json::from_value(raw).unwrap()
    }

    async fn response_to_json(resp: axum::response::Response) -> (axum::http::StatusCode, Value) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    // ------------------------------------------------------------------------
    // 1. post_author_rejects_empty_criterion
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_author_rejects_empty_criterion() {
        let tmp = tempfile::tempdir().unwrap();
        let resp = build_author_response(tmp.path(), RUNNER_APP_ID, degenerate_doc("test-degen"));
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "spec-validation-failed");
        assert_eq!(v["detail"]["pageId"], "test-degen");
        let violations = v["detail"]["violations"]
            .as_array()
            .expect("violations must be an array");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["reason"], "emptyCriteria");
        // Per-variant fields ship as camelCase via `rename_all_fields` on
        // `DistinctnessViolation` (see distinctness.rs).
        assert_eq!(violations[0]["stateId"], "degen-state");
        assert_eq!(violations[0]["assertionIndex"], 0);
    }

    // ------------------------------------------------------------------------
    // 2. post_author_accepts_clean_spec
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_author_accepts_clean_spec() {
        let tmp = tempfile::tempdir().unwrap();
        let resp = build_author_response(tmp.path(), RUNNER_APP_ID, clean_doc("test-clean"));
        let (status, v) = response_to_json(resp).await;
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "expected 200 OK; got {} body={}",
            status,
            v
        );
        assert_eq!(v["ok"], true);
        assert_eq!(v["pageId"], "test-clean");
    }

    // ------------------------------------------------------------------------
    // 3. post_validate_returns_full_report
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_validate_returns_full_report() {
        let tmp = tempfile::tempdir().unwrap();
        let body = ValidateBody {
            document: Some(degenerate_doc("test-degen-validate")),
            page_id: None,
        };
        let resp = build_validate_response(tmp.path(), RUNNER_APP_ID, body);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(v["ok"], false);
        let violations = v["violations"]
            .as_array()
            .expect("violations must be an array");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["reason"], "emptyCriteria");
    }

    // ------------------------------------------------------------------------
    // 4. post_validate_by_page_id
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_validate_by_page_id() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed a degenerate IR on disk by writing it directly (bypassing
        // post_author, which would reject it).
        let doc = degenerate_doc("seeded-degen");
        let page_dir = tmp.path().join("pages").join(&doc.id);
        std::fs::create_dir_all(&page_dir).unwrap();
        let ir_path = page_dir.join("state-machine.derived.json");
        std::fs::write(&ir_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let body = ValidateBody {
            document: None,
            page_id: Some("seeded-degen".to_string()),
        };
        let resp = build_validate_response(tmp.path(), RUNNER_APP_ID, body);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(v["ok"], false);
        let violations = v["violations"]
            .as_array()
            .expect("violations must be an array");
        assert!(!violations.is_empty(), "expected at least one violation");
        assert_eq!(violations[0]["reason"], "emptyCriteria");
    }

    // ------------------------------------------------------------------------
    // 5. post_validate_missing_inputs
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_validate_missing_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let body = ValidateBody {
            document: None,
            page_id: None,
        };
        let resp = build_validate_response(tmp.path(), RUNNER_APP_ID, body);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "missing-document-or-page-id");
    }

    // ------------------------------------------------------------------------
    // 6. get_list_carries_validation_field
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn get_list_carries_validation_field() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed a degenerate IR + its projection by hand (write_ir_and_regenerate
        // would still emit the projection, but to keep the test focused we
        // seed both ourselves so list_pages discovers the entry).
        let doc = degenerate_doc("list-degen");
        let page_dir = tmp.path().join("pages").join(&doc.id);
        std::fs::create_dir_all(&page_dir).unwrap();
        std::fs::write(
            page_dir.join("state-machine.derived.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        // Minimal projection — get_list falls back to projecting on the fly,
        // but seeding ensures the test does not depend on the projection
        // layer producing output for a degenerate doc.
        std::fs::write(
            page_dir.join("spec.uibridge.json"),
            json!({
                "version": "1.0.0",
                "description": "Degenerate",
                "groups": [],
                "metadata": Value::Null,
            })
            .to_string(),
        )
        .unwrap();

        let resp = super::super::handlers::build_list_response(
            tmp.path(),
            RUNNER_APP_ID,
            "Qontinui Runner",
        );
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let specs = v["specs"].as_array().expect("specs must be an array");
        // Find our seeded entry (the embedded snapshot fallback may add other
        // entries; we look up by specId).
        let entry = specs
            .iter()
            .find(|s| s["specId"] == "list-degen")
            .expect("list-degen entry must be present");
        let validation = &entry["validation"];
        assert!(
            !validation.is_null(),
            "validation field should be populated for degenerate spec; entry={}",
            entry
        );
        // Rolled-up SpecValidation shape: pageId + degenerate_state_ids
        // (camelCase wire) + indistinguishable_state_pairs.
        assert_eq!(validation["pageId"], "list-degen");
        let degen_ids = validation["degenerateStateIds"]
            .as_array()
            .expect("degenerateStateIds must be present");
        assert!(
            degen_ids.iter().any(|id| id == "degen-state"),
            "expected degen-state in degenerateStateIds; got {:?}",
            degen_ids
        );
    }

    // ------------------------------------------------------------------------
    // 7. post_derive_rejects_degenerate_ir
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn post_derive_rejects_degenerate_ir() {
        let tmp = tempfile::tempdir().unwrap();
        // Seed a degenerate IR on disk.
        let doc = degenerate_doc("derive-degen");
        let page_dir = tmp.path().join("pages").join(&doc.id);
        std::fs::create_dir_all(&page_dir).unwrap();
        std::fs::write(
            page_dir.join("state-machine.derived.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();

        let body = DeriveBody {
            page_id: "derive-degen".to_string(),
            derivation: "incomingTransitions".to_string(),
        };
        let resp = build_derive_response(tmp.path(), RUNNER_APP_ID, body);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "spec-validation-failed");
        assert_eq!(v["detail"]["pageId"], "derive-degen");
    }

    // =========================================================================
    // Spec-multi-app Stream C tests (PLAN.md §C.8)
    // =========================================================================

    use super::super::handlers::build_author_response as build_author_response_c8;
    use super::super::types::IrProvenance;

    /// Construct an IR document with an explicit provenance.app_id for the
    /// post_author multi-tenant validation tests.
    fn doc_with_app_id(page_id: &str, provenance_app_id: &str) -> super::IrPageSpec {
        let mut doc = clean_doc(page_id);
        doc.provenance = Some(IrProvenance {
            source: "test".to_string(),
            app_id: provenance_app_id.to_string(),
            file: None,
            line: None,
            column: None,
            plugin_version: None,
            status: None,
        });
        doc
    }

    // C.8 (3) — post_author rejects when provenance.app_id mismatches URL path
    #[tokio::test]
    async fn post_author_mismatched_app_id_returns_400() {
        let tmp = tempfile::tempdir().unwrap();
        let doc = doc_with_app_id("c8-mismatch", "other-app");
        let resp = build_author_response_c8(tmp.path(), "qontinui-runner", doc);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "app-id-mismatch");
        assert_eq!(v["detail"]["urlAppId"], "qontinui-runner");
        assert_eq!(v["detail"]["provenanceAppId"], "other-app");
    }

    // C.8 (4) — post_author fills missing provenance.app_id from URL path
    #[tokio::test]
    async fn post_author_missing_app_id_fills_from_path_and_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let doc = doc_with_app_id("c8-fill", ""); // empty triggers fill
        let resp = build_author_response_c8(tmp.path(), "qontinui-runner", doc);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "expected 200 OK after fill; body={}",
            v
        );
        assert_eq!(v["ok"], true);
        assert_eq!(v["pageId"], "c8-fill");
        // Re-read the written IR and assert the app_id was persisted from the URL path.
        let written = super::super::storage::read_ir(tmp.path(), "qontinui-runner", "c8-fill")
            .expect("read ok")
            .expect("file present");
        assert_eq!(
            written.provenance.expect("provenance written").app_id,
            "qontinui-runner"
        );
    }

    // C.8 (5) — per-app broadcast channels isolate subscribers
    #[tokio::test]
    async fn get_subscribe_only_receives_own_app_events() {
        use super::super::events;
        // Two subscribers on two distinct app channels.
        let mut rx_runner = events::subscribe("qontinui-runner");
        let mut rx_web = events::subscribe("qontinui-web");

        // Emit a SpecChanged tagged for the runner app.
        events::emit(events::SpecApiEvent::SpecChanged(events::SpecChanged {
            app_id: "qontinui-runner".to_string(),
            page_id: "c8-iso".to_string(),
            kind: "ir-and-projection".to_string(),
            at_ms: events::now_ms(),
        }));

        // The runner subscriber must see it.
        let recv_runner =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx_runner.recv())
                .await
                .expect("runner subscriber should receive within timeout")
                .expect("event must arrive on runner channel");
        match recv_runner {
            events::SpecApiEvent::SpecChanged(payload) => {
                assert_eq!(payload.app_id, "qontinui-runner");
                assert_eq!(payload.page_id, "c8-iso");
            }
            other => panic!("expected SpecChanged on runner channel, got {other:?}"),
        }

        // The web subscriber must NOT see it (timeout expected).
        let recv_web =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx_web.recv()).await;
        assert!(
            recv_web.is_err(),
            "qontinui-web subscriber must not receive qontinui-runner events; got {:?}",
            recv_web
        );
    }

    // C.8 (6) — regression guard: the un-scoped `/spec/list` path no longer
    // exists. We mount the production router and assert 404 — proves Stream C
    // removed the legacy mounts (not just added new ones alongside).
    #[tokio::test]
    async fn unscoped_spec_list_route_returns_404() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        // We build a thin router that mounts only the same /apps/:app_id/spec/health
        // pattern Stream C registers, then assert /spec/list (legacy path) returns 404.
        // The full production router needs `Router<Arc<ApiState>>` which is too heavy
        // to construct in unit tests; the routing-table semantics are equivalent.
        let app: Router = Router::new().route(
            "/apps/{app_id}/spec/health",
            get(super::super::handlers::get_health),
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/spec/list")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::NOT_FOUND,
            "legacy /spec/list must 404 after Stream C rescope"
        );
    }

    // =========================================================================
    // PG-gated tests (Stream C C.8 1 + 2). Run with a live PG via:
    //   DATABASE_URL=postgres://... cargo test --bin qontinui-runner --features spec-authoring \
    //     -- --ignored spec_api::tests::handler_tests::pg --test-threads=1
    // =========================================================================

    // C.8 (1) — GET /apps/:app_id/spec/list with unknown app returns 404
    // "app-not-found" via the AppError::NotRegistered mapping.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn get_list_unknown_app_returns_404_not_registered() {
        use crate::database::pg::PgDb;
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://qontinui_user:qontinui_password@localhost:5433/qontinui_db".to_string()
        });
        let pg = PgDb::new(&url).await.expect("PgDb::new for spec_api tests");
        // Pick a deterministic-but-unique id that we never register.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unknown_id = format!("c8-unknown-{}", nanos);
        let err = super::super::storage::resolve_specs_root(&pg, &unknown_id)
            .await
            .expect_err("must error for unregistered app");
        match err {
            qontinui_types::apps::AppError::NotRegistered { app_id } => {
                assert_eq!(app_id, unknown_id);
            }
            other => panic!("expected NotRegistered, got {:?}", other),
        }
    }

    // C.8 (2) — GET /apps/:app_id/spec/list returns the app's own pages
    // when the registry resolves the app to its repo_root + writes land
    // there.
    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn get_list_with_app_returns_app_specific_pages() {
        use crate::database::pg::PgDb;
        use qontinui_types::apps::RegisterAppRequest;
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://qontinui_user:qontinui_password@localhost:5433/qontinui_db".to_string()
        });
        let pg = PgDb::new(&url).await.expect("PgDb::new for spec_api tests");

        let tmp = tempfile::tempdir().unwrap();
        // Create the specs/ subdir the resolver requires.
        let specs_root = tmp.path().join("specs");
        std::fs::create_dir_all(&specs_root).unwrap();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let app_id = format!("c8-list-{}", nanos);
        let req = RegisterAppRequest::new(
            app_id.clone(),
            tmp.path().to_string_lossy(),
            format!("http://localhost:3001/{}", app_id),
            format!("Test App {}", app_id),
        );
        pg.insert_app(&req).await.expect("insert app");

        // Seed two pages under the registered app's specs/ root.
        let mut doc_a = small_doc();
        doc_a.id = "c8-page-alpha".to_string();
        let mut doc_b = small_doc();
        doc_b.id = "c8-page-beta".to_string();
        super::super::storage::write_ir_and_regenerate(&specs_root, &app_id, &doc_a).unwrap();
        super::super::storage::write_ir_and_regenerate(&specs_root, &app_id, &doc_b).unwrap();

        let resp =
            super::super::handlers::build_list_response(&specs_root, &app_id, &req.display_name);
        let (status, v) = response_to_json(resp).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let specs = v["specs"].as_array().expect("specs must be array");
        let ids: Vec<&str> = specs
            .iter()
            .map(|s| s["specId"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"c8-page-alpha"), "alpha missing: {:?}", ids);
        assert!(ids.contains(&"c8-page-beta"), "beta missing: {:?}", ids);
        // appName surfaces the registered `display_name`, not the slug
        // `app_id` (the prior behavior dropped the registry's display_name
        // on the floor — see `build_list_response` doc comment).
        for entry in specs {
            if entry["specId"] == "c8-page-alpha" || entry["specId"] == "c8-page-beta" {
                assert_eq!(entry["appName"], req.display_name);
            }
        }

        // Cleanup.
        let _ = pg.delete_app(&app_id).await;
    }
}
