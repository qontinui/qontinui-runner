//! GraphQL schema construction for qontinui-runner.
//!
//! Builds the async-graphql schema from Query, Mutation, and Subscription roots.
//! The schema is mounted alongside the REST API at /graphql (HTTP) and /graphql/ws (WebSocket).

use async_graphql::Schema;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::Extension;
use std::sync::Arc;

use crate::mcp::types::ApiState;

use super::mutation::MutationRoot;
use super::query::QueryRoot;
use super::subscription::SubscriptionRoot;

/// The complete GraphQL schema type.
pub type QontinuiSchema = Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

/// Build the GraphQL schema with ApiState as context data.
///
/// The schema is configured with:
/// - Depth limit of 10 to prevent deeply nested queries
/// - Complexity limit of 200 to prevent expensive queries
/// - ApiState accessible via `ctx.data::<Arc<ApiState>>()`
pub fn build_schema(api_state: Arc<ApiState>) -> QontinuiSchema {
    Schema::build(QueryRoot, MutationRoot, SubscriptionRoot)
        .data(api_state)
        .limit_depth(12)
        .limit_complexity(500)
        .finish()
}

/// Axum handler for GraphQL HTTP requests (queries and mutations).
/// The schema is injected via axum::Extension.
pub async fn graphql_handler(
    Extension(schema): Extension<QontinuiSchema>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}
