/**
 * Check Step Output Handler
 *
 * Handles outputs from check/validation steps, providing:
 * - Parsing of check results with issues
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type CheckStepOutput,
  type StepOutputType,
  type CheckIssue,
  generateStepOutputId,
} from "../../types/step-output";
import type {
  StepOutputHandler,
  StepOutputMetadata,
  TestAutoPopulateConfig,
  AssertableField,
} from "./types";

// ============================================================================
// Raw Input Types
// ============================================================================

interface RawCheckOutput {
  check_type?: string;
  issues?: CheckIssue[];
  errors?: CheckIssue[]; // Alternative field name
  checks_passed?: number;
  passed?: number; // Alternative field name
  checks_failed?: number;
  failed?: number; // Alternative field name
  raw_output?: string;
  output?: string; // Alternative field name
  reference_screenshot?: string;
  current_screenshot?: string;
}

interface CheckStepConfig {
  check_type?: string;
  threshold?: number;
  expected_elements?: string[];
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class CheckHandler implements StepOutputHandler<CheckStepOutput> {
  readonly stepType: StepOutputType = "check";
  readonly displayName = "Check/Validation";
  readonly description = "Validation check with pass/fail results and issues";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): CheckStepOutput {
    const raw = (rawOutput || {}) as RawCheckOutput;
    const config = (stepConfig || {}) as CheckStepConfig;

    const issues = raw.issues || raw.errors || [];
    const checksPassed = raw.checks_passed ?? raw.passed ?? 0;
    const checksFailed = raw.checks_failed ?? raw.failed ?? issues.length;

    const checkType = this.normalizeCheckType(raw.check_type || config.check_type || "custom");

    return {
      id: generateStepOutputId(),
      step_type: "check",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || `${checkType} Check`,
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? checksFailed === 0,
      error: metadata?.error,
      check_type: checkType,
      issues,
      checks_passed: checksPassed,
      checks_failed: checksFailed,
      raw_output: raw.raw_output || raw.output,
      reference_screenshot: raw.reference_screenshot,
      current_screenshot: raw.current_screenshot,
    };
  }

  summarizeForAI(output: CheckStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Check: ${output.step_name}`);
    lines.push(`- **Check Type:** ${output.check_type}`);

    const total = output.checks_passed + output.checks_failed;
    const passRate = total > 0 ? ((output.checks_passed / total) * 100).toFixed(0) : "N/A";
    lines.push(`- **Results:** ${output.checks_passed}/${total} passed (${passRate}%)`);

    const statusEmoji = output.checks_failed === 0 ? "✅" : "❌";
    lines.push(
      `- **Status:** ${statusEmoji} ${output.checks_failed === 0 ? "All Passed" : `${output.checks_failed} Failed`}`,
    );

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Issues
    if (output.issues.length > 0) {
      lines.push("");
      lines.push(`**Issues Found:** ${output.issues.length}`);
      lines.push("");

      // Group by severity
      const errors = output.issues.filter((i) => i.severity === "error");
      const warnings = output.issues.filter((i) => i.severity === "warning");
      const infos = output.issues.filter((i) => i.severity === "info");

      if (errors.length > 0) {
        lines.push(`**Errors (${errors.length}):**`);
        for (const issue of errors.slice(0, 5)) {
          const location = issue.location ? ` [${issue.location}]` : "";
          lines.push(`- ❌ ${issue.message}${location}`);
          if (issue.suggestion) {
            lines.push(`  *Suggestion: ${issue.suggestion}*`);
          }
        }
        if (errors.length > 5) {
          lines.push(`  ... and ${errors.length - 5} more errors`);
        }
      }

      if (warnings.length > 0) {
        lines.push("");
        lines.push(`**Warnings (${warnings.length}):**`);
        for (const issue of warnings.slice(0, 3)) {
          const location = issue.location ? ` [${issue.location}]` : "";
          lines.push(`- ⚠️ ${issue.message}${location}`);
        }
        if (warnings.length > 3) {
          lines.push(`  ... and ${warnings.length - 3} more warnings`);
        }
      }

      if (infos.length > 0 && errors.length === 0 && warnings.length === 0) {
        lines.push("");
        lines.push(`**Info (${infos.length}):**`);
        for (const issue of infos.slice(0, 3)) {
          lines.push(`- ℹ️ ${issue.message}`);
        }
      }
    }

    // Screenshots for visual regression
    if (output.reference_screenshot || output.current_screenshot) {
      lines.push("");
      lines.push("**Screenshots:**");
      if (output.reference_screenshot) lines.push("- Reference screenshot available");
      if (output.current_screenshot) lines.push("- Current screenshot available");
    }

    return lines.join("\n");
  }

  toTestConfig(output: CheckStepOutput): TestAutoPopulateConfig | null {
    const config: Record<string, unknown> = {
      check_type: output.check_type,
      expected_pass_count: output.checks_passed,
      expected_fail_count: output.checks_failed,
    };

    if (output.issues.length > 0) {
      config.known_issues = output.issues.map((i) => ({
        severity: i.severity,
        message: i.message,
      }));
    }

    return {
      config_key: "check_config",
      config_value: config,
      description: `Check config for ${output.check_type} validation`,
    };
  }

  getAssertableFields(output: CheckStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Overall success
    fields.push({
      path: "success",
      name: "Check Success",
      description: "Whether all checks passed",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert all checks passed"],
    });

    // Checks passed
    fields.push({
      path: "checks_passed",
      name: "Checks Passed",
      description: "Number of checks that passed",
      type: "number",
      example_value: output.checks_passed,
      suggested_assertions: ["Assert expected number of checks passed"],
    });

    // Checks failed
    fields.push({
      path: "checks_failed",
      name: "Checks Failed",
      description: "Number of checks that failed",
      type: "number",
      example_value: output.checks_failed,
      suggested_assertions: [
        "Assert no checks failed",
        "Assert failed count below threshold",
      ],
    });

    // Issue count
    fields.push({
      path: "issues.length",
      name: "Issue Count",
      description: "Total number of issues found",
      type: "number",
      example_value: output.issues.length,
      suggested_assertions: ["Assert no issues found", "Assert issue count below threshold"],
    });

    // Error count
    const errorCount = output.issues.filter((i) => i.severity === "error").length;
    fields.push({
      path: "issues.error_count",
      name: "Error Count",
      description: "Number of error-severity issues",
      type: "number",
      example_value: errorCount,
      suggested_assertions: ["Assert no errors found"],
    });

    // Warning count
    const warningCount = output.issues.filter((i) => i.severity === "warning").length;
    fields.push({
      path: "issues.warning_count",
      name: "Warning Count",
      description: "Number of warning-severity issues",
      type: "number",
      example_value: warningCount,
      suggested_assertions: ["Assert warning count below threshold"],
    });

    // Individual issues (first few)
    for (let i = 0; i < Math.min(output.issues.length, 3); i++) {
      const issue = output.issues[i];
      fields.push({
        path: `issues[${i}].message`,
        name: `Issue ${i + 1}`,
        description: `Issue: ${issue.message.substring(0, 50)}...`,
        type: "string",
        example_value: issue.message,
        suggested_assertions: ["Check if issue is expected/known"],
      });
    }

    // Check type
    fields.push({
      path: "check_type",
      name: "Check Type",
      description: "Type of validation performed",
      type: "string",
      example_value: output.check_type,
      suggested_assertions: ["Verify correct check type was run"],
    });

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Check Duration",
        description: "Time taken to run checks in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert check completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    return fields;
  }

  /**
   * Normalize check type to valid enum value.
   */
  private normalizeCheckType(checkType: string): CheckStepOutput["check_type"] {
    const normalized = checkType.toLowerCase().replace(/[_-]/g, "");
    switch (normalized) {
      case "accessibility":
      case "a11y":
        return "accessibility";
      case "visualregression":
      case "visual":
      case "screenshot":
        return "visual_regression";
      case "elementpresence":
      case "element":
      case "presence":
        return "element_presence";
      case "textmatch":
      case "text":
        return "text_match";
      default:
        return "custom";
    }
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const checkHandler = new CheckHandler();
