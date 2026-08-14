//! Regression guard: every coord-bound HTTP **write** in `src-tauri/src` either
//! routes through `auth::attach_device_auth*` or carries an annotated,
//! reviewed exemption.
//!
//! ## Why this test exists
//!
//! `auth::attach_device_auth` increments `DATA_PLANE_TOTAL`, and
//! `DATA_PLANE_AUTHED` when a credential resolves. **A writer that never calls
//! it is not counted as unauthenticated — it is not counted at all.** So the
//! coverage readout can sit at a clean 100% while writers publish anonymously
//! beside it. That readout is what plan `2026-08-03-per-instance-device-identity`
//! Phase 3(b) (gate `64fc698a`) was going to gate an enforcement flip on.
//!
//! The gap did not come from carelessness; it came from *method*. Twice, a
//! session reasoned about a handful of **named functions** ("the two fleet
//! writers are the last ones", then "there are thirteen") instead of running a
//! predicate over all of them. The first count was 2, the second 13, the real
//! one was 30. `session/coord_sync.rs` is the clinching case: ten of its eleven
//! coord writes present the owning session's device-JWT slot and `probe_resume`
//! — added later, for a different reason — presented nothing. Nobody decided
//! that.
//!
//! So the deliverable is a predicate, and this is it. Plan
//! `2026-08-14-runner-unauthenticated-coord-writers` §6.
//!
//! ## What it checks
//!
//! It **walks `src/` at test time** rather than listing files with
//! `include_str!`, because a hand-listed set cannot see a newly-added file —
//! and "a new writer joins the gap silently" is the exact failure being pinned.
//!
//! For every `.post(` / `.put(` / `.patch(` / `.delete(` occurrence that
//!
//! 1. lives in a file that mentions coord at all ([`COORD_MARKERS`]),
//! 2. sits outside a `#[cfg(test)]` block, and
//! 3. looks like a reqwest builder chain (a `.send()` / `.json()` /
//!    `.bearer_auth()` / … nearby),
//!
//! there must be **either** an `attach_device_auth` somewhere in the SAME
//! STATEMENT ([`statement_start`]), **or** a
//! `coord-auth-exempt(<kind>): <reason>` annotation in the comment block
//! immediately above that statement ([`annotation_block`]).
//!
//! Both scans run with **comments stripped** ([`code_only`]) where they are
//! deciding about code, because prose must never satisfy a code-level check.
//! Two false-greens of exactly that shape were found and fixed while building
//! this file: annotation text containing `attach_device_auth` scored a site as
//! ROUTED, and a comment quoting `#[cfg(test)]` opened a skip range.
//!
//! ## Why the coord predicate is per-FILE and not per-function
//!
//! A per-function predicate needs brace matching, and it produces a
//! false-negative class: a coord writer whose enclosing function never says
//! "coord" because its base URL arrives as a `&str` parameter.
//! `session/output_pipe.rs::flush` is exactly that shape, and it was a real
//! anonymous coord writer that a function-scoped scan missed during this very
//! audit. The file-level predicate over-collects instead — it sweeps in the
//! Anthropic/Gemini calls in `commands/ai_settings.rs` and the loopback calls
//! in `main.rs` — and over-collection costs one `not-coord` annotation naming
//! what the call actually talks to.
//!
//! **False positives cost a comment; false negatives cost the property.**
//!
//! ## Why the exemption COUNTS are pinned too
//!
//! "Every site is annotated" alone would let a new writer join the exempt set
//! by writing a plausible-looking comment. [`EXPECTED_EXEMPTIONS`] pins the
//! exact per-file, per-kind counts, so any new exemption — even a correctly
//! annotated one — fails until someone edits the table. That is deliberate:
//! this is not a hand-counted list of *writers* (which would rot on every new
//! writer, the very anti-pattern being fixed) but a hand-reviewed list of
//! *exceptions*, which should rot on every new exception.
//!
//! ## Fixing a failure
//!
//! - **"not routed and not annotated"** — wrap the builder:
//!   `crate::auth::attach_device_auth(client.post(&url).json(&body))`, or
//!   `attach_device_auth_for(.., tenant)` when the request is tenant-scoped, or
//!   `attach_device_auth_blocking(.., tenant)` off a tokio runtime.
//! - **The call is not coord-bound, or must present a different credential** —
//!   annotate it at the site with `// coord-auth-exempt(<kind>): <reason>` and
//!   add it to [`EXPECTED_EXEMPTIONS`]. The reason lives at the site, not in
//!   this file, so it cannot drift from the code it excuses.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Substrings that make a file "coord-touching" and therefore in scope.
///
/// Deliberately broad — see the module doc on over-collection. A file that
/// writes to coord cannot avoid all of these: it needs a base URL from
/// somewhere, and every base source in this repo is spelled with `coord`.
const COORD_MARKERS: &[&str] = &[
    "/coord/",
    "coord_http_base",
    "coord_base",
    "coord_url",
    "coord_client",
    "coord_mcp",
    "coord.qontinui.io",
];

/// HTTP verbs that MUTATE. Reads are out of scope for this pin: `coord_http`
/// routes the ones that go through it, and a `.get(` sweep would pull in every
/// map/cache `.get(` in the tree. Plan §9 records extending this.
const WRITE_VERBS: &[&str] = &[".post(", ".put(", ".patch(", ".delete("];

/// Tokens that identify a `.post(`-alike as a reqwest builder rather than a
/// `HashMap::insert`-alike, searched in a window around the call.
const BUILDER_TOKENS: &[&str] = &[
    ".send()",
    ".json(",
    ".bearer_auth(",
    ".header(",
    ".body(",
    ".query(",
    ".timeout(",
];

/// The marker an exemption annotation must carry.
const EXEMPT_MARKER: &str = "coord-auth-exempt(";

/// Every legal exemption kind, with what it means. A kind outside this set is
/// a typo or an invention, and either way the test rejects it.
const EXEMPT_KINDS: &[(&str, &str)] = &[
    (
        "bootstrap",
        "mints the credential; requiring one would be circular",
    ),
    ("forwarder", "a proxy hop presenting the CALLER's bearer"),
    ("self-refresh", "presents the very token being refreshed"),
    (
        "agent-jwt",
        "presents the per-agent coord JWT, not the device's",
    ),
    ("user-jwt", "presents the operator's Cognito token"),
    (
        "device-jwt-required",
        "presents the device JWT but fails CLOSED; the helper is fail-soft",
    ),
    (
        "diagnostic",
        "reports on the credential chain, so it reads it raw",
    ),
    (
        "not-coord",
        "the peer is not coord (loopback, qontinui-web, a model vendor)",
    ),
];

/// The exact exemption inventory: `(file, kind, count)`.
///
/// Reviewed 2026-08-14 against plan
/// `2026-08-14-runner-unauthenticated-coord-writers` §2b/§2c. Each entry's
/// *reason* lives at the call site; this table exists only so the SET cannot
/// grow without someone editing it.
const EXPECTED_EXEMPTIONS: &[(&str, &str, usize)] = &[
    ("agent_token/mod.rs", "agent-jwt", 1),
    ("bin/qontinui_cli.rs", "not-coord", 1),
    ("ci_node/reporting.rs", "device-jwt-required", 2),
    ("commands/ai_settings.rs", "not-coord", 3),
    ("commands/productivity.rs", "user-jwt", 1),
    ("commands/web_integration.rs", "not-coord", 2),
    ("coord_doctor.rs", "diagnostic", 1),
    ("coord_mcp.rs", "not-coord", 1),
    ("credential_helper.rs", "device-jwt-required", 1),
    ("dirty_poller/mod.rs", "agent-jwt", 1),
    ("env_agent/enroll.rs", "not-coord", 1),
    ("fleet/resource_sample.rs", "device-jwt-required", 1),
    ("main.rs", "not-coord", 1),
    ("mcp/device_jwt_refresher.rs", "not-coord", 1),
    ("mcp/device_jwt_refresher.rs", "self-refresh", 2),
    ("mcp/session_compliance.rs", "device-jwt-required", 1),
    ("mcp/session_message_poller.rs", "device-jwt-required", 2),
    ("mcp_api.rs", "forwarder", 3),
    ("memory/memory_synthesis.rs", "not-coord", 2),
    ("memory/tenant_sync.rs", "not-coord", 1),
    (
        "observable_bridge/git_ops_client.rs",
        "device-jwt-required",
        1,
    ),
    ("orchestration_loop/coord_gate.rs", "device-jwt-required", 1),
    ("pair.rs", "bootstrap", 1),
    ("pair.rs", "not-coord", 2),
    ("session_bus.rs", "device-jwt-required", 1),
    ("session_bus.rs", "not-coord", 1),
    ("terminal/context_watcher.rs", "not-coord", 1),
];

/// Floor on the number of in-scope write sites the walk must find.
///
/// A guard that reports green having scanned nothing is worse than no guard.
/// If a refactor, a moved `src/`, or a broken [`BUILDER_TOKENS`] filter drops
/// the population below this, the test fails and says so rather than passing
/// vacuously. Set well under the 2026-08-14 census (98) so ordinary churn does
/// not trip it.
const MIN_SITES_SCANNED: usize = 70;

/// Floor on the number of sites found routed through the helper. Guards the
/// mirror-image vacuity: a broken auth scan that thinks everything is exempt.
const MIN_SITES_ROUTED: usize = 45;

#[derive(Debug)]
struct Site {
    file: String,
    line: usize,
    text: String,
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// How far past a `#[cfg(test)]` attribute the item's opening brace may sit.
///
/// `#[cfg(test)]` also decorates BRACELESS items — a `static`, a `const`, a
/// `use`, and (in `claude_session/coord_register.rs`) a struct FIELD. An
/// unbounded search for the next `{` sails past those and latches onto the
/// following braced item, which is PRODUCTION code: at the time this bound was
/// added, the field at `coord_register.rs:148` made the scanner skip 590
/// contiguous lines of a coord-writing file, including the `impl` holding its
/// `POST /agents/{id}/log` batcher. Nothing in that span was a write site, so
/// the guard was green — by luck, not by predicate.
///
/// A real `#[cfg(test)] mod`/`fn`/`impl` opens on the attribute line or the one
/// after it (other attributes stack above, not between). Anything further away
/// is a braceless item, and is treated as one.
const CFG_TEST_BRACE_WINDOW: usize = 1;

/// Line index ranges (inclusive) covered by `#[cfg(test)]` items.
///
/// Everything here fails toward **scanning more**. A range that is too small
/// costs a spurious finding someone must look at; a range that is too large
/// silently hides an anonymous coord writer forever. Those are not comparable,
/// so every ambiguous case collapses to `(i, i)` — skip the attribute line only.
fn cfg_test_ranges(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // Comment-stripped, for the same reason the auth scan is: prose must
        // never drive a code-level decision. `plan_workunit_adapter/mod.rs`
        // has a comment quoting `#[cfg(test)]`, and on the raw text that starts
        // a range — the same class as the annotation-prose false-green already
        // fixed one layer down, except this one blanks whole functions.
        if !code_only(lines, i, i).contains("#[cfg(test)]") {
            i += 1;
            continue;
        }
        // Find the item's opening line, take its indentation, and close on the
        // first line that is exactly that indentation plus `}`.
        //
        // NOT brace counting. A `{` inside a string literal — `format!("{")`,
        // a JSON fixture, a raw string — unbalances a counter and silently
        // extends the skipped region. Every `#[cfg(test)]` item in this tree is
        // rustfmt-formatted, so its closing brace sits alone at the item's own
        // indentation; matching on that is immune to anything inside a literal.
        let last = lines.len().saturating_sub(1);
        let end = (i..=(i + CFG_TEST_BRACE_WINDOW).min(last))
            .find(|&j| lines[j].contains('{'))
            .and_then(|open| {
                let indent_len = lines[open].len() - lines[open].trim_start().len();
                let closer = format!("{}}}", &lines[open][..indent_len]);
                (open + 1..lines.len()).find(|&k| lines[k].trim_end() == closer)
            });
        match end {
            Some(end) => {
                out.push((i, end));
                i = end + 1;
            }
            // Braceless item, or unclosed at this indentation.
            None => {
                out.push((i, i));
                i += 1;
            }
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Join `lines[lo..=hi]` with comments removed, so prose can never satisfy a
/// code-level check. Line comments are dropped whole; a trailing `//` truncates
/// its line. Truncating at `//` inside a URL literal is harmless here: the only
/// token looked for is `attach_device_auth`, which in a wrapped call always
/// precedes the URL.
fn code_only(lines: &[&str], lo: usize, hi: usize) -> String {
    lines[lo..=hi]
        .iter()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("//") {
                ""
            } else {
                l.split("//").next().unwrap_or("")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The contiguous comment block immediately above the statement containing the
/// call on line `i`, plus that statement's own lines.
///
/// Walking the block rather than a fixed number of lines is what lets an
/// exemption carry a real, multi-line reason. A fixed window silently rejects
/// the sites whose justification runs long — which are exactly the sites whose
/// justification most needs reading.
fn annotation_block(lines: &[&str], i: usize) -> (String, usize) {
    let start = statement_start(lines, i);
    // Then over the contiguous comment block above the statement.
    let mut top = start;
    while top > 0 && lines[top - 1].trim_start().starts_with("//") {
        top -= 1;
    }
    (lines[top..start].join("\n"), start)
}

/// How far back the statement walk will look before giving up. Bounds the work
/// and, on the rare statement longer than this, fails toward "not routed" —
/// a visible finding rather than a silent pass.
const STATEMENT_LOOKBACK: usize = 10;

/// First line of the statement containing the call on line `i`.
///
/// Walks back until a **statement boundary** — a code line whose trimmed form
/// ends in `;`, `{` or `}`, or a blank line. Everything after that boundary is
/// one statement with the call, whatever its shape:
///
/// ```ignore
/// let resp = crate::auth::attach_device_auth(   // included
///     client                                    // included
///         .post(&url)                           // the call
/// ```
///
/// Boundary-walking rather than "back up over lines starting with `.`", because
/// the wrapped-argument shape — the call sitting as an ARGUMENT to the helper,
/// on its own line — is the most common form in this tree and does not start
/// with `.`:
///
/// ```ignore
/// match crate::auth::attach_device_auth_for(
///     client.post(url).json(&body),             // ← does not start with `.`
///     self.tenant_id.as_ref(),
/// )
/// ```
///
/// And boundary-walking is what makes the scan reject the adjacency false-green
/// a fixed lookback window admits, since the preceding statement's `;` stops
/// the walk:
///
/// ```ignore
/// let a = crate::auth::attach_device_auth(client.post(&u1)).send().await;
/// let c = client.post(&u2).send().await;   // anonymous — walk stops at the `;`
/// ```
///
/// Comment lines are skipped, not treated as boundaries: an annotation block
/// sits between a statement and the code above it, and a multi-line wrapped
/// call is frequently preceded by one.
fn statement_start(lines: &[&str], i: usize) -> usize {
    let mut start = i;
    let floor = i.saturating_sub(STATEMENT_LOOKBACK);
    while start > floor {
        let prev = lines[start - 1];
        let trimmed = prev.trim();
        if trimmed.starts_with("//") {
            // A comment never joins or ends a statement; look past it without
            // pulling it into the statement span.
            break;
        }
        let code = code_only(lines, start - 1, start - 1);
        let code = code.trim_end();
        if code.is_empty() || code.ends_with(';') || code.ends_with('{') || code.ends_with('}') {
            break;
        }
        start -= 1;
    }
    start
}

/// Extract the kind from `coord-auth-exempt(<kind>):`.
fn exempt_kind(window: &str) -> Option<String> {
    let at = window.find(EXEMPT_MARKER)?;
    let rest = &window[at + EXEMPT_MARKER.len()..];
    let close = rest.find(')')?;
    Some(rest[..close].trim().to_string())
}

#[test]
fn every_coord_write_is_authenticated_or_annotated() {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    assert!(
        files.len() > 100,
        "walked {} .rs files under {} — the source tree moved and this guard \
         scanned almost nothing",
        files.len(),
        root.display()
    );

    let mut scanned = 0usize;
    let mut routed = 0usize;
    let mut unaccounted: Vec<Site> = Vec::new();
    let mut found_exemptions: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut bad_kinds: Vec<(Site, String)> = Vec::new();
    let mut empty_reasons: Vec<Site> = Vec::new();

    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .expect("path under src")
            .to_string_lossy()
            .replace('\\', "/");
        let body = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        if !COORD_MARKERS.iter().any(|m| body.contains(m)) {
            continue;
        }
        let lines: Vec<&str> = body.lines().collect();
        let test_ranges = cfg_test_ranges(&lines);

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            if !WRITE_VERBS.iter().any(|v| line.contains(v)) {
                continue;
            }
            if test_ranges.iter().any(|(a, b)| i >= *a && i <= *b) {
                continue;
            }
            // Builder-chain shape, or it is a map/cache call we do not care about.
            let lo = i.saturating_sub(6);
            let hi = (i + 10).min(lines.len());
            let chain = lines[lo..hi].join("\n");
            if !BUILDER_TOKENS.iter().any(|t| chain.contains(t)) {
                continue;
            }
            scanned += 1;

            let (ex_window, stmt_start) = annotation_block(&lines, i);

            // Scoped to THIS statement, not a fixed window. A fixed lookback
            // scores a bare `client.post(&url)` as ROUTED whenever a wrapped
            // call happens to sit on the line above — and because it then
            // counts as routed rather than exempt, `EXPECTED_EXEMPTIONS` never
            // changes and the equality below, the main anti-drift device, never
            // fires. That is precisely "a new writer joins the gap silently".
            if code_only(&lines, stmt_start, i).contains("attach_device_auth") {
                routed += 1;
                continue;
            }

            let site = Site {
                file: rel.clone(),
                line: i + 1,
                text: trimmed.chars().take(90).collect(),
            };
            match exempt_kind(&ex_window) {
                None => unaccounted.push(site),
                Some(kind) => {
                    if !EXEMPT_KINDS.iter().any(|(k, _)| *k == kind) {
                        bad_kinds.push((site, kind));
                        continue;
                    }
                    // A kind with no prose after the colon excuses nothing.
                    //
                    // Measured over the COMMENT BLOCK only — `ex_window` no
                    // longer carries the statement's own source lines. It used
                    // to, which made this check inert: a bare
                    // `// coord-auth-exempt(not-coord):` passed because the
                    // `let resp = client.post(&url)` underneath supplied the
                    // characters. A guard that reports green having verified
                    // nothing is the failure mode this file is least allowed
                    // to have.
                    let at = ex_window.find(EXEMPT_MARKER).expect("marker present");
                    let reason = ex_window[at..]
                        .split_once("):")
                        .map(|(_, r)| r.trim())
                        .unwrap_or_default();
                    if reason.len() < 12 {
                        empty_reasons.push(site);
                        continue;
                    }
                    *found_exemptions.entry((rel.clone(), kind)).or_insert(0) += 1;
                }
            }
        }
    }

    // --- vacuity guards: a green that checked nothing is not a green ---
    assert!(
        scanned >= MIN_SITES_SCANNED,
        "only {scanned} coord-touching HTTP write sites found (floor {MIN_SITES_SCANNED}). \
         The scan is broken or the tree moved — this test cannot honestly pass."
    );
    assert!(
        routed >= MIN_SITES_ROUTED,
        "only {routed} of {scanned} sites read as routed through the auth helper \
         (floor {MIN_SITES_ROUTED}). The auth-detection window is broken."
    );

    // --- the property ---
    assert!(
        unaccounted.is_empty(),
        "coord-bound HTTP write(s) neither routed through `auth::attach_device_auth*` \
         nor annotated `coord-auth-exempt(<kind>): <reason>`:\n{}\n\n\
         Wrap the builder, or annotate the site AND add it to EXPECTED_EXEMPTIONS \
         in tests/coord_auth_pin.rs. A writer that skips the helper is invisible to \
         the DATA_PLANE_TOTAL/AUTHED coverage readout — it reads as 100% while that \
         writer publishes anonymously.",
        unaccounted
            .iter()
            .map(|s| format!("  {}:{}  {}", s.file, s.line, s.text))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        bad_kinds.is_empty(),
        "unknown coord-auth-exempt kind(s); legal kinds are {:?}:\n{}",
        EXEMPT_KINDS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        bad_kinds
            .iter()
            .map(|(s, k)| format!("  {}:{}  kind={k}", s.file, s.line))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        empty_reasons.is_empty(),
        "coord-auth-exempt annotation(s) with no stated reason — every exemption is a \
         permanent hole and must name why it exists:\n{}",
        empty_reasons
            .iter()
            .map(|s| format!("  {}:{}", s.file, s.line))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // --- the equality: the exemption SET may not drift unreviewed ---
    let expected: BTreeMap<(String, String), usize> = EXPECTED_EXEMPTIONS
        .iter()
        .map(|(f, k, n)| ((f.to_string(), k.to_string()), *n))
        .collect();
    assert_eq!(
        found_exemptions, expected,
        "the coord-auth exemption inventory changed.\n\
         Each exemption is a PERMANENT hole in the coverage readout, so the set is \
         reviewed rather than merged: update EXPECTED_EXEMPTIONS in \
         tests/coord_auth_pin.rs deliberately, and state the reason at the call site.\n\
         found:    {found_exemptions:#?}\n\
         expected: {expected:#?}"
    );
}

/// The exemption-kind vocabulary must stay a closed set with distinct names —
/// a duplicated or empty kind would silently widen what
/// `every_coord_write_is_authenticated_or_annotated` accepts.
#[test]
fn exempt_kinds_are_a_closed_distinct_set() {
    let mut seen = std::collections::BTreeSet::new();
    for (kind, why) in EXEMPT_KINDS {
        assert!(!kind.is_empty(), "an exemption kind may not be empty");
        assert!(
            why.len() >= 20,
            "exemption kind `{kind}` needs a meaning, not a label"
        );
        assert!(seen.insert(*kind), "duplicate exemption kind `{kind}`");
    }
    // Every kind in the pinned inventory must be one of them, so a table entry
    // cannot introduce a kind the site-level check would then reject.
    for (file, kind, _) in EXPECTED_EXEMPTIONS {
        assert!(
            EXEMPT_KINDS.iter().any(|(k, _)| k == kind),
            "EXPECTED_EXEMPTIONS names unknown kind `{kind}` for {file}"
        );
    }
}
