//! The runner-local answer value of `GET /apps/{app_id}/journey/edge-timing`
//! (plan
//! `2026-09-20-ui-bridge-represents-the-users-path-and-the-passage-of-time`,
//! Phases 2 and 4). In the LIB crate because `schema_export` (lib) registers
//! it for the generated bindings and cannot reach the runner bin's
//! `journey` module; the bin's `journey::routes` builds it.

use qontinui_types::journey::{JourneyFilter, JourneyLedgerHealth};
use qontinui_vision_core::EdgeTiming;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The answer value of `GET /apps/{app_id}/journey/edge-timing`.
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
    /// Observations whose stored timeline did not parse (counted in `timing`
    /// as without a timeline).
    pub timelines_unparseable: u64,
    /// The distribution, with its sample size.
    pub timing: EdgeTiming,
    /// The ledger the rows were read from.
    pub ledger: JourneyLedgerHealth,
}
