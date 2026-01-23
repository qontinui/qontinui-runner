/**
 * Screenshot Step Output Handler
 *
 * Handles outputs from screenshot/capture steps, providing:
 * - Parsing of screenshot data with detected elements
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type ScreenshotStepOutput,
  type StepOutputType,
  type DetectedScreenElement,
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

interface RawScreenshotOutput {
  screenshot_base64?: string;
  annotated_screenshot_base64?: string;
  dimensions?: { width: number; height: number };
  width?: number;
  height?: number;
  monitor_index?: number;
  detected_elements?: DetectedScreenElement[];
  elements?: DetectedScreenElement[]; // Alternative field name
  analysis_source?: "vision" | "ocr" | "segmentation";
}

interface ScreenshotStepConfig {
  monitor_index?: number;
  capture_type?: string;
  include_analysis?: boolean;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class ScreenshotHandler implements StepOutputHandler<ScreenshotStepOutput> {
  readonly stepType: StepOutputType = "screenshot";
  readonly displayName = "Screenshot";
  readonly description = "Screen capture with optional element detection";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): ScreenshotStepOutput {
    const raw = (rawOutput || {}) as RawScreenshotOutput;
    const config = (stepConfig || {}) as ScreenshotStepConfig;

    const dimensions = raw.dimensions || {
      width: raw.width || 0,
      height: raw.height || 0,
    };

    return {
      id: generateStepOutputId(),
      step_type: "screenshot",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || "Screenshot Capture",
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? !!raw.screenshot_base64,
      error: metadata?.error,
      screenshot_base64: raw.screenshot_base64 || "",
      annotated_screenshot_base64: raw.annotated_screenshot_base64,
      dimensions,
      monitor_index: raw.monitor_index ?? config.monitor_index,
      detected_elements: raw.detected_elements || raw.elements,
      analysis_source: raw.analysis_source,
    };
  }

  summarizeForAI(output: ScreenshotStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Screenshot: ${output.step_name}`);
    lines.push(`- **Dimensions:** ${output.dimensions.width}x${output.dimensions.height}`);

    if (output.monitor_index !== undefined) {
      lines.push(`- **Monitor:** ${output.monitor_index}`);
    }

    if (output.analysis_source) {
      lines.push(`- **Analysis Source:** ${output.analysis_source}`);
    }

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Captured" : "Failed"}`);

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Detected elements summary
    if (output.detected_elements && output.detected_elements.length > 0) {
      lines.push("");
      lines.push(`**Detected Elements:** ${output.detected_elements.length}`);
      lines.push("");
      lines.push("| # | Label | Type | Confidence | Bounds |");
      lines.push("|---|-------|------|------------|--------|");

      for (let i = 0; i < Math.min(output.detected_elements.length, 15); i++) {
        const el = output.detected_elements[i];
        const confidence = (el.confidence * 100).toFixed(0);
        const bounds = `(${el.bounding_box.x}, ${el.bounding_box.y}) ${el.bounding_box.width}x${el.bounding_box.height}`;
        lines.push(`| ${i + 1} | ${el.label} | ${el.element_type} | ${confidence}% | ${bounds} |`);
      }

      if (output.detected_elements.length > 15) {
        lines.push(`*... and ${output.detected_elements.length - 15} more elements*`);
      }
    }

    // Note about screenshot availability
    if (output.screenshot_base64) {
      lines.push("");
      lines.push("**Screenshot:** Available (base64 encoded)");
    }
    if (output.annotated_screenshot_base64) {
      lines.push("**Annotated Screenshot:** Available (with element overlays)");
    }

    return lines.join("\n");
  }

  toTestConfig(output: ScreenshotStepOutput): TestAutoPopulateConfig | null {
    if (!output.screenshot_base64 && !output.detected_elements?.length) {
      return null;
    }

    const config: Record<string, unknown> = {
      dimensions: output.dimensions,
    };

    if (output.monitor_index !== undefined) {
      config.monitor_index = output.monitor_index;
    }

    if (output.detected_elements && output.detected_elements.length > 0) {
      config.expected_element_count = output.detected_elements.length;
      config.element_labels = output.detected_elements.map((e) => e.label);
    }

    return {
      config_key: "screenshot_config",
      config_value: config,
      description: `Screenshot config with ${output.detected_elements?.length || 0} detected elements`,
    };
  }

  getAssertableFields(output: ScreenshotStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Success status
    fields.push({
      path: "success",
      name: "Capture Success",
      description: "Whether the screenshot was captured successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert screenshot captured successfully"],
    });

    // Dimensions
    fields.push({
      path: "dimensions.width",
      name: "Screenshot Width",
      description: "Width of the captured screenshot in pixels",
      type: "number",
      example_value: output.dimensions.width,
      suggested_assertions: ["Assert width matches expected resolution"],
    });

    fields.push({
      path: "dimensions.height",
      name: "Screenshot Height",
      description: "Height of the captured screenshot in pixels",
      type: "number",
      example_value: output.dimensions.height,
      suggested_assertions: ["Assert height matches expected resolution"],
    });

    // Element count
    if (output.detected_elements) {
      fields.push({
        path: "detected_elements.length",
        name: "Element Count",
        description: "Number of elements detected in the screenshot",
        type: "number",
        example_value: output.detected_elements.length,
        suggested_assertions: [
          "Assert expected number of elements detected",
          "Assert at least N elements found",
        ],
      });

      // Individual elements (limited)
      for (let i = 0; i < Math.min(output.detected_elements.length, 5); i++) {
        const el = output.detected_elements[i];
        fields.push({
          path: `detected_elements[${i}].label`,
          name: `Element ${i + 1} Label`,
          description: `Label of detected element: ${el.label}`,
          type: "string",
          example_value: el.label,
          suggested_assertions: [`Assert element "${el.label}" is present`],
        });

        fields.push({
          path: `detected_elements[${i}].confidence`,
          name: `Element ${i + 1} Confidence`,
          description: `Detection confidence for ${el.label}`,
          type: "number",
          example_value: el.confidence,
          suggested_assertions: ["Assert confidence above threshold"],
        });
      }
    }

    return fields;
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const screenshotHandler = new ScreenshotHandler();
