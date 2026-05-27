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
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("read config {path}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("parse config {path}: {e}"))?;
    Ok(Config {
        coord_url: v.get("coord_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        push_token: v.get("push_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        repos: v.get("repos")
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

fn to_bare_slug(owner_name: &str) -> &str {
    owner_name
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(owner_name)
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
        if args[i] == "--config" {
            if i + 1 < args.len() {
                config_path = Some(&args[i + 1]);
                i += 2;
                continue;
            }
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

    let path = match pairs.get("path") {
        Some(p) => p.trim_end_matches(".git").to_string(),
        None => return ExitCode::SUCCESS,
    };

    // Check if this repo is in our registered list.
    let is_registered = config.repos.iter().any(|r| {
        let r_slug = r.trim_end_matches(".git");
        r_slug == path || r_slug.ends_with(&format!("/{}", to_bare_slug(&path)))
    });

    if !is_registered {
        // Not a coord-registered repo; exit with no output so git falls through.
        return ExitCode::SUCCESS;
    }

    let (scheme, host) = match extract_coord_host_and_scheme(&config.coord_url) {
        Some(pair) => pair,
        None => return ExitCode::SUCCESS,
    };

    let repo_slug = to_bare_slug(&path);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "protocol={scheme}");
    let _ = writeln!(out, "host={host}");
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
    fn to_bare_slug_strips_owner() {
        assert_eq!(to_bare_slug("acme/widget"), "widget");
    }

    #[test]
    fn to_bare_slug_no_slash() {
        assert_eq!(to_bare_slug("widget"), "widget");
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
