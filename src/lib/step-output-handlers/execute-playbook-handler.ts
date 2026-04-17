/**
 * Execute Playbook Step Output Handler
 *
 * Handles outputs from `execute_playbook` steps (drives a state machine
 * through recorded transitions from a playbook), providing:
 * - Parsing of per-transition success/failure records
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 *
 * Input schema: `qontinui-schemas/.../per_type/execute_playbook_step.py`
 * (`ExecutePlaybookStep`, wire tag `"execute_playbook"`).
 */

import {
  type ExecutePlaybookStepOutput,
  type PlaybookTransitionRecord,
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

interface RawPlaybookTransition {
  transition_id?: string;
  transition_name?: string;
  success?: boolean;
  passed?: boolean; // Alternative field name
  duration_ms?: number;
  error?: string;
}

interface RawExecutePlaybookOutput {
  transitions?: RawPlaybookTransition[];
  steps?: RawPlaybookTransition[]; // Alternative field name
  transitions_succeeded?: number;
  transitions_failed?: number;
  final_states?: string[];
  completed?: boolean;
}

interface ExecutePlaybookStepConfig {
  playbook_path?: string;
  content?: string;
  timeout_seconds?: number;
}

// ============================================================================
// Handler Implementation
// ============================================================================

export class ExecutePlaybookHandler implements StepOutputHandler<ExecutePlaybookStepOutput> {
  readonly stepType: StepOutputType = "execute_playbook";
  readonly displayName = "Execute Playbook";
  readonly description =
    "Replays a state-machine playbook through recorded transitions with per-step results";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): ExecutePlaybookStepOutput {
    const raw = (rawOutput || {}) as RawExecutePlaybookOutput;
    const config = (stepConfig || {}) as ExecutePlaybookStepConfig;

    const rawTransitions = raw.transitions ?? raw.steps ?? [];
    const transitions: PlaybookTransitionRecord[] = rawTransitions.map((t) => {
      const success = t.success ?? t.passed ?? !t.error;
      return {
        transition_id: t.transition_id ?? "",
        transition_name: t.transition_name,
        success,
        duration_ms: t.duration_ms,
        error: t.error,
      };
    });

    const succeeded = raw.transitions_succeeded ?? transitions.filter((t) => t.success).length;
    const failed = raw.transitions_failed ?? transitions.filter((t) => !t.success).length;

    const completed = raw.completed ?? (transitions.length > 0 && failed === 0);

    return {
      id: generateStepOutputId(),
      step_type: "execute_playbook",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.deriveName(config),
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? (completed && failed === 0),
      error: metadata?.error,
      playbook_path: config.playbook_path,
      content: config.content,
      transitions,
      transitions_succeeded: succeeded,
      transitions_failed: failed,
      final_states: raw.final_states,
      completed,
    };
  }

  summarizeForAI(output: ExecutePlaybookStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Execute Playbook: ${output.step_name}`);

    const statusEmoji = output.success ? "✅" : "❌";
    const statusLabel = output.completed
      ? output.success
        ? "Completed"
        : "Completed with Failures"
      : "Did Not Complete";
    lines.push(`- **Status:** ${statusEmoji} ${statusLabel}`);

    if (output.playbook_path) {
      lines.push(`- **Playbook:** ${output.playbook_path}`);
    } else if (output.content) {
      const lineCount = output.content.split(/\r?\n/).length;
      lines.push(`- **Playbook:** inline (${lineCount} lines)`);
    }

    const total = output.transitions.length;
    lines.push(
      `- **Transitions:** ${output.transitions_succeeded}/${total} succeeded` +
        (output.transitions_failed > 0 ? `, ${output.transitions_failed} failed` : ""),
    );

    if (output.final_states && output.final_states.length > 0) {
      lines.push(`- **Final States:** ${output.final_states.join(", ")}`);
    }

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Per-transition record
    if (output.transitions.length > 0) {
      lines.push("");
      lines.push("**Transition Log:**");
      lines.push("");
      lines.push("| # | Transition | Result | Duration | Error |");
      lines.push("|---|------------|--------|----------|-------|");
      const maxRows = 30;
      const renderable = output.transitions.slice(0, maxRows);
      for (let i = 0; i < renderable.length; i++) {
        const t = renderable[i];
        const name = t.transition_name || t.transition_id || "-";
        const result = t.success ? "✅" : "❌";
        const dur = t.duration_ms !== undefined ? `${t.duration_ms}ms` : "-";
        const err = t.error ? this.truncateValue(t.error) : "-";
        lines.push(`| ${i + 1} | ${this.truncateValue(name)} | ${result} | ${dur} | ${err} |`);
      }
      if (output.transitions.length > maxRows) {
        lines.push(`*... and ${output.transitions.length - maxRows} more transitions*`);
      }
    }

    // Inline playbook body (only when no separate file)
    if (output.content && !output.playbook_path) {
      lines.push("");
      lines.push("**Playbook Content:**");
      lines.push("```markdown");
      lines.push(this.truncateBlock(output.content, 4000));
      lines.push("```");
    }

    // If the block overflows the shared budget, truncate.
    const rendered = lines.join("\n");
    if (rendered.length > 4000) {
      return rendered.substring(0, 4000) + "\n... (truncated)";
    }
    return rendered;
  }

  toTestConfig(output: ExecutePlaybookStepOutput): TestAutoPopulateConfig | null {
    if (!output.playbook_path && !output.content) {
      // Without a playbook reference or body, there's nothing for the
      // test to replay.
      return null;
    }

    const config: Record<string, unknown> = {
      expected_transitions_total: output.transitions.length,
      expected_transitions_succeeded: output.transitions_succeeded,
      expected_transitions_failed: output.transitions_failed,
      expected_completed: output.completed,
    };

    if (output.playbook_path) config.playbook_path = output.playbook_path;
    if (output.content) config.content = output.content;
    if (output.final_states && output.final_states.length > 0) {
      config.expected_final_states = output.final_states;
    }
    if (output.transitions.length > 0) {
      config.expected_transition_ids = output.transitions.map((t) => t.transition_id);
    }

    return {
      config_key: "execute_playbook_config",
      config_value: config,
      description: `Playbook config for ${output.playbook_path || "inline playbook"} (${output.transitions.length} transitions)`,
    };
  }

  getAssertableFields(output: ExecutePlaybookStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    fields.push({
      path: "success",
      name: "Playbook Success",
      description: "Whether the playbook ran end-to-end without failures",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert playbook succeeded"],
    });

    fields.push({
      path: "completed",
      name: "Playbook Completed",
      description: "Whether the playbook ran to completion",
      type: "boolean",
      example_value: output.completed,
      suggested_assertions: ["Assert playbook ran to completion"],
    });

    fields.push({
      path: "transitions.length",
      name: "Transition Count",
      description: "Total transitions attempted",
      type: "number",
      example_value: output.transitions.length,
      suggested_assertions: ["Assert transition count equals expected"],
    });

    fields.push({
      path: "transitions_succeeded",
      name: "Transitions Succeeded",
      description: "Number of transitions that succeeded",
      type: "number",
      example_value: output.transitions_succeeded,
      suggested_assertions: [
        "Assert expected number of transitions succeeded",
        "Assert all transitions succeeded",
      ],
    });

    fields.push({
      path: "transitions_failed",
      name: "Transitions Failed",
      description: "Number of transitions that failed",
      type: "number",
      example_value: output.transitions_failed,
      suggested_assertions: [
        "Assert no transitions failed",
        "Assert failed count is within budget",
      ],
    });

    if (output.final_states) {
      fields.push({
        path: "final_states",
        name: "Final States",
        description: "State machine state(s) at the end of the run",
        type: "array",
        example_value: output.final_states,
        suggested_assertions: ["Assert final states match expected"],
      });
    }

    // Individual failed transitions first (most useful for debugging)
    const failedIdx: number[] = [];
    for (let i = 0; i < output.transitions.length; i++) {
      if (!output.transitions[i].success) failedIdx.push(i);
      if (failedIdx.length >= 3) break;
    }
    for (const i of failedIdx) {
      const t = output.transitions[i];
      fields.push({
        path: `transitions[${i}].error`,
        name: `Failed Transition ${i + 1}`,
        description: `Error for transition ${t.transition_name || t.transition_id}`,
        type: "string",
        example_value: t.error,
        suggested_assertions: ["Assert this transition did not fail"],
      });
    }

    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Playbook Duration",
        description: "Time taken to run the whole playbook in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert playbook completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    return fields;
  }

  /**
   * Derive a compact step name from the playbook source.
   */
  private deriveName(config: ExecutePlaybookStepConfig): string {
    if (config.playbook_path) {
      const parts = config.playbook_path.split(/[\\/]/);
      return parts[parts.length - 1] || config.playbook_path;
    }
    if (config.content) {
      const firstLine = config.content.split(/\r?\n/).find((l) => l.trim().length > 0) ?? "";
      if (firstLine.length > 0)
        return firstLine.length <= 50 ? firstLine : firstLine.substring(0, 47) + "...";
    }
    return "execute_playbook";
  }

  /**
   * Truncate a code-block body using the shared 4000-char budget.
   */
  private truncateBlock(value: string, limit: number): string {
    if (value.length <= limit) return value;
    return value.substring(0, limit) + "\n... (truncated)";
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

export const executePlaybookHandler = new ExecutePlaybookHandler();
