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
    // Relocated, not added: this is the same single loopback POST telling the
    // PRIMARY instance that a secondary is stopping. It moved out of `main.rs`
    // when the close handler's blocking teardown was collapsed into one
    // implementation off the native UI thread. Kind and count are unchanged,
    // so the coverage readout has no new hole.
    ("mcp/ai_session.rs", "not-coord", 1),
    ("mcp/device_jwt_refresher.rs", "not-coord", 1),
    ("mcp/device_jwt_refresher.rs", "self-refresh", 2),
    ("mcp/session_compliance.rs", "device-jwt-required", 1),
    ("mcp/session_message_poller.rs", "device-jwt-required", 2),
    // 4 since the label door: the write forwarder is verb-shaped (a DELETE arm
    // for the door's retract verb and a POST arm), each annotated at the site.
    ("mcp_api.rs", "forwarder", 4),
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

/// Files whose coord **reads** are pinned, and how many authed `.get(` builders
/// each must hold.
///
/// Inherited from `fleet.rs`'s `every_fleet_coord_writer_attaches_device_auth`
/// (runner#1035), which this file replaced. That test's write half is subsumed
/// by the repo-wide predicate above; its READ half is not, and dropping it on
/// the merge would have been a silent coverage regression, so it moves here
/// instead — one file, one place to review.
///
/// Deliberately a short explicit list rather than the repo-wide sweep the
/// writes get. `.get(` is not a discriminating token: `map.get(&k)`,
/// `body.get("token")` and `v.get("result")` are everywhere, and the
/// builder-shape filter cannot separate them from `client.get(&url)` reliably
/// enough to carry a whole-tree assertion. Widening reads properly is a
/// follow-up (plan §9); until then this pins what was already pinned rather
/// than pretending to more.
const PINNED_READ_FILES: &[(&str, usize)] = &[("fleet.rs", 1)];

/// Coord reads in the fleet publishers keep their device-JWT bearer.
///
/// Same reasoning as the write pin: `coord_http`'s module doc records that reads
/// and writes share one token source, so a read that drops the helper both loses
/// its bearer and vanishes from the `DATA_PLANE_TOTAL` / `DATA_PLANE_AUTHED`
/// readout.
#[test]
fn pinned_coord_reads_attach_device_auth() {
    let root = src_root();
    for (rel, want) in PINNED_READ_FILES {
        let path = root.join(rel);
        let body = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let lines: Vec<&str> = body.lines().collect();
        let test_ranges = cfg_test_ranges(&lines);

        let mut bare = 0usize;
        let mut authed = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if !line.contains(".get(&url)") {
                continue;
            }
            if test_ranges.iter().any(|(a, b)| i >= *a && i <= *b) {
                continue;
            }
            let start = statement_start(&lines, i);
            if code_only(&lines, start, i).contains("attach_device_auth") {
                authed += 1;
            } else if code_only(&lines, start, i).contains("client.get(&url)") {
                bare += 1;
            }
        }
        assert_eq!(
            bare, 0,
            "{rel}: {bare} coord GET builder(s) are not wrapped in the auth helper"
        );
        assert!(
            authed >= *want,
            "{rel}: expected at least {want} authenticated coord GET(s), found {authed} — \
             a fleet coord reader lost its device-JWT attachment (or was renamed, in which \
             case update PINNED_READ_FILES deliberately)"
        );
    }
}

// ============================================================================
// Second axis: WHICH TENANT.
//
// Plan `2026-08-29-runner-work-scoped-writes-default-tenant-credential`,
// Phase 4.
//
// The pin above answers "is this coord write authenticated at all". It cannot
// answer the question that follows, and the two are independent: a write can be
// perfectly authenticated and still land under the wrong tenant, because
// `auth::attach_device_auth` presents the DEFAULT binding's JWT. On every coord
// route that derives row ownership from the verified bearer —
// `ident.require_tenant()`, which has no fallback — a caller that could not work
// out its own tenant and used the defaulting wrapper writes a row under
// whichever tenant happens to be default.
//
// On a single-bound device that is right by accident. It becomes a real
// cross-tenant write the moment a second tenant is paired to the same box, and
// nothing in the tree would notice: `attach_device_auth`'s own doc comment has
// said "pass `None` and hope" is wrong since Phase 8b, and 52 call sites
// accumulated under it anyway. A convention that lives only in a doc comment is
// how you get 52 of them.
//
// Phase 5 (plan `2026-08-29-runner-work-scoped-writes-default-tenant-credential`)
// closed the ambiguity in the TYPE rather than in prose: the stating seam now
// takes an `auth::TenantScope`, so `Device` ("the bearer carries no tenancy
// here") and `Unresolved` ("this row has an owner I could not name") are no
// longer the same `None`, and only the second degrades on a multi-bound device.
// `attach_device_auth` is now literally `attach_device_auth_for(rb,
// TenantScope::Device)` — an ASSERTION about the route, which is exactly why
// every use of it still owes the annotation this file counts.
//
// So the same method the pin above used on the first axis applies to the
// second: not a hand-listed set of *writers*, but a predicate over all of them.
// Every use of the DEFAULTING wrapper must declare, at the site, which class it
// is in — and `EXPECTED_TENANT_SCOPES` pins the per-file counts so a new one
// cannot join any class without someone editing this table.
//
// Sites that call `attach_device_auth_for` need no annotation: they have
// already stated their tenant in code, where it is visible and type-checked.
// That asymmetry is the point — the annotation is the cost of using the
// wrapper that decides for you.
// ============================================================================

/// The marker a tenant-scope annotation must carry.
const TENANT_SCOPE_MARKER: &str = "coord-tenant-scope(";

/// The defaulting wrapper. Matching on the trailing `(` is what separates it
/// from `attach_device_auth_for(` / `attach_device_auth_blocking(`, which are
/// the tenant-STATING forms and are deliberately out of scope here.
const DEFAULTING_CALL: &str = "attach_device_auth(";

/// Every legal tenant-scope kind, with what it means. A kind outside this set
/// is a typo or an invention, and either way the test rejects it.
///
/// `device`, `session-noop` and `escalated` are TERMINAL — no future phase
/// lowers them. `device` is the reviewed allowlist; `work-owed` is the one
/// remaining debt, with a named creditor, so its count can be watched to zero
/// rather than tracked in a document that rots.
///
/// `session-owed` is gone: Phase 5 emptied it. Twelve of its nineteen sites now
/// state their tenant in code (`attach_device_auth_for(.., TenantScope)`) and
/// so need no annotation at all; six were reclassified `session-noop` and one
/// `escalated`. A `-owed` kind that could never reach zero would be worse than
/// no count, which is why the six terminal ones did not simply keep waiting for
/// a phase that has nothing to give them.
const TENANT_SCOPE_KINDS: &[(&str, &str)] = &[
    (
        "device",
        "the bearer carries no tenancy on this route — either the row is keyed \
         by device_id with no tenant dimension, or coord derives the tenant \
         from another field the caller already supplies. Either way the default \
         binding is correct by construction and stays correct however many \
         tenants are paired. The reviewed allowlist.",
    ),
    (
        "session-noop",
        "session-scoped, but the route carries no tenant the runner can set: it \
         persists none, or coord derives it from another request field (the \
         path agent_id, an already-stamped claim). TERMINAL — nothing to \
         thread, and no credential choice can move the row.",
    ),
    (
        "work-owed",
        "work-scoped and session-less: the tenant is a property of an artifact \
         (a plan, a work unit, a repo), so there is no session to ask. Owes \
         Phase 6's repo-derived resolution.",
    ),
    (
        "escalated",
        "cannot be resolved by a credential choice at all — a shared helper \
         whose callers span classes, or a route the runner may not be entitled \
         to call. The open question is the call or the mount, not the slot, so \
         classifying it would require a change rather than an annotation.",
    ),
];

/// Per-(file, kind) counts of tenant-scope annotations.
///
/// Pinned for the same reason [`EXPECTED_EXEMPTIONS`] is: "every site is
/// annotated" alone would let a new defaulting call site join the `device`
/// allowlist by writing a plausible-looking comment. This table is a
/// hand-reviewed list of *classifications*, and it should have to change
/// whenever one does.
///
/// It also gives Phase 6 a mechanical finish line: when `work-owed` reaches 0
/// the W class is resolved, and `device` + the two terminal kinds are what
/// should remain. Phase 5 already drove `session-owed` to 0 and its rows are
/// gone from this table.
const EXPECTED_TENANT_SCOPES: &[(&str, &str, usize)] = &[
    ("agent_runtime.rs", "device", 5),
    ("agent_runtime.rs", "session-noop", 3),
    ("agent_worktree/edit_effect_loop.rs", "session-noop", 1),
    ("agent_worktree/fs_backstop.rs", "work-owed", 1),
    ("commands/ai_settings.rs", "device", 1),
    ("commands/claims.rs", "session-noop", 1),
    ("coord_http.rs", "escalated", 1),
    ("coord_questions.rs", "session-noop", 1),
    ("fleet.rs", "device", 6),
    ("git_supervision/commit_forwarder.rs", "work-owed", 1),
    ("install_effects_producer/coord_client.rs", "work-owed", 2),
    ("looping_agent_coord.rs", "device", 5),
    ("mcp/plan_library.rs", "work-owed", 2),
    ("mcp/probe_executor.rs", "device", 1),
    ("plan_workunit_adapter/body_push.rs", "work-owed", 2),
    ("plan_workunit_adapter/push.rs", "work-owed", 5),
    ("repo_detection.rs", "work-owed", 1),
    ("session/handoff.rs", "escalated", 1),
];

/// Totals across the whole table, asserted independently of the per-file rows
/// so a transcription slip in one direction cannot be cancelled by another.
/// The Phase-2 census measured 52 sites at `ebbd3c70` (device 18, session-owed
/// 19, work-owed 14, escalated 1). Phase 5 removed 12 of the 19 from the
/// DEFAULTING wrapper entirely — they state their tenant in code now, so they
/// are no longer scanned here at all — and reclassified the remaining seven
/// (six `session-noop`, one `escalated`). 52 − 12 = 40.
const EXPECTED_TENANT_SCOPE_TOTALS: &[(&str, usize)] = &[
    ("device", 18),
    ("session-noop", 6),
    ("work-owed", 14),
    ("escalated", 2),
];

/// Extract the kind from `coord-tenant-scope(<kind>):`.
fn tenant_scope_kind(window: &str) -> Option<String> {
    let at = window.find(TENANT_SCOPE_MARKER)?;
    let rest = &window[at + TENANT_SCOPE_MARKER.len()..];
    let close = rest.find(')')?;
    Some(rest[..close].trim().to_string())
}

#[test]
fn every_defaulting_call_site_declares_its_tenant_scope() {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();

    let mut sites = 0usize;
    let mut unannotated: Vec<Site> = Vec::new();
    let mut bad_kinds: Vec<(Site, String)> = Vec::new();
    let mut empty_reasons: Vec<Site> = Vec::new();
    let mut found: BTreeMap<(String, String), usize> = BTreeMap::new();

    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .expect("path under src")
            .to_string_lossy()
            .replace('\\', "/");
        let body = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        if !body.contains(DEFAULTING_CALL) {
            continue;
        }
        let lines: Vec<&str> = body.lines().collect();
        let test_ranges = cfg_test_ranges(&lines);

        for (i, line) in lines.iter().enumerate() {
            // Comment-stripped, for the same reason every other scan here is:
            // this file and `auth.rs` both NAME the wrapper in prose, and prose
            // must never be scored as a call site.
            let code = code_only(&lines, i, i);
            if !code.contains(DEFAULTING_CALL) {
                continue;
            }
            // The definition itself is not a call site.
            if code.contains(&format!("fn {DEFAULTING_CALL}")) {
                continue;
            }
            if test_ranges.iter().any(|(a, b)| i >= *a && i <= *b) {
                continue;
            }
            sites += 1;

            let (window, _stmt_start) = annotation_block(&lines, i);
            let site = Site {
                file: rel.clone(),
                line: i + 1,
                text: line.trim().to_string(),
            };
            let Some(kind) = tenant_scope_kind(&window) else {
                unannotated.push(site);
                continue;
            };
            if !TENANT_SCOPE_KINDS.iter().any(|(k, _)| *k == kind) {
                bad_kinds.push((site, kind));
                continue;
            }
            // A kind with no reason after it is an annotation in form only.
            let after = window
                .split_once(&format!("{TENANT_SCOPE_MARKER}{kind}):"))
                .map(|(_, rest)| rest.trim())
                .unwrap_or("");
            if after.is_empty() {
                empty_reasons.push(site);
                continue;
            }
            *found.entry((rel.clone(), kind)).or_default() += 1;
        }
    }

    assert!(
        unannotated.is_empty(),
        "{} call site(s) use the DEFAULTING `attach_device_auth` without declaring a tenant \
         scope:\n{}\n\nEvery use of the defaulting wrapper must say which class it is in, \
         because `None` is not 'anonymous' — it presents the DEFAULT binding's credential. \
         Either state the tenant in code (`attach_device_auth_for(.., tenant)`), which needs no \
         annotation, or add `// coord-tenant-scope(<kind>): <reason>` above the statement AND a \
         row in EXPECTED_TENANT_SCOPES. Legal kinds:\n{}",
        unannotated.len(),
        unannotated
            .iter()
            .map(|s| format!("  {}:{}  {}", s.file, s.line, s.text))
            .collect::<Vec<_>>()
            .join("\n"),
        TENANT_SCOPE_KINDS
            .iter()
            .map(|(k, why)| format!("  {k}: {why}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        bad_kinds.is_empty(),
        "unknown tenant-scope kind(s):\n{}",
        bad_kinds
            .iter()
            .map(|(s, k)| format!("  {}:{}  `{k}`", s.file, s.line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        empty_reasons.is_empty(),
        "tenant-scope annotation(s) with no reason after the kind:\n{}\n\nThe reason lives at \
         the site so it cannot drift from the code it classifies.",
        empty_reasons
            .iter()
            .map(|s| format!("  {}:{}", s.file, s.line))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let expected: BTreeMap<(String, String), usize> = EXPECTED_TENANT_SCOPES
        .iter()
        .map(|(f, k, n)| ((f.to_string(), k.to_string()), *n))
        .collect();
    assert_eq!(
        found, expected,
        "the tenant-scope classification changed. This is a REVIEW prompt, not a nuisance: a \
         site moving between classes changes which tenant its writes land under. Update \
         EXPECTED_TENANT_SCOPES in tests/coord_auth_pin.rs — and if a site joined `device`, \
         confirm its coord route really has no tenant dimension before you do."
    );

    // Totals, checked independently of the per-file rows.
    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for ((_, kind), n) in &found {
        *totals.entry(kind.as_str()).or_default() += n;
    }
    for (kind, want) in EXPECTED_TENANT_SCOPE_TOTALS {
        let got = totals.get(kind).copied().unwrap_or(0);
        assert_eq!(
            got, *want,
            "tenant-scope `{kind}`: found {got}, expected {want}. If a phase legitimately moved \
             sites out of a class, lower the number here in the SAME commit — a debt count that \
             is not watched is not a debt."
        );
    }

    let total: usize = totals.values().sum();
    assert_eq!(
        total, sites,
        "every scanned defaulting call site should have been classified"
    );
    assert_eq!(
        sites, 40,
        "expected 40 defaulting call sites — the Phase-2 census's 52 at ebbd3c70 minus the 12 \
         session-scoped ones Phase 5 moved onto the tenant-STATING seam; found {sites}. A \
         change here is fine — it just has to be deliberate. It goes DOWN when a site adopts \
         `attach_device_auth_for(.., TenantScope)`, and UP only when someone adds a new \
         defaulting caller, which is the event this number exists to make visible."
    );
}

#[test]
fn tenant_scope_kinds_are_a_closed_distinct_set() {
    let mut seen = std::collections::BTreeSet::new();
    for (kind, why) in TENANT_SCOPE_KINDS {
        assert!(
            seen.insert(*kind),
            "TENANT_SCOPE_KINDS lists `{kind}` twice"
        );
        assert!(!why.trim().is_empty(), "kind `{kind}` has no meaning given");
    }
    for (file, kind, n) in EXPECTED_TENANT_SCOPES {
        assert!(
            seen.contains(kind),
            "EXPECTED_TENANT_SCOPES names unknown kind `{kind}` for {file}"
        );
        assert!(
            *n > 0,
            "EXPECTED_TENANT_SCOPES row for {file}/{kind} is 0 — delete the row instead"
        );
    }
    for (kind, _) in EXPECTED_TENANT_SCOPE_TOTALS {
        assert!(
            seen.contains(*kind),
            "EXPECTED_TENANT_SCOPE_TOTALS names unknown kind `{kind}`"
        );
    }
}
