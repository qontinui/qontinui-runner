/**
 * continuationPrompt.ts
 *
 * Utility functions for generating AI continuation prompts with user responses.
 * Used when the AI pauses for user input and needs to continue with the answers.
 */

import type { Finding } from "../types/findings";
import { getCategoryById } from "../services/FindingCategories";

/**
 * A finding with its user response ready for inclusion in a continuation prompt
 */
export interface FindingWithResponse {
  finding: Finding;
  response: string;
}

/**
 * Options for formatting the continuation prompt section
 */
export interface ContinuationPromptOptions {
  /** Include the finding category in the output */
  includeCategory?: boolean;
  /** Include file context if available */
  includeFileContext?: boolean;
  /** Include the original question context */
  includeContext?: boolean;
  /** Custom header text (default: "User has provided the following responses:") */
  headerText?: string;
  /** Custom footer text (optional) */
  footerText?: string;
}

const DEFAULT_OPTIONS: ContinuationPromptOptions = {
  includeCategory: true,
  includeFileContext: true,
  includeContext: false,
  headerText: "User has provided the following responses:",
};

/**
 * Format a single finding with its user response for inclusion in a continuation prompt
 */
export function formatFindingResponse(
  finding: Finding,
  response: string,
  options: ContinuationPromptOptions = {},
): string {
  const opts = { ...DEFAULT_OPTIONS, ...options };
  const lines: string[] = [];

  // Category and title
  const category = opts.includeCategory ? getCategoryById(finding.categoryId) : null;
  const categoryPrefix = category ? `${category.name}: ` : "";
  lines.push(`## ${categoryPrefix}${finding.title}`);

  // File context
  if (opts.includeFileContext && finding.codeContext?.file) {
    const lineInfo = finding.codeContext.line ? `:${finding.codeContext.line}` : "";
    lines.push(`**File:** \`${finding.codeContext.file}${lineInfo}\``);
  }

  // Original question
  if (finding.pendingQuestion) {
    lines.push(`**Question:** ${finding.pendingQuestion.question}`);

    // Include context if requested
    if (opts.includeContext && finding.pendingQuestion.context) {
      lines.push(`**Context:** ${finding.pendingQuestion.context}`);
    }
  }

  // User response
  lines.push(`**Response:** ${response}`);

  return lines.join("\n");
}

/**
 * Generate a continuation prompt section with all user responses
 *
 * @param findingsWithResponses - Array of findings that have user responses
 * @param options - Formatting options
 * @returns Formatted markdown string for inclusion in continuation prompt
 *
 * @example
 * ```typescript
 * const section = generateContinuationPromptSection([
 *   { finding: todoFinding, response: "Use Redis for production" },
 *   { finding: configFinding, response: "yes" }
 * ]);
 *
 * // Returns:
 * // User has provided the following responses:
 * //
 * // ## TODO: Implement caching strategy
 * // **Question:** Should caching use Redis or in-memory store?
 * // **Response:** Use Redis for production
 * //
 * // ## Configuration Issue: Database connection pooling
 * // **Question:** Enable connection pooling?
 * // **Response:** yes
 * ```
 */
export function generateContinuationPromptSection(
  findingsWithResponses: FindingWithResponse[],
  options: ContinuationPromptOptions = {},
): string {
  const opts = { ...DEFAULT_OPTIONS, ...options };

  if (findingsWithResponses.length === 0) {
    return "";
  }

  const sections: string[] = [];

  // Header
  if (opts.headerText) {
    sections.push(opts.headerText);
    sections.push(""); // Empty line after header
  }

  // Format each finding
  for (const { finding, response } of findingsWithResponses) {
    sections.push(formatFindingResponse(finding, response, opts));
    sections.push(""); // Empty line between findings
  }

  // Footer
  if (opts.footerText) {
    sections.push(opts.footerText);
  } else {
    sections.push("Please proceed with implementing these items using the provided context.");
  }

  return sections.join("\n");
}

/**
 * Extract findings with responses from a list of findings
 * Only includes findings that have userResponse set
 */
export function extractFindingsWithResponses(findings: Finding[]): FindingWithResponse[] {
  return findings
    .filter((f) => f.userResponse && f.userResponse.trim().length > 0)
    .map((f) => ({
      finding: f,
      response: f.userResponse!,
    }));
}

/**
 * Generate a full continuation prompt from findings
 * This is a convenience function that extracts responses and formats them
 *
 * @param findings - All findings (will filter for those with responses)
 * @param stepNumber - Optional step number for context
 * @param options - Formatting options
 * @returns Complete continuation prompt section
 */
export function generateContinuationPrompt(
  findings: Finding[],
  stepNumber?: number,
  options: ContinuationPromptOptions = {},
): string {
  const findingsWithResponses = extractFindingsWithResponses(findings);

  if (findingsWithResponses.length === 0) {
    return stepNumber ? `Continue the task from step ${stepNumber}.` : "Continue the task.";
  }

  const basePrompt = stepNumber
    ? `Continue the task from step ${stepNumber}.`
    : "Continue the task.";

  const responsesSection = generateContinuationPromptSection(findingsWithResponses, options);

  return `${basePrompt}\n\n${responsesSection}`;
}

/**
 * Check if any findings have pending user input requests
 */
export function hasPendingInputs(findings: Finding[]): boolean {
  return findings.some((f) => f.status === "needs_input" && f.pendingQuestion && !f.userResponse);
}

/**
 * Get count of findings needing user input
 */
export function getPendingInputCount(findings: Finding[]): number {
  return findings.filter((f) => f.status === "needs_input" && f.pendingQuestion && !f.userResponse)
    .length;
}

/**
 * Get count of findings with user responses ready for continuation
 */
export function getAnsweredCount(findings: Finding[]): number {
  return findings.filter((f) => f.userResponse && f.userResponse.trim().length > 0).length;
}
