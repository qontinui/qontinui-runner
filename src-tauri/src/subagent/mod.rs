//! Subagent analysis dispatch (plan 2026-07-15-runner-pi-deepseek-subagent-analysis, Phase 4).
//!
//! Offloads an analysis task from the main Claude session to a cheaper/faster
//! subagent and returns the analysis TEXT — the caller (an MCP tool result, a
//! workflow step, a Tauri command) injects it into whatever context needs it.
//!
//! Two provider paths, deliberately asymmetric (D1):
//! - **Pi** (agentic): stages `file_refs` copies into a fresh temp dir and
//!   runs the pi CLI with `cwd` there and a read-only tool allowlist
//!   (`read,grep,find,ls`) — pi explores the staged files itself. Copies
//!   mean pi can never touch the originals.
//! - **Deepseek** (one-shot API): reads `file_refs` contents, composes a
//!   single prompt (the `run_analysis` pattern from
//!   `commands/terminal_analysis.rs`), and dispatches through
//!   `run_prompt_with_model_override` with `provider_override =
//!   "openai_compatible"` — inheriting retry + circuit breaker.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Per-file and total content budgets for the Deepseek (inline-content) path.
/// DeepSeek's context window is large but not unbounded; oversized refs fail
/// fast with a clear error instead of a confusing API-side truncation/400.
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024;

/// Default wall-clock budget for a subagent run.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentProvider {
    /// pi coding agent CLI (agentic: explores staged files with its tools).
    Pi,
    /// DeepSeek via the OpenAI-compatible API client (one-shot: file
    /// contents are inlined into the prompt).
    Deepseek,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentAnalysisRequest {
    pub provider: SubagentProvider,
    /// The analysis question/instruction.
    pub prompt: String,
    /// Absolute paths of files to analyze. Pi gets read-only staged COPIES;
    /// Deepseek gets the contents inlined.
    #[serde(default)]
    pub file_refs: Vec<String>,
    /// Wall-clock budget; default 300s.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("file ref rejected: {0}")]
    BadFileRef(String),
    #[error("staging failed: {0}")]
    Staging(#[from] std::io::Error),
    #[error("subagent analysis failed: {0}")]
    Analysis(String),
}

/// Run an analysis on a subagent and return the analysis text.
///
/// Blocking (spawns a subprocess or a blocking HTTP call) — async callers
/// must wrap in `spawn_blocking`.
pub fn execute_analysis(req: &SubagentAnalysisRequest) -> Result<String, SubagentError> {
    let files = validate_file_refs(&req.file_refs)?;
    let timeout = req.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    info!(
        "subagent analysis: provider={:?}, files={}, timeout={}s, prompt_chars={}",
        req.provider,
        files.len(),
        timeout,
        req.prompt.len()
    );
    match req.provider {
        SubagentProvider::Pi => execute_pi(req, &files, timeout),
        SubagentProvider::Deepseek => execute_deepseek(req, &files, timeout),
    }
}

/// Validate + canonicalize file refs. Every ref must be an absolute path to
/// an existing regular file (relative paths would silently depend on the
/// runner's cwd, which is never what the caller means).
fn validate_file_refs(refs: &[String]) -> Result<Vec<PathBuf>, SubagentError> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let p = PathBuf::from(r);
        if !p.is_absolute() {
            return Err(SubagentError::BadFileRef(format!(
                "{} is not an absolute path",
                r
            )));
        }
        if !p.is_file() {
            return Err(SubagentError::BadFileRef(format!(
                "{} does not exist or is not a regular file",
                r
            )));
        }
        out.push(p);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pi path (agentic, temp-dir staging)
// ---------------------------------------------------------------------------

fn execute_pi(
    req: &SubagentAnalysisRequest,
    files: &[PathBuf],
    timeout: u64,
) -> Result<String, SubagentError> {
    // TempDir cleans up on drop (including the error paths below).
    let staging = tempfile::Builder::new()
        .prefix("qontinui-subagent-")
        .tempdir()?;

    let staged_names = stage_copies(files, staging.path())?;

    let mut settings = crate::settings::get_ai_settings().pi_cli;
    // Read-only tool allowlist: the staged dir is pi's whole world, and pi
    // must not modify even the copies (probe-verified flag, plan Risks).
    settings.tools = Some("read,grep,find,ls".to_string());
    settings.timeout_seconds = timeout;

    let prompt = compose_pi_prompt(&req.prompt, &staged_names);
    let response = crate::ai_provider::pi_cli::run_pi_cli_in_dir(
        &prompt,
        &settings,
        None,
        Some(staging.path()),
        None,
    );

    if response.success {
        Ok(response.output)
    } else {
        Err(SubagentError::Analysis(response.error.unwrap_or_else(
            || "pi returned no error detail".to_string(),
        )))
    }
}

/// Copy `files` into `dir`, disambiguating basename collisions with a
/// numeric prefix (`2-name.rs`). Returns the staged file names in input
/// order.
fn stage_copies(files: &[PathBuf], dir: &Path) -> Result<Vec<String>, SubagentError> {
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    for f in files {
        let base = f
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                SubagentError::BadFileRef(format!("{} has no usable file name", f.display()))
            })?
            .to_string();
        let mut name = base.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{}-{}", n, base);
            n += 1;
        }
        std::fs::copy(f, dir.join(&name))?;
        names.push(name);
    }
    Ok(names)
}

fn compose_pi_prompt(prompt: &str, staged_names: &[String]) -> String {
    if staged_names.is_empty() {
        return prompt.to_string();
    }
    let listing = staged_names
        .iter()
        .map(|n| format!("- {}", n))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n\nThe files to analyze are staged in the current directory:\n{}",
        prompt, listing
    )
}

// ---------------------------------------------------------------------------
// Deepseek path (one-shot, inline contents)
// ---------------------------------------------------------------------------

fn execute_deepseek(
    req: &SubagentAnalysisRequest,
    files: &[PathBuf],
    timeout: u64,
) -> Result<String, SubagentError> {
    let mut sections: Vec<(String, String)> = Vec::with_capacity(files.len());
    let mut total: u64 = 0;
    for f in files {
        let meta = std::fs::metadata(f)?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(SubagentError::BadFileRef(format!(
                "{} is {} bytes — over the {}KB per-file limit for inline analysis; \
                 use the pi provider for large files",
                f.display(),
                meta.len(),
                MAX_FILE_BYTES / 1024
            )));
        }
        total += meta.len();
        if total > MAX_TOTAL_BYTES {
            return Err(SubagentError::BadFileRef(format!(
                "file refs total more than {}KB — over the inline-analysis budget; \
                 use the pi provider or fewer files",
                MAX_TOTAL_BYTES / 1024
            )));
        }
        let mut content = String::new();
        std::fs::File::open(f)?
            .read_to_string(&mut content)
            .map_err(|e| {
                SubagentError::BadFileRef(format!(
                    "{} is not valid UTF-8 text ({}); inline analysis handles text files only",
                    f.display(),
                    e
                ))
            })?;
        sections.push((f.display().to_string(), content));
    }

    let prompt = compose_inline_prompt(&req.prompt, &sections);

    // Explicit model + provider: `run_prompt_with_overrides_inner` only
    // honors `provider_override` when `model_override` is present.
    let ai_settings = crate::settings::get_ai_settings();
    let model = ai_settings.openai_compatible.model.clone();
    if timeout != ai_settings.openai_compatible.timeout_seconds {
        // The OpenAI-compatible client reads its timeout from settings; a
        // per-request override would need a settings clone through routing,
        // which routing doesn't model. Log instead of silently ignoring.
        warn!(
            "subagent deepseek path uses the configured openai_compatible timeout ({}s), \
             not the requested {}s",
            ai_settings.openai_compatible.timeout_seconds, timeout
        );
    }

    let response = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &crate::ai_router::TaskContext::default(),
        None,
        Some(&model),
        Some("openai_compatible"),
        None,
        None,
        None,
        None,
    );

    if response.success {
        Ok(response.output)
    } else {
        Err(SubagentError::Analysis(response.error.unwrap_or_else(
            || "DeepSeek returned no error detail".to_string(),
        )))
    }
}

/// Compose the one-shot prompt: instruction first, then each file delimited
/// exactly like `run_analysis` (`commands/terminal_analysis.rs`).
fn compose_inline_prompt(prompt: &str, sections: &[(String, String)]) -> String {
    let mut out = String::with_capacity(
        prompt.len()
            + sections
                .iter()
                .map(|(n, c)| n.len() + c.len() + 32)
                .sum::<usize>(),
    );
    out.push_str(prompt);
    for (name, content) in sections {
        out.push_str(&format!("\n\n--- file: {} ---\n{}", name, content));
    }
    out
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn relative_file_ref_is_rejected() {
        let err = validate_file_refs(&["not/absolute.txt".to_string()]).unwrap_err();
        assert!(matches!(err, SubagentError::BadFileRef(_)));
    }

    #[test]
    fn missing_file_ref_is_rejected() {
        let missing = std::env::temp_dir().join("qontinui-subagent-definitely-missing.txt");
        let err = validate_file_refs(&[missing.display().to_string()]).unwrap_err();
        assert!(matches!(err, SubagentError::BadFileRef(_)));
    }

    #[test]
    fn stage_copies_disambiguates_basename_collisions() {
        let src_a = tempfile::Builder::new().tempdir().unwrap();
        let src_b = tempfile::Builder::new().tempdir().unwrap();
        for d in [&src_a, &src_b] {
            let mut f = std::fs::File::create(d.path().join("same.txt")).unwrap();
            writeln!(f, "content in {}", d.path().display()).unwrap();
        }
        let dst = tempfile::Builder::new().tempdir().unwrap();

        let names = stage_copies(
            &[src_a.path().join("same.txt"), src_b.path().join("same.txt")],
            dst.path(),
        )
        .unwrap();

        assert_eq!(
            names,
            vec!["same.txt".to_string(), "2-same.txt".to_string()]
        );
        assert!(dst.path().join("same.txt").is_file());
        assert!(dst.path().join("2-same.txt").is_file());
    }

    #[test]
    fn pi_prompt_lists_staged_names() {
        let p = compose_pi_prompt("review these", &["a.rs".into(), "2-a.rs".into()]);
        assert!(p.starts_with("review these"));
        assert!(p.contains("- a.rs"));
        assert!(p.contains("- 2-a.rs"));
    }

    #[test]
    fn pi_prompt_without_files_is_unchanged() {
        assert_eq!(compose_pi_prompt("just think", &[]), "just think");
    }

    /// Manual E2E for the Phase 4 gate (requires pi installed + a DeepSeek
    /// key). Stages two real files and runs the full pi path: staging →
    /// read-only pi exploration → analysis text back. Run:
    /// `cargo test --bin qontinui-runner subagent_pi_manual_e2e -- --ignored --nocapture`
    #[test]
    #[ignore = "requires pi CLI + DeepSeek key on the box; run manually"]
    fn subagent_pi_manual_e2e() {
        let dir = tempfile::Builder::new().tempdir().unwrap();
        let alpha = dir.path().join("alpha.txt");
        let beta = dir.path().join("beta.txt");
        std::fs::write(&alpha, "the quick brown fox jumps over the lazy dog\n").unwrap();
        std::fs::write(&beta, "rust runner spawns pi as a subagent for analysis\n").unwrap();

        let req = SubagentAnalysisRequest {
            provider: SubagentProvider::Pi,
            prompt: "Summarize each staged file in one line, naming the file.".to_string(),
            file_refs: vec![alpha.display().to_string(), beta.display().to_string()],
            timeout_secs: Some(180),
        };

        let text = execute_analysis(&req).expect("pi subagent analysis");
        println!("--- pi analysis ---\n{}", text);
        assert!(
            text.contains("alpha") && text.contains("beta"),
            "analysis should reference both staged files: {}",
            text
        );
    }

    /// Manual E2E for the DeepSeek inline path. Run:
    /// `cargo test --bin qontinui-runner subagent_deepseek_manual_e2e -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a DeepSeek key on the box; run manually"]
    fn subagent_deepseek_manual_e2e() {
        let dir = tempfile::Builder::new().tempdir().unwrap();
        let f = dir.path().join("fact.txt");
        std::fs::write(&f, "the magic word is PINEAPPLE\n").unwrap();

        let req = SubagentAnalysisRequest {
            provider: SubagentProvider::Deepseek,
            prompt: "What is the magic word in the attached file? Answer with just the word."
                .to_string(),
            file_refs: vec![f.display().to_string()],
            timeout_secs: Some(120),
        };

        let text = execute_analysis(&req).expect("deepseek subagent analysis");
        println!("--- deepseek analysis ---\n{}", text);
        assert!(
            text.to_uppercase().contains("PINEAPPLE"),
            "analysis should quote the magic word: {}",
            text
        );
    }

    #[test]
    fn inline_prompt_delimits_files_like_run_analysis() {
        let p = compose_inline_prompt(
            "summarize",
            &[("D:\\x\\a.rs".to_string(), "fn main() {}".to_string())],
        );
        assert!(p.starts_with("summarize"));
        assert!(p.contains("--- file: D:\\x\\a.rs ---\nfn main() {}"));
    }
}
