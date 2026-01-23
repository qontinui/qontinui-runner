/**
 * Shell Command Step Output Handler
 *
 * Handles outputs from shell command steps, providing:
 * - Parsing of command output (stdout/stderr)
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type ShellCommandStepOutput,
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

interface RawShellCommandOutput {
  command?: string;
  working_directory?: string;
  exit_code?: number;
  code?: number; // Alternative field name
  stdout?: string;
  output?: string; // Alternative field name
  stderr?: string;
  error_output?: string; // Alternative field name
  env_vars?: Record<string, string>;
  extractions?: Record<string, unknown>;
}

interface ShellCommandStepConfig {
  command?: string;
  working_directory?: string;
  cwd?: string; // Alternative field name
  env?: Record<string, string>;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class ShellCommandHandler implements StepOutputHandler<ShellCommandStepOutput> {
  readonly stepType: StepOutputType = "shell_command";
  readonly displayName = "Shell Command";
  readonly description = "Command execution with stdout, stderr, and exit code";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): ShellCommandStepOutput {
    const raw = (rawOutput || {}) as RawShellCommandOutput;
    const config = (stepConfig || {}) as ShellCommandStepConfig;

    const command = raw.command || config.command || "";
    const exitCode = raw.exit_code ?? raw.code ?? 0;
    const stdout = raw.stdout ?? raw.output ?? "";
    const stderr = raw.stderr ?? raw.error_output ?? "";

    return {
      id: generateStepOutputId(),
      step_type: "shell_command",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.truncateCommand(command),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? exitCode === 0,
      error: metadata?.error ?? (exitCode !== 0 ? `Exit code: ${exitCode}` : undefined),
      command,
      working_directory: raw.working_directory || config.working_directory || config.cwd,
      exit_code: exitCode,
      stdout,
      stderr,
      env_vars: raw.env_vars || config.env,
      extractions: raw.extractions,
    };
  }

  summarizeForAI(output: ShellCommandStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Shell Command: ${output.step_name}`);
    lines.push("```bash");
    lines.push(output.command);
    lines.push("```");

    if (output.working_directory) {
      lines.push(`- **Working Directory:** ${output.working_directory}`);
    }

    const statusEmoji = output.exit_code === 0 ? "✅" : "❌";
    lines.push(`- **Exit Code:** ${statusEmoji} ${output.exit_code}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    // Stdout
    if (output.stdout && output.stdout.trim()) {
      lines.push("");
      lines.push("**Standard Output:**");
      lines.push("```");
      // Truncate long output
      const stdout =
        output.stdout.length > 4000
          ? output.stdout.substring(0, 4000) + "\n... (truncated)"
          : output.stdout;
      lines.push(stdout);
      lines.push("```");
    }

    // Stderr
    if (output.stderr && output.stderr.trim()) {
      lines.push("");
      lines.push("**Standard Error:**");
      lines.push("```");
      const stderr =
        output.stderr.length > 2000
          ? output.stderr.substring(0, 2000) + "\n... (truncated)"
          : output.stderr;
      lines.push(stderr);
      lines.push("```");
    }

    // Extractions
    if (output.extractions && Object.keys(output.extractions).length > 0) {
      lines.push("");
      lines.push("**Extracted Variables:**");
      for (const [key, value] of Object.entries(output.extractions)) {
        const valueStr =
          typeof value === "object" ? JSON.stringify(value) : String(value);
        lines.push(`- \`${key}\`: ${valueStr.substring(0, 100)}`);
      }
    }

    return lines.join("\n");
  }

  toTestConfig(output: ShellCommandStepOutput): TestAutoPopulateConfig | null {
    if (!output.command) {
      return null;
    }

    const config: Record<string, unknown> = {
      command: output.command,
    };

    if (output.working_directory) {
      config.working_directory = output.working_directory;
    }

    if (output.env_vars && Object.keys(output.env_vars).length > 0) {
      config.env_vars = output.env_vars;
    }

    return {
      config_key: "shell_command_config",
      config_value: config,
      description: `Shell command config for: ${this.truncateCommand(output.command)}`,
    };
  }

  getAssertableFields(output: ShellCommandStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Exit code
    fields.push({
      path: "exit_code",
      name: "Exit Code",
      description: "The command exit code (0 typically means success)",
      type: "number",
      example_value: output.exit_code,
      suggested_assertions: [
        "Assert exit code equals 0",
        "Assert exit code is in expected range",
      ],
    });

    // Stdout
    if (output.stdout) {
      fields.push({
        path: "stdout",
        name: "Standard Output",
        description: "The command standard output",
        type: "string",
        example_value: this.truncateValue(output.stdout),
        suggested_assertions: [
          "Assert stdout contains expected text",
          "Assert stdout matches pattern",
          "Parse stdout as JSON and validate",
        ],
      });
    }

    // Stderr
    if (output.stderr) {
      fields.push({
        path: "stderr",
        name: "Standard Error",
        description: "The command standard error output",
        type: "string",
        example_value: this.truncateValue(output.stderr),
        suggested_assertions: [
          "Assert stderr is empty (no errors)",
          "Assert stderr contains expected warning",
        ],
      });
    }

    // Success
    fields.push({
      path: "success",
      name: "Command Success",
      description: "Whether the command completed successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert command succeeded"],
    });

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Execution Duration",
        description: "Command execution time in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert execution time is under threshold",
          "Record as performance metric",
        ],
      });
    }

    // Extractions
    if (output.extractions) {
      for (const [key, value] of Object.entries(output.extractions)) {
        fields.push({
          path: `extractions.${key}`,
          name: `Extracted: ${key}`,
          description: `Variable extracted from output: ${key}`,
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
   * Truncate command for display.
   */
  private truncateCommand(command: string): string {
    if (command.length <= 50) return command;
    return command.substring(0, 47) + "...";
  }

  /**
   * Truncate a value for display.
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

export const shellCommandHandler = new ShellCommandHandler();
