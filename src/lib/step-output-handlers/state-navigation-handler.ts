/**
 * State Navigation Step Output Handler
 *
 * Handles outputs from state navigation steps, providing:
 * - Parsing of navigation results with path taken
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 */

import {
  type StateNavigationStepOutput,
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

interface PathStep {
  from_state: string;
  to_state: string;
  transition_name: string;
  action_taken?: string;
}

interface RawStateNavigationOutput {
  target_state?: string;
  reached?: boolean;
  path_taken?: PathStep[];
  path?: PathStep[]; // Alternative field name
  final_state?: string;
  current_state?: string; // Alternative field name
  final_screenshot?: string;
  screenshot?: string; // Alternative field name
}

interface StateNavigationStepConfig {
  target_state?: string;
  max_steps?: number;
  timeout_ms?: number;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class StateNavigationHandler implements StepOutputHandler<StateNavigationStepOutput> {
  readonly stepType: StepOutputType = "state_navigation";
  readonly displayName = "State Navigation";
  readonly description = "Automated navigation to target state with path tracking";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): StateNavigationStepOutput {
    const raw = (rawOutput || {}) as RawStateNavigationOutput;
    const config = (stepConfig || {}) as StateNavigationStepConfig;

    const targetState = raw.target_state || config.target_state || "";
    const finalState = raw.final_state || raw.current_state;
    const reached = raw.reached ?? finalState === targetState;

    return {
      id: generateStepOutputId(),
      step_type: "state_navigation",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || `Navigate to ${targetState}`,
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? reached,
      error: metadata?.error,
      target_state: targetState,
      reached,
      path_taken: raw.path_taken || raw.path,
      final_state: finalState,
      final_screenshot: raw.final_screenshot || raw.screenshot,
    };
  }

  summarizeForAI(output: StateNavigationStepOutput): string {
    const lines: string[] = [];

    lines.push(`### State Navigation: ${output.step_name}`);
    lines.push(`- **Target State:** ${output.target_state}`);

    if (output.final_state) {
      lines.push(`- **Final State:** ${output.final_state}`);
    }

    const reachedEmoji = output.reached ? "✅" : "❌";
    lines.push(`- **Reached Target:** ${reachedEmoji} ${output.reached ? "Yes" : "No"}`);

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Path taken
    if (output.path_taken && output.path_taken.length > 0) {
      lines.push("");
      lines.push(`**Path Taken:** ${output.path_taken.length} transitions`);
      lines.push("");

      for (let i = 0; i < output.path_taken.length; i++) {
        const step = output.path_taken[i];
        const action = step.action_taken ? ` (${step.action_taken})` : "";
        lines.push(
          `${i + 1}. ${step.from_state} → ${step.to_state} via "${step.transition_name}"${action}`,
        );
      }
    } else if (output.reached) {
      lines.push("");
      lines.push("**Note:** Already at target state (no navigation needed)");
    }

    // Screenshot availability
    if (output.final_screenshot) {
      lines.push("");
      lines.push("**Final Screenshot:** Available");
    }

    return lines.join("\n");
  }

  toTestConfig(output: StateNavigationStepOutput): TestAutoPopulateConfig | null {
    if (!output.target_state) {
      return null;
    }

    const config: Record<string, unknown> = {
      target_state: output.target_state,
      expected_reached: output.reached,
    };

    if (output.path_taken) {
      config.expected_path_length = output.path_taken.length;
      config.path_states = output.path_taken.map((p) => p.to_state);
    }

    if (output.final_state) {
      config.final_state = output.final_state;
    }

    return {
      config_key: "state_navigation_config",
      config_value: config,
      description: `State navigation config for ${output.target_state}`,
    };
  }

  getAssertableFields(output: StateNavigationStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    // Reached target
    fields.push({
      path: "reached",
      name: "Target Reached",
      description: "Whether the target state was reached",
      type: "boolean",
      example_value: output.reached,
      suggested_assertions: ["Assert target state was reached", "Assert navigation succeeded"],
    });

    // Target state
    fields.push({
      path: "target_state",
      name: "Target State",
      description: "The state we were navigating to",
      type: "string",
      example_value: output.target_state,
      suggested_assertions: ["Verify target state is correct"],
    });

    // Final state
    if (output.final_state) {
      fields.push({
        path: "final_state",
        name: "Final State",
        description: "The state reached after navigation",
        type: "string",
        example_value: output.final_state,
        suggested_assertions: [
          "Assert final state equals target",
          "Assert ended in expected state",
        ],
      });
    }

    // Path length
    if (output.path_taken) {
      fields.push({
        path: "path_taken.length",
        name: "Path Length",
        description: "Number of transitions taken",
        type: "number",
        example_value: output.path_taken.length,
        suggested_assertions: [
          "Assert path length is optimal",
          "Assert path length under threshold",
        ],
      });

      // Individual path steps
      for (let i = 0; i < Math.min(output.path_taken.length, 5); i++) {
        const step = output.path_taken[i];
        fields.push({
          path: `path_taken[${i}].transition_name`,
          name: `Transition ${i + 1}`,
          description: `Transition from ${step.from_state} to ${step.to_state}`,
          type: "string",
          example_value: step.transition_name,
          suggested_assertions: [`Assert transition "${step.transition_name}" was used`],
        });
      }
    }

    // Duration
    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Navigation Duration",
        description: "Time taken to navigate in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert navigation completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    // Success
    fields.push({
      path: "success",
      name: "Navigation Success",
      description: "Whether navigation completed without errors",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert navigation succeeded"],
    });

    return fields;
  }
}

// ============================================================================
// Export singleton instance
// ============================================================================

export const stateNavigationHandler = new StateNavigationHandler();
