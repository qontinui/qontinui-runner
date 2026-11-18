//! `GET /fleet/capacity` — this device's row of coord's free-resource model on
//! the loopback, for local tools that hold no coord credential (plan
//! `2026-11-05-one-free-resource-model-places-sessions-builds-and-ci`, Phase 6,
//! "Runner loopback").
//!
//! The answer, inside the usual [`ApiResponse`] envelope, is two things from
//! two sources, kept in separate fields, and a verdict on whether they share a
//! coord:
//!
//! * [`crate::fleet_capacity::current`]'s read, flattened into `data`:
//!   `data.state` is `known` (with `data.row`, the row exactly as coord's
//!   `GET /coord/fleet/capacity?device_id=` served it) or `unknown` (with
//!   `data.reason`), beside `data.coord`, `data.coord_base` (the coord the
//!   read asked; `null` when the runner is not connected), `data.fetched_at`,
//!   `data.cache_age_secs` and `data.cache_ttl_secs`. That module owns the
//!   fetch, the 60 s cache and the shape.
//! * `data.capacity_model`: the D9 dial (plan D9; coord's
//!   `GET /coord/fleet-policy?domain=fleet_resources` `capacity_model`
//!   block), as this runner's fleet-policy poller last read it, rendered by
//!   [`crate::mcp::fleet_policy_poller::capacity_model_view`] —
//!   `{"state":"known","value":<the block, unchanged>,"coord_base":…,"read_at":…,"age_secs":…}`,
//!   or `{"state":"unknown","reason":…}` before the poller has read one, which
//!   a consumer reads as "keep the legacy reader". The poller keeps the last
//!   good dial however old it gets (D9), so `age_secs` is what shows a stale
//!   one. A consumer checks its own name (`local_builds`, `ci_placement`, …)
//!   in `value.permitted_consumers`.
//! * `data.same_coord`: whether the read and the dial came from the same
//!   coord — `true` or `false` comparing `data.coord_base` with
//!   `data.capacity_model.coord_base` (a trailing `/` ignored), and `null`
//!   when either has no base (the read was not connected, or no dial has been
//!   read). The two bases are resolved by different families
//!   (`connected_coord_base` for the read, `coord_base_with_source` for the
//!   poll), and the poller keeps its last good dial indefinitely, so they can
//!   differ; a consumer joining `data.row` with
//!   `data.capacity_model.value.permitted_consumers` joins only on `true`.
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

/// What the route serves: the capacity read, beside it the dial, and whether
/// the two came from the same coord.
#[derive(Debug, Serialize)]
pub(crate) struct FleetCapacityAnswer {
    #[serde(flatten)]
    read: CapacityRead,
    /// The poller's last dial with its age
    /// ([`crate::mcp::fleet_policy_poller::capacity_model_view`]).
    capacity_model: Value,
    /// `true` / `false`: the read's `coord_base` and the dial's agree / do
    /// not; `None` (`null`) when either has no base. Never defaulted.
    same_coord: Option<bool>,
}

/// The answer for `read` and the served `dial`. PURE.
///
/// The join this answers is the coordinator only. The credential is not part
/// of it: the read presents `coord_mcp::read_usable_device_jwt()` and the
/// poll `AuthManager::get_access_token()`, and both read the same legacy
/// `access_token` slot (the default binding). They differ only at the edges —
/// the read adds a usability gate (an expired token is no credential), and
/// the poll falls back to the keychain when the `.enc` store is missing or
/// malformed — so they present the same device's token whenever both present
/// one, and no field reports it.
pub(crate) fn answer(read: CapacityRead, dial: Value) -> FleetCapacityAnswer {
    let same_coord = same_coord(read.coord_base.as_deref(), &dial);
    FleetCapacityAnswer {
        read,
        capacity_model: dial,
        same_coord,
    }
}

/// Whether `read_base` and the known dial's `coord_base` name the same coord,
/// ignoring a trailing `/`; `None` when either is absent. PURE.
fn same_coord(read_base: Option<&str>, dial: &Value) -> Option<bool> {
    let read_base = read_base?;
    let dial_base = dial
        .get("state")
        .and_then(Value::as_str)
        .filter(|state| *state == "known")
        .and_then(|_| dial.get("coord_base"))
        .and_then(Value::as_str)?;
    Some(read_base.trim_end_matches('/') == dial_base.trim_end_matches('/'))
}

/// `GET /fleet/capacity` — this device's capacity, from the runner's cache or
/// a fresh read of coord, the dial the poller last read, and whether they
/// share a coord.
pub(crate) async fn get_fleet_capacity() -> Json<ApiResponse<FleetCapacityAnswer>> {
    let read = crate::fleet_capacity::current().await;
    let dial = crate::mcp::fleet_policy_poller::capacity_model_view(Utc::now());
    Json(ApiResponse::success(answer(read, dial)))
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

    use crate::fleet_capacity::{CapacityCache, Fetched, Prepared};
    use crate::mcp::fleet_policy_poller::{dial_view, CapacityModelReading};
    use std::time::Instant;

    /// A read from a test-owned cache whose preparation resolved `coord_base`
    /// (`None`: not connected) and settled `unknown` there.
    async fn read_from(coord_base: Option<&str>) -> CapacityRead {
        CapacityCache::new()
            .get(
                || (Instant::now(), Utc::now()),
                || Prepared::<std::future::Ready<Fetched>>::Done {
                    coord_base: coord_base.map(str::to_string),
                    fetched: Fetched::Unknown {
                        device_id: None,
                        reason: "no machine.json".to_string(),
                    },
                },
            )
            .await
    }

    /// The dial as served when the poller read it from `coord_base`.
    fn known_dial(coord_base: &str) -> Value {
        dial_view(
            Some(CapacityModelReading {
                dial: serde_json::json!({"mode": "enforce", "permitted_consumers": ["local_builds"]}),
                coord_base: coord_base.to_string(),
                read_at: Utc::now(),
            }),
            Utc::now(),
        )
    }

    /// The same coord, one spelled with a trailing `/`, is the same coord.
    #[tokio::test]
    async fn same_coord_is_true_when_the_bases_agree_but_for_a_trailing_slash() {
        let answer = answer(
            read_from(Some("https://coord.qontinui.io")).await,
            known_dial("https://coord.qontinui.io/"),
        );
        assert_eq!(answer.same_coord, Some(true));
        let wire = serde_json::to_value(&answer).unwrap();
        assert_eq!(wire["same_coord"], true, "{wire}");
        assert_eq!(wire["coord_base"], "https://coord.qontinui.io", "{wire}");
        assert_eq!(
            wire["capacity_model"]["coord_base"], "https://coord.qontinui.io/",
            "{wire}"
        );
    }

    /// A read and a dial from different coords say so.
    #[tokio::test]
    async fn same_coord_is_false_when_the_bases_differ() {
        let answer = answer(
            read_from(Some("https://coord.qontinui.io")).await,
            known_dial("http://localhost:9870"),
        );
        assert_eq!(answer.same_coord, Some(false));
        let wire = serde_json::to_value(&answer).unwrap();
        assert_eq!(wire["same_coord"], false, "{wire}");
    }

    /// With no dial read there is nothing to compare: `null`, never `true`.
    #[tokio::test]
    async fn same_coord_is_null_for_an_unknown_dial() {
        let answer = answer(
            read_from(Some("https://coord.qontinui.io")).await,
            dial_view(None, Utc::now()),
        );
        assert_eq!(answer.same_coord, None);
        let wire = serde_json::to_value(&answer).unwrap();
        assert!(wire["same_coord"].is_null(), "{wire}");
    }

    /// A read that resolved no coord compares with nothing: `null`.
    #[tokio::test]
    async fn same_coord_is_null_for_an_unconnected_read() {
        let answer = answer(
            read_from(None).await,
            known_dial("https://coord.qontinui.io"),
        );
        assert_eq!(answer.same_coord, None);
        let wire = serde_json::to_value(&answer).unwrap();
        assert!(wire["same_coord"].is_null(), "{wire}");
        assert!(wire["coord_base"].is_null(), "{wire}");
    }

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
