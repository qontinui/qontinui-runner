/**
 * Code Execution Step Output Handler
 *
 * Handles outputs from `code_execution` steps (inline Python or Python file
 * run in an optional sandbox), providing:
 * - Parsing of stdout/stderr/return-value/traceback
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 *
 * Input schema: `qontinui-schemas/.../per_type/code_execution_step.py`
 * (`CodeExecutionStep`, wire tag `"code_execution"`).
 */

import {
  type CodeExecutionSandboxMode,
  type CodeExecutionStepOutput,
  type StepOutputType,
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

interface RawCodeExecutionOutput {
  exit_code?: number;
  code?: number; // Alternative field name
  stdout?: string;
  output?: string; // Alternative field name
  stderr?: string;
  error_output?: string; // Alternative field name
  return_value?: unknown;
  result?: unknown; // Alternative field name
  extractions?: Record<string, unknown>;
  exception_type?: string;
  exception?: string; // Alternative field name
  traceback?: string;
}

interface CodeExecutionStepConfig {
  code?: string;
  code_file?: string;
  sandbox_mode?: string;
  timeout_seconds?: number;
}

const KNOWN_SANDBOX_MODES: ReadonlySet<CodeExecutionSandboxMode> = new Set(["enforce", "warn"]);

// ============================================================================
// Handler Implementation
// ============================================================================

export class CodeExecutionHandler implements StepOutputHandler<CodeExecutionStepOutput> {
  readonly stepType: StepOutputType = "code_execution";
  readonly displayName = "Code Execution";
  readonly description =
    "Inline or file-based Python execution with stdout, stderr, return value, and sandbox mode";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): CodeExecutionStepOutput {
    const raw = (rawOutput || {}) as RawCodeExecutionOutput;
    const config = (stepConfig || {}) as CodeExecutionStepConfig;

    const exitCode = raw.exit_code ?? raw.code;
    const stdout = raw.stdout ?? raw.output;
    const stderr = raw.stderr ?? raw.error_output;
    const returnValue = raw.return_value ?? raw.result;
    const exceptionType = raw.exception_type ?? raw.exception;

    const codeText = config.code;
    const codeFile = config.code_file;

    const failed =
      metadata?.success === false || (exitCode !== undefined && exitCode !== 0) || !!exceptionType;

    return {
      id: generateStepOutputId(),
      step_type: "code_execution",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.deriveName(codeText, codeFile),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? !failed,
      error:
        metadata?.error ??
        (exceptionType
          ? `${exceptionType}`
          : exitCode !== undefined && exitCode !== 0
            ? `Exit code: ${exitCode}`
            : undefined),
      code: codeText,
      code_file: codeFile,
      sandbox_mode: this.normalizeSandboxMode(config.sandbox_mode),
      timeout_seconds: config.timeout_seconds,
      exit_code: exitCode,
      stdout,
      stderr,
      return_value: returnValue,
      extractions: raw.extractions,
      exception_type: exceptionType,
      traceback: raw.traceback,
    };
  }

  summarizeForAI(output: CodeExecutionStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Code Execution: ${output.step_name}`);

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Succeeded" : "Failed"}`);

    if (output.exit_code !== undefined) {
      lines.push(`- **Exit Code:** ${output.exit_code}`);
    }

    if (output.sandbox_mode) {
      lines.push(`- **Sandbox Mode:** \`${output.sandbox_mode}\``);
    }

    if (output.timeout_seconds !== undefined) {
      lines.push(`- **Timeout:** ${output.timeout_seconds}s`);
    }

    if (output.code_file) {
      lines.push(`- **Source:** ${output.code_file}`);
    } else if (output.code) {
      lines.push(`- **Source:** inline (${output.code.split(/\r?\n/).length} lines)`);
    }

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Inline code body (only when there's no separate file)
    if (output.code && !output.code_file) {
      lines.push("");
      lines.push("**Code:**");
      lines.push("```python");
      lines.push(this.truncateBlock(output.code, 4000));
      lines.push("```");
    }

    // Stdout
    if (output.stdout && output.stdout.trim()) {
      lines.push("");
      lines.push("**Standard Output:**");
      lines.push("```");
      lines.push(this.truncateBlock(output.stdout, 4000));
      lines.push("```");
    }

    // Stderr
    if (output.stderr && output.stderr.trim()) {
      lines.push("");
      lines.push("**Standard Error:**");
      lines.push("```");
      lines.push(this.truncateBlock(output.stderr, 2000));
      lines.push("```");
    }

    // Traceback
    if (output.traceback && output.traceback.trim()) {
      lines.push("");
      lines.push(`**Traceback (${output.exception_type || "exception"}):**`);
      lines.push("```");
      lines.push(this.truncateBlock(output.traceback, 4000));
      lines.push("```");
    }

    // Return value
    if (output.return_value !== undefined && output.return_value !== null) {
      lines.push("");
      lines.push("**Return Value:**");
      lines.push("```json");
      lines.push(this.stringifyValue(output.return_value, 2000));
      lines.push("```");
    }

    // Extractions
    if (output.extractions && Object.keys(output.extractions).length > 0) {
      lines.push("");
      lines.push("**Extracted Variables:**");
      for (const [key, value] of Object.entries(output.extractions)) {
        const valueStr = typeof value === "object" ? JSON.stringify(value) : String(value);
        lines.push(`- \`${key}\`: ${this.truncateValue(valueStr)}`);
      }
    }

    return lines.join("\n");
  }

  toTestConfig(output: CodeExecutionStepOutput): TestAutoPopulateConfig | null {
    if (!output.code && !output.code_file) {
      // Nothing replayable — tests need source to re-run.
      return null;
    }

    const config: Record<string, unknown> = {};
    if (output.code) config.code = output.code;
    if (output.code_file) config.code_file = output.code_file;
    if (output.sandbox_mode) config.sandbox_mode = output.sandbox_mode;
    if (output.timeout_seconds !== undefined) config.timeout_seconds = output.timeout_seconds;
    if (output.exit_code !== undefined) config.expected_exit_code = output.exit_code;

    return {
      config_key: "code_execution_config",
      config_value: config,
      description: `Code execution config for ${output.code_file || "inline code"}`,
    };
  }

  getAssertableFields(output: CodeExecutionStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    fields.push({
      path: "success",
      name: "Execution Success",
      description: "Whether the code ran without raising or exiting non-zero",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert code executed successfully"],
    });

    if (output.exit_code !== undefined) {
      fields.push({
        path: "exit_code",
        name: "Exit Code",
        description: "Process exit code from the Python runtime",
        type: "number",
        example_value: output.exit_code,
        suggested_assertions: [
          "Assert exit code equals 0",
          "Assert exit code is in expected range",
        ],
      });
    }

    if (output.stdout !== undefined) {
      fields.push({
        path: "stdout",
        name: "Standard Output",
        description: "Captured standard output",
        type: "string",
        example_value: this.truncateValue(output.stdout ?? ""),
        suggested_assertions: [
          "Assert stdout contains expected text",
          "Assert stdout matches pattern",
          "Parse stdout as JSON and validate",
        ],
      });
    }

    if (output.stderr !== undefined) {
      fields.push({
        path: "stderr",
        name: "Standard Error",
        description: "Captured standard error",
        type: "string",
        example_value: this.truncateValue(output.stderr ?? ""),
        suggested_assertions: [
          "Assert stderr is empty (no errors)",
          "Assert stderr contains expected warning",
        ],
      });
    }

    if (output.return_value !== undefined) {
      fields.push({
        path: "return_value",
        name: "Return Value",
        description: "Structured value returned by the code (e.g., JSON trailer)",
        type: this.getJsType(output.return_value),
        example_value: output.return_value,
        suggested_assertions: [
          "Assert return value matches expected",
          "Assert return value is not null",
        ],
      });
    }

    if (output.exception_type) {
      fields.push({
        path: "exception_type",
        name: "Exception Type",
        description: "Runtime/exception class that caused failure",
        type: "string",
        example_value: output.exception_type,
        suggested_assertions: [
          "Assert no exception was raised",
          "Assert raised exception is of expected type",
        ],
      });
    }

    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Execution Duration",
        description: "Execution time in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert execution time is under threshold",
          "Record as performance metric",
        ],
      });
    }

    if (output.extractions) {
      for (const [key, value] of Object.entries(output.extractions)) {
        fields.push({
          path: `extractions.${key}`,
          name: `Extracted: ${key}`,
          description: `Variable extracted from the run: ${key}`,
          type: this.getJsType(value),
          example_value: value,
          suggested_assertions: [
            `Assert ${key} equals expected value`,
            `Assert ${key} is not null`,
          ],
        });
      }
    }

    return fields;
  }

  /**
   * Derive a compact step name from source (file path or first line of code).
   */
  private deriveName(code?: string, codeFile?: string): string {
    if (codeFile) {
      const parts = codeFile.split(/[\\/]/);
      return parts[parts.length - 1] || codeFile;
    }
    if (!code) return "code_execution";
    const firstLine = code.split(/\r?\n/).find((l) => l.trim().length > 0) ?? "";
    if (firstLine.length <= 50) return firstLine || "inline code";
    return firstLine.substring(0, 47) + "...";
  }

  /**
   * Normalize a sandbox_mode string into the typed enum, or drop unknown values.
   */
  private normalizeSandboxMode(mode: string | undefined): CodeExecutionSandboxMode | undefined {
    if (!mode) return undefined;
    return KNOWN_SANDBOX_MODES.has(mode as CodeExecutionSandboxMode)
      ? (mode as CodeExecutionSandboxMode)
      : undefined;
  }

  /**
   * Truncate a code-block body using the shared 4000-char budget.
   */
  private truncateBlock(value: string, limit: number): string {
    if (value.length <= limit) return value;
    return value.substring(0, limit) + "\n... (truncated)";
  }

  /**
   * Stringify a value as JSON, falling back to String() and truncating.
   */
  private stringifyValue(value: unknown, limit: number): string {
    let text: string;
    try {
      text = JSON.stringify(value, null, 2) ?? String(value);
    } catch {
      text = String(value);
    }
    if (text.length <= limit) return text;
    return text.substring(0, limit) + "\n... (truncated)";
  }

  /**
   * Truncate an inline value for display in a bullet line.
   */
  private truncateValue(value: string): string {
    if (value.length <= 200) return value;
    return value.substring(0, 197) + "...";
  }

  /**
   * Get JavaScript type for a value.
   */
  private getJsType(
    value: unknown,
  ): "string" | "number" | "boolean" | "array" | "object" | "null" | "any" {
    if (value === null) return "null";
    if (Array.isArray(value)) return "array";
    const type = typeof value;
    if (type === "string" || type === "number" || type === "boolean") return type;
    if (type === "object") return "object";
    return "any";
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const codeExecutionHandler = new CodeExecutionHandler();
