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

use serde::Deserialize;

/// Filters the picker may apply. All optional — the default is "live sessions
/// across the whole tenant, newest activity first".
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

/// `fleet_sessions_list` — wrapper around coord's
/// `GET /coord/sessions/fleet`.
///
/// Returns coord's response body verbatim as JSON. Transport and non-200
/// statuses become `Err(String)` for the React layer's `.catch`, with the status
/// and body included: a picker that cannot tell "coord said no" from "coord did
/// not answer" is the failure this phase's UNKNOWN discipline exists to prevent.
#[tauri::command]
pub async fn fleet_sessions_list(args: FleetSessionsArgs) -> Result<serde_json::Value, String> {
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let url = build_fleet_url(&base, &args);

    let client =
        crate::coord_http::coord_client().ok_or_else(|| "build http client".to_string())?;

    let resp = crate::coord_http::coord_get(client, &url)
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
}
