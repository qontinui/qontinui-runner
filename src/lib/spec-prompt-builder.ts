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
  expected?: unknown;
  condition?: Record<string, unknown>;
  precondition?: string;
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
 * Format a SpecTarget into a human-readable search description.
 */
function formatTarget(target?: Record<string, unknown>): string {
  if (!target) return "";
  const type = target.type as string | undefined;
  if (type === "search") {
    const criteria = target.criteria as Record<string, unknown> | undefined;
    if (!criteria) return "";
    const parts: string[] = [];
    if (criteria.role) parts.push(`role=${criteria.role}`);
    if (criteria.textContent) parts.push(`text="${criteria.textContent}"`);
    if (criteria.idPattern) parts.push(`id~${criteria.idPattern}`);
    if (criteria.tagName) parts.push(`tag=${criteria.tagName}`);
    return parts.length > 0 ? `(search: ${parts.join(", ")})` : "";
  }
  if (type === "elementId") {
    return `(elementId: ${target.elementId})`;
  }
  return "";
}

/**
 * Format a condition into a human-readable string.
 */
function formatCondition(condition?: Record<string, unknown>): string {
  if (!condition) return "";
  const type = condition.type as string;
  const condTarget = condition.target as Record<string, unknown> | undefined;
  const targetStr = formatTarget(condTarget);
  if (type === "exists") return `[when ${targetStr} exists]`;
  if (type === "notExists") return `[when ${targetStr} absent]`;
  if (type === "hasText") return `[when ${targetStr} has text "${condition.text}"]`;
  return "";
}

/**
 * Build a comprehensive prompt from all selected spec groups.
 * Includes assertion types, targets, conditions, and severity levels
 * to guide the AI in generating diverse verification steps.
 */
export function buildSpecPrompt({
  discoveredSpecs,
  selectedGroupIds,
}: BuildSpecPromptOptions): SpecPromptResult {
  const lines: string[] = [];

  // Assertion type vocabulary
  lines.push("# UI Bridge Spec Assertion Types");
  lines.push("");
  lines.push("The following assertion types are available for verification steps:");
  lines.push("- **exists** / **notExists** — element presence or absence");
  lines.push("- **visible** / **hidden** — element visibility state");
  lines.push("- **enabled** / **disabled** — interactive element enabled/disabled state");
  lines.push("- **focused** — element has keyboard focus");
  lines.push("- **checked** / **unchecked** — checkbox/radio state");
  lines.push("- **hasText** / **containsText** — exact or partial text content match");
  lines.push("- **hasValue** — form input value match");
  lines.push("- **count** — number of matching elements (expected = number)");
  lines.push("- **attribute** — element attribute value (requires attributeName + expected)");
  lines.push("- **hasClass** — element has a CSS class");
  lines.push("- **cssProperty** — computed CSS property value");
  lines.push("");
  lines.push(
    "Severity levels: **critical** (core functionality), **warning** (important features), **info** (nice-to-have).",
  );
  lines.push("");
  lines.push("---");
  lines.push("");
  lines.push("# Page Specifications");
  lines.push("");

  let totalGroups = 0;
  let totalAssertions = 0;
  const pageUrls = new Set<string>();

  for (const spec of discoveredSpecs) {
    const { config } = spec;
    const pageUrl = config.metadata?.pageUrl || spec.specId;

    const selectedGroups = config.groups.filter((g) => selectedGroupIds.has(g.id));
    if (selectedGroups.length === 0) continue;

    pageUrls.add(pageUrl);
    lines.push(`## ${pageUrl}`);
    lines.push("");

    for (const group of selectedGroups) {
      totalGroups++;
      lines.push(`### ${group.name} [${group.category}]`);
      if (group.description) {
        lines.push(group.description);
      }
      lines.push("");

      const enabledAssertions = group.assertions.filter((a) => a.enabled);
      if (enabledAssertions.length > 0) {
        lines.push("Assertions:");
        for (const assertion of enabledAssertions) {
          totalAssertions++;
          const severity = assertion.severity || "info";
          const type = assertion.assertionType || "exists";
          const targetStr = formatTarget(assertion.target);
          const condStr = formatCondition(assertion.condition);
          const expectedStr =
            assertion.expected !== undefined && assertion.expected !== null
              ? ` expected=${JSON.stringify(assertion.expected)}`
              : "";
          const precondStr = assertion.precondition
            ? ` | precondition: "${assertion.precondition}"`
            : "";

          lines.push(
            `- [${severity}] ${assertion.description} — **${type}**${expectedStr} ${targetStr} ${condStr}${precondStr}`.trimEnd(),
          );
        }
        lines.push("");
      }
    }
  }

  const pageCount = pageUrls.size;

  // Closing guidance
  lines.push("---");
  lines.push("");
  lines.push("## Verification Step Guidance");
  lines.push("");
  lines.push("Use the page specifications above to generate appropriate verification steps:");
  lines.push("- Do NOT use only `exists` assertions — verify state, content, and behavior.");
  lines.push("- Use `enabled`/`disabled` to verify interactive element states.");
  lines.push("- Use `hasText`/`containsText` to verify text content and labels.");
  lines.push("- Use `count` to verify the expected number of repeated elements.");
  lines.push("- Use `hasValue` to verify form input values.");
  lines.push("- Use conditions when assertions only apply in certain UI states.");
  lines.push(
    "- Match severity levels to importance: critical for core flows, warning for features, info for polish.",
  );

  return {
    prompt: lines.join("\n"),
    totalGroups,
    totalAssertions,
    pageCount,
  };
}

/** @deprecated Use `buildSpecPrompt` instead. */
export const buildSemanticSpecPrompt = buildSpecPrompt;
