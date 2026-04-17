/**
 * Native Accessibility Step Output Handler
 *
 * Handles outputs from `native_accessibility` steps (UIA / AT-SPI / AX),
 * providing:
 * - Parsing of action-specific result shapes
 *   (`capture`, `click`, `type`, `focus`, `query`, `ai_context`)
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 *
 * Input schema: `qontinui-schemas/.../per_type/native_accessibility_step.py`
 * (`NativeAccessibilityStep`, wire tag `"native_accessibility"`).
 * Snapshot shape: `.../per_type/accessibility_snapshot.py`.
 */

import {
  type NativeAccessibilityAction,
  type NativeAccessibilityStepOutput,
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

interface RawAccessibilityNode {
  ref?: string;
  role?: string;
  name?: string;
  value?: string;
  automation_id?: string;
}

interface RawAccessibilitySnapshot {
  backend?: string;
  title?: string;
  url?: string;
  total_nodes?: number;
  interactive_nodes?: number;
  root?: unknown;
}

interface RawNativeAccessibilityOutput {
  action?: string;
  target?: string;
  backend?: string;
  ref_id?: string;
  text?: string;
  query_label?: string;
  query_role?: string;
  snapshot?: RawAccessibilitySnapshot | unknown;
  matches?: RawAccessibilityNode[] | unknown[];
  total_nodes?: number;
  interactive_nodes?: number;
  title?: string;
  url?: string;
}

interface NativeAccessibilityStepConfig {
  action?: string;
  target?: string;
  ref_id?: string;
  text?: string;
  query_label?: string;
  query_role?: string;
  include_hidden?: boolean;
  interactive_only?: boolean;
  max_depth?: number;
  max_elements?: number;
}

const KNOWN_ACTIONS: ReadonlySet<NativeAccessibilityAction> = new Set([
  "capture",
  "click",
  "type",
  "focus",
  "query",
  "ai_context",
]);

// ============================================================================
// Handler Implementation
// ============================================================================

export class NativeAccessibilityHandler implements StepOutputHandler<NativeAccessibilityStepOutput> {
  readonly stepType: StepOutputType = "native_accessibility";
  readonly displayName = "Native Accessibility";
  readonly description =
    "Native accessibility interaction (UIA/AT-SPI/AX): capture, click, type, focus, query, ai_context";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): NativeAccessibilityStepOutput {
    const raw = (rawOutput || {}) as RawNativeAccessibilityOutput;
    const config = (stepConfig || {}) as NativeAccessibilityStepConfig;

    const action = this.normalizeAction(raw.action ?? config.action) ?? "capture";
    const snapshot = raw.snapshot as RawAccessibilitySnapshot | undefined;

    // Prefer explicit summary fields; fall back to snapshot envelope if provided.
    const totalNodes = raw.total_nodes ?? snapshot?.total_nodes;
    const interactiveNodes = raw.interactive_nodes ?? snapshot?.interactive_nodes;
    const title = raw.title ?? snapshot?.title;
    const url = raw.url ?? snapshot?.url;
    const backend = raw.backend ?? snapshot?.backend;

    return {
      id: generateStepOutputId(),
      step_type: "native_accessibility",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.deriveName(action, raw, config),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? true,
      error: metadata?.error,
      action,
      target: raw.target ?? config.target,
      backend,
      ref_id: raw.ref_id ?? config.ref_id,
      text: raw.text ?? config.text,
      query_label: raw.query_label ?? config.query_label,
      query_role: raw.query_role ?? config.query_role,
      snapshot: raw.snapshot,
      matches: action === "query" ? (raw.matches as unknown[] | undefined) : undefined,
      total_nodes: totalNodes,
      interactive_nodes: interactiveNodes,
      title,
      url,
    };
  }

  summarizeForAI(output: NativeAccessibilityStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Native Accessibility: ${output.step_name}`);
    lines.push(`- **Action:** \`${output.action}\``);

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Succeeded" : "Failed"}`);

    if (output.backend) {
      lines.push(`- **Backend:** \`${output.backend}\``);
    }

    if (output.target) {
      lines.push(`- **Target:** ${output.target}`);
    }

    if (output.ref_id) {
      lines.push(`- **Element Ref:** \`${output.ref_id}\``);
    }

    if (output.text !== undefined && output.action === "type") {
      lines.push(`- **Text:** \`${this.truncateValue(output.text)}\``);
    }

    if (output.query_role || output.query_label) {
      const parts: string[] = [];
      if (output.query_role) parts.push(`role=\`${output.query_role}\``);
      if (output.query_label) parts.push(`label=\`${output.query_label}\``);
      lines.push(`- **Query Filter:** ${parts.join(", ")}`);
    }

    if (output.title) {
      lines.push(`- **Window/Page:** ${this.truncateValue(output.title)}`);
    }

    if (output.url) {
      lines.push(`- **URL:** ${output.url}`);
    }

    if (output.total_nodes !== undefined) {
      const interactive =
        output.interactive_nodes !== undefined ? ` (${output.interactive_nodes} interactive)` : "";
      lines.push(`- **Tree Size:** ${output.total_nodes} nodes${interactive}`);
    }

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Query matches
    if (output.action === "query" && output.matches) {
      lines.push("");
      lines.push(`**Query Matches:** ${output.matches.length}`);
      const maxRows = 15;
      const renderable = output.matches.slice(0, maxRows);
      if (renderable.length > 0) {
        lines.push("");
        lines.push("| Ref | Role | Name | Automation ID |");
        lines.push("|-----|------|------|---------------|");
        for (const match of renderable) {
          const node = match as RawAccessibilityNode;
          const ref = node.ref || "-";
          const role = node.role || "-";
          const name = this.truncateValue(node.name || node.value || "-");
          const automationId = node.automation_id || "-";
          lines.push(`| \`${ref}\` | ${role} | ${name} | ${automationId} |`);
        }
        if (output.matches.length > maxRows) {
          lines.push(`*... and ${output.matches.length - maxRows} more matches*`);
        }
      }
    }

    // Snapshot summary for capture / ai_context
    if ((output.action === "capture" || output.action === "ai_context") && output.snapshot) {
      lines.push("");
      lines.push("**Snapshot:**");
      lines.push("```json");
      lines.push(this.stringifyValue(output.snapshot, 4000));
      lines.push("```");
    }

    return lines.join("\n");
  }

  toTestConfig(output: NativeAccessibilityStepOutput): TestAutoPopulateConfig | null {
    // Without an action, there's no replayable intent to capture.
    if (!output.action) {
      return null;
    }

    const config: Record<string, unknown> = {
      action: output.action,
    };

    if (output.target) config.target = output.target;
    if (output.ref_id) config.ref_id = output.ref_id;
    if (output.text !== undefined && output.action === "type") config.text = output.text;
    if (output.query_role) config.query_role = output.query_role;
    if (output.query_label) config.query_label = output.query_label;
    if (output.backend) config.expected_backend = output.backend;

    // Summary stats are not used as replay input, but are useful as
    // expected-value hints for assertions.
    if (output.total_nodes !== undefined) {
      config.expected_total_nodes = output.total_nodes;
    }
    if (output.interactive_nodes !== undefined) {
      config.expected_interactive_nodes = output.interactive_nodes;
    }

    return {
      config_key: "native_accessibility_config",
      config_value: config,
      description: `Native accessibility config for action=${output.action}${output.target ? ` target=${output.target}` : ""}`,
    };
  }

  getAssertableFields(output: NativeAccessibilityStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    fields.push({
      path: "success",
      name: "Action Success",
      description: "Whether the accessibility action succeeded",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert accessibility action succeeded"],
    });

    fields.push({
      path: "action",
      name: "Action Kind",
      description: "Which accessibility action was performed",
      type: "string",
      example_value: output.action,
      suggested_assertions: ["Assert correct action was performed"],
    });

    if (output.backend) {
      fields.push({
        path: "backend",
        name: "Backend",
        description: "Accessibility backend that served the request (uia/atspi/ax/cdp/...)",
        type: "string",
        example_value: output.backend,
        suggested_assertions: ["Assert expected backend was used"],
      });
    }

    if (output.ref_id) {
      fields.push({
        path: "ref_id",
        name: "Element Ref",
        description: "Stable ref ID of the target element (e.g. `@e3`)",
        type: "string",
        example_value: output.ref_id,
        suggested_assertions: ["Assert target element ref was resolved"],
      });
    }

    if (output.total_nodes !== undefined) {
      fields.push({
        path: "total_nodes",
        name: "Total Nodes",
        description: "Total nodes in the captured accessibility tree",
        type: "number",
        example_value: output.total_nodes,
        suggested_assertions: [
          "Assert tree has expected size",
          "Assert tree size is above threshold",
        ],
      });
    }

    if (output.interactive_nodes !== undefined) {
      fields.push({
        path: "interactive_nodes",
        name: "Interactive Nodes",
        description: "Number of interactive nodes in the tree",
        type: "number",
        example_value: output.interactive_nodes,
        suggested_assertions: ["Assert interactive count is non-zero"],
      });
    }

    if (output.action === "query" && output.matches) {
      fields.push({
        path: "matches.length",
        name: "Query Match Count",
        description: "Number of elements matched by the query",
        type: "number",
        example_value: output.matches.length,
        suggested_assertions: [
          "Assert query returned expected number of elements",
          "Assert query returned at least one element",
          "Assert query returned exactly one element",
        ],
      });

      // Individual match refs for the first few
      const maxMatches = Math.min(output.matches.length, 3);
      for (let i = 0; i < maxMatches; i++) {
        const node = output.matches[i] as RawAccessibilityNode;
        if (node?.ref) {
          fields.push({
            path: `matches[${i}].ref`,
            name: `Match ${i + 1} Ref`,
            description: `Ref of match ${i + 1}: ${node.name || node.role || node.ref}`,
            type: "string",
            example_value: node.ref,
            suggested_assertions: [`Assert match ${i + 1} ref is stable`],
          });
        }
      }
    }

    if (output.title) {
      fields.push({
        path: "title",
        name: "Window/Page Title",
        description: "Title of the captured window or page",
        type: "string",
        example_value: output.title,
        suggested_assertions: ["Assert title matches expected"],
      });
    }

    if (output.url) {
      fields.push({
        path: "url",
        name: "Page URL",
        description: "URL of the captured page (web targets)",
        type: "string",
        example_value: output.url,
        suggested_assertions: ["Assert URL matches expected"],
      });
    }

    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Action Duration",
        description: "Time taken for the accessibility action in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert action completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    return fields;
  }

  /**
   * Derive a compact step name from the action + target or filter.
   */
  private deriveName(
    action: NativeAccessibilityAction,
    raw: RawNativeAccessibilityOutput,
    config: NativeAccessibilityStepConfig,
  ): string {
    const target = raw.target ?? config.target;
    const ref = raw.ref_id ?? config.ref_id;
    const label = raw.query_label ?? config.query_label;
    const role = raw.query_role ?? config.query_role;

    if (ref) return `${action} ${ref}`;
    if (action === "query" && (role || label)) {
      const parts: string[] = [];
      if (role) parts.push(role);
      if (label) parts.push(`"${label}"`);
      return `query ${parts.join(" ")}`;
    }
    if (target) return `${action} (${target})`;
    return action;
  }

  /**
   * Normalize an action string to the typed enum, or return undefined.
   */
  private normalizeAction(action: string | undefined): NativeAccessibilityAction | undefined {
    if (!action) return undefined;
    return KNOWN_ACTIONS.has(action as NativeAccessibilityAction)
      ? (action as NativeAccessibilityAction)
      : undefined;
  }

  /**
   * Stringify a value as JSON with shared 4000-char truncation.
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
   * Truncate an inline value for display in table cells / bullet lines.
   */
  private truncateValue(value: string): string {
    if (value.length <= 80) return value;
    return value.substring(0, 77) + "...";
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const nativeAccessibilityHandler = new NativeAccessibilityHandler();
