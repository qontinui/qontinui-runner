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
//! (`X-Coord-Mcp-Proxy-Key`), discovered by a **`.mcp.json` walk-up from cwd**.
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

const PROXY_KEY_HEADER: &str = "X-Coord-Mcp-Proxy-Key";
const RUNNER_PORT_ENV: &str = "QONTINUI_RUNNER_API_PORT";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("create") => pr_create(&args[1..]),
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
qontinui-pr — Qontinui Runner session PR CLI

USAGE:
  qontinui-pr create --title <title> [options]

OPTIONS (create):
  --repo <owner/name>   Target repo (default: inferred from `git remote get-url origin`)
  --head <branch>       Head branch (default: `git symbolic-ref --short HEAD`)
  --base <branch>       Base branch (default: main)
  --title <text>        PR title (required; `--title -` reads the first line of stdin)
  --body <text>         PR body
  --body-file <path>    Read the PR body from a file
  --draft               Open as a draft PR

Values that themselves begin with `--` must use the `--flag=value` form.

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
    let resp = match client
        .post(&url)
        .header(PROXY_KEY_HEADER, &session.nonce)
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
/// loopback `/coord-mcp` proxy and read its `X-Coord-Mcp-Proxy-Key` header
/// (case-insensitive) + the port embedded in the URL. Entries without the
/// nonce header (e.g. a static-bearer config) are SKIPPED per-entry — a
/// non-nonce `/coord-mcp` entry must not mask a later valid one.
fn parse_mcp_json(text: &str) -> Option<SessionMcpConfig> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let servers = v.get("mcpServers")?.as_object()?;
    for server in servers.values() {
        let url = server.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if !url.contains("/coord-mcp") {
            continue;
        }
        let nonce = match server
            .get("headers")
            .and_then(|h| h.as_object())
            .and_then(|h| {
                h.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(PROXY_KEY_HEADER))
                    .and_then(|(_, val)| val.as_str())
            }) {
            Some(n) => n.to_string(),
            None => continue,
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
}
