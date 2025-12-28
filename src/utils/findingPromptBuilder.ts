/**
 * Finding Prompt Builder Utility
 *
 * Provides utilities for including structured finding output instructions in AI prompts.
 * Used to ensure AI analysis outputs findings in a format parseable by FindingsTracker.
 *
 * @module findingPromptBuilder
 */

import type { FindingCategory } from "../types/findings";
import { BUILT_IN_CATEGORIES } from "../services/FindingCategories";

/**
 * Severity levels available for findings
 */
export const SEVERITY_LEVELS = ["critical", "high", "medium", "low", "info"] as const;
export type SeverityLevel = (typeof SEVERITY_LEVELS)[number];

/**
 * Available category IDs for reference in prompts
 */
export const CATEGORY_IDS = BUILT_IN_CATEGORIES.map((c) => c.id);

/**
 * Generate the structured finding output format reference as a string.
 * This can be appended to prompts to instruct AI on proper output format.
 *
 * @returns Markdown-formatted instructions for finding output
 */
export function getFindingOutputInstructions(): string {
  return `
## Structured Finding Output Format

When you identify issues, report them using this format so they can be tracked:

### Basic Finding (Auto-fixable)
\`\`\`
[FINDING:category_id:severity]
Title: Brief title describing the issue
Description: Detailed description of what's wrong and why
File: path/to/file.ts (if applicable)
Line: 42 (if applicable)
Resolution: What was done to fix it (if already fixed)
[/FINDING]
\`\`\`

### Finding Requiring User Input
\`\`\`
[FINDING:category_id:severity:needs_input]
Title: Brief title describing the issue
Description: Detailed description of the situation
Question: Specific question for the user
Options: Option A | Option B | Option C
File: path/to/file.ts (if applicable)
[/FINDING]
\`\`\`

### Available Categories
${formatCategoriesForPrompt()}

### Severity Levels
- \`critical\`: System-breaking, security vulnerabilities, data loss
- \`high\`: Major functionality issues
- \`medium\`: Notable issues to address soon
- \`low\`: Minor issues
- \`info\`: Informational notes

### Guidelines
- Use \`:needs_input\` when user decisions are required
- Include file paths and line numbers when available
- Be specific in descriptions
- Choose the most appropriate category
`;
}

/**
 * Generate a compact version of the finding instructions for shorter prompts.
 *
 * @returns Compact markdown-formatted instructions
 */
export function getCompactFindingInstructions(): string {
  return `
## Finding Output Format

Report issues with: \`[FINDING:category:severity]\` ... \`[/FINDING]\`

Categories: ${CATEGORY_IDS.join(", ")}
Severities: critical, high, medium, low, info

Add \`:needs_input\` when user decisions needed. Include Title:, Description:, and optionally File:, Line:, Question:, Options:, Resolution:.
`;
}

/**
 * Wrap an existing prompt with finding output instructions.
 * Adds the instructions at the end of the prompt.
 *
 * @param prompt - The original prompt text
 * @param compact - Whether to use compact instructions (default: false)
 * @returns The prompt with finding instructions appended
 */
export function wrapPromptWithFindingInstructions(
  prompt: string,
  compact: boolean = false,
): string {
  const instructions = compact ? getCompactFindingInstructions() : getFindingOutputInstructions();

  return `${prompt}

---
${instructions}`;
}

/**
 * Prepend finding instructions to a prompt (alternative to wrapping).
 *
 * @param prompt - The original prompt text
 * @param compact - Whether to use compact instructions (default: false)
 * @returns The prompt with finding instructions prepended
 */
export function prependFindingInstructions(prompt: string, compact: boolean = false): string {
  const instructions = compact ? getCompactFindingInstructions() : getFindingOutputInstructions();

  return `${instructions}

---

${prompt}`;
}

/**
 * Format categories for inclusion in prompts
 */
function formatCategoriesForPrompt(): string {
  return BUILT_IN_CATEGORIES.map(
    (cat) =>
      `- \`${cat.id}\`: ${cat.name} - ${cat.description} (${formatActionType(cat.defaultActionType)})`,
  ).join("\n");
}

/**
 * Format action type for display
 */
function formatActionType(actionType: string): string {
  const actionTypeMap: Record<string, string> = {
    auto_fix: "auto-fixable",
    needs_user_input: "needs input",
    manual: "manual action",
    informational: "informational",
  };
  return actionTypeMap[actionType] || actionType;
}

/**
 * Create a finding marker string programmatically.
 * Useful for testing or generating example findings.
 *
 * @param options - Finding details
 * @returns Formatted finding marker string
 */
export function createFindingMarker(options: {
  categoryId: string;
  severity: SeverityLevel;
  title: string;
  description: string;
  needsInput?: boolean;
  question?: string;
  optionsList?: string[];
  file?: string;
  line?: number;
  resolution?: string;
}): string {
  const {
    categoryId,
    severity,
    title,
    description,
    needsInput = false,
    question,
    optionsList,
    file,
    line,
    resolution,
  } = options;

  const markerStart = needsInput
    ? `[FINDING:${categoryId}:${severity}:needs_input]`
    : `[FINDING:${categoryId}:${severity}]`;

  const lines: string[] = [markerStart, `Title: ${title}`, `Description: ${description}`];

  if (file) {
    lines.push(`File: ${file}`);
  }

  if (line !== undefined) {
    lines.push(`Line: ${line}`);
  }

  if (needsInput && question) {
    lines.push(`Question: ${question}`);
    if (optionsList && optionsList.length > 0) {
      lines.push(`Options: ${optionsList.join(" | ")}`);
    }
  }

  if (resolution) {
    lines.push(`Resolution: ${resolution}`);
  }

  lines.push("[/FINDING]");

  return lines.join("\n");
}

/**
 * Get category information by ID
 *
 * @param categoryId - The category ID to look up
 * @returns The category if found, undefined otherwise
 */
export function getCategoryInfo(categoryId: string): FindingCategory | undefined {
  return BUILT_IN_CATEGORIES.find((c) => c.id === categoryId);
}

/**
 * Validate that a category ID is valid
 *
 * @param categoryId - The category ID to validate
 * @returns True if valid, false otherwise
 */
export function isValidCategoryId(categoryId: string): boolean {
  return CATEGORY_IDS.includes(categoryId);
}

/**
 * Validate that a severity level is valid
 *
 * @param severity - The severity to validate
 * @returns True if valid, false otherwise
 */
export function isValidSeverity(severity: string): severity is SeverityLevel {
  return SEVERITY_LEVELS.includes(severity as SeverityLevel);
}

/**
 * Generate example findings for testing or documentation purposes
 *
 * @returns Array of example finding marker strings
 */
export function getExampleFindings(): string[] {
  return [
    createFindingMarker({
      categoryId: "code_bug",
      severity: "high",
      title: "Null pointer exception in user lookup",
      description:
        "The getUserById function can return null but the caller doesn't handle this case.",
      file: "src/services/UserService.ts",
      line: 45,
    }),
    createFindingMarker({
      categoryId: "todo",
      severity: "medium",
      title: "Implement caching layer",
      description:
        "The API makes redundant database calls. Adding caching would improve performance.",
      needsInput: true,
      question: "Which caching strategy should we use?",
      optionsList: ["Redis", "In-memory", "Both"],
      file: "src/api/handlers.ts",
      line: 100,
    }),
    createFindingMarker({
      categoryId: "already_fixed",
      severity: "info",
      title: "Login redirect issue",
      description: "Users were not being redirected after login.",
      file: "src/auth/callback.ts",
      line: 15,
      resolution: "Added proper redirect logic after successful authentication",
    }),
  ];
}
