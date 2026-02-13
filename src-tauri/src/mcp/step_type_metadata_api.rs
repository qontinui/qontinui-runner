//! Step Type Metadata API
//!
//! Exposes step type metadata via HTTP so the web backend and other
//! consumers can fetch it instead of maintaining independent copies.

use axum::{response::Json, routing::get, Router};
use serde::Serialize;
use std::sync::Arc;

use crate::mcp::types::{ApiResponse, ApiState};
use crate::workflow_generation::step_type_metadata::get_all_step_type_metadata;

#[derive(Serialize)]
struct StepTypeMetadataResponse {
    step_type: &'static str,
    display_name: &'static str,
    description: &'static str,
    category: &'static str,
    allowed_phases: &'static [&'static str],
    icon: &'static str,
    color: &'static str,
    fields: Vec<FieldDefResponse>,
}

#[derive(Serialize)]
struct FieldDefResponse {
    name: &'static str,
    field_type: &'static str,
    required: bool,
    description: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    enum_values: Vec<&'static str>,
    #[serde(skip_serializing_if = "str::is_empty")]
    default: &'static str,
}

async fn get_step_types_metadata() -> Json<ApiResponse<Vec<StepTypeMetadataResponse>>> {
    let metadata = get_all_step_type_metadata();

    let response: Vec<StepTypeMetadataResponse> = metadata
        .iter()
        .map(|meta| StepTypeMetadataResponse {
            step_type: meta.step_type,
            display_name: meta.display_name,
            description: meta.description,
            category: meta.category.as_str(),
            allowed_phases: meta.allowed_phases,
            icon: meta.icon,
            color: meta.color,
            fields: meta
                .fields
                .iter()
                .map(|f| FieldDefResponse {
                    name: f.name,
                    field_type: f.field_type.as_str(),
                    required: f.required,
                    description: f.description,
                    enum_values: f.enum_values.to_vec(),
                    default: f.default,
                })
                .collect(),
        })
        .collect();

    Json(ApiResponse::success(response))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/step-types/metadata", get(get_step_types_metadata))
}
