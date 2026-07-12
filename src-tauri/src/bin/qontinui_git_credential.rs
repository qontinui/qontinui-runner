use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

#[derive(Default)]
struct Config {
    coord_url: String,
    push_token: String,
    repos: Vec<String>,
}

fn load_config(path: &str) -> Result<Config, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("read config {path}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("parse config {path}: {e}"))?;
    Ok(Config {
        coord_url: v
            .get("coord_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        push_token: v
            .get("push_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        repos: v
            .get("repos")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_stdin_pairs() -> HashMap<String, String> {
    let mut pairs = HashMap::new();
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once('=') {
            pairs.insert(key.to_string(), value.to_string());
        }
    }
    pairs
}

/// Normalize a git-credential request `path` into a repo-slug candidate:
/// strips a leading `/`, the `git/` smart-HTTP route prefix, and a trailing
/// `.git`. `git/qontinui/qontinui-coord.git` → `qontinui/qontinui-coord`;
/// legacy flat `git/qontinui-coord.git` → `qontinui-coord`.
fn normalize_request_path(path: &str) -> &str {
    let p = path.trim_start_matches('/');
    let p = p.strip_prefix("git/").unwrap_or(p);
    p.strip_suffix(".git").unwrap_or(p)
}

/// Resolve the FULL registered `owner/name` slug for a request-path
/// candidate. Coord's registry rows carry owner-qualified slugs
/// (`qontinui/qontinui-coord`); the emitted credential path must carry the
/// owner too, so we always answer with the registry's full slug.
///
/// Owner-qualified candidates must match exactly — a basename fallback here
/// would recreate the owner-collapse collision the cutover exists to fix
/// (`qontinui/tools` vs `fork-org/tools`). Bare single-segment candidates
/// (legacy flat-path remotes, pre-cutover) match by basename.
fn resolve_registered_slug<'a>(repos: &'a [String], candidate: &str) -> Option<&'a str> {
    // Exact match on the full slug.
    for r in repos {
        let slug = r.trim_end_matches(".git");
        if slug == candidate {
            return Some(slug);
        }
    }
    // Legacy flat remotes send only the basename; never apply this to an
    // owner-qualified candidate (owner-collapse hazard).
    if !candidate.contains('/') {
        for r in repos {
            let slug = r.trim_end_matches(".git");
            if slug == candidate || slug.ends_with(&format!("/{candidate}")) {
                return Some(slug);
            }
        }
    }
    None
}

fn extract_coord_host_and_scheme(coord_url: &str) -> Option<(&str, &str)> {
    if let Some(rest) = coord_url.strip_prefix("https://") {
        Some(("https", rest.trim_end_matches('/')))
    } else if let Some(rest) = coord_url.strip_prefix("http://") {
        Some(("http", rest.trim_end_matches('/')))
    } else {
        None
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // Git calls the credential helper with an action: get, store, or erase.
    // The action is the last positional argument after our own flags.
    let mut config_path: Option<&str> = None;
    let mut action: Option<&str> = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            config_path = Some(&args[i + 1]);
            i += 2;
            continue;
        }
        // The action is the remaining positional arg.
        action = Some(&args[i]);
        i += 1;
    }

    let action = match action {
        Some(a) => a,
        None => return ExitCode::SUCCESS,
    };

    // Only respond to "get". For "store" and "erase", exit silently.
    if action != "get" {
        return ExitCode::SUCCESS;
    }

    let config_path = match config_path {
        Some(p) => p,
        None => {
            eprintln!("qontinui-git-credential: --config <path> is required");
            return ExitCode::from(1);
        }
    };

    let config = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qontinui-git-credential: {e}");
            return ExitCode::from(1);
        }
    };

    if config.coord_url.is_empty() || config.push_token.is_empty() {
        return ExitCode::SUCCESS;
    }

    let pairs = parse_stdin_pairs();

    let candidate = match pairs.get("path") {
        Some(p) => normalize_request_path(p),
        None => return ExitCode::SUCCESS,
    };

    // Resolve the request against the registered list; keep the FULL
    // owner-qualified slug for the emitted path.
    let repo_slug = match resolve_registered_slug(&config.repos, candidate) {
        Some(slug) => slug,
        // Not a coord-registered repo; exit with no output so git falls through.
        None => return ExitCode::SUCCESS,
    };

    let (scheme, host) = match extract_coord_host_and_scheme(&config.coord_url) {
        Some(pair) => pair,
        None => return ExitCode::SUCCESS,
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "protocol={scheme}");
    let _ = writeln!(out, "host={host}");
    // Owner-qualified smart-HTTP path (multitenant cutover):
    // `git/<owner>/<name>.git`.
    let _ = writeln!(out, "path=git/{repo_slug}.git");
    let _ = writeln!(out, "username=x-access-token");
    let _ = writeln!(out, "password={}", config.push_token);
    let _ = writeln!(out);

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_owner_qualified_request_path() {
        assert_eq!(
            normalize_request_path("git/qontinui/qontinui-coord.git"),
            "qontinui/qontinui-coord"
        );
        // Leading slash variant (git may pass the path with or without it).
        assert_eq!(
            normalize_request_path("/git/qontinui/qontinui-coord.git"),
            "qontinui/qontinui-coord"
        );
    }

    #[test]
    fn normalize_legacy_flat_request_path() {
        assert_eq!(
            normalize_request_path("git/qontinui-coord.git"),
            "qontinui-coord"
        );
        assert_eq!(normalize_request_path("qontinui-coord"), "qontinui-coord");
    }

    #[test]
    fn resolve_owner_qualified_candidate_exact_match() {
        let repos = vec!["qontinui/qontinui-coord".to_string()];
        assert_eq!(
            resolve_registered_slug(&repos, "qontinui/qontinui-coord"),
            Some("qontinui/qontinui-coord")
        );
    }

    #[test]
    fn resolve_owner_qualified_candidate_rejects_other_owner() {
        // Owner-collapse guard: fork-org/tools must NOT match qontinui/tools.
        let repos = vec!["qontinui/tools".to_string()];
        assert_eq!(resolve_registered_slug(&repos, "fork-org/tools"), None);
    }

    #[test]
    fn resolve_bare_candidate_returns_full_slug() {
        // Legacy flat remote path resolves to the FULL registry slug so the
        // emitted credential path is owner-qualified.
        let repos = vec!["qontinui/qontinui-coord".to_string()];
        assert_eq!(
            resolve_registered_slug(&repos, "qontinui-coord"),
            Some("qontinui/qontinui-coord")
        );
    }

    #[test]
    fn resolve_unregistered_candidate_is_none() {
        let repos = vec!["qontinui/qontinui-coord".to_string()];
        assert_eq!(resolve_registered_slug(&repos, "qontinui/other"), None);
        assert_eq!(resolve_registered_slug(&repos, "other"), None);
    }

    #[test]
    fn emitted_path_is_owner_qualified() {
        // The exact wire format git matches against the remote URL path:
        // `git/<owner>/<name>.git`.
        let repos = vec!["qontinui/qontinui-coord".to_string()];
        let candidate = normalize_request_path("git/qontinui/qontinui-coord.git");
        let slug = resolve_registered_slug(&repos, candidate).unwrap();
        assert_eq!(
            format!("path=git/{slug}.git"),
            "path=git/qontinui/qontinui-coord.git"
        );
    }

    #[test]
    fn extract_coord_host_https() {
        let (scheme, host) = extract_coord_host_and_scheme("https://coord.qontinui.io").unwrap();
        assert_eq!(scheme, "https");
        assert_eq!(host, "coord.qontinui.io");
    }

    #[test]
    fn extract_coord_host_http_with_port() {
        let (scheme, host) = extract_coord_host_and_scheme("http://localhost:9870").unwrap();
        assert_eq!(scheme, "http");
        assert_eq!(host, "localhost:9870");
    }

    #[test]
    fn extract_coord_host_invalid() {
        assert!(extract_coord_host_and_scheme("ftp://foo").is_none());
    }
}
