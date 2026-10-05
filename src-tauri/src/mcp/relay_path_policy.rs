//! Which local API paths the `http_request` relay arm may reach.
//!
//! # Why this exists
//!
//! `backend_relay`'s `http_request` arm is an unrestricted loopback HTTP proxy:
//! the frame chooses the method, the path, the headers and the body, and the
//! runner self-calls its OWN Axum server — which "binds the IPv4 loopback only
//! and these routes carry no further gate" (`mcp::terminals`). Local-caller
//! trust IS the runner API's whole authentication model, and this arm hands a
//! remote party the local caller's position.
//!
//! # The shape of the policy: a TOTAL allowlist
//!
//! [`RELAY_ALLOWED`] is a closed list of `(method, path-pattern)` pairs.
//! Anything not on it is refused — whatever it is, and **whenever it is
//! added**.
//!
//! Round 3 shipped the other shape: a denylist ([`GUARDED_PREFIXES`], now
//! deleted) naming the four prefixes that spawn, write to or kill a terminal,
//! with everything else allowed. Round 4 found what a denylist always
//! eventually contains — the routes nobody thought of:
//!
//! | Reachable under the denylist | What it does |
//! |---|---|
//! | `POST /execute-python` (`mcp::misc`) | runs caller-supplied Python |
//! | `POST /ui-bridge/invoke/get_coord_device_token` | returns the coord device JWT |
//! | `POST /ui-bridge/invoke/spawn_worker_session` | spawned a Claude-backed PTY (command since deleted) |
//! | `POST /sessions/spawn` + `/sessions/{id}/message` | spawns a process in a caller-chosen directory, then writes its stdin |
//!
//! The last two are registered in the same `routes()` function as a prefix
//! that WAS guarded. A denylist of dangerous prefixes fails open for every
//! route added after it — which is the same defect class round 3 set out to
//! fix. Inverting is the only shape that does not.
//!
//! # How the list was derived
//!
//! Not from what looks safe — from what real clients measurably call over
//! THIS arm. There is exactly one sender: qontinui-web's
//! `/api/v1/device-bridge/runner-proxy/{path:path}` route
//! (`backend/app/api/v1/endpoints/device_bridge_ws.py`), which takes the
//! relay arm when the request carries an `X-Qontinui-Device-Id` header and
//! forwards the caller's method and path VERBATIM. Its callers, measured
//! 2026-09-12:
//!
//! * **qontinui-mobile in remote (proxy) mode.** Round 3's note that mobile
//!   drives terminals over the TYPED `remote_terminal_*` frames is correct,
//!   and round 4's brief inferred from it that mobile barely uses this arm.
//!   That inference is WRONG and was worth checking: `HttpTransport`'s
//!   constructor re-points its whole base URL at the runner-proxy whenever a
//!   `proxyBaseUrl` is supplied (`src/api/core/HttpTransport.ts`), and
//!   `resolveProxyDecision` supplies one for every `remote` connection (and
//!   for a LAN connection whose direct probe fails —
//!   `src/hooks/runner/useInitializeClient.ts`). So in remote mode the app's
//!   ENTIRE runner API surface rides this arm: ~60 routes across its domain
//!   clients. Each one below is a measured call site, not a guess.
//! * **qontinui-web's own frontend.** `runnerProxyGet`
//!   (`digital-twin/_lib/runner-relay.ts`) for `useUiBridge`'s three reads,
//!   and the co-pilot planner's `POST prompt-home/plan`
//!   (`lib/co-pilot/planClient.ts`).
//!
//! Nothing else in the workspace sends an `http_request` frame.
//!
//! Three client calls are deliberately NOT listed, because the runner
//! registers no such route and they 404 today: mobile's `/ai/analyze`,
//! `/ai/runs/{id}/insight`, `/ai/patterns/failures`, `/workflow/resumable`,
//! `/workflow/resume`, `/workflow/force-continue`,
//! `/screenshots/{id}/thumbnail`, `/screenshots/{id}/full` and its three
//! `/api/v1/...` SSE streams. Listing a route that does not exist would fail
//! this module's own registration tripwire, and refusing a 404 with a 403
//! changes nothing a client can observe. Add the entry WITH the route.
//!
//! # The drift tripwire
//!
//! An allowlist inverts the drift risk rather than removing it. A new runner
//! route is closed by default — safe, and the whole point. What an allowlist
//! CAN do is rot: an entry whose route was renamed or removed silently stops
//! matching anything, and the client it existed for breaks with a 403 that
//! names the relay rather than the rename.
//!
//! So the tripwire is mechanical, not a comment: `every_allowlisted_route_is_
//! registered_by_the_runner` parses every `.route("…", …)` call under
//! `src/` out of the source tree and fails if any [`RELAY_ALLOWED`] entry does
//! not name one, with the matching method. Nothing is hand-transcribed.
//! `no_terminal_route_is_allowlisted` does the same in the other direction,
//! against `mcp::terminals::route_entries()`.
//!
//! # Normalisation
//!
//! Unchanged from round 3, and reviewed as sound. The verdict is taken on a
//! normalised form; the request is still FORWARDED verbatim, so a legitimate
//! encoding survives. Normalisation refuses rather than resolves anything
//! ambiguous:
//!
//! * everything from the first `?` or `#` is dropped (a query smuggled into
//!   `path` must not hide the route),
//! * `%XX` is decoded once — and a string that decodes DIFFERENTLY a second
//!   time is refused as double-encoded rather than guessed at,
//! * `\` counts as a separator alongside `/`,
//! * empty and `.` segments are dropped; a `..` segment is refused outright,
//! * control bytes are refused,
//! * segments are ASCII-lowercased, so `/TERMINALS` cannot walk past a
//!   case-sensitive comparison.
//!
//! Its over-refusals all fail safe: the worst a rejected legitimate spelling
//! costs is a 403 the caller can fix by spelling the path plainly.

/// Every `(method, path-pattern)` the `http_request` relay arm may carry.
/// **Closed**: anything not matched here is refused.
///
/// Spelled in the route's REGISTERED form — leading slash, `{name}`
/// placeholders — so the registration tripwire can compare it against the
/// `.route(...)` calls literally. A `{...}` segment matches exactly one
/// segment; matching is whole-path and case-insensitive.
///
/// Every entry is a measured client call site (see the module docs). Adding
/// one means naming the client that needs it.
pub const RELAY_ALLOWED: &[(&str, &str)] = &[
    // -- Liveness / identity ------------------------------------------
    // `runner-proxy/health` is the worked example in the web proxy route's
    // own docstring; `/status` is what the mobile app calls on every
    // (re)connect (`RunnerCoreClient.getStatus`).
    ("GET", "/health"),
    ("GET", "/status"),
    // -- qontinui-web: digital-twin UI Bridge panel (`useUiBridge`) -----
    ("GET", "/apps/{app_id}/spec/list"),
    ("GET", "/apps/{app_id}/spec/graph"),
    ("GET", "/ui-bridge/control/snapshot"),
    // -- qontinui-web: co-pilot planner (`planClient.ts`) ---------------
    ("POST", "/prompt-home/plan"),
    // -- mobile: task runs, chat, workflow control ---------------------
    ("GET", "/task-runs"),
    ("GET", "/task-runs/running"),
    ("GET", "/task-runs/{id}"),
    ("GET", "/task-runs/{id}/events"),
    ("GET", "/task-runs/{id}/workflow-state"),
    ("GET", "/task-runs/{id}/screenshots"),
    ("GET", "/task-runs/{id}/output"),
    ("GET", "/task-runs/{id}/session-state"),
    ("POST", "/task-runs/{id}/message"),
    ("POST", "/task-runs/{id}/stop"),
    ("POST", "/task-runs/{id}/pause"),
    ("POST", "/task-runs/{id}/unpause"),
    ("POST", "/task-runs/session"),
    ("POST", "/load-config"),
    ("POST", "/run-workflow"),
    ("POST", "/stop-execution"),
    ("GET", "/configs"),
    ("GET", "/monitors"),
    // -- mobile: findings ----------------------------------------------
    ("GET", "/findings/task/{task_run_id}"),
    ("PUT", "/findings/{finding_id}/status"),
    ("POST", "/findings/{finding_id}/user-response"),
    // -- mobile: screenshots -------------------------------------------
    // The list only. The thumbnail/full image URLs are handed to `<Image>`,
    // which sends no device header and therefore never takes this arm
    // (`src/api/core/relayStatus.ts` names the image loader as a standing
    // exclusion), and the runner registers no such route either way.
    ("GET", "/screenshots/list"),
    // -- mobile: knowledge graph + memory ------------------------------
    ("GET", "/graph/summary"),
    ("GET", "/graph/search"),
    ("GET", "/graph/cross-run-patterns"),
    ("GET", "/graph/phase-stats"),
    ("GET", "/graph/similar-errors"),
    ("GET", "/graph/ineffective-rules"),
    ("GET", "/memory/search"),
    // -- mobile: human-in-the-loop -------------------------------------
    ("GET", "/hitl/pending"),
    ("POST", "/hitl/{id}/respond"),
    // -- mobile: dev processes -----------------------------------------
    ("GET", "/processes"),
    ("GET", "/processes/{id}/output"),
    ("POST", "/processes/{id}/start"),
    ("POST", "/processes/{id}/stop"),
    ("POST", "/processes/{id}/restart"),
    // -- mobile: worktrees ---------------------------------------------
    ("GET", "/worktrees"),
    ("POST", "/worktrees/diff"),
    ("POST", "/worktrees/merge"),
    ("POST", "/worktrees/merge-force"),
    ("POST", "/worktrees/remove"),
    // -- mobile: file browser (read-only) ------------------------------
    ("GET", "/files/roots"),
    ("GET", "/files/browse"),
    ("GET", "/files/read"),
    // -- mobile: prompt + skill library --------------------------------
    ("GET", "/prompts"),
    ("GET", "/prompts/search"),
    ("GET", "/prompts/categories"),
    ("GET", "/skills"),
    ("GET", "/skills/search"),
    ("POST", "/skills/{id}/instantiate"),
    // -- mobile: state explorer ----------------------------------------
    ("GET", "/state-explorer/strategies"),
    ("GET", "/state-explorer/history"),
    ("GET", "/state-explorer/{run_id}"),
    ("POST", "/state-explorer/start"),
    // -- mobile: settings ----------------------------------------------
    ("GET", "/settings/general"),
    ("PUT", "/settings/general"),
    ("GET", "/settings/ai"),
    ("PUT", "/settings/ai"),
    ("POST", "/settings/ai/test-connection"),
    ("GET", "/settings/agentic"),
    ("PUT", "/settings/agentic"),
    ("GET", "/settings/debug"),
    ("PUT", "/settings/debug"),
    ("GET", "/settings/device-info"),
    ("GET", "/settings/storage"),
    ("POST", "/settings/storage/cleanup"),
    // -- mobile: usage analytics ---------------------------------------
    ("GET", "/analytics/account-usage"),
    ("GET", "/analytics/prepaid-balance"),
];

/// What the relay may do with one `http_request` frame's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayPathVerdict {
    /// On [`RELAY_ALLOWED`] for this method — relay it.
    Allow,
    /// The path could not be normalised into something safe to decide on
    /// (traversal, double-encoding, control bytes). Refused rather than
    /// guessed at.
    Malformed,
    /// Not on [`RELAY_ALLOWED`] for this method. The default answer.
    NotAllowed,
}

impl RelayPathVerdict {
    /// True when the frame must NOT reach the local API.
    pub fn is_refusal(self) -> bool {
        !matches!(self, RelayPathVerdict::Allow)
    }

    /// The message the refusal answers the relay with. Names the rule, not the
    /// route table: a caller learning which paths exist from a 403 is a worse
    /// outcome than one learning that this arm does not carry them.
    pub fn message(self) -> &'static str {
        match self {
            RelayPathVerdict::Allow => "",
            RelayPathVerdict::Malformed => {
                "relay path could not be normalised (traversal, double-encoding or control \
                 characters) — refused"
            }
            RelayPathVerdict::NotAllowed => {
                "this path is not carried by the HTTP relay — the relay serves a closed set of \
                 routes, and terminal creation, input and teardown go through the typed relay \
                 frames, which carry the create/attach gate"
            }
        }
    }

    /// A short stable token for logs and tests.
    pub fn code(self) -> &'static str {
        match self {
            RelayPathVerdict::Allow => "allow",
            RelayPathVerdict::Malformed => "relay_path_malformed",
            RelayPathVerdict::NotAllowed => "relay_path_not_allowed",
        }
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes once. A `%` that is not followed by two hex digits is
/// kept literally (that is what a server would do with it too). Invalid UTF-8
/// becomes U+FFFD — this string is only ever compared, never sent.
fn percent_decode_once(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_nibble(b[i + 1]), hex_nibble(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalise a relay path into lowercase segments, or `None` when it cannot be
/// decided on safely. See the module docs for the exact rules.
pub fn normalize_relay_path(raw: &str) -> Option<Vec<String>> {
    // A query or fragment smuggled into `path` must not hide the route.
    let path = raw
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    let once = percent_decode_once(&path);
    // Double-encoded (`%252e` -> `%2e` -> `.`): refuse rather than pick a
    // round. Nothing legitimate on this API spells a path that way.
    if percent_decode_once(&once) != once {
        return None;
    }

    if once.chars().any(|c| c.is_control()) {
        return None;
    }

    let mut segments = Vec::new();
    for seg in once.split(['/', '\\']) {
        let seg = seg.trim();
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            // Never resolved — a relay frame has no business walking up.
            return None;
        }
        segments.push(seg.to_ascii_lowercase());
    }
    Some(segments)
}

/// Does `segments` match `pattern`, where a `{...}` segment matches exactly
/// one segment? Whole-path match, not a prefix.
fn matches_pattern(segments: &[String], pattern: &str) -> bool {
    let wanted: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    if wanted.len() != segments.len() {
        return false;
    }
    wanted
        .iter()
        .zip(segments.iter())
        .all(|(w, s)| (w.starts_with('{') && w.ends_with('}')) || w.eq_ignore_ascii_case(s))
}

/// The verdict for one frame, against a given allowlist. Split out from
/// [`relay_path_verdict`] so tests can drive the matcher with a synthetic
/// table as well as with the real one.
pub fn relay_path_verdict_against(
    method: &str,
    raw_path: &str,
    allowed: &[(&str, &str)],
) -> RelayPathVerdict {
    let Some(segments) = normalize_relay_path(raw_path) else {
        return RelayPathVerdict::Malformed;
    };
    let method = method.trim();
    for (allow_method, pattern) in allowed {
        if allow_method.eq_ignore_ascii_case(method) && matches_pattern(&segments, pattern) {
            return RelayPathVerdict::Allow;
        }
    }
    RelayPathVerdict::NotAllowed
}

/// The verdict for one `http_request` frame's method + path.
pub fn relay_path_verdict(method: &str, raw_path: &str) -> RelayPathVerdict {
    relay_path_verdict_against(method, raw_path, RELAY_ALLOWED)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Normalisation
    // ------------------------------------------------------------------

    #[test]
    fn a_plain_path_normalises_to_lowercase_segments() {
        assert_eq!(
            normalize_relay_path("/Terminals/ABC/Write").unwrap(),
            vec!["terminals", "abc", "write"]
        );
        assert_eq!(normalize_relay_path("health").unwrap(), vec!["health"]);
        assert_eq!(
            normalize_relay_path("/").unwrap(),
            Vec::<String>::new(),
            "a bare root has no segments"
        );
    }

    #[test]
    fn empty_and_dot_segments_are_dropped_and_traversal_is_refused() {
        assert_eq!(
            normalize_relay_path("//terminals").unwrap(),
            vec!["terminals"]
        );
        assert_eq!(
            normalize_relay_path("/./terminals").unwrap(),
            vec!["terminals"]
        );
        assert_eq!(
            normalize_relay_path("/a/.//./b").unwrap(),
            vec!["a", "b"],
            "repeated empties and dots collapse"
        );
        assert!(normalize_relay_path("/x/../terminals").is_none());
        assert!(normalize_relay_path("..").is_none());
    }

    #[test]
    fn a_query_or_fragment_cannot_hide_the_route() {
        assert_eq!(
            normalize_relay_path("terminals?x=1").unwrap(),
            vec!["terminals"]
        );
        assert_eq!(
            normalize_relay_path("terminals#frag").unwrap(),
            vec!["terminals"]
        );
    }

    #[test]
    fn percent_encoding_is_decoded_once_and_double_encoding_is_refused() {
        assert_eq!(
            normalize_relay_path("%2Fterminals").unwrap(),
            vec!["terminals"]
        );
        assert_eq!(
            normalize_relay_path("/ter%6Dinals").unwrap(),
            vec!["terminals"]
        );
        // `%252e%252e` -> `%2e%2e` -> `..`: two rounds, so refused.
        assert!(normalize_relay_path("/x/%252e%252e/terminals").is_none());
        // A lone `%` is literal, not an escape, and is not double-encoding.
        assert_eq!(normalize_relay_path("/50%off").unwrap(), vec!["50%off"]);
        assert_eq!(normalize_relay_path("/a%").unwrap(), vec!["a%"]);
    }

    #[test]
    fn backslash_is_a_separator_and_control_bytes_are_refused() {
        assert_eq!(
            normalize_relay_path("\\terminals\\x").unwrap(),
            vec!["terminals", "x"]
        );
        assert_eq!(
            normalize_relay_path("%5Cterminals").unwrap(),
            vec!["terminals"]
        );
        assert!(normalize_relay_path("/terminals%00").is_none());
        assert!(normalize_relay_path("/term\ninals").is_none());
    }

    // ------------------------------------------------------------------
    // The verdict — closed by default
    // ------------------------------------------------------------------

    /// The property the whole module exists for. A path nobody listed is
    /// refused, and that must hold for routes this file has never heard of.
    #[test]
    fn an_unlisted_path_is_refused_whatever_the_method() {
        for path in [
            "/anything-at-all",
            "/a/b/c/d/e",
            "/status/extra",
            "/settings",
            "/graph",
            "/files",
            "/files/write",
            "/task-runs/{id}/generate-workflow",
            "/executor/restart",
            "/agent-worktrees/reclaim",
            "",
            "/",
        ] {
            for method in [
                "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
            ] {
                assert_eq!(
                    relay_path_verdict(method, path),
                    RelayPathVerdict::NotAllowed,
                    "{method} {path} must not be relayable"
                );
            }
        }
    }

    /// The routes round 4 found reachable under round 3's denylist.
    /// Arbitrary code execution, a credential mint, a PTY spawn, and
    /// spawn-then-write-stdin. All are registered routes — that is what
    /// makes them the point. (`spawn_worker_session`, the Claude-backed PTY
    /// of the original finding, was deleted with the Productivity board;
    /// `terminal_create` is the surviving PTY spawn.)
    #[test]
    fn the_round_four_findings_are_refused() {
        for (method, path) in [
            ("POST", "/execute-python"),
            ("POST", "/ui-bridge/invoke/get_coord_device_token"),
            ("POST", "/ui-bridge/invoke/terminal_create"),
            ("POST", "/sessions/spawn"),
            ("POST", "/sessions/abc/message"),
        ] {
            assert_eq!(
                relay_path_verdict(method, path),
                RelayPathVerdict::NotAllowed,
                "{method} {path}"
            );
            // …and under every other method too: an allowlist does not care
            // which verb an unlisted path is asked for.
            for other in ["GET", "PUT", "PATCH", "DELETE"] {
                assert!(
                    relay_path_verdict(other, path).is_refusal(),
                    "{other} {path}"
                );
            }
        }
    }

    /// Every evasion the reviewer named, on the route that spawns a PTY.
    /// Preserved from round 3 — the property is unchanged, only the reason
    /// it holds is (unlisted, rather than explicitly denied).
    #[test]
    fn no_spelling_of_the_create_route_is_relayable() {
        for path in [
            "/terminals",
            "terminals",
            "//terminals",
            "/./terminals",
            "/TERMINALS",
            "/Terminals",
            "%2fterminals",
            "/ter%6Dinals",
            "\\terminals",
            "/terminals?workingDir=/",
            "/terminals#x",
            "/terminals/",
        ] {
            assert_eq!(
                relay_path_verdict("POST", path),
                RelayPathVerdict::NotAllowed,
                "POST {path} must not be relayable"
            );
        }
        // Traversal is refused as malformed rather than resolved — also a
        // refusal, which is the property that matters.
        assert_eq!(
            relay_path_verdict("POST", "/x/../terminals"),
            RelayPathVerdict::Malformed
        );
        assert_eq!(
            relay_path_verdict("POST", "/x/%252e%252e/terminals"),
            RelayPathVerdict::Malformed
        );
    }

    /// The terminal surface, every method. Preserved from round 3.
    #[test]
    fn every_terminal_surface_is_closed_to_every_method() {
        let paths = [
            "/terminals",
            "/terminals/abc",
            "/terminals/abc/write",
            "/terminals/abc/submit-prompt",
            "/terminals/abc/resize",
            "/terminals/abc/move",
            "/terminals/abc/ws",
            "/terminals/abc/buffer",
            "/terminal-pages",
            "/ui-bridge/tauri/invoke",
            "/steward/dev-ops/start",
            "/stewards",
        ];
        for path in paths {
            for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
                assert_eq!(
                    relay_path_verdict(method, path),
                    RelayPathVerdict::NotAllowed,
                    "{method} {path}"
                );
            }
        }
    }

    /// What stays relayable — the measured client surface. Without this, a
    /// policy that refused everything would pass every test above.
    #[test]
    fn the_measured_client_surface_still_relays() {
        for (method, path) in [
            ("GET", "/health"),
            ("GET", "/status"),
            ("GET", "/apps/qontinui-web/spec/list"),
            ("GET", "/apps/qontinui-web/spec/graph"),
            ("GET", "/ui-bridge/control/snapshot"),
            ("POST", "/prompt-home/plan"),
            ("GET", "/task-runs"),
            ("GET", "/task-runs/running"),
            ("GET", "/task-runs/abc-123"),
            ("GET", "/task-runs/abc-123/events"),
            ("POST", "/task-runs/abc-123/message"),
            ("POST", "/run-workflow"),
            ("POST", "/hitl/q-1/respond"),
            ("POST", "/worktrees/merge"),
            ("GET", "/files/browse"),
            ("PUT", "/settings/general"),
            ("GET", "/analytics/account-usage"),
            // Case and encoding survive normalisation.
            ("get", "/STATUS"),
            ("GET", "//status"),
            ("GET", "/status?foo=bar"),
        ] {
            assert_eq!(
                relay_path_verdict(method, path),
                RelayPathVerdict::Allow,
                "{method} {path} must stay relayable"
            );
        }
    }

    /// An allowance is per METHOD: a read does not buy a write on the same
    /// path, and a wildcard segment does not span separators.
    #[test]
    fn an_allowance_is_scoped_to_its_method_and_to_one_segment() {
        assert_eq!(
            relay_path_verdict("GET", "/task-runs/abc"),
            RelayPathVerdict::Allow
        );
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            assert_eq!(
                relay_path_verdict(method, "/task-runs/abc"),
                RelayPathVerdict::NotAllowed,
                "{method} /task-runs/abc is a read allowance only"
            );
        }
        // `{id}` is one segment, and the match is whole-path.
        assert_eq!(
            relay_path_verdict("GET", "/task-runs/a/b"),
            RelayPathVerdict::NotAllowed
        );
        assert_eq!(
            relay_path_verdict("GET", "/task-runs/abc/events/more"),
            RelayPathVerdict::NotAllowed
        );
        // `GET /files/read` is allowed; nothing under it is.
        assert_eq!(
            relay_path_verdict("GET", "/files/read"),
            RelayPathVerdict::Allow
        );
        assert_eq!(
            relay_path_verdict("GET", "/files/read/etc/passwd"),
            RelayPathVerdict::NotAllowed
        );
    }

    /// The matcher itself, against a synthetic table.
    #[test]
    fn the_matcher_is_case_insensitive_and_wildcards_one_segment() {
        let allowed = [("GET", "/terminals"), ("GET", "/terminals/{id}/buffer")];

        assert_eq!(
            relay_path_verdict_against("GET", "/terminals", &allowed),
            RelayPathVerdict::Allow
        );
        assert_eq!(
            relay_path_verdict_against("get", "/TERMINALS", &allowed),
            RelayPathVerdict::Allow,
            "method and path comparison are both case-insensitive"
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/buffer", &allowed),
            RelayPathVerdict::Allow,
            "{{id}} matches exactly one segment"
        );
        assert_eq!(
            relay_path_verdict_against("POST", "/terminals", &allowed),
            RelayPathVerdict::NotAllowed
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/buffer/more", &allowed),
            RelayPathVerdict::NotAllowed
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/write", &allowed),
            RelayPathVerdict::NotAllowed
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/a/b/buffer", &allowed),
            RelayPathVerdict::NotAllowed
        );
        // An EMPTY table refuses everything — the shape the policy degrades to.
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals", &[]),
            RelayPathVerdict::NotAllowed
        );
    }

    #[test]
    fn a_refusal_is_a_refusal_and_carries_a_code() {
        assert!(!RelayPathVerdict::Allow.is_refusal());
        assert!(RelayPathVerdict::NotAllowed.is_refusal());
        assert!(RelayPathVerdict::Malformed.is_refusal());
        assert_eq!(
            RelayPathVerdict::NotAllowed.code(),
            "relay_path_not_allowed"
        );
        assert_eq!(RelayPathVerdict::Malformed.code(), "relay_path_malformed");
        assert!(!RelayPathVerdict::NotAllowed.message().is_empty());
        assert!(!RelayPathVerdict::Malformed.message().is_empty());
    }

    // ------------------------------------------------------------------
    // The drift tripwires — mechanical, against the real route table
    // ------------------------------------------------------------------

    /// The routing verbs a `MethodRouter` is built from.
    const ROUTING_VERBS: &[&str] = &[
        "get", "post", "put", "patch", "delete", "head", "options", "any",
    ];

    /// The two registration calls the walk reads. Spelled with `concat!` so
    /// this file's own text never contains a call shape the walk would read.
    const ROUTE_CALL: &str = concat!(".route", "(");
    const ROUTE_SERVICE_CALL: &str = concat!(".route_service", "(");

    /// Every `.rs` file under `src-tauri/src`, read at test time — the tree
    /// both the allowlist tripwires and the route census scan.
    fn crate_rust_sources() -> Vec<String> {
        let mut out = Vec::new();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                if let Ok(src) = std::fs::read_to_string(&path) {
                    out.push(src);
                }
            }
        }
        out
    }

    /// Every `(METHOD, path)` the runner registers, parsed out of the source
    /// tree. Axum 0.8 exposes no router introspection (the reason
    /// `ui_bridge::manifest_matches_route_calls` scans source too), so the
    /// registrations themselves are the only machine-readable route table
    /// there is.
    ///
    /// Deliberately permissive about METHOD: it collects every routing verb
    /// appearing anywhere in the `.route(...)` call. A false positive there
    /// only makes this tripwire accept an allowlist entry it should have
    /// questioned; a false NEGATIVE would fail a correct entry, which is the
    /// error worth avoiding in a test nobody can debug at 2am.
    ///
    /// `pub(crate)` so `mcp::origin_guard`'s allowlist tripwires reuse this
    /// one enumerator rather than growing a second.
    pub(crate) fn registered_routes() -> std::collections::HashSet<(String, String)> {
        let mut out = std::collections::HashSet::new();
        for src in crate_rust_sources() {
            collect_routes(&src, ROUTING_VERBS, &mut out);
        }
        out
    }

    /// One registration call site, as [`walk_route_calls`] reads it.
    struct RouteCall<'a> {
        /// Byte offset of the call's needle in its file.
        at: usize,
        path: RoutePath<'a>,
        /// Everything after the path argument, up to the `)` closing the call.
        chain: &'a str,
    }

    /// A registration call's first argument.
    enum RoutePath<'a> {
        /// A string literal: the registered path itself.
        Literal(&'a str),
        /// A constant or binding (`ROUTE_PATH`, `template`) whose value a
        /// source scan cannot read — recorded by name so the site still counts.
        Named(&'a str),
    }

    /// THE walk: every `needle` call in `src` whose first argument is a string
    /// literal or a plain (possibly `::`-qualified) name, with the text of the
    /// rest of the call. Both [`collect_routes`] and the HTTP route census read
    /// the tree through this one walk, so they cannot disagree about which
    /// calls exist.
    fn walk_route_calls<'a>(src: &'a str, needle: &str, mut visit: impl FnMut(RouteCall<'a>)) {
        let bytes = src.as_bytes();
        let mut from = 0usize;
        while let Some(rel) = src.get(from..).and_then(|rest| rest.find(needle)) {
            let at = from + rel;
            let open = at + needle.len();
            from = open;
            let Some(after_open) = src.get(open..) else {
                continue;
            };
            let first = open + (after_open.len() - after_open.trim_start().len());
            let Some(arg) = src.get(first..) else {
                continue;
            };
            let (path, chain_start) = if let Some(literal) = arg.strip_prefix('"') {
                let Some(len) = literal.find('"') else {
                    continue;
                };
                let Some(path) = literal.get(..len) else {
                    continue;
                };
                (RoutePath::Literal(path), first + 1 + len + 1)
            } else {
                let len = arg
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                    .unwrap_or(arg.len());
                let name = arg.get(..len).unwrap_or("");
                let then_comma = arg
                    .get(len..)
                    .is_some_and(|rest| rest.trim_start().starts_with(','));
                if name.is_empty() || !then_comma {
                    continue;
                }
                (RoutePath::Named(name), first + len)
            };
            // Walk to the `)` that closes the call.
            let mut depth = 1usize;
            let mut i = chain_start;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            let Some(chain) = src.get(chain_start..i.saturating_sub(1)) else {
                continue;
            };
            visit(RouteCall { at, path, chain });
        }
    }

    /// Pull `(METHOD, path)` pairs out of one file's literal-path `.route(…)`
    /// calls.
    fn collect_routes(
        src: &str,
        verbs: &[&str],
        out: &mut std::collections::HashSet<(String, String)>,
    ) {
        walk_route_calls(src, ROUTE_CALL, |call| {
            let RoutePath::Literal(path) = call.path else {
                return;
            };
            let chain = call.chain;
            for verb in verbs {
                // `get(` / `.get(` / `routing::get(` all count.
                let mut at = 0usize;
                while let Some(rel) = chain.get(at..).and_then(|rest| rest.find(verb)) {
                    let start = at + rel;
                    at = start + verb.len();
                    let before_ok = !chain
                        .get(..start)
                        .and_then(|before| before.chars().next_back())
                        .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    let after_ok = chain
                        .get(at..)
                        .is_some_and(|after| after.trim_start().starts_with('('));
                    if before_ok && after_ok {
                        out.insert((verb.to_uppercase(), path.to_string()));
                        break;
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------------
    // The HTTP route census — a snapshot of every registration site
    // ------------------------------------------------------------------

    /// The census snapshot, relative to `CARGO_MANIFEST_DIR`.
    const HTTP_ROUTE_SNAPSHOT: &str = "http-routes.snapshot.txt";
    /// Set to `1` to rewrite the snapshot from the tree instead of comparing.
    const UPDATE_HTTP_ROUTE_SNAPSHOT: &str = "UPDATE_HTTP_ROUTE_SNAPSHOT";

    /// Every registration site in the tree as one `<METHODS> <path> <handler>`
    /// line, sorted, duplicates kept. There is deliberately NO file or module
    /// column, and the handler is named without its module qualifiers: moving
    /// a handler or a whole router between files leaves the census identical,
    /// while dropping a route, changing its methods or rebinding its handler
    /// changes it.
    pub(crate) fn http_route_census_lines() -> Vec<String> {
        let mut lines = Vec::new();
        for src in crate_rust_sources() {
            census_one_file(&src, &mut lines);
        }
        lines.sort();
        lines
    }

    fn census_one_file(src: &str, out: &mut Vec<String>) {
        for (needle, is_service) in [(ROUTE_CALL, false), (ROUTE_SERVICE_CALL, true)] {
            walk_route_calls(src, needle, |call| {
                if in_line_comment(src, call.at) {
                    return;
                }
                let path = match call.path {
                    RoutePath::Literal(p) if !p.is_empty() && !p.contains(char::is_whitespace) => {
                        p.to_string()
                    }
                    // A literal with whitespace is prose that happens to
                    // follow the call shape (a string or doc example), never
                    // a path.
                    RoutePath::Literal(_) => return,
                    RoutePath::Named(name) => format!("<{name}>"),
                };
                if is_service {
                    out.push(format!("SERVICE {path} {}", handler_name(call.chain)));
                    return;
                }
                let pairs = method_handlers(call.chain);
                if pairs.is_empty() {
                    // A `MethodRouter` built by a helper call rather than inline:
                    // the methods are not visible here, the site still is.
                    out.push(format!("? {path} {}", handler_name(call.chain)));
                    return;
                }
                let mut by_handler: std::collections::BTreeMap<
                    String,
                    std::collections::BTreeSet<String>,
                > = std::collections::BTreeMap::new();
                for (verb, handler) in pairs {
                    by_handler.entry(handler).or_default().insert(verb);
                }
                for (handler, verbs) in by_handler {
                    let verbs: Vec<String> = verbs.into_iter().collect();
                    out.push(format!("{} {path} {handler}", verbs.join(",")));
                }
            });
        }
    }

    /// Whether the byte at `at` sits after a `//` on its own line.
    fn in_line_comment(src: &str, at: usize) -> bool {
        let line_start = src
            .get(..at)
            .and_then(|before| before.rfind('\n'))
            .map_or(0, |n| n + 1);
        src.get(line_start..at)
            .is_some_and(|line| line.contains("//"))
    }

    /// If `b[i]` opens a string or char literal, the index just past it.
    fn skip_literal(b: &[u8], i: usize) -> Option<usize> {
        match b.get(i) {
            Some(b'"') => {
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 2,
                        b'"' => return Some(j + 1),
                        _ => j += 1,
                    }
                }
                Some(b.len())
            }
            // `'('` / `'\''` — a lifetime (`'a`) has no closing quote here.
            Some(b'\'') if b.get(i + 2) == Some(&b'\'') => Some(i + 3),
            Some(b'\'') if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') => {
                Some(i + 4)
            }
            _ => None,
        }
    }

    fn is_ident_byte(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_'
    }

    /// The `(VERB, handler)` pairs of a `MethodRouter` chain such as
    /// `get(a).post(b).layer(…)`. Only verbs at the chain's top level count, so
    /// a verb inside a handler closure or a layer argument is not mistaken for
    /// a registration. `get_service(…)` and friends count as their verb.
    fn method_handlers(chain: &str) -> Vec<(String, String)> {
        let b = chain.as_bytes();
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut i = 0usize;
        while i < b.len() {
            if let Some(next) = skip_literal(b, i) {
                i = next;
                continue;
            }
            let c = b[i];
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ if depth == 0 && is_ident_byte(c) && (i == 0 || !is_ident_byte(b[i - 1])) => {
                    let mut end = i;
                    while end < b.len() && is_ident_byte(b[end]) {
                        end += 1;
                    }
                    let word = chain.get(i..end).unwrap_or("");
                    let verb = word.strip_suffix("_service").unwrap_or(word);
                    let mut open = end;
                    while open < b.len() && b[open].is_ascii_whitespace() {
                        open += 1;
                    }
                    if ROUTING_VERBS.contains(&verb) && b.get(open) == Some(&b'(') {
                        let close = matching_close(b, open);
                        let arg = chain.get(open + 1..close).unwrap_or("");
                        out.push((verb.to_ascii_uppercase(), handler_name(arg)));
                        i = close + 1;
                    } else {
                        i = end;
                    }
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        out
    }

    /// The index of the `)` matching the `(` at `open` (or the end).
    fn matching_close(b: &[u8], open: usize) -> usize {
        let mut depth = 0i32;
        let mut i = open;
        while i < b.len() {
            if let Some(next) = skip_literal(b, i) {
                i = next;
                continue;
            }
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        b.len()
    }

    /// The name a handler expression is registered under, without the module
    /// path it is reached through: `crate::mcp::x::handler` → `handler`,
    /// `GraphQLSubscription::new(…)` → `GraphQLSubscription::new`, a closure →
    /// `<closure>`. Dropping the leading lowercase (module) segments is what
    /// keeps the census stable across a move between files.
    fn handler_name(expr: &str) -> String {
        let expr = expr.trim_start().trim_start_matches(',').trim_start();
        if expr.starts_with('|') || expr.starts_with("move") || expr.starts_with("async") {
            return "<closure>".to_string();
        }
        let len = expr
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(expr.len());
        let path = expr.get(..len).unwrap_or("").trim_end_matches(':');
        let segments: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
        let keep_from = segments
            .iter()
            .position(|s| s.starts_with(|c: char| c.is_ascii_uppercase()))
            .unwrap_or(segments.len().saturating_sub(1));
        let name = segments.get(keep_from..).map(|s| s.join("::")).unwrap_or_default();
        if name.is_empty() {
            "<expr>".to_string()
        } else {
            name
        }
    }

    /// Lines of `a` not matched in `b`, as a multiset difference.
    fn multiset_minus(a: &[String], b: &[String]) -> Vec<String> {
        let mut remaining: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for line in b {
            *remaining.entry(line.as_str()).or_default() += 1;
        }
        let mut out = Vec::new();
        for line in a {
            match remaining.get_mut(line.as_str()) {
                Some(n) if *n > 0 => *n -= 1,
                _ => out.push(line.clone()),
            }
        }
        out
    }

    /// **The HTTP route census.** Every route registration in the tree,
    /// pinned against `src-tauri/http-routes.snapshot.txt`. A dropped route, a
    /// changed method set or a rebound handler fails it; moving code between
    /// files does not, because the census carries no file column. Guards the
    /// `mcp_api.rs` split (plan
    /// `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`).
    #[test]
    fn http_route_census() {
        let actual = http_route_census_lines();
        assert!(
            actual.len() > 500,
            "the route census found only {} registrations — the walk is broken, not the router",
            actual.len()
        );
        let snapshot =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(HTTP_ROUTE_SNAPSHOT);
        if std::env::var(UPDATE_HTTP_ROUTE_SNAPSHOT).as_deref() == Ok("1") {
            let mut text = String::from(
                "# HTTP route census: every route registration call under src-tauri/src, one\n\
                 # `<METHODS> <path> <handler>` line each, sorted, duplicates kept. `?` = a\n\
                 # MethodRouter built elsewhere; `SERVICE` = a route_service mount; `<name>` = a\n\
                 # path held in a constant or binding. No file column, so a move between files\n\
                 # leaves this unchanged. GENERATED by mcp::relay_path_policy::tests::\n\
                 # http_route_census; regenerate with UPDATE_HTTP_ROUTE_SNAPSHOT=1.\n",
            );
            for line in &actual {
                text.push_str(line);
                text.push('\n');
            }
            std::fs::write(&snapshot, text)
                .unwrap_or_else(|e| panic!("writing {}: {e}", snapshot.display()));
            eprintln!(
                "http_route_census: rewrote {} ({} routes)",
                snapshot.display(),
                actual.len()
            );
            return;
        }
        let text = std::fs::read_to_string(&snapshot).unwrap_or_else(|e| {
            panic!(
                "reading {}: {e} — generate it with \
                 `{UPDATE_HTTP_ROUTE_SNAPSHOT}=1` and this test",
                snapshot.display()
            )
        });
        let expected: Vec<String> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        if expected == actual {
            return;
        }
        let removed = multiset_minus(&expected, &actual);
        let added = multiset_minus(&actual, &expected);
        panic!(
            "the HTTP route table changed.\n\
             \n\
             In the snapshot, missing from the tree ({} — a dropped or re-methoded route):\n{}\n\
             \n\
             In the tree, missing from the snapshot ({}):\n{}\n\
             \n\
             If the change is intended, regenerate the snapshot and commit it:\n  \
             {UPDATE_HTTP_ROUTE_SNAPSHOT}=1 <cargo-guard.sh test> http_route_census\n\
             A pure move between files never changes the census; if a move did, the move \
             dropped or altered a registration.",
            removed.len(),
            removed.iter().map(|l| format!("  - {l}")).collect::<Vec<_>>().join("\n"),
            added.len(),
            added.iter().map(|l| format!("  + {l}")).collect::<Vec<_>>().join("\n"),
        );
    }

    /// The census reader against synthetic sources: methods group per
    /// handler, module paths are dropped, nested verbs and comments are not
    /// read, and named paths still count.
    #[test]
    fn the_census_reads_methods_handlers_and_named_paths() {
        let src = [
            "Router::new()",
            "    {R}\"/a\", get(crate::mcp::x::alpha).post(crate::mcp::x::alpha))",
            "    {R}\"/b\", axum::routing::get(beta).delete(gamma).layer(from_fn(get_mw)))",
            "    {R}\"/c\", get(|| async { s.get(1) }))",
            "    {R}ROUTE_PATH, put(delta))",
            "    {R}\"/d\", built_elsewhere())",
            "    {S}\"/ws\", GraphQLSubscription::new(schema.clone()))",
            "    // {R}\"/commented\", get(nope))",
        ]
        .join("\n")
        .replace("{R}", ROUTE_CALL)
        .replace("{S}", ROUTE_SERVICE_CALL);
        let mut lines = Vec::new();
        census_one_file(&src, &mut lines);
        lines.sort();
        assert_eq!(
            lines,
            vec![
                "? /d built_elsewhere",
                "DELETE /b gamma",
                "GET /b beta",
                "GET /c <closure>",
                "GET,POST /a alpha",
                "PUT <ROUTE_PATH> delta",
                "SERVICE /ws GraphQLSubscription::new",
            ]
        );
    }

    /// **The mechanical tripwire.** An allowlist entry that names no
    /// registered route matches nothing: the client it exists for gets a 403
    /// blaming the relay, when the real cause is a renamed or deleted route.
    /// Parsed out of the tree rather than transcribed, so it cannot go stale.
    #[test]
    fn every_allowlisted_route_is_registered_by_the_runner() {
        let registered = registered_routes();
        assert!(
            registered.len() > 500,
            "the route scan found only {} registrations — it is broken, not the allowlist",
            registered.len()
        );
        let mut missing: Vec<String> = Vec::new();
        for (method, pattern) in RELAY_ALLOWED {
            let hit = registered.iter().any(|(m, p)| {
                m == method
                    && (p == pattern
                        || (p.trim_start_matches('/') == pattern.trim_start_matches('/')))
            });
            if !hit {
                // A route may be registered under a differently NAMED
                // placeholder (`{id}` vs `{run_id}`); compare shapes too.
                let shape_hit = registered
                    .iter()
                    .any(|(m, p)| m == method && same_shape(p, pattern));
                if !shape_hit {
                    missing.push(format!("{method} {pattern}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "RELAY_ALLOWED names routes the runner does not register — a rename or a typo, \
             and each one is a client broken with a 403: {missing:#?}"
        );
    }

    /// Two patterns with the same segment count where every non-placeholder
    /// segment agrees.
    fn same_shape(a: &str, b: &str) -> bool {
        let sa: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
        let sb: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
        if sa.len() != sb.len() {
            return false;
        }
        sa.iter().zip(sb.iter()).all(|(x, y)| {
            let xw = x.starts_with('{') && x.ends_with('}');
            let yw = y.starts_with('{') && y.ends_with('}');
            (xw && yw) || x.eq_ignore_ascii_case(y)
        })
    }

    /// The other direction, against the terminal module's own real route
    /// table: nothing that spawns, writes to or kills a PTY may be
    /// allowlisted, now or after the next route is added there.
    #[test]
    fn no_terminal_route_is_allowlisted() {
        for (method, path) in crate::mcp::terminals::route_entries() {
            let concrete = path.replace("{id}", "11111111-2222-3333-4444-555555555555");
            for candidate in [path.to_string(), concrete] {
                assert_eq!(
                    relay_path_verdict(method, &candidate),
                    RelayPathVerdict::NotAllowed,
                    "{method} {candidate} is allowlisted — terminal routes go through the typed \
                     relay frames, which carry the create/attach gate"
                );
            }
        }
    }

    /// The tauri-invoke proxy safelists the terminal commands, so the whole
    /// `/ui-bridge/invoke` and `/ui-bridge/tauri/invoke` family stays off the
    /// list. Pinned against the real safelist so the two cannot drift apart
    /// silently.
    #[test]
    fn the_tauri_invoke_proxy_is_not_allowlisted() {
        let safelist = crate::mcp::tauri_proxy::ALLOWED_PROXIED_COMMANDS;
        for cmd in ["terminal_create", "terminal_write", "terminal_close"] {
            assert!(
                safelist.contains(&cmd),
                "{cmd} left the safelist — re-read why the invoke proxy is off RELAY_ALLOWED"
            );
        }
        for path in [
            "/ui-bridge/tauri/invoke",
            "/ui-bridge/invoke/terminal_create",
            "/ui-bridge/commands",
        ] {
            for method in ["GET", "POST"] {
                assert_eq!(
                    relay_path_verdict(method, path),
                    RelayPathVerdict::NotAllowed,
                    "{method} {path}"
                );
            }
        }
    }

    /// No entry is listed twice, and every one is spelled in the registered
    /// form this module's tripwire compares against.
    #[test]
    fn the_allowlist_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for (method, pattern) in RELAY_ALLOWED {
            assert!(
                seen.insert((method.to_uppercase(), pattern.to_string())),
                "{method} {pattern} is listed twice"
            );
            assert!(
                pattern.starts_with('/'),
                "{pattern} must be spelled with a leading slash"
            );
            assert_eq!(
                *method,
                method.to_uppercase(),
                "{method} must be spelled in upper case"
            );
            assert!(
                !pattern.contains('?') && !pattern.contains('*'),
                "{pattern}: patterns carry no query and no glob — `{{name}}` is the only wildcard"
            );
        }
    }
}
