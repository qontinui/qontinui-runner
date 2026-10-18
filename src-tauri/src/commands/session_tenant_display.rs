//! The ONE derived answer to "which tenant is this session?" — the projection
//! the session-info dropdown's `Tenant` row, the zone `TenantBadge` and the
//! UI-Bridge oracle all render (plan
//! `2026-10-17-session-info-tenant-row-renders-unknown-for-a-resolved-tenant`
//! Phase 2).
//!
//! ## Why it exists
//!
//! The `Tenant` row used to render `identity.tenantId` — the lifecycle record's
//! SPAWN STAMP, which is null whenever the spawn chose no tenant. On a
//! multi-bound device that is the ordinary case, so a session whose credential
//! resolved a tenant (and whose every coord write lands in it) read `unknown`.
//! The tenancy block already resolved it; nothing projected it into the row.
//!
//! ## Precedence (decided in the plan, not here)
//!
//! 1. The credential, when resolved on a PER-TENANT slot (`source:
//!    credential`). A default-slot credential's tenant is the local default
//!    binding, not a coord answer, so it is labelled `credential_default_slot`
//!    and never promoted to "via credential".
//! 2. The data plane: `owned` (`data_plane`) or `device` (`data_plane_device`).
//! 3. The spawn stamp, labelled as a stamp (`stamp_only`), never as an
//!    observation.
//! 4. Otherwise `unknown`, with the first reason a leg supplied.
//!
//! When the legs DISAGREE (`tenancy.diverged`) the status is `mismatch` and
//! every leg's tenant is listed — the display never picks one silently.
//!
//! Pure: no I/O. The caller hands in the bound-set read and a slug lookup.

use serde::Serialize;

use super::session_info::{SessionTenancy, TENANCY_RESOLVED, TENANCY_UNKNOWN};
use crate::auth::BindingSetRead;

/// [`TenantDisplay::status`]: the acting tenant is resolved and no leg
/// disagrees with it.
pub const DISPLAY_RESOLVED: &str = "resolved";
/// [`TenantDisplay::status`]: two or more legs name different tenants (or a
/// coord answer was observed from another tenant).
pub const DISPLAY_MISMATCH: &str = "mismatch";
/// [`TenantDisplay::status`]: only the spawn stamp names a tenant — a record of
/// what the spawn chose, not an observation of what the session acts as.
pub const DISPLAY_STAMP_ONLY: &str = "stamp_only";
/// [`TenantDisplay::status`]: nothing names a tenant; `reason` says why.
pub const DISPLAY_UNKNOWN: &str = "unknown";

pub const SOURCE_CREDENTIAL: &str = "credential";
pub const SOURCE_CREDENTIAL_DEFAULT_SLOT: &str = "credential_default_slot";
pub const SOURCE_DATA_PLANE: &str = "data_plane";
pub const SOURCE_DATA_PLANE_DEVICE: &str = "data_plane_device";
pub const SOURCE_ROW_STAMP: &str = "row_stamp";

/// [`BoundTenants::status`]: the binding store was read.
pub const BOUND_MEASURED: &str = "measured";
/// [`BoundTenants::status`]: the binding store could not be read.
pub const BOUND_UNKNOWN: &str = "unknown";

/// The `unknown` reason when no leg supplied one.
pub const NO_LEG_RESOLVED: &str = "no tenancy leg resolved";

/// One leg's tenant, in a `mismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantConflict {
    /// `credential` | `data_plane` | `row_stamp` | `spawn_device_default` |
    /// `repo_expected` | `coord_row`.
    pub leg: String,
    pub tenant_id: String,
}

/// The device's bound tenant set, as read — never an empty list in place of an
/// unreadable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundTenants {
    /// [`BOUND_MEASURED`] | [`BOUND_UNKNOWN`].
    pub status: String,
    /// The bound tenants when measured; empty when unknown.
    pub tenant_ids: Vec<String>,
    /// Why the set is unknown.
    pub reason: Option<String>,
}

/// See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantDisplay {
    /// [`DISPLAY_RESOLVED`] | [`DISPLAY_MISMATCH`] | [`DISPLAY_STAMP_ONLY`] |
    /// [`DISPLAY_UNKNOWN`].
    pub status: String,
    /// The tenant the precedence picked — present for every status but
    /// `unknown`, including `mismatch` (it is the acting tenant there too; the
    /// conflict is listed beside it).
    pub tenant_id: Option<String>,
    /// Coord's own slug for `tenant_id`, only from an answer that tenant gave
    /// this session. Never another tenant's.
    pub tenant_slug: Option<String>,
    /// Which leg `tenant_id` came from — see the `SOURCE_*` constants.
    pub source: Option<String>,
    /// Why the acting tenant is not (fully) established:
    /// - no tenant picked (`unknown`, and a `mismatch` with no acting tenant):
    ///   the first reason a leg gave, else [`NO_LEG_RESOLVED`];
    /// - `stamp_only`: the credential / data-plane reason;
    /// - a data-plane source while the credential is UNKNOWN: the credential's
    ///   reason, so a surface can say the credential could not be read;
    /// - otherwise `None`.
    pub reason: Option<String>,
    /// Every leg's tenant, when `mismatch`; empty otherwise.
    pub conflicts: Vec<TenantConflict>,
    /// The repo leg's verdict detail, when `mismatch`.
    pub conflict_detail: Option<String>,
    pub bound: BoundTenants,
    /// `Some(true)` on a device measured to hold more than one binding;
    /// `None` — never `Some(false)` — when the bound set is unknown.
    pub multi_bound: Option<bool>,
    /// Whether `tenant_id` is one of the bound tenants; `None` when either is
    /// unknown.
    pub acting_in_bound_set: Option<bool>,
}

/// Coord's slug for `tenant`, from the latest answer this session's nonce
/// received — only when that answer names `tenant`. The same matching rule as
/// the repo leg's slug (`TenancyRepoExpected::from_read`).
pub(crate) fn slug_from_answer(
    latest: Option<&crate::coord_mcp_tenant::VerdictRecord>,
    tenant: &str,
) -> Option<String> {
    let v = latest?;
    let answered = v.answered_tenant_id.as_deref()?;
    if answered.trim().eq_ignore_ascii_case(tenant.trim()) {
        v.answered_tenant_slug
            .clone()
            .filter(|s| !s.trim().is_empty())
    } else {
        None
    }
}

/// Project the display. Pure.
pub(crate) fn project_tenant_display(
    tenancy: &SessionTenancy,
    bound: BindingSetRead,
    slug_for: impl Fn(&str) -> Option<String>,
) -> TenantDisplay {
    let picked: Option<(&str, &'static str)> = acting_tenant(tenancy);

    let source = picked.map(|(_, s)| s);
    let status = match source {
        _ if tenancy.diverged => DISPLAY_MISMATCH,
        Some(SOURCE_ROW_STAMP) => DISPLAY_STAMP_ONLY,
        Some(_) => DISPLAY_RESOLVED,
        None => DISPLAY_UNKNOWN,
    };
    // Decided by the source alone, so a `mismatch` carries the same reason its
    // non-diverged twin would — above all when it has no acting tenant.
    let reason = match source {
        None => Some(unresolved_reason(tenancy).unwrap_or_else(|| NO_LEG_RESOLVED.to_string())),
        Some(SOURCE_ROW_STAMP) => unresolved_reason(tenancy),
        Some(SOURCE_DATA_PLANE | SOURCE_DATA_PLANE_DEVICE) => credential_unknown_reason(tenancy),
        Some(_) => None,
    };

    let (conflicts, conflict_detail) = if tenancy.diverged {
        (
            conflicting_legs(tenancy),
            tenancy.repo_expected.verdict_detail.clone(),
        )
    } else {
        (Vec::new(), None)
    };

    let bound = match bound {
        BindingSetRead::Measured(ids) => BoundTenants {
            status: BOUND_MEASURED.to_string(),
            tenant_ids: ids,
            reason: None,
        },
        BindingSetRead::Unknown(reason) => BoundTenants {
            status: BOUND_UNKNOWN.to_string(),
            tenant_ids: Vec::new(),
            reason: Some(reason.to_string()),
        },
    };
    let measured = bound.status == BOUND_MEASURED;
    let multi_bound = measured.then(|| bound.tenant_ids.len() > 1);
    let tenant_id = picked.map(|(t, _)| t.to_string());
    let acting_in_bound_set = match (&tenant_id, measured) {
        (Some(t), true) => Some(
            bound
                .tenant_ids
                .iter()
                .any(|b| b.trim().eq_ignore_ascii_case(t)),
        ),
        _ => None,
    };

    TenantDisplay {
        status: status.to_string(),
        tenant_slug: tenant_id.as_deref().and_then(&slug_for),
        tenant_id,
        source: source.map(str::to_string),
        reason,
        conflicts,
        conflict_detail,
        bound,
        multi_bound,
        acting_in_bound_set,
    }
}

/// The precedence of the module docs: `(tenant, source)`.
fn acting_tenant(t: &SessionTenancy) -> Option<(&str, &'static str)> {
    if t.credential.status == TENANCY_RESOLVED {
        if let Some(id) = t.credential.tenant_id.as_deref() {
            let source = if t.credential.slot.as_deref() == Some("tenant") {
                SOURCE_CREDENTIAL
            } else {
                SOURCE_CREDENTIAL_DEFAULT_SLOT
            };
            return Some((id, source));
        }
    }
    if let Some(id) = t.data_plane.tenant_id.as_deref() {
        match t.data_plane.status.as_str() {
            "owned" => return Some((id, SOURCE_DATA_PLANE)),
            "device" => return Some((id, SOURCE_DATA_PLANE_DEVICE)),
            _ => {}
        }
    }
    t.row.tenant_id.as_deref().map(|id| (id, SOURCE_ROW_STAMP))
}

/// The first reason a leg gave for not naming a tenant: the credential's, then
/// the data plane's.
fn unresolved_reason(t: &SessionTenancy) -> Option<String> {
    t.credential
        .reason
        .clone()
        .or_else(|| t.data_plane.reason.clone())
}

/// The reason a credential leg that is UNKNOWN but names no reason of its own
/// carries, so a data-plane-sourced display never loses the credential context.
pub const CREDENTIAL_UNKNOWN_NO_REASON: &str = "credential_unknown_no_reason";

/// The credential's reason when the credential leg is UNKNOWN — the context a
/// data-plane-sourced display carries so it never reads as if the credential
/// agreed. An unknown credential with no reason still answers
/// [`CREDENTIAL_UNKNOWN_NO_REASON`], never `None`.
fn credential_unknown_reason(t: &SessionTenancy) -> Option<String> {
    (t.credential.status == TENANCY_UNKNOWN).then(|| {
        t.credential
            .reason
            .clone()
            .unwrap_or_else(|| CREDENTIAL_UNKNOWN_NO_REASON.to_string())
    })
}

/// Every leg that names a tenant, in a fixed order. The spawn-time device
/// default takes part only as `fold_divergence` uses it — the expectation of a
/// spawn that neither stamped a tenant nor has a data-plane tenant.
fn conflicting_legs(t: &SessionTenancy) -> Vec<TenantConflict> {
    let mut legs: Vec<TenantConflict> = Vec::new();
    let mut push = |leg: &str, id: Option<&str>| {
        if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) {
            legs.push(TenantConflict {
                leg: leg.to_string(),
                tenant_id: id.to_string(),
            });
        }
    };
    push("credential", t.credential.tenant_id.as_deref());
    push("data_plane", t.data_plane.tenant_id.as_deref());
    push("row_stamp", t.row.tenant_id.as_deref());
    if t.row.tenant_id.is_none() && t.data_plane.tenant_id.is_none() {
        push(
            "spawn_device_default",
            t.row.spawn_device_default_tenant_id.as_deref(),
        );
    }
    match t.repo_expected.expected_tenant() {
        Some(id) => push("repo_expected", Some(id)),
        // A repo several tenants own, and no caller-named tenant: every owner.
        None if t.repo_expected.status == "several" => {
            for owner in &t.repo_expected.tenant_ids {
                push("repo_expected", Some(owner));
            }
        }
        None => {}
    }
    push("coord_row", t.coord_row.registered_tenant());
    legs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{BindingTenantRead, TenantScope};
    use crate::claude_session::coord_register::{RecordedCoordRow, RecordedRowOrigin};
    use crate::commands::session_info::{
        coord_row_from_registrar, project_tenancy, CoordRowRead, SpawnDefaultRead, TenancyCoordRow,
        TenancyRepoExpected, TerminalRowTenant, COORD_ROW_ADOPTED, COORD_ROW_NOT_APPLICABLE,
        COORD_ROW_NOT_REGISTERED, COORD_ROW_OTHER_TERMINAL, COORD_ROW_REGISTRAR_POISONED,
        COORD_ROW_TERMINAL_PLANE, COORD_ROW_TERMINAL_ROW_NOT_WIRED,
        COORD_ROW_TERMINAL_ROW_RESUME_UNCONFIRMED, COORD_ROW_TERMINAL_ROW_TENANT_UNRESOLVED,
        DIVERGENCE_AGREE, DIVERGENCE_DIVERGED, TENANCY_UNKNOWN,
    };
    use crate::coord_mcp::CredentialTenantRead;
    use uuid::Uuid;

    /// The 2026-10-17 measurement's acting tenant (`qontinui`).
    const QONTINUI: &str = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
    const MERYTS: &str = "11111111-1111-4111-8111-111111111111";
    const BAUHAUS: &str = "22222222-2222-4222-8222-222222222222";
    const PIZZERIA: &str = "33333333-3333-4333-8333-333333333333";

    fn u(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    fn four_bindings() -> BindingSetRead {
        BindingSetRead::Measured(vec![
            MERYTS.into(),
            QONTINUI.into(),
            BAUHAUS.into(),
            PIZZERIA.into(),
        ])
    }

    fn no_slug(_: &str) -> Option<String> {
        None
    }

    /// A tenancy block from its legs, with no repo leg and no coord-row leg.
    fn tenancy(
        row: Option<&str>,
        data_plane: Option<TenantScope>,
        credential: CredentialTenantRead,
        default_binding: BindingTenantRead,
    ) -> SessionTenancy {
        project_tenancy(
            row,
            data_plane,
            &credential,
            default_binding,
            SpawnDefaultRead::Recorded(Some(u(QONTINUI))),
            Some(u(QONTINUI)),
            None,
        )
    }

    /// The hosting terminal's own coord row (`SessionRegistry`'s).
    const TERMINAL_ROW: Uuid = Uuid::from_u128(0x7e7e_0000_0000_4000_8000_000000000001);
    /// A coord row that is NOT the hosting terminal's.
    const OTHER_ROW: Uuid = Uuid::from_u128(0x7e7e_0000_0000_4000_8000_000000000002);

    fn row(coord_session_id: Uuid, origin: RecordedRowOrigin) -> Option<RecordedCoordRow> {
        Some(RecordedCoordRow {
            coord_session_id,
            origin,
        })
    }

    /// What `read_session_tenancy` would pass for the terminal's row of `t`:
    /// the data plane's own answer, with `observed` from the registry.
    fn terminal_tenant(t: &SessionTenancy, observed: bool) -> TerminalRowTenant {
        TerminalRowTenant::from_reads(t.data_plane.tenant_id.is_some(), Some(observed))
    }

    /// The coord-row leg of a terminal-hosted session with no registrar entry:
    /// the pinned terminal names its own row (freshly registered by this
    /// process), and the data plane of `t` read it.
    fn terminal_plane_row(t: &SessionTenancy) -> TenancyCoordRow {
        TenancyCoordRow::from_read(coord_row_from_registrar(
            row(TERMINAL_ROW, RecordedRowOrigin::TerminalPinned),
            Some(TERMINAL_ROW),
            true,
            terminal_tenant(t, true),
        ))
    }

    /// The same session after transcript auto-bind ADOPTED the terminal's own
    /// row into the registrar.
    fn adopted_terminal_row(t: &SessionTenancy) -> TenancyCoordRow {
        TenancyCoordRow::from_read(coord_row_from_registrar(
            row(TERMINAL_ROW, RecordedRowOrigin::Adopted),
            Some(TERMINAL_ROW),
            true,
            terminal_tenant(t, true),
        ))
    }

    /// The same pane restored after a runner restart: `resume_external`
    /// re-stamped the row's tenant from today's resolution and nothing
    /// observed the tenant coord created the row under.
    fn resumed_unconfirmed_terminal_row(t: &SessionTenancy) -> TenancyCoordRow {
        TenancyCoordRow::from_read(coord_row_from_registrar(
            row(TERMINAL_ROW, RecordedRowOrigin::TerminalPinned),
            Some(TERMINAL_ROW),
            true,
            terminal_tenant(t, false),
        ))
    }

    /// `t` with its terminal-plane coord-row leg attached.
    fn with_terminal_row(
        t: SessionTenancy,
        leg: fn(&SessionTenancy) -> TenancyCoordRow,
    ) -> SessionTenancy {
        let row = leg(&t);
        t.with_coord_row(row)
    }

    /// The 2026-10-17 session: no spawn stamp, a per-tenant credential and an
    /// owned data plane on the acting tenant, on a four-binding device.
    fn regression_tenancy() -> SessionTenancy {
        tenancy(
            None,
            Some(TenantScope::Owned(u(QONTINUI))),
            CredentialTenantRead::Resolved(Some(u(QONTINUI))),
            BindingTenantRead::Bound(u(QONTINUI)),
        )
    }

    // ---- the regression (plan 2026-10-17, the hand-found defect) ----------

    #[test]
    fn regression_2026_10_17_no_stamp_four_bindings_resolves_via_credential() {
        let d = project_tenant_display(&regression_tenancy(), four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
        assert_eq!(d.source.as_deref(), Some("credential"));
        assert_eq!(d.multi_bound, Some(true));
        assert_eq!(d.acting_in_bound_set, Some(true));
        assert_eq!(d.reason, None);
        assert!(d.conflicts.is_empty());
        assert_eq!(d.bound.status, "measured");
        assert_eq!(d.bound.tenant_ids.len(), 4);
    }

    // ---- bound-set cardinality: 0 / 1 / 4 / unknown ------------------------

    #[test]
    fn zero_bindings_is_measured_and_not_multi_bound() {
        let d = project_tenant_display(
            &regression_tenancy(),
            BindingSetRead::Measured(vec![]),
            no_slug,
        );
        assert_eq!(d.bound.status, "measured");
        assert_eq!(d.multi_bound, Some(false));
        assert_eq!(d.acting_in_bound_set, Some(false));
    }

    #[test]
    fn one_binding_is_not_multi_bound() {
        let d = project_tenant_display(
            &regression_tenancy(),
            BindingSetRead::Measured(vec![QONTINUI.into()]),
            no_slug,
        );
        assert_eq!(d.multi_bound, Some(false));
        assert_eq!(d.acting_in_bound_set, Some(true));
    }

    #[test]
    fn an_unknown_bound_set_withholds_multi_bound_rather_than_saying_false() {
        let d = project_tenant_display(
            &regression_tenancy(),
            BindingSetRead::Unknown("paired_user_unreadable"),
            no_slug,
        );
        assert_eq!(d.bound.status, "unknown");
        assert_eq!(d.bound.reason.as_deref(), Some("paired_user_unreadable"));
        assert!(d.bound.tenant_ids.is_empty());
        assert_eq!(d.multi_bound, None);
        assert_eq!(d.acting_in_bound_set, None);
        // The acting tenant is still resolved — the bound set is context.
        assert_eq!(d.status, "resolved");
    }

    #[test]
    fn an_acting_tenant_outside_the_bound_set_says_so() {
        let d = project_tenant_display(
            &regression_tenancy(),
            BindingSetRead::Measured(vec![MERYTS.into(), BAUHAUS.into()]),
            no_slug,
        );
        assert_eq!(d.multi_bound, Some(true));
        assert_eq!(d.acting_in_bound_set, Some(false));
    }

    // ---- source precedence --------------------------------------------------

    #[test]
    fn a_default_slot_credential_is_never_labelled_via_credential() {
        let t = tenancy(
            None,
            None,
            CredentialTenantRead::Resolved(None),
            BindingTenantRead::Bound(u(QONTINUI)),
        );
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
        assert_eq!(d.source.as_deref(), Some("credential_default_slot"));
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
    }

    #[test]
    fn an_owned_data_plane_answers_when_the_credential_is_unknown() {
        let t = tenancy(
            None,
            Some(TenantScope::Owned(u(QONTINUI))),
            CredentialTenantRead::NotLive,
            BindingTenantRead::Unknown,
        );
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
        assert_eq!(d.source.as_deref(), Some("data_plane"));
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
        // The credential could not be read: the display says so rather than
        // reading as if the credential agreed.
        assert_eq!(d.reason.as_deref(), Some("nonce_not_live"));
    }

    #[test]
    fn a_device_data_plane_is_labelled_data_plane_device() {
        let t = tenancy(
            None,
            Some(TenantScope::Device),
            CredentialTenantRead::NoNonce,
            BindingTenantRead::Bound(u(QONTINUI)),
        );
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
        assert_eq!(d.source.as_deref(), Some("data_plane_device"));
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
        assert_eq!(d.reason.as_deref(), Some("no_session_nonce"));
    }

    #[test]
    fn a_data_plane_source_with_a_resolved_credential_carries_no_reason() {
        // Only an UNKNOWN credential's reason reaches a data-plane display.
        let mut t = tenancy(
            None,
            Some(TenantScope::Owned(u(QONTINUI))),
            CredentialTenantRead::NotLive,
            BindingTenantRead::Unknown,
        );
        t.credential.status = "resolved".into();
        t.credential.tenant_id = None;
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.source.as_deref(), Some("data_plane"));
        assert_eq!(d.reason, None);
    }

    #[test]
    fn a_stamp_alone_is_stamp_only_and_carries_why_nothing_else_resolved() {
        let t = tenancy(
            Some(QONTINUI),
            None,
            CredentialTenantRead::DeliveryUnrecorded,
            BindingTenantRead::Unknown,
        );
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "stamp_only");
        assert_eq!(d.source.as_deref(), Some("row_stamp"));
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
        assert_eq!(d.reason.as_deref(), Some("coord_mcp_delivery_unrecorded"));
    }

    // ---- every credential error arm reaches `reason` -----------------------

    #[test]
    fn every_credential_error_reason_propagates_into_an_unknown_display() {
        let refusal = crate::coord_mcp::ProxyRefusal {
            status: 403,
            code: "COORD_MCP_PROXY_TENANT_UNRESOLVABLE",
            retryable: false,
            message: "no tenant".to_string(),
        };
        for (credential, binding, want) in [
            (
                CredentialTenantRead::NoNonce,
                BindingTenantRead::Unknown,
                "no_session_nonce",
            ),
            (
                CredentialTenantRead::NotLive,
                BindingTenantRead::Unknown,
                "nonce_not_live",
            ),
            (
                CredentialTenantRead::DeliveryUnrecorded,
                BindingTenantRead::Unknown,
                "coord_mcp_delivery_unrecorded",
            ),
            (
                CredentialTenantRead::Refused(refusal),
                BindingTenantRead::Unknown,
                "COORD_MCP_PROXY_TENANT_UNRESOLVABLE: no tenant",
            ),
            (
                CredentialTenantRead::Resolved(None),
                BindingTenantRead::Unknown,
                "default_slot_binding_unknown",
            ),
            (
                CredentialTenantRead::Resolved(None),
                BindingTenantRead::Unbound,
                "default_slot_binding_unbound",
            ),
        ] {
            // No data plane, no stamp, and no recorded spawn default.
            let t = project_tenancy(
                None,
                Some(TenantScope::Unresolved),
                &credential,
                binding,
                SpawnDefaultRead::NotRecorded("not_recorded"),
                None,
                None,
            );
            let d = project_tenant_display(&t, four_bindings(), no_slug);
            assert_eq!(d.status, "unknown", "{want}");
            assert_eq!(d.tenant_id, None, "{want}");
            assert_eq!(d.source, None, "{want}");
            assert_eq!(d.reason.as_deref(), Some(want));
        }
    }

    #[test]
    fn the_data_plane_reason_is_used_when_the_credential_gave_none() {
        // A credential with no reason cannot be unknown, so this exercises the
        // fallback by building the block directly from an unknown credential
        // whose reason was cleared.
        let mut t = project_tenancy(
            None,
            None,
            &CredentialTenantRead::NoNonce,
            BindingTenantRead::Unknown,
            SpawnDefaultRead::NotRecorded("not_recorded"),
            None,
            None,
        );
        t.credential.reason = None;
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.reason.as_deref(), Some("no_coord_session"));
        t.data_plane.reason = None;
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.reason.as_deref(), Some("no tenancy leg resolved"));
    }

    // ---- mismatch ----------------------------------------------------------

    #[test]
    fn credential_and_data_plane_disagreeing_is_a_mismatch_naming_both() {
        let t = tenancy(
            None,
            Some(TenantScope::Owned(u(BAUHAUS))),
            CredentialTenantRead::Resolved(Some(u(QONTINUI))),
            BindingTenantRead::Unknown,
        );
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "mismatch");
        // The acting tenant is still named, beside the conflict.
        assert_eq!(d.tenant_id.as_deref(), Some(QONTINUI));
        assert_eq!(d.source.as_deref(), Some("credential"));
        assert_eq!(
            d.conflicts,
            vec![
                TenantConflict {
                    leg: "credential".into(),
                    tenant_id: QONTINUI.into()
                },
                TenantConflict {
                    leg: "data_plane".into(),
                    tenant_id: BAUHAUS.into()
                },
            ]
        );
    }

    #[test]
    fn the_verdict_only_mismatch_arm_lists_conflicts_and_the_detail() {
        // One id across every leg, but coord was OBSERVED answering from
        // another tenant: `diverged` by the verdict alone.
        let t = regression_tenancy().with_repo_expected(TenancyRepoExpected::from_read(
            &crate::coord_mcp::RepoExpectationRead::Known {
                cwd: Some(qontinui_runner_lib::repo_tenant::CwdTenant::NoRepo),
                caller_named: None,
                latest: Some(crate::coord_mcp_tenant::VerdictRecord {
                    verdict: "mismatch".into(),
                    detail: Some("answered by pizzeria, expected qontinui".into()),
                    answered_tenant_id: Some(PIZZERIA.into()),
                    answered_tenant_slug: Some("pizzeria".into()),
                    observed_at: "2026-10-17T00:00:00Z".into(),
                }),
            },
        ));
        assert!(t.diverged);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "mismatch");
        assert_eq!(
            d.conflict_detail.as_deref(),
            Some("answered by pizzeria, expected qontinui")
        );
        assert_eq!(
            d.conflicts
                .iter()
                .map(|c| c.leg.as_str())
                .collect::<Vec<_>>(),
            ["credential", "data_plane"]
        );
    }

    #[test]
    fn a_mismatch_with_no_acting_tenant_still_says_why() {
        // Nothing names an acting tenant (no nonce, an unresolved data plane,
        // no stamp), yet the spawn-time default and coord's row disagree. The
        // display is a mismatch AND carries the reason no tenant was picked.
        let t = tenancy(
            None,
            Some(TenantScope::Unresolved),
            CredentialTenantRead::NoNonce,
            BindingTenantRead::Unknown,
        )
        .with_coord_row(TenancyCoordRow::from_read(CoordRowRead::Registered(u(
            PIZZERIA,
        ))));
        assert!(t.diverged);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "mismatch");
        assert_eq!(d.tenant_id, None);
        assert_eq!(d.source, None);
        assert_eq!(d.reason.as_deref(), Some("no_session_nonce"));
        assert_eq!(
            d.conflicts,
            vec![
                TenantConflict {
                    leg: "spawn_device_default".into(),
                    tenant_id: QONTINUI.into()
                },
                TenantConflict {
                    leg: "coord_row".into(),
                    tenant_id: PIZZERIA.into()
                },
            ]
        );
    }

    #[test]
    fn a_several_owner_repo_lists_every_owner_in_the_conflict() {
        let t = regression_tenancy().with_repo_expected(TenancyRepoExpected::from_read(
            &crate::coord_mcp::RepoExpectationRead::Known {
                cwd: Some(qontinui_runner_lib::repo_tenant::CwdTenant::Several {
                    repo: "acme/x".into(),
                    tenant_ids: vec![u(MERYTS), u(BAUHAUS)],
                }),
                caller_named: None,
                latest: None,
            },
        ));
        assert!(t.diverged);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        let repo: Vec<&str> = d
            .conflicts
            .iter()
            .filter(|c| c.leg == "repo_expected")
            .map(|c| c.tenant_id.as_str())
            .collect();
        assert_eq!(repo, [MERYTS, BAUHAUS]);
    }

    #[test]
    fn a_non_diverged_block_has_no_conflicts_even_with_a_verdict_detail() {
        let mut t = regression_tenancy();
        t.repo_expected.verdict_detail = Some("ok".into());
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert!(d.conflicts.is_empty());
        assert_eq!(d.conflict_detail, None);
    }

    // ---- slug -------------------------------------------------------------

    fn answer(tenant: &str, slug: &str) -> crate::coord_mcp_tenant::VerdictRecord {
        crate::coord_mcp_tenant::VerdictRecord {
            verdict: "agree".into(),
            detail: None,
            answered_tenant_id: Some(tenant.into()),
            answered_tenant_slug: Some(slug.into()),
            observed_at: "2026-10-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_slug_of_the_acting_tenant_is_shown() {
        let a = answer(QONTINUI, "qontinui");
        let d = project_tenant_display(&regression_tenancy(), four_bindings(), |t| {
            slug_from_answer(Some(&a), t)
        });
        assert_eq!(d.tenant_slug.as_deref(), Some("qontinui"));
    }

    #[test]
    fn a_slug_for_the_wrong_tenant_is_dropped() {
        let a = answer(PIZZERIA, "pizzeria");
        let d = project_tenant_display(&regression_tenancy(), four_bindings(), |t| {
            slug_from_answer(Some(&a), t)
        });
        assert_eq!(d.tenant_slug, None);
        assert_eq!(slug_from_answer(None, QONTINUI), None);
    }

    // ---- Phase 4: coord's own row tenant -----------------------------------

    #[test]
    fn a_registered_coord_row_on_the_acting_tenant_agrees() {
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(
            CoordRowRead::Registered(u(QONTINUI)),
        ));
        assert_eq!(t.coord_row.status, "registered");
        assert_eq!(t.coord_row.source.as_deref(), Some("registrar"));
        assert_eq!(t.divergence, DIVERGENCE_AGREE);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
    }

    #[test]
    fn a_registered_coord_row_on_another_tenant_is_a_mismatch() {
        // The merge-train-oracle shape: the credential acts as qontinui while
        // coord's row — what messages and the census route by — is pizzeria's.
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(
            CoordRowRead::Registered(u(PIZZERIA)),
        ));
        assert!(t.diverged);
        assert_eq!(t.divergence, DIVERGENCE_DIVERGED);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "mismatch");
        assert_eq!(
            d.conflicts.last(),
            Some(&TenantConflict {
                leg: "coord_row".into(),
                tenant_id: PIZZERIA.into()
            })
        );
    }

    #[test]
    fn a_terminal_plane_session_is_not_applicable_and_keeps_agreement() {
        // The 2026-10-17 population: a terminal-hosted session whose coord row
        // is the hosting terminal's own (SessionRegistry's). With no registrar
        // entry the pinned terminal names that row; it must not flip
        // divergence from agree to unknown.
        let read = coord_row_from_registrar(
            row(TERMINAL_ROW, RecordedRowOrigin::TerminalPinned),
            Some(TERMINAL_ROW),
            true,
            TerminalRowTenant::Observed,
        );
        assert_eq!(read, CoordRowRead::NotApplicable(COORD_ROW_TERMINAL_PLANE));
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(read));
        assert_eq!(t.coord_row.status, COORD_ROW_NOT_APPLICABLE);
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some("terminal_plane_row_is_the_data_plane_leg")
        );
        assert_eq!(t.coord_row.tenant_id, None);
        assert!(!t.diverged);
        assert_eq!(t.divergence, DIVERGENCE_AGREE);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn an_adopted_terminal_row_is_decided_by_coord_id_and_keeps_agreement() {
        // B1: transcript auto-bind ADOPTS the terminal's own SessionRegistry
        // row into the registrar for every tailed transcript. That entry is
        // `adopted`, but it is the hosting terminal's row by coord id, so the
        // leg is not applicable — it must not read
        // unknown(adopted_row_tenant_not_observed).
        let t = with_terminal_row(regression_tenancy(), adopted_terminal_row);
        assert_eq!(t.coord_row.status, COORD_ROW_NOT_APPLICABLE);
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some(COORD_ROW_TERMINAL_PLANE)
        );
        assert_eq!(t.divergence, DIVERGENCE_AGREE);
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
    }

    #[test]
    fn a_resumed_pane_whose_row_tenant_is_unconfirmed_is_unknown_not_agree() {
        // A pane restored after a runner restart goes through
        // `resume_external`, which re-stamps the row's tenant from TODAY's
        // resolution. Created under A, now resolving B, the data plane says B
        // while coord's row is A — so the leg must not stand aside.
        for origin in [
            RecordedRowOrigin::TerminalPinned,
            RecordedRowOrigin::Adopted,
        ] {
            assert_eq!(
                coord_row_from_registrar(
                    row(TERMINAL_ROW, origin),
                    Some(TERMINAL_ROW),
                    true,
                    TerminalRowTenant::ResumeUnconfirmed,
                ),
                CoordRowRead::Unknown(COORD_ROW_TERMINAL_ROW_RESUME_UNCONFIRMED)
            );
        }
        let t = with_terminal_row(regression_tenancy(), resumed_unconfirmed_terminal_row);
        assert_eq!(t.coord_row.status, TENANCY_UNKNOWN);
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some("terminal_row_resume_unconfirmed")
        );
        assert!(!t.diverged);
        assert_eq!(t.divergence, "unknown");
        // The acting tenant is unaffected: the credential still answers.
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");

        // The confirmed counterpart — a row this process registered fresh —
        // stands aside and keeps agreement.
        let t = with_terminal_row(regression_tenancy(), terminal_plane_row);
        assert_eq!(t.coord_row.status, COORD_ROW_NOT_APPLICABLE);
        assert_eq!(t.divergence, DIVERGENCE_AGREE);
    }

    #[test]
    fn the_terminal_row_tenant_is_observed_only_when_the_registry_says_so() {
        use TerminalRowTenant::*;
        assert_eq!(TerminalRowTenant::from_reads(false, Some(true)), Unresolved);
        assert_eq!(TerminalRowTenant::from_reads(false, None), Unresolved);
        assert_eq!(TerminalRowTenant::from_reads(true, Some(true)), Observed);
        assert_eq!(
            TerminalRowTenant::from_reads(true, Some(false)),
            ResumeUnconfirmed
        );
        // The registry not holding the row is not an observation.
        assert_eq!(TerminalRowTenant::from_reads(true, None), ResumeUnconfirmed);
    }

    #[test]
    fn a_clear_successor_resolving_to_the_terminal_row_is_not_applicable() {
        // W2: a `/clear` successor's id is not the pin, but its adoption chain
        // resolves to the pinned terminal's row — the same coord id the hosting
        // terminal carries. Not a registrar-plane miss.
        let read = coord_row_from_registrar(
            row(TERMINAL_ROW, RecordedRowOrigin::TerminalPinned),
            Some(TERMINAL_ROW),
            true,
            TerminalRowTenant::Observed,
        );
        assert_eq!(read, CoordRowRead::NotApplicable(COORD_ROW_TERMINAL_PLANE));
        // Its own later mint (a registrar row of its own) answers for itself.
        assert_eq!(
            coord_row_from_registrar(
                row(
                    OTHER_ROW,
                    RecordedRowOrigin::Minted {
                        tenant: Some(u(QONTINUI))
                    }
                ),
                Some(TERMINAL_ROW),
                true,
                TerminalRowTenant::Observed,
            ),
            CoordRowRead::Registered(u(QONTINUI))
        );
    }

    #[test]
    fn the_terminal_row_stands_aside_only_when_wired_and_its_tenant_resolved() {
        // W1: no coord id on the terminal — nothing read the row.
        let not_wired = coord_row_from_registrar(None, None, true, TerminalRowTenant::Unresolved);
        assert_eq!(
            not_wired,
            CoordRowRead::Unknown(COORD_ROW_TERMINAL_ROW_NOT_WIRED)
        );
        // The terminal's row, but the data plane named no tenant for it.
        for origin in [
            RecordedRowOrigin::TerminalPinned,
            RecordedRowOrigin::Adopted,
        ] {
            assert_eq!(
                coord_row_from_registrar(
                    row(TERMINAL_ROW, origin),
                    Some(TERMINAL_ROW),
                    true,
                    TerminalRowTenant::Unresolved
                ),
                CoordRowRead::Unknown(COORD_ROW_TERMINAL_ROW_TENANT_UNRESOLVED)
            );
        }
        // Either way `agree` cannot pass while coord's row is unobserved.
        for read in [
            not_wired,
            CoordRowRead::Unknown(COORD_ROW_TERMINAL_ROW_TENANT_UNRESOLVED),
        ] {
            let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(read));
            assert_eq!(t.coord_row.status, TENANCY_UNKNOWN);
            assert_eq!(t.divergence, "unknown");
        }
    }

    #[test]
    fn a_session_this_process_resolves_no_row_for_makes_divergence_unknown() {
        let read =
            coord_row_from_registrar(None, Some(TERMINAL_ROW), false, TerminalRowTenant::Observed);
        assert_eq!(read, CoordRowRead::Unknown(COORD_ROW_NOT_REGISTERED));
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(read));
        assert_eq!(t.coord_row.status, TENANCY_UNKNOWN);
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some("not_registered_by_this_process")
        );
        assert!(!t.diverged);
        assert_eq!(t.divergence, "unknown");
        // The acting tenant is unaffected: the credential still answers.
        let d = project_tenant_display(&t, four_bindings(), no_slug);
        assert_eq!(d.status, "resolved");
    }

    #[test]
    fn the_registrar_read_maps_each_arm_of_a_row_that_is_not_the_terminals() {
        let minted = |tenant| RecordedRowOrigin::Minted { tenant };
        for terminal_row in [None, Some(TERMINAL_ROW)] {
            for pinned_here in [false, true] {
                let read = |origin| {
                    coord_row_from_registrar(
                        row(OTHER_ROW, origin),
                        terminal_row,
                        pinned_here,
                        TerminalRowTenant::Observed,
                    )
                };
                assert_eq!(
                    read(minted(Some(u(QONTINUI)))),
                    CoordRowRead::Registered(u(QONTINUI))
                );
                assert_eq!(
                    read(minted(None)),
                    CoordRowRead::Unknown("registered_without_tenant")
                );
                assert_eq!(
                    read(RecordedRowOrigin::Adopted),
                    CoordRowRead::Unknown(COORD_ROW_ADOPTED)
                );
                assert_eq!(
                    read(RecordedRowOrigin::StatePoisoned),
                    CoordRowRead::Unknown(COORD_ROW_REGISTRAR_POISONED)
                );
                assert_eq!(
                    read(RecordedRowOrigin::TerminalPinned),
                    CoordRowRead::Unknown(COORD_ROW_OTHER_TERMINAL)
                );
            }
        }
    }

    #[test]
    fn an_adopted_row_that_is_not_the_terminals_reports_its_tenant_as_not_observed() {
        // The registrar holds the CALLER's tenant for an adopted row, not the
        // tenant coord created it under: unless it is the hosting terminal's
        // own row, it is unknown.
        let read = coord_row_from_registrar(
            row(OTHER_ROW, RecordedRowOrigin::Adopted),
            Some(TERMINAL_ROW),
            false,
            TerminalRowTenant::Observed,
        );
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(read));
        assert_eq!(t.coord_row.status, TENANCY_UNKNOWN);
        assert_eq!(t.coord_row.tenant_id, None);
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some("adopted_row_tenant_not_observed")
        );
        assert_eq!(t.divergence, "unknown");
    }

    #[test]
    fn a_poisoned_registrar_state_is_unknown_not_adopted() {
        let read = coord_row_from_registrar(
            row(OTHER_ROW, RecordedRowOrigin::StatePoisoned),
            Some(TERMINAL_ROW),
            false,
            TerminalRowTenant::Observed,
        );
        assert_eq!(read, CoordRowRead::Unknown(COORD_ROW_REGISTRAR_POISONED));
        let t = regression_tenancy().with_coord_row(TenancyCoordRow::from_read(read));
        assert_eq!(
            t.coord_row.reason.as_deref(),
            Some("registrar_state_poisoned")
        );
        assert_eq!(t.divergence, "unknown");
    }

    #[test]
    fn an_unknown_credential_without_a_reason_still_names_one() {
        // S1: a data-plane-sourced display beside an unknown credential that
        // gave no reason must not drop the credential context.
        let mut t = tenancy(
            None,
            Some(TenantScope::Owned(u(QONTINUI))),
            CredentialTenantRead::NotLive,
            BindingTenantRead::Unknown,
        );
        assert_eq!(t.credential.status, TENANCY_UNKNOWN);
        t.credential.reason = None;
        assert_eq!(
            credential_unknown_reason(&t).as_deref(),
            Some(CREDENTIAL_UNKNOWN_NO_REASON)
        );
    }

    #[test]
    fn a_projection_without_the_coord_row_leg_does_not_block_agreement() {
        // `not_read` is never served; it must not turn every older test block
        // into `unknown`.
        assert_eq!(regression_tenancy().coord_row.status, "not_read");
        assert_eq!(regression_tenancy().divergence, DIVERGENCE_AGREE);
    }

    #[test]
    fn attaching_a_leg_after_the_display_clears_the_stale_display() {
        let t = regression_tenancy().with_display(four_bindings(), no_slug);
        assert!(t.display.is_some());
        let t = t.with_coord_row(TenancyCoordRow::from_read(CoordRowRead::Registered(u(
            PIZZERIA,
        ))));
        assert!(t.display.is_none());
    }

    // ---- the TS contract fixture -------------------------------------------

    /// The fixture the vitest `useSessionInfo.test.ts` renders: each case's
    /// `tenancy` is exactly what this module serializes, so a Rust-side rename
    /// breaks HERE and a TS-side one breaks THERE.
    const DISPLAY_FIXTURE: &str =
        include_str!("../../../src/components/terminal/__fixtures__/session-tenancy-display.json");

    fn fixture_cases() -> Vec<(&'static str, SessionTenancy)> {
        let qontinui_answer = answer(QONTINUI, "qontinui");
        vec![
            (
                // A terminal-hosted session, whose coord row is
                // SessionRegistry's, so the coord-row leg is not applicable.
                "regression_2026_10_17",
                with_terminal_row(regression_tenancy(), terminal_plane_row)
                    .with_display(four_bindings(), |t| {
                        slug_from_answer(Some(&qontinui_answer), t)
                    }),
            ),
            (
                // The same session once transcript auto-bind ADOPTED the
                // terminal's own row into the registrar: still the terminal's
                // row by coord id, so still not applicable and still `agree`.
                "adopted_terminal_row",
                with_terminal_row(regression_tenancy(), adopted_terminal_row)
                    .with_display(four_bindings(), |t| {
                        slug_from_answer(Some(&qontinui_answer), t)
                    }),
            ),
            (
                // The same pane restored after a runner restart: its row's
                // tenant is today's re-stamp, never observed on coord's row,
                // so the leg is unknown and the comparison cannot agree.
                "resumed_unconfirmed_terminal_row",
                with_terminal_row(regression_tenancy(), resumed_unconfirmed_terminal_row)
                    .with_display(four_bindings(), |t| {
                        slug_from_answer(Some(&qontinui_answer), t)
                    }),
            ),
            (
                // An adopted row that is NOT the hosting terminal's: its
                // create-time tenant was never observed, so `unknown`.
                "adopted_non_terminal_row",
                regression_tenancy()
                    .with_coord_row(TenancyCoordRow::from_read(coord_row_from_registrar(
                        row(OTHER_ROW, RecordedRowOrigin::Adopted),
                        Some(TERMINAL_ROW),
                        false,
                        TerminalRowTenant::Observed,
                    )))
                    .with_display(four_bindings(), no_slug),
            ),
            (
                "coord_row_mismatch",
                regression_tenancy()
                    .with_coord_row(TenancyCoordRow::from_read(CoordRowRead::Registered(u(
                        PIZZERIA,
                    ))))
                    .with_display(four_bindings(), no_slug),
            ),
            (
                "unknown_no_nonce",
                project_tenancy(
                    None,
                    Some(TenantScope::Unresolved),
                    &CredentialTenantRead::NoNonce,
                    BindingTenantRead::Unknown,
                    SpawnDefaultRead::NotRecorded("not_recorded"),
                    None,
                    None,
                )
                .with_coord_row(TenancyCoordRow::from_read(CoordRowRead::Unknown(
                    COORD_ROW_NOT_REGISTERED,
                )))
                .with_display(BindingSetRead::Unknown("paired_user_unreadable"), no_slug),
            ),
            (
                "mismatch_no_acting_tenant",
                tenancy(
                    None,
                    Some(TenantScope::Unresolved),
                    CredentialTenantRead::NoNonce,
                    BindingTenantRead::Unknown,
                )
                .with_coord_row(TenancyCoordRow::from_read(CoordRowRead::Registered(u(
                    PIZZERIA,
                ))))
                .with_display(four_bindings(), no_slug),
            ),
            ("data_plane_credential_unknown", {
                let t = tenancy(
                    None,
                    Some(TenantScope::Owned(u(QONTINUI))),
                    CredentialTenantRead::NotLive,
                    BindingTenantRead::Unknown,
                );
                with_terminal_row(t, terminal_plane_row).with_display(four_bindings(), no_slug)
            }),
        ]
    }

    /// Regenerate with `REGEN_SESSION_TENANCY_DISPLAY_FIXTURE=1` set on this
    /// test's run; the test then rewrites the fixture and passes.
    #[test]
    fn the_ts_contract_fixture_is_what_this_module_serializes() {
        if std::env::var_os("REGEN_SESSION_TENANCY_DISPLAY_FIXTURE").is_some() {
            let cases: Vec<serde_json::Value> = fixture_cases()
                .into_iter()
                .map(|(name, t)| serde_json::json!({ "name": name, "tenancy": t }))
                .collect();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../src/components/terminal/__fixtures__/session-tenancy-display.json");
            let mut body = serde_json::to_string_pretty(&cases).unwrap();
            body.push('\n');
            std::fs::write(path, body).unwrap();
            return;
        }
        let fixture: serde_json::Value = serde_json::from_str(DISPLAY_FIXTURE).unwrap();
        let cases = fixture.as_array().expect("fixture is an array");
        let built = fixture_cases();
        assert_eq!(cases.len(), built.len());
        for (case, (name, tenancy)) in cases.iter().zip(built) {
            assert_eq!(case["name"], name);
            assert_eq!(
                case["tenancy"],
                serde_json::to_value(&tenancy).unwrap(),
                "fixture case {name} drifted from the Rust projection; regenerate it from \
                 this test's output"
            );
        }
    }
}
