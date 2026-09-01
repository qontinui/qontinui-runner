//! `qontinui-pr` — the session CLI delivered onto every runner-hosted
//! terminal's PATH by the identity-shim materializer
//! (`install_effects_producer::intercept::shim_materializer::materialize_identity`).
//!
//! Named `qontinui-pr` (NOT `qontinui`): the identity shim dir is PREPENDED to
//! PATH in every runner terminal, so a bin named `qontinui` would shadow the
//! Python qontinui library's `qontinui` console script.
//!
//! ## `qontinui-pr create`
//! Opens a pull request WITHOUT a personal GitHub login on the machine: it
//! POSTs the runner's loopback `POST /vcs/pull-requests` proxy, which injects
//! the session's live JWT and forwards to coord's brokered PR-creation
//! route (`POST {coord}/coord/repos/{owner}/{repo}/pull-requests`). Coord's
//! verdict (201 / 403 / 404 / 429) surfaces verbatim.
//!
//! ## Runner discovery (port + loopback auth)
//! The loopback route requires the per-session coord-mcp proxy nonce
//! (`Authorization: Bearer <nonce>`, or the legacy `X-Coord-Mcp-Proxy-Key` —
//! both are read here and the runner accepts either), discovered by a
//! **`.mcp.json` walk-up from cwd**.
//! The runner provisions every session workdir with a `.mcp.json` whose
//! coord-mcp server entry carries BOTH the nonce header and a loopback URL on
//! the ACTUALLY-BOUND API port (`coord_mcp::write_coord_mcp_proxy_config` /
//! `write_coord_mcp_agent_proxy_config`; the reconciler rewrites it on port
//! drift). The nonce and the port are read from the SAME entry, so the POST
//! always lands on the runner that issued the nonce — there is deliberately NO
//! port probing/scanning fallback (a scan can bind the nonce to a DIFFERENT
//! runner, which then 401s it).
//!
//! Borrowing an ANCESTOR directory's `.mcp.json` is intentional: nested
//! worktrees/subdirs inside a provisioned session workdir inherit the session's
//! credential, and because port + nonce travel together the borrowed entry
//! still pairs the nonce with its issuing runner's port.
//!
//! `QONTINUI_RUNNER_API_PORT` is honored as an EXPLICIT operator override of
//! the port only (the nonce still comes from `.mcp.json`).
//!
//! Style: matches the sibling standalone bins (`qontinui_git_credential`) —
//! hand-rolled arg parsing, no CLI crates; `reqwest::blocking` for HTTP (the
//! package dependency already carries the `blocking` feature).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The `.mcp.json` proxy-header contract — header names AND the resolver that
/// reads a nonce back out of a `headers` object — IMPORTED from the shared
/// module rather than re-implemented.
///
/// An earlier revision carried its own `PROXY_KEY_HEADER` /
/// `AUTHORIZATION_HEADER` / `BEARER_PREFIX` / `looks_like_jwt` and a hand-rolled
/// preference order, on the stated premise that "this bin is a separate crate
/// root and cannot reach `coord_mcp::COORD_MCP_PROXY_KEY_HEADER`". The premise
/// was false: `coord_mcp_config` is `pub mod` in `lib.rs` (it was extracted
/// there precisely because `coord_doctor` needed it too), and a bin can depend
/// on its own lib crate — this file already imports
/// `qontinui_runner_lib::plan_workunit_adapter` outside `mod tests`. "Kept in
/// sync by shape, not by import" is the five-silent-readers failure mode that
/// module was created to eliminate.
use qontinui_runner_lib::coord_mcp_config::{
    proxy_nonce_from_header_object, COORD_MCP_PROXY_KEY_HEADER_JSON,
};

const RUNNER_PORT_ENV: &str = "QONTINUI_RUNNER_API_PORT";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("create") => pr_create(&args[1..]),
        Some("plan-library-backfill") => plan_library_backfill(&args[1..]),
        Some("plan-workunit-backfill") => plan_workunit_backfill(&args[1..]),
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("qontinui-pr: unknown command {other:?}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

const USAGE: &str = "\
qontinui-pr — Qontinui Runner session CLI

USAGE:
  qontinui-pr create --title <title> [options]
  qontinui-pr plan-library-backfill [options]
  qontinui-pr plan-workunit-backfill [options]

OPTIONS (create):
  --repo <owner/name>   Target repo (default: inferred from `git remote get-url origin`)
  --head <branch>       Head branch (default: `git symbolic-ref --short HEAD`)
  --base <branch>       Base branch (default: main)
  --title <text>        PR title (required; `--title -` reads the first line of stdin)
  --body <text>         PR body
  --body-file <path>    Read the PR body from a file
  --draft               Open as a draft PR

OPTIONS (plan-library-backfill):
  --dry-run             Scan and report only — no network call whatsoever
  --plans-dir <path>    Active plans dir      (default: $QONTINUI_PLANS_DIR)
  --prompts-dir <path>  Prompts dir           (default: $QONTINUI_PROMPTS_DIR)
  --backend <url>       qontinui-web base URL. OVERRIDES the environment
                        ($QONTINUI_WEB_BACKEND_URL, then $QONTINUI_API_URL),
                        which is what a runner terminal exports by default.
  --limit <n>           Push at most N artifacts (ordering is the scan order)

OPTIONS (plan-workunit-backfill):
  --dry-run             Scan and report only — no network call whatsoever
  --plans-dir <path>    Active plans dir (default: $QONTINUI_PLAN_ADAPTER_DIR,
                        then $QONTINUI_PLANS_DIR — the adapter variable first,
                        because it is the one that would have armed the loop).
                        The runner's `paths.plans_dir` setting is NEVER read.
                        The run prints which source won.
  --coord <url>         coord base URL. OVERRIDES the environment
                        ($COORD_HTTP_URL) and the active runner profile.
  --limit <n>           Push at most N work units (ordering is the scan order)

Values that themselves begin with `--` must use the `--flag=value` form.

`create` opens the PR through the runner's coord-brokered loopback proxy — no
personal `gh auth login` required. On success prints the PR URL to stdout.

`plan-library-backfill` walks the three scan roots, classifies each markdown
file to an artifact kind, and upserts it into the qontinui-web plan & prompt
library with the runner's own device JWT. `--dry-run` prints the per-kind counts
and the duplicated/divergent stem list without contacting anything.

`plan-workunit-backfill` is its WORK-UNIT half: it parses the active plans dir
and upserts each plan into `coord.work_units`. It deliberately bypasses the
runner's `paths.plans_dir` gate — that gate is exactly what it routes around, so
a machine whose reconcile loop never armed can be caught up WITHOUT a runner
restart. Idempotent: each unit's push is seeded from coord's current status, so
an unchanged corpus emits no status write, and a changed one still goes through
the agent-owner deferral. `--dry-run` contacts nothing, so it shows the FILE
side only — it cannot tell you which units would transition.";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PrCreateArgs {
    repo: Option<String>,
    head: Option<String>,
    base: Option<String>,
    title: Option<String>,
    body: Option<String>,
    body_file: Option<String>,
    draft: bool,
}

fn parse_pr_create_args(args: &[String]) -> Result<PrCreateArgs, String> {
    let mut out = PrCreateArgs::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // `--flag=value` form: split once on the first `=`.
        let (flag, inline) = match arg.starts_with("--") {
            true => match arg.split_once('=') {
                Some((f, v)) => (f, Some(v)),
                None => (arg, None),
            },
            false => (arg, None),
        };
        // Take the flag's value: the inline `=value` if given, else the next
        // argv element — but a next element that LOOKS like a flag is an error
        // (`--title --draft` must not yield a PR titled "--draft"); use
        // `--flag=value` for values that legitimately begin with `--`.
        let mut consumed = 1usize;
        let mut take_value = |slot: &mut Option<String>| -> Result<(), String> {
            if let Some(v) = inline {
                *slot = Some(v.to_string());
                return Ok(());
            }
            match args.get(i + 1) {
                Some(v) if v.starts_with("--") => Err(format!(
                    "{flag} requires a value but got the flag-like {v:?} — \
                     use {flag}=<value> if the value really starts with --"
                )),
                Some(v) => {
                    *slot = Some(v.clone());
                    consumed = 2;
                    Ok(())
                }
                None => Err(format!("{flag} requires a value")),
            }
        };
        match flag {
            "--repo" => take_value(&mut out.repo)?,
            "--head" => take_value(&mut out.head)?,
            "--base" => take_value(&mut out.base)?,
            "--title" => take_value(&mut out.title)?,
            "--body" => take_value(&mut out.body)?,
            "--body-file" => take_value(&mut out.body_file)?,
            "--draft" => {
                if inline.is_some() {
                    return Err("--draft does not take a value".to_string());
                }
                out.draft = true;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
        i += consumed;
    }
    Ok(out)
}

fn pr_create(args: &[String]) -> ExitCode {
    let parsed = match parse_pr_create_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qontinui-pr: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    // --title (required; `-` reads the first line of stdin).
    let title = match parsed.title.as_deref() {
        Some("-") => match first_stdin_line() {
            Some(t) if !t.trim().is_empty() => t,
            _ => {
                eprintln!("qontinui-pr: --title - given but stdin had no title line");
                return ExitCode::from(2);
            }
        },
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => {
            eprintln!("qontinui-pr: --title is required (`--title -` reads it from stdin)");
            return ExitCode::from(2);
        }
    };

    // --body / --body-file (mutually additive: --body-file wins if both given).
    let body = match &parsed.body_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("qontinui-pr: read --body-file {path}: {e}");
                return ExitCode::from(2);
            }
        },
        None => parsed.body.clone(),
    };

    // --repo default: infer from `git remote get-url origin` in cwd.
    let repo = match parsed.repo.clone().or_else(|| {
        git_stdout(&["remote", "get-url", "origin"]).and_then(|u| repo_from_remote_url(&u))
    }) {
        Some(r) => r,
        None => {
            eprintln!(
                "qontinui-pr: could not infer the repo from `git remote get-url origin` — \
                 pass --repo owner/name"
            );
            return ExitCode::from(2);
        }
    };

    // --head default: the current branch.
    let head = match parsed
        .head
        .clone()
        .or_else(|| git_stdout(&["symbolic-ref", "--short", "HEAD"]))
    {
        Some(h) if !h.trim().is_empty() => h.trim().to_string(),
        _ => {
            eprintln!(
                "qontinui-pr: could not resolve the current branch (detached HEAD?) — pass --head"
            );
            return ExitCode::from(2);
        }
    };

    let base = parsed.base.clone().unwrap_or_else(|| "main".to_string());

    // Session-credential discovery (see the module comment): the nonce AND the
    // port come from the SAME `.mcp.json` entry, so the POST lands on the
    // runner that issued the nonce.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = match find_session_mcp_config(&cwd) {
        Some(s) => s,
        None => {
            eprintln!(
                "qontinui-pr: no runner session credential found — no `.mcp.json` with a \
                 loopback coord-mcp nonce entry between {} and the filesystem root. \
                 This session was not provisioned by the runner, or provisioning is \
                 degraded (check for a `.coord-mcp-status` breadcrumb in the session \
                 workdir). Fallback: `gh pr create` works where a personal \
                 `gh auth login` exists.",
                cwd.display()
            );
            return ExitCode::from(1);
        }
    };
    let port = match resolve_port(&session, std::env::var(RUNNER_PORT_ENV).ok().as_deref()) {
        Some(p) => p,
        None => {
            eprintln!(
                "qontinui-pr: the session `.mcp.json` coord-mcp URL carries no loopback \
                 port and ${RUNNER_PORT_ENV} is not set — cannot pair the nonce with \
                 its issuing runner. Fallback: `gh pr create` works where a personal \
                 `gh auth login` exists."
            );
            return ExitCode::from(1);
        }
    };

    let mut payload = serde_json::json!({
        "repo": repo,
        "head": head,
        "base": base,
        "title": title,
    });
    if let Some(b) = body {
        payload["body"] = serde_json::json!(b);
    }
    if parsed.draft {
        payload["draft"] = serde_json::json!(true);
    }

    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qontinui-pr: build http client: {e}");
            return ExitCode::from(1);
        }
    };
    let url = format!("http://127.0.0.1:{port}/vcs/pull-requests");
    // coord-auth-exempt(not-coord): 127.0.0.1 loopback to this runner's own
    // `/vcs/pull-requests`, authenticated by the session proxy nonce.
    let resp = match client
        .post(&url)
        .header(COORD_MCP_PROXY_KEY_HEADER_JSON, &session.nonce)
        .json(&payload)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("qontinui-pr: POST {url}: {e}");
            return ExitCode::from(1);
        }
    };

    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if status.is_success() {
        // On success print the PR URL (and nothing else) to stdout.
        let pr_url = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(String::from));
        match pr_url {
            Some(u) => println!("{u}"),
            None => println!("{}", text.trim()),
        }
        ExitCode::SUCCESS
    } else {
        // Coord's (or the runner proxy's) error body, verbatim, to stderr.
        eprintln!("qontinui-pr: create failed ({status}): {}", text.trim());
        ExitCode::from(1)
    }
}

// ===========================================================================
// `qontinui-pr plan-library-backfill`
//
// Plan `2026-08-10-plan-and-prompt-library-in-web` Phase 2: the one-shot
// backfill, so the ~1,100-file corpus lands without waiting for a reconcile
// tick.
//
// It reads the two scan roots from flags, falling back to the environment the
// runner already exports into every session (`QONTINUI_PLANS_DIR`,
// `QONTINUI_PROMPTS_DIR`) — so inside a runner-provisioned terminal the bare
// command already knows where to look.
// ===========================================================================

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BackfillArgs {
    dry_run: bool,
    plans_dir: Option<String>,
    prompts_dir: Option<String>,
    backend: Option<String>,
    limit: Option<usize>,
}

fn parse_backfill_args(args: &[String]) -> Result<BackfillArgs, String> {
    let mut out = BackfillArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if arg.starts_with("--") => (f, Some(v)),
            _ => (arg, None),
        };
        let mut consumed = 1usize;
        // Same value-taking discipline as `create`: a next element that looks
        // like a flag is an error, so `--plans-dir --dry-run` cannot silently
        // scan a directory named "--dry-run".
        let mut value = |consumed: &mut usize| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v.to_string());
            }
            match args.get(i + 1) {
                Some(v) if v.starts_with("--") => Err(format!(
                    "{flag} requires a value but got the flag-like {v:?} — \
                     use {flag}=<value> if the value really starts with --"
                )),
                Some(v) => {
                    *consumed = 2;
                    Ok(v.clone())
                }
                None => Err(format!("{flag} requires a value")),
            }
        };
        match flag {
            "--dry-run" => out.dry_run = true,
            "--plans-dir" => out.plans_dir = Some(value(&mut consumed)?),
            "--prompts-dir" => out.prompts_dir = Some(value(&mut consumed)?),
            "--backend" => out.backend = Some(value(&mut consumed)?),
            "--limit" => {
                let raw = value(&mut consumed)?;
                out.limit = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--limit expects a number, got {raw:?}"))?,
                );
            }
            other => return Err(format!("unknown option {other:?}")),
        }
        i += consumed;
    }
    Ok(out)
}

/// A non-empty env var, or `None`. A blank value is *unset* everywhere in this
/// tree; keep that here so `QONTINUI_PROMPTS_DIR=""` disables the prompt scan
/// rather than scanning a directory named `""`.
fn env_dir(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

fn plan_library_backfill(args: &[String]) -> ExitCode {
    use qontinui_runner_lib::plan_workunit_adapter::body_push as bp;
    use qontinui_runner_lib::plan_workunit_adapter::PlanConvention;

    let parsed = match parse_backfill_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qontinui-pr: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let roots = bp::scan_roots(
        parsed.plans_dir.or_else(|| env_dir("QONTINUI_PLANS_DIR")),
        parsed
            .prompts_dir
            .or_else(|| env_dir("QONTINUI_PROMPTS_DIR")),
    );
    if roots.is_empty() {
        eprintln!(
            "qontinui-pr: no scan root configured. Pass --plans-dir/--prompts-dir, \
             or run inside a runner-provisioned terminal where $QONTINUI_PLANS_DIR and friends \
             are exported."
        );
        return ExitCode::from(2);
    }

    // The push path reports per-artifact failures through `tracing::warn!`, and
    // a bin with no subscriber SWALLOWS them — the operator would see
    // `errors=1` with no reason, which is the exact silent-failure shape this
    // tree argues against everywhere else. Install a minimal stderr subscriber
    // so the diagnosis (a 401 naming the dependency mismatch, a 422 naming the
    // field) actually reaches the terminal. `RUST_LOG` still wins if set.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let conv = PlanConvention::operator_default();
    let mut per_root: Vec<(String, usize)> = Vec::new();
    let mut all: Vec<bp::ScannedArtifact> = Vec::new();
    let mut skipped: Vec<bp::SkippedFile> = Vec::new();
    for root in &roots {
        let found = bp::scan_one_root(root, &conv, &mut skipped);
        let label = format!(
            "{} [{}]",
            root.label,
            root.source_repo.as_deref().unwrap_or("no repo")
        );
        per_root.push((label, found.len()));
        all.extend(found);
    }

    let report = bp::build_report(&all, skipped, per_root);
    println!("{}", bp::render_report(&report));

    if parsed.dry_run {
        println!("dry run: nothing was pushed.");
        return ExitCode::SUCCESS;
    }

    // Flag-first: an explicit `--backend` OUTRANKS the ambient environment.
    // The env vars are exported into every runner-provisioned terminal, so
    // threading the flag into the lowest-precedence slot (the shipped bug)
    // meant `--backend http://127.0.0.1:8000` was silently ignored exactly
    // where an operator would type it — sending the whole corpus wherever
    // $QONTINUI_API_URL happened to point, plausibly production.
    let base = match bp::backend_base_from_flag_or_env(parsed.backend) {
        Some(b) => b,
        None => {
            eprintln!(
                "qontinui-pr: no qontinui-web backend configured — pass --backend <url> or set \
                 $QONTINUI_WEB_BACKEND_URL. Refusing to guess a host for a {}-artifact push.",
                report.scanned
            );
            return ExitCode::from(2);
        }
    };
    let sink = bp::HttpArtifactSink::new(&base);
    let to_push: &[bp::ScannedArtifact] = match parsed.limit {
        Some(n) if n < all.len() => &all[..n],
        _ => &all,
    };
    println!("pushing {} artifact(s) to {base} …", to_push.len());

    // The push path is async (it shares `reqwest`'s async client with the
    // runner's own adapter); this bin has no ambient runtime, so give it a
    // single-threaded one rather than duplicating the client as blocking.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("qontinui-pr: build tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let mut state = bp::ArtifactSyncState::new();
    let summary = runtime.block_on(bp::backfill_once(&sink, to_push, &mut state));

    println!(
        "created={} updated={} unchanged={} skipped={} kind_forks={} errors={}",
        summary.created,
        summary.updated,
        summary.unchanged_remote,
        summary.skipped_local,
        summary.ambiguous_kind,
        summary.errors
    );
    println!(
        "edges: set={} unresolved={} errors={} gave_up={}",
        summary.edges_set, summary.edges_unresolved, summary.edge_errors, summary.edges_given_up
    );
    if summary.ambiguous_kind > 0 {
        println!(
            "note: {} artifact(s) hit a kind fork — several rows share (slug, source_repo) with \
             no locked kind. Nothing was written for those; resolve with \
             PATCH /api/v1/plan-library/<id>/kind and re-run.",
            summary.ambiguous_kind
        );
    }
    if summary.errors > 0 {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

// ===========================================================================
// `qontinui-pr plan-workunit-backfill` — the WORK-UNIT half of the backfill
// pair, and the catch-up path the fleet did not have.
//
// `plan-library-backfill` (above) pushes plan BODIES to qontinui-web's
// `agent.work_artifacts`. Nothing pushed plan WORK UNITS to `coord.work_units`
// except `plan_workunit_adapter::trigger`'s periodic loop — which
// `spawn_if_configured` arms only when a plans dir resolves from
// `paths.plans_dir` or `QONTINUI_PLAN_ADAPTER_DIR`, once, at runner boot, with
// no re-arm on a settings change. A machine that never had either setting
// therefore never ingested a single plan, and the only remedy was to configure
// it and wait for a future runner start (a running runner must never be
// restarted — served policy `production-and-cost` `runner-lifecycle`).
//
// This subcommand bypasses that gate on purpose: it takes the plans dir as an
// argument and drives the SAME `push_work_unit` path the loop uses, so a
// non-participating machine can be caught up now, from a terminal, with no
// runner lifecycle event at all.
// ===========================================================================

/// Per-machine override for the adapter's plans dir. Read as the FIRST default
/// after the flag — ahead of `$QONTINUI_PLANS_DIR` — so this subcommand scans
/// the directory that would have armed the reconcile loop. Note the sibling
/// `plan-library-backfill` reads `$QONTINUI_PLANS_DIR` and this variable not at
/// all, so on a box exporting both the two subcommands can scan different
/// directories; that is why every run here prints which source won.
const PLAN_ADAPTER_DIR_ENV: &str = qontinui_runner_lib::plan_workunit_adapter::PLAN_ADAPTER_DIR_ENV;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct WorkUnitBackfillArgs {
    dry_run: bool,
    plans_dir: Option<String>,
    coord: Option<String>,
    limit: Option<usize>,
}

fn parse_workunit_backfill_args(args: &[String]) -> Result<WorkUnitBackfillArgs, String> {
    let mut out = WorkUnitBackfillArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if arg.starts_with("--") => (f, Some(v)),
            _ => (arg, None),
        };
        let mut consumed = 1usize;
        // Same value-taking discipline as the two siblings: a next element that
        // looks like a flag is an error, never a value.
        let mut value = |consumed: &mut usize| -> Result<String, String> {
            if let Some(v) = inline {
                return Ok(v.to_string());
            }
            match args.get(i + 1) {
                Some(v) if v.starts_with("--") => Err(format!(
                    "{flag} requires a value but got the flag-like {v:?} — \
                     use {flag}=<value> if the value really starts with --"
                )),
                Some(v) => {
                    *consumed = 2;
                    Ok(v.clone())
                }
                None => Err(format!("{flag} requires a value")),
            }
        };
        match flag {
            "--dry-run" => out.dry_run = true,
            "--plans-dir" => out.plans_dir = Some(value(&mut consumed)?),
            "--coord" => out.coord = Some(value(&mut consumed)?),
            "--limit" => {
                let raw = value(&mut consumed)?;
                out.limit = Some(
                    raw.parse::<usize>()
                        .map_err(|_| format!("--limit expects a number, got {raw:?}"))?,
                );
            }
            other => return Err(format!("unknown option {other:?}")),
        }
        i += consumed;
    }
    Ok(out)
}

/// Resolve the plans dir for the backfill from three ALREADY-READ candidates,
/// returning the value AND which slot won. Pure, so the precedence is testable
/// without touching the process environment.
///
/// Order: the flag, then `$QONTINUI_PLAN_ADAPTER_DIR`, then
/// `$QONTINUI_PLANS_DIR`. The adapter variable sits ABOVE the sibling
/// backfill's on purpose — it is the one that would have armed the reconcile
/// loop (`plan_workunit_adapter::trigger::resolve_plans_dir_with_source`), and
/// a catch-up that ingested a *different* corpus than the loop would have is
/// worse than no catch-up. On a box exporting both, the caller prints which one
/// won rather than leaving the operator to guess.
///
/// Deliberately does NOT consult the runner's `paths.plans_dir` setting — that
/// is precisely the gate this command exists to route around, and reading it
/// would make the command a no-op on exactly the machines that need it.
fn resolve_backfill_plans_dir_from(
    flag: Option<String>,
    adapter_env: Option<String>,
    plans_env: Option<String>,
) -> Option<(String, &'static str)> {
    let nonblank = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    if let Some(v) = nonblank(flag) {
        return Some((v, "--plans-dir"));
    }
    if let Some(v) = nonblank(adapter_env) {
        return Some((v, PLAN_ADAPTER_DIR_ENV));
    }
    nonblank(plans_env).map(|v| (v, "QONTINUI_PLANS_DIR"))
}

/// [`resolve_backfill_plans_dir_from`] reading this process's environment.
fn resolve_backfill_plans_dir(flag: Option<String>) -> Option<(String, &'static str)> {
    resolve_backfill_plans_dir_from(
        flag,
        env_dir(PLAN_ADAPTER_DIR_ENV),
        env_dir("QONTINUI_PLANS_DIR"),
    )
}

/// The coord base for the backfill from already-read candidates, with its
/// provenance. Flag-first for the same reason `plan-library-backfill` is: the
/// env is ambient in every runner terminal, so threading an explicit `--coord`
/// into the lowest slot would silently ignore it exactly where an operator would
/// type it.
fn resolve_backfill_coord_base_from(
    flag: Option<String>,
    env: Option<String>,
    profile: Option<String>,
) -> Option<(String, &'static str)> {
    let nonblank = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    if let Some(v) = nonblank(flag) {
        return Some((v, "--coord"));
    }
    if let Some(v) = nonblank(env) {
        return Some((v, "COORD_HTTP_URL"));
    }
    nonblank(profile).map(|v| (v, "the runner's connected profile"))
}

/// [`resolve_backfill_coord_base_from`] reading this process's environment and
/// the runner's active profile.
fn resolve_backfill_coord_base(flag: Option<String>) -> Option<(String, &'static str)> {
    resolve_backfill_coord_base_from(
        flag,
        env_dir("COORD_HTTP_URL"),
        qontinui_runner_lib::profiles::connected_coord_base(),
    )
}

/// `--limit 0` parses fine and then pushes nothing while looking like a real
/// run. Refuse it, and do so in a pure function so the rule is testable.
fn reject_zero_limit(args: &WorkUnitBackfillArgs) -> Result<(), String> {
    if args.limit == Some(0) {
        return Err(
            "--limit 0 would push nothing. Use --dry-run to inspect the scan without \
             contacting coord."
                .to_string(),
        );
    }
    Ok(())
}

fn plan_workunit_backfill(args: &[String]) -> ExitCode {
    use qontinui_runner_lib::plan_workunit_adapter as pwa;
    // `current_status` is a trait method — the preflight probe below calls it
    // directly on the concrete sink.
    use qontinui_runner_lib::plan_workunit_adapter::WorkUnitSink as _;

    let parsed = match parse_workunit_backfill_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qontinui-pr: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = reject_zero_limit(&parsed) {
        eprintln!("qontinui-pr: {e}");
        return ExitCode::from(2);
    }

    let Some((plans_dir, dir_source)) = resolve_backfill_plans_dir(parsed.plans_dir) else {
        eprintln!(
            "qontinui-pr: no plans dir configured. Pass --plans-dir <path>, or export \
             ${PLAN_ADAPTER_DIR_ENV} / $QONTINUI_PLANS_DIR. (This command deliberately ignores \
             the runner's `paths.plans_dir` setting — routing around it is the whole point.)"
        );
        return ExitCode::from(2);
    };

    // Same reason the sibling installs one: `push_work_unit` reports every
    // failure through `tracing::warn!`, and a bin with no subscriber swallows
    // them — the operator would see `failed=N` with no reason.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let conv = pwa::PlanConvention::operator_default();
    let all = pwa::read_plan_dir(Path::new(&plans_dir), &conv);
    // Name the SOURCE, not just the path: on a box exporting both variables the
    // corpus this ingests and the corpus the reconcile loop would have ingested
    // can differ, and "which dir won" is not derivable from the path alone.
    println!(
        "found {} plan file(s) in {plans_dir} (from {dir_source})",
        all.len()
    );
    if all.is_empty() {
        // An empty scan is UNKNOWN, not "nothing to do": a mistyped path and an
        // already-ingested corpus look identical from here. Say which one this
        // is not.
        eprintln!(
            "qontinui-pr: {plans_dir} yielded no *.md plan files — check the path (the scan is \
             non-recursive, matching the reconcile loop). Nothing was pushed."
        );
        return ExitCode::from(2);
    }

    let to_push: &[pwa::ParsedWorkUnit] = match parsed.limit {
        Some(n) if n < all.len() => &all[..n],
        _ => &all,
    };

    if parsed.dry_run {
        for u in to_push {
            println!("  {} status={} title={:?}", u.slug, u.status, u.title);
        }
        // Be explicit about what a dry run CANNOT tell you. It contacts nothing,
        // so it shows the file side only — which unit would be created, which
        // refreshed and which TRANSITIONED (the arm that overwrites coord)
        // depends entirely on each unit's current remote status.
        println!(
            "dry run: nothing was pushed, and nothing was read — this lists the FILE side only. \
             Which of these would be created / refreshed / transitioned depends on each unit's \
             current status in coord, which a dry run does not fetch."
        );
        return ExitCode::SUCCESS;
    }

    let Some((base, base_source)) = resolve_backfill_coord_base(parsed.coord) else {
        eprintln!(
            "qontinui-pr: no coord base configured — pass --coord <url> or set $COORD_HTTP_URL. \
             Refusing to guess a host for a {}-unit push.",
            to_push.len()
        );
        return ExitCode::from(2);
    };
    let sink = pwa::HttpWorkUnitSink::new(&base);

    // `push_work_unit` is async (it shares the runner's `reqwest` async client);
    // this bin has no ambient runtime.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("qontinui-pr: build tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    // Preflight (see the E4 note on `HttpWorkUnitSink::current_status`): the
    // whole backfill is seeded by `GET /coord/work-units`, an operator-tier
    // route the tree records as 403-ing a device JWT. Without this probe a
    // credential that cannot read it produces one identical warn per plan —
    // ~1,400 lines saying nothing, `failed=1400`, and no diagnosis. Probe ONCE
    // against the first unit and refuse the run with the reason, so the operator
    // learns what to fix instead of scrolling. A probe that SUCCEEDS costs one
    // request; the loop re-reads that unit anyway.
    // `first()` rather than `[0]`: the emptiness guards above already make this
    // non-empty, but an index whose safety depends on a check forty lines away
    // is a panic waiting for the next edit.
    let probe_slug = match to_push.first() {
        Some(u) => u.slug.clone(),
        None => {
            eprintln!("qontinui-pr: nothing to push after applying --limit.");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = runtime.block_on(sink.current_status(&probe_slug)) {
        eprintln!(
            "qontinui-pr: preflight read of {base} failed, so NOTHING was pushed: {e:#}\n\
             The backfill seeds every unit from its current coord status; without that read it \
             cannot tell an absent unit from an unreadable one, and writing anyway could \
             overwrite a status an agent set. (`GET /coord/work-units` is operator-tier — a \
             device JWT is documented to 403 on it.)"
        );
        return ExitCode::from(1);
    }
    println!(
        "pushing {} work unit(s) to {base} (from {base_source}) …",
        to_push.len()
    );

    let summary = runtime.block_on(pwa::backfill_work_units_once(to_push, &sink));

    println!(
        "scanned={} created={} refreshed={} transitioned={} deferred={} failed={}",
        summary.scanned,
        summary.created,
        summary.refreshed,
        summary.transitioned,
        summary.deferred,
        summary.failed
    );
    if summary.deferred > 0 {
        println!(
            "note: {} unit(s) deferred — a real agent last drove them, so the markdown proxy \
             left their status alone (graduation-bootstrap P2a). Not an error:",
            summary.deferred
        );
        for d in &summary.deferred_units {
            println!("  {} owner={} file wanted={}", d.slug, d.owner, d.wanted);
        }
    }
    if summary.failed > 0 {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// First line of stdin (for `--title -`).
fn first_stdin_line() -> Option<String> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok()?;
    let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Run `git <args>` in cwd and return trimmed stdout on success.
fn git_stdout(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Extract `owner/name` from a git remote URL. Handles the common GitHub
/// forms: `https://github.com/owner/name(.git)`, `git@github.com:owner/name(.git)`,
/// and `ssh://git@github.com/owner/name(.git)`.
fn repo_from_remote_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let tail = if let Some(rest) = url.strip_prefix("git@") {
        // git@host:owner/name
        rest.split_once(':')?.1
    } else if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        // [git@]host/owner/name
        rest.split_once('/')?.1
    } else {
        return None;
    };
    let tail = tail.trim_end_matches(".git");
    let (owner_path, name) = tail.rsplit_once('/')?;
    // The owner is the LAST path segment before the repo name (drops any
    // leading path noise on unusual remotes).
    let owner = owner_path.rsplit('/').next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// A discovered session credential: the coord-mcp proxy nonce plus the bound
/// runner port parsed from the loopback URL (when present).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMcpConfig {
    nonce: String,
    port: Option<u16>,
}

/// Walk up from `start` looking for a `.mcp.json` with a coord-mcp proxy
/// entry. First hit wins (the session workdir is the nearest ancestor).
/// Borrowing an ancestor's config is intentional — nested worktrees/subdirs
/// inherit the enclosing session's credential, and the entry pairs the nonce
/// with its issuing runner's port (see the module comment).
fn find_session_mcp_config(start: &Path) -> Option<SessionMcpConfig> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(".mcp.json");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            if let Some(cfg) = parse_mcp_json(&text) {
                return Some(cfg);
            }
        }
        dir = d.parent();
    }
    None
}

/// Parse a `.mcp.json` payload: find a server entry whose URL points at a
/// loopback `/coord-mcp` proxy and read its proxy nonce — from
/// `Authorization: Bearer <nonce>` (preferred; the Phase 2 shape) or the legacy
/// `X-Coord-Mcp-Proxy-Key` header, both matched case-insensitively — plus the
/// port embedded in the URL. Entries with NEITHER (e.g. a static-bearer config,
/// whose `Authorization` holds a JWT) are SKIPPED per-entry — a non-nonce
/// `/coord-mcp` entry must not mask a later valid one.
///
/// The nonce extraction itself is `coord_mcp_config::proxy_nonce_from_header_object`
/// — the SAME function the runner's own config readers and `coord doctor` use,
/// so the accepted shapes, the preference order and the nonce-vs-JWT
/// discriminator cannot drift between the walk-up and the door it presents to.
fn parse_mcp_json(text: &str) -> Option<SessionMcpConfig> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let servers = v.get("mcpServers")?.as_object()?;
    for server in servers.values() {
        let url = server.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if !url.contains("/coord-mcp") {
            continue;
        }
        let Some(nonce) = server
            .get("headers")
            .and_then(proxy_nonce_from_header_object)
        else {
            continue;
        };
        return Some(SessionMcpConfig {
            nonce,
            port: port_from_url(url),
        });
    }
    None
}

/// Port from a `http://host:port/...` URL.
fn port_from_url(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = rest.split('/').next()?;
    host_port.rsplit_once(':')?.1.parse().ok()
}

/// Resolve the runner port: the explicit `QONTINUI_RUNNER_API_PORT` override
/// when set (operator escape hatch), else the port from the SAME `.mcp.json`
/// entry that carried the nonce. NO probing/scan fallback — a scanned port can
/// belong to a different runner than the nonce's issuer, which then 401s it.
fn resolve_port(session: &SessionMcpConfig, env_override: Option<&str>) -> Option<u16> {
    if let Some(p) = env_override.and_then(|v| v.trim().parse::<u16>().ok()) {
        return Some(p);
    }
    session.port
}

#[cfg(test)]
mod backfill_tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_full_backfill_flag_set() {
        let parsed = parse_backfill_args(&argv(&[
            "--dry-run",
            "--plans-dir",
            "D:/qontinui-root/plans",
            "--prompts-dir",
            "D:/qontinui-root/prompts",
            "--backend",
            "http://127.0.0.1:8000",
            "--limit",
            "25",
        ]))
        .expect("must parse");
        assert!(parsed.dry_run);
        assert_eq!(parsed.plans_dir.as_deref(), Some("D:/qontinui-root/plans"));
        assert_eq!(
            parsed.prompts_dir.as_deref(),
            Some("D:/qontinui-root/prompts")
        );
        assert_eq!(parsed.backend.as_deref(), Some("http://127.0.0.1:8000"));
        assert_eq!(parsed.limit, Some(25));
    }

    #[test]
    fn no_flags_is_a_valid_env_driven_invocation() {
        assert_eq!(parse_backfill_args(&[]).unwrap(), BackfillArgs::default());
    }

    /// A flag-shaped value is an error, not a directory named `--dry-run`.
    #[test]
    fn a_flag_like_value_is_rejected() {
        let err = parse_backfill_args(&argv(&["--plans-dir", "--dry-run"])).unwrap_err();
        assert!(err.contains("--plans-dir"), "got {err}");
        assert!(err.contains("flag-like"), "got {err}");
        // …unless spelled with `=`.
        assert_eq!(
            parse_backfill_args(&argv(&["--plans-dir=--weird"]))
                .unwrap()
                .plans_dir
                .as_deref(),
            Some("--weird")
        );
    }

    #[test]
    fn unknown_flags_and_bad_limits_are_rejected() {
        assert!(parse_backfill_args(&argv(&["--nope"])).is_err());
        assert!(parse_backfill_args(&argv(&["--limit", "many"])).is_err());
        assert!(parse_backfill_args(&argv(&["--plans-dir"])).is_err());
    }

    /// The new subcommand must be reachable AND documented — a hand-rolled
    /// dispatch and a hand-written USAGE string can drift silently.
    #[test]
    fn the_subcommand_is_documented_in_usage() {
        assert!(USAGE.contains("plan-library-backfill"));
        assert!(USAGE.contains("--dry-run"));
        assert!(USAGE.contains("QONTINUI_PROMPTS_DIR"));
    }

    // ---- plan-workunit-backfill -------------------------------------------

    #[test]
    fn workunit_backfill_parses_its_full_flag_set() {
        let parsed = parse_workunit_backfill_args(&argv(&[
            "--dry-run",
            "--plans-dir",
            "/home/x/qontinui-dev-notes/plans",
            "--coord=https://coord.qontinui.io",
            "--limit",
            "40",
        ]))
        .expect("must parse");
        assert!(parsed.dry_run);
        assert_eq!(
            parsed.plans_dir.as_deref(),
            Some("/home/x/qontinui-dev-notes/plans")
        );
        assert_eq!(parsed.coord.as_deref(), Some("https://coord.qontinui.io"));
        assert_eq!(parsed.limit, Some(40));
    }

    #[test]
    fn workunit_backfill_rejects_flag_like_values_and_junk() {
        let err = parse_workunit_backfill_args(&argv(&["--plans-dir", "--dry-run"])).unwrap_err();
        assert!(err.contains("flag-like"), "got {err}");
        assert!(parse_workunit_backfill_args(&argv(&["--backend", "x"])).is_err());
        assert!(parse_workunit_backfill_args(&argv(&["--limit", "lots"])).is_err());
        assert_eq!(
            parse_workunit_backfill_args(&[]).unwrap(),
            WorkUnitBackfillArgs::default()
        );
    }

    /// Full precedence, tested against SUPPLIED candidates rather than the
    /// ambient process environment — the earlier version of this test never set
    /// a variable, so the property in its own name went unverified and its
    /// result depended on the box it ran on.
    ///
    /// `QONTINUI_PLAN_ADAPTER_DIR` deliberately outranks `QONTINUI_PLANS_DIR`:
    /// it is the variable that would have armed the reconcile loop, and a
    /// catch-up that ingests a different corpus than the loop would have is
    /// worse than no catch-up.
    #[test]
    fn workunit_backfill_plans_dir_precedence_is_flag_then_adapter_then_plans() {
        let d = |v: &str| Some(v.to_string());
        assert_eq!(
            resolve_backfill_plans_dir_from(d("/flag"), d("/adapter"), d("/plans")),
            Some(("/flag".to_string(), "--plans-dir"))
        );
        assert_eq!(
            resolve_backfill_plans_dir_from(None, d("/adapter"), d("/plans")),
            Some(("/adapter".to_string(), PLAN_ADAPTER_DIR_ENV))
        );
        assert_eq!(
            resolve_backfill_plans_dir_from(None, None, d("/plans")),
            Some(("/plans".to_string(), "QONTINUI_PLANS_DIR"))
        );
        assert_eq!(resolve_backfill_plans_dir_from(None, None, None), None);
        // Blank is UNSET at every layer — never a directory named "   ".
        assert_eq!(
            resolve_backfill_plans_dir_from(d("   "), d("  "), d("/plans")),
            Some(("/plans".to_string(), "QONTINUI_PLANS_DIR"))
        );
    }

    /// The coord base has the same shape, and the same blank-is-unset rule —
    /// the arm most likely to surprise is a blank `--coord` silently falling
    /// through to the ambient environment (or, worse, to the profile's
    /// production URL), so pin it.
    #[test]
    fn workunit_backfill_coord_base_precedence_and_blank_handling() {
        let d = |v: &str| Some(v.to_string());
        assert_eq!(
            resolve_backfill_coord_base_from(d("http://flag"), d("http://env"), d("http://prof")),
            Some(("http://flag".to_string(), "--coord"))
        );
        assert_eq!(
            resolve_backfill_coord_base_from(None, d("http://env"), d("http://prof")),
            Some(("http://env".to_string(), "COORD_HTTP_URL"))
        );
        assert_eq!(
            resolve_backfill_coord_base_from(None, None, d("http://prof")),
            Some(("http://prof".to_string(), "the runner's connected profile"))
        );
        assert_eq!(resolve_backfill_coord_base_from(None, None, None), None);
        assert_eq!(
            resolve_backfill_coord_base_from(d(" "), None, d("http://prof")),
            Some(("http://prof".to_string(), "the runner's connected profile")),
            "a blank --coord is unset, not a base URL of \" \""
        );
    }

    /// `--limit 0` parses, and is then REFUSED — the refusal is what keeps the
    /// preflight probe's slice index non-empty, so it is tested, not assumed.
    #[test]
    fn workunit_backfill_limit_zero_is_refused() {
        let zero = parse_workunit_backfill_args(&argv(&["--limit", "0"])).unwrap();
        assert_eq!(zero.limit, Some(0));
        let err = reject_zero_limit(&zero).unwrap_err();
        assert!(err.contains("--limit 0"), "got {err}");
        assert!(reject_zero_limit(&WorkUnitBackfillArgs::default()).is_ok());
        assert!(reject_zero_limit(
            &parse_workunit_backfill_args(&argv(&["--limit", "1"])).unwrap()
        )
        .is_ok());
    }

    /// USAGE must document the second backfill too, and must name the gate it
    /// routes around — an operator reading `--help` on a machine that ingests
    /// nothing needs to be told why.
    #[test]
    fn the_workunit_subcommand_is_documented_in_usage() {
        assert!(USAGE.contains("plan-workunit-backfill"));
        assert!(USAGE.contains("--coord <url>"));
        assert!(USAGE.contains("paths.plans_dir"));
        // The dry run's blind spot must be documented, not discovered.
        assert!(USAGE.contains("it cannot tell you which units would transition"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_full_flag_set() {
        let args: Vec<String> = [
            "--repo",
            "qontinui/qontinui-runner",
            "--head",
            "feat/x",
            "--base",
            "develop",
            "--title",
            "feat: x",
            "--body",
            "body text",
            "--draft",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let parsed = parse_pr_create_args(&args).unwrap();
        assert_eq!(parsed.repo.as_deref(), Some("qontinui/qontinui-runner"));
        assert_eq!(parsed.head.as_deref(), Some("feat/x"));
        assert_eq!(parsed.base.as_deref(), Some("develop"));
        assert_eq!(parsed.title.as_deref(), Some("feat: x"));
        assert_eq!(parsed.body.as_deref(), Some("body text"));
        assert!(parsed.draft);
    }

    #[test]
    fn parse_args_rejects_unknown_flag_and_missing_value() {
        let bad: Vec<String> = vec!["--frobnicate".to_string()];
        assert!(parse_pr_create_args(&bad).is_err());
        let dangling: Vec<String> = vec!["--title".to_string()];
        assert!(parse_pr_create_args(&dangling).is_err());
    }

    #[test]
    fn parse_args_rejects_flag_like_value() {
        // `--title --draft` must NOT yield a PR titled "--draft".
        let args: Vec<String> = ["--title", "--draft"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = parse_pr_create_args(&args).unwrap_err();
        assert!(err.contains("--title"), "{err}");
        assert!(
            err.contains("--flag=value")
                || err.contains("--title=<value>")
                || err.contains("--title="),
            "{err}"
        );
        // A single-dash value is fine (`--title -` is the stdin sentinel).
        let ok: Vec<String> = ["--title", "-"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            parse_pr_create_args(&ok).unwrap().title.as_deref(),
            Some("-")
        );
    }

    #[test]
    fn parse_args_supports_flag_equals_value_form() {
        // Values that legitimately begin with `--` use the `=` form.
        let args: Vec<String> = ["--title=--weird title", "--base=main", "--draft"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse_pr_create_args(&args).unwrap();
        assert_eq!(parsed.title.as_deref(), Some("--weird title"));
        assert_eq!(parsed.base.as_deref(), Some("main"));
        assert!(parsed.draft);
        // Value containing `=` splits only on the FIRST `=`.
        let args: Vec<String> = vec!["--body=a=b=c".to_string()];
        assert_eq!(
            parse_pr_create_args(&args).unwrap().body.as_deref(),
            Some("a=b=c")
        );
        // --draft takes no value.
        assert!(parse_pr_create_args(&["--draft=true".to_string()]).is_err());
    }

    #[test]
    fn repo_from_remote_url_handles_common_github_forms() {
        for url in [
            "https://github.com/qontinui/qontinui-runner.git",
            "https://github.com/qontinui/qontinui-runner",
            "git@github.com:qontinui/qontinui-runner.git",
            "ssh://git@github.com/qontinui/qontinui-runner.git",
            "https://github.com/qontinui/qontinui-runner/",
        ] {
            assert_eq!(
                repo_from_remote_url(url).as_deref(),
                Some("qontinui/qontinui-runner"),
                "{url}"
            );
        }
        assert_eq!(repo_from_remote_url("not-a-url"), None);
        assert_eq!(repo_from_remote_url("https://github.com/loner"), None);
    }

    #[test]
    fn parse_mcp_json_reads_proxy_nonce_and_port() {
        let text = r#"{
            "mcpServers": {
                "coord-mcp": {
                    "type": "http",
                    "url": "http://127.0.0.1:9877/coord-mcp",
                    "headers": { "X-Coord-Mcp-Proxy-Key": "abc123" }
                }
            }
        }"#;
        let cfg = parse_mcp_json(text).unwrap();
        assert_eq!(cfg.nonce, "abc123");
        assert_eq!(cfg.port, Some(9877));
    }

    #[test]
    fn parse_mcp_json_header_lookup_is_case_insensitive() {
        let text = r#"{
            "mcpServers": {
                "coord-mcp": {
                    "url": "http://127.0.0.1:9876/coord-mcp",
                    "headers": { "x-coord-mcp-proxy-key": "n0nce" }
                }
            }
        }"#;
        assert_eq!(parse_mcp_json(text).unwrap().nonce, "n0nce");
    }

    #[test]
    fn parse_mcp_json_ignores_static_bearer_configs() {
        // Agent-path static-bearer shape (no nonce header) must yield None —
        // the CLI cannot authenticate the loopback route with a bearer.
        let text = r#"{
            "mcpServers": {
                "coord-mcp": {
                    "url": "https://coord.qontinui.io/mcp",
                    "headers": { "Authorization": "Bearer xyz" }
                }
            }
        }"#;
        assert!(parse_mcp_json(text).is_none());
    }

    #[test]
    fn parse_mcp_json_skips_non_nonce_entry_without_masking_later_valid_one() {
        // A `/coord-mcp` entry WITHOUT the nonce header must be skipped
        // per-entry (continue), so a later valid entry is still found —
        // previously a `?` aborted the whole parse.
        let text = r#"{
            "mcpServers": {
                "coord-mcp-static": {
                    "url": "http://127.0.0.1:9876/coord-mcp",
                    "headers": { "Authorization": "Bearer xyz" }
                },
                "coord-mcp": {
                    "url": "http://127.0.0.1:9877/coord-mcp",
                    "headers": { "X-Coord-Mcp-Proxy-Key": "later-valid" }
                }
            }
        }"#;
        let cfg = parse_mcp_json(text).unwrap();
        assert_eq!(cfg.nonce, "later-valid");
        assert_eq!(cfg.port, Some(9877));
    }

    #[test]
    fn port_from_url_parses_loopback_urls() {
        assert_eq!(port_from_url("http://127.0.0.1:9877/coord-mcp"), Some(9877));
        assert_eq!(port_from_url("http://localhost:9876/x"), Some(9876));
        assert_eq!(port_from_url("https://coord.qontinui.io/mcp"), None);
    }

    #[test]
    fn resolve_port_pairs_config_port_with_nonce_env_is_explicit_override() {
        let session = SessionMcpConfig {
            nonce: "n".to_string(),
            port: Some(9878),
        };
        // Default: the port from the SAME .mcp.json as the nonce.
        assert_eq!(resolve_port(&session, None), Some(9878));
        // Explicit env override wins.
        assert_eq!(resolve_port(&session, Some("9899")), Some(9899));
        // Garbled env is ignored — falls back to the config port.
        assert_eq!(resolve_port(&session, Some("not-a-port")), Some(9878));
        // No port anywhere → None (caller errors out; NO scanning fallback).
        let portless = SessionMcpConfig {
            nonce: "n".to_string(),
            port: None,
        };
        assert_eq!(resolve_port(&portless, None), None);
        assert_eq!(resolve_port(&portless, Some("9880")), Some(9880));
    }

    #[test]
    fn find_session_mcp_config_walks_up_from_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"url":"http://127.0.0.1:9878/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"walkup"}}}}"#,
        )
        .unwrap();
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let cfg = find_session_mcp_config(&nested).unwrap();
        assert_eq!(cfg.nonce, "walkup");
        assert_eq!(cfg.port, Some(9878));
    }

    /// Phase 2 (plan 2026-08-20): the runner now emits the nonce in
    /// `Authorization: Bearer <nonce>` as well. This walk-up is a genuine
    /// reader break if it only knows the legacy name — against an
    /// Authorization-only config it would find NO key at all and the CLI would
    /// have nothing to send, silently (`continue` → `None`).
    #[test]
    fn walk_up_reads_the_nonce_from_authorization_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"url":"http://127.0.0.1:9878/coord-mcp","headers":{"Authorization":"Bearer authshape"}}}}"#,
        )
        .unwrap();
        let cfg = find_session_mcp_config(root).unwrap();
        assert_eq!(cfg.nonce, "authshape");
        assert_eq!(cfg.port, Some(9878));
    }

    /// Both present → `Authorization` wins, matching the runner's request-side
    /// resolver so the CLI can never present the losing key.
    #[test]
    fn walk_up_prefers_authorization_when_both_headers_are_present() {
        let cfg = parse_mcp_json(
            r#"{"mcpServers":{"coord-mcp":{"url":"http://127.0.0.1:9878/coord-mcp","headers":{"Authorization":"Bearer fromauth","X-Coord-Mcp-Proxy-Key":"fromlegacy"}}}}"#,
        )
        .unwrap();
        assert_eq!(cfg.nonce, "fromauth");
    }

    /// A STATIC-BEARER entry (a real JWT in `Authorization`) is still skipped
    /// per-entry rather than presented as a proxy key — and must not mask a
    /// later valid entry.
    #[test]
    fn walk_up_skips_a_static_bearer_entry_and_keeps_scanning() {
        let cfg = parse_mcp_json(
            r#"{"mcpServers":{"coord-mcp":{"url":"https://coord.example.test/coord-mcp","headers":{"Authorization":"Bearer eyJhbGciOiJFZERTQSJ9.eyJhIjoxfQ.c2ln"}},"coord-mcp-proxy":{"url":"http://127.0.0.1:9878/coord-mcp","headers":{"Authorization":"Bearer realnonce"}}}}"#,
        )
        .unwrap();
        assert_eq!(cfg.nonce, "realnonce");
        assert_eq!(cfg.port, Some(9878));
    }
}
