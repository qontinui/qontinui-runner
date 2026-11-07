//! The runner-local answer value of `GET /apps/{app_id}/journey/edge-timing`
//! (plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`,
//! Phases 2 and 4). In the LIB crate because `schema_export` (lib) registers
//! it for the generated bindings and cannot reach the runner bin's
//! `journey` module; the bin's `journey::routes` builds it.

use std::collections::BTreeMap;

use qontinui_types::journey::{JourneyFilter, JourneyLedgerHealth};
use qontinui_vision_core::EdgeTiming;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The answer value of `GET /apps/{app_id}/journey/edge-timing`.
///
/// **Samples POOL every trigger and outcome of the `(from, to)` pair**:
/// affordance clicks and typed URLs alike, and `changed`, `settle_timeout`
/// and `error` closes alike. Keeping only `affordance` + `changed` would drop
/// exactly the slow and failing interactions (a `settle_timeout` close is the
/// slowest sample there is) and bias every percentile fast. The pooled
/// composition is reported in `samplesByOutcome` / `samplesByTrigger`, so a
/// reader who wants one slice can see whether the pool is dominated by
/// another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(deny_unknown_fields)]
pub struct JourneyEdgeTimingAnswer {
    /// The from-node key asked about.
    pub from_key: String,
    /// The to-node key asked about.
    pub to_key: String,
    /// The filter the observations were narrowed by.
    pub filter_echo: JourneyFilter,
    /// Live (not withdrawn) observations of this edge the filter removed.
    pub edges_filtered_out: u64,
    /// Withdrawn observations of this edge, excluded.
    pub edges_invalidated: u64,
    /// Untrusted EDGE rows anywhere in the read (an edge row that fails
    /// `validate()` or whose `observedAt` is not RFC 3339) — the edge rows
    /// the graph routes list as `rejected` (that list also holds frontier
    /// rows; this count does not). Their node keys cannot be trusted, so none is matched
    /// to this edge, and any of them may be one of its observations; never
    /// samples, never "filtered out".
    pub rows_rejected: u64,
    /// Samples per edge outcome (wire string), pooled into `timing`.
    pub samples_by_outcome: BTreeMap<String, u64>,
    /// Samples per navigation trigger (wire string), pooled into `timing`.
    pub samples_by_trigger: BTreeMap<String, u64>,
    /// Observations whose stored timeline did not parse (counted in `timing`
    /// as without a timeline).
    pub timelines_unparseable: u64,
    /// The distribution, with its sample size.
    pub timing: EdgeTiming,
    /// The ledger the rows were read from.
    pub ledger: JourneyLedgerHealth,
}
