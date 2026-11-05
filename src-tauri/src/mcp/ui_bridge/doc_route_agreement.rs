//! The bundled fleet docs name only UI Bridge routes the runner serves.
//!
//! The runner embeds the fleet's slash commands (`crate::fleet_commands`) and
//! skills (`crate::fleet_skills`) and provisions them into every session it
//! spawns. Those docs are where an agent learns the UI Bridge's vocabulary,
//! so a doc that names a deleted route sends every session that reads it to
//! a `404`. That happened: `/ui-bridge` and `/ufix` told agents to call
//! `GET /ui-bridge/control/annotated-screenshot` long after the vision
//! pipeline deleted it (plan
//! `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
//! Phase 6).
//!
//! The docs are authored in qontinui-claude-config and reach this repo through
//! the bundle sync bot, so this test is the check that runs when a sync PR
//! carries a doc naming a route the runner does not serve.
//!
//! ## What "served" means here
//!
//! The plan preferred building the real router and probing each literal.
//! `crate::mcp_api::create_router` needs a live `tauri::AppHandle`, so a unit
//! test cannot build it. Its own fallback, a union of the per-family route
//! tables, misses every family that has no table (`/ui-bridge/devices/*`,
//! `/ui-bridge/sdk/*` and others). This test reuses the runner's existing
//! source-scanning route enumerator instead
//! (`relay_path_policy::tests::registered_routes`, also used by
//! `origin_guard`'s tripwires). It reads every literal `.route(…)` path and
//! both routes of every `add_dual!(…)`. That is a superset of
//! `route_manifest()`, which is what `GET /ui-bridge/_routes` serves and which
//! covers the ui_bridge family only. `scan_covers_the_route_manifest` checks
//! the enumerator against `route_manifest()`, so it cannot quietly go blind.
//!
//! ## What a doc literal must satisfy
//!
//! Every `/ui-bridge/...` path a bundled doc names against the runner must:
//!
//! * be served: the same length as a registered route and segment-wise equal
//!   to it, where a `{param}` segment in the route matches any one segment. A
//!   catch-all `{*rest}` route never matches, so one proxy route cannot
//!   vouch for every path under it. A doc literal ending in `*` names a family
//!   by prefix (`/x/*`, or `/x/ab*` for the next segment). The few bare family
//!   names in [`FAMILY_NAMES`] pass when some served route sits under them;
//!   **or**
//! * be in [`REMOVED_UI_BRIDGE_ROUTES`] **and** sit in a paragraph that says
//!   "deleted" or "removed", so the doc reports the deletion rather than
//!   prescribing the call; **or**
//! * be on [`ALLOW_LIST`], each entry with its reason.
//!
//! Literals that are not runner paths are excluded before the check. These
//! are repo paths (`qontinui/ui-bridge`, `../ui-bridge/packages/...`), the
//! web relay (`<origin>/api/ui-bridge`), and the bare word `ui-bridge`.
//!
//! [`REMOVED_UI_BRIDGE_ROUTES`]: super::removed_routes::REMOVED_UI_BRIDGE_ROUTES

use regex::Regex;

use super::removed_routes::removed_route_replacement;

/// Doc literals that pass without being served, each with its reason. Keep
/// this short. An entry that no bundled doc names any more, or that the
/// runner now serves, fails `allow_list_is_live`.
const ALLOW_LIST: &[(&str, &str)] = &[(
    "/ui-bridge/control/diagnose-stuck-screen",
    "named only in ui-bridge-debug's deletion note; the runner never served \
     it under this name, so qontinui-runner#2580 (17b5e0ab9) dropped it from \
     REMOVED_UI_BRIDGE_ROUTES rather than give a never-served path a \
     replacement hint",
)];

/// The non-vacuity floor, counted over CHECKABLE literals only: those with at
/// least two segments and no trailing `*` (a bare `/ui-bridge`, mostly the
/// slash command's own name in prose, or a `/ui-bridge/x/*` family passes
/// whatever the routes are, so it proves nothing). The test prints the live
/// count. The floor sits well under it, so routine doc edits do not trip it,
/// but an extractor that stopped finding URLs or truncated every match to
/// `/ui-bridge` does.
const MIN_CHECKABLE_LITERALS: usize = 50;

/// Doc literals that name a route FAMILY rather than a route: shorter than
/// any route they cover, written without a trailing `*`. Any other literal
/// must name a whole route (or end in `*` to name a family).
const FAMILY_NAMES: &[&str] = &["/ui-bridge", "/ui-bridge/invoke"];

/// One `/ui-bridge` path a bundled doc names.
#[derive(Debug, Clone)]
struct DocLiteral {
    doc: String,
    line: usize,
    path: String,
    /// The blank-line-delimited paragraph the literal sits in, lowercased.
    paragraph: String,
}

/// Every bundled markdown doc as `(name, text)`: the commands from
/// `FLEET_COMMANDS`, plus every `.md` file under the embedded skills tree.
fn bundled_docs() -> Vec<(String, &'static str)> {
    fn walk(dir: &'static include_dir::Dir<'static>, out: &mut Vec<(String, &'static str)>) {
        for file in dir.files() {
            if file.path().extension().is_some_and(|e| e == "md") {
                if let Some(text) = file.contents_utf8() {
                    out.push((
                        format!("fleet_skills/{}", file.path().display()),
                        text,
                    ));
                }
            }
        }
        for sub in dir.dirs() {
            walk(sub, out);
        }
    }
    let mut docs: Vec<(String, &'static str)> = crate::fleet_commands::FLEET_COMMANDS
        .iter()
        .map(|(name, text)| (format!("fleet_commands/{name}.md"), *text))
        .collect();
    walk(&crate::fleet_skills::FLEET_SKILLS, &mut docs);
    docs
}

/// Whether the text in front of a `/ui-bridge` match makes it a runner path.
/// `prefix` is the token the match is glued to: the characters back to the
/// last space, tab, quote, backtick, `(`, `[`, `|` or `=`.
fn is_runner_base(prefix: &str) -> bool {
    // A bare path (`/ui-bridge/...` in prose or a table).
    if prefix.is_empty() {
        return true;
    }
    // A URL or a templated base: judge the segment in front of the match.
    let last = prefix.rsplit('/').next().unwrap_or(prefix);
    if last.is_empty() {
        // A prefix ending in `/` (`x/`, or `https:/` from a hostless
        // `https://ui-bridge...`): a path only if nothing alphanumeric
        // precedes it.
        return !prefix.chars().any(|c| c.is_ascii_alphanumeric());
    }
    // A placeholder base: `$BASE`, `${RUNNER}`, `<runner>`, `{base}`.
    if last.starts_with(['$', '<', '{']) || last.ends_with(['>', '}']) {
        return true;
    }
    // A host with a port (`127.0.0.1:9876`, `localhost:<port>`), or a
    // loopback host without one. A bare word (`qontinui`, `api`, `..`) is a
    // repo or web-relay path, not the runner.
    let host = last.split(':').next().unwrap_or(last);
    last.contains(':') && !host.is_empty()
        || matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Strip what is not part of the path: a query, a fragment, and trailing
/// punctuation picked up from prose.
fn clean_path(raw: &str) -> String {
    let end = raw.find(['?', '#']).unwrap_or(raw.len());
    let path = raw.get(..end).unwrap_or(raw);
    path.trim_end_matches(['.', ',', ';', ':', ')', ']']).to_string()
}

/// Every runner `/ui-bridge` literal in `text`.
fn extract_literals(doc: &str, text: &str) -> Vec<DocLiteral> {
    let re = Regex::new(r"/ui-bridge(?:/[A-Za-z0-9_{}<>:.*\-]+)*(?:\?[^\s`'\x22)]*)?")
        .expect("static regex");
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        for m in re.find_iter(line) {
            let before = line.get(..m.start()).unwrap_or("");
            let prefix = before
                .rsplit([' ', '\t', '`', '"', '\'', '(', '[', '|', '='])
                .next()
                .unwrap_or("");
            if !is_runner_base(prefix) {
                continue;
            }
            let path = clean_path(m.as_str());
            // Repo paths that happen to start `/ui-bridge` once a base was
            // stripped (`.../ui-bridge/packages/...`).
            if ["/src/", "/packages/", "/dist/"]
                .iter()
                .any(|p| path.contains(p))
            {
                continue;
            }
            out.push(DocLiteral {
                doc: doc.to_string(),
                line: idx + 1,
                path,
                paragraph: paragraph_around(&lines, idx),
            });
        }
    }
    out
}

/// The blank-line-delimited paragraph containing line `idx`, lowercased.
fn paragraph_around(lines: &[&str], idx: usize) -> String {
    let blank = |i: usize| lines.get(i).is_none_or(|l| l.trim().is_empty());
    let mut start = idx;
    while start > 0 && !blank(start - 1) {
        start -= 1;
    }
    let mut end = idx;
    while end + 1 < lines.len() && !blank(end + 1) {
        end += 1;
    }
    lines
        .get(start..=end)
        .unwrap_or_default()
        .join("\n")
        .to_lowercase()
}

/// Every route path the runner registers, from the source-scanning
/// enumerator `relay_path_policy` and `origin_guard` already share: literal
/// `.route(…)` / `.route_service(…)` paths plus both routes of every
/// `add_dual!(…)`. It reads test code too, so a path only a test router
/// registers counts as served here, and so does a debug-only route (the
/// `/ui-bridge/test/*` fixtures, compiled only under `debug_assertions` or the
/// `test-fixtures` feature) that a release runner answers 404. That is the
/// permissive direction, chosen there for the same reason: a false negative
/// fails a correct doc.
fn served_route_templates() -> Vec<String> {
    let mut out: Vec<String> = crate::mcp::relay_path_policy::tests::registered_routes()
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Seg {
    Lit(String),
    /// `{x}`, `<x>`, `:x` — one segment, any value.
    Param,
    /// `{*x}` in a route — the rest of the path.
    Rest,
}

fn segments(path: &str) -> Vec<Seg> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with("{*") && s.ends_with('}') {
                Seg::Rest
            } else if (s.starts_with('{') && s.ends_with('}'))
                || (s.starts_with('<') && s.ends_with('>'))
                || (s.starts_with(':') && s.len() > 1)
            {
                Seg::Param
            } else {
                Seg::Lit(s.to_string())
            }
        })
        .collect()
}

/// Whether doc literal `doc` names something the route template `route`
/// serves. Without a `*`, the literal must cover the whole route: every
/// segment matches and the two have the same length. A literal ending in `*`
/// names a family by prefix. `/x/*` covers every route under `/x`, and
/// `/x/ab*` covers every route under `/x` whose next segment starts with `ab`.
fn names_served(doc: &str, route: &str) -> bool {
    let (doc, star_prefix) = match doc.strip_suffix('*') {
        // `/x/*`: a whole-segment family.
        Some(d) if d.ends_with('/') => (d.trim_end_matches('/'), Some("")),
        // `/x/ab*`: the last segment is a string prefix.
        Some(d) => match d.rsplit_once('/') {
            Some((head, last)) => (head, Some(last)),
            None => (d, Some("")),
        },
        None => (doc, None),
    };
    let d = segments(doc);
    let r = segments(route);
    for (i, ds) in d.iter().enumerate() {
        let Some(rs) = r.get(i) else {
            return false;
        };
        let ok = match (ds, rs) {
            (Seg::Lit(a), Seg::Lit(b)) => a == b,
            (Seg::Lit(_) | Seg::Param, Seg::Param) => true,
            // A catch-all route never stands in for a named route: one
            // `/ui-bridge/{*rest}` proxy would otherwise pass every literal
            // under it, deleted ones included.
            (_, Seg::Rest) | (Seg::Param, Seg::Lit(_)) | (Seg::Rest, _) => false,
        };
        if !ok {
            return false;
        }
    }
    match star_prefix {
        None => d.len() == r.len(),
        Some(p) => match r.get(d.len()) {
            Some(Seg::Lit(next)) => next.starts_with(p),
            Some(Seg::Param) => p.is_empty(),
            Some(Seg::Rest) | None => false,
        },
    }
}

/// Whether `path` names a route at all, or only a family that the rules let
/// pass without a check.
fn is_checkable(path: &str) -> bool {
    !path.ends_with('*') && segments(path).len() >= 2 && !FAMILY_NAMES.contains(&path)
}

/// The doc literals that satisfy none of the three rules, as report lines.
fn offenders(literals: &[DocLiteral], routes: &[String]) -> Vec<String> {
    literals
        .iter()
        .filter(|lit| {
            let served = routes.iter().any(|r| names_served(&lit.path, r))
                || (FAMILY_NAMES.contains(&lit.path.as_str())
                    && routes.iter().any(|r| r.starts_with(&format!("{}/", lit.path))));
            let reported_removed = removed_route_replacement(lit.path.trim_end_matches('*'))
                .is_some()
                && (lit.paragraph.contains("deleted") || lit.paragraph.contains("removed"));
            let allowed = ALLOW_LIST.iter().any(|(p, _)| *p == lit.path);
            !(served || reported_removed || allowed)
        })
        .map(|lit| {
            let hint = removed_route_replacement(lit.path.trim_end_matches('*'))
                .map(|r| format!(" (removed; use {r})"))
                .unwrap_or_default();
            format!("{}:{}: {}{hint}", lit.doc, lit.line, lit.path)
        })
        .collect()
}

fn all_literals() -> Vec<DocLiteral> {
    bundled_docs()
        .iter()
        .flat_map(|(name, text)| extract_literals(name, text))
        .collect()
}

#[test]
fn bundled_docs_name_only_served_ui_bridge_routes() {
    let literals = all_literals();
    let routes = served_route_templates();
    let checkable = literals.iter().filter(|l| is_checkable(&l.path)).count();
    assert!(
        checkable >= MIN_CHECKABLE_LITERALS,
        "found only {checkable} checkable /ui-bridge literals in the bundled docs \
         (floor {MIN_CHECKABLE_LITERALS}) — the extractor has gone blind, or the \
         bundle lost its UI Bridge docs"
    );
    eprintln!(
        "doc_route_agreement: {} runner /ui-bridge literals in the bundled docs \
         ({checkable} checkable), {} registered route templates",
        literals.len(),
        routes.len()
    );
    let bad = offenders(&literals, &routes);
    assert!(
        bad.is_empty(),
        "bundled docs name UI Bridge routes the runner does not serve. Fix the doc \
         in qontinui-claude-config (the source of this bundle), or, if the doc reports \
         a deletion, say so in the same paragraph:\n  {}",
        bad.join("\n  ")
    );
}

/// The source scan must see at least what `GET /ui-bridge/_routes` serves,
/// or a route the runner serves would read as unserved, and a scan that found
/// nothing would make every check above fail rather than pass. This pins the
/// scan from below.
#[test]
fn scan_covers_the_route_manifest() {
    let routes = served_route_templates();
    let missing: Vec<String> = super::route_manifest()
        .iter()
        .map(|(_, p)| p.to_string())
        .filter(|p| !routes.iter().any(|r| names_served(p, r)))
        .collect();
    assert!(
        missing.is_empty(),
        "route_manifest() serves paths the source scan did not find: {missing:?}"
    );
}

#[test]
fn allow_list_is_live() {
    let literals = all_literals();
    let routes = served_route_templates();
    for (path, reason) in ALLOW_LIST {
        assert!(!reason.trim().is_empty(), "{path}: allow-list entry needs a reason");
        assert!(
            literals.iter().any(|l| l.path == *path),
            "{path}: no bundled doc names it any more — delete the allow-list entry"
        );
        assert!(
            !routes.iter().any(|r| names_served(path, r)),
            "{path}: the runner serves it now — delete the allow-list entry"
        );
    }
}

/// Mutation check: the regression this test exists for. Put the exact line
/// the bundled `/ui-bridge` command carried before qontinui-claude-config#1996
/// back into it, and the check must fail.
#[test]
fn reinserted_annotated_screenshot_call_is_caught() {
    let doc = crate::fleet_commands::FLEET_COMMANDS
        .iter()
        .find(|(name, _)| *name == "ui-bridge")
        .map(|(_, text)| *text)
        .expect("the bundled /ui-bridge command");
    let mutated = format!(
        "{doc}\n\nTake a screenshot:\n\n\
         curl -s \"http://127.0.0.1:9876/ui-bridge/control/annotated-screenshot?runner=true\"\n"
    );
    let routes = served_route_templates();
    let bad = offenders(&extract_literals("ui-bridge.md", &mutated), &routes);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("/ui-bridge/control/annotated-screenshot"));
    assert!(bad[0].contains("vision/capture"), "{bad:?}");
    // The unmutated doc is clean.
    assert!(offenders(&extract_literals("ui-bridge.md", doc), &routes).is_empty());
}

/// A deletion note may name the removed route; a prescription may not.
#[test]
fn removed_route_passes_only_in_a_deletion_paragraph() {
    let routes = served_route_templates();
    let note = "The `/ui-bridge/control/screenshot` route was deleted.\n";
    assert!(offenders(&extract_literals("d", note), &routes).is_empty());
    let call = "Call `POST /ui-bridge/control/screenshot` for a frame.\n";
    assert_eq!(offenders(&extract_literals("d", call), &routes).len(), 1);
}

#[test]
fn extraction_keeps_runner_urls_and_drops_repo_and_relay_paths() {
    let text = "\
curl http://127.0.0.1:9876/ui-bridge/vision/capture -d '{}'\n\
GET http://localhost:<port>/ui-bridge/_routes\n\
curl $RUNNER/ui-bridge/health\n\
see `/ui-bridge/vision/cache/<sha256>`, then GET /ui-bridge/vision/cache/{sha256}.\n\
the repo qontinui/ui-bridge, the relay at https://qontinui.io/api/ui-bridge/control/x\n\
node ../ui-bridge/packages/ui-bridge-wrapper/dist/inject-cli.cjs\n";
    let paths: Vec<String> = extract_literals("t", text).into_iter().map(|l| l.path).collect();
    assert_eq!(
        paths,
        vec![
            "/ui-bridge/vision/capture",
            "/ui-bridge/_routes",
            "/ui-bridge/health",
            "/ui-bridge/vision/cache/<sha256>",
            "/ui-bridge/vision/cache/{sha256}",
        ]
    );
}

#[test]
fn matching_is_segment_wise() {
    let r = "/ui-bridge/invoke/{command_name}";
    assert!(names_served("/ui-bridge/invoke/get_coord_device_token", r));
    assert!(!names_served("/ui-bridge/invoke", r)); // a family: FAMILY_NAMES, not this
    assert!(names_served("/ui-bridge/invoke/{name}", r));
    assert!(!names_served("/ui-bridge/invoke/a/b", r));
    assert!(!names_served("/ui-bridge/invok", r));
    assert!(names_served("/ui-bridge/vision/*", "/ui-bridge/vision/capture"));
    assert!(names_served("/ui-bridge/vision/cap*", "/ui-bridge/vision/capture"));
    assert!(!names_served("/ui-bridge/vision/cache*", "/ui-bridge/vision/capture"));
    assert!(!names_served("/ui-bridge/control/screenshot", "/ui-bridge/control/screenshots"));
    // A truncated or partial literal is not a route, and the route is.
    assert!(!names_served("/ui-bridge/vision", "/ui-bridge/vision/capture"));
    // A catch-all route never stands in for a named one.
    assert!(!names_served("/ui-bridge/sdk/a/b/c", "/ui-bridge/sdk/{*rest}"));
    assert!(!names_served("/ui-bridge/sdk/ai/media/*", "/ui-bridge/sdk/{*rest}"));
}
