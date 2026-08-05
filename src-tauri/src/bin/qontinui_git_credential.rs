use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use base64::Engine as _;

/// Decision logging is opt-in via `QONTINUI_GIT_CRED_DEBUG=1`. This helper
/// runs on EVERY git credential lookup in registered repos and git surfaces
/// helper stderr straight to users, so the default must be silent.
fn debug_enabled() -> bool {
    std::env::var("QONTINUI_GIT_CRED_DEBUG").ok().as_deref() == Some("1")
}

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if debug_enabled() {
            eprintln!("qontinui-git-credential: {}", format_args!($($arg)*));
        }
    };
}

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

/// Extract `(scheme, host)` from the coord base URL. The host keeps its port
/// verbatim (`"localhost:9870"`) — the request-host comparison is exact,
/// port included. Any non-http(s) or hostless URL is `None`.
fn extract_coord_host_and_scheme(coord_url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = if let Some(rest) = coord_url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = coord_url.strip_prefix("http://") {
        ("http", rest)
    } else {
        return None;
    };
    let host = rest.split('/').next().unwrap_or(rest);
    if host.is_empty() {
        None
    } else {
        Some((scheme, host))
    }
}

/// Best-effort JWT expiry for debug logging: if the push token looks like a
/// JWT whose payload segment base64url-decodes to JSON with a numeric `exp`,
/// return it. Any failure at any step is `None` — this must never affect the
/// emit decision, and there is no signature verification (logging only).
fn jwt_exp(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let (_header, payload) = (parts.next()?, parts.next()?);
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp")?.as_i64()
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

/// Normalize a registry row into a slug. Single `.git` strip, matching
/// [`normalize_request_path`] exactly — a repo genuinely named `foo.git`
/// would be mangled by repeated stripping, and one strip is the git
/// semantic for the URL suffix.
fn normalize_registry_slug(row: &str) -> &str {
    row.strip_suffix(".git").unwrap_or(row)
}

/// The owner coord maps legacy bare repo slugs under. Mirrors coord's
/// `git_origin::default_repo_owner` exactly — env `GITHUB_REPO_OWNER`
/// (trimmed, non-empty) else `"qontinui"`.
fn default_repo_owner() -> String {
    owner_or_default(std::env::var("GITHUB_REPO_OWNER").ok().as_deref())
}

/// Pure core of [`default_repo_owner`], split out for deterministic tests
/// (no process-global env mutation in the test suite).
fn owner_or_default(env_value: Option<&str>) -> String {
    match env_value.map(str::trim) {
        Some(owner) if !owner.is_empty() => owner.to_string(),
        _ => "qontinui".to_string(),
    }
}

/// Decide whether a request-path candidate names a registered repo. The
/// resolved slug drives ONLY the emit decision (whether to answer with
/// credentials at all) — it is never echoed back into the credential
/// description.
///
/// Matching rules:
/// - Exact slug match always wins.
/// - Bare single-segment candidates (legacy flat-path remotes, pre-cutover)
///   match owner-qualified rows by basename. If several rows share the
///   basename, the row owned by the default owner (coord's legacy mapping)
///   wins; with no default-owner row among them we fail closed and emit
///   nothing — no credential beats a wrong-owner credential.
/// - Owner-qualified candidates with no exact match fall back to a BARE
///   registry row equal to their basename (cutover window: the remote is
///   already owner-qualified but the registry row is still bare). A
///   basename match against a DIFFERENT owner-qualified row stays
///   forbidden — that would recreate the owner-collapse collision the
///   cutover exists to fix (`qontinui/tools` vs `fork-org/tools`).
fn resolve_registered_slug<'a>(repos: &'a [String], candidate: &str) -> Option<&'a str> {
    // Exact match on the full slug.
    for r in repos {
        let slug = normalize_registry_slug(r);
        if slug == candidate {
            return Some(slug);
        }
    }
    if let Some((_, basename)) = candidate.rsplit_once('/') {
        // Owner-qualified candidate, no exact match: a bare registry row
        // equal to the basename is the same repo mid-cutover.
        return repos
            .iter()
            .map(|r| normalize_registry_slug(r))
            .find(|slug| *slug == basename);
    }
    // Legacy flat remotes send only the basename.
    let matches: Vec<&str> = repos
        .iter()
        .map(|r| normalize_registry_slug(r))
        .filter(|slug| slug.ends_with(&format!("/{candidate}")))
        .collect();
    match matches.as_slice() {
        [] => None,
        [only] => Some(only),
        many => {
            // Ambiguous basename: prefer the default-owner row (the owner
            // coord itself maps this bare slug under); otherwise fail closed.
            let preferred = format!("{}/{candidate}", default_repo_owner());
            many.iter().find(|slug| **slug == preferred).copied()
        }
    }
}

/// Pure decision core: given the parsed config and the request pairs, return
/// the full output block to print (`Some`) or `None` for a silent
/// fall-through to the next helper. Split from `main()` so tests exercise
/// every exit path without spawning processes.
fn respond(config: &Config, pairs: &HashMap<String, String>) -> Option<String> {
    if config.push_token.is_empty() {
        debug_log!("skip: empty config (no push_token)");
        return None;
    }

    // Host scoping. The runner sets `credential.useHttpPath=true` per repo,
    // so git sends `path=` to this helper for EVERY host a registered repo
    // talks to — github.com included. The registered-repo check alone would
    // therefore hand coord credentials to non-coord hosts. Only answer when
    // the request host equals the coord host exactly (port included, e.g.
    // "localhost:9870"); everything else falls through to the next helper.
    let (_scheme, coord_host) = match extract_coord_host_and_scheme(&config.coord_url) {
        Some(pair) => pair,
        None => {
            debug_log!(
                "skip: config coord_url missing or unparseable ({:?})",
                config.coord_url
            );
            return None;
        }
    };
    let request_host = match pairs.get("host") {
        Some(h) => h.as_str(),
        None => {
            debug_log!("skip: request has no host attribute");
            return None;
        }
    };
    if request_host != coord_host {
        debug_log!("skip: request host {request_host} is not the coord host {coord_host}");
        return None;
    }

    let candidate = match pairs.get("path") {
        Some(p) => normalize_request_path(p),
        None => {
            debug_log!("skip: request has no path attribute");
            return None;
        }
    };

    // The resolved slug drives ONLY the decision to answer — the credential
    // description git asked about is never rewritten (see below).
    let slug = match resolve_registered_slug(&config.repos, candidate) {
        Some(s) => s,
        None => {
            // Not a coord-registered repo; no output so git falls through.
            debug_log!("skip: {candidate} is not a coord-registered repo");
            return None;
        }
    };

    if debug_enabled() {
        match jwt_exp(&config.push_token) {
            Some(exp) => {
                debug_log!(
                    "emit: coord credentials for host {request_host} repo {slug} (token exp {exp})"
                )
            }
            None => debug_log!("emit: coord credentials for host {request_host} repo {slug}"),
        }
    }

    // Emit ONLY the credentials. protocol/host/path are deliberately omitted:
    // git treats missing attributes as unchanged, so the request's own
    // description stays intact — rewriting it (e.g. to a normalized
    // `path=git/<slug>.git`) would pollute chained credential stores with a
    // description that differs from what git actually asked about.
    Some(format!(
        "username=x-access-token\npassword={}\n\n",
        config.push_token
    ))
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

    let pairs = parse_stdin_pairs();

    if let Some(output) = respond(&config, &pairs) {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(output.as_bytes());
    }

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
    fn resolve_ambiguous_bare_candidate_prefers_default_owner() {
        // Two owners share the basename: the default-owner row (coord's own
        // legacy mapping for bare slugs) wins — never first-match order.
        let repos = vec!["fork-org/tools".to_string(), "qontinui/tools".to_string()];
        assert_eq!(
            resolve_registered_slug(&repos, "tools"),
            Some("qontinui/tools")
        );
    }

    #[test]
    fn resolve_ambiguous_bare_candidate_without_default_owner_fails_closed() {
        // Ambiguous basename with NO default-owner row: emitting nothing
        // beats emitting a wrong-owner credential.
        let repos = vec!["fork-org/tools".to_string(), "other-org/tools".to_string()];
        assert_eq!(resolve_registered_slug(&repos, "tools"), None);
    }

    #[test]
    fn resolve_owner_qualified_candidate_falls_back_to_bare_registry_row() {
        // Cutover window: remote already owner-qualified, registry row still
        // bare — the basename-equal bare row is the same repo.
        let repos = vec!["qontinui-coord".to_string()];
        assert_eq!(
            resolve_registered_slug(&repos, "qontinui/qontinui-coord"),
            Some("qontinui-coord")
        );
        // But a basename match against a DIFFERENT owner-qualified row stays
        // forbidden (owner-collapse hazard).
        let repos = vec!["fork-org/qontinui-coord".to_string()];
        assert_eq!(
            resolve_registered_slug(&repos, "qontinui/qontinui-coord"),
            None
        );
    }

    #[test]
    fn registry_and_request_git_suffix_normalization_are_symmetric() {
        // Both sides strip at most ONE `.git`. A repo genuinely named
        // `foo.git`, listed with a `.git` route suffix
        // (`qontinui/foo.git` + `.git`), must keep its real name — the old
        // `trim_end_matches` collapsed it to `qontinui/foo`, so it could
        // never match the request-side candidate (which is single-stripped).
        assert_eq!(
            normalize_registry_slug("qontinui/repo.git"),
            "qontinui/repo"
        );
        assert_eq!(
            normalize_registry_slug("qontinui/foo.git.git"),
            "qontinui/foo.git"
        );
        let repos = vec!["qontinui/foo.git.git".to_string()];
        let candidate = normalize_request_path("git/qontinui/foo.git.git");
        assert_eq!(candidate, "qontinui/foo.git");
        assert_eq!(
            resolve_registered_slug(&repos, candidate),
            Some("qontinui/foo.git")
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
        // Trailing slash / path segments never leak into the host.
        let (_, host) = extract_coord_host_and_scheme("http://localhost:9870/").unwrap();
        assert_eq!(host, "localhost:9870");
        let (_, host) = extract_coord_host_and_scheme("https://coord.qontinui.io/coord").unwrap();
        assert_eq!(host, "coord.qontinui.io");
    }

    #[test]
    fn extract_coord_host_invalid() {
        assert!(extract_coord_host_and_scheme("ftp://foo").is_none());
        assert!(extract_coord_host_and_scheme("coord.qontinui.io").is_none());
        assert!(extract_coord_host_and_scheme("https://").is_none());
        assert!(extract_coord_host_and_scheme("").is_none());
    }

    fn test_config(coord_url: &str) -> Config {
        Config {
            coord_url: coord_url.to_string(),
            push_token: "tok-123".to_string(),
            repos: vec!["qontinui/qontinui-coord".to_string()],
        }
    }

    fn pairs(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const CRED_BLOCK: &str = "username=x-access-token\npassword=tok-123\n\n";

    #[test]
    fn respond_github_host_registered_repo_falls_through() {
        // The live bug this closes: useHttpPath=true makes git send `path=`
        // for github.com too — a registered slug there must NOT get coord
        // credentials.
        let config = test_config("https://coord.qontinui.io");
        let req = pairs(&[
            ("protocol", "https"),
            ("host", "github.com"),
            ("path", "qontinui/qontinui-coord.git"),
        ]);
        assert_eq!(respond(&config, &req), None);
    }

    #[test]
    fn respond_coord_host_registered_repo_emits() {
        let config = test_config("https://coord.qontinui.io");
        let req = pairs(&[
            ("protocol", "https"),
            ("host", "coord.qontinui.io"),
            ("path", "git/qontinui/qontinui-coord.git"),
        ]);
        assert_eq!(respond(&config, &req).as_deref(), Some(CRED_BLOCK));
    }

    #[test]
    fn respond_coord_host_unregistered_repo_falls_through() {
        let config = test_config("https://coord.qontinui.io");
        let req = pairs(&[
            ("host", "coord.qontinui.io"),
            ("path", "git/other-org/other.git"),
        ]);
        assert_eq!(respond(&config, &req), None);
    }

    #[test]
    fn respond_missing_host_falls_through() {
        let config = test_config("https://coord.qontinui.io");
        let req = pairs(&[("path", "git/qontinui/qontinui-coord.git")]);
        assert_eq!(respond(&config, &req), None);
    }

    #[test]
    fn respond_missing_path_falls_through() {
        let config = test_config("https://coord.qontinui.io");
        let req = pairs(&[("host", "coord.qontinui.io")]);
        assert_eq!(respond(&config, &req), None);
    }

    #[test]
    fn respond_port_bearing_host_matches_exactly() {
        let config = test_config("http://localhost:9870");
        let req = pairs(&[
            ("host", "localhost:9870"),
            ("path", "git/qontinui/qontinui-coord.git"),
        ]);
        assert_eq!(respond(&config, &req).as_deref(), Some(CRED_BLOCK));

        // Mismatched or missing port is a different host — fall through.
        for host in ["localhost:9871", "localhost"] {
            let req = pairs(&[("host", host), ("path", "git/qontinui/qontinui-coord.git")]);
            assert_eq!(respond(&config, &req), None, "host {host} must not match");
        }
    }

    #[test]
    fn respond_empty_or_unparseable_coord_url_fails_closed() {
        // A config without coord_url (or with a non-http(s) one) can't be
        // host-scoped — never emit.
        for coord_url in ["", "ftp://coord.qontinui.io"] {
            let config = test_config(coord_url);
            let req = pairs(&[
                ("host", "coord.qontinui.io"),
                ("path", "git/qontinui/qontinui-coord.git"),
            ]);
            assert_eq!(respond(&config, &req), None, "coord_url {coord_url:?}");
        }
    }

    #[test]
    fn respond_empty_push_token_falls_through() {
        let mut config = test_config("https://coord.qontinui.io");
        config.push_token = String::new();
        let req = pairs(&[
            ("host", "coord.qontinui.io"),
            ("path", "git/qontinui/qontinui-coord.git"),
        ]);
        assert_eq!(respond(&config, &req), None);
    }

    #[test]
    fn jwt_exp_is_best_effort() {
        // Well-formed JWT-shaped token with a numeric exp.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"exp":1767225600,"sub":"push"}"#);
        let token = format!("eyJhbGciOiJIUzI1NiJ9.{payload}.sig");
        assert_eq!(jwt_exp(&token), Some(1767225600));

        // Anything else decodes to None without erroring.
        assert_eq!(jwt_exp("opaque-token"), None);
        assert_eq!(jwt_exp("a.b"), None);
        assert_eq!(jwt_exp("a.!!!.c"), None);
        assert_eq!(jwt_exp("a.b.c.d"), None);
        let no_exp = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#);
        assert_eq!(jwt_exp(&format!("h.{no_exp}.s")), None);
    }

    #[test]
    fn owner_or_default_matches_coord_legacy_mapping() {
        // Same rule as coord `git_origin::default_repo_owner`: env value
        // (trimmed, non-empty) wins, else "qontinui".
        assert_eq!(owner_or_default(None), "qontinui");
        assert_eq!(owner_or_default(Some("")), "qontinui");
        assert_eq!(owner_or_default(Some("   ")), "qontinui");
        assert_eq!(owner_or_default(Some("acme")), "acme");
        assert_eq!(owner_or_default(Some("  acme ")), "acme");
    }
}
