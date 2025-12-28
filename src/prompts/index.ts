/**
 * Prompts Module
 *
 * This module contains prompt templates and instruction files for AI integration.
 * The markdown files in this directory document the structured output formats
 * that AI should use when reporting findings.
 *
 * Files:
 * - finding-output-instructions.md: Complete documentation of the finding output format
 *
 * For programmatic access to prompt building utilities, use:
 * @see ../utils/findingPromptBuilder.ts
 */

// Re-export the prompt builder utilities for convenience
export {
  getFindingOutputInstructions,
  getCompactFindingInstructions,
  wrapPromptWithFindingInstructions,
  prependFindingInstructions,
  createFindingMarker,
  getExampleFindings,
  SEVERITY_LEVELS,
  CATEGORY_IDS,
} from "../utils/findingPromptBuilder";

export type { SeverityLevel } from "../utils/findingPromptBuilder";
