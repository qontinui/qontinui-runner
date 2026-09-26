//! GraphQL schema construction for qontinui-runner.
//!
//! Builds the async-graphql schema from Query, Mutation, and Subscription roots.
//! The schema is mounted alongside the REST API at /graphql (HTTP) and /graphql/ws (WebSocket).

use async_graphql::http::GraphiQLSource;
use async_graphql::Schema;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::response::{Html, IntoResponse};
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
/// - Depth limit of 12 to prevent deeply nested queries
/// - Complexity limit of 500 to prevent expensive queries
/// - Introspection disabled in release builds
/// - ApiState accessible via `ctx.data::<Arc<ApiState>>()`
pub fn build_schema(api_state: Arc<ApiState>) -> QontinuiSchema {
    let builder = Schema::build(QueryRoot, MutationRoot, SubscriptionRoot)
        .data(api_state)
        .limit_depth(12)
        .limit_complexity(500);

    // Disable introspection in release builds to reduce attack surface.
    // GraphiQL and codegen only need introspection during development.
    #[cfg(not(dev))]
    let builder = builder.disable_introspection();

    builder.finish()
}

/// Export the GraphQL schema as SDL (Schema Definition Language).
/// Useful for frontend codegen, documentation, and introspection tests.
pub fn export_sdl() -> String {
    let schema = Schema::build(QueryRoot, MutationRoot, SubscriptionRoot).finish();
    schema.sdl()
}

/// Axum handler for GraphQL HTTP requests (queries and mutations).
/// The schema is injected via axum::Extension.
pub async fn graphql_handler(
    Extension(schema): Extension<QontinuiSchema>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

/// Axum handler for the GraphiQL interactive IDE (GET /graphql).
/// Serves a self-contained HTML page for exploring the schema,
/// running queries, and testing subscriptions during development.
pub async fn graphiql_handler() -> impl IntoResponse {
    Html(
        GraphiQLSource::build()
            .endpoint("/graphql")
            .subscription_endpoint("/graphql/ws")
            .finish(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_exports_valid_sdl() {
        let sdl = export_sdl();

        // Verify root types exist
        assert!(sdl.contains("type QueryRoot"), "Missing QueryRoot");
        assert!(sdl.contains("type MutationRoot"), "Missing MutationRoot");
        assert!(
            sdl.contains("type SubscriptionRoot"),
            "Missing SubscriptionRoot"
        );

        // Verify domain entity types
        assert!(sdl.contains("type GqlTaskRun"), "Missing GqlTaskRun");
        assert!(
            sdl.contains("type GqlWorkflowSummary"),
            "Missing GqlWorkflowSummary"
        );
        assert!(sdl.contains("type GqlWorkflow"), "Missing GqlWorkflow");
        assert!(sdl.contains("type GqlFinding"), "Missing GqlFinding");
        assert!(
            sdl.contains("type GqlFindingSummary"),
            "Missing GqlFindingSummary"
        );
        assert!(
            sdl.contains("type GqlRunnerStatus"),
            "Missing GqlRunnerStatus"
        );
        assert!(
            sdl.contains("type GqlOrchestrationLoopStatus"),
            "Missing GqlOrchestrationLoopStatus"
        );

        // Verify UI Bridge types
        assert!(
            sdl.contains("type UiBridgeHealth"),
            "Missing UiBridgeHealth"
        );
        assert!(sdl.contains("type ActionResult"), "Missing ActionResult");
        assert!(
            sdl.contains("type SdkConnectionInfo"),
            "Missing SdkConnectionInfo"
        );

        // Verify input types
        assert!(
            sdl.contains("input CreateTaskRunInput"),
            "Missing CreateTaskRunInput"
        );
        assert!(
            sdl.contains("input UpdateFindingStatusInput"),
            "Missing UpdateFindingStatusInput"
        );

        // Verify key query resolvers
        assert!(sdl.contains("taskRuns("), "Missing taskRuns query");
        assert!(sdl.contains("taskRun("), "Missing taskRun query");
        assert!(sdl.contains("workflows:"), "Missing workflows query");
        assert!(sdl.contains("findings("), "Missing findings query");
        assert!(sdl.contains("runnerStatus:"), "Missing runnerStatus query");
        assert!(
            sdl.contains("uiBridgeHealth:"),
            "Missing uiBridgeHealth query"
        );

        // Verify key mutation resolvers
        assert!(sdl.contains("stopTaskRun("), "Missing stopTaskRun mutation");
        assert!(
            sdl.contains("createTaskRun("),
            "Missing createTaskRun mutation"
        );
        assert!(sdl.contains("runWorkflow("), "Missing runWorkflow mutation");
        assert!(
            sdl.contains("updateFindingStatus("),
            "Missing updateFindingStatus mutation"
        );

        // Verify subscription resolvers
        assert!(
            sdl.contains("uiBridgeHealthStream("),
            "Missing uiBridgeHealthStream subscription"
        );
        assert!(
            sdl.contains("runnerEvents("),
            "Missing runnerEvents subscription"
        );
        assert!(
            sdl.contains("taskRunProgress("),
            "Missing taskRunProgress subscription"
        );
        assert!(
            sdl.contains("findingUpdates("),
            "Missing findingUpdates subscription"
        );
        assert!(
            sdl.contains("orchestrationLoopStatusStream("),
            "Missing orchestrationLoopStatusStream subscription"
        );

        // Verify error monitor types
        assert!(sdl.contains("type GqlErrorEvent"), "Missing GqlErrorEvent");
        assert!(
            sdl.contains("type GqlErrorSummary"),
            "Missing GqlErrorSummary"
        );
        assert!(
            sdl.contains("type GqlErrorPattern"),
            "Missing GqlErrorPattern"
        );

        // Verify error monitor queries
        assert!(sdl.contains("errorEvents("), "Missing errorEvents query");
        assert!(sdl.contains("errorSummary("), "Missing errorSummary query");
        assert!(
            sdl.contains("errorPatterns("),
            "Missing errorPatterns query"
        );

        // Verify enums
        assert!(
            sdl.contains("enum GqlFindingCategory"),
            "Missing GqlFindingCategory enum"
        );
        assert!(
            sdl.contains("enum GqlFindingSeverity"),
            "Missing GqlFindingSeverity enum"
        );
        assert!(
            sdl.contains("enum GqlFindingStatus"),
            "Missing GqlFindingStatus enum"
        );
        assert!(
            sdl.contains("enum CircuitBreakerState"),
            "Missing CircuitBreakerState enum"
        );

        // The committed SDL is what external consumers codegen against, so it
        // is a DRIFT GATE, not a side effect: a schema change that is not
        // regenerated into `schema.graphql` fails here. Regenerate with
        // `QONTINUI_UPDATE_GRAPHQL_SDL=1` and commit the result. (Until
        // 2026-09-23 this test rewrote the file unconditionally and asserted
        // nothing, so a stale published SDL — 13 of 27 error codes — passed.)
        let sdl_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("graphql")
            .join("schema.graphql");
        if std::env::var("QONTINUI_UPDATE_GRAPHQL_SDL").as_deref() == Ok("1") {
            std::fs::write(&sdl_path, &sdl).expect("Failed to write schema.graphql");
            println!("Schema exported to: {}", sdl_path.display());
            return;
        }
        let committed = std::fs::read_to_string(&sdl_path).expect("Failed to read schema.graphql");
        assert!(
            committed == sdl,
            "src/graphql/schema.graphql is stale against the live schema. \
             Regenerate: `npm run graphql:schema` (QONTINUI_UPDATE_GRAPHQL_SDL=1 \
             cargo test --bin qontinui-runner schema_exports_valid_sdl), then `npm run graphql:codegen`, and commit both."
        );
    }

    /// The GraphQL `UiBridgeErrorCode` is the canonical enum itself (not a
    /// copy), so its variant SET cannot drift. What could still drift is the
    /// SPELLING: async-graphql and serde each apply their own rename rule. This
    /// pins that every GraphQL value name equals the serde wire name an HTTP
    /// caller sees, and that the published SDL lists every one of them.
    #[test]
    fn ui_bridge_error_code_graphql_names_equal_serde_wire_names() {
        use crate::graphql::types::UiBridgeErrorCode;
        use async_graphql::resolver_utils::EnumType;

        let items = <UiBridgeErrorCode as EnumType>::items();
        assert!(items.len() >= 27, "expected the full canonical vocabulary");
        let sdl = export_sdl();
        let block_start = sdl
            .find("enum UiBridgeErrorCode {")
            .expect("SDL must publish enum UiBridgeErrorCode");
        // The enum's closing brace sits alone on its line; a bare `find('}')`
        // would stop inside a variant description that quotes a JSON payload.
        let block_len = sdl[block_start..]
            .find("\n}\n")
            .expect("enum UiBridgeErrorCode block must close");
        let block = &sdl[block_start..block_start + block_len];
        for item in items {
            let wire = serde_json::to_value(item.value).expect("serialize code");
            assert_eq!(
                wire.as_str(),
                Some(item.name),
                "GraphQL name and serde wire name diverge for {:?}",
                item.value
            );
            assert!(
                block.lines().any(|l| l.trim() == item.name),
                "SDL enum UiBridgeErrorCode is missing {}",
                item.name
            );
        }
    }
}
