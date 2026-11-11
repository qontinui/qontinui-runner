//! The typed answer to a refused terminal spawn — ONE classification shared by
//! every door that runs `acquire_for_terminal` and then spawns.
//!
//! A spawn can be refused as plain `Err` text from four producers, each of
//! which documents its prefix as stable:
//!
//! - the spawn-tenant refusal (`terminal:tenant_<kind>:`) — from the shared
//!   [`crate::commands::terminal::admit_spawn_tenant`] precheck, or from the
//!   identity seam in `TerminalSession::spawn`, which alone can judge a cwd
//!   that declares another tenant;
//! - the resource guard's CRITICAL verdict
//!   ([`crate::resource_guard::CRITICAL_REFUSAL_PREFIX`]) — from
//!   [`crate::resource_guard::precheck_spawn`] before the allocation, or the
//!   seam's `admit_spawn` after it;
//! - the trust gate's block on a pinned-account spawn
//!   ([`crate::claude_session::trust_gate::SPAWN_BLOCKED_REFUSAL_PREFIX`]),
//!   with its own code when the block rests on a tenant dial coord could not
//!   answer.
//!
//! `POST /terminals` typed these first (qontinui-runner#2734). The proxied
//! `terminal_create` and the backend-relay create answered the same texts
//! untyped, so one refusal had three answers depending on the door it came
//! through. They now all read [`spawn_refusal_code`], so they cannot drift
//! apart (plan
//! `2026-11-08-a-spawn-refused-after-its-worktree-allocation-leaves-the-tree-on-disk`,
//! Phase 0).

use axum::http::StatusCode;
use serde_json::Value;

/// A failed `TerminalManager::create`, typed by whether a child process may
/// already have started in the session's working directory (plan
/// `2026-11-08-a-spawn-refused-after-its-worktree-allocation-leaves-the-tree-on-disk`,
/// Why #5 and D1).
///
/// `child_started == false` only when the failure came BEFORE the child spawn:
/// the resource guard, the spawn-tenant identity seam, the trust gate, the
/// in-flight claim, or the PTY open. Everything after it — the pane's reader or
/// writer, and a poisoned sessions lock after a fully built session — is
/// `true`, because the child may outlive the reported failure. A door may
/// remove the tree it allocated only on `false`.
///
/// `message` is the text the door answers with, verbatim: every refusal prefix
/// [`spawn_refusal_code`] classifies survives unchanged. `Display` prints it
/// alone, so a caller that only stringifies the error is unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnFailure {
    pub(crate) child_started: bool,
    pub(crate) message: String,
}

impl SpawnFailure {
    /// A failure before any child process existed.
    pub(crate) fn before_child(message: impl Into<String>) -> Self {
        Self {
            child_started: false,
            message: message.into(),
        }
    }

    /// A failure after the child spawn — a child may hold the working dir.
    pub(crate) fn after_child(message: impl Into<String>) -> Self {
        Self {
            child_started: true,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<SpawnFailure> for String {
    fn from(failure: SpawnFailure) -> Self {
        failure.message
    }
}

/// The `<kind>` of a `terminal:tenant_<kind>:` spawn-tenant refusal text, or
/// `None` for any other text.
pub(crate) fn spawn_tenant_refusal_kind(text: &str) -> Option<&str> {
    text.strip_prefix("terminal:tenant_")
        .and_then(|rest| rest.split_once(':'))
        .map(|(kind, _)| kind)
        .filter(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// The typed answer to a spawn-tenant refusal: the prefix lifted into
/// `TERMINAL_TENANT_<KIND>` so a client can branch on it, and a status that
/// follows the kind. An unreadable credential store is UNKNOWN and not the
/// caller's to fix, so it is a 503 — as the provision-session door answers it —
/// and every other kind (malformed, unpaired, a cwd declaring another tenant)
/// is a 400. `None` for a text outside the family.
pub(crate) fn spawn_tenant_refusal_code(text: &str) -> Option<(StatusCode, String)> {
    let kind = spawn_tenant_refusal_kind(text)?;
    let status = if kind == "credential_store_unreadable" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_REQUEST
    };
    Some((
        status,
        format!("TERMINAL_TENANT_{}", kind.to_ascii_uppercase()),
    ))
}

/// The resource guard's and the trust gate's refusals.
///
/// - The guard's CRITICAL verdict clears when the box recovers, so it is a 503.
/// - The trust gate's block is a standing verdict about the workspace, so it is
///   a 409. One that rests on a tenant dial coord could not answer is worth
///   retrying at once, so it is a 503 with its own code.
fn guard_or_trust_refusal_code(error: &str) -> Option<(StatusCode, &'static str)> {
    use crate::claude_session::trust_gate;
    if error.starts_with(crate::resource_guard::CRITICAL_REFUSAL_PREFIX) {
        Some((StatusCode::SERVICE_UNAVAILABLE, "RESOURCE_GUARD_CRITICAL"))
    } else if trust_gate::refusal_rests_on_unread_dial(error) {
        Some((StatusCode::SERVICE_UNAVAILABLE, "SPAWN_BLOCKED_DIAL_UNREAD"))
    } else if trust_gate::is_spawn_blocked_refusal(error) {
        Some((StatusCode::CONFLICT, "SPAWN_BLOCKED"))
    } else {
        None
    }
}

/// The typed status and code for a refused terminal spawn, or `None` when the
/// text is not one of the recognised refusal families — which every door keeps
/// answering untyped, as the runner's own fault. Matching is by PREFIX, so a
/// text that merely mentions a family mid-string is not a refusal.
pub(crate) fn spawn_refusal_code(error: &str) -> Option<(StatusCode, String)> {
    spawn_tenant_refusal_code(error).or_else(|| {
        guard_or_trust_refusal_code(error).map(|(status, code)| (status, code.to_string()))
    })
}

/// The backend relay's error frame for a refused `terminal_create`.
///
/// The message is unchanged from the untyped frame — `Failed to create
/// terminal: <error>` — and a recognised refusal adds its `code`, as the relay's
/// other create refusals already carry one (`create_refusal_frame`, the drain
/// gate's `class.code()`). `echo_remote` adds the `remote` echo the relay routes
/// a remote create's reply by.
pub(crate) fn relay_spawn_failed_frame(error: &str, data: &Value, echo_remote: bool) -> Value {
    let mut frame = serde_json::json!({
        "type": "error",
        "message": format!("Failed to create terminal: {error}"),
        "request_id": data.get("request_id"),
    });
    if let Some((_, code)) = spawn_refusal_code(error) {
        frame["code"] = Value::String(code);
    }
    if echo_remote {
        let remote = crate::mcp::remote_terminal::remote_echo(data);
        // Top-level `grant_jti` from the remote block, as the relay's other
        // remote refusal and lifecycle frames carry it (`terminal_ended_frame`).
        frame["grant_jti"] = remote["grant_jti"].clone();
        frame["remote"] = remote;
    }
    frame
}

/// The relay's resource-guard precheck, answered with the SAME frame its
/// failed-create arm gives for the same text — so a client cannot tell which
/// of the two gates answered ([`crate::resource_guard::precheck_spawn`]'s
/// contract). It runs before the grant gate, where the admitted create does not
/// exist yet, so the `remote` echo is keyed on the frame carrying a `remote`
/// block rather than on an admitted grant.
pub(crate) fn relay_precheck(
    check: impl FnOnce() -> Result<(), String>,
    data: &Value,
) -> Result<(), Value> {
    check().map_err(|refusal| {
        let carries_remote = crate::mcp::remote_terminal::parse_remote_block(data).is_some();
        relay_spawn_failed_frame(&refusal, data, carries_remote)
    })
}

/// Test-only: one real refusal text per family, each built by its producer, with
/// the status and code every door must answer it with.
#[cfg(test)]
pub(crate) fn refusal_family_fixtures() -> Vec<(String, StatusCode, &'static str)> {
    use crate::claude_session::trust_gate::{
        annotate_unread_dial, DialResolution, TrustGateDecision,
    };
    use crate::coord_mcp::{DeclaredWorkdirKey, SpawnTenantRefusal};
    let tenant = uuid::Uuid::from_u128(0xE7E7_0000_0000_4000_8000_0000_0000_00E7);
    let blocked = || TrustGateDecision::SpawnBlocked {
        rule: "ask-first",
        failed: vec!["coord_worktree_row=failed".to_string()],
    };
    vec![
        (
            SpawnTenantRefusal::WorkdirDeclaresOtherTenant {
                tenant,
                declared_file: std::path::PathBuf::from("/w/.mcp.json"),
                declared: DeclaredWorkdirKey::NotLive,
            }
            .to_string(),
            StatusCode::BAD_REQUEST,
            "TERMINAL_TENANT_WORKDIR_DECLARES_OTHER_TENANT",
        ),
        (
            SpawnTenantRefusal::NotPaired { tenant }.to_string(),
            StatusCode::BAD_REQUEST,
            "TERMINAL_TENANT_NOT_PAIRED",
        ),
        (
            SpawnTenantRefusal::CredentialStoreUnreadable {
                tenant,
                error: "io error".to_string(),
            }
            .to_string(),
            StatusCode::SERVICE_UNAVAILABLE,
            "TERMINAL_TENANT_CREDENTIAL_STORE_UNREADABLE",
        ),
        (
            crate::resource_guard::critical_refusal_fixture("terminal session"),
            StatusCode::SERVICE_UNAVAILABLE,
            "RESOURCE_GUARD_CRITICAL",
        ),
        (
            blocked().refusal().expect("a block renders a refusal"),
            StatusCode::CONFLICT,
            "SPAWN_BLOCKED",
        ),
        (
            annotate_unread_dial(
                blocked(),
                &DialResolution::Unresolved {
                    error: "coord timed out".to_string(),
                },
            )
            .refusal()
            .expect("a block renders a refusal"),
            StatusCode::SERVICE_UNAVAILABLE,
            "SPAWN_BLOCKED_DIAL_UNREAD",
        ),
    ]
}

/// Test-only: texts that are NOT refusals — a plain runner fault, and each
/// family's prefix appearing mid-string.
#[cfg(test)]
pub(crate) const UNRECOGNISED_SPAWN_FAILURES: &[&str] = &[
    "Failed to open PTY: out of ptys",
    "wrapped: terminal:tenant_not_paired: elsewhere",
    "wrapped: resource_guard:critical:thread_count: elsewhere",
    "wrapped: spawn_blocked (ask-first): elsewhere",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn every_refusal_family_maps_to_its_typed_status_and_code() {
        for (text, status, code) in refusal_family_fixtures() {
            assert_eq!(
                spawn_refusal_code(&text),
                Some((status, code.to_string())),
                "{text}"
            );
        }
        for text in UNRECOGNISED_SPAWN_FAILURES {
            assert_eq!(spawn_refusal_code(text), None, "{text}");
        }
    }

    /// The relay keeps its message and adds the code; an unrecognised failure
    /// keeps today's frame exactly, with no `code` key at all.
    #[test]
    fn the_relay_frame_adds_the_code_and_keeps_the_message() {
        let data = serde_json::json!({ "request_id": "r-1" });
        for (text, _, code) in refusal_family_fixtures() {
            let frame = relay_spawn_failed_frame(&text, &data, false);
            assert_eq!(frame["type"], "error");
            assert_eq!(frame["code"], code, "{text}");
            assert_eq!(
                frame["message"],
                format!("Failed to create terminal: {text}")
            );
            assert_eq!(frame["request_id"], "r-1");
            assert!(frame.get("remote").is_none());
        }
        for text in UNRECOGNISED_SPAWN_FAILURES {
            let frame = relay_spawn_failed_frame(text, &data, false);
            assert_eq!(
                frame,
                serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to create terminal: {text}"),
                    "request_id": "r-1",
                })
            );
        }
    }

    /// The precheck's answer equals the failed-create arm's for the same text:
    /// on a remote create (the `Err` arm echoes `remote` because a grant was
    /// admitted) and on the operator-web / mobile path (no block, no echo).
    #[test]
    fn the_relay_precheck_answers_as_the_failed_create_arm_does() {
        let remote = serde_json::json!({
            "request_id": "r-2",
            "remote": { "grant_jti": "jti-9", "source_device_id": "dev-1" },
        });
        let local = serde_json::json!({ "request_id": "r-3" });
        for (text, _, code) in refusal_family_fixtures() {
            let refused = |data: &Value| {
                let t = text.clone();
                relay_precheck(move || Err(t), data).expect_err("a refusal is a frame")
            };
            let frame = refused(&remote);
            assert_eq!(frame, relay_spawn_failed_frame(&text, &remote, true));
            assert_eq!(frame["code"], code);
            assert_eq!(frame["remote"]["grant_jti"], "jti-9");
            assert_eq!(
                frame["grant_jti"], "jti-9",
                "top-level grant_jti, as sibling frames"
            );
            assert_eq!(
                refused(&local),
                relay_spawn_failed_frame(&text, &local, false)
            );
        }
        assert!(relay_precheck(|| Ok(()), &remote).is_ok());
    }

    // ---- Structural: every `acquire_for_terminal` door -------------------

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Counts the CODE braces of Rust source fed to it line by line — skipping
    /// `//` and nested `/* */` comments, string literals (escapes included),
    /// raw strings (`r"…"`, `r#"…"#`, spanning lines), and char literals, while
    /// leaving lifetimes (`'a`) alone. Not a Rust lexer: enough of one that a
    /// brace inside a format string or a doc example cannot move the depth,
    /// which a plain `{`/`}` count did — it ran `terminal/mod.rs`'s test module
    /// to end of file, so a door appended after it read as test code.
    #[derive(Default)]
    struct BraceLexer {
        block_comment: u32,
        in_str: bool,
        raw_hashes: Option<usize>,
        /// When set, [`Self::feed`] also collects the line's CODE bytes —
        /// string, char and comment contents dropped ([`Self::code_of`]).
        out: Option<Vec<u8>>,
    }

    impl BraceLexer {
        /// Is the lexer between tokens — not inside a comment or string — at
        /// the start of the next line?
        fn in_code(&self) -> bool {
            self.block_comment == 0 && !self.in_str && self.raw_hashes.is_none()
        }

        /// `line` with string, char-literal and comment contents removed, so a
        /// `?` in a format string or a `return` in a comment is not code.
        fn code_of(&mut self, line: &str) -> String {
            self.out = Some(Vec::new());
            self.feed(line);
            String::from_utf8_lossy(&self.out.take().unwrap_or_default()).into_owned()
        }

        fn emit(&mut self, bytes: &[u8]) {
            if let Some(out) = self.out.as_mut() {
                out.extend_from_slice(bytes);
            }
        }

        /// `(opening, closing)` code braces on `line`.
        fn feed(&mut self, line: &str) -> (i64, i64) {
            let b = line.as_bytes();
            let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
            let (mut open, mut close) = (0i64, 0i64);
            let mut i = 0;
            while i < b.len() {
                if self.block_comment > 0 {
                    if b[i..].starts_with(b"*/") {
                        self.block_comment -= 1;
                        i += 2;
                    } else if b[i..].starts_with(b"/*") {
                        self.block_comment += 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if let Some(h) = self.raw_hashes {
                    if b[i] == b'"'
                        && b.len() > i + h
                        && b[i + 1..=i + h].iter().all(|&c| c == b'#')
                    {
                        self.raw_hashes = None;
                        i += 1 + h;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                if self.in_str {
                    match b[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            self.in_str = false;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                    continue;
                }
                match b[i] {
                    b'/' if b[i..].starts_with(b"//") => break,
                    b'/' if b[i..].starts_with(b"/*") => {
                        self.block_comment += 1;
                        i += 2;
                    }
                    b'r' if (i == 0
                        || !ident(b[i - 1])
                        || (b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2])))) =>
                    {
                        let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                        if b.get(i + 1 + hashes) == Some(&b'"') {
                            self.raw_hashes = Some(hashes);
                            self.emit(b"\"\"");
                            i += 2 + hashes;
                        } else {
                            self.emit(b"r");
                            i += 1;
                        }
                    }
                    b'"' => {
                        self.in_str = true;
                        self.emit(b"\"\"");
                        i += 1;
                    }
                    b'\'' => {
                        // A char literal ('x', '\n', '\u{..}', a multi-byte
                        // char) closes within a few bytes; a lifetime does not.
                        let close_at = if b.get(i + 1) == Some(&b'\\') {
                            b[i + 2..]
                                .iter()
                                .position(|&c| c == b'\'')
                                .map(|p| i + 2 + p)
                        } else {
                            let width = match b.get(i + 1) {
                                Some(&c) if c >= 0xF0 => 4,
                                Some(&c) if c >= 0xE0 => 3,
                                Some(&c) if c >= 0xC0 => 2,
                                _ => 1,
                            };
                            (b.get(i + 1 + width) == Some(&b'\'')).then_some(i + 1 + width)
                        };
                        // A char literal is emitted as `''`; a lifetime keeps
                        // its tick (its name follows as ordinary code).
                        self.emit(if close_at.is_some() { b"''" } else { b"'" });
                        i = close_at.map_or(i + 1, |c| c + 1);
                    }
                    b'{' => {
                        open += 1;
                        self.emit(b"{");
                        i += 1;
                    }
                    b'}' => {
                        close += 1;
                        self.emit(b"}");
                        i += 1;
                    }
                    c => {
                        self.emit(&[c]);
                        i += 1;
                    }
                }
            }
            (open, close)
        }
    }

    /// A source file split for the scan: every line kept at its own index (so
    /// a site's line number is true), with `//` lines blanked — a comment
    /// naming a guard cannot satisfy the check — and the BODY of every
    /// `#[cfg(test)] mod x { … }` block blanked into `test_lines`. Only a block
    /// is skipped: a `#[cfg(test)] mod x;` DECLARATION hides nothing, and the
    /// production code below it is scanned (it used to truncate the file at
    /// the first `#[cfg(test)]\nmod `, which also matched a declaration and
    /// hid the rest of nine files, `main.rs` among them).
    ///
    /// A skipped block must close on a line that is exactly `}` at the `mod`
    /// line's own indentation; anything else is a lost brace count, recorded in
    /// `errors` so the scan fails loudly instead of hiding production code.
    struct ScannedSource {
        production: Vec<String>,
        test_lines: Vec<String>,
        errors: Vec<String>,
    }

    fn scan_source(src: &str) -> ScannedSource {
        let lines: Vec<&str> = src.lines().collect();
        let mut production = Vec::with_capacity(lines.len());
        let mut test_lines = Vec::new();
        let mut errors = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let opens_test_block = lines[i].trim() == "#[cfg(test)]"
                && lines[i + 1..]
                    .iter()
                    .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("#["))
                    .is_some_and(|l| {
                        let t = l.trim();
                        (t.starts_with("mod ")
                            || t.starts_with("pub mod ")
                            || t.starts_with("pub(crate) mod "))
                            && t.ends_with('{')
                    });
            if opens_test_block {
                // Blank from the attribute through the block's closing brace.
                let attr_line = i + 1;
                let indent = lines[i].len() - lines[i].trim_start().len();
                let mut lexer = BraceLexer::default();
                let mut depth = 0i64;
                let mut opened = false;
                let mut closed_on = None;
                while i < lines.len() {
                    // Independent of the brace count it guards: inside the
                    // block, code sits deeper than the `mod` line. An ITEM at
                    // the block's own indentation (or shallower) means the count
                    // ran past the module and is hiding production code — even
                    // when it later closes on a bare `}`.
                    if opened && depth > 0 && lexer.in_code() && item_at_or_above(lines[i], indent)
                    {
                        errors.push(format!(
                            "line {}: `{}` sits at the indentation of the #[cfg(test)] block \
                             opened at line {attr_line} but inside its brace count — the count \
                             is lost and production code would be hidden",
                            i + 1,
                            lines[i].trim()
                        ));
                    }
                    test_lines.push(lines[i].to_string());
                    production.push(String::new());
                    let (open, close) = lexer.feed(lines[i]);
                    depth += open - close;
                    opened |= open > 0;
                    i += 1;
                    if opened && depth <= 0 {
                        closed_on = Some(lines[i - 1]);
                        break;
                    }
                }
                let expected = format!("{}}}", " ".repeat(indent));
                if closed_on != Some(expected.as_str()) {
                    errors.push(format!(
                        "the #[cfg(test)] block at line {attr_line} closed on {closed_on:?}, \
                         not a bare `}}` at its own indentation — the brace count is lost"
                    ));
                }
                continue;
            }
            let l = lines[i];
            production.push(if l.trim_start().starts_with("//") {
                String::new()
            } else {
                l.to_string()
            });
            i += 1;
        }
        ScannedSource {
            production,
            test_lines,
            errors,
        }
    }

    /// The marker comment a door places on its real spawn call.
    const SPAWN_MARKER: &str = "door-scan: spawn";

    /// Each `acquire_for_terminal` door's REAL spawn call — the token the
    /// first code line after its [`SPAWN_MARKER`] must carry, so the marker
    /// cannot be moved up to empty the early-exit window.
    const DOOR_SPAWN_TOKENS: &[(&str, &str)] = &[
        ("commands/terminal.rs", "create_manager.create("),
        ("mcp/backend_relay.rs", "tm.create("),
        ("mcp/tauri_proxy.rs", "tm.create("),
        ("mcp/terminals.rs", "terminal_manager.create("),
    ];

    /// The line of the [`SPAWN_MARKER`] after the allocation at `alloc`
    /// (within `..=end`), provided the first CODE line after it carries
    /// `token`, the door's real spawn.
    fn spawn_marker(
        raw: &[String],
        code: &[String],
        alloc: usize,
        end: usize,
        token: &str,
    ) -> Result<usize, String> {
        let at = (alloc + 1..=end.min(raw.len().saturating_sub(1)))
            .find(|&i| raw[i].contains(SPAWN_MARKER))
            .ok_or_else(|| format!("no `{SPAWN_MARKER}` marker after the allocation"))?;
        let next = (at + 1..=end.min(code.len().saturating_sub(1)))
            .find(|&i| !code[i].trim().is_empty())
            .ok_or_else(|| format!("nothing follows the `{SPAWN_MARKER}` marker"))?;
        if code[next].contains(token) {
            Ok(at)
        } else {
            Err(format!(
                "the `{SPAWN_MARKER}` marker at line {} is not on the real spawn (`{token}`); \
                 the code after it is `{}`",
                at + 1,
                code[next].trim()
            ))
        }
    }

    /// A marker moved up, off the real spawn, fails — it would otherwise
    /// shrink the early-exit window to nothing.
    #[test]
    fn a_spawn_marker_off_the_real_spawn_is_refused() {
        let placed: Vec<String> = [
            "    let (wd, ctx) = acquire_for_terminal(a).await;",
            "    let x = resolve()?;",
            "    // door-scan: spawn",
            "    match tm.create(",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let code = code_lines(&placed);
        assert_eq!(spawn_marker(&placed, &code, 0, 3, "tm.create("), Ok(2));
        let moved: Vec<String> = [
            "    let (wd, ctx) = acquire_for_terminal(a).await;",
            "    // door-scan: spawn",
            "    let x = resolve()?;",
            "    match tm.create(",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let code = code_lines(&moved);
        assert!(spawn_marker(&moved, &code, 0, 3, "tm.create(").is_err());
    }

    /// The reasons a `door-scan: keep(<reason>)` marker may give for an exit
    /// that legitimately leaves the allocation in place.
    const KEEP_REASONS: &[&str] = &[
        "post-child",        // a child ran in the tree (or may have)
        "handed-back",       // the failure carries the context to a settle seam
        "nothing-allocated", // the allocation did not happen on this path
        "spawned",           // the session started and holds the context
        "loop-exit",         // a `break`; the code after the loop settles the allocation
        "retry",             // a `continue` that carries the context into the next attempt
    ];

    /// `Ok(true)` when `line` is a well-formed keep marker, `Ok(false)` when it
    /// is no marker at all, `Err` for a marker with an unknown reason.
    fn keep_marker(line: &str) -> Result<bool, String> {
        let Some(rest) = line.trim().strip_prefix("// door-scan: keep(") else {
            return Ok(false);
        };
        let reason = rest.split(')').next().unwrap_or_default();
        if KEEP_REASONS.contains(&reason) {
            Ok(true)
        } else {
            Err(format!(
                "unknown keep reason `{reason}` (allowed: {KEEP_REASONS:?})"
            ))
        }
    }

    /// Per line: the brace depth at its start, and the lowest depth reached
    /// while it is read — over CODE lines, so braces in strings never count.
    fn depth_profile(code: &[String]) -> Vec<(i64, i64)> {
        let mut depth = 0i64;
        code.iter()
            .map(|l| {
                let start = depth;
                let mut low = depth;
                for c in l.chars() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            low = low.min(depth);
                        }
                        _ => {}
                    }
                }
                (start, low)
            })
            .collect()
    }

    /// Is the block holding the removal at `(r, cr)` (line, byte column of the
    /// token) still open at the exit at `(e, ce)` — the same block or an
    /// enclosing one, never a closed sibling? Keyed on the depth AT the token,
    /// so `if c { remove_unspawned(ctx); }` on one line does not cover a later
    /// exit, and the walk stops at the exit's own column, so a `}` after it on
    /// its line does not count against it.
    fn in_scope(
        code: &[String],
        profile: &[(i64, i64)],
        r: (usize, usize),
        e: (usize, usize),
    ) -> bool {
        let braces = |l: &str, from: usize, to: usize, depth: &mut i64, low: &mut i64| {
            for (i, c) in l.char_indices() {
                if i < from || i >= to {
                    continue;
                }
                match c {
                    '{' => *depth += 1,
                    '}' => {
                        *depth -= 1;
                        *low = (*low).min(*depth);
                    }
                    _ => {}
                }
            }
        };
        let (mut depth, mut scratch) = (profile[r.0].0, 0i64);
        braces(&code[r.0], 0, r.1, &mut depth, &mut scratch);
        let held = depth;
        let mut low = held;
        for line in r.0..=e.0 {
            let from = if line == r.0 { r.1 } else { 0 };
            let to = if line == e.0 { e.1 } else { usize::MAX };
            if line != r.0 {
                depth = profile[line].0;
            }
            braces(&code[line], from, to, &mut depth, &mut low);
        }
        low >= held
    }

    /// The CODE of each line — string, char-literal and comment contents
    /// dropped — lexed in order, so state carries across lines.
    fn code_lines(lines: &[String]) -> Vec<String> {
        let mut lexer = BraceLexer::default();
        lines.iter().map(|l| lexer.code_of(l)).collect()
    }

    /// The lines strictly between `from` and `to` whose code can leave the
    /// enclosing fn early — or leave the block that holds the allocation: any
    /// `return`, `continue` or `break` keyword (with or without a value), a
    /// `bail!` / `ensure!` macro, or any `?`.
    ///
    /// `break` is counted deliberately even though it stays in the fn: whether
    /// the code after the loop handles the allocation is exactly what a reader
    /// must not assume, so each one carries its own removal or a
    /// `keep(loop-exit)` marker naming where the allocation is settled.
    fn early_exits(code: &[String], from: usize, to: usize) -> Vec<usize> {
        (from + 1..to.min(code.len()))
            .filter(|&i| exit_col(&code[i]).is_some())
            .collect()
    }

    /// The byte column of the first exit on a CODE line, if any: a `?`, a
    /// `return` / `continue` / `break` keyword, a `bail!` / `ensure!`, or a
    /// TAIL-POSITION `Err(` — a match arm or block whose value is the
    /// refusal (`=> Err(..)`, or a line that is just `Err(..)` — never the
    /// `Err(e) =>` pattern that opens an arm).
    fn exit_col(l: &str) -> Option<usize> {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let word = |w: &str| {
            l.match_indices(w)
                .find(|(i, _)| {
                    !l.get(..*i)
                        .and_then(|h| h.chars().next_back())
                        .is_some_and(ident)
                        && !l
                            .get(i + w.len()..)
                            .and_then(|t| t.chars().next())
                            .is_some_and(ident)
                })
                .map(|(i, _)| i)
        };
        let trimmed = l.trim_start();
        let tail_err = if trimmed.starts_with("Err(") && !l.contains("=>") {
            Some(l.len() - trimmed.len())
        } else {
            l.find("=> Err(").map(|i| i + 3)
        };
        [
            l.find('?'),
            word("return"),
            word("continue"),
            word("break"),
            l.find("bail!"),
            l.find("ensure!"),
            tail_err,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A tail-position `Err(..)` — an arm's or block's value — is an exit; the
    /// `Err(e) =>` pattern that opens an arm is not.
    #[test]
    fn a_tail_position_err_is_an_exit_and_an_err_pattern_is_not() {
        let lines: Vec<String> = [
            "    let ctx = acquire();",
            "        Err(e) => {",
            "            Err(e)",
            "        Err(j) => Err(format!(\"{j}\")),",
            "        Err(_) => warn!(\"x\"),",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(early_exits(&code_lines(&lines), 0, 5), vec![2, 3]);
    }

    /// The window check catches what it must and nothing in strings: a
    /// `return Ok(..)` and a `?` are exits; a `?` inside a format string is not.
    #[test]
    fn an_injected_return_ok_in_the_spawn_window_is_an_early_exit() {
        let lines: Vec<String> = [
            "    let (wd, ctx) = acquire_for_terminal(a, b).await;",
            "    return Ok(Json(x));",
            "    let y = format!(\"{a:?} return\");",
            "    let z = f()?;",
            "    // a comment saying return? is not code",
            "    // door-scan: spawn",
            "    match tm.create(",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(early_exits(&code_lines(&lines), 0, 5), vec![1, 3]);
    }

    /// `break` (bare or with a value), `bail!` and `ensure!` are exits too; a
    /// `breakpoint` identifier is not.
    #[test]
    fn break_bail_and_ensure_are_early_exits() {
        let lines: Vec<String> = [
            "    let ctx = acquire();",
            "        break;",
            "        break 'outer Some(x);",
            "    anyhow::bail!(\"refused\");",
            "    anyhow::ensure!(ok, \"refused\");",
            "    let breakpoint = 1;",
            "    let rebreak_x = 2;",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(early_exits(&code_lines(&lines), 0, 7), vec![1, 2, 3, 4]);
    }

    fn exits_of(src: &str) -> Vec<String> {
        let raw: Vec<String> = src.lines().map(str::to_string).collect();
        unremoved_exits("x.rs", &raw, "door", Some("acquire("))
    }

    /// An exit after the anchor with no removal since the previous exit is
    /// reported; one removal earlier in the fn does not cover a later exit.
    #[test]
    fn every_door_exit_needs_its_own_removal() {
        let found = exits_of(
            "fn door() {\n    let ctx = acquire();\n    if a {\n        remove_unspawned(ctx);\n        \
             return Err(1);\n    }\n    if b {\n        return Err(2);\n    }\n    if c {\n        \
             // door-scan: keep(post-child) a child ran\n        return Err(3);\n    }\n}\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("Err(2)"), "{found:?}");
    }

    /// A tail-position refusal arm is held to its own removal: with the
    /// removal the door is clean; delete it and the scan goes red.
    #[test]
    fn a_tail_refusal_arm_without_its_removal_goes_red() {
        let with = "fn door() {\n    let ctx = acquire();\n    match spawn() {\n        Ok(c) => \
                    {\n            // door-scan: keep(spawned) running\n            return Ok(c);\n        \
                    }\n        Err(e) => {\n            remove_unspawned(ctx);\n            Err(e)\n        \
                    }\n    }\n}\n";
        assert!(exits_of(with).is_empty(), "{:?}", exits_of(with));
        let without = with.replace("            remove_unspawned(ctx);\n", "");
        let found = exits_of(&without);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("Err(e)"), "{found:?}");
    }

    /// A door fn with no exit the scan can see is refused unless it says why.
    #[test]
    fn a_door_with_no_visible_exit_needs_a_no_exits_marker() {
        let bare = "fn door() {\n    let ctx = acquire();\n    run(ctx);\n}\n";
        assert!(
            exits_of(bare).iter().any(|e| e.contains("NO exit")),
            "{:?}",
            exits_of(bare)
        );
        let declared = "fn door() {\n    // door-scan: no-exits(infallible) run cannot fail\n    \
                        let ctx = acquire();\n    run(ctx);\n}\n";
        assert!(exits_of(declared).is_empty(), "{:?}", exits_of(declared));
    }

    /// The removal's depth is taken AT its token: a one-line `if c {
    /// remove_unspawned(ctx); }` closes before a later exit and covers nothing.
    #[test]
    fn a_one_line_block_removal_does_not_cover_a_later_exit() {
        let found = exits_of(
            "fn door() {\n    let ctx = acquire();\n    if c { remove_unspawned(ctx); }\n    \
             return Err(2);\n}\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        let same_line = exits_of(
            "fn door() {\n    let ctx = acquire();\n    if c { remove_unspawned(ctx); return Err(2); }\n}\n",
        );
        assert!(same_line.is_empty(), "{same_line:?}");
    }

    /// A removal in a closed SIBLING block does not cover an exit; one in an
    /// enclosing block does.
    #[test]
    fn a_removal_in_a_sibling_block_does_not_cover_an_exit() {
        let sibling = exits_of(
            "fn door() {\n    let ctx = acquire();\n    if a {\n        remove_unspawned(ctx);\n    \
             }\n    if b {\n        return Err(2);\n    }\n}\n",
        );
        assert_eq!(sibling.len(), 1, "{sibling:?}");
        let enclosing = exits_of(
            "fn door() {\n    let ctx = acquire();\n    remove_unspawned(ctx);\n    if b {\n        \
             return Err(2);\n    }\n}\n",
        );
        assert!(enclosing.is_empty(), "{enclosing:?}");
    }

    /// A keep marker counts only directly above its exit and only with a known
    /// reason.
    #[test]
    fn a_keep_marker_must_be_directly_above_and_name_a_known_reason() {
        let distant = exits_of(
            "fn door() {\n    let ctx = acquire();\n    // door-scan: keep(post-child) here\n    \
             let y = 1;\n    return Err(2);\n}\n",
        );
        assert_eq!(distant.len(), 1, "{distant:?}");
        let unknown = exits_of(
            "fn door() {\n    let ctx = acquire();\n    // door-scan: keep(because) trust me\n    \
             return Err(2);\n}\n",
        );
        assert!(
            unknown.iter().any(|e| e.contains("unknown keep reason")),
            "{unknown:?}"
        );
        let good = exits_of(
            "fn door() {\n    let ctx = acquire();\n    // door-scan: keep(handed-back) seam\n    \
             return Err(2);\n}\n",
        );
        assert!(good.is_empty(), "{good:?}");
    }

    /// The production lines when each skipped test block is ended by
    /// INDENTATION alone — the first bare `}` at the `mod` line's own
    /// indentation — with no brace lexing at all.
    fn indent_production(src: &str) -> Vec<String> {
        let lines: Vec<&str> = src.lines().collect();
        let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let mut i = 0;
        while i < lines.len() {
            let is_attr = lines[i].trim() == "#[cfg(test)]";
            let mod_at = is_attr
                .then(|| {
                    (i + 1..lines.len()).find(|&j| {
                        !lines[j].trim().is_empty() && !lines[j].trim_start().starts_with("#[")
                    })
                })
                .flatten()
                .filter(|&j| {
                    let t = lines[j].trim();
                    (t.starts_with("mod ")
                        || t.starts_with("pub mod ")
                        || t.starts_with("pub(crate) mod "))
                        && t.ends_with('{')
                });
            if let Some(m) = mod_at {
                let indent = lines[i].len() - lines[i].trim_start().len();
                let closer = format!("{}}}", " ".repeat(indent));
                let end = (m + 1..lines.len())
                    .find(|&j| lines[j] == closer)
                    .unwrap_or(lines.len() - 1);
                for line in out.iter_mut().take(end + 1).skip(i) {
                    line.clear();
                }
                i = end + 1;
                continue;
            }
            i += 1;
        }
        out
    }

    /// A self-check on the scan that does not hold by construction: the
    /// number of production call sites under the LEXER's test-block spans
    /// must equal the number under INDENTATION-derived spans. A lexer that
    /// lost its count and swallowed code (through to end of file, say) hides
    /// call sites the indentation still shows as production.
    fn partition_disagreement(
        src: &str,
        lexed_production: &[String],
        is_call: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let lexed = lexed_production.iter().filter(|l| is_call(l)).count();
        let indented = indent_production(src).iter().filter(|l| is_call(l)).count();
        (lexed != indented).then(|| {
            format!(
                "the lexer counts {lexed} production call site(s) but the indentation counts \
                 {indented} — a skipped test block's span is wrong"
            )
        })
    }

    /// The partition self-check can fire: here a lone `}` at column 0 inside
    /// a raw string ends the block early by indentation while the lexer
    /// (correctly) keeps going, so the two counts of `target(` disagree.
    #[test]
    fn the_partition_self_check_fires_when_the_two_spans_disagree() {
        let src = "#[cfg(test)]\nmod tests {\n    const S: &str = r#\"\n}\n\"#;\n    \
                   fn t() { target(); }\n}\nfn prod() { target(); }\n";
        let lexed = scan_source(src).production;
        let msg = partition_disagreement(src, &lexed, |l| l.contains("target("));
        assert!(msg.is_some(), "the two spans disagree on this source");
        let agree =
            "#[cfg(test)]\nmod tests {\n    fn t() { target(); }\n}\nfn prod() { target(); }\n";
        let lexed = scan_source(agree).production;
        assert!(partition_disagreement(agree, &lexed, |l| l.contains("target(")).is_none());
    }

    /// Does `line` start a Rust item at indentation `indent` or shallower?
    fn item_at_or_above(line: &str, indent: usize) -> bool {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || line.len() - trimmed.len() > indent {
            return false;
        }
        let mut rest = trimmed;
        for q in ["pub(crate) ", "pub(super) ", "pub ", "async ", "unsafe "] {
            rest = rest.strip_prefix(q).unwrap_or(rest);
        }
        [
            "fn ", "impl", "mod ", "struct ", "enum ", "trait ", "use ", "const ", "static ",
            "type ", "#[",
        ]
        .iter()
        .any(|k| rest.starts_with(k))
    }

    /// The independent check on a skipped test block: an item at the block's
    /// own indentation inside it is reported, though the block still closes on
    /// a bare `}` (so the close check alone would pass).
    #[test]
    fn a_test_block_that_swallows_a_production_item_is_a_scan_error() {
        let src = "#[cfg(test)]\nmod tests {\n    fn a() {\n    }\nfn leaked() {\n}\n}\n";
        let scanned = scan_source(src);
        assert!(
            scanned.errors.iter().any(|e| e.contains("fn leaked")),
            "{:?}",
            scanned.errors
        );
        let ok = "#[cfg(test)]\nmod tests {\n    fn a() {\n    }\n}\nfn prod() {}\n";
        let scanned = scan_source(ok);
        assert!(scanned.errors.is_empty(), "{:?}", scanned.errors);
        assert_eq!(scanned.production[5], "fn prod() {}");
    }

    /// Does `line` open a function item, at ANY indentation (a free fn, or a
    /// method inside an `impl`)? `fn`, optionally after `pub`/`pub(..)`,
    /// `const`, `async`, `unsafe` and `extern "…"`.
    fn opens_fn(line: &str) -> bool {
        let mut rest = line.trim_start();
        loop {
            let before = rest;
            for q in [
                "pub(crate) ",
                "pub(super) ",
                "pub ",
                "const ",
                "async ",
                "unsafe ",
            ] {
                if let Some(r) = rest.strip_prefix(q) {
                    rest = r.trim_start();
                }
            }
            if let Some(r) = rest.strip_prefix("extern ") {
                rest = r.trim_start();
                if let Some(abi) = rest.strip_prefix('"') {
                    rest = abi.split_once('"').map_or("", |(_, r)| r).trim_start();
                }
            }
            if rest == before {
                break;
            }
        }
        rest.strip_prefix("fn ")
            .is_some_and(|r| r.starts_with(|c: char| c.is_alphabetic() || c == '_'))
    }

    /// The body span `[start, end]` of the function enclosing line `idx`: the
    /// nearest `fn` opener above it, at any indentation, whose braces —
    /// matched from its opening line — have not closed before `idx`. `None`
    /// when no enclosing fn exists.
    fn enclosing_fn(lines: &[String], idx: usize) -> Option<(usize, usize)> {
        for start in (0..=idx).rev() {
            if !opens_fn(&lines[start]) {
                continue;
            }
            let mut lexer = BraceLexer::default();
            let mut depth = 0i64;
            let mut opened = false;
            for (end, line) in lines.iter().enumerate().skip(start) {
                let (open, close) = lexer.feed(line);
                depth += open - close;
                opened |= open > 0;
                if opened && depth <= 0 {
                    if end >= idx {
                        return Some((start, end));
                    }
                    break;
                }
                if !opened && line.trim_end().ends_with(';') {
                    break; // a bodyless declaration (trait method)
                }
            }
        }
        None
    }

    /// The production lines of `src` (see [`scan_source`]).
    fn production_code_lines(src: &str) -> Vec<String> {
        scan_source(src).production
    }

    /// Plan `2026-11-08-…-leaves-the-tree-on-disk` Phases 0 and 2: every call site of
    /// `acquire_for_terminal` under `src-tauri/src` runs the resource-guard
    /// precheck BEFORE it (a CRITICAL refusal after the allocation leaks a
    /// `git worktree add`) and removes-and-reports an unspawned allocation
    /// (`remove_unspawned`) AFTER it, on the
    /// failure arm (coord otherwise keeps the failed allocation `allocated`).
    /// Enumerated from the source, so a new door is covered the moment it
    /// appears [policy: testing/coverage-is-enumerated-not-salient].
    ///
    /// What it checks is PRESENCE IN THE ENCLOSING FN, not arm precision: the
    /// precheck must appear between the enclosing fn's opening line and the
    /// allocation, and `remove_unspawned(` between the allocation and that fn's
    /// closing brace. The enclosing fn is found at ANY indentation (a door in an
    /// `impl` method is bound by its own braces, never by an unrelated top-level
    /// fn above it), and only `#[cfg(test)] mod x { … }` BLOCKS are skipped — a
    /// `#[cfg(test)] mod x;` declaration hides no production code; a skipped
    /// block that swallows an item at its own indentation is a scan error
    /// ([`scan_source`]).
    ///
    /// Between the allocation and the door's first spawn (`.create(` or
    /// `spawn_blocking`), no line may leave the fn early (`return Err`, a `?`):
    /// such an exit drops the context, leaving the fresh tree on disk with no
    /// spawn-failed report — the shape POST /terminals' account resolution had.
    #[test]
    fn every_acquire_for_terminal_door_prechecks_and_removes_an_unspawned_allocation() {
        // Spelled in two halves so this test's own source is never a match.
        let call = concat!("acquire_for_terminal", "(");
        let definition = concat!("fn acquire_for_terminal", "(");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        files.sort();

        let mut doors = Vec::new();
        let mut failures = Vec::new();
        for file in &files {
            let src = std::fs::read_to_string(file).expect("read source");
            let scanned = scan_source(&src);
            for e in &scanned.errors {
                failures.push(format!("{}: {e}", file.display()));
            }
            let lines = scanned.production;
            let is_call = |l: &str| {
                !l.trim_start().starts_with("//") && l.contains(call) && !l.contains(definition)
            };
            failures.extend(
                partition_disagreement(&src, &lines, |l| is_call(l))
                    .map(|e| format!("{}: {e}", file.display())),
            );
            for (idx, line) in lines.iter().enumerate() {
                if !is_call(line) {
                    continue;
                }
                let rel = file
                    .strip_prefix(&root)
                    .unwrap_or(file)
                    .display()
                    .to_string();
                let site = format!("{rel}:{}", idx + 1);
                let Some((start, end)) = enclosing_fn(&lines, idx) else {
                    failures.push(format!("{site}: no enclosing fn found"));
                    continue;
                };
                let before = lines[start..idx].join("\n");
                let after = lines[idx..=end].join("\n");
                if !before.contains("precheck_spawn(") {
                    failures.push(format!(
                        "{site}: no `precheck_spawn(` before the allocation in `{}`",
                        lines[start].trim()
                    ));
                }
                let raw: Vec<String> = src.lines().map(str::to_string).collect();
                let code = code_lines(&lines);
                let token = DOOR_SPAWN_TOKENS
                    .iter()
                    .find(|(f, _)| *f == rel.replace('\\', "/"))
                    .map(|(_, t)| *t);
                let marker = match token {
                    None => Err(format!(
                        "{site}: no entry in DOOR_SPAWN_TOKENS names this door's real spawn call"
                    )),
                    Some(token) => spawn_marker(&raw, &code, idx, end, token),
                };
                match marker {
                    Err(e) => failures.push(format!("{site}: {e}")),
                    Ok(spawn) => {
                        for i in early_exits(&code, idx, spawn) {
                            failures.push(format!(
                                "{rel}:{}: an early exit between the allocation (line {}) \
                                 and the spawn drops the context: `{}`",
                                i + 1,
                                idx + 1,
                                lines[i].trim()
                            ));
                        }
                    }
                }
                if !after.contains("remove_unspawned(") {
                    failures.push(format!(
                        "{site}: no `remove_unspawned(` after the allocation in `{}`",
                        lines[start].trim()
                    ));
                }
                doors.push(rel);
            }
        }

        // Anti-vacuity: the four doors known at plan time must be found, so a
        // broken scan cannot pass by finding nothing.
        for known in [
            "commands/terminal.rs",
            "mcp/backend_relay.rs",
            "mcp/tauri_proxy.rs",
            "mcp/terminals.rs",
        ] {
            assert!(
                doors.iter().any(|d| d.replace('\\', "/") == known),
                "the scan did not find the `{known}` door; found {doors:?}"
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The fn name a `fn` opener line declares.
    fn fn_name(line: &str) -> String {
        let after = line.split("fn ").nth(1).unwrap_or("");
        after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    }

    /// Plan `2026-11-08-…-leaves-the-tree-on-disk` Phase 3: every OTHER
    /// allocating entry point — `acquire(AcquireRequest`, `acquire_for_worker(`,
    /// `acquire_continuation_workdir(`, `materialize_worktrees(` — is enumerated
    /// from the source, and each production call site must be in the
    /// disposition table below [policy: testing/coverage-is-enumerated-not-salient].
    /// A new call site is RED until it is classified.
    ///
    /// A `Door` must hand its unspawned allocation to the remover: each fn the
    /// row names must call the row's removal token (the door's own fn, or the
    /// spawn fns it dispatches to). An `Allocator` is an internal helper whose
    /// result is returned to a caller that is itself a row here.
    #[test]
    fn every_other_allocating_door_removes_an_unspawned_allocation() {
        enum Disposition {
            Allocator,
            Door(&'static [&'static str], &'static str),
        }
        use Disposition::*;
        // (file, enclosing fn of the call) -> disposition.
        let table: &[(&str, &str, Disposition)] = &[
            (
                "agent_worktree/isolated_edit.rs",
                "acquire_for_terminal",
                Allocator,
            ),
            (
                "agent_worktree/isolated_edit.rs",
                "acquire_for_worker",
                Allocator,
            ),
            (
                "agent_runtime.rs",
                "acquire_continuation_workdir",
                Allocator,
            ),
            (
                "commands/ai_session.rs",
                "reacquire_and_restore_session_worktree",
                Allocator,
            ),
            (
                "agent_runtime.rs",
                "run_gate_continuation_inner",
                Door(
                    &["run_continuation_terminal", "run_continuation_headless"],
                    "remove_unspawned_continuation(",
                ),
            ),
            (
                "agent_runtime.rs",
                "run_agent_subprocess",
                Door(&["run_agent_subprocess"], "remove_unspawned_launch_trees("),
            ),
            (
                "orchestration_loop/ai_session_executor.rs",
                "dispatch_subtask",
                Door(&["dispatch_subtask"], "remove_unspawned("),
            ),
        ];
        // The resume door reaches its allocation through the Allocator row's
        // fn, so it is checked by that fn's caller.
        let indirect: &[(&str, &str, &str)] = &[(
            "commands/ai_session.rs",
            "resume_ai_sessions",
            "remove_unspawned(",
        )];
        // The removal helpers themselves must reach the core.
        let helpers: &[(&str, &str, &str)] = &[
            (
                "agent_runtime.rs",
                "remove_unspawned_continuation",
                "remove_unspawned_unreported(",
            ),
            (
                "agent_runtime.rs",
                "remove_unspawned_launch_trees",
                "remove_unspawned_trees(",
            ),
        ];

        // Spelled in pieces so this test's own source is never a match.
        let calls = [
            concat!("acquire(", "AcquireRequest"),
            concat!("acquire_for_worker", "("),
            concat!("acquire_continuation_workdir", "("),
            concat!("materialize_worktrees", "("),
        ];
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        files.sort();

        let mut sources = std::collections::BTreeMap::new();
        let mut failures = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for file in &files {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .display()
                .to_string()
                .replace('\\', "/");
            let src = std::fs::read_to_string(file).expect("read source");
            let scanned = scan_source(&src);
            for e in &scanned.errors {
                failures.push(format!("{rel}: {e}"));
            }
            let lines = scanned.production;
            for (idx, line) in lines.iter().enumerate() {
                let is_call = calls.iter().any(|c| line.contains(c))
                    && !calls
                        .iter()
                        .any(|c| line.contains(&format!("fn {}", c.trim_end_matches('('))));
                if !is_call {
                    continue;
                }
                let site = format!("{rel}:{}", idx + 1);
                let Some((start, _)) = enclosing_fn(&lines, idx) else {
                    failures.push(format!("{site}: no enclosing fn found"));
                    continue;
                };
                let name = fn_name(&lines[start]);
                if table.iter().any(|(f, n, _)| *f == rel && *n == name) {
                    seen.insert((rel.clone(), name));
                } else {
                    failures.push(format!(
                        "{site}: allocating call in `{name}` has no disposition — classify it \
                         as a Door (with its removal) or an Allocator"
                    ));
                }
            }
            sources.insert(rel, lines);
        }

        let body_of = |file: &str, name: &str| -> Option<String> {
            let lines = sources.get(file)?;
            let idx = lines
                .iter()
                .position(|l| opens_fn(l) && fn_name(l) == name)?;
            let (start, end) = enclosing_fn(lines, idx)?;
            Some(lines[start..=end].join("\n"))
        };
        let require = |file: &str, name: &str, token: &str| match body_of(file, name) {
            None => Some(format!("{file}: fn `{name}` not found")),
            Some(body) if !body.contains(token) => {
                Some(format!("{file}: `{name}` never calls `{token}`"))
            }
            Some(_) => None,
        };
        for (file, name, disposition) in table {
            // Anti-vacuity: every row must still match a live call site.
            if !seen.contains(&(file.to_string(), name.to_string())) {
                failures.push(format!(
                    "{file}: the table row for `{name}` matched no call site (stale row or a \
                     blind scan)"
                ));
            }
            if let Door(fns, token) = disposition {
                for f in *fns {
                    failures.extend(require(file, f, token));
                }
            }
        }
        for (file, name, token) in indirect.iter().chain(helpers) {
            failures.extend(require(file, name, token));
        }

        // Every early exit a door fn can take once it holds the allocation
        // must hand it to the remover itself — a removal call (or a settle seam
        // that makes one) between the previous exit and this one — or carry a
        // `door-scan:` marker saying why the tree is legitimately kept there.
        // One removal call somewhere in the body is not enough.
        let exit_doors: &[(&str, &str, Option<&str>)] = &[
            ("agent_runtime.rs", "run_continuation_terminal", None),
            ("agent_runtime.rs", "run_continuation_headless", None),
            (
                "agent_runtime.rs",
                "run_agent_subprocess",
                Some(concat!("materialize_worktrees", "(")),
            ),
            (
                "orchestration_loop/ai_session_executor.rs",
                "dispatch_subtask",
                Some(concat!("acquire_for_worker", "(")),
            ),
            (
                "commands/ai_session.rs",
                "resume_ai_sessions",
                Some("reacquire_and_restore_session_worktree("),
            ),
        ];
        for (file, name, anchor) in exit_doors {
            let raw_src = std::fs::read_to_string(root.join(file)).expect("read source");
            let raw: Vec<String> = raw_src.lines().map(str::to_string).collect();
            failures.extend(unremoved_exits(file, &raw, name, *anchor));
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The calls that hand an unspawned allocation to the remover.
    const REMOVAL_TOKENS: &[&str] = &[
        "remove_unspawned(",
        "remove_unspawned_unreported(",
        "remove_unspawned_continuation(",
        "remove_unspawned_launch_trees(",
        "remove_unspawned_trees(",
        "settle_worker_spawn(",
        "settle_resume_spawn(",
        "abandon_before_resume_spawn(",
    ];

    /// Each early exit of fn `name` after `anchor` (the fn's start when
    /// `None`) with neither a removal call in its CODE since the previous exit
    /// nor a `door-scan:` marker comment there.
    fn unremoved_exits(
        file: &str,
        raw: &[String],
        name: &str,
        anchor: Option<&str>,
    ) -> Vec<String> {
        let production = production_code_lines(&raw.join("\n"));
        let Some(open) = production
            .iter()
            .position(|l| opens_fn(l) && fn_name(l) == name)
        else {
            return vec![format!("{file}: door fn `{name}` not found")];
        };
        let Some((start, end)) = enclosing_fn(&production, open) else {
            return vec![format!("{file}: door fn `{name}` has no body")];
        };
        let from = match anchor {
            None => start,
            Some(a) => match (start..=end).find(|&i| production[i].contains(a)) {
                Some(i) => i,
                None => return vec![format!("{file}: `{name}` no longer calls `{a}`")],
            },
        };
        let code = code_lines(&production);
        let profile = depth_profile(&code);
        let mut out = Vec::new();
        // The scan's own spans must agree on where this door fn is.
        let opener = format!("fn {name}(");
        out.extend(
            partition_disagreement(&raw.join("\n"), &production, |l| l.contains(&opener))
                .map(|e| format!("{file}: {e}")),
        );
        let exits = early_exits(&code, from, end + 1);
        if exits.is_empty() && !(start..=end).any(|i| raw[i].contains("door-scan: no-exits(")) {
            out.push(format!(
                "{file}: `{name}` has NO exit after the allocation that the scan can see — a \
                 door that can refuse always has one; if this one truly cannot, say so with \
                 `// door-scan: no-exits(<reason>)`"
            ));
        }
        let mut since = from;
        for e in exits {
            let ce = exit_col(&code[e]).unwrap_or(0);
            let removed = (since..=e).any(|r| {
                REMOVAL_TOKENS.iter().any(|t| {
                    code[r].find(t).is_some_and(|cr| {
                        (r, cr) < (e, ce) && in_scope(&code, &profile, (r, cr), (e, ce))
                    })
                })
            });
            let declared = match e.checked_sub(1).map(|a| keep_marker(&raw[a])) {
                Some(Ok(true)) => true,
                Some(Err(why)) => {
                    out.push(format!("{file}:{}: {why}", e));
                    true
                }
                _ => false,
            };
            if !removed && !declared {
                out.push(format!(
                    "{file}:{}: `{name}` can leave here holding the allocation with no removal \
                     in this block (or an enclosing one) since line {} and no \
                     `door-scan: keep(<reason>)` marker directly above: `{}`",
                    e + 1,
                    since + 1,
                    production[e].trim()
                ));
            }
            since = e + 1;
        }
        out
    }

    /// The relay's precheck sits BEFORE its grant gate, as the drain gate does,
    /// so a refusal spends no create grant.
    #[test]
    fn the_relay_prechecks_before_its_grant_gate_spends_a_grant() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/backend_relay.rs"),
        )
        .expect("read backend_relay.rs");
        let code = production_code_lines(&src).join("\n");
        let (_, body) = code
            .split_once("async fn handle_terminal_create(")
            .expect("handle_terminal_create");
        let precheck = body.find("precheck_spawn(").expect("the relay prechecks");
        let gate = body
            .find("let admitted = match gate(data)")
            .expect("the relay's grant gate");
        assert!(
            precheck < gate,
            "the relay must precheck before its grant gate consumes the grant"
        );
    }
}
