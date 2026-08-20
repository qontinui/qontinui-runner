//! Terminal prompt library — coord-backed, versioned prompt templates.
//!
//! Plan `2026-07-24-terminal-prompt-library-command` Phase 3. The Terminal
//! page's `/prompt` modal lists curated prompt documents (coord
//! `prompt_documents` rows with `kind = "prompt_template"`), authored and
//! versioned on the web exactly like policies. Each document's `body` is
//! markdown with YAML frontmatter carrying the typed argument schema
//! (reusing [`crate::skills::SkillParameter`] verbatim — the same shape the
//! skills playbook parser consumes).
//!
//! ## The agent door, not the operator door
//!
//! All fetches go through `/coord/agent-prompt-documents/*` — coord's
//! device/agent (`require_jwt`) sub-router. The sibling
//! `/coord/prompt-documents/*` surface is the OPERATOR door: it resolves
//! tenancy solely from a verified Cognito operator context and 403s a
//! device JWT (confirmed against prod; see the doc comment on
//! [`crate::mcp::continuation_verdict`]'s rules URL). URL-shape regression
//! tests below pin this the same way `continuation_verdict.rs` does.
//!
//! ## Caching
//!
//! The list endpoint returns summaries WITHOUT bodies, so each entry is
//! hydrated with a per-document GET — a bounded N+1 (the library is
//! single-digit sized). Mitigated by a process-global 45s-TTL cache
//! (mirroring `continuation_verdict::CACHE_TTL`) plus the list `ETag` sent
//! back as `If-None-Match` (the `terminal::auto_response_fleet` conditional
//! -fetch pattern): a 304 refreshes the TTL and serves the cache without
//! re-hydrating anything.
//!
//! ## Degrade posture (never a silent empty list)
//!
//! Auth and network failures return `Ok` with a structured
//! `auth: { state: ok | unauthorized | unpaired }` and a human `reason`
//! (the `get_fleet_health` pattern) — never `Err` — so the modal can render
//! an honest "not paired" / "coord unreachable" state. A document whose
//! frontmatter fails to parse degrades to a parameterless prompt carrying
//! its raw body and a `parse_error`, never dropped.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::skills::playbook_parser::split_frontmatter;
use crate::skills::SkillParameter;

/// The coord `prompt_documents.kind` this library serves.
const KIND: &str = "prompt_template";

/// TTL for the process-global library cache. Mirrors
/// `continuation_verdict::CACHE_TTL`'s 45s freshness posture.
const CACHE_TTL: Duration = Duration::from_secs(45);

/// Per-request HTTP timeout (list + each hydration GET).
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

// ===========================================================================
// Wire types (serialized to the frontend in one `invoke`)
// ===========================================================================

/// One prompt template, fully hydrated and frontmatter-parsed in Rust so
/// the frontend never touches YAML.
#[derive(Debug, Clone, Serialize)]
pub struct PromptTemplate {
    /// The document name — the stable slug (`ui-bridge-new-project`).
    pub name: String,
    /// Display title from frontmatter (falls back to the slug).
    pub title: String,
    /// One-line description from the document row.
    pub description: String,
    /// Grouping category from frontmatter (falls back to "General").
    pub category: String,
    /// Optional emoji/icon from frontmatter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// "spawn" | "insert" | "copy" — normalized; unknown values fall back
    /// to "spawn" (the primary, non-programmer path).
    pub default_action: String,
    /// The document's `current_version` (the `v{n}` badge).
    pub version: i64,
    /// Typed argument schema — the existing skills shape, verbatim.
    pub parameters: Vec<SkillParameter>,
    /// Markdown body AFTER frontmatter (the `{{name}}` substitution
    /// target). On a frontmatter parse failure this is the RAW body.
    pub body: String,
    /// Set when the frontmatter failed to parse — the document degrades to
    /// a parameterless prompt rather than being dropped or erroring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// Structured auth state — the `get_fleet_health` contract.
#[derive(Debug, Clone, Serialize)]
pub struct PromptLibraryAuth {
    /// "ok" | "unauthorized" | "unpaired".
    pub state: &'static str,
}

/// The `list_prompt_templates` response.
#[derive(Debug, Clone, Serialize)]
pub struct PromptLibraryResponse {
    pub prompts: Vec<PromptTemplate>,
    pub auth: PromptLibraryAuth,
    /// Human-readable explanation whenever the library is degraded (auth
    /// rejection, coord unreachable, stale-cache fallback) so the UI can
    /// explain an empty or stale list instead of rendering silence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PromptLibraryResponse {
    fn ok(prompts: Vec<PromptTemplate>, reason: Option<String>) -> Self {
        Self {
            prompts,
            auth: PromptLibraryAuth { state: "ok" },
            reason,
        }
    }

    fn auth_degraded(have_token: bool, reason: String) -> Self {
        Self {
            prompts: Vec::new(),
            auth: PromptLibraryAuth {
                state: if have_token {
                    "unauthorized"
                } else {
                    "unpaired"
                },
            },
            reason: Some(reason),
        }
    }
}

// ===========================================================================
// URL builders (the regression-test surface — see tests below)
// ===========================================================================

/// The agent-door LIST url, filtered to prompt templates. Coord scopes the
/// rows to the caller's tenant from the bearer — never pass a tenant here.
fn list_url(base: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents?kind={KIND}",
        base.trim_end_matches('/')
    )
}

/// The agent-door single-document url used to hydrate a summary's body.
///
/// `name` comes back from coord's list response, so it is percent-encoded
/// rather than interpolated raw: a document name carrying a space or slash
/// would otherwise build an unparseable URL (or, worse, a path-traversing
/// one). Slug-shaped names — the norm — encode to themselves.
fn document_url(base: &str, name: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents/{KIND}/{}",
        base.trim_end_matches('/'),
        urlencoding::encode(name)
    )
}

// ===========================================================================
// Frontmatter parsing (pure — unit-tested)
// ===========================================================================

/// The YAML frontmatter schema at the top of a prompt document's body.
/// `parameters` entries are the EXISTING [`SkillParameter`] shape.
#[derive(Debug, Deserialize)]
struct PromptFrontmatter {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    default_action: Option<String>,
    #[serde(default)]
    parameters: Vec<SkillParameter>,
}

/// Normalize a frontmatter `default_action` to the closed set. Unknown or
/// absent values fall back to "spawn" — the primary one-button path.
fn normalize_default_action(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some("insert") => "insert".to_string(),
        Some("copy") => "copy".to_string(),
        Some("spawn") | None => "spawn".to_string(),
        Some(other) => {
            debug!(
                action = other,
                "prompt-library: unknown default_action — using spawn"
            );
            "spawn".to_string()
        }
    }
}

/// Parse one hydrated document into a [`PromptTemplate`].
///
/// Degrade contract: a body with no frontmatter is a valid parameterless
/// prompt (no error); a body whose frontmatter is present but unparseable
/// degrades to a parameterless prompt carrying the RAW body and a
/// `parse_error` — never dropped silently, never a hard error.
fn parse_prompt_document(
    name: &str,
    description: &str,
    version: i64,
    raw_body: &str,
) -> PromptTemplate {
    let degraded = |parse_error: Option<String>| PromptTemplate {
        name: name.to_string(),
        title: name.to_string(),
        description: description.to_string(),
        category: "General".to_string(),
        icon: None,
        default_action: "spawn".to_string(),
        version,
        parameters: Vec::new(),
        body: raw_body.to_string(),
        parse_error,
    };

    let Some((frontmatter_str, body)) = split_frontmatter(raw_body) else {
        // No frontmatter at all — a plain, parameterless prompt.
        return degraded(None);
    };

    if frontmatter_str.trim().is_empty() {
        // An EMPTY `---\n---` block is a legitimate parameterless prompt, not
        // a parse failure: serde_yaml deserializes empty input as a unit value
        // and would reject it, surfacing a bogus `parse_error` in the modal.
        // Keep the body AFTER the delimiters, so the `---` never gets typed
        // into a session.
        return PromptTemplate {
            body: body.to_string(),
            ..degraded(None)
        };
    }

    match serde_yaml::from_str::<PromptFrontmatter>(frontmatter_str) {
        Ok(fm) => PromptTemplate {
            name: name.to_string(),
            title: fm
                .title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| name.to_string()),
            description: description.to_string(),
            category: fm
                .category
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| "General".to_string()),
            icon: fm.icon.filter(|i| !i.trim().is_empty()),
            default_action: normalize_default_action(fm.default_action.as_deref()),
            version,
            parameters: fm.parameters,
            body: body.to_string(),
            parse_error: None,
        },
        Err(e) => {
            warn!(
                document = name,
                error = %e,
                "prompt-library: frontmatter parse failed — degrading to parameterless prompt"
            );
            degraded(Some(format!("frontmatter parse failed: {e}")))
        }
    }
}

// ===========================================================================
// Process-global TTL + ETag cache
// ===========================================================================

#[derive(Clone)]
struct CacheEntry {
    at: Instant,
    etag: Option<String>,
    prompts: Vec<PromptTemplate>,
}

static CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Snapshot the cache: (fresh-entry-if-within-TTL, stored ETag, any-entry).
fn cache_snapshot() -> (Option<CacheEntry>, Option<String>, Option<CacheEntry>) {
    let Ok(g) = cache().lock() else {
        return (None, None, None);
    };
    let any = g.clone();
    let etag = any.as_ref().and_then(|e| e.etag.clone());
    let fresh = any.clone().filter(|e| e.at.elapsed() < CACHE_TTL);
    (fresh, etag, any)
}

fn cache_store(etag: Option<String>, prompts: Vec<PromptTemplate>) {
    if let Ok(mut g) = cache().lock() {
        *g = Some(CacheEntry {
            at: Instant::now(),
            etag,
            prompts,
        });
    }
}

/// On a 304, refresh the TTL stamp so the next 45s serve straight from
/// cache without another conditional round-trip.
fn cache_touch() {
    if let Ok(mut g) = cache().lock() {
        if let Some(entry) = g.as_mut() {
            entry.at = Instant::now();
        }
    }
}

// ===========================================================================
// Response-shape helpers
// ===========================================================================

/// Unwrap coord's row envelope: some surfaces serve the row flat, others
/// under `document` (the `rules_from_doc_body` posture).
fn unwrap_document(body: &Value) -> &Value {
    body.get("document").unwrap_or(body)
}

/// Pull the summary array out of the list response — `documents: [...]`
/// (the served shape), tolerating a bare top-level array.
fn list_documents(body: &Value) -> Vec<Value> {
    if let Some(docs) = body.get("documents").and_then(Value::as_array) {
        return docs.clone();
    }
    body.as_array().cloned().unwrap_or_default()
}

/// Degrade for a NON-auth failure (coord unreachable / non-2xx / undecodable):
/// serve the stale cache when we have one, with an honest reason attached, so a
/// transient blip never blanks an already-loaded library. Takes the cached
/// prompts by value (rather than capturing the cache snapshot in a closure) so
/// the caller can still consult that snapshot on the 304 path.
/// A document whose body could not be fetched at all. It is still LISTED —
/// with its summary metadata and a `parse_error` — because dropping it would
/// make a transport failure look like "this prompt does not exist".
fn hydration_failure(
    name: &str,
    description: String,
    version: i64,
    parse_error: String,
) -> PromptTemplate {
    PromptTemplate {
        name: name.to_string(),
        title: name.to_string(),
        description,
        category: "General".to_string(),
        icon: None,
        default_action: "spawn".to_string(),
        version,
        parameters: Vec::new(),
        body: String::new(),
        parse_error: Some(parse_error),
    }
}

fn stale_fallback(cached: Option<Vec<PromptTemplate>>, reason: String) -> PromptLibraryResponse {
    match cached {
        Some(prompts) => {
            PromptLibraryResponse::ok(prompts, Some(format!("{reason} — showing cached library")))
        }
        None => PromptLibraryResponse::ok(Vec::new(), Some(reason)),
    }
}

// ===========================================================================
// The Tauri command
// ===========================================================================

/// `list_prompt_templates` — fetch the tenant's prompt-template library
/// from coord (agent door, device JWT), hydrate + frontmatter-parse each
/// document, and return typed data in one `invoke`.
///
/// `Ok` in every auth/network outcome (see module docs); `Err` only for
/// local setup failures (HTTP client construction).
#[tauri::command]
pub async fn list_prompt_templates() -> Result<PromptLibraryResponse, String> {
    let base_owned = crate::profiles::coord_base_with_source().0;
    let base = base_owned.trim_end_matches('/');

    // 1. Fresh cache → serve without touching coord.
    let (fresh, etag, any_cached) = cache_snapshot();
    if let Some(entry) = fresh {
        return Ok(PromptLibraryResponse::ok(entry.prompts, None));
    }

    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let have_token = crate::coord_http::have_device_token();

    // The stale-cache payload for the NON-auth degrade paths. Cloned up front
    // so `any_cached` stays available for the 304 path below.
    let cached_prompts = any_cached.as_ref().map(|e| e.prompts.clone());

    // 2. Conditional list fetch.
    let mut req = crate::coord_http::coord_get(&client, list_url(base));
    if let Some(etag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "prompt-library: list fetch failed");
            return Ok(stale_fallback(
                cached_prompts,
                format!("coord unreachable: {e}"),
            ));
        }
    };

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        cache_touch();
        let prompts = any_cached.map(|e| e.prompts).unwrap_or_default();
        return Ok(PromptLibraryResponse::ok(prompts, None));
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // Auth rejection is NOT a transport error: structured auth state,
        // never Err, never a silent empty list.
        return Ok(PromptLibraryResponse::auth_degraded(
            have_token,
            format!(
                "coord rejected the prompt-library read (HTTP {})",
                status.as_u16()
            ),
        ));
    }
    if !status.is_success() {
        warn!(status = status.as_u16(), "prompt-library: list non-2xx");
        return Ok(stale_fallback(
            cached_prompts,
            format!("coord answered HTTP {}", status.as_u16()),
        ));
    }

    // Capture the list ETag before consuming the body.
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let list_body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "prompt-library: list parse failed");
            return Ok(stale_fallback(
                cached_prompts,
                format!("coord list response unparseable: {e}"),
            ));
        }
    };

    // 3. Hydrate each summary (bounded N+1 — single-digit library).
    let mut prompts = Vec::new();
    let mut unauthorized = false;
    for summary in list_documents(&list_body) {
        let Some(name) = summary.get("name").and_then(Value::as_str) else {
            continue;
        };
        let description = summary
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let version = summary
            .get("current_version")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let doc_resp = match crate::coord_http::coord_get(&client, document_url(base, name))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(document = name, error = %e, "prompt-library: hydration fetch failed");
                prompts.push(hydration_failure(
                    name,
                    description,
                    version,
                    format!("fetch failed: {e}"),
                ));
                continue;
            }
        };
        let doc_status = doc_resp.status();
        if doc_status.as_u16() == 401 || doc_status.as_u16() == 403 {
            unauthorized = true;
            continue;
        }
        if !doc_status.is_success() {
            warn!(
                document = name,
                status = doc_status.as_u16(),
                "prompt-library: hydration non-2xx"
            );
            prompts.push(hydration_failure(
                name,
                description,
                version,
                format!("fetch failed: HTTP {}", doc_status.as_u16()),
            ));
            continue;
        }
        let doc_body: Value = match doc_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(document = name, error = %e, "prompt-library: hydration parse failed");
                prompts.push(hydration_failure(
                    name,
                    description,
                    version,
                    format!("response unparseable: {e}"),
                ));
                continue;
            }
        };
        let doc = unwrap_document(&doc_body);
        let raw_body = doc.get("body").and_then(Value::as_str).unwrap_or_default();
        prompts.push(parse_prompt_document(name, &description, version, raw_body));
    }

    if unauthorized && prompts.is_empty() {
        return Ok(PromptLibraryResponse::auth_degraded(
            have_token,
            "coord rejected the prompt-document reads".to_string(),
        ));
    }

    // 4. Store + serve.
    cache_store(new_etag, prompts.clone());
    debug!(count = prompts.len(), "prompt-library: library fetched");
    Ok(PromptLibraryResponse::ok(prompts, None))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── URL-shape regression (mirrors continuation_verdict.rs:876-912) ──

    /// The fetches MUST target coord's device/agent door, never the
    /// operator `TenantId`-gated one: `/coord/prompt-documents/*` 403s a
    /// device JWT (confirmed against prod), and the degrade posture here
    /// would hide that forever as a permanently-empty library.
    #[test]
    fn list_url_uses_the_device_authed_door_not_the_operator_one() {
        let url = list_url("https://coord.example.com");
        assert_eq!(
            url,
            "https://coord.example.com/coord/agent-prompt-documents?kind=prompt_template"
        );
        assert!(
            url.contains("/coord/agent-prompt-documents"),
            "must use the require_jwt door a device JWT can actually read: {url}"
        );
        assert!(
            !url.contains("/coord/prompt-documents/"),
            "the operator TenantId door 403s a device JWT: {url}"
        );
    }

    #[test]
    fn document_url_uses_the_device_authed_door_not_the_operator_one() {
        let url = document_url("https://coord.example.com/", "ui-bridge-new-project");
        assert_eq!(
            url,
            "https://coord.example.com/coord/agent-prompt-documents/prompt_template/ui-bridge-new-project"
        );
        assert!(url.contains("/coord/agent-prompt-documents/"));
        assert!(
            !url.contains("/coord/prompt-documents/"),
            "the operator TenantId door 403s a device JWT: {url}"
        );
    }

    #[test]
    fn document_url_percent_encodes_the_name_and_stays_on_the_agent_door() {
        // Names are server-supplied; a space must not produce an unparseable
        // URL and a slash must not escape the `{kind}/{name}` segment.
        let url = document_url("https://coord.example.com", "odd name/../policy");
        assert!(url.starts_with(
            "https://coord.example.com/coord/agent-prompt-documents/prompt_template/"
        ));
        assert!(!url.contains(' '), "space must be encoded: {url}");
        assert!(
            !url.contains("/../"),
            "a traversal segment must be encoded, not preserved: {url}"
        );
    }

    #[test]
    fn urls_never_carry_a_tenant() {
        // Coord scopes rows from the bearer; a tenant in the URL is the
        // operator-door pattern leaking back in.
        for url in [
            list_url("https://coord.example.com"),
            document_url("https://coord.example.com", "test-fix-loop"),
        ] {
            assert!(
                !url.contains("tenant"),
                "tenant must come from the bearer: {url}"
            );
        }
    }

    // ── Frontmatter → SkillParameter deserialization ─────────────────────

    /// The representative seed frontmatter from the plan's "Prompt document
    /// format" example.
    const SEED_BODY: &str = r#"---
title: Start a new UI Bridge project
category: Getting Started
icon: "🚀"
default_action: spawn
parameters:
  - name: project_name
    type: string
    label: Project name
    description: A short name for your project.
    required: true
    placeholder: my-dashboard
  - name: goal
    type: string
    label: What should it do?
    description: Describe in plain language what you want to build.
    required: true
  - name: language
    type: select
    label: Language
    description: The UI Bridge requires TypeScript or React.
    required: true
    default: TypeScript
    options:
      - { label: TypeScript, value: TypeScript }
      - { label: React, value: React }
---

Create a new {{language}} project called `{{project_name}}`.

Goal: {{goal}}
"#;

    #[test]
    fn seed_frontmatter_parses_into_skill_parameters() {
        let p = parse_prompt_document(
            "ui-bridge-new-project",
            "Start a new UI Bridge project wired to the UI Bridge.",
            3,
            SEED_BODY,
        );
        assert_eq!(p.parse_error, None);
        assert_eq!(p.title, "Start a new UI Bridge project");
        assert_eq!(p.category, "Getting Started");
        assert_eq!(p.icon.as_deref(), Some("🚀"));
        assert_eq!(p.default_action, "spawn");
        assert_eq!(p.version, 3);
        assert_eq!(p.parameters.len(), 3);

        let project_name = &p.parameters[0];
        assert_eq!(project_name.name, "project_name");
        assert_eq!(project_name.param_type, "string");
        assert_eq!(project_name.label, "Project name");
        assert!(project_name.required);
        assert_eq!(project_name.placeholder.as_deref(), Some("my-dashboard"));

        let language = &p.parameters[2];
        assert_eq!(language.param_type, "select");
        assert_eq!(
            language.default,
            Some(serde_json::Value::String("TypeScript".into()))
        );
        let options = language.options.as_ref().expect("select options");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "TypeScript");
        assert_eq!(options[1].value, "React");

        // Body is the markdown AFTER frontmatter.
        assert!(p.body.starts_with("Create a new {{language}} project"));
        assert!(!p.body.contains("---"));
    }

    #[test]
    fn unparseable_frontmatter_degrades_to_parameterless_prompt() {
        // `parameters` is a scalar — cannot deserialize into Vec<SkillParameter>.
        let raw = "---\ntitle: Broken\nparameters: not-a-list\n---\n\nThe body text.\n";
        let p = parse_prompt_document("broken-doc", "desc", 2, raw);
        let err = p.parse_error.as_deref().expect("parse_error must be set");
        assert!(err.contains("frontmatter parse failed"));
        assert!(p.parameters.is_empty());
        // Degrades with the RAW body — the document is never dropped.
        assert_eq!(p.body, raw);
        assert_eq!(p.title, "broken-doc");
        assert_eq!(p.default_action, "spawn");
        assert_eq!(p.version, 2);
    }

    #[test]
    fn body_without_frontmatter_is_a_plain_prompt_not_an_error() {
        let p = parse_prompt_document("plain", "a plain prompt", 1, "Just run the loop.\n");
        assert_eq!(p.parse_error, None);
        assert!(p.parameters.is_empty());
        assert_eq!(p.body, "Just run the loop.\n");
        assert_eq!(p.title, "plain");
        assert_eq!(p.category, "General");
    }

    #[test]
    fn blank_frontmatter_block_is_a_plain_prompt_not_a_parse_error() {
        // A delimited but CONTENTLESS block. `serde_yaml` reads blank input as
        // a unit value and would reject it as a struct — the guard keeps that
        // from surfacing a bogus `parse_error` in the modal.
        let p = parse_prompt_document("blank-fm", "d", 1, "---\n \n---\n\nJust the body.\n");
        assert_eq!(p.parse_error, None, "a blank --- block is not a failure");
        assert!(p.parameters.is_empty());
        // The delimiters must not leak into what gets typed into a session.
        assert_eq!(p.body, "Just the body.\n");
    }

    #[test]
    fn bare_delimiter_pair_has_no_frontmatter_and_keeps_the_raw_body() {
        // `split_frontmatter` needs a `\n---` CLOSER after the opener, so
        // `---\n---` has no closing delimiter and reads as "no frontmatter".
        // Documented rather than assumed: it decides which body the prompt runs.
        let raw = "---\n---\n\nJust the body.\n";
        assert_eq!(split_frontmatter(raw), None);
        let p = parse_prompt_document("bare", "d", 1, raw);
        assert_eq!(p.parse_error, None);
        assert_eq!(p.body, raw);
    }

    #[test]
    fn default_action_normalizes_unknown_values_to_spawn() {
        assert_eq!(normalize_default_action(Some("insert")), "insert");
        assert_eq!(normalize_default_action(Some("copy")), "copy");
        assert_eq!(normalize_default_action(Some("spawn")), "spawn");
        assert_eq!(normalize_default_action(Some("launch")), "spawn");
        assert_eq!(normalize_default_action(None), "spawn");
    }

    #[test]
    fn list_documents_reads_the_documents_envelope_and_bare_arrays() {
        let enveloped = serde_json::json!({"documents": [{"name": "a"}], "total": 1});
        assert_eq!(list_documents(&enveloped).len(), 1);
        let bare = serde_json::json!([{"name": "a"}, {"name": "b"}]);
        assert_eq!(list_documents(&bare).len(), 2);
        assert!(list_documents(&serde_json::json!({})).is_empty());
    }
}
