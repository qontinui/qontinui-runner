//! AI Model Pricing Module
//!
//! Provides pricing data and cost calculation for AI model usage.
//! This module tracks pricing per model and calculates costs based on token usage.
//!
//! NOTE: This module is intentionally kept for future use in cost tracking.

#![allow(dead_code)]

/// Pricing structure for an AI model.
///
/// Prices are in USD per 1 million tokens.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per 1 million input tokens (USD)
    pub input_per_million: f64,
    /// Cost per 1 million output tokens (USD)
    pub output_per_million: f64,
}

impl ModelPricing {
    /// Create a new pricing structure.
    pub const fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
        }
    }
}

// ============================================================================
// Model Pricing Data (as of 2024/2025)
// ============================================================================

// Claude 3.5 models
const CLAUDE_3_5_SONNET_PRICING: ModelPricing = ModelPricing::new(3.0, 15.0);
const CLAUDE_3_5_HAIKU_PRICING: ModelPricing = ModelPricing::new(0.25, 1.25);

// Claude 3 models
const CLAUDE_3_OPUS_PRICING: ModelPricing = ModelPricing::new(15.0, 75.0);
const CLAUDE_3_SONNET_PRICING: ModelPricing = ModelPricing::new(3.0, 15.0);
const CLAUDE_3_HAIKU_PRICING: ModelPricing = ModelPricing::new(0.25, 1.25);

// Claude Opus 4 models (latest as of 2025)
const CLAUDE_OPUS_4_PRICING: ModelPricing = ModelPricing::new(15.0, 75.0);
const CLAUDE_OPUS_4_5_PRICING: ModelPricing = ModelPricing::new(15.0, 75.0);

// Claude Sonnet 4 models (latest as of 2025)
const CLAUDE_SONNET_4_PRICING: ModelPricing = ModelPricing::new(3.0, 15.0);

// Claude Haiku 4.5 (latest as of 2025)
const CLAUDE_HAIKU_4_5_PRICING: ModelPricing = ModelPricing::new(0.80, 4.0);

// Gemini models (Google AI)
const GEMINI_1_5_PRO_PRICING: ModelPricing = ModelPricing::new(1.25, 5.0);
const GEMINI_1_5_FLASH_PRICING: ModelPricing = ModelPricing::new(0.075, 0.30);
const GEMINI_2_0_FLASH_PRICING: ModelPricing = ModelPricing::new(0.10, 0.40);

// ============================================================================
// Pricing Lookup
// ============================================================================

/// Get pricing for a model by its ID.
///
/// Model IDs are matched case-insensitively and support various formats:
/// - Full model IDs: "claude-3-5-sonnet-20240620", "claude-3-opus-20240229"
/// - Short names: "claude-3-5-sonnet", "claude-3-opus", "opus", "sonnet"
/// - Gemini models: "gemini-1.5-pro", "gemini-1.5-flash"
///
/// Returns `None` if the model is not recognized.
pub fn get_pricing(model_id: &str) -> Option<ModelPricing> {
    let model_lower = model_id.to_lowercase();

    // Claude Opus 4.5 (newest)
    if model_lower.contains("opus-4-5") || model_lower.contains("opus-4.5") {
        return Some(CLAUDE_OPUS_4_5_PRICING);
    }

    // Claude Opus 4
    if model_lower.contains("opus-4") && !model_lower.contains("opus-4-5") {
        return Some(CLAUDE_OPUS_4_PRICING);
    }

    // Claude Sonnet 4
    if model_lower.contains("sonnet-4")
        && !model_lower.contains("3-5")
        && !model_lower.contains("3.5")
    {
        return Some(CLAUDE_SONNET_4_PRICING);
    }

    // Claude 3.5 Sonnet (various date-stamped versions)
    if model_lower.contains("3-5-sonnet")
        || model_lower.contains("3.5-sonnet")
        || model_lower.contains("claude-3-5-sonnet")
        || model_lower.contains("claude-3.5-sonnet")
    {
        return Some(CLAUDE_3_5_SONNET_PRICING);
    }

    // Claude Haiku 4.5
    if model_lower.contains("haiku-4-5") || model_lower.contains("haiku-4.5") {
        return Some(CLAUDE_HAIKU_4_5_PRICING);
    }

    // Claude 3.5 Haiku
    if model_lower.contains("3-5-haiku")
        || model_lower.contains("3.5-haiku")
        || model_lower.contains("claude-3-5-haiku")
        || model_lower.contains("claude-3.5-haiku")
    {
        return Some(CLAUDE_3_5_HAIKU_PRICING);
    }

    // Claude 3 Opus
    if model_lower.contains("3-opus") || model_lower.contains("claude-3-opus") {
        return Some(CLAUDE_3_OPUS_PRICING);
    }

    // Claude 3 Sonnet
    if model_lower.contains("3-sonnet") || model_lower.contains("claude-3-sonnet") {
        return Some(CLAUDE_3_SONNET_PRICING);
    }

    // Claude 3 Haiku
    if model_lower.contains("3-haiku") || model_lower.contains("claude-3-haiku") {
        return Some(CLAUDE_3_HAIKU_PRICING);
    }

    // Generic model names (short forms)
    if model_lower == "opus" || model_lower.ends_with("-opus") {
        return Some(CLAUDE_3_OPUS_PRICING);
    }
    if model_lower == "sonnet" || model_lower.ends_with("-sonnet") {
        return Some(CLAUDE_3_5_SONNET_PRICING);
    }
    if model_lower == "haiku" || model_lower.ends_with("-haiku") {
        return Some(CLAUDE_3_5_HAIKU_PRICING);
    }

    // Claude CLI (use default sonnet pricing as it's the most common)
    if model_lower == "claude-cli" || model_lower == "claude" {
        return Some(CLAUDE_3_5_SONNET_PRICING);
    }

    // Gemini 2.0 Flash
    if model_lower.contains("gemini-2.0-flash") || model_lower.contains("gemini-2-0-flash") {
        return Some(GEMINI_2_0_FLASH_PRICING);
    }

    // Gemini 1.5 Pro
    if model_lower.contains("gemini-1.5-pro")
        || model_lower.contains("gemini-1-5-pro")
        || model_lower.contains("gemini-pro")
    {
        return Some(GEMINI_1_5_PRO_PRICING);
    }

    // Gemini 1.5 Flash
    if model_lower.contains("gemini-1.5-flash")
        || model_lower.contains("gemini-1-5-flash")
        || model_lower.contains("gemini-flash")
    {
        return Some(GEMINI_1_5_FLASH_PRICING);
    }

    // Unknown model
    None
}

/// Calculate cost in cents (USD) based on token usage and model.
///
/// # Arguments
/// * `input_tokens` - Number of input tokens consumed
/// * `output_tokens` - Number of output tokens generated
/// * `model_id` - Model identifier string
///
/// # Returns
/// Cost in cents (rounded down to nearest cent), or `None` if model is not recognized.
///
/// # Example
/// ```
/// use qontinui_runner::ai_pricing::calculate_cost_cents;
///
/// let cost = calculate_cost_cents(1000, 500, "claude-3-5-sonnet");
/// assert!(cost.is_some());
/// ```
pub fn calculate_cost_cents(input_tokens: u64, output_tokens: u64, model_id: &str) -> Option<u32> {
    let pricing = get_pricing(model_id)?;

    // Calculate cost in USD
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_million;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_million;
    let total_usd = input_cost + output_cost;

    // Convert to cents (100 cents = 1 USD)
    // Round to nearest cent for more accuracy
    Some((total_usd * 100.0).round() as u32)
}

/// Calculate cost in cents with separate input and output costs returned.
///
/// Useful for detailed cost breakdowns.
///
/// # Returns
/// Tuple of (total_cents, input_cents, output_cents), or `None` if model is not recognized.
pub fn calculate_cost_breakdown(
    input_tokens: u64,
    output_tokens: u64,
    model_id: &str,
) -> Option<(u32, u32, u32)> {
    let pricing = get_pricing(model_id)?;

    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_million;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_million;

    let input_cents = (input_cost * 100.0).round() as u32;
    let output_cents = (output_cost * 100.0).round() as u32;
    let total_cents = input_cents + output_cents;

    Some((total_cents, input_cents, output_cents))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_pricing_claude_models() {
        // Claude 3.5 Sonnet
        assert!(get_pricing("claude-3-5-sonnet-20240620").is_some());
        assert!(get_pricing("claude-3.5-sonnet").is_some());
        assert!(get_pricing("CLAUDE-3-5-SONNET").is_some()); // Case insensitive

        // Claude 3.5 Haiku
        assert!(get_pricing("claude-3-5-haiku-20241022").is_some());
        assert!(get_pricing("claude-3.5-haiku").is_some());

        // Claude 3 Opus
        assert!(get_pricing("claude-3-opus-20240229").is_some());
        assert!(get_pricing("claude-3-opus").is_some());

        // Short names
        assert!(get_pricing("opus").is_some());
        assert!(get_pricing("sonnet").is_some());
        assert!(get_pricing("haiku").is_some());

        // Claude CLI
        assert!(get_pricing("claude-cli").is_some());
    }

    #[test]
    fn test_get_pricing_claude_4_models() {
        // Claude Opus 4.5
        assert!(get_pricing("claude-opus-4-5-20251101").is_some());
        assert!(get_pricing("claude-opus-4.5").is_some());

        // Claude Opus 4
        assert!(get_pricing("claude-opus-4-20250514").is_some());

        // Claude Sonnet 4
        assert!(get_pricing("claude-sonnet-4-20250514").is_some());
    }

    #[test]
    fn test_get_pricing_gemini_models() {
        assert!(get_pricing("gemini-1.5-pro").is_some());
        assert!(get_pricing("gemini-1.5-flash").is_some());
        assert!(get_pricing("gemini-2.0-flash").is_some());
        assert!(get_pricing("gemini-pro").is_some());
    }

    #[test]
    fn test_get_pricing_unknown_model() {
        assert!(get_pricing("unknown-model-xyz").is_none());
        assert!(get_pricing("gpt-4").is_none()); // OpenAI not supported
    }

    #[test]
    fn test_calculate_cost_cents() {
        // Claude 3.5 Sonnet: $3/$15 per 1M tokens
        // 1M input tokens = $3.00 = 300 cents
        // 1M output tokens = $15.00 = 1500 cents
        let cost = calculate_cost_cents(1_000_000, 1_000_000, "claude-3-5-sonnet");
        assert_eq!(cost, Some(1800)); // 300 + 1500 = 1800 cents

        // Small usage (10K input, 5K output)
        // Input: 10000 / 1M * $3 = $0.03 = 3 cents
        // Output: 5000 / 1M * $15 = $0.075 = ~8 cents (rounded)
        let cost = calculate_cost_cents(10_000, 5_000, "claude-3-5-sonnet");
        assert_eq!(cost, Some(11)); // 3 + 8 = 11 cents

        // Zero tokens
        let cost = calculate_cost_cents(0, 0, "claude-3-5-sonnet");
        assert_eq!(cost, Some(0));
    }

    #[test]
    fn test_calculate_cost_cents_opus() {
        // Claude 3 Opus: $15/$75 per 1M tokens
        // 100K input = $1.50 = 150 cents
        // 50K output = $3.75 = 375 cents
        let cost = calculate_cost_cents(100_000, 50_000, "claude-3-opus");
        assert_eq!(cost, Some(525)); // 150 + 375 = 525 cents
    }

    #[test]
    fn test_calculate_cost_cents_haiku() {
        // Claude 3.5 Haiku: $0.25/$1.25 per 1M tokens
        // 1M input = $0.25 = 25 cents
        // 1M output = $1.25 = 125 cents
        let cost = calculate_cost_cents(1_000_000, 1_000_000, "claude-3-5-haiku");
        assert_eq!(cost, Some(150)); // 25 + 125 = 150 cents
    }

    #[test]
    fn test_calculate_cost_cents_unknown() {
        let cost = calculate_cost_cents(1_000_000, 1_000_000, "unknown-model");
        assert!(cost.is_none());
    }

    #[test]
    fn test_calculate_cost_breakdown() {
        let breakdown = calculate_cost_breakdown(100_000, 50_000, "claude-3-5-sonnet");
        assert!(breakdown.is_some());

        let (total, input, output) = breakdown.unwrap();
        // Input: 100K / 1M * $3 = $0.30 = 30 cents
        // Output: 50K / 1M * $15 = $0.75 = 75 cents
        assert_eq!(input, 30);
        assert_eq!(output, 75);
        assert_eq!(total, 105);
    }
}
