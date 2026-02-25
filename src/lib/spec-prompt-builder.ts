/**
 * spec-prompt-builder.ts
 *
 * Builds a structured AI prompt from discovered UI Bridge spec data.
 * Used by AiGeneratePanel to create spec-driven workflow generation requests.
 */

// =============================================================================
// Types (mirrors the spec JSON structure)
// =============================================================================

export interface SpecAssertion {
  id: string;
  description: string;
  category: string;
  severity: "critical" | "warning" | "info";
  enabled: boolean;
  target?: Record<string, unknown>;
  assertionType?: string;
  condition?: Record<string, unknown>;
}

export interface SpecGroup {
  id: string;
  name: string;
  description: string;
  category: string;
  assertions: SpecAssertion[];
  source?: string;
}

export interface SpecConfig {
  version: string;
  description: string;
  groups: SpecGroup[];
  metadata?: {
    component?: string;
    pageUrl?: string;
    tags?: string[];
    [key: string]: unknown;
  };
}

export interface DiscoveredSpec {
  specId: string;
  config: SpecConfig;
}

// =============================================================================
// Prompt builder
// =============================================================================

export interface BuildSpecPromptOptions {
  discoveredSpecs: DiscoveredSpec[];
  selectedGroupIds: Set<string>;
}

export interface SpecPromptResult {
  prompt: string;
  totalGroups: number;
  totalAssertions: number;
  pageCount: number;
}

/**
 * Build a natural-language prompt from semantic page specs only.
 * Filters to groups with category "semantic" and presents them as
 * high-level page purpose descriptions rather than technical assertions.
 */
export function buildSemanticSpecPrompt({
  discoveredSpecs,
  selectedGroupIds,
}: BuildSpecPromptOptions): SpecPromptResult {
  const lines: string[] = [
    "The following are semantic page specifications describing what each page is designed to do.",
    "",
  ];

  let totalGroups = 0;
  let totalAssertions = 0;
  const pageUrls = new Set<string>();

  for (const spec of discoveredSpecs) {
    const { config } = spec;
    const pageUrl = config.metadata?.pageUrl || spec.specId;

    const selectedGroups = config.groups.filter(
      (g) => selectedGroupIds.has(g.id) && g.category === "semantic",
    );
    if (selectedGroups.length === 0) continue;

    pageUrls.add(pageUrl);
    lines.push(`## ${pageUrl}`);
    lines.push("");

    for (const group of selectedGroups) {
      totalGroups++;
      if (group.description) {
        lines.push(group.description);
        lines.push("");
      }

      const enabledAssertions = group.assertions.filter((a) => a.enabled);
      if (enabledAssertions.length > 0) {
        lines.push("Key capabilities:");
        for (const assertion of enabledAssertions) {
          totalAssertions++;
          lines.push(`- ${assertion.description}`);
        }
        lines.push("");
      }
    }
  }

  const pageCount = pageUrls.size;

  lines.push(
    "Use these page descriptions to understand what each page does and generate appropriate verification steps.",
    "Determine your own element-level checks based on the semantic intent described above.",
  );

  return {
    prompt: lines.join("\n"),
    totalGroups,
    totalAssertions,
    pageCount,
  };
}
