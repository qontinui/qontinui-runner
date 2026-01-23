/**
 * MCP Call Step Output Handler
 *
 * Handles outputs from MCP (Model Context Protocol) tool calls, providing:
 * - Parsing of MCP tool results
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type McpCallStepOutput,
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

interface RawMcpCallOutput {
  server_id?: string;
  server?: string; // Alternative field name
  tool_name?: string;
  tool?: string; // Alternative field name
  arguments?: Record<string, unknown>;
  args?: Record<string, unknown>; // Alternative field name
  result?: unknown;
  response?: unknown; // Alternative field name
  is_error?: boolean;
  error?: boolean; // Alternative field name
}

interface McpCallStepConfig {
  server_id?: string;
  tool_name?: string;
  arguments?: Record<string, unknown>;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class McpCallHandler implements StepOutputHandler<McpCallStepOutput> {
  readonly stepType: StepOutputType = "mcp_call";
  readonly displayName = "MCP Tool Call";
  readonly description = "MCP server tool invocation with arguments and result";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): McpCallStepOutput {
    const raw = (rawOutput || {}) as RawMcpCallOutput;
    const config = (stepConfig || {}) as McpCallStepConfig;

    const serverId = raw.server_id || raw.server || config.server_id || "unknown";
    const toolName = raw.tool_name || raw.tool || config.tool_name || "unknown";
    const args = raw.arguments || raw.args || config.arguments || {};
    const result = raw.result ?? raw.response;
    const isError = raw.is_error ?? raw.error ?? false;

    return {
      id: generateStepOutputId(),
      step_type: "mcp_call",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || `${serverId}:${toolName}`,
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? !isError,
      error: metadata?.error ?? (isError && typeof result === "string" ? result : undefined),
      server_id: serverId,
      tool_name: toolName,
      arguments: args,
      result,
      is_error: isError,
    };
  }

  summarizeForAI(output: McpCallStepOutput): string {
    const lines: string[] = [];

    lines.push(`### MCP Tool Call: ${output.step_name}`);
    lines.push(`- **Server:** ${output.server_id}`);
    lines.push(`- **Tool:** ${output.tool_name}`);

    // Arguments
    if (Object.keys(output.arguments).length > 0) {
      lines.push("");
      lines.push("**Arguments:**");
      lines.push("```json");
      try {
        let argsStr = JSON.stringify(output.arguments, null, 2);
        if (argsStr.length > 2000) {
          argsStr = argsStr.substring(0, 2000) + "\n... (truncated)";
        }
        lines.push(argsStr);
      } catch {
        lines.push(String(output.arguments).substring(0, 1000));
      }
      lines.push("```");
    }

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Success" : "Error"}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    // Result
    if (output.result !== undefined) {
      lines.push("");
      lines.push("**Result:**");
      lines.push("```json");
      try {
        let resultStr = JSON.stringify(output.result, null, 2);
        if (resultStr.length > 6000) {
          resultStr = resultStr.substring(0, 6000) + "\n... (truncated)";
        }
        lines.push(resultStr);
      } catch {
        lines.push(String(output.result).substring(0, 3000));
      }
      lines.push("```");
    }

    if (output.error) {
      lines.push("");
      lines.push(`**Error:** ${output.error}`);
    }

    return lines.join("\n");
  }

  toTestConfig(output: McpCallStepOutput): TestAutoPopulateConfig | null {
    if (!output.server_id || !output.tool_name) {
      return null;
    }

    return {
      config_key: "mcp_call_config",
      config_value: {
        server_id: output.server_id,
        tool_name: output.tool_name,
        arguments: output.arguments,
      },
      description: `MCP config for ${output.server_id}:${output.tool_name}`,
    };
  }

  getAssertableFields(output: McpCallStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Success status
    fields.push({
      path: "success",
      name: "Call Success",
      description: "Whether the MCP tool call completed successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert tool call succeeded", "Assert no error returned"],
    });

    // Is error flag
    fields.push({
      path: "is_error",
      name: "Is Error",
      description: "Whether the tool returned an error response",
      type: "boolean",
      example_value: output.is_error,
      suggested_assertions: ["Assert is_error is false"],
    });

    // Result
    if (output.result !== undefined) {
      fields.push({
        path: "result",
        name: "Tool Result",
        description: "The result returned by the MCP tool",
        type: typeof output.result === "object" ? "object" : "any",
        example_value: this.truncateValue(output.result),
        suggested_assertions: [
          "Assert result is not null",
          "Assert result contains expected data",
        ],
      });

      // Extract nested fields from result
      if (typeof output.result === "object" && output.result !== null) {
        this.extractNestedFields(output.result as Record<string, unknown>, "result", fields);
      }
    }

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Call Duration",
        description: "Time taken for the MCP call in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert response time is under threshold",
          "Record as performance metric",
        ],
      });
    }

    // Arguments (for verification that correct args were passed)
    if (Object.keys(output.arguments).length > 0) {
      for (const [key, value] of Object.entries(output.arguments)) {
        fields.push({
          path: `arguments.${key}`,
          name: `Argument: ${key}`,
          description: `Argument passed to the tool: ${key}`,
          type: this.getJsType(value),
          example_value: value,
          suggested_assertions: [`Verify ${key} was passed correctly`],
        });
      }
    }

    return fields;
  }

  /**
   * Extract assertable fields from nested objects.
   */
  private extractNestedFields(
    obj: Record<string, unknown>,
    basePath: string,
    fields: AssertableField[],
    depth = 0,
  ): void {
    if (depth > 2) return;

    for (const [key, value] of Object.entries(obj)) {
      const path = `${basePath}.${key}`;

      // Skip very long arrays
      if (Array.isArray(value) && value.length > 50) {
        fields.push({
          path,
          name: key,
          description: `Array with ${value.length} items`,
          type: "array",
          example_value: `[${value.length} items]`,
          suggested_assertions: [`Assert ${key} has expected length`],
        });
        continue;
      }

      fields.push({
        path,
        name: key,
        description: `Result field: ${key}`,
        type: this.getJsType(value),
        example_value: this.truncateValue(value),
        suggested_assertions: [
          `Assert ${key} equals expected value`,
          `Assert ${key} is not null`,
        ],
      });

      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        this.extractNestedFields(value as Record<string, unknown>, path, fields, depth + 1);
      }
    }
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

  /**
   * Truncate a value for display.
   */
  private truncateValue(value: unknown): unknown {
    if (typeof value === "string" && value.length > 200) {
      return value.substring(0, 197) + "...";
    }
    if (Array.isArray(value) && value.length > 5) {
      return [...value.slice(0, 5), `... (${value.length - 5} more)`];
    }
    return value;
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const mcpCallHandler = new McpCallHandler();
