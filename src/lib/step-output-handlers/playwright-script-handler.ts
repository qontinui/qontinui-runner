/**
 * Playwright Script Step Output Handler
 *
 * Handles outputs from Playwright script execution steps, providing:
 * - Parsing of script execution results
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type PlaywrightScriptStepOutput,
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

interface ConsoleLogEntry {
  type: "log" | "warn" | "error" | "info";
  message: string;
  timestamp: string;
}

interface NetworkRequestEntry {
  url: string;
  method: string;
  status?: number;
  response_body?: unknown;
}

interface RawPlaywrightScriptOutput {
  script_content?: string;
  final_url?: string;
  url?: string; // Alternative field name
  page_title?: string;
  title?: string; // Alternative field name
  console_logs?: ConsoleLogEntry[];
  logs?: ConsoleLogEntry[]; // Alternative field name
  network_requests?: NetworkRequestEntry[];
  requests?: NetworkRequestEntry[]; // Alternative field name
  final_screenshot?: string;
  screenshot?: string; // Alternative field name
  extracted_data?: Record<string, unknown>;
  data?: Record<string, unknown>; // Alternative field name
}

interface PlaywrightScriptStepConfig {
  script_id?: string;
  script_content?: string;
  target_url?: string;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class PlaywrightScriptHandler implements StepOutputHandler<PlaywrightScriptStepOutput> {
  readonly stepType: StepOutputType = "playwright_script";
  readonly displayName = "Playwright Script";
  readonly description = "Browser automation script with page state and network data";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): PlaywrightScriptStepOutput {
    const raw = (rawOutput || {}) as RawPlaywrightScriptOutput;
    const config = (stepConfig || {}) as PlaywrightScriptStepConfig;

    return {
      id: generateStepOutputId(),
      step_type: "playwright_script",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || "Playwright Script",
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? true,
      error: metadata?.error,
      script_content: raw.script_content || config.script_content,
      final_url: raw.final_url || raw.url,
      page_title: raw.page_title || raw.title,
      console_logs: raw.console_logs || raw.logs,
      network_requests: raw.network_requests || raw.requests,
      final_screenshot: raw.final_screenshot || raw.screenshot,
      extracted_data: raw.extracted_data || raw.data,
    };
  }

  summarizeForAI(output: PlaywrightScriptStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Playwright Script: ${output.step_name}`);

    if (output.final_url) {
      lines.push(`- **Final URL:** ${output.final_url}`);
    }

    if (output.page_title) {
      lines.push(`- **Page Title:** ${output.page_title}`);
    }

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Completed" : "Failed"}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Console logs
    if (output.console_logs && output.console_logs.length > 0) {
      lines.push("");
      lines.push(`**Console Logs:** ${output.console_logs.length} entries`);

      const errors = output.console_logs.filter((l) => l.type === "error");
      const warnings = output.console_logs.filter((l) => l.type === "warn");

      if (errors.length > 0) {
        lines.push(`- ❌ ${errors.length} errors`);
      }
      if (warnings.length > 0) {
        lines.push(`- ⚠️ ${warnings.length} warnings`);
      }

      // Show recent errors
      if (errors.length > 0) {
        lines.push("");
        lines.push("**Console Errors:**");
        for (const err of errors.slice(0, 3)) {
          lines.push(`- ${err.message.substring(0, 100)}`);
        }
      }
    }

    // Network requests
    if (output.network_requests && output.network_requests.length > 0) {
      lines.push("");
      lines.push(`**Network Requests:** ${output.network_requests.length}`);

      const failed = output.network_requests.filter((r) => r.status && r.status >= 400);
      if (failed.length > 0) {
        lines.push(`- ❌ ${failed.length} failed requests`);
        for (const req of failed.slice(0, 3)) {
          lines.push(`  - ${req.method} ${req.url} (${req.status})`);
        }
      }
    }

    // Extracted data
    if (output.extracted_data && Object.keys(output.extracted_data).length > 0) {
      lines.push("");
      lines.push("**Extracted Data:**");
      lines.push("```json");
      try {
        let dataStr = JSON.stringify(output.extracted_data, null, 2);
        if (dataStr.length > 2000) {
          dataStr = dataStr.substring(0, 2000) + "\n... (truncated)";
        }
        lines.push(dataStr);
      } catch {
        lines.push(String(output.extracted_data).substring(0, 1000));
      }
      lines.push("```");
    }

    // Screenshot availability
    if (output.final_screenshot) {
      lines.push("");
      lines.push("**Final Screenshot:** Available");
    }

    return lines.join("\n");
  }

  toTestConfig(output: PlaywrightScriptStepOutput): TestAutoPopulateConfig | null {
    const config: Record<string, unknown> = {};

    if (output.final_url) {
      config.expected_url = output.final_url;
    }

    if (output.page_title) {
      config.expected_title = output.page_title;
    }

    if (output.extracted_data) {
      config.extracted_data = output.extracted_data;
    }

    if (Object.keys(config).length === 0) {
      return null;
    }

    return {
      config_key: "playwright_script_config",
      config_value: config,
      description: `Playwright script config for ${output.final_url || output.step_name}`,
    };
  }

  getAssertableFields(output: PlaywrightScriptStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Success status
    fields.push({
      path: "success",
      name: "Script Success",
      description: "Whether the Playwright script completed successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert script executed successfully"],
    });

    // Final URL
    if (output.final_url) {
      fields.push({
        path: "final_url",
        name: "Final URL",
        description: "The URL after script execution",
        type: "string",
        example_value: output.final_url,
        suggested_assertions: [
          "Assert navigated to expected URL",
          "Assert URL contains expected path",
        ],
      });
    }

    // Page title
    if (output.page_title) {
      fields.push({
        path: "page_title",
        name: "Page Title",
        description: "The page title after script execution",
        type: "string",
        example_value: output.page_title,
        suggested_assertions: ["Assert page title matches expected"],
      });
    }

    // Console errors
    if (output.console_logs) {
      const errorCount = output.console_logs.filter((l) => l.type === "error").length;
      fields.push({
        path: "console_logs.error_count",
        name: "Console Error Count",
        description: "Number of console errors during execution",
        type: "number",
        example_value: errorCount,
        suggested_assertions: [
          "Assert no console errors",
          "Assert error count below threshold",
        ],
      });
    }

    // Network request count
    if (output.network_requests) {
      fields.push({
        path: "network_requests.length",
        name: "Network Request Count",
        description: "Total number of network requests made",
        type: "number",
        example_value: output.network_requests.length,
        suggested_assertions: ["Assert expected number of API calls made"],
      });

      const failedCount = output.network_requests.filter(
        (r) => r.status && r.status >= 400,
      ).length;
      fields.push({
        path: "network_requests.failed_count",
        name: "Failed Request Count",
        description: "Number of failed network requests (4xx/5xx)",
        type: "number",
        example_value: failedCount,
        suggested_assertions: ["Assert no failed network requests"],
      });
    }

    // Extracted data fields
    if (output.extracted_data) {
      for (const [key, value] of Object.entries(output.extracted_data)) {
        fields.push({
          path: `extracted_data.${key}`,
          name: `Extracted: ${key}`,
          description: `Data extracted from page: ${key}`,
          type: this.getJsType(value),
          example_value: value,
          suggested_assertions: [
            `Assert ${key} equals expected value`,
            `Assert ${key} is not null`,
          ],
        });
      }
    }

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Script Duration",
        description: "Script execution time in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert script completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    return fields;
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

export const playwrightScriptHandler = new PlaywrightScriptHandler();
