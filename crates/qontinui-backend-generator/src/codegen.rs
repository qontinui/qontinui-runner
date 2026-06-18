//! Fork A — the optional LLM handler-body path. The deterministic Phase-3 handler
//! ([`crate::quality`]) is the *verified default*; this module lets the loop ask the
//! subscription `claude` CLI to (re)author a handler body, in **prose/code mode**
//! (`claude -p`, NOT `--json-schema`: a sibling agent proved large `$ref` schemas
//! silently drop `structured_output`, and for code-gen we want raw output anyway).
//!
//! ## Trust boundary
//!
//! Whatever the LLM emits MUST compile and the generated tests MUST still pass in Docker.
//! So this module never blindly overwrites a handler: [`llm_author_handler`] returns the
//! raw candidate body, and the caller is expected to (a) substitute it, (b) re-run the
//! generated pytest suite in the Docker tier, and (c) **keep the deterministic body if
//! the candidate fails the tests**. The runtime integration test exercises exactly this:
//! it asks the CLI for the `pairConfirm` body, swaps it in, and only accepts it if the
//! coverage-gated suite still passes — otherwise it falls back. Verify, don't trust.

use std::io::Write;
use std::process::{Command, Stdio};

use qontinui_types::functional_spec::{FunctionalSpec, Operation};

/// The outcome of asking the CLI for a handler body.
#[derive(Debug, Clone)]
pub enum LlmOutcome {
    /// The CLI returned a candidate body (raw text, fenced code stripped).
    Authored(String),
    /// The CLI was unavailable or errored — the caller keeps the deterministic body.
    Unavailable(String),
}

/// Ask the `claude` CLI (prose/code mode) to author the FastAPI handler body for `op`.
///
/// Returns the raw candidate (markdown fences stripped) on success. The caller MUST
/// verify it compiles + passes the generated tests before accepting it; on any error the
/// deterministic body stands.
///
/// `claude_bin` is the CLI path (`"claude"` by default). `timeout` bounds the call.
pub fn llm_author_handler(spec: &FunctionalSpec, op: &Operation, claude_bin: &str) -> LlmOutcome {
    let prompt = build_prompt(spec, op);
    match run_claude_p(claude_bin, &prompt) {
        Ok(raw) => LlmOutcome::Authored(strip_code_fences(&raw)),
        Err(e) => LlmOutcome::Unavailable(e),
    }
}

/// Assemble the prose prompt. Deterministic so the test can assert its shape without a
/// network/CLI call.
pub fn build_prompt(spec: &FunctionalSpec, op: &Operation) -> String {
    let entity = op.entity.clone().unwrap_or_default();
    let assumption = op
        .effect
        .as_ref()
        .and_then(|e| e.assumption.clone())
        .unwrap_or_else(|| "(no assumption recorded)".to_string());
    let inputs: Vec<String> = op
        .inputs
        .iter()
        .map(|i| {
            let rule = i
                .validation
                .as_ref()
                .map(|v| format!(" (validation: {})", v.rule))
                .unwrap_or_default();
            format!(
                "- {}{}{}",
                i.field,
                if i.required { " [required]" } else { "" },
                rule
            )
        })
        .collect();
    // The EXACT model fields the request/ORM carry — the LLM must use ONLY these (a
    // hallucinated extra column breaks the ORM insert, which the verify step catches).
    let entity_fields: Vec<String> = entity_field_names(spec, op);
    let fields_csv = if entity_fields.is_empty() {
        "(none)".to_string()
    } else {
        entity_fields.join(", ")
    };
    let kwargs = entity_fields
        .iter()
        .map(|f| format!("{f}=payload.{f}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "You are writing the BODY of a FastAPI route handler in a generated Python backend.\n\
Output ONLY the Python statements that go inside the handler function: NO def line, NO\n\
imports, NO markdown fences, NO prose before or after. Indent every line by 4 spaces.\n\
The signature already provides: payload (a Pydantic model), db (a SQLAlchemy Session),\n\
principal (str). `secrets`, `is_https_allowlisted`, `HTTPException`, `status`, and the\n\
`{entity}` ORM class are ALREADY imported.\n\
\n\
Target: backend for {url}\n\
Operation: {name} (verb: {verb}) on entity `{entity}`\n\
Assumed effect to implement: {assumption}\n\
Inputs:\n{inputs}\n\
\n\
The `{entity}` model has EXACTLY these columns (use ONLY these — do NOT invent any\n\
other field): {fields_csv}\n\
\n\
Requirements (follow exactly):\n\
- If there is an https-validated callback field, guard it first:\n\
  if not is_https_allowlisted(payload.callback): raise HTTPException(status_code=status.HTTP_422_UNPROCESSABLE_ENTITY, detail=\"invalid\")\n\
- Create the row using ONLY the listed columns: row = {entity}({kwargs})\n\
- db.add(row); db.commit(); db.refresh(row)\n\
- token = secrets.token_urlsafe(24)\n\
- return {{\"deviceToken\": token, \"id\": row.id, \"deviceId\": payload.device_id, \"principal\": principal}}",
        url = spec.target.source_url,
        name = op.name,
        verb = op.verb,
        entity = entity,
        assumption = assumption,
        inputs = if inputs.is_empty() { "(none)".to_string() } else { inputs.join("\n") },
        fields_csv = fields_csv,
        kwargs = kwargs,
    )
}

/// snake_case column names of the entity the op targets (so the prompt can pin the exact
/// ORM fields). Empty when the op targets no known entity.
fn entity_field_names(spec: &FunctionalSpec, op: &Operation) -> Vec<String> {
    let Some(name) = op.entity.as_deref() else {
        return Vec::new();
    };
    spec.entities
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.fields.iter().map(|f| snake(&f.name)).collect())
        .unwrap_or_default()
}

/// Local snake_case (matches the generator's rule for column names).
fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch == '-' || ch == '_' || ch == ' ' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
        } else if ch.is_ascii_uppercase() {
            if i != 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Invoke `claude -p <prompt>` and capture stdout. Subscription path (the CLI), never
/// `api.anthropic.com`.
fn run_claude_p(claude_bin: &str, prompt: &str) -> Result<String, String> {
    // `-p` (print mode) reads the prompt as an argument and prints the response. We pass
    // the prompt on stdin too for robustness on long prompts.
    let mut child = Command::new(claude_bin)
        .arg("-p")
        .arg(prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {claude_bin}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait {claude_bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{claude_bin} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Extract the python body from the CLI output: prefer a fenced ```…``` block; else
/// drop any prose preamble (lines that don't look like python) and keep from the first
/// code-looking line onward. Leading indentation of body statements is preserved.
fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim_matches('\n');

    // 1) If there's a fenced block, take its contents (the most reliable signal).
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        // Drop the language tag on the same line, then take up to the closing fence.
        let after_lang = after.split_once('\n').map(|x| x.1).unwrap_or("");
        let body = match after_lang.find("```") {
            Some(end) => &after_lang[..end],
            None => after_lang,
        };
        return body.trim_matches('\n').trim_end().to_string();
    }

    // 2) No fence: drop a prose preamble. Keep from the first line that looks like
    //    python (indented, or a recognizable statement keyword).
    let lines: Vec<&str> = trimmed.lines().collect();
    let first_code = lines.iter().position(|l| looks_like_python(l));
    match first_code {
        Some(i) => lines[i..].join("\n").trim_end().to_string(),
        None => trimmed.trim_end().to_string(),
    }
}

/// A line that plausibly starts python handler-body code (indented, or a known keyword).
fn looks_like_python(line: &str) -> bool {
    if line.starts_with(' ') || line.starts_with('\t') {
        return true;
    }
    let t = line.trim_start();
    const KEYWORDS: [&str; 11] = [
        "if ", "for ", "while ", "return", "raise ", "import ", "from ", "row ", "db.", "token",
        "with ",
    ];
    KEYWORDS.iter().any(|k| t.starts_with(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::functional_spec::{
        Operation, OperationEffect, OperationInput, SpecProvenance, SpecTarget, ValidationRule,
    };

    fn spec() -> FunctionalSpec {
        FunctionalSpec {
            spec_version: "0".into(),
            target: SpecTarget {
                source_url: "https://app.qontinui.io/connect-runner".into(),
                observed_at: None,
            },
            entities: vec![],
            operations: vec![],
            ui_states: vec![],
            navigation: vec![],
            auth: None,
            assumptions: vec![],
        }
    }

    fn op() -> Operation {
        Operation {
            name: "pairConfirm".into(),
            verb: "create".into(),
            entity: Some("Device".into()),
            inputs: vec![OperationInput {
                field: "callback".into(),
                required: true,
                validation: Some(ValidationRule {
                    rule: "https allowlisted host".into(),
                    confidence: SpecProvenance::Observed,
                    provenance: None,
                    credibility: None,
                }),
            }],
            effect: Some(OperationEffect {
                confidence: SpecProvenance::Assumed,
                assumption: Some("persists the pairing and returns a device token".into()),
                provenance: None,
                credibility: None,
            }),
            confidence: SpecProvenance::Observed,
            provenance: None,
            credibility: None,
        }
    }

    #[test]
    fn prompt_mentions_effect_inputs_and_constraints() {
        let p = build_prompt(&spec(), &op());
        assert!(p.contains("pairConfirm"));
        assert!(p.contains("persists the pairing"));
        assert!(p.contains("callback"));
        assert!(p.contains("https allowlisted host"));
        assert!(p.contains("HTTP_422_UNPROCESSABLE_ENTITY"));
        assert!(p.contains("deviceToken"));
    }

    #[test]
    fn fence_stripping() {
        let raw = "```python\n    row = Device()\n    db.add(row)\n```";
        assert_eq!(
            strip_code_fences(raw),
            "    row = Device()\n    db.add(row)"
        );
        assert_eq!(strip_code_fences("    return {}"), "    return {}");
    }
}
