//! Which local API paths the `http_request` relay arm may reach.
//!
//! # Why this exists
//!
//! `backend_relay`'s `http_request` arm is an unrestricted loopback HTTP proxy:
//! the frame chooses the method, the path, the headers and the body, and the
//! runner self-calls its OWN Axum server — which "binds the IPv4 loopback only
//! and these routes carry no further gate" (`mcp::terminals`). Phase 3b hardened
//! the TYPED `terminal_create` frame so a blockless frame can no longer choose
//! `working_dir`, `intent_repo` or `agent_session_id`. That gate is worth
//! nothing while the sibling arm of the same `match` reaches
//! `POST /terminals` with those three fields verbatim — same socket, same
//! party, identical outcome (review round 2, finding 1).
//!
//! # The shape of the policy
//!
//! A blanket allowlist over the whole runner API is not available: `routes()`
//! in `mcp_api` merges ~60 modules and the mobile remote-runner relay exists to
//! reach them generically. What IS available, and is what this module
//! implements, is a **closed-by-default guarded namespace**:
//!
//! * [`GUARDED_PREFIXES`] names the path prefixes that spawn, write to, or kill
//!   a terminal. Everything at or under one of them is refused **whatever the
//!   method**, unless it appears on [`RELAY_ALLOWED_IN_GUARDED`] — which is
//!   EMPTY today.
//! * A route added under a guarded prefix later is therefore refused without
//!   anyone remembering this file — the property a denylist cannot have. The
//!   `terminals` module's own [`crate::mcp::terminals::route_entries`] is
//!   asserted against this policy in that module's tests, and a `.route(` call
//!   added there without a matching entry fails a second test, so the
//!   enumeration cannot silently drift either.
//!
//! # What is guarded, and why each one
//!
//! | Prefix | Why |
//! |---|---|
//! | `terminals` | `POST /terminals` (spawn, with `working_dir` / `intent_repo` / `agent_session_id`), `POST /terminals/{id}/write` and `/submit-prompt` (stdin), `DELETE /terminals/{id}` (kill), `/resize`, `/move`, `/ws` (full duplex I/O). The list/buffer reads are in the same namespace and are closed with them: nothing reaches them over this arm today (see below), so opening them buys nothing and costs the simple rule. |
//! | `terminal-pages` | The same resource, grouped by page. Read-only today; closed for the same reason. |
//! | `ui-bridge/tauri/invoke` | `mcp::tauri_proxy::ALLOWED_PROXIED_COMMANDS` safelists `terminal_create`, `terminal_write`, `terminal_close` and `list_terminals`, and its `terminal_create` arm reads `working_dir`, `intent_repo` AND `agent_session_id` exactly as `POST /terminals` does. Closing `/terminals` while leaving this open would move the hole, not shut it. |
//! | `steward` / `stewards` | `POST /steward/{kind}/start` spawns a PTY and types a launch command into it; `/stop` kills one. It chooses none of the three parameters (fixed spec, `None` working dir), so it is not the working-dir hole — but it is squarely "spawns and kills a terminal", and nothing reaches it over this arm. |
//!
//! # What this does NOT break
//!
//! Measured against the two clients of the arm, 2026-09-11:
//!
//! * **qontinui-mobile.** In `remote` (proxy) mode the terminal tab creates,
//!   closes and lists through `RemoteTerminalClient` — the TYPED
//!   `terminal_create` / `terminal_close` / `terminal_list` frames, i.e. the
//!   gated path. `useTerminalSessions` (the only HTTP `GET /terminals` caller)
//!   is `enabled: mode === 'lan'`, and LAN mode talks to `:9876` directly and
//!   never enters this relay. `TerminalClient`'s other methods are LAN-only for
//!   the same reason.
//! * **qontinui-web.** The single frontend user of
//!   `/api/v1/device-bridge/runner-proxy/*` is `runnerProxyGet` in
//!   `digital-twin/_lib/runner-relay.ts`, used by `useUiBridge` for
//!   `/ui-bridge/*` reads — not the Tauri invoke proxy, and not `/terminals`.
//!
//! # Normalisation
//!
//! The verdict is taken on a normalised form; the request is still FORWARDED
//! verbatim, so a legitimate encoding survives. Normalisation refuses rather
//! than resolves anything ambiguous:
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

/// Path prefixes the `http_request` arm may never reach. Segment-wise
/// prefixes: `terminals` guards `/terminals/{id}/write` too.
///
/// Spelled in the NORMALISED form this module produces — lowercase, no leading
/// slash, `/`-joined.
pub const GUARDED_PREFIXES: &[&str] = &[
    "terminals",
    "terminal-pages",
    "ui-bridge/tauri/invoke",
    "steward",
    "stewards",
];

/// Routes inside a guarded prefix that ARE relayable, as `(method, pattern)`.
///
/// **Empty on purpose.** It is the escape hatch for a future read that a
/// client actually needs over this arm, and it exists rather than being
/// implied so that re-opening one route is an edit to a list instead of a
/// rewrite of the rule. A `{}` segment in the pattern matches exactly one
/// segment.
pub const RELAY_ALLOWED_IN_GUARDED: &[(&str, &str)] = &[];

/// What the relay may do with one `http_request` frame's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayPathVerdict {
    /// Not a terminal-mutating surface — relay it.
    Allow,
    /// The path could not be normalised into something safe to decide on
    /// (traversal, double-encoding, control bytes). Refused rather than
    /// guessed at.
    Malformed,
    /// At or under a [`GUARDED_PREFIXES`] entry and not on
    /// [`RELAY_ALLOWED_IN_GUARDED`].
    Guarded,
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
            RelayPathVerdict::Guarded => {
                "this path is not reachable over the HTTP relay — terminal creation, input and \
                 teardown go through the typed relay frames, which carry the create/attach gate"
            }
        }
    }

    /// A short stable token for logs and tests.
    pub fn code(self) -> &'static str {
        match self {
            RelayPathVerdict::Allow => "allow",
            RelayPathVerdict::Malformed => "relay_path_malformed",
            RelayPathVerdict::Guarded => "relay_path_guarded",
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

/// Does `segments` start with the `/`-joined `prefix`?
fn starts_with_prefix(segments: &[String], prefix: &str) -> bool {
    let wanted: Vec<&str> = prefix.split('/').filter(|s| !s.is_empty()).collect();
    if wanted.is_empty() || segments.len() < wanted.len() {
        return false;
    }
    wanted
        .iter()
        .zip(segments.iter())
        .all(|(w, s)| w.eq_ignore_ascii_case(s))
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

/// The verdict for one frame, against a given policy. Split out from
/// [`relay_path_verdict`] so the allowlist arm is exercised by tests with a
/// synthetic table rather than sitting untested behind an empty const.
pub fn relay_path_verdict_against(
    method: &str,
    raw_path: &str,
    guarded: &[&str],
    allowed: &[(&str, &str)],
) -> RelayPathVerdict {
    let Some(segments) = normalize_relay_path(raw_path) else {
        return RelayPathVerdict::Malformed;
    };
    if !guarded.iter().any(|p| starts_with_prefix(&segments, p)) {
        return RelayPathVerdict::Allow;
    }
    let method = method.trim();
    for (allow_method, pattern) in allowed {
        if allow_method.eq_ignore_ascii_case(method) && matches_pattern(&segments, pattern) {
            return RelayPathVerdict::Allow;
        }
    }
    RelayPathVerdict::Guarded
}

/// The verdict for one `http_request` frame's method + path.
pub fn relay_path_verdict(method: &str, raw_path: &str) -> RelayPathVerdict {
    relay_path_verdict_against(method, raw_path, GUARDED_PREFIXES, RELAY_ALLOWED_IN_GUARDED)
}

#[cfg(test)]
mod tests {
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
    // The verdict
    // ------------------------------------------------------------------

    /// Every evasion the reviewer named, on the route that spawns a PTY.
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
                RelayPathVerdict::Guarded,
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

    /// The whole guarded set, every method. `write` is the attach gate's
    /// equivalent; the tauri-invoke proxy is the create gate's.
    #[test]
    fn every_guarded_prefix_is_closed_to_every_method() {
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
                    RelayPathVerdict::Guarded,
                    "{method} {path}"
                );
            }
        }
    }

    /// The allowlist is EMPTY, and that is a property worth pinning: a later
    /// edit that opens a route must be a deliberate one that also updates the
    /// test above.
    #[test]
    fn nothing_inside_a_guarded_prefix_is_allowlisted_today() {
        assert_eq!(RELAY_ALLOWED_IN_GUARDED, &[] as &[(&str, &str)]);
    }

    /// What stays relayable. This is the set the mobile + digital-twin clients
    /// actually use, plus the generic remainder of the runner API.
    #[test]
    fn the_rest_of_the_api_still_relays() {
        for path in [
            "/health",
            "/status",
            "/ui-bridge/control/page/state",
            "/ui-bridge/control/terminal-sessions",
            "/ui-bridge/control/terminal-sessions/abc",
            "/workflows",
            "/task-runs/running",
            "/hitl/q-1/respond",
            "/worktrees/merge",
            // A sibling of a guarded prefix that merely shares a stem.
            "/terminals-report",
            "/ui-bridge/tauri/other",
        ] {
            for method in ["GET", "POST", "DELETE"] {
                assert_eq!(
                    relay_path_verdict(method, path),
                    RelayPathVerdict::Allow,
                    "{method} {path}"
                );
            }
        }
    }

    /// The allowlist arm, against a synthetic table — the const one is empty,
    /// and an untested escape hatch is not an escape hatch.
    #[test]
    fn an_allowlisted_route_inside_a_guarded_prefix_relays_for_that_method_only() {
        let guarded = ["terminals"];
        let allowed = [("GET", "/terminals"), ("GET", "/terminals/{id}/buffer")];
        let g: Vec<&str> = guarded.to_vec();

        assert_eq!(
            relay_path_verdict_against("GET", "/terminals", &g, &allowed),
            RelayPathVerdict::Allow
        );
        assert_eq!(
            relay_path_verdict_against("get", "/TERMINALS", &g, &allowed),
            RelayPathVerdict::Allow,
            "method and path comparison are both case-insensitive"
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/buffer", &g, &allowed),
            RelayPathVerdict::Allow,
            "{{id}} matches exactly one segment"
        );
        // A different method on the same path is not covered.
        assert_eq!(
            relay_path_verdict_against("POST", "/terminals", &g, &allowed),
            RelayPathVerdict::Guarded
        );
        // The pattern is a whole-path match, not a prefix: no walking past it.
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/buffer/more", &g, &allowed),
            RelayPathVerdict::Guarded
        );
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/xyz/write", &g, &allowed),
            RelayPathVerdict::Guarded
        );
        // And the wildcard does not span separators.
        assert_eq!(
            relay_path_verdict_against("GET", "/terminals/a/b/buffer", &g, &allowed),
            RelayPathVerdict::Guarded
        );
    }

    #[test]
    fn a_refusal_is_a_refusal_and_carries_a_code() {
        assert!(!RelayPathVerdict::Allow.is_refusal());
        assert!(RelayPathVerdict::Guarded.is_refusal());
        assert!(RelayPathVerdict::Malformed.is_refusal());
        assert_eq!(RelayPathVerdict::Guarded.code(), "relay_path_guarded");
        assert_eq!(RelayPathVerdict::Malformed.code(), "relay_path_malformed");
        assert!(!RelayPathVerdict::Guarded.message().is_empty());
        assert!(!RelayPathVerdict::Malformed.message().is_empty());
    }

    /// The tauri-invoke proxy is guarded because it safelists the terminal
    /// commands. If that safelist ever stops carrying them this guard can be
    /// revisited — until then the two must not drift apart silently.
    #[test]
    fn the_tauri_invoke_proxy_still_safelists_the_terminal_commands() {
        let safelist = crate::mcp::tauri_proxy::ALLOWED_PROXIED_COMMANDS;
        for cmd in ["terminal_create", "terminal_write", "terminal_close"] {
            assert!(
                safelist.contains(&cmd),
                "{cmd} left the safelist — re-read `GUARDED_PREFIXES`'s entry for \
                 `ui-bridge/tauri/invoke`"
            );
        }
        assert_eq!(
            relay_path_verdict("POST", "/ui-bridge/tauri/invoke"),
            RelayPathVerdict::Guarded
        );
    }
}
