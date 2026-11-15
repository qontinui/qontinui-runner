//! `GET /fleet/capacity` — this device's row of coord's free-resource model on
//! the loopback, for local tools that hold no coord credential (plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`, Phase 6,
//! "Runner loopback").
//!
//! The answer, inside the usual [`ApiResponse`] envelope, is two things from
//! two sources, kept in separate fields:
//!
//! * [`crate::fleet_capacity::current`]'s read, flattened into `data`:
//!   `data.state` is `known` (with `data.row`, the row exactly as coord's
//!   `GET /coord/fleet/capacity?device_id=` served it) or `unknown` (with
//!   `data.reason`), beside `data.coord`, `data.fetched_at`,
//!   `data.cache_age_secs` and `data.cache_ttl_secs`. That module owns the
//!   fetch, the 60 s cache and the shape.
//! * `data.capacity_model`: the D9 dial (plan D9; coord's
//!   `GET /coord/fleet-policy?domain=fleet_resources` `capacity_model`
//!   block), as this runner's fleet-policy poller last read it, rendered by
//!   [`crate::mcp::fleet_policy_poller::capacity_model_view`] —
//!   `{"state":"known","value":<the block, unchanged>,"read_at":…,"age_secs":…}`,
//!   or `{"state":"unknown","reason":…}` before the poller has read one, which
//!   a consumer reads as "keep the legacy reader". The poller keeps the last
//!   good dial however old it gets (D9), so `age_secs` is what shows a stale
//!   one. A consumer checks its own name (`local_builds`, `ci_placement`, …)
//!   in `value.permitted_consumers`.
//!
//! An `unknown` is still a 200 with `success: true`: the loopback answered,
//! and its answer is that the capacity is UNKNOWN — which a caller must not
//! read as zero.
//!
//! # Origin guard
//!
//! The route answers a coord read made with this device's credential, the
//! same class as the `/coord-mcp/*` reads, so it is on
//! [`crate::mcp::origin_guard::CREDENTIAL_DOORS`]: agents, scripts and the
//! runner's own webview reach it, and no browser origin does.
//!
//! # Registration
//!
//! `mcp_api::create_router` merges [`routes`]. The path is a literal in the
//! `.route(` call, not [`ROUTE_PATH`], because the route census and the door
//! tripwire read literal paths only
//! (`relay_path_policy::tests::registered_routes`); a test pins the two equal.

use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

use crate::fleet_capacity::CapacityRead;
use crate::mcp::types::{ApiResponse, ApiState};

/// The one path this family serves.
pub const ROUTE_PATH: &str = "/fleet/capacity";

/// What the route serves: the capacity read, and beside it the dial.
#[derive(Debug, Serialize)]
pub(crate) struct FleetCapacityAnswer {
    #[serde(flatten)]
    read: CapacityRead,
    /// The poller's last dial with its age
    /// ([`crate::mcp::fleet_policy_poller::capacity_model_view`]).
    capacity_model: Value,
}

/// `GET /fleet/capacity` — this device's capacity, from the runner's cache or
/// a fresh read of coord, and the dial the poller last read.
pub(crate) async fn get_fleet_capacity() -> Json<ApiResponse<FleetCapacityAnswer>> {
    let read = crate::fleet_capacity::current().await;
    let capacity_model = crate::mcp::fleet_policy_poller::capacity_model_view(Utc::now());
    Json(ApiResponse::success(FleetCapacityAnswer {
        read,
        capacity_model,
    }))
}

/// Routes contributed to the runner's main router from `mcp_api.rs`.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/fleet/capacity", get(get_fleet_capacity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// The literal `routes()` registers is [`ROUTE_PATH`], read by the same
    /// census the origin guard's door tripwire reads.
    #[test]
    fn routes_registers_the_documented_path() {
        let registered = crate::mcp::relay_path_policy::tests::registered_routes();
        assert!(
            registered.contains(&("GET".to_string(), ROUTE_PATH.to_string())),
            "GET {ROUTE_PATH} is not a registered route"
        );
    }

    /// The handler answers with the envelope and the read. On an isolated,
    /// empty machine no coord is configured, so the read is `unknown` with its
    /// reason, carries no row, and never touches the network. The handler
    /// takes no state, so it is mounted on a bare router.
    #[tokio::test]
    async fn the_handler_serves_the_read_in_the_envelope() {
        let _amb = crate::test_env::isolated_ambient();
        let response = Router::new()
            .route(ROUTE_PATH, get(get_fleet_capacity))
            .oneshot(
                Request::builder()
                    .uri(ROUTE_PATH)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["success"], true, "an unknown is an answer: {body}");
        let data = &body["data"];
        assert_eq!(data["state"], "unknown", "{data}");
        assert!(data["row"].is_null(), "an unknown carries no row: {data}");
        assert!(data["reason"].is_string(), "an unknown names why: {data}");
        assert!(data["cache_ttl_secs"].is_u64(), "{data}");
        // The dial is its own field beside the read, in one of its two states
        // (which one depends on whether this process's poller ever read one),
        // and a known one carries its age.
        let dial = &data["capacity_model"];
        match dial["state"].as_str() {
            Some("known") => assert!(dial["age_secs"].is_number(), "{data}"),
            Some("unknown") => assert!(dial["reason"].is_string(), "{data}"),
            _ => panic!("the dial is known or unknown: {data}"),
        }
    }
}
