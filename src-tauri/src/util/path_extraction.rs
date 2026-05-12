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
use std::time::Duration;

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::ai_provider::{OneshotClaudeApiWarm, OneshotError, OneshotLlm};
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

// ─────────────────────────────────────────────────────────────────────────────
// AI extractor (sibling to `extract_candidate_paths`).
//
// Phase 1 of the `ai-extracted-candidate-paths` plan adds a Haiku-4.5-backed
// path inferrer that complements the regex extractor above. The regex path
// only catches tokens that *look* like file paths in the literal text; the
// AI path can infer plausible files from a natural-language description
// like "fix the OAuth bug" with no path tokens at all.
//
// Architecture:
//   - Calls `claude_api_warm::run_claude_api_warm_with_structured_output`
//     directly (NOT `routing::run_prompt_with_structured_output`, which
//     drops cache_control and the system_prefix split — see
//     `ai_provider/routing.rs:840-865`).
//   - The system prefix is stable and ≥ 16384 chars so Haiku 4.5 attaches
//     `cache_control: ephemeral` to it (per
//     `cache_aware_builder.rs:141-150`). The user message carries the
//     per-call cwd / repo snippet / git log / prompt and is never cached.
//   - Wraps the blocking warm call in `tauri::async_runtime::spawn_blocking`
//     and the await in a 5s `tokio::time::timeout` — on Elapsed the caller
//     falls back to the regex extractor.
//   - Post-processes the model's JSON `{ "paths": [...] }` through the same
//     normalize → resolve-against-cwd → blacklist → dedupe → cap pipeline
//     the regex extractor uses, so downstream consumers can't tell which
//     extractor produced a given path.
// ─────────────────────────────────────────────────────────────────────────────

/// Default model used by the AI extractor. Mirrors the script-emitter and
/// coordinator's default — Haiku 4.5 is the cheapest cache-eligible Claude.
const AI_EXTRACT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Tight token cap — the model emits a JSON array of up to 50 short strings.
const AI_EXTRACT_MAX_TOKENS: u32 = 1024;

/// Stable schema name surfaced to the warm provider's tool-call API.
const AI_EXTRACT_SCHEMA_NAME: &str = "candidate_paths_v1";

/// Hard timeout for the warm call. Path extraction is best-effort; on
/// timeout the caller falls back to the cheap regex extractor rather than
/// blocking the launch probe.
const AI_EXTRACT_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors returned by [`extract_paths_via_ai`]. Always informational —
/// callers fall back to the regex extractor.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("AI provider unreachable")]
    Offline,
    #[error("Rate limited")]
    RateLimited,
    #[error("AI extractor disabled by user setting")]
    Disabled,
    #[error("AI response could not be parsed: {0}")]
    ParseFailure(String),
    #[error("AI provider error: {0}")]
    Other(String),
}

/// Context bundle the AI extractor sees alongside the prompt. Built by
/// the probe handler from cwd state. Kept small (~3 KB total) since this
/// rides in `user_message` (uncached per-call).
#[derive(Debug, Clone, Default)]
pub struct AiContextBundle {
    pub cwd: String,
    /// First-2-levels dir listing of cwd, capped ~80 entries.
    pub repo_tree_snippet: String,
    /// `git log --oneline -5` output, best-effort. Empty on failure.
    pub recent_git_log: String,
}

/// Ask Haiku 4.5 to infer plausible candidate file paths from a
/// natural-language `prompt`, given a small `bundle` of context (cwd, dir
/// listing, recent git log). Returns the post-processed (normalized,
/// cwd-resolved, blacklist-filtered, deduped, capped) paths.
///
/// Best-effort: any error class — timeout, offline, rate limit, parse
/// failure, transport error — is returned as `Err` so the caller can fall
/// back to [`extract_candidate_paths`]. The function never panics.
pub async fn extract_paths_via_ai(
    prompt: &str,
    bundle: &AiContextBundle,
) -> Result<Vec<String>, ExtractError> {
    let provider = OneshotClaudeApiWarm::with_overrides(AI_EXTRACT_MODEL, AI_EXTRACT_MAX_TOKENS);
    extract_paths_via_ai_inner(prompt, bundle, &provider).await
}

async fn extract_paths_via_ai_inner(
    prompt: &str,
    bundle: &AiContextBundle,
    provider: &dyn OneshotLlm,
) -> Result<Vec<String>, ExtractError> {
    let system_prefix = build_system_prefix();
    let user_message = build_user_message(prompt, bundle);
    let schema = ai_extract_schema();
    let cwd = bundle.cwd.clone();

    debug!(
        "path_extraction.ai_call_started prompt_len={} cwd_len={} repo_snippet_len={} git_log_len={} system_len={} user_len={}",
        prompt.len(),
        bundle.cwd.len(),
        bundle.repo_tree_snippet.len(),
        bundle.recent_git_log.len(),
        system_prefix.len(),
        user_message.len(),
    );

    let call_future = provider.call_json(
        &system_prefix,
        &user_message,
        schema,
        AI_EXTRACT_SCHEMA_NAME,
    );

    let output_value = match tokio::time::timeout(AI_EXTRACT_TIMEOUT, call_future).await {
        Ok(Ok(v)) => v,
        Ok(Err(OneshotError::NoCredentials)) => {
            info!("path_extraction.ai_disabled reason=no_credentials");
            return Err(ExtractError::Offline);
        }
        Ok(Err(OneshotError::Disabled)) => {
            info!("path_extraction.ai_disabled reason=provider_disabled");
            return Err(ExtractError::Offline);
        }
        Ok(Err(OneshotError::BudgetExhausted)) => {
            info!("path_extraction.ai_rate_limited reason=budget");
            return Err(ExtractError::RateLimited);
        }
        Ok(Err(OneshotError::ProviderTransport(msg))) => {
            let lower = msg.to_ascii_lowercase();
            if lower.contains("rate") && lower.contains("limit") {
                info!("path_extraction.ai_rate_limited");
                return Err(ExtractError::RateLimited);
            }
            warn!("path_extraction.ai_transport_error: {}", msg);
            return Err(ExtractError::Offline);
        }
        Ok(Err(OneshotError::SchemaParse(msg))) => {
            return Err(ExtractError::ParseFailure(msg));
        }
        Err(_elapsed) => {
            info!(
                "path_extraction.ai_timeout after {}s",
                AI_EXTRACT_TIMEOUT.as_secs()
            );
            return Err(ExtractError::Offline);
        }
    };

    // `OneshotLlm::call_json` returns a parsed JSON `Value`. Serialize back
    // to a string so the existing `parse_ai_paths_response(&str, &cwd)`
    // post-processor (slash-normalize → cwd-resolve → blacklist → dedupe →
    // cap) stays untouched. The downstream pipeline expects a stringy input
    // and we don't want to dual-track string vs Value parsers.
    let output = serde_json::to_string(&output_value)
        .map_err(|e| ExtractError::ParseFailure(format!("re-serialize: {}", e)))?;

    let paths = parse_ai_paths_response(&output, &cwd)?;
    debug!(
        "path_extraction.ai_call_succeeded returned={} cwd_resolved={}",
        paths.len(),
        !cwd.is_empty(),
    );
    Ok(paths)
}

/// Parse the warm provider's tool-call output as `{ "paths": [...] }` and
/// run the standard post-processing pipeline (slash-normalize → resolve
/// against `cwd` → [`normalize_path`] → blacklist → dedupe → cap).
///
/// Split out of [`extract_paths_via_ai`] so the post-processing is unit
/// testable without a live network call. The async function is a thin
/// orchestrator on top.
fn parse_ai_paths_response(output: &str, cwd: &str) -> Result<Vec<String>, ExtractError> {
    let parsed: Value = serde_json::from_str(output)
        .map_err(|e| ExtractError::ParseFailure(format!("not valid JSON: {}", e)))?;

    let paths_val = parsed
        .get("paths")
        .ok_or_else(|| ExtractError::ParseFailure("missing required field `paths`".to_string()))?;

    let arr = paths_val
        .as_array()
        .ok_or_else(|| ExtractError::ParseFailure("`paths` is not an array".to_string()))?;

    let mut out: Vec<String> = Vec::with_capacity(arr.len().min(MAX_CANDIDATES));
    for item in arr {
        let Some(raw) = item.as_str() else {
            // Skip non-string entries silently — Haiku occasionally hallucinates
            // a nested object. We don't want to fail the whole call for one.
            continue;
        };

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Forward-slash normalize before resolution / blacklist match — same
        // canonicalization the regex extractor applies.
        let slashed = trimmed.replace('\\', "/");

        let resolved = if cwd.is_empty() || is_absolute_token(&slashed) {
            slashed
        } else {
            let joined = Path::new(cwd).join(&slashed);
            joined.to_string_lossy().replace('\\', "/")
        };

        let normalized = normalize_path(&resolved);

        if PATH_BLACKLIST
            .iter()
            .any(|needle| normalized.contains(needle))
        {
            continue;
        }

        if !out.contains(&normalized) {
            out.push(normalized);
        }

        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }

    Ok(out)
}

/// Build the JSON Schema describing the expected `{ "paths": [...] }`
/// tool-call output. Stable so the warm provider's prompt cache can key
/// off the tool definition.
fn ai_extract_schema() -> Value {
    json!({
        "type": "object",
        "required": ["paths"],
        "properties": {
            "paths": {
                "type": "array",
                "maxItems": 50,
                "items": { "type": "string" }
            }
        }
    })
}

/// Build the per-call user message. Stays under ~3 KB in normal use so
/// it doesn't dilute the cache-hit ratio against the stable system prefix.
fn build_user_message(prompt: &str, bundle: &AiContextBundle) -> String {
    format!(
        "cwd: {cwd}\n\
         \n\
         Repo tree snippet:\n\
         {tree}\n\
         \n\
         Recent git log:\n\
         {log}\n\
         \n\
         User prompt:\n\
         {prompt}\n",
        cwd = bundle.cwd,
        tree = bundle.repo_tree_snippet,
        log = bundle.recent_git_log,
        prompt = prompt,
    )
}

/// Stable system prefix served to Haiku 4.5. MUST be ≥ 16384 chars for
/// `cache_control: ephemeral` to attach (per
/// `cache_aware_builder.rs:141-150`). Padded with realistic project-layout
/// hints to clear the threshold while still being relevant guidance.
fn build_system_prefix() -> String {
    let prefix = String::from(
        "You are a path-inference assistant for a code-automation runner.\n\
         \n\
         ## Task\n\
         \n\
         Given a developer's natural-language prompt about code work they are about \
         to perform — typically one or two sentences describing a bug, refactor, \
         feature, or investigation — return the file paths they are MOST LIKELY \
         to read or edit. Your output is consumed by a launch-time conflict-detection \
         probe that compares your suggestions against an in-memory file-lock \
         registry. Suggesting too few real targets means the probe misses a \
         conflict; suggesting noise means the probe asks the user to confirm a \
         conflict that doesn't exist. Aim for high precision and reasonable recall.\n\
         \n\
         You are NOT writing code. You are NOT explaining the bug. You are NOT \
         producing a plan. You output one JSON object — `{\"paths\": [...]}` — \
         and nothing else.\n\
         \n\
         ## Output schema\n\
         \n\
         The response MUST conform to this JSON schema:\n\
         \n\
         ```json\n\
         {\n\
           \"type\": \"object\",\n\
           \"required\": [\"paths\"],\n\
           \"properties\": {\n\
             \"paths\": {\n\
               \"type\": \"array\",\n\
               \"maxItems\": 50,\n\
               \"items\": { \"type\": \"string\" }\n\
             }\n\
           }\n\
         }\n\
         ```\n\
         \n\
         Each entry in `paths` is a forward-slash file or directory path. Either \
         relative (resolved by the caller against the cwd they pass in the user \
         message) or absolute. Glob characters are NOT supported — emit concrete \
         paths or path prefixes. If you genuinely cannot infer any plausible \
         target from the prompt, return `{\"paths\": []}`.\n\
         \n\
         ## Worked examples\n\
         \n\
         These illustrate the expected granularity and style. The actual cwd \
         layout will be hinted at via the `Repo tree snippet` block in the user \
         message — let that override the generic conventions when the two conflict.\n\
         \n\
         Example 1 — bug in a known subsystem:\n\
         Prompt: \"fix the auth bug where expired tokens still pass middleware\"\n\
         Output: {\"paths\": [\"src/auth/middleware.rs\", \"src/auth/token.rs\", \
         \"src/auth/mod.rs\", \"tests/auth_test.rs\"]}\n\
         \n\
         Example 2 — refactor naming a flow:\n\
         Prompt: \"refactor the OAuth flow to share a single Discovery client\"\n\
         Output: {\"paths\": [\"src/oauth/flow.rs\", \"src/oauth/discovery.rs\", \
         \"src/oauth/client.rs\", \"src/oauth/mod.rs\"]}\n\
         \n\
         Example 3 — prompt naming a specific function/module token:\n\
         Prompt: \"the db_retry backoff is too aggressive, halve the multiplier\"\n\
         Output: {\"paths\": [\"src/db_retry.rs\", \"src/db/retry.rs\"]}\n\
         \n\
         Example 4 — frontend page work:\n\
         Prompt: \"add a loading spinner to the project settings page\"\n\
         Output: {\"paths\": [\"src/pages/settings/project.tsx\", \
         \"src/components/Spinner.tsx\", \"src/pages/settings/index.tsx\"]}\n\
         \n\
         Example 5 — Python service work:\n\
         Prompt: \"the email sending service is dropping retries on 503\"\n\
         Output: {\"paths\": [\"app/services/email.py\", \"app/services/__init__.py\", \
         \"tests/services/test_email.py\"]}\n\
         \n\
         Example 6 — config / migrations:\n\
         Prompt: \"add a nullable last_seen_at column to the users table\"\n\
         Output: {\"paths\": [\"migrations/versions/\", \"app/models/user.py\", \
         \"schema.sql\"]}\n\
         \n\
         Example 7 — purely descriptive, no obvious files:\n\
         Prompt: \"thinking about the project's roadmap for Q3\"\n\
         Output: {\"paths\": []}\n\
         \n\
         Example 8 — CLI tool work:\n\
         Prompt: \"the deploy command is missing a --dry-run flag\"\n\
         Output: {\"paths\": [\"src/cli/deploy.rs\", \"src/cli/mod.rs\", \
         \"src/main.rs\"]}\n\
         \n\
         Example 9 — test-only change:\n\
         Prompt: \"add coverage for the empty-array branch of normalize_path\"\n\
         Output: {\"paths\": [\"src/util/path.rs\", \"tests/util/path_test.rs\"]}\n\
         \n\
         Example 10 — prompt names a file directly:\n\
         Prompt: \"clean up the comments in src/main.rs\"\n\
         Output: {\"paths\": [\"src/main.rs\"]}\n\
         \n\
         ## Blacklist\n\
         \n\
         NEVER suggest paths under any of:\n\
         \n\
         - `node_modules/` — third-party JavaScript dependencies; users edit \
           their imports, not the vendored copy.\n\
         - `target/` — Rust/Cargo build artifacts.\n\
         - `.git/` — version-control internals.\n\
         - `dist/` — bundled output produced by build tooling.\n\
         - `build/` — generated artifacts.\n\
         \n\
         The caller's post-processor will drop any path whose normalized form \
         contains one of these substrings, but emitting them wastes your output \
         token budget and confuses the probe's telemetry. Suppress them at \
         source.\n\
         \n\
         ## Project-layout hints\n\
         \n\
         When the user message's `Repo tree snippet` is empty, sparse, or \
         ambiguous, fall back to the conventions below. These are the layouts \
         the runner sees most often in practice.\n\
         \n\
         Rust crates (Cargo workspace or single crate):\n\
         - `Cargo.toml` and `Cargo.lock` at the workspace root.\n\
         - `src/main.rs` (binary) or `src/lib.rs` (library) is the crate root.\n\
         - `src/<module>.rs` plus `src/<module>/mod.rs` and `src/<module>/<sub>.rs` \
           for nested modules.\n\
         - `tests/` for integration tests, `benches/` for criterion benches, \
           `examples/` for runnable examples.\n\
         - `build.rs` at the crate root for build scripts.\n\
         - Workspace members live under `crates/<name>/` or as siblings under \
           the workspace root.\n\
         - Tauri apps put Rust under `src-tauri/src/` and the frontend at the \
           project root.\n\
         \n\
         TypeScript / JavaScript projects (Node / React / Next.js / Vite):\n\
         - `package.json`, `tsconfig.json`, `vite.config.ts` or \
           `next.config.js` at the project root.\n\
         - `src/` is the canonical source root for Vite / CRA-style apps.\n\
         - Next.js uses `pages/` (Pages Router) or `app/` (App Router) plus \
           `pages/api/` or `app/api/` for routes; components live in \
           `components/` or `src/components/`.\n\
         - React component files end in `.tsx`/`.jsx`; co-located styles often \
           use `*.module.css` or `*.module.scss`.\n\
         - Hooks conventionally live in `src/hooks/` and named `use*.ts(x)`.\n\
         - State stores live in `src/store/`, `src/stores/`, or `src/state/`.\n\
         - API client wrappers live in `src/api/`, `src/lib/api/`, or \
           `src/services/`.\n\
         - Tests are co-located as `<file>.test.ts`/`<file>.spec.ts` or live \
           under `__tests__/` or `tests/`.\n\
         - Build output (DO NOT suggest): `dist/`, `build/`, `.next/`, \
           `out/`.\n\
         \n\
         Python projects:\n\
         - `pyproject.toml`, `setup.py`, or `setup.cfg` at the project root.\n\
         - Source under `src/<package>/` or directly `<package>/`.\n\
         - Each package directory has an `__init__.py`.\n\
         - FastAPI / Flask apps typically split into `app/main.py`, \
           `app/routers/<feature>.py`, `app/models/<entity>.py`, \
           `app/services/<feature>.py`, `app/schemas/<entity>.py`.\n\
         - Database migrations under `migrations/versions/` (Alembic) or \
           `<app>/migrations/` (Django).\n\
         - Settings / config typically under `app/config.py`, \
           `<app>/settings.py`, or `<app>/core/config.py`.\n\
         - Tests under `tests/` mirroring the source layout, files named \
           `test_*.py`.\n\
         \n\
         Go modules:\n\
         - `go.mod` and `go.sum` at the module root.\n\
         - Binaries under `cmd/<name>/main.go`.\n\
         - Library packages directly under the module root or under \
           `internal/<pkg>/`.\n\
         - Tests are co-located as `<file>_test.go`.\n\
         \n\
         Documentation / spec / config files (all roots):\n\
         - `README.md`, `CHANGELOG.md`, `LICENSE` at the project root.\n\
         - `.github/workflows/*.yml` for GitHub Actions.\n\
         - `Dockerfile`, `docker-compose.yml`, `.dockerignore` at the project \
           root for container builds.\n\
         - `.env`, `.env.local`, `.env.example` for environment overrides — \
           but treat `.env` files as sensitive; only suggest them when the \
           prompt explicitly references env vars.\n\
         \n\
         Schema / migration files:\n\
         - SQLAlchemy / Alembic: `migrations/versions/<rev>_<slug>.py`.\n\
         - Diesel (Rust): `migrations/<timestamp>_<slug>/{up,down}.sql`.\n\
         - Prisma (TS): `prisma/schema.prisma`, `prisma/migrations/`.\n\
         - Raw SQL: `schema.sql`, `db/schema.sql`, or `sql/<feature>.sql`.\n\
         \n\
         CI / tooling configs (mention only when the prompt references them):\n\
         - `.eslintrc.js`/`.eslintrc.json`, `.prettierrc`, `tsconfig*.json`, \
           `vitest.config.ts`, `jest.config.js`, `playwright.config.ts`, \
           `pytest.ini`, `tox.ini`, `mypy.ini`, `ruff.toml`, `clippy.toml`, \
           `rust-toolchain.toml`, `rustfmt.toml`.\n\
         \n\
         Common naming patterns to recognize in prompts:\n\
         - `auth`, `login`, `signin`, `signup`, `oauth`, `oidc`, `saml`, \
           `session`, `token`, `jwt`, `cookie` → authentication subsystem.\n\
         - `db`, `database`, `pool`, `query`, `migration`, `schema`, `sql`, \
           `orm`, `repo`, `repository` → persistence layer.\n\
         - `api`, `route`, `endpoint`, `handler`, `controller`, `view` → HTTP \
           surface.\n\
         - `worker`, `job`, `task`, `queue`, `consumer`, `producer` → \
           background processing.\n\
         - `client`, `service`, `adapter`, `provider`, `connector` → \
           outbound integrations.\n\
         - `model`, `entity`, `schema`, `dto`, `record` → data shapes.\n\
         - `test`, `spec`, `fixture`, `mock`, `stub` → test layer (mirror the \
           source path under `tests/` or `__tests__/`).\n\
         - `cli`, `command`, `flag`, `arg` → command-line surface (typically \
           `cli/<command>.rs` or `commands/<name>.py`).\n\
         - `ui`, `page`, `component`, `view`, `screen`, `modal`, `dialog` → \
           frontend tree.\n\
         - `config`, `settings`, `env`, `flag`, `feature_flag` → config layer.\n\
         - `log`, `tracing`, `telemetry`, `metric`, `span` → observability \
           layer (often `src/telemetry.rs` or `app/telemetry/`).\n\
         \n\
         Cross-language conventions:\n\
         - When the prompt names a CamelCase type like `OAuthFlow`, also \
           consider snake_case (`oauth_flow.rs`, `oauth_flow.py`) and \
           kebab-case (`oauth-flow.ts`) variants.\n\
         - When the prompt names a function in backticks (e.g. \
           `\\`normalize_path\\``), include the file most likely to define it \
           — typically `src/util/<token>.rs` or `<pkg>/<token>.py` — plus the \
           sibling test file.\n\
         - When the prompt names an error message or log line verbatim, \
           consider `grep`-able single-source files: error definitions are \
           usually in `error.rs`, `errors.py`, `exceptions.py`, or per-module \
           error files.\n\
         \n\
         Granularity guidance:\n\
         - Prefer concrete `.rs`/`.ts`/`.py` files over directories.\n\
         - It is acceptable to emit a directory path (with trailing `/`) when \
           the prompt is genuinely module-scoped (\"refactor the auth module\") \
           and you don't know which file inside is the entry point. The \
           caller's post-processor handles directories the same as files.\n\
         - Cap your output at ~10 paths in practice; emitting more dilutes \
           precision. The schema's `maxItems: 50` is a hard limit, not a \
           target.\n\
         \n\
         ## Tauri / desktop-app conventions (this runner's primary surface)\n\
         \n\
         The qontinui runner this prompt serves is a Tauri 2 desktop app, so \
         its layout is a useful reference even when the user is working on a \
         non-Tauri target. Tauri apps split between a Rust backend in \
         `src-tauri/` and a web-tech frontend at the project root.\n\
         \n\
         Backend (Rust) layout under `src-tauri/`:\n\
         - `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `src-tauri/build.rs`.\n\
         - `src-tauri/tauri.conf.json` — app config, allowlists, bundler \
           settings.\n\
         - `src-tauri/src/main.rs` — process entry, builds the Tauri Builder, \
           registers `#[tauri::command]` handlers and plugins.\n\
         - `src-tauri/src/lib.rs` — re-exports for tests and external bins.\n\
         - `src-tauri/src/commands/` — one file per command surface (e.g. \
           `commands/ai_settings.rs`, `commands/file_registry.rs`, \
           `commands/runner.rs`).\n\
         - `src-tauri/src/ai_provider/` — Claude / Gemini / OAuth / warm-cache \
           plumbing; subfiles include `claude_api_warm.rs`, \
           `cache_aware_builder.rs`, `routing.rs`, `circuit_breaker.rs`, \
           `oauth/`.\n\
         - `src-tauri/src/coordinator/` — workflow scheduler and decide loop \
           (`act.rs`, `decide.rs`, `observe.rs`, `llm_decide.rs`).\n\
         - `src-tauri/src/database/pg/` — Postgres bindings, generated \
           Clorinde queries, migrations.\n\
         - `src-tauri/src/executor/` — workflow / step executor and \
           supporting infra (file_registry.rs, step_executor.rs, etc.).\n\
         - `src-tauri/src/mcp/` — Model Context Protocol surface and the \
           internal HTTP API the frontend calls.\n\
         - `src-tauri/src/util/` — small leaf helpers like this very file \
           (`util/path_extraction.rs`).\n\
         - `src-tauri/migrations/` — sqlx-style raw-SQL migrations \
           (`v01_init.sql`, `v02_add_runs.sql`, …).\n\
         - `src-tauri/icons/` — bundle icon assets per platform.\n\
         - `src-tauri/capabilities/` — Tauri 2 capability JSON files \
           controlling per-window IPC permissions.\n\
         \n\
         Frontend (TypeScript / React) layout at the project root:\n\
         - `package.json`, `vite.config.ts`, `tsconfig.json`, \
           `index.html`.\n\
         - `src/main.tsx` — React entry; mounts `<App />`.\n\
         - `src/App.tsx` — top-level layout shell.\n\
         - `src/pages/<feature>/<page>.tsx` — route components.\n\
         - `src/components/<group>/<Component>.tsx` — shared UI primitives.\n\
         - `src/hooks/use<Behavior>.ts` — custom hooks; common ones include \
           `usePageEvents.ts`, `useUiBridge.ts`, `useAiSettings.ts`.\n\
         - `src/api/<service>.ts` — fetch wrappers around the Rust HTTP \
           surface.\n\
         - `src/state/<slice>.ts` — Zustand or context stores.\n\
         - `src/lib/<helper>.ts` — pure utilities.\n\
         - `src/styles/` or per-component `*.module.css`.\n\
         - `src/__tests__/` or co-located `*.test.ts(x)` for Vitest.\n\
         - `src/specs/<area>.spec.uibridge.json` — UI Bridge state-machine \
           specs (compiled into runtime navigation).\n\
         \n\
         Cross-cutting (root):\n\
         - `.github/workflows/<job>.yml` — CI pipelines.\n\
         - `scripts/` or `tools/` — repo automation scripts.\n\
         - `docs/` — long-form documentation and design notes.\n\
         - `plans/` — active multi-phase implementation plans.\n\
         \n\
         ## Common-bug → likely-files cheat sheet\n\
         \n\
         Use these as priors when the prompt is vague but flavored:\n\
         \n\
         - \"login / signin / signup is broken\" → `src/auth/login.rs`, \
           `src/auth/session.rs`, `src/pages/login.tsx`, \
           `src/api/auth.ts`, `tests/auth_test.rs`.\n\
         - \"connection refused / network timeout\" → `src/network.rs`, \
           `src/transport/<proto>.rs`, `src/api/client.ts`, retry/backoff \
           helpers.\n\
         - \"flaky test / intermittent failure\" → the test file named in \
           the prompt plus `tests/common/mod.rs` or `tests/fixtures/`.\n\
         - \"slow query / N+1\" → ORM model file plus the route/handler that \
           reads it; for SQLAlchemy: `app/models/<entity>.py`, \
           `app/services/<feature>.py`.\n\
         - \"deadlock / race / mutex\" → the file owning the lock plus its \
           tests; in Rust look for `Mutex`, `RwLock`, `tokio::sync::*`.\n\
         - \"memory leak / OOM\" → long-lived caches, channel buffers, \
           event loops; common files: `cache.rs`, `subscriptions.rs`.\n\
         - \"CI red on main\" → `.github/workflows/<name>.yml` plus the test \
           file named in the failing job.\n\
         - \"docker build fails\" → `Dockerfile`, `docker-compose.yml`, \
           `.dockerignore`.\n\
         - \"migration rollback\" → `migrations/versions/<latest>.py` (or \
           equivalent), `app/models/<entity>.py`.\n\
         - \"feature flag stuck on / off\" → `app/config.py`, \
           `src/config.rs`, `app/feature_flags/`.\n\
         - \"docs are out of date\" → `README.md`, `docs/<area>.md`, \
           `CHANGELOG.md`.\n\
         \n\
         ## Anti-patterns (do not emit)\n\
         \n\
         - Do NOT emit `*` glob patterns. Concrete paths only.\n\
         - Do NOT emit URLs (`https://…`) — they are not paths.\n\
         - Do NOT emit shell commands (`grep -rn …`) — output goes to a \
           path comparator, not a shell.\n\
         - Do NOT emit pseudo-paths like `<auth module>` or `the file \
           that does X`.\n\
         - Do NOT emit paths to vendored/generated trees (see Blacklist).\n\
         - Do NOT emit paths that are obviously wrong for the inferred \
           language: a Python prompt should not produce `.rs` paths unless \
           the repo is explicitly polyglot (the tree snippet will reveal \
           that).\n\
         - Do NOT emit binary blobs, lock files (`Cargo.lock`, \
           `package-lock.json`, `poetry.lock`) unless the prompt explicitly \
           talks about dependency resolution.\n\
         \n\
         ## When the tree snippet is rich\n\
         \n\
         If the user message's `Repo tree snippet` lists concrete files that \
         match keywords from the prompt, prefer those over the generic \
         conventions. The snippet is ground truth for this repo; the \
         conventions in this system prompt are priors for when ground truth \
         is missing or ambiguous.\n\
         \n\
         If the snippet contradicts a generic prior (e.g. snippet shows \
         `lib/` instead of `src/`, or `internal/` for a Go module), trust the \
         snippet. The conventions are starting points, not constraints.\n\
         \n\
         ## When the recent git log is rich\n\
         \n\
         The `Recent git log` block lists the last few commit subjects on the \
         current branch. If a recent commit subject mentions the same \
         subsystem as the prompt (e.g. log says `\"oauth: tighten token \
         refresh\"` and the prompt says `\"the oauth refresh is broken\"`), \
         strongly bias toward files mentioned in or implied by that commit. \
         Recently-touched files are usually the right neighborhood.\n\
         \n\
         ## Edge cases\n\
         \n\
         - Prompt mentions a function name in backticks but no module: emit \
           the most likely defining file plus its sibling test, leaning on \
           the snake_case → file-name convention for the prompt's primary \
           language.\n\
         - Prompt is a stack-trace excerpt: emit the files named in the \
           frames, ignoring stdlib frames.\n\
         - Prompt is a single noun (\"the cache\"): emit `cache.rs`, \
           `src/cache/`, `app/cache.py`, etc., scoped by the snippet's \
           apparent language.\n\
         - Prompt is a question (\"why does X happen?\"): treat it the same \
           as a bug-fix prompt — return the files most likely to hold the \
           answer.\n\
         - Prompt is empty or whitespace: return `{\"paths\": []}`.\n\
         \n\
         Respond with the JSON object only — no prose.\n",
    );

    debug_assert!(
        prefix.len() >= 16384,
        "system_prefix must be ≥ 16384 chars for Haiku 4.5 cache_control attach (got {})",
        prefix.len()
    );
    prefix
}

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

    // ── AI extractor tests (Phase 1 of ai-extracted-candidate-paths). ────
    //
    // The async function `extract_paths_via_ai` is split into a sync
    // `parse_ai_paths_response` helper plus a thin spawn_blocking + timeout
    // wrapper. Most coverage targets the sync helper so tests don't need a
    // live network or a mocked warm provider. Two `#[ignore]` async tests
    // document the shape of the live call without blocking CI.

    #[test]
    fn test_ai_extract_schema_well_formed() {
        let schema = ai_extract_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "paths");
        assert_eq!(schema["properties"]["paths"]["type"], "array");
        assert_eq!(schema["properties"]["paths"]["maxItems"], 50);
        assert_eq!(schema["properties"]["paths"]["items"]["type"], "string");
    }

    #[test]
    fn test_build_system_prefix_meets_cache_threshold() {
        // Haiku 4.5's min_cacheable_chars is 16384 (cache_aware_builder.rs:141-150).
        // Below that the warm path silently skips the cache_control marker, so
        // the prefix must clear the threshold for prompt caching to engage.
        let prefix = build_system_prefix();
        assert!(
            prefix.len() >= 16384,
            "system_prefix must be ≥ 16384 chars for Haiku 4.5 cache_control attach (got {})",
            prefix.len()
        );
    }

    #[test]
    fn test_build_user_message_includes_prompt_and_cwd() {
        let bundle = AiContextBundle {
            cwd: "/repo/foo".to_string(),
            repo_tree_snippet: "src/\n  main.rs\n".to_string(),
            recent_git_log: "abc123 fix: thing".to_string(),
        };
        let prompt = "fix the OAuth bug where tokens never expire";
        let msg = build_user_message(prompt, &bundle);

        assert!(
            msg.contains("/repo/foo"),
            "user message must include cwd verbatim, got: {}",
            msg
        );
        assert!(
            msg.contains(prompt),
            "user message must include prompt verbatim, got: {}",
            msg
        );
        assert!(msg.contains("src/\n  main.rs"));
        assert!(msg.contains("abc123 fix: thing"));
    }

    #[test]
    fn test_ai_response_parsing_happy_path() {
        let output = r#"{"paths": ["src/auth.rs", "src/oauth.rs"]}"#;
        let paths = parse_ai_paths_response(output, "").expect("happy path parses");
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&normalize_path("src/auth.rs")));
        assert!(paths.contains(&normalize_path("src/oauth.rs")));
    }

    #[test]
    fn test_ai_response_parsing_malformed_returns_parse_failure() {
        let result = parse_ai_paths_response("{not valid json", "");
        match result {
            Err(ExtractError::ParseFailure(_)) => {}
            other => panic!("expected ParseFailure, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_response_parsing_missing_paths_field_returns_parse_failure() {
        // The JSON parses but lacks the required `paths` key.
        let result = parse_ai_paths_response(r#"{"other": []}"#, "");
        match result {
            Err(ExtractError::ParseFailure(msg)) => {
                assert!(msg.contains("paths"), "msg should mention `paths`: {}", msg);
            }
            other => panic!("expected ParseFailure, got {:?}", other),
        }
    }

    #[test]
    fn test_ai_response_parsing_blacklist_filtered() {
        let output = r#"{"paths": [
            "src/auth.rs",
            "node_modules/foo.js",
            "target/debug/x.rs",
            ".git/HEAD",
            "dist/bundle.js",
            "build/out.o"
        ]}"#;
        let paths = parse_ai_paths_response(output, "").expect("parse ok");
        assert_eq!(
            paths,
            vec![normalize_path("src/auth.rs")],
            "all blacklisted entries should be dropped, got {:?}",
            paths
        );
    }

    #[test]
    fn test_ai_response_parsing_dedupes_and_caps() {
        // 60 distinct paths plus 5 duplicates of the first — expect ≤ 50
        // and no duplicates.
        let mut entries: Vec<String> = (0..60).map(|i| format!("\"src/file{}.rs\"", i)).collect();
        for _ in 0..5 {
            entries.push("\"src/file0.rs\"".to_string());
        }
        let output = format!(r#"{{"paths": [{}]}}"#, entries.join(","));

        let paths = parse_ai_paths_response(&output, "").expect("parse ok");
        assert!(
            paths.len() <= MAX_CANDIDATES,
            "expected ≤{} paths, got {}",
            MAX_CANDIDATES,
            paths.len()
        );
        assert_eq!(paths.len(), MAX_CANDIDATES);

        // Dedupe check: the deduplicated count equals the raw count.
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), paths.len(), "no duplicates expected");
    }

    #[test]
    fn test_ai_response_parsing_resolves_relative_against_cwd() {
        let output = r#"{"paths": ["src/auth.rs"]}"#;
        let paths = parse_ai_paths_response(output, "/repo").expect("parse ok");
        assert_eq!(
            paths,
            vec![normalize_path("/repo/src/auth.rs")],
            "relative token should be joined onto cwd before normalization"
        );
    }

    #[test]
    fn test_ai_response_parsing_absolute_token_skips_cwd() {
        let output = r#"{"paths": ["/etc/hosts.conf"]}"#;
        let paths = parse_ai_paths_response(output, "/repo").expect("parse ok");
        assert_eq!(paths, vec![normalize_path("/etc/hosts.conf")]);
    }

    #[test]
    fn test_ai_response_parsing_skips_blank_and_non_string_entries() {
        // Includes whitespace-only, empty, and a nested object. All should
        // be silently skipped — the call shouldn't fail.
        let output = r#"{"paths": ["", "   ", {"nested": "obj"}, "src/main.rs"]}"#;
        let paths = parse_ai_paths_response(output, "").expect("parse ok");
        assert_eq!(paths, vec![normalize_path("src/main.rs")]);
    }

    use crate::ai_provider::{OneshotError, OneshotLlm};
    use serde_json::Value;
    use std::sync::Mutex;

    /// Test-only fake provider. Each call consumes the response by move
    /// (`OneshotError` is `Debug` but not `Clone`); construct one fake per
    /// test rather than reusing. Optional `delay` is applied before the
    /// response, so a `with_delay(TIMEOUT+1s)` fake exercises the timeout
    /// branch of `extract_paths_via_ai_inner`.
    struct FakeOneshotLlm {
        response: Mutex<Option<Result<Value, OneshotError>>>,
        delay: Duration,
    }

    impl FakeOneshotLlm {
        fn returning(response: Result<Value, OneshotError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
                delay: Duration::from_millis(0),
            }
        }

        fn with_delay(delay: Duration) -> Self {
            Self {
                response: Mutex::new(Some(Ok(serde_json::json!({"paths": []})))),
                delay,
            }
        }
    }

    #[async_trait::async_trait]
    impl OneshotLlm for FakeOneshotLlm {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn call_json(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
            _schema: Value,
            _schema_name: &str,
        ) -> Result<Value, OneshotError> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.response
                .lock()
                .expect("FakeOneshotLlm poisoned")
                .take()
                .expect("FakeOneshotLlm::call_json invoked twice — construct a fresh fake per test")
        }
    }

    #[tokio::test]
    async fn test_extract_paths_via_ai_timeout() {
        // Fake that sleeps past AI_EXTRACT_TIMEOUT. The plan called for
        // `start_paused = true` to keep this <1ms wall-clock, but that
        // attribute requires tokio's `test-util` feature which isn't
        // enabled in this crate's Cargo.toml (tokio = features = ["full"]
        // — `full` does NOT include `test-util`). To keep this PR scoped
        // to the three planned source files, the test uses a real delay
        // and pays the ~AI_EXTRACT_TIMEOUT wall-clock cost. A follow-up
        // adding `tokio` to dev-dependencies with the `test-util` feature
        // is the right way to speed this back up.
        let fake = FakeOneshotLlm::with_delay(AI_EXTRACT_TIMEOUT + Duration::from_secs(1));
        let bundle = AiContextBundle::default();
        let result = extract_paths_via_ai_inner("anything", &bundle, &fake).await;
        assert!(
            matches!(result, Err(ExtractError::Offline)),
            "expected Err(Offline), got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_no_credentials_maps_to_offline() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::NoCredentials));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        assert!(
            matches!(result, Err(ExtractError::Offline)),
            "got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_disabled_maps_to_offline() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::Disabled));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        assert!(
            matches!(result, Err(ExtractError::Offline)),
            "got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_budget_exhausted_maps_to_rate_limited() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::BudgetExhausted));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        assert!(
            matches!(result, Err(ExtractError::RateLimited)),
            "got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_transport_rate_limit_maps_to_rate_limited() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::ProviderTransport(
            "HTTP 429: rate limit exceeded".to_string(),
        )));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        assert!(
            matches!(result, Err(ExtractError::RateLimited)),
            "got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_transport_generic_maps_to_offline() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::ProviderTransport(
            "connection refused".to_string(),
        )));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        assert!(
            matches!(result, Err(ExtractError::Offline)),
            "got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_schema_parse_maps_to_parse_failure() {
        let fake = FakeOneshotLlm::returning(Err(OneshotError::SchemaParse(
            "missing field `paths`".to_string(),
        )));
        let result =
            extract_paths_via_ai_inner("anything", &AiContextBundle::default(), &fake).await;
        match result {
            Err(ExtractError::ParseFailure(msg)) => assert!(msg.contains("missing field")),
            other => panic!("expected ParseFailure, got {:?}", other),
        }
    }

    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY in keychain or warm OAuth credentials"]
    async fn test_extract_paths_via_ai_real_call() {
        let bundle = AiContextBundle {
            cwd: env!("CARGO_MANIFEST_DIR").replace('\\', "/"),
            repo_tree_snippet: "src/\n  ai_provider/\n  util/\n    path_extraction.rs\n"
                .to_string(),
            recent_git_log: String::new(),
        };
        let result = extract_paths_via_ai("fix the OAuth bug", &bundle).await;
        match result {
            Ok(paths) => {
                // Soft assertion — the model is non-deterministic, but for
                // an OAuth-flavored prompt at least one path should mention
                // auth/oauth tokens.
                assert!(
                    paths.iter().any(|p| {
                        let lower = p.to_lowercase();
                        lower.contains("oauth") || lower.contains("auth")
                    }),
                    "expected at least one auth/oauth path, got {:?}",
                    paths
                );
            }
            Err(ExtractError::Offline) => {
                // Acceptable ONLY when no credential was supplied. If
                // ANTHROPIC_API_KEY is set in the env, an Offline result means the
                // env-var fallback in resolve_warm_credential regressed — fail loud
                // so the CI lane catches it instead of silently passing.
                if std::env::var("ANTHROPIC_API_KEY")
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                {
                    panic!("Offline result despite ANTHROPIC_API_KEY being set — env-var fallback regression?");
                }
            }
            Err(e) => panic!("unexpected extractor error: {:?}", e),
        }
    }
}
