/**
 * API Request Step Output Handler
 *
 * Handles outputs from API request steps, providing:
 * - Parsing of HTTP response data
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type ApiRequestStepOutput,
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

interface RawApiRequestOutput {
  method?: string;
  url?: string;
  request_headers?: Record<string, string>;
  request_body?: string;
  status_code?: number;
  status?: number; // Alternative field name
  response_headers?: Record<string, string>;
  response?: unknown;
  data?: unknown; // Alternative field name
  extractions?: Record<string, unknown>;
  duration_ms?: number;
  error?: string;
}

interface ApiRequestStepConfig {
  method?: string;
  url?: string;
  headers?: Record<string, string>;
  body?: string;
  timeout_ms?: number;
  follow_redirects?: boolean;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class ApiRequestHandler implements StepOutputHandler<ApiRequestStepOutput> {
  readonly stepType: StepOutputType = "api_request";
  readonly displayName = "API Request";
  readonly description = "HTTP request/response data with status, headers, and body";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): ApiRequestStepOutput {
    const raw = (rawOutput || {}) as RawApiRequestOutput;
    const config = (stepConfig || {}) as ApiRequestStepConfig;

    const method = raw.method || config.method || "GET";
    const url = raw.url || config.url || "";
    const statusCode = raw.status_code ?? raw.status;
    const response = raw.response ?? raw.data;

    return {
      id: generateStepOutputId(),
      step_type: "api_request",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || `${method} ${url}`,
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? raw.duration_ms ?? 0,
      success: metadata?.success ?? (statusCode !== undefined ? statusCode < 400 : true),
      error: metadata?.error ?? raw.error,
      method,
      url,
      request_headers: raw.request_headers || config.headers,
      request_body: raw.request_body || config.body,
      status_code: statusCode,
      response_headers: raw.response_headers,
      response,
      extractions: raw.extractions,
      source_config: {
        method,
        url,
        headers: config.headers || raw.request_headers,
        body: config.body || raw.request_body,
        timeout_ms: config.timeout_ms,
        follow_redirects: config.follow_redirects,
      },
    };
  }

  summarizeForAI(output: ApiRequestStepOutput): string {
    const lines: string[] = [];

    lines.push(`### API Request: ${output.step_name}`);
    lines.push(`- **Method:** ${output.method}`);
    lines.push(`- **URL:** ${output.url}`);

    if (output.status_code !== undefined) {
      const statusEmoji = output.status_code < 400 ? "✅" : "❌";
      lines.push(`- **Status:** ${statusEmoji} ${output.status_code}`);
    }

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Response summary
    if (output.response !== undefined) {
      lines.push("");
      lines.push("**Response Data:**");
      lines.push("```json");
      try {
        let responseStr = JSON.stringify(output.response, null, 2);
        // Truncate very long responses
        if (responseStr.length > 8000) {
          responseStr = responseStr.substring(0, 8000) + "\n... (truncated)";
        }
        lines.push(responseStr);
      } catch {
        lines.push(String(output.response).substring(0, 4000));
      }
      lines.push("```");
    }

    // Extractions summary
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

  toTestConfig(output: ApiRequestStepOutput): TestAutoPopulateConfig | null {
    // Only provide config if we have a valid source config
    if (!output.source_config.url) {
      return null;
    }

    return {
      config_key: "api_request_config",
      config_value: output.source_config,
      description: `API request configuration for ${output.method} ${output.url}`,
    };
  }

  getAssertableFields(output: ApiRequestStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Status code
    if (output.status_code !== undefined) {
      fields.push({
        path: "status_code",
        name: "HTTP Status Code",
        description: "The HTTP response status code",
        type: "number",
        example_value: output.status_code,
        suggested_assertions: [
          "Assert status code equals expected value",
          "Assert status code is in 2xx range",
          "Assert status code is not 4xx or 5xx",
        ],
      });
    }

    // Response body - only extract nested fields, don't show the object itself
    if (output.response !== undefined) {
      // Extract nested fields from response (these are the useful assertable values)
      if (typeof output.response === "object" && output.response !== null) {
        this.extractNestedFields(output.response as Record<string, unknown>, "response", fields);
      } else {
        // Non-object response (e.g., string, number)
        fields.push({
          path: "response",
          name: "Response Body",
          description: "The full response body",
          type: this.getJsType(output.response),
          example_value: this.truncateValue(output.response),
          suggested_assertions: [
            "Assert response equals expected value",
            "Assert response is not null/empty",
          ],
        });
      }
    }

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Response Duration",
        description: "Time taken for the request in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert response time is under threshold",
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
          description: `Variable extracted from response: ${key}`,
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
   * Extract assertable fields from nested objects.
   * Only shows leaf values (primitives, arrays with counts).
   * Intermediate objects are recursed into but not added as fields.
   */
  private extractNestedFields(
    obj: Record<string, unknown>,
    basePath: string,
    fields: AssertableField[],
    depth = 0,
  ): void {
    // Limit depth to avoid excessive nesting
    if (depth > 3) return;

    for (const [key, value] of Object.entries(obj)) {
      const path = `${basePath}.${key}`;

      // Handle arrays
      if (Array.isArray(value)) {
        fields.push({
          path,
          name: key,
          description: `Array with ${value.length} items`,
          type: "array",
          example_value: value.length > 5
            ? `[${value.length} items]`
            : JSON.stringify(value.slice(0, 5)),
          suggested_assertions: [
            `Assert ${key} has expected length`,
            `Assert ${key} contains expected items`,
          ],
        });
        continue;
      }

      // For nested objects, recurse but don't add the object itself as a field
      if (typeof value === "object" && value !== null) {
        this.extractNestedFields(value as Record<string, unknown>, path, fields, depth + 1);
        continue;
      }

      // Add leaf value fields (primitives)
      fields.push({
        path,
        name: key,
        description: `Response field: ${key}`,
        type: this.getJsType(value),
        example_value: this.truncateValue(value),
        suggested_assertions: [
          `Assert ${key} equals expected value`,
          `Assert ${key} is not null`,
        ],
      });
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
   * Truncate a value for display as an example.
   */
  private truncateValue(value: unknown): unknown {
    if (typeof value === "string" && value.length > 100) {
      return value.substring(0, 100) + "...";
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

export const apiRequestHandler = new ApiRequestHandler();
