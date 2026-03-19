/**
 * Shared types for autoresearch components.
 *
 * Extracted to break circular dependency: PerAgentTab ↔ AgentCampaignConfig/History/TraceComparison.
 */

export type AgentType = "spec_analyst" | "locator" | "implementer" | "verifier";
export type PerAgentView = "configure" | "monitor" | "results" | "history";
