//! AWAS manifest ingestion — the **high-confidence** comprehension path (plan
//! Phase 3, §3a/§3d).
//!
//! A cooperating site serves `/.well-known/ai-actions.json`; its declared action
//! set is *stated*, not inferred, so every node seeded from it is
//! [`EvidenceClass::AwasDeclared`] — the one class the clamp pins
//! (`EvidenceClass::is_pinned`) and the merge never downgrades.
//!
//! Like `input.rs`, [`AwasManifest`] is a *permissive mirror* of the wire shape
//! the Python reader (`qontinui/src/qontinui/awas/types.py`) declares: the
//! top-level keys are camelCase aliases (`schemaVersion`, `appName`, `baseUrl`,
//! …, with their snake_case spellings accepted too), while action / parameter /
//! auth keys are snake_case because the pydantic models declare no alias there.
//! `#[serde(default)]` everywhere, no `deny_unknown_fields`, so a fuller live
//! manifest still parses. The manifest is loaded from a file/fixture exactly as
//! the snapshot is — fetching it live is the same runtime-deferred capture leg,
//! and this crate carries no HTTP client by design.
//!
//! ## What the seed does NOT claim
//!
//! `OperationEffect` is `Assumed` **by construction, always** (§5). A manifest
//! *declaring* `side_effect: true` still does not show what persists server-side
//! — it sharpens the `assumption` text, never the provenance class. So the seed
//! emits every effect at `Assumed` and classes it `Silent`, exactly as the
//! degraded path does, and the clamp would force it regardless.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use qontinui_types::functional_spec::{
    AuthModel, AuthRole, Entity, EntityField, FunctionalSpec, Operation, OperationEffect,
    OperationInput, SpecProvenance, SpecTarget, ValidationRule,
};

use crate::clamp::EvidenceClass;

/// An AWAS manifest (`/.well-known/ai-actions.json`). Top-level keys are
/// camelCase on the wire (pydantic `alias=`); the snake_case spelling is
/// accepted as an alias so a `populate_by_name` producer also parses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AwasManifest {
    #[serde(default, rename = "schemaVersion", alias = "schema_version")]
    pub schema_version: String,
    #[serde(default, rename = "appName", alias = "app_name")]
    pub app_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "baseUrl", alias = "base_url")]
    pub base_url: String,
    #[serde(default)]
    pub actions: Vec<AwasAction>,
    #[serde(default)]
    pub auth: Option<AwasAuth>,
    #[serde(default, rename = "conformanceLevel", alias = "conformance_level")]
    pub conformance_level: Option<String>,
    #[serde(default, rename = "openapiUrl", alias = "openapi_url")]
    pub openapi_url: Option<String>,
    #[serde(default, rename = "mcpManifestUrl", alias = "mcp_manifest_url")]
    pub mcp_manifest_url: Option<String>,
}

/// One declared action — an AI-accessible endpoint. Keys are snake_case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AwasAction {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// HTTP method (`GET`/`POST`/`PUT`/`PATCH`/`DELETE`); any case accepted.
    #[serde(default)]
    pub method: String,
    /// Endpoint path relative to `base_url`.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub intent: String,
    /// Whether the action modifies data.
    #[serde(default)]
    pub side_effect: bool,
    #[serde(default)]
    pub parameters: Vec<AwasParameter>,
    /// JSON Schema for the request body (`properties` / `title` are read).
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
    /// JSON Schema for the response (`properties` / `title` are read).
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub required_scopes: Vec<String>,
    /// Requests per minute, when declared.
    #[serde(default)]
    pub rate_limit: Option<u64>,
}

/// One parameter of an [`AwasAction`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AwasParameter {
    #[serde(default)]
    pub name: String,
    /// `query` | `path` | `body` | `header`.
    #[serde(default)]
    pub location: String,
    /// Parameter type (`string`, `integer`, `boolean`, …); `string` when absent.
    #[serde(rename = "type", default = "default_param_type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Allowed values, when enumerated.
    #[serde(rename = "enum", default)]
    pub enum_values: Option<Vec<String>>,
}

fn default_param_type() -> String {
    "string".into()
}

/// The manifest's auth configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AwasAuth {
    /// `bearer_token` | `api_key` | `oauth2` | `basic` | `none`.
    #[serde(rename = "type", default)]
    pub auth_type: String,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub header_name: Option<String>,
    #[serde(default)]
    pub scopes: Vec<AwasScope>,
}

/// One declared scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AwasScope {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Seed a `FunctionalSpec` from a manifest, returning the seed **and** its
/// evidence-class map — every emitted provenance-bearing node keyed by its
/// `enumerate_nodes` dotted ref at [`EvidenceClass::AwasDeclared`] (the effect
/// node is [`EvidenceClass::Silent`], see the module docs), so the clamp's
/// `is_pinned` protects exactly the declared set.
///
/// Mapping (plan Phase 3.2, corrected against the frozen types):
/// - action → `Operation{name: id, verb: <from method>, entity: <schema title
///   or derived from the endpoint>, inputs: <from parameters>, effect: Assumed,
///   confidence: Observed, provenance: "awas:<endpoint> <METHOD> — <intent>"}`.
///   The observed endpoint rides in `provenance` — the `endpoint_for` override
///   path the oracle test pins.
/// - parameter → `OperationInput{field, required, validation: rule
///   "enum:a|b|c" | "type:<type>", provenance carrying the `location`}`.
///   Nothing dropped, nothing invented on the frozen struct.
/// - `input_schema` / `output_schema` `properties` → entity fields, entities
///   merged by name across actions (fields unioned by name, first wins).
/// - `auth` → `AuthModel{model: <type>, roles: <scopes>}`; a manifest with no
///   `auth` seeds NO auth node (left to the explorer path at `AuthShellInfer`).
///
/// `ui_states` / `navigation` / `assumptions` are left empty — the
/// deterministic substrate owns those. Pure: no clock, no I/O.
pub fn awas_to_spec_seed(
    manifest: &AwasManifest,
) -> (FunctionalSpec, BTreeMap<String, EvidenceClass>) {
    let mut classes = BTreeMap::new();
    let mut entities: Vec<Entity> = Vec::new();
    let mut operations: Vec<Operation> = Vec::new();

    for action in &manifest.actions {
        let method = action.method.trim().to_ascii_uppercase();
        let entity_name = entity_for_action(action);

        let inputs: Vec<OperationInput> = action
            .parameters
            .iter()
            .map(|p| parameter_to_input(action, p))
            .collect();

        classes.insert(
            format!("operations.{}", action.id),
            EvidenceClass::AwasDeclared,
        );
        for input in &inputs {
            if input.validation.is_some() {
                classes.insert(
                    format!("operations.{}.inputs.{}.validation", action.id, input.field),
                    EvidenceClass::AwasDeclared,
                );
            }
        }
        classes.insert(
            format!("operations.{}.effect", action.id),
            EvidenceClass::Silent,
        );

        operations.push(Operation {
            name: action.id.clone(),
            verb: verb_for_method(&method),
            entity: entity_name.clone(),
            inputs,
            effect: Some(OperationEffect {
                confidence: SpecProvenance::Assumed,
                assumption: Some(format!(
                    "AWAS declares side_effect={} for {} {}",
                    action.side_effect, method, action.endpoint
                )),
                provenance: Some(format!("awas:{}", action.id)),
                credibility: None,
            }),
            confidence: SpecProvenance::Observed,
            provenance: Some(format!(
                "awas:{} {} — {}",
                action.endpoint, method, action.intent
            )),
            credibility: None,
        });

        if let Some(entity_name) = &entity_name {
            for (schema, label) in [
                (&action.input_schema, "input_schema"),
                (&action.output_schema, "output_schema"),
            ] {
                let Some(schema) = schema else { continue };
                let fields = schema_fields(schema, &format!("awas:{}.{label}", action.id));
                if fields.is_empty() {
                    continue;
                }
                merge_seed_entity(
                    &mut entities,
                    &mut classes,
                    entity_name,
                    fields,
                    &format!("awas:{}", action.id),
                );
            }
        }
    }

    let auth = manifest.auth.as_ref().map(|a| {
        classes.insert("auth".into(), EvidenceClass::AwasDeclared);
        let roles = a
            .scopes
            .iter()
            .map(|s| {
                classes.insert(
                    format!("auth.roles.{}", s.name),
                    EvidenceClass::AwasDeclared,
                );
                AuthRole {
                    name: s.name.clone(),
                    confidence: SpecProvenance::Observed,
                    provenance: Some("awas:auth.scopes".into()),
                    credibility: None,
                }
            })
            .collect();
        AuthModel {
            model: if a.auth_type.trim().is_empty() {
                "none".into()
            } else {
                a.auth_type.trim().to_ascii_lowercase()
            },
            confidence: SpecProvenance::Observed,
            roles,
            provenance: Some("awas:auth".into()),
            credibility: None,
        }
    });

    let spec = FunctionalSpec {
        spec_version: "0".into(),
        target: SpecTarget {
            source_url: manifest.base_url.clone(),
            observed_at: None,
        },
        entities,
        operations,
        ui_states: vec![],
        navigation: vec![],
        auth,
        assumptions: vec![],
    };
    (spec, classes)
}

/// `GET`→`read`, `POST`→`create`, `PUT`/`PATCH`→`update`, `DELETE`→`delete`;
/// anything else is the lower-cased method (the `endpoint_for` rule treats an
/// unknown verb as `custom`→`POST`).
fn verb_for_method(method_upper: &str) -> String {
    match method_upper {
        "GET" => "read".into(),
        "POST" => "create".into(),
        "PUT" | "PATCH" => "update".into(),
        "DELETE" => "delete".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// The entity an action acts on: a schema `title` when either schema names one,
/// else derived from the endpoint's first non-`api`/non-version path segment
/// (naively singularised, capitalised). `None` only when nothing derives.
fn entity_for_action(action: &AwasAction) -> Option<String> {
    for schema in [&action.input_schema, &action.output_schema]
        .into_iter()
        .flatten()
    {
        if let Some(title) = schema.get("title").and_then(|t| t.as_str()) {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    entity_from_endpoint(&action.endpoint)
}

/// `/api/v1/runners/pair` → `Runner`.
fn entity_from_endpoint(endpoint: &str) -> Option<String> {
    let path = endpoint.split(['?', '#']).next().unwrap_or_default();
    let segment = path
        .split('/')
        .find(|s| !s.is_empty() && !s.eq_ignore_ascii_case("api") && !is_version_token(s))?;
    let singular = segment.strip_suffix('s').unwrap_or(segment);
    let mut chars = singular.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// `v1`, `V2`, `v10` … — a version segment, not an entity.
fn is_version_token(segment: &str) -> bool {
    let mut chars = segment.chars();
    matches!(chars.next(), Some('v') | Some('V'))
        && chars.clone().next().is_some()
        && chars.all(|c| c.is_ascii_digit())
}

fn parameter_to_input(action: &AwasAction, p: &AwasParameter) -> OperationInput {
    let param_type = if p.param_type.trim().is_empty() {
        "string"
    } else {
        p.param_type.trim()
    };
    let rule = match &p.enum_values {
        Some(values) if !values.is_empty() => format!("enum:{}", values.join("|")),
        _ => format!("type:{param_type}"),
    };
    let mut provenance = format!("awas:{}.parameters.{}", action.id, p.name);
    if !p.location.is_empty() {
        provenance.push_str(&format!(" location={}", p.location));
    }
    OperationInput {
        field: p.name.clone(),
        required: p.required,
        validation: Some(ValidationRule {
            rule,
            confidence: SpecProvenance::Observed,
            provenance: Some(provenance),
            credibility: None,
        }),
    }
}

/// Read a JSON Schema's `properties` into entity fields (each property's `type`,
/// or `string`; `enum` values when present). Key order is serde_json's object
/// order — sorted, hence deterministic.
fn schema_fields(schema: &serde_json::Value, provenance: &str) -> Vec<EntityField> {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        return vec![];
    };
    props
        .iter()
        .map(|(name, prop)| {
            let values: Vec<String> = prop
                .get("enum")
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let field_type = prop
                .get("type")
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "string".into());
            EntityField {
                name: name.clone(),
                field_type,
                values,
                confidence: SpecProvenance::Observed,
                provenance: Some(provenance.to_string()),
                credibility: None,
            }
        })
        .collect()
}

/// Merge seeded fields into the entity of that name (creating it on first
/// sight), unioning fields by name — first wins — and classing every node
/// `AwasDeclared`.
fn merge_seed_entity(
    entities: &mut Vec<Entity>,
    classes: &mut BTreeMap<String, EvidenceClass>,
    name: &str,
    fields: Vec<EntityField>,
    provenance: &str,
) {
    classes.insert(format!("entities.{name}"), EvidenceClass::AwasDeclared);
    let entity = match entities.iter_mut().find(|e| e.name == name) {
        Some(e) => e,
        None => {
            entities.push(Entity {
                name: name.to_string(),
                fields: vec![],
                relationships: vec![],
                confidence: SpecProvenance::Observed,
                provenance: Some(provenance.to_string()),
                credibility: None,
            });
            entities.last_mut().expect("just pushed")
        }
    };
    for f in fields {
        classes.insert(
            format!("entities.{name}.fields.{}", f.name),
            EvidenceClass::AwasDeclared,
        );
        if !entity.fields.iter().any(|x| x.name == f.name) {
            entity.fields.push(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_derives_from_endpoint_skipping_api_and_version_segments() {
        assert_eq!(
            entity_from_endpoint("/api/v1/runners/pair").as_deref(),
            Some("Runner")
        );
        assert_eq!(
            entity_from_endpoint("/api/runners/pair").as_deref(),
            Some("Runner")
        );
        assert_eq!(
            entity_from_endpoint("/devices?x=1").as_deref(),
            Some("Device")
        );
        assert_eq!(entity_from_endpoint("/api/v2").as_deref(), None);
        assert_eq!(entity_from_endpoint("").as_deref(), None);
    }

    #[test]
    fn schema_title_wins_over_endpoint_derivation() {
        let action = AwasAction {
            endpoint: "/api/runners/pair".into(),
            input_schema: Some(serde_json::json!({ "title": "Device", "properties": {} })),
            ..Default::default()
        };
        assert_eq!(entity_for_action(&action).as_deref(), Some("Device"));
    }

    #[test]
    fn verb_maps_methods_and_lowercases_unknown() {
        assert_eq!(verb_for_method("GET"), "read");
        assert_eq!(verb_for_method("POST"), "create");
        assert_eq!(verb_for_method("PUT"), "update");
        assert_eq!(verb_for_method("PATCH"), "update");
        assert_eq!(verb_for_method("DELETE"), "delete");
        assert_eq!(verb_for_method("OPTIONS"), "options");
    }

    #[test]
    fn enum_parameter_becomes_enum_rule_with_location_in_provenance() {
        let action = AwasAction {
            id: "setMode".into(),
            ..Default::default()
        };
        let p = AwasParameter {
            name: "mode".into(),
            location: "query".into(),
            enum_values: Some(vec!["fast".into(), "slow".into()]),
            ..Default::default()
        };
        let input = parameter_to_input(&action, &p);
        let v = input.validation.unwrap();
        assert_eq!(v.rule, "enum:fast|slow");
        assert_eq!(
            v.provenance.as_deref(),
            Some("awas:setMode.parameters.mode location=query")
        );
        assert_eq!(v.confidence, SpecProvenance::Observed);
    }

    #[test]
    fn manifest_without_auth_seeds_no_auth_node() {
        let (spec, classes) = awas_to_spec_seed(&AwasManifest::default());
        assert!(spec.auth.is_none());
        assert!(!classes.contains_key("auth"));
        assert!(spec.entities.is_empty() && spec.operations.is_empty());
    }

    #[test]
    fn entities_merge_by_name_across_actions_first_field_wins() {
        let manifest = AwasManifest {
            actions: vec![
                AwasAction {
                    id: "a".into(),
                    method: "get".into(),
                    endpoint: "/api/devices".into(),
                    output_schema: Some(serde_json::json!({
                        "properties": { "id": { "type": "integer" } }
                    })),
                    ..Default::default()
                },
                AwasAction {
                    id: "b".into(),
                    method: "post".into(),
                    endpoint: "/api/devices".into(),
                    input_schema: Some(serde_json::json!({
                        "properties": { "id": { "type": "string" }, "name": {} }
                    })),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (spec, classes) = awas_to_spec_seed(&manifest);
        assert_eq!(spec.entities.len(), 1, "one Device, not two");
        let device = &spec.entities[0];
        assert_eq!(device.name, "Device");
        assert_eq!(device.fields.len(), 2);
        let id = device.fields.iter().find(|f| f.name == "id").unwrap();
        assert_eq!(id.field_type, "integer", "first declaration wins");
        assert_eq!(id.provenance.as_deref(), Some("awas:a.output_schema"));
        let name = device.fields.iter().find(|f| f.name == "name").unwrap();
        assert_eq!(
            name.field_type, "string",
            "untyped property defaults to string"
        );
        assert_eq!(
            classes.get("entities.Device.fields.name"),
            Some(&EvidenceClass::AwasDeclared)
        );
        assert_eq!(spec.operations[0].verb, "read");
        assert_eq!(spec.operations[1].verb, "create");
    }
}
