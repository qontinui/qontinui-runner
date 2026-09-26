//! Skill wire types.
//!
//! A skill is a named, parameterized template that produces pre-configured
//! workflow step(s) when instantiated. These are the types the skill registry
//! (`skills` in the runner binary) serves, persists and exports.
//!
//! ## Why this lives in the lib
//!
//! The schema-export pipeline (`schema_export::export_all_schemas`) is part of
//! this lib crate, and the binary-only `skills` module is invisible to it. The
//! binary's `skills` module re-exports everything here, so there is one Rust
//! definition and the generated TypeScript (`qontinui-schemas`
//! `ts/src/workflow/skill.ts` aliases it) cannot drift from it.
//!
//! ## Schema-only vocabularies
//!
//! Several fields are free `String`s on the wire and stay that way — they are
//! deserialized leniently from prompt-template frontmatter, user rows and
//! community payloads, and tightening them would newly reject input that parses
//! today. Their CLOSED vocabulary (what this program produces and what the
//! frontend renders) is declared once below as a schema-only enum and attached
//! with `#[schemars(with = ...)]`, so the generated bindings carry the literal
//! union without the serde behaviour changing. These enums are never
//! constructed; they exist for `JsonSchema` alone.
//!
//! Likewise every `Option` field that is skipped when `None` is schema'd as its
//! inner type, so the binding reads `?: T` (absent, never `null` — what the
//! wire does) rather than schemars' default `?: T | null`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Schema-only: `SkillDefinition.category`.
#[derive(JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SkillCategory {
    CodeQuality,
    Testing,
    Monitoring,
    AiTask,
    Deployment,
    Composition,
    Custom,
}

/// Schema-only: `SkillParameter.type`.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SkillParameterType {
    String,
    Number,
    Boolean,
    Select,
}

/// Schema-only: one entry of `SkillDefinition.allowed_phases` — the workflow
/// phases a skill's steps may be placed in.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SkillAllowedPhase {
    Setup,
    Verification,
    Agentic,
    Completion,
}

/// Schema-only: `SkillDefinition.approval_status`.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SkillApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

/// Schema-only: `SkillExportManifest.content_type` — a skill export always
/// declares `"skills"`.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SkillExportContentType {
    Skills,
}

/// Schema-only: `SkillDefinition.source`. Names the three values this
/// program's producers emit; [`SkillSource::Other`] preserves a foreign value
/// verbatim on a round trip but is never produced, so it is not part of the
/// published vocabulary.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(title = "SkillSource")]
pub enum SkillSourceSchema {
    Builtin,
    User,
    Community,
}

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillParameterOption {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub url: Option<String>,
}

/// Derive a human-readable label from a parameter `name`.
///
/// `project_name` → `"Project name"`. Splits on `_`, `-` and whitespace, drops
/// empty segments (so `a__b`, `_leading` and `trailing_` never produce doubled,
/// leading or trailing spaces), joins with single spaces, and uppercases the
/// first character. Idempotent on already-humanized input (`"Focus area"` →
/// `"Focus area"`), and never panics — including on `""` and on names whose
/// first character is multi-byte.
pub fn humanize_param_name(name: &str) -> String {
    let joined = name
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let mut chars = joined.chars();
    match chars.next() {
        // `to_uppercase` is char-correct (never byte indexing), so a non-ASCII
        // leading character is widened rather than sliced mid-codepoint.
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A single skill / prompt-template parameter.
///
/// `label`, `description` and `required` stay non-`Option` and carry no
/// `skip_serializing_if`: defaulting happens on the way IN (see the manual
/// [`Deserialize`] impl below), never on the way OUT, so every serialized
/// payload still carries all three keys — which is why the generated schema
/// (and so `qontinui-schemas/ts/src/generated/SkillParameter.d.ts`) marks them
/// required.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkillParameter {
    pub name: String,
    #[serde(rename = "type")]
    #[schemars(with = "SkillParameterType")]
    pub param_type: String,
    pub label: String,
    pub description: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Value")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<SkillParameterOption>")]
    pub options: Option<Vec<SkillParameterOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "f64")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "f64")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ParameterDependency")]
    pub depends_on: Option<ParameterDependency>,
}

/// Wire shape for [`SkillParameter`] deserialization.
///
/// `label`, `description` and `required` are optional here so a non-programmer
/// authoring prompt-template YAML frontmatter can write just `{name, type}`
/// without failing the whole struct — which previously degraded the ENTIRE
/// prompt to parameterless with a `parse_error`.
///
/// This lives in the type's own `Deserialize` rather than in a per-consumer
/// fixup pass on purpose: there are four deserialization sites
/// (`SkillDefinition.parameters`, `CreateSkillRequest`, `UpdateSkillRequest`,
/// `PromptFrontmatter.parameters`) and any of them could silently forget a
/// fixup. Here, sibling access (`label` derived from `name`) happens in exactly
/// one place that no call site can skip.
#[derive(Deserialize)]
struct SkillParameterWire {
    name: String,
    #[serde(rename = "type")]
    param_type: String,
    /// `Option` on the WIRE only. An explicitly-supplied empty label is
    /// preserved as empty; only an ABSENT key is humanized from `name`.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<Value>,
    #[serde(default)]
    options: Option<Vec<SkillParameterOption>>,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    depends_on: Option<ParameterDependency>,
}

impl<'de> Deserialize<'de> for SkillParameter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = SkillParameterWire::deserialize(d)?;
        Ok(SkillParameter {
            label: w.label.unwrap_or_else(|| humanize_param_name(&w.name)),
            name: w.name,
            param_type: w.param_type,
            description: w.description,
            required: w.required,
            default: w.default,
            options: w.options,
            placeholder: w.placeholder,
            min: w.min,
            max: w.max,
            pattern: w.pattern,
            depends_on: w.depends_on,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "SkillParameterDependency")]
pub struct ParameterDependency {
    pub param: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillRef {
    pub skill_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "HashMap<String, Value>")]
    pub parameter_overrides: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind")]
pub enum SkillTemplate {
    #[serde(rename = "single_step")]
    SingleStep { step: HashMap<String, Value> },
    #[serde(rename = "multi_step")]
    MultiStep { steps: Vec<HashMap<String, Value>> },
    #[serde(rename = "composition")]
    Composition { skill_refs: Vec<SkillRef> },
    /// Markdown playbook with domain knowledge (TuriX-CUA inspired).
    ///
    /// Playbooks are human-editable markdown files with YAML frontmatter that
    /// provide LLM context about how to interact with specific applications.
    /// Unlike other templates that generate workflow steps, playbooks inject
    /// domain knowledge into AI prompts.
    #[serde(rename = "playbook")]
    Playbook {
        /// Full markdown content (the body after frontmatter).
        content: String,
        /// Trigger conditions for when this playbook should be included.
        #[serde(default)]
        triggers: Vec<PlaybookTrigger>,
    },
}

/// Trigger condition for a playbook.
///
/// Determines when a playbook should be automatically included in AI prompts
/// based on the current automation context (app name, URL pattern, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(title = "SkillPlaybookTrigger")]
pub struct PlaybookTrigger {
    /// Type of trigger: "app_name", "url_pattern", "tag".
    pub trigger_type: String,
    /// Value to match against (exact match for app_name, glob for url_pattern).
    pub value: String,
}

/// Where a [`SkillDefinition`] came from.
///
/// This was a bare `String` documented in place as
/// `"builtin" | "user" | "community"` — the one place in this crate where the
/// provenance pattern `crate::agent_commands::CommandSource` holds as a type
/// had decayed into free text, so `crate::capability_manifest` had to PARSE a
/// string to answer a question the type system should hold. Plan
/// `2026-08-31-published-build-parity-check` Phase 3 gives it the same typed
/// treatment.
///
/// # Why there is an [`Other`](Self::Other) variant, unlike `CommandSource`
///
/// `CommandSource` values are minted only inside this binary, so its variants
/// can be closed. A `SkillDefinition` is also DESERIALIZED — out of this
/// device's `user_skills` table (`database::pg::skills`) and out of an import
/// or community payload written by another program version. A closed enum
/// would turn an unrecognised `source` into a hard parse failure for the whole
/// skill, which is a behaviour change (those paths accept the row today), and
/// coercing it to `User` instead would be a silent guess. `Other` keeps the
/// original bytes verbatim, so the value round-trips losslessly, no row is
/// newly rejected, and `capability_manifest::rung_for_skill_source` can match
/// on a NAMED variant rather than on an unbounded string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// Compiled into the binary: `BUILTIN_SKILLS_JSON` is
    /// `include_str!("builtin.json")`.
    Builtin,
    /// Authored on this device and stored in its own `user_skills` table.
    User,
    /// Pulled from an organization/community registry over HTTP by
    /// `mcp::skills::sync_pull` and then PERSISTED into the same local table by
    /// `PgDb::import_skills`. Both halves of that sentence matter — see
    /// `capability_manifest::rung_for_skill_source` for why the two halves name
    /// different rungs and why the mapping therefore refuses to pick one.
    Community,
    /// A value none of this binary's producers emit, preserved verbatim so it
    /// survives a read/write round trip instead of being guessed at.
    Other(String),
}

impl SkillSource {
    /// The wire string. Mirrors `CommandSource::as_str`; this is the value that
    /// is serialized, stored in `user_skills.source`, and read by
    /// `capability_manifest::rung_for_skill_source`.
    #[must_use]
    pub fn wire(&self) -> &str {
        match self {
            SkillSource::Builtin => "builtin",
            SkillSource::User => "user",
            SkillSource::Community => "community",
            SkillSource::Other(raw) => raw.as_str(),
        }
    }

    /// Total: every string maps, and an unrecognised one becomes
    /// [`Other`](Self::Other) rather than a default or an error.
    ///
    /// Compared **case-sensitively and untrimmed**: these are wire values
    /// written by producing code, not operator input, so normalising here would
    /// quietly accept a shape no producer emits and hide a real drift.
    #[must_use]
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "builtin" => SkillSource::Builtin,
            "user" => SkillSource::User,
            "community" => SkillSource::Community,
            other => SkillSource::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for SkillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.wire())
    }
}

/// Serialized as the bare wire string, so the JSON shape is byte-identical to
/// what the `String` field produced.
impl Serialize for SkillSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire())
    }
}

/// Deserialized from any string via [`SkillSource::from_wire`], which is total —
/// so no payload that parsed before this type existed fails to parse now.
impl<'de> Deserialize<'de> for SkillSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(SkillSource::from_wire(&raw))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    #[schemars(with = "SkillCategory")]
    pub category: String,
    pub tags: Vec<String>,
    pub icon: String,
    pub color: String,
    #[schemars(with = "Vec<SkillAllowedPhase>")]
    pub allowed_phases: Vec<String>,
    pub parameters: Vec<SkillParameter>,
    pub template: SkillTemplate,
    /// Provenance. Typed rather than free text — see [`SkillSource`].
    #[schemars(with = "SkillSourceSchema")]
    pub source: SkillSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SkillAuthor")]
    pub author: Option<SkillAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<String>")]
    pub depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "u64")]
    pub usage_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SkillApprovalStatus")]
    pub approval_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub forked_from: Option<String>,
}

/// Tracks that a step was created from a skill
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillOrigin {
    pub skill_id: String,
    pub skill_slug: String,
    pub parameter_values: HashMap<String, Value>,
}

// =============================================================================
// Export / Import Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillExportManifest {
    pub version: String,
    pub exported_at: String,
    pub app_version: String,
    #[schemars(with = "SkillExportContentType")]
    pub content_type: String,
    pub skill_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SkillExport {
    pub manifest: SkillExportManifest,
    pub skills: Vec<SkillDefinition>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkillImportResult {
    pub imported: usize,
    pub skipped: usize,
    pub overwritten: usize,
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
