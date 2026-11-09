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
    }

    impl BraceLexer {
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
                            i += 2 + hashes;
                        } else {
                            i += 1;
                        }
                    }
                    b'"' => {
                        self.in_str = true;
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
                        i = close_at.map_or(i + 1, |c| c + 1);
                    }
                    b'{' => {
                        open += 1;
                        i += 1;
                    }
                    b'}' => {
                        close += 1;
                        i += 1;
                    }
                    _ => i += 1,
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
    /// `#[cfg(test)] mod x;` declaration hides no production code. As a check
    /// on the scan itself, every file's raw count of calls must equal the
    /// production calls scanned plus the ones inside skipped test blocks.
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
            let raw = src.lines().filter(|l| is_call(l)).count();
            let in_tests = scanned.test_lines.iter().filter(|l| is_call(l)).count();
            let in_production = lines.iter().filter(|l| is_call(l)).count();
            assert_eq!(
                raw,
                in_production + in_tests,
                "{}: the scan lost a call site (raw {raw}, production {in_production}, \
                 test blocks {in_tests})",
                file.display()
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
