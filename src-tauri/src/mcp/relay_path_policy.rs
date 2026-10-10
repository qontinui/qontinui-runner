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

    /// The two route registration calls the walk reads, by method name
    /// ([`Lexed::calls`] finds the `(` after it).
    const ROUTE_CALL: &str = ".route";
    const ROUTE_SERVICE_CALL: &str = ".route_service";

    /// Every `.rs` file under `src-tauri/src` with its path relative to
    /// `CARGO_MANIFEST_DIR` (`/`-separated), read at test time — the tree
    /// both the allowlist tripwires and the route census scan.
    fn crate_rust_sources() -> Vec<(String, String)> {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        let mut stack = vec![manifest.join("src")];
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
                let Ok(rel) = path.strip_prefix(&manifest) else {
                    continue;
                };
                let rel = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Ok(src) = std::fs::read_to_string(&path) {
                    out.push((rel, src));
                }
            }
        }
        out
    }

    /// One Rust source file as two views of identical byte length, so an
    /// offset found in one addresses the same text in the other:
    ///
    /// - `code` — the source with every comment blanked to spaces, so a
    ///   comment reads as whitespace wherever it sits;
    /// - `skel` — `code` with every string, raw-string and char literal's
    ///   CONTENTS blanked as well (delimiters kept), so a call shape inside a
    ///   literal is never found and a bracket inside one is never counted.
    ///
    /// Newlines survive both, so line structure is kept. Search and bracket
    /// matching run on `skel`; literal text (a path) is read from `code`.
    struct Lexed {
        code: String,
        skel: String,
    }

    impl Lexed {
        fn new(src: &str) -> Self {
            let b = src.as_bytes();
            let n = b.len();
            let mut code = b.to_vec();
            let mut skel = b.to_vec();
            let blank = |v: &mut Vec<u8>, from: usize, to: usize| {
                for byte in v.iter_mut().take(to.min(n)).skip(from) {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
            };
            let mut i = 0usize;
            while i < n {
                let c = b[i];
                let next = b.get(i + 1).copied();
                if c == b'/' && next == Some(b'/') {
                    let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p);
                    blank(&mut code, i, end);
                    blank(&mut skel, i, end);
                    i = end;
                } else if c == b'/' && next == Some(b'*') {
                    let mut depth = 1usize;
                    let mut j = i + 2;
                    while j < n && depth > 0 {
                        if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                            depth += 1;
                            j += 2;
                        } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                            depth -= 1;
                            j += 2;
                        } else {
                            j += 1;
                        }
                    }
                    blank(&mut code, i, j);
                    blank(&mut skel, i, j);
                    i = j;
                } else if c == b'"' {
                    let mut j = i + 1;
                    while j < n && b[j] != b'"' {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    blank(&mut skel, i + 1, j);
                    i = j + 1;
                } else if c == b'\'' {
                    // `'x'`, `'\n'`, `'\u{..}'` are literals; `'a` is a lifetime.
                    if next == Some(b'\\') {
                        let close = b[(i + 3).min(n)..]
                            .iter()
                            .position(|&x| x == b'\'')
                            .map_or(n, |p| i + 3 + p);
                        blank(&mut skel, i + 1, close);
                        i = close + 1;
                    } else {
                        let width = src
                            .get(i + 1..)
                            .and_then(|rest| rest.chars().next())
                            .map_or(1, char::len_utf8);
                        if b.get(i + 1 + width) == Some(&b'\'') {
                            blank(&mut skel, i + 1, i + 1 + width);
                            i += 2 + width;
                        } else {
                            i += 1;
                        }
                    }
                } else if is_ident_byte(c) && (i == 0 || !is_ident_byte(b[i - 1])) {
                    // A raw string (`r"…"`, `r#"…"#`, `br"…"`) starts like an
                    // identifier; anything else is an identifier, skipped whole
                    // so its letters are never read as a literal prefix.
                    let hashes_at = match (c, next) {
                        (b'r', _) => Some(i + 1),
                        (b'b' | b'c', Some(b'r')) => Some(i + 2),
                        _ => None,
                    };
                    let raw_open = hashes_at.and_then(|h| {
                        let hashes = b[h.min(n)..].iter().take_while(|&&x| x == b'#').count();
                        (b.get(h + hashes) == Some(&b'"')).then_some((h + hashes + 1, hashes))
                    });
                    if let Some((body, hashes)) = raw_open {
                        let mut j = body;
                        while j < n {
                            if b[j] == b'"'
                                && b[(j + 1).min(n)..]
                                    .iter()
                                    .take(hashes)
                                    .filter(|&&x| x == b'#')
                                    .count()
                                    == hashes
                            {
                                break;
                            }
                            j += 1;
                        }
                        blank(&mut skel, body, j);
                        i = j + 1 + hashes;
                    } else {
                        while i < n && is_ident_byte(b[i]) {
                            i += 1;
                        }
                    }
                } else {
                    i += 1;
                }
            }
            // Blanking replaces whole characters (every one begins and ends
            // inside the blanked span) with ASCII spaces, so both stay UTF-8.
            Self {
                code: String::from_utf8(code).expect("blanking whole characters keeps UTF-8"),
                skel: String::from_utf8(skel).expect("blanking whole characters keeps UTF-8"),
            }
        }

        /// Blank `[from, to)` in both views.
        fn blank(&mut self, from: usize, to: usize) {
            for view in [&mut self.code, &mut self.skel] {
                let mut bytes = std::mem::take(view).into_bytes();
                for byte in bytes.iter_mut().take(to).skip(from) {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
                *view = String::from_utf8(bytes).expect("a test item spans whole characters");
            }
        }

        /// Every item gated on a test-only cfg ([`is_test_cfg`]) as
        /// `(attribute start, item start, item end)`: a `mod … { … }`, a
        /// `fn … { … }`, a `;`-terminated item such as `mod tests;` — or,
        /// when the gated thing does not open with an item keyword
        /// ([`ITEM_KEYWORDS`]), a struct field, struct-literal field, enum
        /// variant or match arm, which also ends at its first depth-0 `,` or
        /// where its enclosing `}` / `)` / `]` closes. (A `,` inside a field's
        /// generic type ends it early; that leaves test-only text unblanked,
        /// never production text blanked.)
        fn test_items(&self) -> Vec<(usize, usize, usize)> {
            let b = self.skel.as_bytes();
            let mut out = Vec::new();
            let mut from = 0usize;
            while let Some(rel) = self.skel.get(from..).and_then(|rest| rest.find("#[")) {
                let attr = from + rel;
                let attr_end = matching_close(b, attr + 1);
                from = attr_end + 1;
                if !is_test_cfg(&stripped(self.skel.get(attr + 2..attr_end).unwrap_or(""))) {
                    continue;
                }
                // Skip any further attributes, then find the item's body.
                let mut i = attr_end + 1;
                loop {
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'[') {
                        i = matching_close(b, i + 1) + 1;
                    } else {
                        break;
                    }
                }
                let item = i;
                let mut word_end = item;
                while word_end < b.len() && is_ident_byte(b[word_end]) {
                    word_end += 1;
                }
                let is_item = self
                    .skel
                    .get(item..word_end)
                    .is_some_and(|word| ITEM_KEYWORDS.contains(&word));
                let mut end = b.len();
                while i < b.len() {
                    match b[i] {
                        b'{' => {
                            end = matching_close(b, i) + 1;
                            break;
                        }
                        b';' => {
                            end = i + 1;
                            break;
                        }
                        // A field, variant or arm ends at its separator.
                        b',' if !is_item => {
                            end = i + 1;
                            break;
                        }
                        // The enclosing scope closed first: the gated thing
                        // was a field, variant or arm, and ends here.
                        b'}' | b')' | b']' => {
                            end = i;
                            break;
                        }
                        b'(' | b'[' => i = matching_close(b, i) + 1,
                        _ => i += 1,
                    }
                }
                out.push((attr, item, end));
                from = end;
            }
            out
        }

        /// Blank every test-only item ([`Self::test_items`]) in both views.
        /// Mock routers built inside unit tests are not the runner's routes.
        fn blank_test_items(&mut self) {
            for (attr, _, end) in self.test_items() {
                self.blank(attr, end);
            }
        }

        /// Every `mod <name> …` in the file, as `(keyword offset, name, the
        /// byte after the name)`.
        fn mod_keywords(&self) -> Vec<(usize, &str, usize)> {
            let b = self.skel.as_bytes();
            let mut out = Vec::new();
            let mut from = 0usize;
            while let Some(rel) = self.skel.get(from..).and_then(|rest| rest.find("mod")) {
                let at = from + rel;
                from = at + 3;
                if (at > 0 && is_ident_byte(b[at - 1]))
                    || !b.get(from).is_some_and(u8::is_ascii_whitespace)
                {
                    continue;
                }
                let name_at = skip_whitespace(b, from);
                let mut name_end = name_at;
                while name_end < b.len() && is_ident_byte(b[name_end]) {
                    name_end += 1;
                }
                if name_end > name_at {
                    let name = self.skel.get(name_at..name_end).unwrap_or("");
                    out.push((at, name, skip_whitespace(b, name_end)));
                }
            }
            out
        }

        /// Where the attributes of the item whose keyword sits at `kw` begin:
        /// back over a `pub` / `pub(…)` visibility, then over every `#[…]`.
        fn attribute_stack_start(&self, kw: usize) -> usize {
            let b = self.skel.as_bytes();
            let back_ws = |mut i: usize| {
                while i > 0 && b[i - 1].is_ascii_whitespace() {
                    i -= 1;
                }
                i
            };
            let mut i = back_ws(kw);
            if i > 0 && b[i - 1] == b')' {
                if let Some(open) = matching_open(b, i - 1) {
                    i = back_ws(open);
                }
            }
            if i >= 3 && b.get(i - 3..i) == Some(b"pub") {
                i -= 3;
            }
            loop {
                let end = back_ws(i);
                if end == 0 || b[end - 1] != b']' {
                    return i;
                }
                match matching_open(b, end - 1) {
                    Some(open) if open > 0 && b[open - 1] == b'#' => i = open - 1,
                    _ => return i,
                }
            }
        }

        /// The `"…"` of a `#[path = "…"]` among the attributes in
        /// `[from, to)`, read one attribute at a time.
        fn path_attribute(&self, from: usize, to: usize) -> Option<&str> {
            let b = self.skel.as_bytes();
            let mut i = skip_whitespace(b, from);
            while i < to && b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'[') {
                let close = matching_close(b, i + 1);
                let body = stripped(self.skel.get(i + 2..close).unwrap_or(""));
                if body.starts_with("path=\"") {
                    let lit = self.code.get(i + 2..close)?;
                    let open = lit.find('"')?;
                    let rest = lit.get(open + 1..)?;
                    return rest.get(..rest.find('"')?);
                }
                i = skip_whitespace(b, close + 1);
            }
            None
        }

        /// The names of the inline `mod name { … }` blocks enclosing `at`,
        /// outermost first.
        fn enclosing_inline_mods(&self, at: usize) -> Vec<&str> {
            let b = self.skel.as_bytes();
            self.mod_keywords()
                .into_iter()
                .filter(|&(kw, _, after)| {
                    kw < at && b.get(after) == Some(&b'{') && at < matching_close(b, after)
                })
                .map(|(_, name, _)| name)
                .collect()
        }

        /// The files this file declares as test-only out-of-line modules, as
        /// paths relative to `CARGO_MANIFEST_DIR` — `rel` is this file's own.
        /// A `mod name;` is test-only when it is itself a test-only item, sits
        /// inside one (`#[cfg(test)] mod tests { mod helpers; }`), or the whole
        /// file is (`whole_file`). Resolved as rustc does: `#[path = "…"]`
        /// relative to this file's directory (plus any enclosing inline
        /// modules, under the file's stem unless it is a `mod.rs`/`lib.rs`/
        /// `main.rs`), `..` normalised; otherwise both of Rust's layouts
        /// (`name.rs`, `name/mod.rs`), only one of which can exist.
        fn test_module_files(&self, rel: &str, whole_file: bool) -> Vec<String> {
            let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
            let stem = file.strip_suffix(".rs").unwrap_or(file);
            let mod_rs = matches!(stem, "mod" | "lib" | "main");
            let b = self.skel.as_bytes();
            let test_spans = self.test_items();
            let mut out = Vec::new();
            for (kw, name, after) in self.mod_keywords() {
                if b.get(after) != Some(&b';') {
                    continue;
                }
                let stack = self.attribute_stack_start(kw);
                let gated = whole_file
                    || test_spans
                        .iter()
                        .any(|&(attr, _, end)| attr <= kw && kw < end);
                if !gated {
                    continue;
                }
                let inline = self.enclosing_inline_mods(kw);
                let mut base = vec![dir];
                if !mod_rs && (!inline.is_empty() || self.path_attribute(stack, kw).is_none()) {
                    base.push(stem);
                }
                base.extend(inline.iter().copied());
                let base = base.join("/");
                match self.path_attribute(stack, kw) {
                    Some(path) => out.push(normalized(&format!("{base}/{path}"))),
                    None => {
                        out.push(normalized(&format!("{base}/{name}.rs")));
                        out.push(normalized(&format!("{base}/{name}/mod.rs")));
                    }
                }
            }
            out
        }

        /// Each call of the method `name` (`.route`, `.merge`, …) in `skel`:
        /// the index just past its `(` and the index of the `)` closing it.
        /// Whitespace may sit on either side of the method name (`r. route`,
        /// `.route (`) and a turbofish before the `(` (`.route::<S>(`); a
        /// longer name (`.route_service` for `.route`, `reroute` for `route`)
        /// is not a call of `name`.
        fn calls(&self, name: &str) -> Vec<(usize, usize)> {
            let b = self.skel.as_bytes();
            let method = name.trim_start_matches('.');
            let mut out = Vec::new();
            let mut from = 0usize;
            while let Some(rel) = self.skel.get(from..).and_then(|rest| rest.find(method)) {
                let at = from + rel;
                from = at + method.len();
                if b.get(from).copied().is_some_and(is_ident_byte)
                    || (at > 0 && is_ident_byte(b[at - 1]))
                {
                    continue;
                }
                let mut dot = at;
                while dot > 0 && b[dot - 1].is_ascii_whitespace() {
                    dot -= 1;
                }
                if dot == 0 || b[dot - 1] != b'.' {
                    continue;
                }
                let mut open = skip_whitespace(b, from);
                if b.get(open) == Some(&b':') && b.get(open + 1) == Some(&b':') {
                    open = skip_whitespace(b, open + 2);
                    if b.get(open) != Some(&b'<') {
                        continue;
                    }
                    open = skip_whitespace(b, matching_angle(b, open) + 1);
                }
                if b.get(open) == Some(&b'(') {
                    out.push((open + 1, matching_close(b, open)));
                }
            }
            out
        }

        /// Each invocation of the macro `name!` in `skel` — `name!(…)`,
        /// `name ! [ … ]`, `name!{…}` — as the index of its opening delimiter
        /// and of the delimiter closing it. A longer identifier ending in
        /// `name` is not an invocation.
        fn macro_calls(&self, name: &str) -> Vec<(usize, usize)> {
            let b = self.skel.as_bytes();
            let mut out = Vec::new();
            let mut from = 0usize;
            while let Some(rel) = self.skel.get(from..).and_then(|rest| rest.find(name)) {
                let at = from + rel;
                from = at + name.len();
                if (at > 0 && is_ident_byte(b[at - 1]))
                    || b.get(from).copied().is_some_and(is_ident_byte)
                {
                    continue;
                }
                let bang = skip_whitespace(b, from);
                if b.get(bang) != Some(&b'!') {
                    continue;
                }
                let open = skip_whitespace(b, bang + 1);
                if matches!(b.get(open), Some(b'(' | b'[' | b'{')) {
                    let close = matching_close(b, open);
                    out.push((open, close));
                    from = close.max(from);
                }
            }
            out
        }

        /// Whether the census reads this file: its code (comments and
        /// literals aside) mentions `axum`, names the word `Router` or a
        /// `routing::<verb>` / `routing::MethodRouter` path — a file split out
        /// of a router module that reaches axum only through `use super::*;`
        /// still builds a `Router` — or defines a `macro_rules!`: a macro
        /// body names no types, and `add_dual!`'s own body is the only record
        /// of the two paths each invocation expands to. So a `.merge(` or
        /// `.route(` on some other type (a commit-report merge, a bandit
        /// router) is not taken for an HTTP registration.
        fn is_router_source(&self) -> bool {
            let b = self.skel.as_bytes();
            // `word` not run into a longer identifier on either side, and
            // followed (whitespace aside) by `then` when one is given.
            let names_word = |word: &str, then: Option<u8>| {
                let ends_in_ident = word.bytes().last().is_some_and(is_ident_byte);
                self.skel.match_indices(word).any(|(at, _)| {
                    let after = at + word.len();
                    (at == 0 || !is_ident_byte(b[at - 1]))
                        && !(ends_in_ident && b.get(after).copied().is_some_and(is_ident_byte))
                        && then.is_none_or(|next| b.get(skip_whitespace(b, after)) == Some(&next))
                })
            };
            // `routing::get` / `routing::MethodRouter`, but not this crate's own
            // `crate::routing::q_router` (the model router, not axum's).
            let names_axum_routing = self.skel.match_indices("routing::").any(|(at, word)| {
                let rest = self.skel.get(at + word.len()..).unwrap_or("");
                let len = rest.bytes().take_while(|&c| is_ident_byte(c)).count();
                let ident = rest.get(..len).unwrap_or("");
                (at == 0 || !is_ident_byte(b[at - 1]))
                    && (ROUTING_VERBS.contains(&ident.strip_suffix("_service").unwrap_or(ident))
                        || ident == "MethodRouter")
            });
            self.skel.contains("axum")
                || names_word("Router", None)
                || names_axum_routing
                || names_word("macro_rules", Some(b'!'))
        }

        /// Whether the whole file is test-only: an inner `#![cfg(test)]` (or
        /// `#![cfg(all(…, test, …))]`) among the attributes opening it.
        fn is_test_only_file(&self) -> bool {
            let b = self.skel.as_bytes();
            let mut i = skip_whitespace(b, 0);
            while b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'!') {
                let open = skip_whitespace(b, i + 2);
                if b.get(open) != Some(&b'[') {
                    break;
                }
                let close = matching_close(b, open);
                if is_test_cfg(&stripped(self.skel.get(open + 1..close).unwrap_or(""))) {
                    return true;
                }
                i = skip_whitespace(b, close + 1);
            }
            false
        }
    }

    /// The keywords a gated ITEM or statement opens with. Anything else under
    /// a test-only cfg is a field, variant or arm, and ends at its `,`.
    const ITEM_KEYWORDS: &[&str] = &[
        "fn",
        "mod",
        "impl",
        "struct",
        "enum",
        "trait",
        "type",
        "const",
        "static",
        "use",
        "let",
        "macro_rules",
        "pub",
        "unsafe",
        "async",
        "extern",
    ];

    /// `i` advanced past any ASCII whitespace in `b`.
    fn skip_whitespace(b: &[u8], mut i: usize) -> usize {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    }

    /// The index of the `>` matching the `<` at `open` (or the end) — for a
    /// turbofish, whose angle brackets [`matching_close`] does not count.
    fn matching_angle(b: &[u8], open: usize) -> usize {
        let mut depth = 0i32;
        for (i, &c) in b.iter().enumerate().skip(open) {
            match c {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        b.len()
    }

    /// The index of the bracket opening the one at `close` (a `)`, `]` or
    /// `}`), searching backwards, or `None` when it is unbalanced.
    fn matching_open(b: &[u8], close: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = close + 1;
        while i > 0 {
            i -= 1;
            match b[i] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// `text` with every whitespace character removed.
    fn stripped(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// Whether an attribute body (whitespace removed, literal contents
    /// blanked) gates on `test`: `cfg(test)`, or `cfg(all(…))` with `test`
    /// among its top-level arguments (`cfg(all(feature="x",test))`).
    fn is_test_cfg(attr: &str) -> bool {
        if attr == "cfg(test)" {
            return true;
        }
        let Some(args) = attr
            .strip_prefix("cfg(all(")
            .and_then(|rest| rest.strip_suffix("))"))
        else {
            return false;
        };
        let b = args.as_bytes();
        let mut start = 0usize;
        loop {
            let comma = top_level_comma(b, start, b.len());
            let end = comma.unwrap_or(b.len());
            if args.get(start..end) == Some("test") {
                return true;
            }
            match comma {
                Some(c) => start = c + 1,
                None => return false,
            }
        }
    }

    /// `path` with `.` components dropped and each `..` resolved against
    /// the component before it.
    fn normalized(path: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        parts.join("/")
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
    /// one enumerator rather than growing a second. It reads the same
    /// registration walks as the HTTP route census — literal-path `.route(…)`
    /// calls ([`walk_route_calls`]) and the two routes of every
    /// `add_dual!(…)` ([`walk_add_dual`]) — over every file, test code
    /// included.
    pub(crate) fn registered_routes() -> std::collections::HashSet<(String, String)> {
        let mut out = std::collections::HashSet::new();
        for (_, src) in crate_rust_sources() {
            registered_in(&Lexed::new(&src), &mut out);
        }
        out
    }

    /// One file's contribution to [`registered_routes`]: its literal-path
    /// `.route(…)` calls and both routes of each readable `add_dual!(…)`.
    fn registered_in(lx: &Lexed, out: &mut std::collections::HashSet<(String, String)>) {
        collect_routes(lx, ROUTING_VERBS, out);
        walk_add_dual(lx, |dual| {
            if let AddDual::Read { method, tail, .. } = dual {
                for path in dual_paths(tail) {
                    out.insert((method.clone(), path));
                }
            }
        });
    }

    /// One registration call site, as [`walk_route_calls`] reads it.
    struct RouteCall<'a> {
        path: RoutePath<'a>,
        /// Everything after the path argument, up to the `)` closing the
        /// call, from the literal-blanked view.
        chain: &'a str,
    }

    /// A registration call's first argument.
    enum RoutePath<'a> {
        /// A string literal: the registered path itself.
        Literal(&'a str),
        /// A constant or binding (`ROUTE_PATH`, `template`) whose value a
        /// source scan cannot read — recorded by its last path segment, so the
        /// site still counts and a `super::`/`crate::` qualifier added or
        /// dropped by a move does not change it.
        Named(&'a str),
        /// Anything else (`&format!(…)`, `concat!(…)`): the argument text,
        /// comments blanked, so the site is visible as a blind spot.
        Unread(&'a str),
    }

    /// THE walk: every `needle` call in the file, its first argument
    /// classified, with the text of the rest of the call. Comments read as
    /// whitespace and calls inside literals are not calls. Both
    /// [`collect_routes`] and the HTTP route census read the tree through this
    /// one walk, so they cannot disagree about which calls exist.
    fn walk_route_calls<'a>(lx: &'a Lexed, needle: &str, mut visit: impl FnMut(RouteCall<'a>)) {
        let skel = lx.skel.as_bytes();
        for (open, close) in lx.calls(needle) {
            let mut first = open;
            while first < close && skel[first].is_ascii_whitespace() {
                first += 1;
            }
            let Some(arg) = lx.skel.get(first..close) else {
                continue;
            };
            let name_len = arg
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .unwrap_or(arg.len());
            let (path, chain_start) = if arg.starts_with('"') {
                let Some(len) = arg.get(1..).and_then(|rest| rest.find('"')) else {
                    continue;
                };
                let Some(path) = lx.code.get(first + 1..first + 1 + len) else {
                    continue;
                };
                (RoutePath::Literal(path), first + 1 + len + 1)
            } else if name_len > 0
                && arg
                    .get(name_len..)
                    .is_some_and(|rest| rest.trim_start().starts_with(','))
            {
                let name = arg.get(..name_len).unwrap_or("");
                let last = name.rsplit("::").find(|s| !s.is_empty()).unwrap_or(name);
                (RoutePath::Named(last), first + name_len)
            } else {
                let comma = top_level_comma(skel, first, close).unwrap_or(close);
                let text = lx.code.get(first..comma).unwrap_or("");
                (RoutePath::Unread(text), comma)
            };
            let Some(chain) = lx.skel.get(chain_start..close) else {
                continue;
            };
            visit(RouteCall { path, chain });
        }
    }

    /// The first `,` at bracket depth 0 in `b[from..to)`.
    fn top_level_comma(b: &[u8], from: usize, to: usize) -> Option<usize> {
        let mut i = from;
        while i < to {
            match b[i] {
                b'(' | b'[' | b'{' => i = matching_close(b, i),
                b',' => return Some(i),
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Pull `(METHOD, path)` pairs out of one file's literal-path `.route(…)`
    /// calls.
    fn collect_routes(
        lx: &Lexed,
        verbs: &[&str],
        out: &mut std::collections::HashSet<(String, String)>,
    ) {
        walk_route_calls(lx, ROUTE_CALL, |call| {
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
    /// The non-vacuity floor: ~70% of the census as regenerated on 2026-10-05
    /// (1789 lines). A walk that silently stops reading — a lexer that
    /// swallows a whole file, a needle that no longer matches — falls far
    /// below it, while ordinary route churn never approaches it.
    const MIN_CENSUS_LINES: usize = 1252;

    /// The router-composition methods the census reads besides `.route`: the
    /// sites that splice a whole sub-router (or a fallback) into the tree.
    /// Dropping one drops every route behind it, which no `.route(` line shows.
    const COMPOSITION_CALLS: &[(&str, &str, bool)] = &[
        (".merge", "MERGE", false),
        (".nest", "NEST", true),
        (".nest_service", "NEST_SERVICE", true),
        (".fallback", "FALLBACK", false),
        (".fallback_service", "FALLBACK_SERVICE", false),
    ];

    /// The macro that registers one handler under both `/ui-bridge/control/`
    /// and `/ui-bridge/ai/` (`mcp::ui_bridge::routing`).
    const ADD_DUAL_MACRO: &str = "add_dual";

    /// Whether `rel` (relative to `CARGO_MANIFEST_DIR`) is a test-only file:
    /// `tests.rs`, anything under a `tests/` directory, or a file some other
    /// file declares as a test-only module (`#[cfg(test)] mod
    /// tier_matrix_tests;`). A `*_tests.rs` NAME alone does not qualify —
    /// `mcp/image_quality_tests.rs` and `mcp/verification_tests.rs` are
    /// production route families.
    fn is_test_file(rel: &str, test_module_files: &std::collections::HashSet<String>) -> bool {
        let mut parts = rel.split('/');
        let file = parts.next_back().unwrap_or("");
        file == "tests.rs" || test_module_files.contains(rel) || parts.any(|dir| dir == "tests")
    }

    /// The `.rs` files under `src` that git tracks, relative to
    /// `CARGO_MANIFEST_DIR` — or why git could not answer (no checkout, no
    /// git binary), in which case the census reads every file on disk.
    fn git_tracked_sources() -> Result<std::collections::HashSet<String>, String> {
        let out = std::process::Command::new("git")
            .args(["ls-files", "-z", "--", "src"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .map_err(|e| format!("running git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git ls-files exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listed: std::collections::HashSet<String> = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| p.ends_with(".rs"))
            .map(str::to_string)
            .collect();
        if listed.is_empty() {
            return Err("git ls-files listed no .rs file under src".to_string());
        }
        Ok(listed)
    }

    /// Every registration site in the runner's non-test source as one line,
    /// sorted, duplicates kept:
    ///
    /// - `<METHODS> <path> <handler>` — a `.route(…)`, or one of the two
    ///   routes an `add_dual!(…)` expands to;
    /// - `? <path> <expr>` — a `.route(…)` whose `MethodRouter` is built
    ///   elsewhere; `SERVICE <path> <expr>` — a `.route_service(…)`;
    /// - `MERGE <target>`, `NEST <prefix> <target>`, `NEST_SERVICE …`,
    ///   `FALLBACK <target>`, `FALLBACK_SERVICE <target>` — a composition site;
    /// - `UNREAD <text>` — a registration whose path argument a source scan
    ///   cannot classify: a visible blind spot rather than a silent one.
    ///
    /// There is deliberately NO file or module column, and names are reduced
    /// past their module qualifiers: moving a handler, a sub-router or a whole
    /// router between files leaves the census identical, while dropping a
    /// route or a merge, changing a method set or rebinding a handler changes
    /// it.
    ///
    /// Read from: every `.rs` under `src` that git TRACKS (all of them, with a
    /// note on stderr, when git cannot answer) that is a router source
    /// ([`Lexed::is_router_source`]), less test-only files ([`is_test_file`], an inner `#![cfg(test)]`, and —
    /// transitively — every module a test-only file or item declares) and
    /// test-only items. A route in a new file is therefore counted only once
    /// the file is `git add`ed.
    ///
    /// Returns the sorted lines and the file set they were read from (which
    /// names the git fallback when it was taken), for the failure message.
    pub(crate) fn http_route_census_lines() -> (Vec<String>, String) {
        let (tracked, file_set) = match git_tracked_sources() {
            Ok(tracked) => (Some(tracked), "the .rs files git tracks".to_string()),
            Err(why) => {
                let file_set = format!(
                    "EVERY .rs file on disk, untracked ones included — git could not answer \
                     ({why})"
                );
                eprintln!("http_route_census: reading {file_set}");
                (None, file_set)
            }
        };
        let sources: Vec<(String, Lexed)> = crate_rust_sources()
            .into_iter()
            .filter(|(rel, _)| tracked.as_ref().is_none_or(|t| t.contains(rel)))
            .map(|(rel, src)| (rel, Lexed::new(&src)))
            .collect();
        let test_files = test_only_files(&sources);
        let mut lines = Vec::new();
        for (rel, lx) in sources {
            if lx.is_router_source() && !test_files.contains(&rel) {
                census_lexed(lx, &mut lines);
            }
        }
        lines.sort();
        (lines, file_set)
    }

    /// Every test-only file among `sources`: [`is_test_file`] by name or
    /// declaration, an inner `#![cfg(test)]`, or a module some test-only file
    /// or item declares — followed to a fixpoint, so a test module's own
    /// submodules count too.
    fn test_only_files(sources: &[(String, Lexed)]) -> std::collections::HashSet<String> {
        let mut test_files = std::collections::HashSet::new();
        loop {
            let before = test_files.len();
            for (rel, lx) in sources {
                let whole = is_test_file(rel, &test_files) || lx.is_test_only_file();
                if whole {
                    test_files.insert(rel.clone());
                }
                test_files.extend(lx.test_module_files(rel, whole));
            }
            if test_files.len() == before {
                return test_files;
            }
        }
    }

    /// The census lines of one file's source, `#[cfg(test)]` items excluded.
    fn census_one_file(src: &str, out: &mut Vec<String>) {
        census_lexed(Lexed::new(src), out);
    }

    /// [`census_one_file`] over an already-lexed file.
    fn census_lexed(mut lx: Lexed, out: &mut Vec<String>) {
        lx.blank_test_items();
        let lx = &lx;
        for (needle, is_service) in [(ROUTE_CALL, false), (ROUTE_SERVICE_CALL, true)] {
            walk_route_calls(lx, needle, |call| {
                let path = match call.path {
                    RoutePath::Literal(p) => p.to_string(),
                    RoutePath::Named(name) => format!("<{name}>"),
                    RoutePath::Unread(text) => {
                        out.push(format!("UNREAD {}", collapsed(text)));
                        return;
                    }
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
                push_method_lines(&path, pairs, out);
            });
        }
        census_compositions(lx, out);
        census_add_dual(lx, out);
    }

    /// One `<METHODS> <path> <handler>` line per handler of a route.
    fn push_method_lines(path: &str, pairs: Vec<(String, String)>, out: &mut Vec<String>) {
        let mut by_handler: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for (verb, handler) in pairs {
            by_handler.entry(handler).or_default().insert(verb);
        }
        for (handler, verbs) in by_handler {
            let verbs: Vec<String> = verbs.into_iter().collect();
            out.push(format!("{} {path} {handler}", verbs.join(",")));
        }
    }

    /// The `MERGE` / `NEST` / `FALLBACK` lines of one file.
    fn census_compositions(lx: &Lexed, out: &mut Vec<String>) {
        let skel = lx.skel.as_bytes();
        for &(needle, label, has_prefix) in COMPOSITION_CALLS {
            for (open, close) in lx.calls(needle) {
                if !has_prefix {
                    let target = composition_target(lx.skel.get(open..close).unwrap_or(""));
                    out.push(format!("{label} {target}"));
                    continue;
                }
                let comma = top_level_comma(skel, open, close).unwrap_or(close);
                let prefix = lx.code.get(open..comma).unwrap_or("").trim();
                let is_name = !prefix.is_empty()
                    && prefix
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':');
                let prefix = match prefix.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                    Some(literal) => literal.to_string(),
                    // A named prefix by its last segment, as a named route path.
                    None if is_name => {
                        format!(
                            "<{}>",
                            prefix
                                .rsplit("::")
                                .find(|s| !s.is_empty())
                                .unwrap_or(prefix)
                        )
                    }
                    None => format!("<{}>", collapsed(prefix)),
                };
                let target =
                    composition_target(lx.skel.get((comma + 1).min(close)..close).unwrap_or(""));
                out.push(format!("{label} {prefix} {target}"));
            }
        }
    }

    /// The name a composition argument (`expr`, from the literal-blanked
    /// view) is recorded under: the last two segments of a leading path once
    /// any `self` / `super` / `crate` qualifiers are dropped
    /// (`crate::mcp::canvas::routes()` and `super::canvas::routes()` →
    /// `canvas::routes`, `self::sub` → `sub`) — enough to keep distinct
    /// sub-routers distinct while a move's qualifier change drops out —
    /// `<closure>` for a closure, `<if: a | b>` / `<match: a | b>` for a
    /// conditional, naming each branch's value the same way (so dropping a
    /// sub-router from one branch changes the line), and `<expr>` for
    /// anything else that does not start with a path.
    fn composition_target(expr: &str) -> String {
        let expr = expr.trim();
        if is_closure(expr) {
            return "<closure>".to_string();
        }
        let len = expr
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(expr.len());
        let path = expr.get(..len).unwrap_or("").trim_end_matches(':');
        let segments: Vec<&str> = path
            .split("::")
            .filter(|s| !s.is_empty())
            .skip_while(|s| matches!(*s, "self" | "super" | "crate"))
            .collect();
        match segments.as_slice() {
            ["if"] => {
                let branches = if_branches(expr.as_bytes());
                return format!("<if: {}>", branch_targets(expr, &branches));
            }
            ["match"] => {
                let arms = match_arms(expr.as_bytes());
                return format!("<match: {}>", branch_targets(expr, &arms));
            }
            [only] if matches!(*only, "unsafe" | "loop" | "async" | "move") => {
                return "<expr>".to_string();
            }
            [] => return "<expr>".to_string(),
            _ => {}
        }
        segments
            .get(segments.len().saturating_sub(2)..)
            .map(|s| s.join("::"))
            .unwrap_or_default()
    }

    /// The branch values `[from, to)` of each `branches` span in `expr`,
    /// each as [`composition_target`] names it, joined by ` | `.
    fn branch_targets(expr: &str, branches: &[(usize, usize)]) -> String {
        branches
            .iter()
            .map(|&(from, to)| composition_target(expr.get(from..to).unwrap_or("")))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// The index of the first `{` at bracket depth 0 in `b[from..)`, skipping
    /// any `(…)` / `[…]` on the way — the block after an `if` condition or a
    /// `match` scrutinee.
    fn first_block(b: &[u8], mut i: usize) -> Option<usize> {
        while i < b.len() {
            match b[i] {
                b'{' => return Some(i),
                b'(' | b'[' => i = matching_close(b, i) + 1,
                _ => i += 1,
            }
        }
        None
    }

    /// The value of the block whose `{` is at `open`: the text after its last
    /// depth-0 `;`, as `(from, to)` in `b`.
    fn block_value(b: &[u8], open: usize) -> (usize, usize) {
        let close = matching_close(b, open);
        let mut from = open + 1;
        let mut i = from;
        while i < close {
            match b[i] {
                b'(' | b'[' | b'{' => i = matching_close(b, i) + 1,
                b';' => {
                    from = i + 1;
                    i += 1;
                }
                _ => i += 1,
            }
        }
        (from, close)
    }

    /// The value spans of every branch of the `if … { … } else …` in `b`,
    /// `else if` chains followed.
    fn if_branches(b: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while let Some(open) = first_block(b, i) {
            out.push(block_value(b, open));
            let next = skip_whitespace(b, matching_close(b, open) + 1);
            if b.get(next..next + 4) != Some(&b"else"[..]) {
                break;
            }
            let after = skip_whitespace(b, next + 4);
            if b.get(after) == Some(&b'{') {
                out.push(block_value(b, after));
                break;
            }
            // `else if …`: the next block is that `if`'s.
            i = after;
        }
        out
    }

    /// The value spans of every arm of the `match … { … }` in `b`: each
    /// arm's body after its `=>`, a block body by its value.
    fn match_arms(b: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let Some(open) = first_block(b, 0) else {
            return out;
        };
        let close = matching_close(b, open);
        let mut i = open + 1;
        while i < close {
            match b[i] {
                b'(' | b'[' | b'{' => i = matching_close(b, i) + 1,
                b'=' if b.get(i + 1) == Some(&b'>') => {
                    let body = skip_whitespace(b, i + 2);
                    if b.get(body) == Some(&b'{') {
                        out.push(block_value(b, body));
                        i = matching_close(b, body) + 1;
                    } else {
                        let end = top_level_comma(b, body, close).unwrap_or(close);
                        out.push((body, end));
                        i = end;
                    }
                }
                _ => i += 1,
            }
        }
        out
    }

    /// One `add_dual!(…)` invocation, as [`walk_add_dual`] reads it.
    enum AddDual<'a> {
        /// `add_dual!(router, <verb>, "<tail>", handler)`: the upper-cased
        /// verb, the tail, and the handler's name.
        Read {
            method: String,
            tail: &'a str,
            handler: String,
        },
        /// Any other argument shape: the invocation's text, comments blanked.
        Unread(&'a str),
    }

    /// The two paths an `add_dual!` tail is registered under
    /// (`mcp::ui_bridge::routing`).
    fn dual_paths(tail: &str) -> [String; 2] {
        ["control", "ai"].map(|ns| format!("/ui-bridge/{ns}/{tail}"))
    }

    /// THE `add_dual!` walk: every invocation in the file, its arguments
    /// read. Shared by [`registered_routes`] and the HTTP route census.
    fn walk_add_dual<'a>(lx: &'a Lexed, mut visit: impl FnMut(AddDual<'a>)) {
        let skel = lx.skel.as_bytes();
        for (open, close) in lx.macro_calls(ADD_DUAL_MACRO) {
            let mut args = Vec::new();
            let mut start = open + 1;
            while let Some(comma) = top_level_comma(skel, start, close) {
                args.push((start, comma));
                start = comma + 1;
            }
            args.push((start, close));
            let text = |(a, b): (usize, usize)| lx.code.get(a..b).unwrap_or("").trim();
            let read = match args.as_slice() {
                [_, method, tail, handler] => {
                    let method = text(*method);
                    let tail = text(*tail)
                        .strip_prefix('"')
                        .and_then(|t| t.strip_suffix('"'));
                    match tail {
                        Some(tail) if ROUTING_VERBS.contains(&method) => Some(AddDual::Read {
                            method: method.to_ascii_uppercase(),
                            tail,
                            handler: handler_name(lx.skel.get(handler.0..handler.1).unwrap_or("")),
                        }),
                        _ => None,
                    }
                }
                _ => None,
            };
            visit(read.unwrap_or_else(|| {
                // From the macro's name, wherever whitespace put the `!`.
                let name_at = lx
                    .skel
                    .get(..open)
                    .and_then(|before| before.rfind(ADD_DUAL_MACRO))
                    .unwrap_or(open);
                AddDual::Unread(lx.code.get(name_at..close + 1).unwrap_or(""))
            }));
        }
    }

    /// The census lines of every `add_dual!` in one file: its two routes, or
    /// an `UNREAD` line when its arguments are not in the expected shape.
    fn census_add_dual(lx: &Lexed, out: &mut Vec<String>) {
        walk_add_dual(lx, |dual| match dual {
            AddDual::Read {
                method,
                tail,
                handler,
            } => {
                for path in dual_paths(tail) {
                    out.push(format!("{method} {path} {handler}"));
                }
            }
            AddDual::Unread(text) => out.push(format!("UNREAD {}", collapsed(text))),
        });
    }

    /// `text` with every whitespace run collapsed to one space, cut to 40
    /// characters — the stable rendering of an argument the census cannot read.
    fn collapsed(text: &str) -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(40)
            .collect()
    }

    fn is_ident_byte(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_'
    }

    /// Whether `expr` begins a closure: `|…|`, or the keyword `move` /
    /// `async` — as a whole word, so `move_terminal_handler` is a name.
    fn is_closure(expr: &str) -> bool {
        let starts_word = |kw: &str| {
            expr.strip_prefix(kw)
                .is_some_and(|rest| !rest.bytes().next().is_some_and(is_ident_byte))
        };
        expr.starts_with('|') || starts_word("move") || starts_word("async")
    }

    /// The `(VERB, handler)` pairs of a `MethodRouter` chain such as
    /// `get(a).post(b).layer(…)`, read from the literal-blanked view. Only
    /// verbs at the chain's top level count, so a verb inside a handler
    /// closure or a layer argument is not mistaken for a registration.
    /// `get_service(…)` and friends count as their verb.
    fn method_handlers(chain: &str) -> Vec<(String, String)> {
        let b = chain.as_bytes();
        let mut out = Vec::new();
        let mut depth = 0i32;
        let mut i = 0usize;
        while i < b.len() {
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

    /// The index of the bracket matching the one at `open` (or the end), in
    /// text whose comments and literal contents are already blanked.
    fn matching_close(b: &[u8], open: usize) -> usize {
        let mut depth = 0i32;
        let mut i = open;
        while i < b.len() {
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
        if is_closure(expr) {
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
        let name = segments
            .get(keep_from..)
            .map(|s| s.join("::"))
            .unwrap_or_default();
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

    /// **The HTTP route census.** Every route registration and router
    /// composition in the runner's non-test source, pinned against
    /// `src-tauri/http-routes.snapshot.txt`. A dropped route, merge or nest, a
    /// changed method set or a rebound handler fails it; moving code between
    /// files does not, because the census carries no file column. Guards the
    /// `mcp_api.rs` split (plan
    /// `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`).
    #[test]
    fn http_route_census() {
        let (actual, file_set) = http_route_census_lines();
        assert!(
            actual.len() >= MIN_CENSUS_LINES,
            "the route census found only {} lines (floor {MIN_CENSUS_LINES}) in {file_set} — \
             the walk is broken, not the router",
            actual.len()
        );
        let snapshot =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(HTTP_ROUTE_SNAPSHOT);
        if std::env::var(UPDATE_HTTP_ROUTE_SNAPSHOT).as_deref() == Ok("1") {
            let mut text = String::from(
                "# HTTP route census: every route registration and router composition in the\n\
                 # runner's non-test source under src-tauri/src (git-tracked files; test files\n\
                 # and #[cfg(test)] items excluded), one line each, sorted, duplicates kept.\n\
                 # `<METHODS> <path> <handler>` = a route (add_dual! counts as its two routes);\n\
                 # `?` = a MethodRouter built elsewhere; `SERVICE` = a route_service mount;\n\
                 # `<name>` = a path held in a constant or binding; MERGE / NEST / NEST_SERVICE\n\
                 # / FALLBACK / FALLBACK_SERVICE = a composition site; UNREAD = a registration\n\
                 # whose path argument the scan cannot read. No file column, so a move between\n\
                 # files leaves this unchanged. GENERATED by\n\
                 # mcp::relay_path_policy::tests::http_route_census; regenerate with\n\
                 # UPDATE_HTTP_ROUTE_SNAPSHOT=1.\n",
            );
            for line in &actual {
                text.push_str(line);
                text.push('\n');
            }
            std::fs::write(&snapshot, text)
                .unwrap_or_else(|e| panic!("writing {}: {e}", snapshot.display()));
            eprintln!(
                "http_route_census: rewrote {} ({} lines)",
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
            "the HTTP route table changed (census read from {file_set}).\n\
             \n\
             In the snapshot, missing from the tree ({} — a dropped or re-methoded route, or a \
             dropped merge/nest):\n{}\n\
             \n\
             In the tree, missing from the snapshot ({}):\n{}\n\
             \n\
             If the change is intended, regenerate the snapshot and commit it:\n  \
             {UPDATE_HTTP_ROUTE_SNAPSHOT}=1 bash <workspace-root>/qontinui-claude-config/scripts/\
             cargo-guard.sh test -- http_route_census\n\
             (cargo-guard refuses `--lib`: it narrows TARGETS; a name filter selects the test.)\n\
             A pure move between files never removes a census line: a move may only ADD \
             composition lines (a new `.merge` of a split-out sub-router), so the 'missing from \
             the tree' list above must be empty — anything in it is a registration the move \
             dropped or altered. A route in a file git does not track yet, or in a file whose \
             code names none of `axum`, `Router` or `routing::<verb>` (and defines no macro), is not \
             read — `git add` the file, or add a `use axum::…;` to its code (a comment or a \
             string naming axum does not count).",
            removed.len(),
            removed
                .iter()
                .map(|l| format!("  - {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            added.len(),
            added
                .iter()
                .map(|l| format!("  + {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// The census reader against synthetic sources: methods group per
    /// handler, module paths are dropped, nested verbs, comments and literals
    /// are not read, compositions and `add_dual!` count, unclassifiable paths
    /// surface as `UNREAD`, and test-only items are excluded — while a
    /// test-only struct field, enum variant or match arm hides nothing after
    /// its enclosing scope.
    #[test]
    fn the_census_reads_methods_handlers_and_named_paths() {
        // `@post` is spelled out of the literal and restored by the
        // `.replace` below: `journey::route_coverage` text-scans every `.rs`
        // file for `add_dual!(.., post, "<tail>", <handler>)` and would
        // otherwise read this fixture as two real mounted POST routes
        // (`/ui-bridge/{ai,control}/wait`) and demand they be classified.
        let fixture = r##"
struct Config {
    #[cfg(test)]
    probe: Option<Probe>,
    live: bool,
}

enum Mode {
    #[cfg(test)]
    Mock(u8),
    Live,
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        #[cfg(test)]
        Mode::Mock(_) => "mock",
        Mode::Live => "live",
    }
}

fn mode_router(m: Mode, router: Router) -> Router {
    match m {
        #[cfg(test)]
        Mode::Mock(_) => router,
        Mode::Live => { router.route("/live-arm", get(live_arm)) }
    }
}

fn holder() -> Holder {
    Holder {
        #[cfg(test)]
        probe: None,
        router: Router::new().route("/held", post(held)),
    }
}

fn routes() -> Router {
    let r = Router::new()
        .route("/a", get(crate::mcp::x::alpha).post(crate::mcp::x::alpha))
        .route("/b", axum::routing::get(beta).delete(gamma).layer(from_fn(get_mw)))
        .route("/c", get(|| async { s.get(1) }))
        .route(ROUTE_PATH, put(delta))
        .route(super::QUALIFIED_PATH, put(delta))
        .route("/d", built_elsewhere())
        .route_service("/ws", GraphQLSubscription::new(schema.clone()))
        .route(
            // a comment before the path
            "/e", post(epsilon))
        .route(/* inline */ "/f", get(phi))
        .route(&format!("/g/{}", id), get(gee))
        .route("/m", post(move_terminal_handler))
        .route("/paren", get(paren_handler).layer(x(")")))
        .route ("/spaced", get(spaced))
        .route::<AppState>("/turbofish", get(turbo))
        . route("/dot-spaced", get(dot_spaced))
        // .route("/commented", get(nope))
        .merge(crate::mcp::canvas::routes())
        .merge(super::canvas::routes())
        .merge(self::sub)
        .merge(super::sub)
        .merge(sub)
        .merge(health::sub)
        .merge(if enabled { on_routes() } else { Router::new() })
        .merge(if a { crate::x::routes() }
            else if b { let r = 1; super::y::routes() }
            else { axum::Router::new() })
        .merge(match mode {
            Mode::A => crate::a::routes(),
            Mode::B => { b::routes() }
            _ => |r| r,
        })
        .merge(graphql_routes)
        .nest("/api", crate::api::router())
        .nest_service("/static", ServeDir::new("dist"))
        .fallback(not_found_handler);
    let r = add_dual!(r, @post, "wait", crate::mcp::ui_bridge::wait_handler);
    let r = add_dual ! (r, get, "spaced-dual", spaced_dual);
    let r = add_dual!(r, get, tail_from_const, h);
    let quoted = ".route(\"/in-a-string\", get(nope))";
    let raw = r#".route("/in-a-raw-string", get(nope))"#;
    r
}

#[cfg(test)]
mod tests {
    fn mock() -> Router {
        Router::new().route("/mock", get(mock_handler)).merge(mock_routes())
    }
}

#[cfg(all(test, unix))]
fn unix_mock() -> Router {
    Router::new().route("/unix-mock", get(mock_handler))
}

#[cfg(all(feature = "e2e", test))]
fn e2e_mock() -> Router {
    Router::new().route("/e2e-mock", get(mock_handler))
}
"##
        .replace("@post", "post");
        let src = fixture.as_str();
        let mut lines = Vec::new();
        census_one_file(src, &mut lines);
        lines.sort();
        assert_eq!(
            lines,
            vec![
                "? /d built_elsewhere",
                "DELETE /b gamma",
                "FALLBACK not_found_handler",
                "GET /b beta",
                "GET /c <closure>",
                "GET /dot-spaced dot_spaced",
                "GET /f phi",
                "GET /live-arm live_arm",
                "GET /paren paren_handler",
                "GET /spaced spaced",
                "GET /turbofish turbo",
                "GET /ui-bridge/ai/spaced-dual spaced_dual",
                "GET /ui-bridge/control/spaced-dual spaced_dual",
                "GET,POST /a alpha",
                "MERGE <if: on_routes | Router::new>",
                "MERGE <if: x::routes | y::routes | Router::new>",
                "MERGE <match: a::routes | b::routes | <closure>>",
                "MERGE canvas::routes",
                "MERGE canvas::routes",
                "MERGE graphql_routes",
                "MERGE health::sub",
                "MERGE sub",
                "MERGE sub",
                "MERGE sub",
                "NEST /api api::router",
                "NEST_SERVICE /static ServeDir::new",
                "POST /e epsilon",
                "POST /held held",
                "POST /m move_terminal_handler",
                "POST /ui-bridge/ai/wait wait_handler",
                "POST /ui-bridge/control/wait wait_handler",
                "PUT <QUALIFIED_PATH> delta",
                "PUT <ROUTE_PATH> delta",
                "SERVICE /ws GraphQLSubscription::new",
                "UNREAD &format!(\"/g/{}\", id)",
                "UNREAD add_dual!(r, get, tail_from_const, h)",
            ]
        );

        // `registered_routes` reads the same walks: literal `.route` paths
        // and both routes of each readable `add_dual!`.
        let mut registered = std::collections::HashSet::new();
        registered_in(&Lexed::new(src), &mut registered);
        for (m, p) in [
            ("POST", "/ui-bridge/control/wait"),
            ("POST", "/ui-bridge/ai/wait"),
            ("GET", "/spaced"),
            ("GET", "/turbofish"),
        ] {
            assert!(
                registered.contains(&(m.to_string(), p.to_string())),
                "{m} {p} not registered"
            );
        }
        assert!(Lexed::new(src).is_router_source());
        assert!(
            Lexed::new("macro_rules! m { () => { r.route(\"/x\", get(h)) } }").is_router_source()
        );
        assert!(
            !Lexed::new("// axum\nfn f(o: Other) { o.merge(next).route(&ctx) }").is_router_source()
        );
        assert!(
            !Lexed::new("fn f(o: BanditRouter) { o.route(\"Router\") } // Router")
                .is_router_source()
        );

        // A file split out of a router module reaches axum through
        // `use super::*;` alone and is still read.
        let split = "use super::*;\n\npub(super) fn routes() -> Router {\n    \
                     Router::new().route(\"/split\", get(split_handler))\n}\n";
        assert!(Lexed::new(split).is_router_source());
        let via_routing = "use super::*;\nfn add(r: R) -> R { r.route(\"/r\", routing::get(h)) }";
        assert!(Lexed::new(via_routing).is_router_source());
        let model_router = "fn pick(t: &crate::routing::q_router::TaskState) { r.route(&t) }";
        assert!(!Lexed::new(model_router).is_router_source());
        let mut lines = Vec::new();
        census_one_file(split, &mut lines);
        assert_eq!(lines, vec!["GET /split split_handler"]);
    }

    /// Which files are test-only: by name, by an inner `#![cfg(test)]`, and
    /// by a test-only `mod` declaration — `#[path]` read wherever it sits in
    /// the attribute stack and normalised, enclosing inline modules honoured,
    /// and a test module's own submodules followed.
    #[test]
    fn the_census_finds_test_only_files() {
        assert!(Lexed::new("//! doc\n#![cfg(test)]\nuse x;\n").is_test_only_file());
        assert!(
            Lexed::new("#![allow(dead_code)]\n#![cfg(all(feature = \"x\", test))]\n")
                .is_test_only_file()
        );
        assert!(!Lexed::new("#![cfg(feature = \"test\")]\nfn f() {}\n").is_test_only_file());
        assert!(!Lexed::new("fn f() {}\n#[cfg(test)]\nmod tests {}\n").is_test_only_file());

        let decls = Lexed::new(concat!(
            "#[cfg(test)]\n#[path = \"p_tests.rs\"]\nmod p;\n",
            "#[path = \"../shared/r_tests.rs\"]\n#[cfg(test)]\nmod r;\n",
            "#[allow(clippy::path_buf_push_overwrite)]\n#[cfg(test)]\n#[path = \"s_tests.rs\"]\nmod s;\n",
            "#[cfg(test)]\npub(crate) mod q_tests;\n",
            "pub mod image_quality_tests;\n",
            "mod outer {\n    #[cfg(test)]\n    mod inner_tests;\n}\n",
            "#[cfg(test)]\nmod tests {\n    mod helpers;\n}\n",
        ))
        .test_module_files("src/mcp/foo.rs", false);
        assert_eq!(
            decls,
            vec![
                "src/mcp/p_tests.rs",
                "src/shared/r_tests.rs",
                "src/mcp/s_tests.rs",
                "src/mcp/foo/q_tests.rs",
                "src/mcp/foo/q_tests/mod.rs",
                "src/mcp/foo/outer/inner_tests.rs",
                "src/mcp/foo/outer/inner_tests/mod.rs",
                "src/mcp/foo/tests/helpers.rs",
                "src/mcp/foo/tests/helpers/mod.rs",
            ]
        );
        assert_eq!(
            Lexed::new("pub mod a;\n").test_module_files("src/mcp/mod.rs", true),
            vec!["src/mcp/a.rs", "src/mcp/a/mod.rs"]
        );

        let sources: Vec<(String, Lexed)> = [
            ("src/mcp/foo.rs", "#[cfg(test)]\nmod foo_tests;\n"),
            ("src/mcp/foo/foo_tests.rs", "mod deeper;\n"),
            ("src/mcp/foo/foo_tests/deeper.rs", "fn f() {}\n"),
            ("src/mcp/e2e.rs", "#![cfg(test)]\nmod part;\n"),
            ("src/mcp/e2e/part.rs", "fn f() {}\n"),
            ("src/mcp/origin_guard/tests.rs", "fn f() {}\n"),
            ("src/mcp/image_quality_tests.rs", "fn routes() {}\n"),
        ]
        .into_iter()
        .map(|(rel, src)| (rel.to_string(), Lexed::new(src)))
        .collect();
        let test_files = test_only_files(&sources);
        for rel in [
            "src/mcp/foo/foo_tests.rs",
            "src/mcp/foo/foo_tests/deeper.rs",
            "src/mcp/e2e.rs",
            "src/mcp/e2e/part.rs",
            "src/mcp/origin_guard/tests.rs",
        ] {
            assert!(test_files.contains(rel), "{rel} not test-only");
        }
        for rel in ["src/mcp/foo.rs", "src/mcp/image_quality_tests.rs"] {
            assert!(!test_files.contains(rel), "{rel} taken for test-only");
        }
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
