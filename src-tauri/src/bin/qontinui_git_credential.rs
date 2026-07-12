use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

#[derive(Default)]
struct Config {
    push_token: String,
    repos: Vec<String>,
}

fn load_config(path: &str) -> Result<Config, String> {
    let data = std::fs::read_to_string(path).map_err(|e| format!("read config {path}: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("parse config {path}: {e}"))?;
    Ok(Config {
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

    if config.push_token.is_empty() {
        return ExitCode::SUCCESS;
    }

    let pairs = parse_stdin_pairs();

    let candidate = match pairs.get("path") {
        Some(p) => normalize_request_path(p),
        None => return ExitCode::SUCCESS,
    };

    // The resolved slug drives ONLY the decision to answer — the credential
    // description git asked about is never rewritten (see below).
    if resolve_registered_slug(&config.repos, candidate).is_none() {
        // Not a coord-registered repo; exit with no output so git falls through.
        return ExitCode::SUCCESS;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    // Emit ONLY the credentials. protocol/host/path are deliberately omitted:
    // git treats missing attributes as unchanged, so the request's own
    // description stays intact — rewriting it (e.g. to a normalized
    // `path=git/<slug>.git`) would pollute chained credential stores with a
    // description that differs from what git actually asked about.
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
    fn resolve_ambiguous_bare_candidate_prefers_default_owner() {
        // Two owners share the basename: the default-owner row (coord's own
        // legacy mapping for bare slugs) wins — never first-match order.
        let repos = vec![
            "fork-org/tools".to_string(),
            "qontinui/tools".to_string(),
        ];
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
        assert_eq!(normalize_registry_slug("qontinui/repo.git"), "qontinui/repo");
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
