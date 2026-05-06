//! Phase 3 — promote `/decompose-plan` to an in-product Rust feature.
//!
//! Mirrors the personal slash command at
//! `qontinui-claude-config/.claude/commands/decompose-plan.md` so any
//! qontinui user can decompose a plan markdown into a structured task graph
//! without depending on the Claude CLI or the personal config repo.
//!
//! High-level flow (matches the slash command's §1–§7):
//!
//! 1. Read the plan markdown from disk + compute SHA-256.
//! 2. Idempotency: if the plan is already at this hash, short-circuit.
//! 3. Use [`OneshotLlm`] to identify phases + tasks and infer
//!    `expected_file_claims` / `depends_on_indices` (the LLM-inference work
//!    that replaces the slash command's prompted body).
//! 4. POST the structured payload to the runner's `/plans/decompose`
//!    endpoint (the existing transactional inserter at
//!    `mcp/plans.rs:84`). We deliberately do not duplicate that logic.
//! 5. Stamp the markdown with `<!-- decomposed: <hash> at <timestamp> -->`
//!    so the next invocation's hash check short-circuits.
//!
//! See `plans/productivity-stack-product-readiness.md` Phase 3 for the
//! design notes.

use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::ai_provider::oneshot::{OneshotError, OneshotLlm, OneshotLlmExt};
use crate::database::pg::PgDb;

// =============================================================================
// LLM response schema
// =============================================================================

/// Top-level LLM response: a plan summary plus the full decomposed task
/// list. The LLM round-trips through [`OneshotLlmExt::call_typed`] which
/// auto-derives the JSON schema from this struct via `schemars`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DecomposedTasks {
    pub plan_summary: PlanSummary,
    pub tasks: Vec<DecomposedTask>,
}

/// Plan-level metadata extracted from the markdown's H1 + first paragraph.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PlanSummary {
    pub title: String,
    pub summary: String,
}

/// One decomposed task. `depends_on_indices` are positions into the parent
/// `DecomposedTasks::tasks` array — the runner's `/plans/decompose` endpoint
/// resolves them to UUIDs in a single transaction.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DecomposedTask {
    pub phase_name: String,
    pub sequence_in_phase: i32,
    pub description: String,
    #[serde(default)]
    pub expected_file_claims: Vec<String>,
    #[serde(default)]
    pub expected_dirs: Vec<String>,
    #[serde(default)]
    pub depends_on_indices: Vec<usize>,
    #[serde(default)]
    pub notes: Option<String>,
}

// =============================================================================
// Public API
// =============================================================================

/// Public result returned by [`decompose_plan_in_product`] / the Tauri
/// command. Includes everything the UI needs to render a success toast +
/// link.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecomposeResult {
    pub plan_id: String,
    pub task_count: usize,
    pub version_hash: String,
    /// True when the plan was already decomposed at this hash and we
    /// short-circuited without calling the LLM.
    pub idempotent_skip: bool,
    /// True when we appended a `<!-- decomposed: -->` stamp to the plan
    /// markdown. False when the stamp was already present.
    pub stamped: bool,
}

/// Errors surfaced by [`decompose_plan_in_product`]. The Tauri command maps
/// these to UI hint strings.
#[derive(Debug)]
pub enum DecomposeError {
    PlanNotFound(String),
    PlanRead(String),
    LlmDisabled,
    LlmNoCredentials,
    LlmCallFailed(String),
    LlmSchemaParse(String),
    EmptyDecomposition,
    HttpPost(String),
    StampWrite(String),
}

impl std::fmt::Display for DecomposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecomposeError::PlanNotFound(p) => write!(f, "plan markdown not found: {}", p),
            DecomposeError::PlanRead(e) => write!(f, "failed to read plan markdown: {}", e),
            DecomposeError::LlmDisabled => {
                write!(f, "no LLM provider is configured (Settings -> AI)")
            }
            DecomposeError::LlmNoCredentials => {
                write!(f, "the active LLM provider has no credentials configured")
            }
            DecomposeError::LlmCallFailed(e) => write!(f, "LLM call failed: {}", e),
            DecomposeError::LlmSchemaParse(e) => {
                write!(f, "LLM response did not match expected schema: {}", e)
            }
            DecomposeError::EmptyDecomposition => write!(
                f,
                "LLM produced zero tasks; the plan likely needs more structure"
            ),
            DecomposeError::HttpPost(e) => write!(f, "POST /plans/decompose failed: {}", e),
            DecomposeError::StampWrite(e) => write!(f, "failed to stamp plan markdown: {}", e),
        }
    }
}

impl std::error::Error for DecomposeError {}

impl From<OneshotError> for DecomposeError {
    fn from(e: OneshotError) -> Self {
        match e {
            OneshotError::NoCredentials => DecomposeError::LlmNoCredentials,
            OneshotError::Disabled => DecomposeError::LlmDisabled,
            OneshotError::SchemaParse(msg) => DecomposeError::LlmSchemaParse(msg),
            OneshotError::ProviderTransport(msg) => DecomposeError::LlmCallFailed(msg),
            OneshotError::BudgetExhausted => {
                DecomposeError::LlmCallFailed("budget exhausted".to_string())
            }
        }
    }
}

// =============================================================================
// System prompt — mirrors the slash command's §2–§4 prompt body
// =============================================================================

/// System prompt used for the LLM round-trip. Captures the slash command's
/// §2 (phase identification) + §3 (claim inference) + §4 (dependency
/// detection) rules verbatim. Kept as a `pub const` so the snapshot test
/// below can pin it.
///
/// Update history: any edit to this string requires updating the
/// `system_prompt_snapshot` test below.
pub const DECOMPOSE_SYSTEM_PROMPT: &str = "You are decomposing a software project plan markdown into a structured task graph.\n\
Your job: read the plan and emit a JSON object describing every actionable task.\n\
\n\
PHASE IDENTIFICATION (slash command rule 2):\n\
- Plans use the convention 'Phase N -- <name>' for headers; treat each such header as a phase boundary.\n\
- Each top-level bullet under a phase header is a candidate task. Multi-line bullets are one task each.\n\
- If a phase contains only prose paragraphs (no bullets), it's a narrative phase, not a task phase. Skip it.\n\
- Set sequence_in_phase to the bullet's 0-based position within its phase.\n\
\n\
CLAIM INFERENCE (slash command rule 3 -- prioritised algorithm):\n\
1. Explicit hints: if a task has 'Files to modify:' or 'Files to create:' sub-bullets, those paths ARE the claims, full stop.\n\
2. Path-shaped tokens: scan the task body for tokens that look like absolute paths (D:/..., qontinui-runner/..., paths ending .rs/.ts/.tsx/.md). Each unique path is a candidate claim.\n\
3. Function-name grounding: for any function or symbol name mentioned in the task body, if it ALREADY appears as a path-shaped token elsewhere in the same task, keep the path. If a symbol is named without a path, drop it -- you cannot grep from inside this prompt.\n\
4. Self-verification: every claim must trace back to either an explicit hint or a path-shaped token. Drop unjustified claims rather than guessing.\n\
\n\
DIRECTORIES: only include a directory in expected_dirs when the task body mentions a tree of files under that dir (e.g. 'all files under qontinui-runner/src/components/foo/'). Do not infer directories from individual file paths.\n\
\n\
DEPENDENCY DETECTION (slash command rule 4):\n\
- Task B depends on task A when (B's phase index > A's phase index) AND (B's claims overlap A's, OR B's body explicitly references symbols A creates -- 'the new FooBar from Phase 3').\n\
- Same-phase dependencies are unusual; assume parallel unless an explicit 'after the previous step' hint exists.\n\
- depends_on_indices contains the 0-based positions of prerequisite tasks in the SAME tasks array you're emitting.\n\
\n\
OUTPUT REQUIREMENTS:\n\
- plan_summary.title: extract from the plan's H1 (first '# ' line). If no H1, use the file's basename.\n\
- plan_summary.summary: extract from the first non-blank paragraph after the H1. Trim to 500 characters.\n\
- tasks: emit every actionable task across every phase, in plan order. Do NOT collapse multi-line bullets into one task.\n\
- expected_file_claims: absolute or repo-relative paths only. No glob patterns.\n\
- notes: optional free-form string for caveats ('ambiguous: <symbol>', 'narrative phase skipped: <name>').\n\
\n\
Be exhaustive. If a phase has 7 bullets, emit 7 tasks. The runner's transactional inserter validates the result; partial decomposition is not acceptable.";

// =============================================================================
// Implementation
// =============================================================================

/// Decompose a plan markdown into the in-product task graph.
///
/// See module docs for the high-level flow. Returns
/// [`DecomposeError::LlmDisabled`] / [`DecomposeError::LlmNoCredentials`]
/// when no provider is configured; the Tauri command surfaces these as
/// "configure an LLM provider in Settings -> AI" hints.
pub async fn decompose_plan_in_product(
    pg: &Arc<PgDb>,
    llm: &dyn OneshotLlm,
    plan_path: &str,
    runner_port: u16,
) -> Result<DecomposeResult, DecomposeError> {
    info!(
        "productivity.decompose.started plan_path={} provider={}",
        plan_path,
        llm.name()
    );

    // 1. Read + hash.
    let plan_text = read_plan(plan_path)?;
    let version_hash = sha256_hex(plan_text.as_bytes());
    debug!(
        "productivity.decompose.hashed plan_path={} hash={}",
        plan_path, version_hash
    );

    // 2. Idempotency: skip if the plan is already decomposed at this hash.
    //    Uses the existing `get_plan_by_path` query so we don't add a new
    //    DB API surface. (TODO: a future revision could expose a richer
    //    "is decomposed?" GET endpoint that also reports task count, per
    //    decompose-plan.md step 1.)
    if let Ok(Some(existing)) = pg.get_plan_by_path(plan_path).await {
        if existing.version_hash == version_hash {
            info!(
                "productivity.decompose.idempotent_skip plan_id={} hash={}",
                existing.id, version_hash
            );
            // Count tasks for the response so the UI can still render a
            // useful toast. Best-effort; failure → 0.
            let task_count = pg
                .list_tasks_for_plan(&existing.id)
                .await
                .map(|t| t.len())
                .unwrap_or(0);
            return Ok(DecomposeResult {
                plan_id: existing.id,
                task_count,
                version_hash,
                idempotent_skip: true,
                stamped: false,
            });
        }
    }

    // 3. LLM inference.
    let user_prompt = format!(
        "Plan markdown path: {}\n\n--- BEGIN PLAN ---\n{}\n--- END PLAN ---",
        plan_path, plan_text
    );
    let decomposed: DecomposedTasks = llm
        .call_typed::<DecomposedTasks>(DECOMPOSE_SYSTEM_PROMPT, &user_prompt)
        .await?;

    if decomposed.tasks.is_empty() {
        warn!(
            "productivity.decompose.empty plan_path={} (LLM returned zero tasks)",
            plan_path
        );
        return Err(DecomposeError::EmptyDecomposition);
    }

    info!(
        "productivity.decompose.llm_ok plan_path={} task_count={}",
        plan_path,
        decomposed.tasks.len()
    );

    // 4. POST to /plans/decompose. We use HTTP loopback rather than calling
    //    the inner handler directly because the handler is module-private
    //    and depends on `ApiState` (not `AppState`). Loopback also keeps
    //    the same code path used by the slash command, so transactional
    //    semantics + upcoming-claim-registry plumbing are exercised
    //    identically. Runner binds 127.0.0.1 only, so this never leaves
    //    the loopback interface.
    let task_count = decomposed.tasks.len();
    let plan_id = post_decompose(runner_port, plan_path, &version_hash, decomposed).await?;

    // 5. Stamp the plan markdown so the next invocation short-circuits at
    //    the hash check above. Best-effort: a stamp failure is logged but
    //    doesn't fail the operation (the row is already in PG).
    let stamped = match stamp_plan(plan_path, &plan_text, &version_hash) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "productivity.decompose.stamp_failed plan_path={} err={}",
                plan_path, e
            );
            false
        }
    };

    info!(
        "productivity.decompose.completed plan_id={} task_count={} stamped={}",
        plan_id, task_count, stamped
    );

    Ok(DecomposeResult {
        plan_id,
        task_count,
        version_hash,
        idempotent_skip: false,
        stamped,
    })
}

// =============================================================================
// Helpers — file IO, hashing, HTTP, stamp
// =============================================================================

fn read_plan(plan_path: &str) -> Result<String, DecomposeError> {
    let path = std::path::Path::new(plan_path);
    if !path.exists() {
        return Err(DecomposeError::PlanNotFound(plan_path.to_string()));
    }
    std::fs::read_to_string(path).map_err(|e| DecomposeError::PlanRead(e.to_string()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Wire shape of the `/plans/decompose` POST body. Mirrors
/// `crate::mcp::plans::DecomposePlanRequest` / `DecomposeTaskInput`. We
/// re-declare it here rather than importing because the corresponding
/// types in `mcp/plans.rs` are `Deserialize`-only (they live on the
/// receiving side). `Serialize` is needed for the outgoing JSON body.
#[derive(Debug, Serialize)]
struct WireDecomposePlanRequest<'a> {
    markdown_path: &'a str,
    version_hash: &'a str,
    title: Option<String>,
    summary: Option<String>,
    tasks: Vec<WireDecomposeTaskInput>,
}

#[derive(Debug, Serialize)]
struct WireDecomposeTaskInput {
    phase_name: String,
    sequence_in_phase: i32,
    description: String,
    expected_file_claims: Vec<String>,
    expected_dirs: Vec<String>,
    depends_on_indices: Vec<usize>,
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireDecomposePlanResponse {
    #[serde(rename = "planId")]
    plan_id: String,
}

async fn post_decompose(
    runner_port: u16,
    plan_path: &str,
    version_hash: &str,
    decomposed: DecomposedTasks,
) -> Result<String, DecomposeError> {
    let url = format!("http://127.0.0.1:{}/plans/decompose", runner_port);

    let summary_truncated: String = decomposed.plan_summary.summary.chars().take(500).collect();

    let body = WireDecomposePlanRequest {
        markdown_path: plan_path,
        version_hash,
        title: Some(decomposed.plan_summary.title),
        summary: Some(summary_truncated),
        tasks: decomposed
            .tasks
            .into_iter()
            .map(|t| WireDecomposeTaskInput {
                phase_name: t.phase_name,
                sequence_in_phase: t.sequence_in_phase,
                description: t.description,
                expected_file_claims: t.expected_file_claims,
                expected_dirs: t.expected_dirs,
                depends_on_indices: t.depends_on_indices,
                notes: t.notes,
            })
            .collect(),
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| DecomposeError::HttpPost(format!("client build: {}", e)))?;

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| DecomposeError::HttpPost(format!("send to {}: {}", url, e)))?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(DecomposeError::HttpPost(format!(
            "{} returned {}: {}",
            url, status, body_text
        )));
    }

    let parsed: WireDecomposePlanResponse = response
        .json()
        .await
        .map_err(|e| DecomposeError::HttpPost(format!("parse response: {}", e)))?;

    Ok(parsed.plan_id)
}

/// Append `<!-- decomposed: <hash> at <ISO-8601> -->` to the plan markdown
/// so a future invocation's hash check short-circuits. Returns:
/// - `Ok(true)` if a fresh stamp was written.
/// - `Ok(false)` if a stamp for the same hash already exists (no-op).
/// - `Err(_)` on IO failure.
fn stamp_plan(plan_path: &str, plan_text: &str, version_hash: &str) -> Result<bool, String> {
    if plan_already_stamped(plan_text, version_hash) {
        return Ok(false);
    }

    let timestamp = chrono::Utc::now().to_rfc3339();
    let stamp = format!("\n<!-- decomposed: {} at {} -->\n", version_hash, timestamp);

    let mut new_content = plan_text.to_string();
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&stamp);

    std::fs::write(plan_path, new_content).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Look for a `<!-- decomposed: <hash> at ... -->` marker matching the
/// given hash. Returns true if such a marker exists.
fn plan_already_stamped(plan_text: &str, version_hash: &str) -> bool {
    let needle = format!("<!-- decomposed: {} at", version_hash);
    plan_text.contains(&needle)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::oneshot::OneshotError;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;

    /// Minimal mock adapter — returns a canned response or error. Used
    /// without touching live providers.
    struct MockOneshot {
        canned: Mutex<Option<Result<Value, OneshotError>>>,
    }

    #[async_trait]
    impl OneshotLlm for MockOneshot {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn call_json(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
            _schema: Value,
            _schema_name: &str,
        ) -> Result<Value, OneshotError> {
            self.canned
                .lock()
                .unwrap()
                .take()
                .expect("canned response already consumed")
        }
    }

    fn mock_ok(value: Value) -> MockOneshot {
        MockOneshot {
            canned: Mutex::new(Some(Ok(value))),
        }
    }

    fn mock_err(e: OneshotError) -> MockOneshot {
        MockOneshot {
            canned: Mutex::new(Some(Err(e))),
        }
    }

    // --- System prompt snapshot -----------------------------------------

    /// Pin the system prompt's structure so accidental edits flag a test
    /// failure. Update both the prompt and this test together.
    #[test]
    fn system_prompt_snapshot() {
        // Header + the four named rule blocks must all be present. We
        // assert on stable substrings rather than the full string so
        // typo fixes that don't change semantics don't break the test.
        let p = DECOMPOSE_SYSTEM_PROMPT;
        assert!(p.starts_with("You are decomposing a software project plan markdown"));
        assert!(p.contains("PHASE IDENTIFICATION (slash command rule 2)"));
        assert!(p.contains("CLAIM INFERENCE (slash command rule 3"));
        assert!(p.contains("DEPENDENCY DETECTION (slash command rule 4)"));
        assert!(p.contains("OUTPUT REQUIREMENTS"));
        assert!(p.contains("plan_summary.title"));
        assert!(p.contains("plan_summary.summary"));
        assert!(p.contains("expected_file_claims"));
        assert!(p.contains("depends_on_indices"));
        // No emoji or trailing whitespace surprises.
        for line in p.lines() {
            assert!(
                !line.ends_with(' ') && !line.ends_with('\t'),
                "trailing whitespace in line: {:?}",
                line
            );
        }
    }

    #[test]
    fn system_prompt_size_is_reasonable() {
        // Guardrail: a future edit that bloats the prompt past ~5KB
        // forces a deliberate update of this assertion.
        let len = DECOMPOSE_SYSTEM_PROMPT.len();
        assert!(
            len > 1000,
            "prompt suspiciously short ({}b); did rules get dropped?",
            len
        );
        assert!(
            len < 5000,
            "prompt grew past 5KB ({}b); cache cost goes up",
            len
        );
    }

    // --- LLM-disabled / no-credentials paths ----------------------------

    #[tokio::test]
    async fn llm_disabled_round_trips_typed_call() {
        // Verifies that the `OneshotError -> DecomposeError` mapping
        // produces `LlmDisabled` for `Disabled` (without invoking the
        // full pipeline, which needs PG + a runner port).
        let mock = mock_err(OneshotError::Disabled);
        let result: Result<DecomposedTasks, OneshotError> =
            mock.call_typed::<DecomposedTasks>("sys", "user").await;
        let dec_err = DecomposeError::from(result.expect_err("disabled"));
        match dec_err {
            DecomposeError::LlmDisabled => {}
            other => panic!("expected LlmDisabled, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn no_credentials_maps_to_decompose_error() {
        let mock = mock_err(OneshotError::NoCredentials);
        let result: Result<DecomposedTasks, OneshotError> =
            mock.call_typed::<DecomposedTasks>("sys", "user").await;
        let dec_err = DecomposeError::from(result.expect_err("no creds"));
        match dec_err {
            DecomposeError::LlmNoCredentials => {}
            other => panic!("expected LlmNoCredentials, got {:?}", other),
        }
    }

    // --- Schema-parse error path ----------------------------------------

    #[tokio::test]
    async fn schema_parse_error_maps_to_decompose_error() {
        // Provider returned a syntactically valid JSON object but it
        // doesn't match `DecomposedTasks` — `call_typed` surfaces that
        // as `OneshotError::SchemaParse`, which we map to
        // `DecomposeError::LlmSchemaParse`.
        let mock = mock_ok(serde_json::json!({ "wrong_field": 42 }));
        let result: Result<DecomposedTasks, OneshotError> =
            mock.call_typed::<DecomposedTasks>("sys", "user").await;
        let dec_err = DecomposeError::from(result.expect_err("schema mismatch"));
        match dec_err {
            DecomposeError::LlmSchemaParse(msg) => {
                assert!(
                    msg.contains("deserialize") || msg.contains("missing"),
                    "{}",
                    msg
                );
            }
            other => panic!("expected LlmSchemaParse, got {:?}", other),
        }
    }

    // --- Stamping idempotency -------------------------------------------

    #[test]
    fn stamp_plan_writes_marker_when_absent() {
        let dir = std::env::temp_dir().join("qontinui-decompose-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("plan-{}.md", uuid::Uuid::new_v4()));
        let body = "# Test plan\n\nA paragraph.\n";
        std::fs::write(&path, body).unwrap();

        let hash = sha256_hex(body.as_bytes());
        let path_str = path.to_string_lossy().to_string();
        let stamped = stamp_plan(&path_str, body, &hash).expect("stamp ok");
        assert!(stamped);

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains(&format!("<!-- decomposed: {} at", hash)));

        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stamp_plan_is_idempotent_for_same_hash() {
        let dir = std::env::temp_dir().join("qontinui-decompose-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("plan-{}.md", uuid::Uuid::new_v4()));
        let body = "# Test\n";
        std::fs::write(&path, body).unwrap();

        let hash = sha256_hex(body.as_bytes());
        let path_str = path.to_string_lossy().to_string();

        let first = stamp_plan(&path_str, body, &hash).expect("first stamp");
        assert!(first);

        // Re-read the just-stamped content and try again — should no-op.
        let stamped_body = std::fs::read_to_string(&path).unwrap();
        let second = stamp_plan(&path_str, &stamped_body, &hash).expect("second stamp");
        assert!(!second, "stamp should be idempotent for the same hash");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plan_already_stamped_detects_marker() {
        let body = "# Plan\n\nbody\n\n<!-- decomposed: deadbeef at 2026-01-01T00:00:00Z -->\n";
        assert!(plan_already_stamped(body, "deadbeef"));
        assert!(!plan_already_stamped(body, "cafef00d"));
    }

    // --- Hashing --------------------------------------------------------

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256 of the empty string.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // --- Schema round-trip ----------------------------------------------

    #[test]
    fn decomposed_tasks_schema_is_well_formed() {
        let schema =
            serde_json::to_value(schemars::schema_for!(DecomposedTasks)).expect("schema_for!");
        // Top-level should be an object with `plan_summary` and `tasks`.
        let s = schema.to_string();
        assert!(s.contains("plan_summary"));
        assert!(s.contains("tasks"));
        assert!(s.contains("phase_name"));
        assert!(s.contains("expected_file_claims"));
        assert!(s.contains("depends_on_indices"));
    }

    #[test]
    fn decomposed_tasks_round_trips_through_serde() {
        let dt = DecomposedTasks {
            plan_summary: PlanSummary {
                title: "Test".to_string(),
                summary: "summary".to_string(),
            },
            tasks: vec![DecomposedTask {
                phase_name: "Phase 1 -- Foundation".to_string(),
                sequence_in_phase: 0,
                description: "First task".to_string(),
                expected_file_claims: vec!["src/lib.rs".to_string()],
                expected_dirs: vec![],
                depends_on_indices: vec![],
                notes: None,
            }],
        };
        let json = serde_json::to_value(&dt).unwrap();
        let parsed: DecomposedTasks = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].phase_name, "Phase 1 -- Foundation");
    }
}
