/**
 * AI Test Generation Prompt Builder
 *
 * Provides a unified interface for building AI test generation prompts
 * from various analysis types (step outputs, page analyses, etc.).
 *
 * Uses the step output handler registry to format step outputs consistently.
 */

import type {
  CollectedAnalysisSet,
  PageAnalysis,
  ApiRequestAnalysis,
} from "../../types/test-builder";
import { getStepOutputFromAnalysis } from "../../types/test-builder";
import type { StepOutput } from "../../types/step-output";
import {
  summarizeOutputsForAI,
  collectAssertableFields,
  type AssertableField,
} from "../step-output-handlers";

// ============================================================================
// Types
// ============================================================================

export type TestType = "playwright_cdp" | "qontinui_vision" | "python_script" | "repository_test";

export interface PromptBuilderOptions {
  /** User's natural language description of the test */
  userPrompt: string;
  /** Type of test to generate */
  testType: TestType;
  /** Single page analysis (legacy) */
  pageAnalysis?: PageAnalysis | null;
  /** Collected analyses (new unified system) */
  collectedAnalyses?: CollectedAnalysisSet | null;
  /** Include assertable fields in prompt */
  includeAssertableFields?: boolean;
  /** Maximum response size hint for truncation */
  maxResponseLength?: number;
}

export interface BuiltPrompt {
  /** The full prompt text */
  prompt: string;
  /** Assertable fields extracted from analyses */
  assertableFields: AssertableField[];
  /** Summary of what was included */
  includedSections: string[];
}

// ============================================================================
// Test Type Instructions
// ============================================================================

const TEST_TYPE_INSTRUCTIONS: Record<TestType, string[]> = {
  playwright_cdp: [
    "Generate a Playwright test in TypeScript that:",
    "- Uses @playwright/test's test and expect",
    "- Assumes connection via CDP to existing browser",
    "- Uses proper selectors from the element analysis",
    "- Includes meaningful assertions",
  ],
  qontinui_vision: [
    "Generate a Qontinui Vision assertion config in JSON that:",
    "- Uses element bounding boxes from the analysis",
    "- Supports assertion types: element_present, text_present, element_within_bounds",
    "- Returns a valid JSON object with an 'assertions' array",
  ],
  python_script: [
    "Generate a Python test script that:",
    "- Uses assertion(condition, message) for assertions",
    "- Uses log(message) for logging",
    "- Uses metric(name, value) for metrics",
    "- Has a test_main() function as the entry point",
    "",
    "### Available Helpers",
    "```python",
    "def assertion(condition: bool, message: str) -> None:",
    "    '''Record an assertion. If condition is False, test fails.'''",
    "",
    "def log(message: str) -> None:",
    "    '''Log a message to the test output.'''",
    "",
    "def metric(name: str, value: float) -> None:",
    "    '''Record a numeric metric.'''",
    "",
    "# Environment variables available:",
    "# LAST_API_RESPONSE - JSON string of the last API response",
    "# WORKFLOW_CONFIG - JSON string of the linked workflow config",
    "# EXTRACTED_VARS - JSON dict of variables extracted from previous steps",
    "```",
    "",
    "### Output Format",
    "The script should print a JSON object at the end with this structure:",
    "```json",
    "{",
    '  "passed": true,',
    '  "summary": "Brief summary of verification result",',
    '  "issues": ["List of specific issues found"],',
    '  "metrics": {"metric_name": value}',
    "}",
    "```",
  ],
  repository_test: [
    "Generate a configuration comment with:",
    "- The test command to run",
    "- Expected output format",
    "- Any setup requirements",
  ],
};

// ============================================================================
// Prompt Builder Implementation
// ============================================================================

/**
 * Build a test generation prompt from collected analyses.
 */
export function buildTestGenerationPrompt(options: PromptBuilderOptions): BuiltPrompt {
  const {
    userPrompt,
    testType,
    pageAnalysis,
    collectedAnalyses,
    includeAssertableFields = true,
    maxResponseLength = 15000,
  } = options;

  const lines: string[] = [];
  const includedSections: string[] = [];
  let allAssertableFields: AssertableField[] = [];

  // Task header
  lines.push("## Task");
  lines.push(`Generate a ${testType} test based on the following requirements.`);
  lines.push("Return ONLY the test code, no explanations or markdown code blocks.");
  lines.push("");

  // Process collected analyses (new system)
  if (collectedAnalyses?.analyses.length) {
    const stepOutputs: StepOutput[] = [];
    const pageAnalyses: Array<{ name: string; data: PageAnalysis }> = [];
    const apiAnalyses: Array<{ name: string; data: ApiRequestAnalysis }> = [];

    // Categorize analyses
    for (const analysis of collectedAnalyses.analyses) {
      const stepOutput = getStepOutputFromAnalysis(analysis);
      if (stepOutput) {
        stepOutputs.push(stepOutput);
      } else if (analysis.type === "playwright" || analysis.type === "vision") {
        pageAnalyses.push({ name: analysis.name, data: analysis.data });
      } else if (analysis.type === "api_request") {
        apiAnalyses.push({ name: analysis.name, data: analysis.data });
      }
    }

    // Format step outputs using registry
    if (stepOutputs.length > 0) {
      lines.push(summarizeOutputsForAI(stepOutputs));
      includedSections.push(`${stepOutputs.length} step outputs`);

      if (includeAssertableFields) {
        allAssertableFields = collectAssertableFields(stepOutputs);
      }
    }

    // Format page analyses
    if (pageAnalyses.length > 0) {
      lines.push(formatPageAnalyses(pageAnalyses, testType));
      includedSections.push(`${pageAnalyses.length} page analyses`);
    }

    // Format API analyses (legacy format)
    if (apiAnalyses.length > 0) {
      lines.push(formatApiAnalyses(apiAnalyses));
      includedSections.push(`${apiAnalyses.length} API analyses`);
    }
  }

  // Single page analysis (legacy support)
  if (pageAnalysis && pageAnalysis.elements?.length) {
    lines.push(formatSinglePageAnalysis(pageAnalysis, testType));
    includedSections.push("single page analysis");
  }

  // Assertable fields section
  if (includeAssertableFields && allAssertableFields.length > 0) {
    lines.push(formatAssertableFields(allAssertableFields));
    includedSections.push(`${allAssertableFields.length} assertable fields`);
  }

  // User request
  lines.push("## User Request");
  lines.push("");
  lines.push(userPrompt);
  lines.push("");

  // Test type requirements
  lines.push("## Test Type Requirements");
  lines.push("");
  lines.push(...TEST_TYPE_INSTRUCTIONS[testType]);
  lines.push("");

  // Ground truth usage hints for python_script
  if (testType === "python_script" && (pageAnalysis || collectedAnalyses)) {
    lines.push("### Using Ground Truth Data");
    lines.push("Use the analysis data provided above to:");
    lines.push("- Verify API-detected elements match actual page elements");
    lines.push("- Compare bounding boxes (API response vs ground truth)");
    lines.push("- Check if API missed any clickable elements");
    lines.push("- Validate that element counts are reasonable");
    lines.push("");
  }

  lines.push("Return only the test code, no explanation.");

  // Truncate if needed
  let prompt = lines.join("\n");
  if (prompt.length > maxResponseLength) {
    prompt = prompt.substring(0, maxResponseLength) + "\n\n... (truncated)";
  }

  return {
    prompt,
    assertableFields: allAssertableFields,
    includedSections,
  };
}

// ============================================================================
// Formatting Helpers
// ============================================================================

/**
 * Format a single page analysis (legacy support).
 */
function formatSinglePageAnalysis(analysis: PageAnalysis, testType: TestType): string {
  const lines: string[] = [];

  lines.push("## Page Analysis (Ground Truth)");
  lines.push("");

  if (testType === "python_script") {
    lines.push("**This is ground truth data from actual page analysis.**");
    lines.push("Use this to verify that API responses correctly identify these elements.");
    lines.push("");
  }

  lines.push("The following elements were detected on the page:");
  lines.push("");
  lines.push("| # | Label | Type | Text | Selector/Bounds |");
  lines.push("|---|-------|------|------|-----------------|");

  for (let idx = 0; idx < Math.min(analysis.elements.length, 30); idx++) {
    const el = analysis.elements[idx];
    const label = el.label || "unknown";
    const elType = el.element_type || "unknown";
    let text = el.text_content || "-";
    if (text.length > 30) text = text.substring(0, 30) + "...";
    text = text !== "-" ? `"${text}"` : "-";

    const selector = el.selector || `(${el.bounding_box?.x || 0}, ${el.bounding_box?.y || 0})`;
    lines.push(`| ${idx + 1} | ${label} | ${elType} | ${text} | ${selector} |`);
  }

  lines.push("");

  if (analysis.url) {
    lines.push(`Page URL: ${analysis.url}`);
    lines.push("");
  }

  return lines.join("\n");
}

/**
 * Format multiple page analyses.
 */
function formatPageAnalyses(
  analyses: Array<{ name: string; data: PageAnalysis }>,
  _testType: TestType,
): string {
  const lines: string[] = [];

  lines.push("## Page Analyses (Ground Truth)");
  lines.push("");
  lines.push(`**${analyses.length} page analyses collected:**`);
  lines.push("");

  for (let i = 0; i < analyses.length; i++) {
    const { name, data } = analyses[i];
    lines.push(`### Analysis ${i + 1}: ${name}`);
    lines.push(`- Source: ${data.source}`);
    lines.push(`- Elements: ${data.elements.length}`);
    if (data.url) lines.push(`- URL: ${data.url}`);
    lines.push("");

    // Show element table (limited)
    if (data.elements.length > 0) {
      lines.push("| # | Label | Type | Text |");
      lines.push("|---|-------|------|------|");
      for (let j = 0; j < Math.min(data.elements.length, 15); j++) {
        const el = data.elements[j];
        let text = el.text_content || "-";
        if (text.length > 25) text = text.substring(0, 25) + "...";
        lines.push(`| ${j + 1} | ${el.label || "-"} | ${el.element_type || "-"} | ${text} |`);
      }
      if (data.elements.length > 15) {
        lines.push(`*... and ${data.elements.length - 15} more elements*`);
      }
      lines.push("");
    }
  }

  return lines.join("\n");
}

/**
 * Format API analyses (legacy format).
 */
function formatApiAnalyses(analyses: Array<{ name: string; data: ApiRequestAnalysis }>): string {
  const lines: string[] = [];

  lines.push("## API Analyses");
  lines.push("");

  for (let i = 0; i < analyses.length; i++) {
    const { name, data } = analyses[i];
    lines.push(`### API ${i + 1}: ${name}`);
    lines.push(`- Method: ${data.method}`);
    lines.push(`- URL: ${data.url}`);
    if (data.status_code) lines.push(`- Status: ${data.status_code}`);
    if (data.duration_ms) lines.push(`- Duration: ${data.duration_ms}ms`);
    lines.push("");

    if (data.response !== undefined) {
      lines.push("**Response:**");
      lines.push("```json");
      try {
        let responseStr = JSON.stringify(data.response, null, 2);
        if (responseStr.length > 6000) {
          responseStr = responseStr.substring(0, 6000) + "\n... (truncated)";
        }
        lines.push(responseStr);
      } catch {
        lines.push(String(data.response).substring(0, 3000));
      }
      lines.push("```");
      lines.push("");
    }
  }

  return lines.join("\n");
}

/**
 * Format assertable fields for the AI.
 */
function formatAssertableFields(fields: AssertableField[]): string {
  const lines: string[] = [];

  lines.push("## Assertable Fields");
  lines.push("");
  lines.push("The following fields from the collected data can be used in assertions:");
  lines.push("");

  // Group by path prefix (output ID)
  const byOutput = new Map<string, AssertableField[]>();
  for (const field of fields) {
    const prefix = field.path.split(".")[0];
    const existing = byOutput.get(prefix) || [];
    existing.push(field);
    byOutput.set(prefix, existing);
  }

  for (const [outputId, outputFields] of byOutput) {
    lines.push(`### ${outputId}`);
    for (const field of outputFields.slice(0, 10)) {
      lines.push(`- **${field.name}** (\`${field.path}\`): ${field.description}`);
      if (field.suggested_assertions?.length) {
        lines.push(`  - Suggested: ${field.suggested_assertions[0]}`);
      }
    }
    if (outputFields.length > 10) {
      lines.push(`  *... and ${outputFields.length - 10} more fields*`);
    }
    lines.push("");
  }

  return lines.join("\n");
}

// ============================================================================
// Export for convenience
// ============================================================================

export type { AssertableField };
