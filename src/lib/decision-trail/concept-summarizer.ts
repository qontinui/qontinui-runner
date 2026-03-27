/**
 * Concept Summarizer — AI prompt builder for generating ConceptSummary entries.
 *
 * Takes high-level feature context (from architecture specs, memory files, etc.)
 * and builds prompts to generate rich concept summaries.
 */

export interface ConceptInput {
  name: string;
  implementationFiles?: string[];
  relatedDecisionIds?: string[];
  inspirationSource?: string;
  inspirationUrl?: string;
  existingDescription?: string;
}

/** Build an AI prompt to generate a ConceptSummary. */
export function buildConceptSummaryPrompt(input: ConceptInput): string {
  const fileList = input.implementationFiles?.length
    ? `Implementation files:\n${input.implementationFiles.map((f) => `- ${f}`).join("\n")}`
    : "";

  const decisions = input.relatedDecisionIds?.length
    ? `Related decisions: ${input.relatedDecisionIds.join(", ")}`
    : "";

  const inspiration = input.inspirationSource
    ? `Inspiration: ${input.inspirationSource}${input.inspirationUrl ? ` (${input.inspirationUrl})` : ""}`
    : "";

  const existing = input.existingDescription
    ? `Existing context:\n${input.existingDescription}`
    : "";

  return `You are writing a concept summary for a major feature in Qontinui, an AI-powered automation platform.

Feature: ${input.name}
${fileList}
${decisions}
${inspiration}
${existing}

Generate a JSON object with:
{
  "tagline": "One sentence, benefit-focused summary",
  "description": "3-5 paragraph explanation covering: what this feature IS, what problem it solves, how it works (high-level architecture), how it benefits Qontinui users",
  "benefits": ["Concrete benefit 1", "Concrete benefit 2", ...],
  "inspiration": {
    "source": "${input.inspirationSource || ""}",
    "url": "${input.inspirationUrl || ""}",
    "description": "What the inspiration project does",
    "whatWeAdapted": "What specific patterns/ideas we adapted",
    "howItBenefitsQontinui": "How this adaptation improves the platform"
  }
}

Write in a clear, technical but accessible tone. Focus on concrete value, not marketing language. Respond with valid JSON only.`;
}

/** Predefined concept inputs for major existing features. */
export const SEED_CONCEPTS: ConceptInput[] = [
  {
    name: "Knowledge Graphs",
    inspirationSource: "petgraph crate",
    existingDescription:
      "Cross-run learning that makes automations smarter over time. 6 DB tables, 16 MCP endpoints, petgraph-based graph engine, React visualization components. Insights from one run inform future runs.",
  },
  {
    name: "UI Bridge",
    existingDescription:
      "Deep integration with target applications for AI-driven testing and automation. Relay architecture, semantic snapshots, change tracking. SDK packages for web, React, and mobile platforms.",
  },
  {
    name: "Meta-Optimizer",
    existingDescription:
      "AI that optimizes its own prompts based on execution results. Pipeline architecture with reflection fixes. Canary rollouts for safe prompt deployment.",
  },
  {
    name: "Activity Timeline",
    inspirationSource: "screenpipe",
    existingDescription:
      "Searchable history of everything seen on screen during automation. FTS-indexed capture history, watchers for reactive agents, paired capture modes. Content-hash deduplication.",
  },
  {
    name: "Decision Trail",
    existingDescription:
      "Persistent, searchable timeline of every architectural decision made across the project. Auto-captured from development activity, AI-enriched with rationale and alternatives. Three views: timeline, concepts, graph.",
  },
];
