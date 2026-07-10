//! `qontinui` — the session CLI delivered onto every runner-hosted terminal's
//! PATH by the identity-shim materializer
//! (`install_effects_producer::intercept::shim_materializer::materialize_identity`).
//!
//! ## `qontinui pr create`
//! Opens a pull request WITHOUT a personal GitHub login on the machine: it
//! POSTs the runner's loopback `POST /vcs/pull-requests` proxy, which injects
//! the runner's live device JWT and forwards to coord's brokered PR-creation
//! route (`POST {coord}/coord/repos/{owner}/{repo}/pull-requests`). Coord's
//! verdict (201 / 403 / 404 / 429) surfaces verbatim.
//!
//! ## Runner discovery chain (port + loopback auth)
//! The loopback route requires the per-session coord-mcp proxy nonce
//! (`X-Coord-Mcp-Proxy-Key`). Discovery, strongest first:
//!
//! 1. **`.mcp.json` walk-up from cwd** (authoritative). The runner provisions
//!    every session workdir with a `.mcp.json` whose `coord-mcp` server entry
//!    carries BOTH the nonce header and a loopback URL on the ACTUALLY-BOUND
//!    API port (`coord_mcp::write_coord_mcp_proxy_config`; the reconciler
//!    rewrites it on port drift). This is the only on-disk copy of the nonce —
//!    without it the CLI cannot authenticate and fails with a clear error.
//! 2. **`QONTINUI_RUNNER_API_PORT` env** (port fallback). Injected into every
//!    runner terminal at PTY spawn (`terminal/session.rs`), but sourced from
//!    the bootstrap default rather than the bound port, so it ranks below the
//!    `.mcp.json` URL.
//! 3. **Loopback probe** (last resort): `GET /health` on 9876, then 9877-9899
//!    (the temp/secondary-runner range), first responder wins.
//!
//! The port candidates are only ever PROBED with the safe `GET /health`; the
//! PR-creation POST fires exactly once, against the selected port.
//!
//! Style: matches the sibling standalone bins (`qontinui_git_credential`) —
//! hand-rolled arg parsing, no CLI crates; `reqwest::blocking` for HTTP (the
//! package dependency already carries the `blocking` feature).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const PROXY_KEY_HEADER: &str = "X-Coord-Mcp-Proxy-Key";
const RUNNER_PORT_ENV: &str = "QONTINUI_RUNNER_API_PORT";
const PRIMARY_PORT: u16 = 9876;
const SECONDARY_PORT_RANGE: std::ops::RangeInclusive<u16> = 9877..=9899;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pr") => match args.get(1).map(String::as_str) {
            Some("create") => pr_create(&args[2..]),
            other => {
                eprintln!(
                    "qontinui: unknown pr subcommand {:?}\n{}",
                    other.unwrap_or(""),
                    USAGE
                );
                ExitCode::from(2)
            }
        },
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("qontinui: unknown command {other:?}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

const USAGE: &str = "\
qontinui — Qontinui Runner session CLI

USAGE:
  qontinui pr create --title <title> [options]

OPTIONS (pr create):
  --repo <owner/name>   Target repo (default: inferred from `git remote get-url origin`)
  --head <branch>       Head branch (default: `git symbolic-ref --short HEAD`)
  --base <branch>       Base branch (default: main)
  --title <text>        PR title (required; `--title -` reads the first line of stdin)
  --body <text>         PR body
  --body-file <path>    Read the PR body from a file
  --draft               Open as a draft PR

Opens the PR through the runner's coord-brokered loopback proxy — no personal
`gh auth login` required. On success prints the PR URL to stdout.";

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
        let flag = args[i].as_str();
        let mut take_value = |slot: &mut Option<String>| -> Result<(), String> {
            match args.get(i + 1) {
                Some(v) => {
                    *slot = Some(v.clone());
                    Ok(())
                }
                None => Err(format!("{flag} requires a value")),
            }
        };
        match flag {
            "--repo" => {
                take_value(&mut out.repo)?;
                i += 2;
            }
            "--head" => {
                take_value(&mut out.head)?;
                i += 2;
            }
            "--base" => {
                take_value(&mut out.base)?;
                i += 2;
            }
            "--title" => {
                take_value(&mut out.title)?;
                i += 2;
            }
            "--body" => {
                take_value(&mut out.body)?;
                i += 2;
            }
            "--body-file" => {
                take_value(&mut out.body_file)?;
                i += 2;
            }
            "--draft" => {
                out.draft = true;
                i += 1;
            }
            other => return Err(format!("unknown flag {other:?}")),
        }
    }
    Ok(out)
}

fn pr_create(args: &[String]) -> ExitCode {
    let parsed = match parse_pr_create_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("qontinui: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    // --title (required; `-` reads the first line of stdin).
    let title = match parsed.title.as_deref() {
        Some("-") => match first_stdin_line() {
            Some(t) if !t.trim().is_empty() => t,
            _ => {
                eprintln!("qontinui: --title - given but stdin had no title line");
                return ExitCode::from(2);
            }
        },
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => {
            eprintln!("qontinui: --title is required (`--title -` reads it from stdin)");
            return ExitCode::from(2);
        }
    };

    // --body / --body-file (mutually additive: --body-file wins if both given).
    let body = match &parsed.body_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("qontinui: read --body-file {path}: {e}");
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
                "qontinui: could not infer the repo from `git remote get-url origin` — \
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
                "qontinui: could not resolve the current branch (detached HEAD?) — pass --head"
            );
            return ExitCode::from(2);
        }
    };

    let base = parsed.base.clone().unwrap_or_else(|| "main".to_string());

    // Runner discovery (see the module comment for the chain).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = find_session_mcp_config(&cwd);
    let nonce = match session.as_ref().map(|s| s.nonce.clone()) {
        Some(n) => n,
        None => {
            eprintln!(
                "qontinui: no runner session credential found — no `.mcp.json` with a \
                 coord-mcp entry between {} and the filesystem root. This session was \
                 not provisioned by the runner (or provisioning is degraded). \
                 Fallback: `gh pr create` works where a personal `gh auth login` exists.",
                cwd.display()
            );
            return ExitCode::from(1);
        }
    };
    let port = match discover_port(session.and_then(|s| s.port)) {
        Some(p) => p,
        None => {
            eprintln!(
                "qontinui: no reachable runner loopback API found (tried the session \
                 .mcp.json port, ${RUNNER_PORT_ENV}, and 9876-9899) — is the runner up?"
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
            eprintln!("qontinui: build http client: {e}");
            return ExitCode::from(1);
        }
    };
    let url = format!("http://127.0.0.1:{port}/vcs/pull-requests");
    let resp = match client
        .post(&url)
        .header(PROXY_KEY_HEADER, &nonce)
        .json(&payload)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("qontinui: POST {url}: {e}");
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
        eprintln!("qontinui: pr create failed ({status}): {}", text.trim());
        ExitCode::from(1)
    }
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
/// loopback `/coord-mcp` proxy and read its `X-Coord-Mcp-Proxy-Key` header
/// (case-insensitive) + the port embedded in the URL. Static-bearer (agent)
/// configs without the nonce header yield `None`.
fn parse_mcp_json(text: &str) -> Option<SessionMcpConfig> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let servers = v.get("mcpServers")?.as_object()?;
    for server in servers.values() {
        let url = server.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if !url.contains("/coord-mcp") {
            continue;
        }
        let nonce = server
            .get("headers")
            .and_then(|h| h.as_object())
            .and_then(|h| {
                h.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(PROXY_KEY_HEADER))
                    .and_then(|(_, val)| val.as_str())
            })
            .map(String::from)?;
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

/// Quick loopback liveness probe: `GET /health` with a short timeout (safe —
/// only the final PR POST is a write, and it fires once).
fn health_ok(client: &reqwest::blocking::Client, port: u16) -> bool {
    client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Resolve the runner port per the module-comment discovery chain, verifying
/// each candidate with `GET /health` before committing the POST to it.
fn discover_port(mcp_port: Option<u16>) -> Option<u16> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok()?;
    let env_port: Option<u16> = std::env::var(RUNNER_PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok());
    let mut candidates: Vec<u16> = Vec::new();
    let mut push = |p: u16| {
        if !candidates.contains(&p) {
            candidates.push(p);
        }
    };
    if let Some(p) = mcp_port {
        push(p);
    }
    if let Some(p) = env_port {
        push(p);
    }
    push(PRIMARY_PORT);
    for p in SECONDARY_PORT_RANGE {
        push(p);
    }
    candidates.into_iter().find(|&p| health_ok(&client, p))
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
    fn port_from_url_parses_loopback_urls() {
        assert_eq!(port_from_url("http://127.0.0.1:9877/coord-mcp"), Some(9877));
        assert_eq!(port_from_url("http://localhost:9876/x"), Some(9876));
        assert_eq!(port_from_url("https://coord.qontinui.io/mcp"), None);
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
}
