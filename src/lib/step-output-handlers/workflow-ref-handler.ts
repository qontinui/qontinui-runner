/**
 * Workflow Reference Step Output Handler
 *
 * Handles outputs from workflow reference steps (nested workflow execution), providing:
 * - Parsing of nested workflow results
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type WorkflowRefStepOutput,
  type StepOutputType,
  type StepOutput,
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

interface RawWorkflowRefOutput {
  workflow_id?: string;
  workflow_name?: string;
  nested_outputs?: StepOutput[];
  outputs?: StepOutput[]; // Alternative field name
  final_state?: string;
  output_variables?: Record<string, unknown>;
  variables?: Record<string, unknown>; // Alternative field name
}

interface WorkflowRefStepConfig {
  workflow_id?: string;
  workflow_name?: string;
  input_variables?: Record<string, unknown>;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class WorkflowRefHandler implements StepOutputHandler<WorkflowRefStepOutput> {
  readonly stepType: StepOutputType = "workflow_ref";
  readonly displayName = "Workflow Reference";
  readonly description = "Nested workflow execution with collected outputs";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): WorkflowRefStepOutput {
    const raw = (rawOutput || {}) as RawWorkflowRefOutput;
    const config = (stepConfig || {}) as WorkflowRefStepConfig;

    return {
      id: generateStepOutputId(),
      step_type: "workflow_ref",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || raw.workflow_name || config.workflow_name || "Workflow",
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? true,
      error: metadata?.error,
      workflow_id: raw.workflow_id || config.workflow_id || "",
      workflow_name: raw.workflow_name || config.workflow_name || "Unknown Workflow",
      nested_outputs: raw.nested_outputs || raw.outputs,
      final_state: raw.final_state,
      output_variables: raw.output_variables || raw.variables,
    };
  }

  summarizeForAI(output: WorkflowRefStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Workflow Reference: ${output.step_name}`);
    lines.push(`- **Workflow ID:** ${output.workflow_id}`);
    lines.push(`- **Workflow Name:** ${output.workflow_name}`);

    if (output.final_state) {
      lines.push(`- **Final State:** ${output.final_state}`);
    }

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Completed" : "Failed"}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Nested outputs summary
    if (output.nested_outputs && output.nested_outputs.length > 0) {
      lines.push("");
      lines.push(`**Nested Steps:** ${output.nested_outputs.length}`);

      // Count by type
      const byType = new Map<string, number>();
      for (const nested of output.nested_outputs) {
        const count = byType.get(nested.step_type) || 0;
        byType.set(nested.step_type, count + 1);
      }

      for (const [type, count] of byType) {
        lines.push(`- ${count} ${type} steps`);
      }

      // Show first few nested outputs
      lines.push("");
      lines.push("**Step Sequence:**");
      for (let i = 0; i < Math.min(output.nested_outputs.length, 5); i++) {
        const nested = output.nested_outputs[i];
        const status = nested.success ? "✓" : "✗";
        lines.push(`${i + 1}. [${status}] ${nested.step_name} (${nested.step_type})`);
      }
      if (output.nested_outputs.length > 5) {
        lines.push(`   ... and ${output.nested_outputs.length - 5} more steps`);
      }
    }

    // Output variables
    if (output.output_variables && Object.keys(output.output_variables).length > 0) {
      lines.push("");
      lines.push("**Output Variables:**");
      for (const [key, value] of Object.entries(output.output_variables)) {
        const valueStr = typeof value === "object" ? JSON.stringify(value) : String(value);
        lines.push(`- \`${key}\`: ${valueStr.substring(0, 100)}`);
      }
    }

    return lines.join("\n");
  }

  toTestConfig(output: WorkflowRefStepOutput): TestAutoPopulateConfig | null {
    if (!output.workflow_id) {
      return null;
    }

    const config: Record<string, unknown> = {
      workflow_id: output.workflow_id,
      workflow_name: output.workflow_name,
    };

    if (output.final_state) {
      config.expected_final_state = output.final_state;
    }

    if (output.nested_outputs) {
      config.expected_step_count = output.nested_outputs.length;
    }

    if (output.output_variables) {
      config.output_variables = output.output_variables;
    }

    return {
      config_key: "workflow_ref_config",
      config_value: config,
      description: `Workflow reference config for ${output.workflow_name}`,
    };
  }

  getAssertableFields(output: WorkflowRefStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Success status
    fields.push({
      path: "success",
      name: "Workflow Success",
      description: "Whether the nested workflow completed successfully",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert workflow completed successfully"],
    });

    // Final state
    if (output.final_state) {
      fields.push({
        path: "final_state",
        name: "Final State",
        description: "The state reached after workflow execution",
        type: "string",
        example_value: output.final_state,
        suggested_assertions: ["Assert workflow ended in expected state"],
      });
    }

    // Nested output count
    if (output.nested_outputs) {
      fields.push({
        path: "nested_outputs.length",
        name: "Step Count",
        description: "Number of steps executed in the nested workflow",
        type: "number",
        example_value: output.nested_outputs.length,
        suggested_assertions: ["Assert expected number of steps executed"],
      });

      // Count successful nested steps
      const successCount = output.nested_outputs.filter((o) => o.success).length;
      fields.push({
        path: "nested_outputs.success_count",
        name: "Successful Steps",
        description: "Number of steps that completed successfully",
        type: "number",
        example_value: successCount,
        suggested_assertions: ["Assert all steps succeeded"],
      });
    }

    // Output variables
    if (output.output_variables) {
      for (const [key, value] of Object.entries(output.output_variables)) {
        fields.push({
          path: `output_variables.${key}`,
          name: `Variable: ${key}`,
          description: `Output variable from workflow: ${key}`,
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
        name: "Workflow Duration",
        description: "Total execution time in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert workflow completes within time limit",
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

export const workflowRefHandler = new WorkflowRefHandler();
