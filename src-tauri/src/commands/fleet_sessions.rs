//! Fleet session DISCOVERY — "which Claude Code sessions exist on which machine".
//!
//! Plan `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 2. This is
//! the read the Terminal page's device picker is built on: before a tab can be
//! attached to a session on another fleet box, the operator has to be able to
//! see what is out there.
//!
//! Thin by design. It wraps coord's `GET /coord/sessions/fleet` and returns the
//! body as opaque JSON, exactly as [`crate::commands::claims`] does — the wire
//! shape is owned by coord and typed once on the TypeScript side, rather than
//! being restated in a Rust DTO that can drift out of step with it.
//!
//! ## Read-only, and it adds no capability
//!
//! coord's route is `FleetPrincipal`-gated, so the device JWT this runner
//! already holds is exactly the credential it takes. Nothing here mints,
//! escalates, or widens anything: there is no write path, and no keystroke path
//! — those are Phases 3-5, and they are gated on the authorization-grain work
//! this phase deliberately does not touch.
//!
//! ## An empty list is UNKNOWN, never "nobody is working"
//!
//! coord answers with three capability flags beside the rows
//! (`sessionBridgeColumnPresent`, `workAxisColumnsPresent`,
//! `deviceIdentityColumnsPresent`). They are passed through UNCHANGED and the UI
//! is required to read them: a `false` means that field is degraded, not
//! observed. Collapsing a degraded read into "no remote sessions" would be a
//! positive claim coord did not make.

use std::time::Duration;

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::TenantScope;
use crate::coord_mcp::{ProxyRefusal, SpawnTenantRefusal};

/// Filters the picker may apply. All optional — the default is "live sessions
/// across the whole tenant, most recently STARTED first". That is coord's walk
/// order, `started_at DESC, id DESC`, and not recency of activity: since
/// qontinui-coord#2085 the key a cursor walks must not move, and a heartbeat
/// moves `last_heartbeat_at` (every 15 s by default, `DEFAULT_HEARTBEAT_SECS`).
/// Activity is still on every row as `lastHeartbeatAt`; the picker orders each
/// device's rows by it (`groupByDevice`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSessionsArgs {
    /// Restrict to one device. Applied by coord in SQL.
    pub device_id: Option<String>,
    /// Restrict to one `coord.sessions.state`.
    pub state: Option<String>,
    /// Include sessions that have closed. Defaults to false — a picker offering
    /// an attach wants live sessions.
    #[serde(default)]
    pub include_closed: bool,
    /// Page size. coord clamps to its own ceiling and echoes the effective
    /// value back as `limit`.
    pub limit: Option<i64>,
    /// A `nextCursor` from a previous page of the SAME scope, passed back
    /// verbatim to advance the keyset walk.
    ///
    /// OPAQUE: never construct, parse or rewrite it here. coord fingerprints
    /// the scope (`device_id` / `state` / `include_closed`) into the token and
    /// answers `400 cursor_scope_mismatch` when a cursor is replayed under a
    /// different one, so this wrapper must forward the caller's string
    /// unchanged and let coord adjudicate. `limit` is deliberately NOT part of
    /// that fingerprint — resizing a page changes the slice, not the sequence.
    pub cursor: Option<String>,
    /// The tenant whose fleet to read, as a UUID string — the Fleet view's
    /// tenant selector (plan
    /// `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`).
    ///
    /// NOT a query parameter: coord's route takes no tenant argument and scopes
    /// every row to the PRINCIPAL's tenant, so "read tenant X" is spelled
    /// "present tenant X's credential". [`fleet_scope`] turns this into the
    /// [`TenantScope`] the request is authenticated under. Absent or blank ⇒
    /// the device's own authority order decides
    /// ([`crate::coord_mcp::session_tenant_or_refuse`] with no nonce), and
    /// that is a deliberate CHANGE from the pre-field read, which always
    /// presented the default slot ([`TenantScope::Device`]):
    ///
    /// - an unpinned machine (`machine.json` `Unpinned`, no admitted
    ///   `$QONTINUI_TENANT_ID`) still presents [`TenantScope::Device`] — the
    ///   pre-field behaviour, unchanged;
    /// - a PINNED machine (or an admitted `$QONTINUI_TENANT_ID`) now presents
    ///   [`TenantScope::Owned`] for that tenant, so the read lists the pinned
    ///   tenant's fleet rather than whatever the default slot holds;
    /// - a pin that cannot be honoured — an env tenant that fails admission,
    ///   or an unreadable pin with no `tenant_id` claim in the device JWT to
    ///   fall back on — REFUSES with a typed `fleet_sessions:<code>` error and
    ///   sends no request, where the pre-field read would have silently
    ///   presented the default slot.
    ///
    /// The attach and create mints resolve "no tenant" by the same rule
    /// ([`fleet_scope`]), so a row listed under the pin is minted under it.
    ///
    /// coord fingerprints the tenant into a page cursor, so a cursor must be
    /// replayed under the SAME tenant it was minted under — the frontend keys
    /// its walk on this field for that reason.
    #[serde(default)]
    pub tenant: Option<String>,
}

/// Per-request deadline. Discovery is a foreground read behind a picker, so a
/// slow coord must surface as an error the UI can retry rather than a spinner
/// that never resolves.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Build the request URL for a filter set.
///
/// Pure and separate from the command so the filter rules are testable without
/// a live coord — the same discipline coord's own `build_fleet_sql` uses on the
/// other side of this call. Testing a re-implementation of these rules instead
/// would pass happily while the real path diverged, which is what an earlier
/// revision of this module's tests actually did.
///
/// Blank and whitespace-only filters are DROPPED rather than sent: `device_id=`
/// would make coord match the empty string and return nothing, which the picker
/// would then render as "no sessions" — an absence manufactured by a typo.
fn build_fleet_url(base: &str, args: &FleetSessionsArgs) -> String {
    let base = base.trim_end_matches('/');
    let mut query: Vec<(&str, String)> = Vec::new();

    if let Some(d) = args.device_id.as_deref() {
        let d = d.trim();
        if !d.is_empty() {
            query.push(("device_id", d.to_string()));
        }
    }
    if let Some(s) = args.state.as_deref() {
        let s = s.trim();
        if !s.is_empty() {
            query.push(("state", s.to_string()));
        }
    }
    if args.include_closed {
        query.push(("include_closed", "true".to_string()));
    }
    if let Some(l) = args.limit {
        query.push(("limit", l.to_string()));
    }
    if let Some(c) = args.cursor.as_deref() {
        let c = c.trim();
        if !c.is_empty() {
            query.push(("cursor", c.to_string()));
        }
    }

    let url = format!("{base}/coord/sessions/fleet");
    if query.is_empty() {
        return url;
    }
    let qs = query
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{url}?{qs}")
}

/// Which tenant's credential the fleet read presents — the ONE place the Fleet
/// view's tenant is decided, run BEFORE any request is built.
///
/// - **An explicit tenant** wins for THIS READ only. It must parse as a UUID and
///   be admitted by [`crate::coord_mcp::validate_spawn_tenant`] — the rule
///   spawns already use for "may this runner act as that tenant?" — and a
///   refusal is a typed error, never a fallback to another tenant: a fallback
///   would answer with the wrong tenant's sessions under the asked tenant's
///   label. It moves no session's tenant and writes no pin; it is a
///   session-less, device-level read.
/// - **No tenant** consults [`crate::coord_mcp::session_tenant_or_refuse`] with
///   no nonce — the authority order, not a second copy of it: `Some(t)` ⇒
///   `Owned(t)`, `None` ⇒ `Device` (the default slot, which is the
///   single-tenant / unpinned operator's normal state), a refusal ⇒ this error.
///
/// The attach- and create-grant mints resolve their tenant through THIS
/// function too, so "no tenant" means the same tenant on the read that listed a
/// row and on the mint that opens it — a probe that lists under the pin and
/// mints under the default slot would 404 every non-default row.
pub(crate) fn fleet_scope(arg: Option<&str>) -> Result<TenantScope, String> {
    fleet_scope_with(arg, crate::coord_mcp::validate_spawn_tenant, || {
        crate::coord_mcp::session_tenant_or_refuse(None)
    })
}

/// The tenant a resolved scope names, for a tab's reattach: `Owned(t)` ⇒ `t`;
/// `Device` (and `Unresolved`, which [`fleet_scope`] never returns) ⇒ `None`,
/// i.e. "let the authority order decide again".
pub(crate) fn scope_tenant(scope: TenantScope) -> Option<String> {
    match scope {
        TenantScope::Owned(t) => Some(t.to_string()),
        TenantScope::Device | TenantScope::Unresolved => None,
    }
}

/// Pure-over-injected-parts core of [`fleet_scope`], so the admission rule and
/// the default arm are unit-testable without a credential store. `admit` and
/// `default_authority` are each called at most once, and only on their arm.
fn fleet_scope_with(
    arg: Option<&str>,
    admit: impl FnOnce(Uuid) -> Result<(), SpawnTenantRefusal>,
    default_authority: impl FnOnce() -> Result<Option<Uuid>, ProxyRefusal>,
) -> Result<TenantScope, String> {
    match arg.map(str::trim).filter(|t| !t.is_empty()) {
        Some(raw) => {
            let tenant = Uuid::parse_str(raw).map_err(|e| {
                format!(
                    "fleet_sessions:tenant_invalid: {raw:?} is not a tenant uuid ({e}) — no \
                     request was sent"
                )
            })?;
            admit(tenant).map_err(|refusal| {
                format!(
                    "fleet_sessions:tenant_refused: {refusal} (no request was sent; no other \
                     tenant's credential is substituted)"
                )
            })?;
            Ok(TenantScope::Owned(tenant))
        }
        None => match default_authority() {
            Ok(Some(tenant)) => Ok(TenantScope::Owned(tenant)),
            Ok(None) => Ok(TenantScope::Device),
            Err(refusal) => Err(format!(
                "fleet_sessions:{}: {} (no request was sent)",
                refusal.code, refusal.message
            )),
        },
    }
}

/// `fleet_sessions_list` — wrapper around coord's
/// `GET /coord/sessions/fleet`.
///
/// Returns coord's response body verbatim as JSON. Transport and non-200
/// statuses become `Err(String)` for the React layer's `.catch`, with the status
/// and body included: a picker that cannot tell "coord said no" from "coord did
/// not answer" is the failure this phase's UNKNOWN discipline exists to prevent.
#[tauri::command]
pub async fn fleet_sessions_list(args: FleetSessionsArgs) -> Result<serde_json::Value, String> {
    // Decided first, so a malformed or unbound tenant is refused before any
    // request exists — never sent anonymously, never sent as another tenant.
    // Off the async runtime: the resolver reads machine.json and the credential
    // store, as the coord-mcp proxy's own call of it does.
    let tenant = args.tenant.clone();
    let scope = spawn_blocking_tracked(move || fleet_scope(tenant.as_deref()))
        .await
        .map_err(|e| format!("fleet_sessions:tenant_resolution_failed: {e}"))??;
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let url = build_fleet_url(&base, &args);

    let client =
        crate::coord_http::coord_client().ok_or_else(|| "build http client".to_string())?;

    let resp = crate::coord_http::coord_get_for(client, &url, scope)
        .timeout(DISCOVERY_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;

    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| format!("read /coord/sessions/fleet body: {e}"))?;

    if status.is_success() {
        serde_json::from_str::<serde_json::Value>(&body_text)
            .map_err(|e| format!("parse /coord/sessions/fleet body: {e} (raw: {body_text})"))
    } else {
        // 401/403 here means "this runner is not paired, or its device JWT has
        // expired" — a credential answer, not an empty fleet. Surfaced with the
        // status so the UI can say which.
        Err(format!(
            "GET /coord/sessions/fleet returned {} — body: {body_text}",
            status.as_u16()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://coord.example.test";

    /// Defaults must not smuggle filters into the query string: an unfiltered
    /// call is what the picker's first paint makes, and a stray `state=` would
    /// silently hide sessions.
    #[test]
    fn default_args_produce_a_bare_url() {
        let url = build_fleet_url(BASE, &FleetSessionsArgs::default());
        assert_eq!(url, "https://coord.example.test/coord/sessions/fleet");
    }

    /// Blank strings are filters the caller did not mean. Sending `device_id=`
    /// would make coord match the empty string and return nothing, which the UI
    /// would then render as "no sessions" — an absence manufactured by a typo.
    ///
    /// This asserts against the REAL url builder, not a restatement of its
    /// rules: the earlier version of this test re-implemented the predicate and
    /// would have passed even if the command stopped trimming.
    #[test]
    fn blank_filters_are_dropped_not_sent() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                device_id: Some("   ".to_string()),
                state: Some(String::new()),
                include_closed: false,
                limit: None,
                cursor: None,
                tenant: None,
            },
        );
        assert_eq!(url, "https://coord.example.test/coord/sessions/fleet");
        assert!(!url.contains("device_id"));
        assert!(!url.contains("state"));
    }

    /// Real filters ARE sent, and are trimmed on the way.
    #[test]
    fn real_filters_are_sent_trimmed() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                device_id: Some("  abc-123  ".to_string()),
                state: Some("working".to_string()),
                include_closed: true,
                limit: Some(25),
                cursor: None,
                tenant: None,
            },
        );
        assert!(url.contains("device_id=abc-123"));
        assert!(url.contains("state=working"));
        assert!(url.contains("include_closed=true"));
        assert!(url.contains("limit=25"));
    }

    /// A filter value is URL-ENCODED, so a value carrying `&` or `=` cannot
    /// inject a second parameter.
    #[test]
    fn filter_values_are_encoded() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                state: Some("a&limit=9999".to_string()),
                ..Default::default()
            },
        );
        assert!(url.contains("state=a%26limit%3D9999"));
        // exactly one parameter reached the query string
        assert_eq!(url.matches('&').count(), 0);
    }

    /// A trailing slash on the coord base must not produce a double slash —
    /// `profiles::coord_base_with_source` may return either form.
    #[test]
    fn trailing_slash_on_base_is_normalised() {
        let url = build_fleet_url("https://coord.example.test/", &FleetSessionsArgs::default());
        assert_eq!(url, "https://coord.example.test/coord/sessions/fleet");
    }

    /// The keyset walk is only reachable if this wrapper FORWARDS the cursor.
    ///
    /// It did not, until 2026-09-11. `FleetSessionsArgs` had no `cursor` field
    /// and `FleetSessionsArgs` carries no `deny_unknown_fields`, so serde
    /// silently DROPPED the key the picker was sending: the request succeeded,
    /// coord served page one again, and the walk could never advance. A silent
    /// drop is the worst shape available here — the UI has every reason to
    /// believe it paged.
    #[test]
    fn a_cursor_is_forwarded_so_the_walk_can_advance() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                cursor: Some("Q3Vyc29yLXYx".to_string()),
                ..Default::default()
            },
        );
        assert!(
            url.contains("cursor=Q3Vyc29yLXYx"),
            "the cursor must reach coord or the walk silently repeats page one: {url}"
        );
    }

    /// The token is OPAQUE, so it is forwarded byte-for-byte apart from the
    /// trim every other filter gets. base64url can carry `-` and `_`, and
    /// percent-encoding must not mangle a token coord will compare exactly.
    #[test]
    fn a_cursor_is_forwarded_verbatim_not_rewritten() {
        // The fixture is deliberately low-entropy, obviously synthetic, and NOT
        // bound to a name in the secret family. Two separate gitleaks rules
        // fire on the obvious spellings: the default JWT rule on anything
        // `eyJ`-prefixed (a cursor is base64url of JSON, so a realistic one is
        // byte-indistinguishable from a JWT), and `generic-api-key` on a
        // high-entropy literal assigned to `token` / `key` / `secret` — which
        // also base64-DECODES the value and re-fires on the plaintext. Gitleaks
        // scans commit history, so either one fails the branch permanently.
        // It still carries `-` and `_`, the base64url property this test pins.
        let cursor_fixture = "cursor-page-2_of-3";
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                cursor: Some(format!("  {cursor_fixture}  ")),
                ..Default::default()
            },
        );
        assert!(
            url.contains(&format!("cursor={cursor_fixture}")),
            "cursor must survive the round trip unaltered: {url}"
        );
    }

    /// A blank cursor is page one, not a filter. Sending `cursor=` would make
    /// coord adjudicate an empty token rather than start a fresh walk.
    #[test]
    fn a_blank_cursor_is_dropped_not_sent() {
        for blank in ["", "   "] {
            let url = build_fleet_url(
                BASE,
                &FleetSessionsArgs {
                    cursor: Some(blank.to_string()),
                    ..Default::default()
                },
            );
            assert_eq!(url, "https://coord.example.test/coord/sessions/fleet");
            assert!(!url.contains("cursor"));
        }
    }

    /// A cursor rides ALONGSIDE its scope, never instead of it: coord
    /// fingerprints `device_id` / `state` / `include_closed` into the token and
    /// answers `400 cursor_scope_mismatch` if they disagree, so dropping one on
    /// an advance would break every walk that has a filter applied.
    #[test]
    fn a_cursor_does_not_displace_the_scope_it_was_minted_under() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                device_id: Some("dev-1".to_string()),
                state: Some("active".to_string()),
                include_closed: true,
                limit: Some(100),
                cursor: Some("tok".to_string()),
                tenant: None,
            },
        );
        for expected in [
            "device_id=dev-1",
            "state=active",
            "include_closed=true",
            "limit=100",
            "cursor=tok",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
    }

    // ---- Tenant selection (plan 2026-09-29-fleet-view-reads-one-unchosen-tenant…, Phase 2) ----

    const TENANT: &str = "c231d9da-0000-4000-8000-000000000001";

    fn tenant() -> Uuid {
        Uuid::parse_str(TENANT).unwrap()
    }

    fn admit_ok(_: Uuid) -> Result<(), SpawnTenantRefusal> {
        Ok(())
    }

    fn default_unreachable() -> Result<Option<Uuid>, ProxyRefusal> {
        panic!("an explicit tenant must never consult the default authority")
    }

    /// coord's route takes NO tenant argument (`FleetSessionsQuery` has none,
    /// and the tenant is positional `$1` from the principal), so the tenant
    /// must never leak into the query string — it travels as the credential.
    #[test]
    fn the_tenant_is_never_a_query_parameter() {
        let url = build_fleet_url(
            BASE,
            &FleetSessionsArgs {
                tenant: Some(TENANT.to_string()),
                ..Default::default()
            },
        );
        assert_eq!(url, "https://coord.example.test/coord/sessions/fleet");
        assert!(!url.contains("tenant"));
    }

    /// The picker sends `tenant` in the invoke args; a silent serde drop here is
    /// the cursor bug of 2026-09-11 over again (the read succeeds, under the
    /// wrong tenant).
    #[test]
    fn the_tenant_arg_deserializes_from_the_invoke_payload() {
        let args: FleetSessionsArgs = serde_json::from_value(serde_json::json!({
            "deviceId": "dev-1",
            "tenant": TENANT,
        }))
        .unwrap();
        assert_eq!(args.tenant.as_deref(), Some(TENANT));
        let absent: FleetSessionsArgs = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(absent.tenant, None);
    }

    /// An explicit, admitted tenant is presented as THAT tenant's credential.
    #[test]
    fn an_admitted_tenant_is_owned() {
        let scope = fleet_scope_with(Some(TENANT), admit_ok, default_unreachable).unwrap();
        assert_eq!(scope, TenantScope::Owned(tenant()));
        // Trimmed like every other filter.
        let padded = format!("  {TENANT}  ");
        let scope = fleet_scope_with(Some(&padded), admit_ok, default_unreachable).unwrap();
        assert_eq!(scope, TenantScope::Owned(tenant()));
    }

    /// A malformed tenant is refused before admission is even asked, and before
    /// any request: the refusal is typed and says nothing was sent.
    #[test]
    fn a_malformed_tenant_is_refused_before_any_request() {
        let err = fleet_scope_with(
            Some("not-a-uuid"),
            |_| panic!("a malformed tenant must not reach admission"),
            default_unreachable,
        )
        .unwrap_err();
        assert!(err.starts_with("fleet_sessions:tenant_invalid:"), "{err}");
        assert!(err.contains("no request was sent"), "{err}");
    }

    /// An UNBOUND tenant is refused — never downgraded to the default slot,
    /// which would list another tenant's sessions under this tenant's label.
    #[test]
    fn an_unbound_tenant_is_refused_not_downgraded() {
        for refusal in [
            SpawnTenantRefusal::NotPaired { tenant: tenant() },
            SpawnTenantRefusal::CredentialStoreUnreadable {
                tenant: tenant(),
                error: "io".to_string(),
            },
        ] {
            let code = refusal.code();
            let err =
                fleet_scope_with(Some(TENANT), |_| Err(refusal), default_unreachable).unwrap_err();
            assert!(err.starts_with("fleet_sessions:tenant_refused:"), "{err}");
            assert!(err.contains(code), "the admission code must survive: {err}");
        }
    }

    /// No tenant ⇒ the device's ONE authority order, arm for arm.
    #[test]
    fn no_tenant_follows_the_session_authority_order() {
        let never_admit = |_| -> Result<(), SpawnTenantRefusal> {
            panic!("the default arm is admitted by the authority itself")
        };
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(
                fleet_scope_with(blank, never_admit, || Ok(Some(tenant()))).unwrap(),
                TenantScope::Owned(tenant())
            );
            assert_eq!(
                fleet_scope_with(blank, never_admit, || Ok(None)).unwrap(),
                TenantScope::Device,
                "an unpinned single-tenant device keeps today's default slot"
            );
            let err = fleet_scope_with(blank, never_admit, || {
                Err(ProxyRefusal {
                    status: 503,
                    code: "COORD_MCP_PROXY_TENANT_UNRESOLVABLE",
                    retryable: false,
                    message: "machine.json is unreadable".to_string(),
                })
            })
            .unwrap_err();
            assert!(
                err.contains("COORD_MCP_PROXY_TENANT_UNRESOLVABLE")
                    && err.contains("machine.json is unreadable"),
                "{err}"
            );
        }
    }

    /// The REAL admission, against an isolated store holding no credential for
    /// the tenant: refused. Never touches the operator's `~/.qontinui`.
    #[test]
    fn the_real_admission_refuses_a_tenant_this_device_does_not_hold() {
        let _amb = crate::test_env::isolated_ambient();
        // The fixture restores this on drop; it keeps the legacy-slot read off
        // the operator's real OS keychain.
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        let err = fleet_scope(Some(TENANT)).unwrap_err();
        assert!(err.starts_with("fleet_sessions:tenant_refused:"), "{err}");
        let err = fleet_scope(Some("zzz")).unwrap_err();
        assert!(err.starts_with("fleet_sessions:tenant_invalid:"), "{err}");
    }

    /// The body of `fleet_sessions_list`, for the source guard below.
    fn fleet_sessions_list_body() -> &'static str {
        let src = include_str!("fleet_sessions.rs");
        // Assembled at runtime: a literal here would appear in `src` itself.
        let needle = format!("pub async fn {}(", "fleet_sessions_list");
        assert_eq!(src.matches(needle.as_str()).count(), 1);
        let (_, after) = src.split_once(needle.as_str()).unwrap();
        after.split_once("\n}\n").unwrap().0
    }

    /// SOURCE GUARD — the read states its tenant. The defect this plan fixes was
    /// a CALL-SITE choice: `coord_get` asserts `TenantScope::Device`, so the
    /// view showed whichever tenant the default slot named. The cross-tenant
    /// slot-miss posture beneath `coord_get_for` is pinned separately by
    /// `auth::unknown_tenant_slot_miss_sends_unauthenticated`.
    #[test]
    fn fleet_sessions_list_presents_the_tenant_it_reads() {
        let body = fleet_sessions_list_body();
        let scope_at = body
            .find("fleet_scope(")
            .unwrap_or_else(|| panic!("the read must decide its tenant via fleet_scope:\n{body}"));
        let get_at = body
            .find("coord_get_for(")
            .unwrap_or_else(|| panic!("the read must use the tenant-stating seam:\n{body}"));
        assert!(
            scope_at < get_at,
            "the tenant must be decided BEFORE the request is built:\n{body}"
        );
        assert!(
            !body.contains("coord_http::coord_get("),
            "the defaulting coord_get presents the default slot whatever tenant was asked:\n{body}"
        );
    }
}
