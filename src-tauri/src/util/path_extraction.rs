//! Path-token extraction from arbitrary text (prompt or worker output).
//!
//! This module hosts the canonical implementation of "given a chunk of text,
//! pull out things that look like file paths." It originated as a private
//! `extract_file_paths` helper inside
//! `unified_workflow_executor/agentic_verification_loop.rs` and was promoted
//! here so the launch-time predictive-conflict probe (see plan
//! `predictive-conflict-warning-plan.md` Phase 1) can share the same
//! extractor without duplicating the heuristic.
//!
//! Two consumers, two configurations:
//!   - **Verification loop** (original caller): passes `cwd=""`. Behavior is
//!     identical to the original helper — relative tokens stay relative,
//!     no path resolution.
//!   - **Conflict probe** (new caller): passes the launch cwd. Relative
//!     tokens are resolved against that cwd before normalization, so the
//!     extracted candidates can be compared symmetrically against
//!     `FileRegistryManager` keys (which the registry also normalizes).
//!
//! Output is always normalized via
//! [`crate::executor::file_registry::normalize_path`] so callers downstream
//! never have to re-normalize. On Windows this lowercases as well — the
//! registry uses the same normalization, so the symmetry holds.

use std::path::Path;

use crate::executor::file_registry::normalize_path;

/// Substrings that, if present in a normalized path, disqualify it as a
/// candidate. These are the noise sources that dominate false positives in
/// long natural-language prompts: dependency trees, build artifacts,
/// version-control internals.
///
/// The blacklist is checked **after** normalization (lowercased on Windows,
/// always forward-slashed) so the trailing slash is the canonical token
/// boundary — `node_modules/x.js` matches but a (very unlikely)
/// `my_node_modules.txt` does not.
const PATH_BLACKLIST: &[&str] = &["node_modules/", "target/", ".git/", "dist/", "build/"];

/// Maximum number of candidates returned. Raised from the verification
/// loop's original cap of 10 to give the launch probe headroom for long
/// natural-language prompts. Past 50, we are almost certainly extracting
/// noise rather than meaningful collision targets.
const MAX_CANDIDATES: usize = 50;

/// Extract candidate file paths from `prompt`, optionally resolved against
/// `cwd`.
///
/// # Parameters
/// - `prompt`: text to scan. `None` short-circuits to an empty vector — the
///   conflict-probe HTTP handler relies on this for the no-prompt-yet case.
/// - `cwd`: working directory to resolve relative tokens against. Pass an
///   empty string to disable resolution (the verification-loop legacy
///   behavior). When non-empty, every relative token is joined onto `cwd`
///   before normalization. Absolute tokens (Unix `/foo` or Windows
///   `C:\foo` / `D:/foo`) pass through `cwd` resolution unchanged.
///
/// # Heuristic
/// 1. Whitespace-tokenize the prompt.
/// 2. Strip surrounding punctuation (` ` ' " , `).
/// 3. Reject tokens that don't look path-like: must contain a `/` or `\`,
///    contain a `.`, be 5..=199 chars, and not start with `http` or `//`.
/// 4. If the token is longer than 60 chars after slash normalization,
///    keep only the trailing 3 segments. This was the original helper's
///    way of trimming pathological one-line dumps without losing the
///    file identity.
/// 5. If `cwd` is non-empty AND the token is relative (no leading `/`,
///    no `<letter>:/` drive prefix), join it onto `cwd`.
/// 6. Apply [`normalize_path`].
/// 7. Drop the result if it contains any [`PATH_BLACKLIST`] substring.
/// 8. Deduplicate; cap at [`MAX_CANDIDATES`].
pub fn extract_candidate_paths(prompt: Option<&str>, cwd: &str) -> Vec<String> {
    let Some(text) = prompt else {
        return Vec::new();
    };

    let mut paths: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ',');

        // Match patterns like src/foo/bar.rs, ./components/App.tsx, etc.
        if !((cleaned.contains('/') || cleaned.contains('\\'))
            && cleaned.contains('.')
            && cleaned.len() > 4
            && cleaned.len() < 200
            && !cleaned.starts_with("http")
            && !cleaned.starts_with("//"))
        {
            continue;
        }

        // Forward-slash normalize (preserves the original helper's behavior
        // before we hand off to `normalize_path`, which on Windows also
        // lowercases).
        let slashed = cleaned.replace('\\', "/");

        // Trim pathologically long tokens to their last 3 segments — this
        // mirrors the original helper.
        let trimmed: String = if slashed.len() > 60 {
            slashed
                .rsplit('/')
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/")
        } else {
            slashed
        };

        // Resolve relative tokens against `cwd` when one is provided.
        // Absolute tokens (Unix `/x` or Windows drive-rooted `<letter>:/x`)
        // bypass resolution.
        let resolved = if cwd.is_empty() || is_absolute_token(&trimmed) {
            trimmed
        } else {
            let joined = Path::new(cwd).join(&trimmed);
            joined.to_string_lossy().replace('\\', "/")
        };

        let normalized = normalize_path(&resolved);

        // Blacklist applies post-normalization so the substring match sees
        // the canonical (slashed, possibly lowercased) form.
        if PATH_BLACKLIST
            .iter()
            .any(|needle| normalized.contains(needle))
        {
            continue;
        }

        if !paths.contains(&normalized) {
            paths.push(normalized);
        }

        if paths.len() >= MAX_CANDIDATES {
            break;
        }
    }
    paths
}

/// Return true if `token` (already forward-slashed) looks like an absolute
/// path: either Unix-style leading `/` or a Windows-style drive prefix
/// (`C:/`, `D:/`, etc.). Used to short-circuit cwd resolution.
fn is_absolute_token(token: &str) -> bool {
    if token.starts_with('/') {
        return true;
    }
    // Windows drive prefix: `<letter>:/...`
    let bytes = token.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' {
        let c = bytes[0] as char;
        if c.is_ascii_alphabetic() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tests promoted from agentic_verification_loop.rs (cwd="" preserves
    //    the original behavior for the verification-loop callsite).
    //    Comparisons use `normalize_path` to stay portable across Linux
    //    (case-preserving) and Windows (lowercasing) — the post-normalization
    //    form is what callers actually receive. ─────────────────────────

    #[test]
    fn test_extract_paths_basic() {
        let text = "Modified src/main.rs and src/lib.rs successfully";
        let paths = extract_candidate_paths(Some(text), "");
        assert!(paths.contains(&normalize_path("src/main.rs")));
        assert!(paths.contains(&normalize_path("src/lib.rs")));
    }

    #[test]
    fn test_extract_paths_backslash() {
        let text = "Edited src\\components\\App.tsx";
        let paths = extract_candidate_paths(Some(text), "");
        assert!(paths.contains(&normalize_path("src/components/App.tsx")));
    }

    #[test]
    fn test_extract_paths_ignores_urls() {
        let text = "See https://example.com/foo/bar.html for details";
        let paths = extract_candidate_paths(Some(text), "");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_paths_ignores_comments() {
        let text = "// this is a comment without any paths";
        let paths = extract_candidate_paths(Some(text), "");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_extract_paths_empty() {
        assert!(extract_candidate_paths(Some(""), "").is_empty());
    }

    #[test]
    fn test_extract_paths_deduplicates() {
        let text = "Edit src/main.rs then src/main.rs again";
        let paths = extract_candidate_paths(Some(text), "");
        assert_eq!(
            paths
                .iter()
                .filter(|p| **p == normalize_path("src/main.rs"))
                .count(),
            1
        );
    }

    #[test]
    fn test_extract_paths_caps_at_50() {
        // The new cap is 50 (was 10). Generate 60 distinct path tokens and
        // verify the cap holds.
        let text = (0..60)
            .map(|i| format!("src/file{}.rs", i))
            .collect::<Vec<_>>()
            .join(" ");
        let paths = extract_candidate_paths(Some(&text), "");
        assert!(paths.len() <= MAX_CANDIDATES);
        assert_eq!(paths.len(), MAX_CANDIDATES);
    }

    #[test]
    fn test_extract_paths_strips_quotes() {
        let text = r#"Changed `src/foo.rs` and "src/bar.rs""#;
        let paths = extract_candidate_paths(Some(text), "");
        assert!(paths.contains(&normalize_path("src/foo.rs")));
        assert!(paths.contains(&normalize_path("src/bar.rs")));
    }

    // ── New tests for the extended (cwd + blacklist + None) behavior. ─

    #[test]
    fn test_relative_token_resolved_against_cwd() {
        let paths = extract_candidate_paths(Some("edit src/foo/bar.rs"), "/repo");
        assert_eq!(paths, vec![normalize_path("/repo/src/foo/bar.rs")]);
    }

    #[test]
    fn test_natural_language_no_paths() {
        let paths = extract_candidate_paths(Some("refactor the auth module"), "/repo");
        assert!(
            paths.is_empty(),
            "natural-language text should yield no paths, got {:?}",
            paths
        );
    }

    #[test]
    fn test_absolute_windows_path_passes_through() {
        let paths = extract_candidate_paths(Some(r"D:\absolute\path.rs"), "/ignored-cwd");
        assert_eq!(paths.len(), 1, "expected one path, got {:?}", paths);
        // Windows lowercases via normalize_path; Linux preserves case.
        assert_eq!(paths[0], normalize_path("D:/absolute/path.rs"));
    }

    #[test]
    fn test_blacklist_drops_node_modules_and_caps_total() {
        // Build a 200-line prompt with one node_modules mention and many
        // legitimate path mentions. Verify the node_modules mention is
        // filtered out and the total is capped at 50.
        let mut lines = vec!["Found a bug in node_modules/foo.js bundle".to_string()];
        for i in 0..199 {
            lines.push(format!("Touched src/feature_{}/handler.rs today", i));
        }
        let prompt = lines.join("\n");

        let paths = extract_candidate_paths(Some(&prompt), "/repo");

        assert!(
            paths.len() <= MAX_CANDIDATES,
            "expected ≤{} paths, got {}",
            MAX_CANDIDATES,
            paths.len()
        );
        assert!(
            !paths.iter().any(|p| p.contains("node_modules/")),
            "node_modules/ should be blacklisted, got {:?}",
            paths
        );
    }

    #[test]
    fn test_none_prompt_returns_empty() {
        assert!(extract_candidate_paths(None, "/repo").is_empty());
        assert!(extract_candidate_paths(None, "").is_empty());
    }

    // ── Additional coverage for blacklist completeness and absolute-token
    //    detection edge cases. ────────────────────────────────────────────

    #[test]
    fn test_blacklist_covers_target_git_dist_build() {
        let text =
            "saw target/debug/foo.rs and .git/objects/x.pack and dist/bundle.js and build/out.o";
        let paths = extract_candidate_paths(Some(text), "");
        assert!(
            paths.is_empty(),
            "all four blacklisted prefixes should be filtered, got {:?}",
            paths
        );
    }

    #[test]
    fn test_unix_absolute_path_skips_cwd_resolution() {
        let paths = extract_candidate_paths(Some("/etc/hosts.conf"), "/repo");
        assert_eq!(paths, vec![normalize_path("/etc/hosts.conf")]);
    }
}
