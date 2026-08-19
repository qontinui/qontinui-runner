//! Model Capability Catalog
//!
//! What a model or backend supports, held as **data** rather than inferred
//! from substrings of its name.
//!
//! # Why this module exists
//!
//! Before it, the runner decided capability questions with five independent
//! hand-maintained substring ladders — `ai_router::sanitize_model_ids`,
//! `ai_pricing::get_pricing`, `ai_provider::cache_aware_builder::min_cacheable_chars`,
//! the eight `starts_with("claude")` guards in `ai_provider::routing`, and
//! `meta_optimizer::cost_optimizer`. Each rotted independently, and at least
//! one rotted into a live bug: a correctly configured `claude-opus-5` was
//! silently rewritten to `claude-opus-4-20250514`.
//!
//! # The tri-state is the point
//!
//! [`CapabilityState`] is deliberately three-valued. **An absent catalog fact
//! is `Unknown`, not `Unsupported`.** A boolean cannot represent the state that
//! actually causes the bug — "we never asked" — and coercing it to "no"
//! silently disables working capabilities. This is the fleet's
//! `silent-empty-is-unknown` rule expressed in the type system.
//!
//! Concretely: [`CapabilityState::Unknown`] must never be a reason to strip
//! content or refuse a route. Callers that must act on an unknown fact should
//! attempt the capability and let the backend be the authority, because a
//! provider error is a better signal than a silent local downgrade.
//!
//! # Two layers
//!
//! Model-level *facts* ([`ModelFacts`]) are held separately from the *API
//! shape* ([`ApiShape`]) the model is reached through, because provider quirks
//! cluster by API shape, not by model. A one-layer profile keyed on model id
//! duplicates every quirk per model and drifts.

#![allow(dead_code)]

// ============================================================================
// Capability state
// ============================================================================

/// Three-valued capability fact.
///
/// `Unknown` is the default for anything the catalog does not state. It is a
/// distinct value from `Unsupported` on purpose — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapabilityState {
    /// The catalog states the capability is available.
    Supported,
    /// The catalog states the capability is NOT available.
    Unsupported,
    /// The catalog says nothing. **Not** a synonym for `Unsupported`.
    ///
    /// This is the `Default` on purpose: a `CapabilityState` that nobody set
    /// must read as "we never asked", never as "no".
    #[default]
    Unknown,
}

impl CapabilityState {
    /// True only when the catalog positively states support.
    ///
    /// Do NOT use this to decide whether to strip content — `Unknown` returns
    /// `false` here, and treating that as "strip" is the exact coercion this
    /// type exists to prevent. Use [`Self::is_known_unsupported`] for that.
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// True only when the catalog positively states the capability is absent.
    ///
    /// This is the correct predicate for "should I downgrade?": an `Unknown`
    /// fact answers `false`, so the capability is attempted and the backend
    /// gets to be the authority.
    pub fn is_known_unsupported(self) -> bool {
        matches!(self, Self::Unsupported)
    }

    /// True when the catalog has no opinion.
    pub fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

// ============================================================================
// API shape (the second layer)
// ============================================================================

/// The wire protocol a model is reached through.
///
/// Quirks cluster here rather than on the model — every model behind
/// `AnthropicMessages` shares its content-block rules regardless of family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiShape {
    /// Anthropic Messages API (`POST /v1/messages`).
    AnthropicMessages,
    /// Google Generative Language API.
    GoogleGenerativeAi,
    /// OpenAI Chat Completions and compatible endpoints (vLLM, LM Studio, …).
    OpenAiCompletions,
}

// ============================================================================
// Model facts
// ============================================================================

/// Price per million tokens, in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Cost {
    pub const fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }
}

/// Everything the catalog knows about one model.
///
/// Every field that can legitimately be unknown is an `Option` or a
/// [`CapabilityState`] — there are no sentinel defaults standing in for
/// missing knowledge.
#[derive(Debug, Clone, Copy)]
pub struct ModelFacts {
    /// Canonical model id.
    pub id: &'static str,
    /// Wire protocol this model is reached through.
    pub api_shape: ApiShape,
    /// Maximum input context in tokens. `None` = not stated.
    pub context_window: Option<u32>,
    /// Maximum output tokens per response. `None` = not stated.
    pub max_output: Option<u32>,
    /// Token pricing. `None` = not stated — cost is then UNKNOWN, and callers
    /// must not silently record zero.
    pub cost: Option<Cost>,
    /// Whether the model accepts image content blocks.
    pub image_input: CapabilityState,
    /// Minimum block size in CHARACTERS for `cache_control: ephemeral` to
    /// actually fire. `None` = not stated.
    ///
    /// Characters rather than tokens because the request builder operates on
    /// `String::len()`; the values are `documented_token_min × 4`.
    pub cache_min_block_chars: Option<usize>,
    /// The model to fall back to when this one is unavailable. Every declared
    /// target MUST resolve in this catalog — enforced by
    /// `guardrail_every_fallback_target_resolves`.
    pub fallback: Option<&'static str>,
    /// The provider no longer serves this model.
    ///
    /// A retired model stays in the catalog because **knowing a model's price
    /// and knowing it is routable are different questions** — historical cost
    /// records still need the price long after the endpoint stops answering.
    /// So [`cost_for`] happily prices a retired model, while
    /// [`is_recognized_id`] refuses it as a routing target.
    pub retired: bool,
}

impl ModelFacts {
    /// A wholly-unknown model. Every fact is absent; nothing is asserted.
    const fn unknown(id: &'static str, api_shape: ApiShape) -> Self {
        Self {
            id,
            api_shape,
            context_window: None,
            max_output: None,
            cost: None,
            image_input: CapabilityState::Unknown,
            cache_min_block_chars: None,
            fallback: None,
            retired: false,
        }
    }
}

// ============================================================================
// The catalog snapshot
// ============================================================================

/// Checked-in capability snapshot.
///
/// **Pricing values are carried over verbatim from the `ai_pricing` ladder
/// this catalog replaces** so the migration is a fact-for-fact swap with no
/// silent repricing. Where a price is genuinely not known here it is `None`
/// (UNKNOWN), never a guess — the Claude 5 family is the current example, and
/// its `None` reproduces exactly what the old `get_pricing` returned for those
/// ids (nothing), only now the absence is explicit rather than a fall-through.
pub const CATALOG: &[ModelFacts] = &[
    // ---- Claude 5 family -------------------------------------------------
    // Cost and context are NOT stated here. That is a deliberate `Unknown`,
    // not an oversight: guessing a price silently corrupts cost accounting,
    // and the old ladder already returned nothing for these ids.
    ModelFacts {
        id: "claude-opus-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: None,
        max_output: None,
        cost: None,
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-sonnet-5"),
        retired: false,
    },
    ModelFacts {
        id: "claude-sonnet-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: None,
        max_output: None,
        cost: None,
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-haiku-4-5"),
        retired: false,
    },
    ModelFacts {
        id: "claude-fable-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: None,
        max_output: None,
        cost: None,
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-sonnet-5"),
        retired: false,
    },
    // ---- Claude 4.x ------------------------------------------------------
    ModelFacts {
        id: "claude-opus-4-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(15.0, 75.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-sonnet-4"),
        retired: false,
    },
    ModelFacts {
        id: "claude-opus-4",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(15.0, 75.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-sonnet-4"),
        retired: false,
    },
    ModelFacts {
        id: "claude-sonnet-4",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(3.0, 15.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: Some("claude-haiku-4-5"),
        retired: false,
    },
    ModelFacts {
        id: "claude-haiku-4-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(0.80, 4.0)),
        image_input: CapabilityState::Supported,
        // Haiku 4.x needs a 4096-TOKEN block before caching fires.
        cache_min_block_chars: Some(16384),
        fallback: None,
        retired: false,
    },
    // ---- Claude 3.x (legacy, still served) -------------------------------
    ModelFacts {
        id: "claude-3-5-sonnet",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(3.0, 15.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: None,
        retired: false,
    },
    ModelFacts {
        // Modern naming for the same model as `claude-3-5-haiku` below. Both
        // spellings are served, so both are catalog entries rather than one
        // being an alias — they carry identical facts.
        id: "claude-haiku-3-5",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(0.25, 1.25)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(8192),
        fallback: None,
        retired: false,
    },
    ModelFacts {
        id: "claude-3-5-haiku",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(0.25, 1.25)),
        image_input: CapabilityState::Supported,
        // Haiku 3.x: 2048-token floor.
        cache_min_block_chars: Some(8192),
        fallback: None,
        retired: false,
    },
    ModelFacts {
        id: "claude-3-opus",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(15.0, 75.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: None,
        retired: true,
    },
    ModelFacts {
        id: "claude-3-sonnet",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(3.0, 15.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(4096),
        fallback: None,
        retired: true,
    },
    ModelFacts {
        id: "claude-3-haiku",
        api_shape: ApiShape::AnthropicMessages,
        context_window: Some(200_000),
        max_output: None,
        cost: Some(Cost::new(0.25, 1.25)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: Some(8192),
        fallback: None,
        retired: true,
    },
    // ---- Gemini ----------------------------------------------------------
    ModelFacts {
        id: "gemini-2-0-flash",
        api_shape: ApiShape::GoogleGenerativeAi,
        context_window: Some(1_000_000),
        max_output: None,
        cost: Some(Cost::new(0.10, 0.40)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: None,
        fallback: None,
        retired: false,
    },
    ModelFacts {
        id: "gemini-1-5-pro",
        api_shape: ApiShape::GoogleGenerativeAi,
        context_window: Some(2_000_000),
        max_output: None,
        cost: Some(Cost::new(1.25, 5.0)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: None,
        fallback: Some("gemini-1-5-flash"),
        retired: false,
    },
    ModelFacts {
        id: "gemini-1-5-flash",
        api_shape: ApiShape::GoogleGenerativeAi,
        context_window: Some(1_000_000),
        max_output: None,
        cost: Some(Cost::new(0.075, 0.30)),
        image_input: CapabilityState::Supported,
        cache_min_block_chars: None,
        fallback: None,
        retired: false,
    },
];

// ============================================================================
// Aliases
// ============================================================================

/// Short names and legacy spellings that resolve onto a catalog entry.
///
/// Carried over from `ai_pricing::get_pricing`'s generic-short-form branch so
/// the swap preserves behaviour exactly.
const ALIASES: &[(&str, &str)] = &[
    ("opus", "claude-3-opus"),
    ("sonnet", "claude-3-5-sonnet"),
    ("haiku", "claude-3-5-haiku"),
    ("claude", "claude-3-5-sonnet"),
    ("claude-cli", "claude-3-5-sonnet"),
    ("gemini-pro", "gemini-1-5-pro"),
    ("gemini-flash", "gemini-1-5-flash"),
];

// ============================================================================
// Lookup
// ============================================================================

/// Model families in Anthropic's `claude-<family>-<generation>` id scheme.
pub const CLAUDE_FAMILIES: &[&str] = &["haiku", "sonnet", "opus", "fable"];

/// Resolve a model id to its catalog facts.
///
/// Resolution order — most specific first, so a dated id
/// (`claude-opus-4-20250514`) resolves to the `claude-opus-4` entry rather
/// than to a shorter, wronger prefix:
///
/// 1. exact id match,
/// 2. alias table,
/// 3. LONGEST catalog id that is a prefix of the query.
///
/// Returns `None` when nothing matches. `None` means **unknown**, not
/// unsupported — callers must not turn it into a refusal or a downgrade.
pub fn lookup(model_id: &str) -> Option<&'static ModelFacts> {
    let q = normalize_id(model_id);
    if q.is_empty() {
        return None;
    }
    lookup_normalized(&q)
}

/// Fold a model id into the catalog's canonical spelling: lowercase, trimmed,
/// and dots replaced by dashes.
///
/// Providers spell the same model both ways (`claude-opus-4.5` and
/// `claude-opus-4-5`), and the ladder this catalog replaces matched both. The
/// fold happens ONCE, before any matching, so the whole id space is uniformly
/// dashed. Doing it as a fallback pass instead is subtly wrong: `claude-opus-4.5`
/// prefix-matches `claude-opus-4` on the un-folded first pass, so a
/// more-specific `claude-opus-4-5` entry would never be reached.
fn normalize_id(model_id: &str) -> String {
    model_id.trim().to_ascii_lowercase().replace('.', "-")
}

/// One resolution pass over an already-lowercased, already-trimmed query.
fn lookup_normalized(q: &str) -> Option<&'static ModelFacts> {
    if let Some(f) = CATALOG.iter().find(|f| f.id == q) {
        return Some(f);
    }
    if let Some((_, target)) = ALIASES.iter().find(|(alias, _)| *alias == q) {
        // Alias targets are catalog ids by construction; the guardrail test
        // enforces it, so a miss here is a catalog bug, not a caller error.
        return CATALOG.iter().find(|f| f.id == *target);
    }
    // Longest-prefix wins: `claude-opus-4-5-x` must not match `claude-opus-4`.
    if let Some(f) = CATALOG
        .iter()
        .filter(|f| q.starts_with(f.id))
        .max_by_key(|f| f.id.len())
    {
        return Some(f);
    }
    // Bare-family SUFFIX, e.g. a vendor-prefixed `anthropic/claude-3-opus` or
    // any `…-opus`. The ladder this replaces ended with
    // `model_lower.ends_with("-opus")` and friends, so dropping it would
    // silently unprice ids that are priced today. Deliberately narrow: only
    // the bare family names, never arbitrary catalog ids, so a suffix match
    // cannot pull an unrelated model in.
    BARE_FAMILY_ALIASES
        .iter()
        .find(|(family, _)| q.ends_with(&format!("-{family}")))
        .and_then(|(_, target)| CATALOG.iter().find(|f| f.id == *target))
}

/// Bare family names, and the model each one prices as. Used for both the
/// exact-alias match and the `-<family>` suffix match.
const BARE_FAMILY_ALIASES: &[(&str, &str)] = &[
    ("opus", "claude-3-opus"),
    ("sonnet", "claude-3-5-sonnet"),
    ("haiku", "claude-3-5-haiku"),
];

/// Whether this id names a model the runner should accept as configuration.
///
/// Generation-agnostic by design: a known FAMILY prefix
/// (`claude-opus-`, `claude-sonnet-`, …) is accepted even when the specific
/// generation is not in the catalog yet, because a model id the catalog has
/// not heard of is **`Unknown`, not invalid** — and rejecting it is what
/// silently downgraded `claude-opus-5`. Only ids matching no family and no
/// catalog entry are reported unknown.
pub fn is_recognized_id(model_id: &str) -> bool {
    let q = normalize_id(model_id);
    if q.is_empty() {
        return false;
    }
    // A known FAMILY is enough, whatever the generation — this is what stops
    // the next model release from silently downgrading a correct config.
    if CLAUDE_FAMILIES
        .iter()
        .any(|family| q.starts_with(&format!("claude-{family}-")))
    {
        return true;
    }
    // Otherwise the catalog must know it AND still serve it. A retired model
    // keeps its price (for historical cost records) but is not a routing
    // target — see `ModelFacts::retired`.
    lookup(&q).is_some_and(|f| !f.retired)
}

/// Token pricing for a model, or `None` when the catalog does not state it.
///
/// Replaces `ai_pricing::get_pricing`'s substring ladder. `None` keeps its old
/// meaning — cost unknown — and callers must not record it as zero.
pub fn cost_for(model_id: &str) -> Option<Cost> {
    lookup(model_id).and_then(|f| f.cost)
}

/// Whether a model accepts image content blocks.
///
/// Returns [`CapabilityState::Unknown`] for anything the catalog does not
/// state. **Do not strip images on `Unknown`** — attempt the send and let the
/// backend answer.
pub fn image_input(model_id: &str) -> CapabilityState {
    lookup(model_id)
        .map(|f| f.image_input)
        .unwrap_or(CapabilityState::Unknown)
}

/// Maximum input context in tokens, when stated.
pub fn context_window(model_id: &str) -> Option<u32> {
    lookup(model_id).and_then(|f| f.context_window)
}

/// The prompt-cache minimum block size in characters, when stated.
pub fn cache_min_block_chars(model_id: &str) -> Option<usize> {
    lookup(model_id).and_then(|f| f.cache_min_block_chars)
}

/// The conservative floor used when the catalog does not state one.
///
/// 4096 chars = the 1024-token minimum that every non-Haiku Anthropic model
/// documents. Getting this too LOW means a `cache_control` marker is silently
/// ignored by the API (0 cache reads, no error); too HIGH means a cacheable
/// block goes unmarked. Both cost only cache efficiency, never correctness,
/// which is why an `Unknown` here is allowed to resolve to a number at all —
/// unlike [`image_input`], where the same coercion would drop user content.
pub const DEFAULT_CACHE_MIN_BLOCK_CHARS: usize = 4096;

/// The prompt-cache minimum for a model.
///
/// Resolution: the catalog's per-model value, else the documented FAMILY rule
/// below, else [`DEFAULT_CACHE_MIN_BLOCK_CHARS`].
///
/// The family rule exists because the cache floor is genuinely a property of
/// the model family and generation, not of the individual dated release —
/// Anthropic documents a 4096-token minimum for Haiku 4.x and 2048 for Haiku
/// 3.x, against 1024 for everything else. Encoding it as a rule rather than
/// enumerating every dated id is what lets an unreleased `claude-haiku-4-9`
/// inherit the right floor instead of silently taking one four times too small.
pub fn cache_min_block_chars_or_default(model_id: &str) -> usize {
    if let Some(stated) = cache_min_block_chars(model_id) {
        return stated;
    }
    let q = normalize_id(model_id);
    // Both naming schemes: `claude-haiku-4-*` and the legacy `claude-4-haiku`
    // ordering the old ladder caught with a bare `contains`.
    if q.contains("haiku-4") {
        16384
    } else if q.contains("haiku-3") {
        8192
    } else {
        DEFAULT_CACHE_MIN_BLOCK_CHARS
    }
}

/// The API shape a model is reached through, when known.
pub fn api_shape(model_id: &str) -> Option<ApiShape> {
    lookup(model_id).map(|f| f.api_shape)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Guardrails: these fail the BUILD when the catalog is malformed ----

    /// Every declared fallback target must resolve to a real catalog entry.
    ///
    /// This is the build-time chain validation: a fallback pointing at a model
    /// that does not exist is a route that dead-ends at runtime, discovered
    /// only when the primary is already failing.
    #[test]
    fn guardrail_every_fallback_target_resolves() {
        for facts in CATALOG {
            if let Some(target) = facts.fallback {
                assert!(
                    CATALOG.iter().any(|f| f.id == target),
                    "model `{}` declares fallback `{}`, which is not in the catalog",
                    facts.id,
                    target
                );
            }
        }
    }

    /// Every alias must resolve to a real catalog entry.
    #[test]
    fn guardrail_every_alias_target_resolves() {
        for (alias, target) in ALIASES {
            assert!(
                CATALOG.iter().any(|f| f.id == *target),
                "alias `{alias}` targets `{target}`, which is not in the catalog"
            );
        }
    }

    /// Catalog ids must be unique — a duplicate makes `lookup` order-dependent.
    #[test]
    fn guardrail_catalog_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for facts in CATALOG {
            assert!(seen.insert(facts.id), "duplicate catalog id `{}`", facts.id);
        }
    }

    /// Ids must be lowercase — `lookup` lowercases the query, so a
    /// mixed-case catalog id would be unreachable.
    #[test]
    fn guardrail_catalog_ids_are_lowercase() {
        for facts in CATALOG {
            assert_eq!(
                facts.id,
                facts.id.to_ascii_lowercase(),
                "catalog id `{}` is not lowercase and would be unreachable",
                facts.id
            );
        }
    }

    /// No model may declare itself as its own fallback.
    #[test]
    fn guardrail_no_self_fallback() {
        for facts in CATALOG {
            assert_ne!(
                facts.fallback,
                Some(facts.id),
                "model `{}` declares itself as its own fallback",
                facts.id
            );
        }
    }

    /// Fallback chains must terminate — no cycles.
    #[test]
    fn guardrail_fallback_chains_terminate() {
        for start in CATALOG {
            let mut seen = std::collections::HashSet::new();
            let mut cur = start.id;
            seen.insert(cur);
            while let Some(next) = CATALOG
                .iter()
                .find(|f| f.id == cur)
                .and_then(|f| f.fallback)
            {
                assert!(
                    seen.insert(next),
                    "fallback chain from `{}` cycles at `{}`",
                    start.id,
                    next
                );
                cur = next;
            }
        }
    }

    /// A fixture whose fallback target is absent must be caught. This asserts
    /// the guardrail above has teeth, rather than passing vacuously.
    #[test]
    fn guardrail_detects_a_broken_chain() {
        const BROKEN: &[ModelFacts] = &[ModelFacts {
            fallback: Some("model-that-does-not-exist"),
            ..ModelFacts::unknown("fixture-model", ApiShape::AnthropicMessages)
        }];
        let broken_targets: Vec<_> = BROKEN
            .iter()
            .filter_map(|f| f.fallback)
            .filter(|t| !CATALOG.iter().any(|c| c.id == *t))
            .collect();
        assert_eq!(
            broken_targets,
            vec!["model-that-does-not-exist"],
            "the fallback-resolution check does not actually detect a broken target"
        );
    }

    // ---- Tri-state semantics ----------------------------------------------

    /// The load-bearing invariant: an absent fact is `Unknown`, and `Unknown`
    /// is NOT `Unsupported`.
    #[test]
    fn unknown_never_coerces_to_unsupported() {
        let state = image_input("some-model-nobody-has-heard-of");
        assert_eq!(state, CapabilityState::Unknown);
        assert!(state.is_unknown());
        assert!(
            !state.is_known_unsupported(),
            "an UNKNOWN capability must never answer `is_known_unsupported` — \
             that is the coercion that silently disables working features"
        );
        assert!(!state.is_supported());
    }

    /// `is_supported` and `is_known_unsupported` are not complements. If they
    /// ever become complements, the tri-state has collapsed to a boolean.
    #[test]
    fn supported_and_known_unsupported_are_not_complements() {
        for state in [
            CapabilityState::Supported,
            CapabilityState::Unsupported,
            CapabilityState::Unknown,
        ] {
            assert!(
                !(state.is_supported() && state.is_known_unsupported()),
                "{state:?} claims both"
            );
        }
        assert!(
            !CapabilityState::Unknown.is_supported()
                && !CapabilityState::Unknown.is_known_unsupported(),
            "Unknown must answer false to BOTH predicates"
        );
    }

    #[test]
    fn default_capability_state_is_unknown() {
        assert_eq!(CapabilityState::default(), CapabilityState::Unknown);
    }

    // ---- Lookup -----------------------------------------------------------

    #[test]
    fn exact_ids_resolve() {
        assert_eq!(lookup("claude-opus-5").unwrap().id, "claude-opus-5");
        assert_eq!(lookup("gemini-1.5-pro").unwrap().id, "gemini-1-5-pro");
    }

    #[test]
    fn lookup_is_case_insensitive_and_trims() {
        assert_eq!(lookup("  CLAUDE-Opus-5 ").unwrap().id, "claude-opus-5");
    }

    /// Dated ids must resolve to the LONGEST matching prefix, not the first.
    #[test]
    fn dated_ids_resolve_to_the_longest_prefix() {
        assert_eq!(
            lookup("claude-opus-4-20250514").unwrap().id,
            "claude-opus-4"
        );
        // `claude-opus-4-5-...` must NOT collapse onto `claude-opus-4`.
        assert_eq!(
            lookup("claude-opus-4-5-20251101").unwrap().id,
            "claude-opus-4-5"
        );
    }

    /// Dotted and dashed spellings of the same model must agree — the ladder
    /// this replaces matched both, and dropping one would silently unprice a
    /// model whose id merely used the other separator.
    #[test]
    fn dotted_and_dashed_spellings_agree() {
        for (dotted, dashed) in [
            ("claude-3.5-sonnet", "claude-3-5-sonnet"),
            ("claude-3.5-haiku", "claude-3-5-haiku"),
            ("claude-opus-4.5", "claude-opus-4-5"),
        ] {
            assert_eq!(
                lookup(dotted).map(|f| f.id),
                Some(dashed),
                "`{dotted}` should resolve to `{dashed}`"
            );
        }
        // Dotted provider spellings fold onto the dashed canonical id.
        assert_eq!(lookup("gemini-1.5-pro").unwrap().id, "gemini-1-5-pro");
        assert_eq!(lookup("gemini-2.0-flash").unwrap().id, "gemini-2-0-flash");
    }

    /// Vendor-prefixed and otherwise-decorated ids ending in a bare family
    /// name still price, exactly as the `ends_with("-opus")` ladder did.
    #[test]
    fn bare_family_suffixes_still_price() {
        assert_eq!(
            lookup("anthropic/claude-3-opus").unwrap().id,
            "claude-3-opus"
        );
        assert_eq!(
            lookup("some-vendor-sonnet").unwrap().id,
            "claude-3-5-sonnet"
        );
        assert_eq!(lookup("bedrock-haiku").unwrap().id, "claude-3-5-haiku");
        // A suffix match must not pull in unrelated models.
        assert!(lookup("gpt-4").is_none());
        assert!(lookup("llama-3-70b").is_none());
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(lookup("opus").unwrap().id, "claude-3-opus");
        assert_eq!(lookup("claude-cli").unwrap().id, "claude-3-5-sonnet");
    }

    #[test]
    fn unknown_model_returns_none_not_a_guess() {
        assert!(lookup("llama-3-70b").is_none());
        assert!(cost_for("llama-3-70b").is_none());
        assert!(context_window("llama-3-70b").is_none());
    }

    #[test]
    fn empty_id_resolves_to_nothing() {
        assert!(lookup("").is_none());
        assert!(lookup("   ").is_none());
        assert!(!is_recognized_id(""));
    }

    // ---- Recognition (the ai_router consumer) ------------------------------

    /// A current-generation id is recognized even though the catalog may not
    /// carry that exact entry — the family prefix is enough.
    #[test]
    fn future_generations_are_recognized_by_family() {
        for id in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-fable-5",
            "claude-opus-6", // does not exist yet — must still pass
            "claude-sonnet-7-20300101",
        ] {
            assert!(is_recognized_id(id), "`{id}` should be recognized");
        }
    }

    #[test]
    fn retired_and_foreign_ids_are_not_recognized() {
        for id in [
            "claude-3-opus-20240229",
            "claude-2.1",
            "gpt-4-turbo",
            "garbage",
        ] {
            assert!(!is_recognized_id(id), "`{id}` should NOT be recognized");
        }
    }

    /// Legacy models the provider still serves stay routable.
    #[test]
    fn legacy_still_served_ids_are_recognized() {
        for id in [
            "claude-3-5-sonnet-20240620",
            "claude-3-5-haiku",
            "gemini-1.5-pro",
        ] {
            assert!(is_recognized_id(id), "`{id}` should be recognized");
        }
    }

    /// A retired model keeps its PRICE even though it is not routable — the
    /// two questions are separate, and historical cost records need the price.
    #[test]
    fn retired_models_keep_their_price() {
        assert!(!is_recognized_id("claude-3-opus-20240229"));
        assert_eq!(
            cost_for("claude-3-opus-20240229")
                .unwrap()
                .input_per_million,
            15.0,
            "a retired model must still price historical usage"
        );
    }

    // ---- Cost parity with the ladder this replaces -------------------------

    /// The prices carried over must match the `ai_pricing` ladder exactly, so
    /// the Phase 3 swap cannot silently reprice anything.
    #[test]
    fn cost_parity_with_the_replaced_ladder() {
        let cases: &[(&str, f64, f64)] = &[
            ("claude-3-5-sonnet-20240620", 3.0, 15.0),
            ("claude-3-opus-20240229", 15.0, 75.0),
            ("claude-3-haiku", 0.25, 1.25),
            ("claude-opus-4-20250514", 15.0, 75.0),
            ("claude-sonnet-4-20250514", 3.0, 15.0),
            ("claude-haiku-4-5-20251001", 0.80, 4.0),
            ("gemini-1.5-pro", 1.25, 5.0),
            ("gemini-1.5-flash", 0.075, 0.30),
            ("gemini-2.0-flash", 0.10, 0.40),
            ("opus", 15.0, 75.0),
            ("sonnet", 3.0, 15.0),
            ("haiku", 0.25, 1.25),
            ("claude-cli", 3.0, 15.0),
        ];
        for (id, input, output) in cases {
            let cost = cost_for(id).unwrap_or_else(|| panic!("no cost for `{id}`"));
            assert_eq!(cost.input_per_million, *input, "input price for `{id}`");
            assert_eq!(cost.output_per_million, *output, "output price for `{id}`");
        }
    }

    /// The Claude 5 family has NO stated price — and that must stay an honest
    /// `None` rather than drifting into a guess.
    #[test]
    fn claude_5_cost_is_unknown_not_guessed() {
        for id in ["claude-opus-5", "claude-sonnet-5", "claude-fable-5"] {
            assert!(
                cost_for(id).is_none(),
                "`{id}` has a price in the catalog — if it is now published, add it \
                 deliberately; do not let a guess land here"
            );
        }
    }

    // ---- Cache floor parity ------------------------------------------------

    /// Parity with `cache_aware_builder::min_cacheable_chars`.
    #[test]
    fn cache_floor_parity_with_the_replaced_ladder() {
        assert_eq!(
            cache_min_block_chars_or_default("claude-haiku-4-5-20251001"),
            16384
        );
        assert_eq!(cache_min_block_chars_or_default("claude-3-5-haiku"), 8192);
        assert_eq!(cache_min_block_chars_or_default("claude-sonnet-4"), 4096);
        assert_eq!(cache_min_block_chars_or_default("claude-opus-5"), 4096);
        // Unknown model → the documented conservative default.
        assert_eq!(
            cache_min_block_chars_or_default("llama-3-70b"),
            DEFAULT_CACHE_MIN_BLOCK_CHARS
        );
        // …but the raw accessor still reports the absence honestly.
        assert!(cache_min_block_chars("llama-3-70b").is_none());
    }

    #[test]
    fn api_shape_is_recorded() {
        assert_eq!(
            api_shape("claude-opus-5"),
            Some(ApiShape::AnthropicMessages)
        );
        assert_eq!(
            api_shape("gemini-1-5-pro"),
            Some(ApiShape::GoogleGenerativeAi)
        );
        assert_eq!(api_shape("llama-3-70b"), None);
    }
}
