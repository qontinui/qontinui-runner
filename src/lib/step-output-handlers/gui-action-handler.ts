/**
 * GUI Action Step Output Handler
 *
 * Handles outputs from GUI action steps (click, type, scroll, etc.), providing:
 * - Parsing of action results with screenshots
 * - AI summary generation for verification
 * - Test config auto-population (target info)
 * - Assertable field extraction
 */

import {
  type GuiActionStepOutput,
  type StepOutputType,
  type ScreenBoundingBox,
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

interface RawGuiActionOutput {
  action_type?: string;
  target_location?: { x: number; y: number };
  target_bounds?: ScreenBoundingBox;
  target_label?: string;
  confidence?: number;
  typed_text?: string;
  key_pressed?: string;
  scroll_delta?: { x: number; y: number };
  screenshot_before?: string;
  screenshot_after?: string;
  monitor_index?: number;
  success?: boolean;
  error?: string;
}

interface GuiActionStepConfig {
  action?: string;
  type?: string; // Alternative field name
  target?: string;
  image_name?: string;
  text?: string;
  key?: string;
  scroll?: { x: number; y: number };
  monitor_index?: number;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class GuiActionHandler implements StepOutputHandler<GuiActionStepOutput> {
  readonly stepType: StepOutputType = "gui_action";
  readonly displayName = "GUI Action";
  readonly description = "Mouse/keyboard action with target location and screenshots";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): GuiActionStepOutput {
    const raw = (rawOutput || {}) as RawGuiActionOutput;
    const config = (stepConfig || {}) as GuiActionStepConfig;

    const actionType = this.normalizeActionType(
      raw.action_type || config.action || config.type || "click",
    );
    const targetLabel = raw.target_label || config.target || config.image_name;

    return {
      id: generateStepOutputId(),
      step_type: "gui_action",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || `${actionType} ${targetLabel || ""}`.trim(),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? raw.success ?? true,
      error: metadata?.error ?? raw.error,
      action_type: actionType,
      target_location: raw.target_location,
      target_bounds: raw.target_bounds,
      target_label: targetLabel,
      confidence: raw.confidence,
      typed_text: raw.typed_text || config.text,
      key_pressed: raw.key_pressed || config.key,
      scroll_delta: raw.scroll_delta || config.scroll,
      screenshot_before: raw.screenshot_before,
      screenshot_after: raw.screenshot_after,
      monitor_index: raw.monitor_index ?? config.monitor_index,
    };
  }

  summarizeForAI(output: GuiActionStepOutput): string {
    const lines: string[] = [];

    lines.push(`### GUI Action: ${output.step_name}`);
    lines.push(`- **Action Type:** ${output.action_type}`);

    if (output.target_label) {
      lines.push(`- **Target:** ${output.target_label}`);
    }

    if (output.target_location) {
      lines.push(`- **Location:** (${output.target_location.x}, ${output.target_location.y})`);
    }

    if (output.target_bounds) {
      lines.push(
        `- **Bounds:** (${output.target_bounds.x}, ${output.target_bounds.y}) ` +
          `${output.target_bounds.width}x${output.target_bounds.height}`,
      );
    }

    if (output.confidence !== undefined) {
      const confidencePercent = (output.confidence * 100).toFixed(1);
      lines.push(`- **Confidence:** ${confidencePercent}%`);
    }

    if (output.typed_text) {
      lines.push(`- **Typed Text:** "${output.typed_text}"`);
    }

    if (output.key_pressed) {
      lines.push(`- **Key Pressed:** ${output.key_pressed}`);
    }

    if (output.scroll_delta) {
      lines.push(`- **Scroll Delta:** (${output.scroll_delta.x}, ${output.scroll_delta.y})`);
    }

    if (output.monitor_index !== undefined) {
      lines.push(`- **Monitor:** ${output.monitor_index}`);
    }

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Success" : "Failed"}`);

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Note about screenshots
    if (output.screenshot_before || output.screenshot_after) {
      lines.push("");
      lines.push("**Screenshots Available:**");
      if (output.screenshot_before) lines.push("- Before action");
      if (output.screenshot_after) lines.push("- After action");
    }

    return lines.join("\n");
  }

  toTestConfig(output: GuiActionStepOutput): TestAutoPopulateConfig | null {
    // Provide target info for GUI-based tests
    if (!output.target_label && !output.target_bounds) {
      return null;
    }

    const config: Record<string, unknown> = {
      action_type: output.action_type,
    };

    if (output.target_label) {
      config.target_label = output.target_label;
    }

    if (output.target_bounds) {
      config.target_bounds = output.target_bounds;
    }

    if (output.target_location) {
      config.target_location = output.target_location;
    }

    if (output.monitor_index !== undefined) {
      config.monitor_index = output.monitor_index;
    }

    return {
      config_key: "gui_action_config",
      config_value: config,
      description: `GUI action config for ${output.action_type} on ${output.target_label || "target"}`,
    };
  }

  getAssertableFields(output: GuiActionStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Success status
    fields.push({
      path: "success",
      name: "Action Success",
      description: "Whether the GUI action completed successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert action succeeded", "Assert no error occurred"],
    });

    // Confidence
    if (output.confidence !== undefined) {
      fields.push({
        path: "confidence",
        name: "Recognition Confidence",
        description: "Confidence score for target recognition (0-1)",
        type: "number",
        example_value: output.confidence,
        suggested_assertions: [
          "Assert confidence is above threshold (e.g., > 0.8)",
          "Record confidence as metric",
        ],
      });
    }

    // Target location
    if (output.target_location) {
      fields.push({
        path: "target_location.x",
        name: "Target X Coordinate",
        description: "X coordinate of the action target",
        type: "number",
        example_value: output.target_location.x,
        suggested_assertions: ["Assert X is within expected range"],
      });
      fields.push({
        path: "target_location.y",
        name: "Target Y Coordinate",
        description: "Y coordinate of the action target",
        type: "number",
        example_value: output.target_location.y,
        suggested_assertions: ["Assert Y is within expected range"],
      });
    }

    // Target bounds
    if (output.target_bounds) {
      fields.push({
        path: "target_bounds",
        name: "Target Bounding Box",
        description: "Bounding box of the detected target element",
        type: "object",
        example_value: output.target_bounds,
        suggested_assertions: [
          "Assert bounds has positive dimensions",
          "Assert bounds is within screen",
        ],
      });
    }

    // Screenshots
    if (output.screenshot_after) {
      fields.push({
        path: "screenshot_after",
        name: "Screenshot After Action",
        description: "Screenshot captured after the action",
        type: "string",
        suggested_assertions: [
          "Visual comparison with expected state",
          "Check for expected elements in screenshot",
        ],
      });
    }

    return fields;
  }

  /**
   * Normalize action type string to valid enum value.
   */
  private normalizeActionType(
    action: string,
  ): GuiActionStepOutput["action_type"] {
    const normalized = action.toLowerCase().replace(/[_-]/g, "");
    switch (normalized) {
      case "click":
      case "leftclick":
        return "click";
      case "doubleclick":
      case "dblclick":
        return "double_click";
      case "rightclick":
        return "right_click";
      case "type":
      case "text":
      case "input":
        return "type";
      case "scroll":
        return "scroll";
      case "drag":
        return "drag";
      case "hover":
      case "move":
        return "hover";
      case "keypress":
      case "key":
        return "key_press";
      default:
        return "click";
    }
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const guiActionHandler = new GuiActionHandler();
