/**
 * Prompt Step Output Handler
 *
 * Handles outputs from prompt steps (AI task instructions), providing:
 * - Parsing of prompt responses (synchronous or background session)
 * - AI summary generation for verification
 * - Test config auto-population
 * - Assertable field extraction
 *
 * Input schema:  `qontinui-schemas/.../per_type/prompt_step.py` (`PromptStep`)
 * Response schema: `.../per_type/run_prompt_response.py` (`RunPromptResponse`)
 */

import {
  type PromptStepOutput,
  type PromptStepPhase,
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

/**
 * Raw shape accepted from the runner's prompt execution layer.
 * Matches the `RunPromptResponse` Python schema plus a few fields the
 * dispatcher echoes back (phase, provider, model) for downstream surfaces.
 */
interface RawPromptOutput {
  response?: string;
  response_text?: string;
  output?: string; // Immediate output text on sync runs
  output_text?: string;
  data?: { response?: string; output?: string } | null;
  session_id?: string;
  task_run_id?: string;
  log_file?: string;
  state_file?: string;
  pid?: number;
  phase?: string;
  provider?: string;
  model?: string;
  extractions?: Record<string, unknown>;
  criterion_ids?: string[];
  is_summary_step?: boolean;
}

/**
 * Subset of `PromptStep` that the handler reads from step config to
 * reconstruct what the model was asked to do.
 */
interface PromptStepConfig {
  content?: string;
  prompt?: string;
  prompt_id?: string;
  provider?: string;
  model?: string;
  phase?: string;
  criterion_ids?: string[];
  is_summary_step?: boolean;
}

const KNOWN_PHASES: ReadonlySet<PromptStepPhase> = new Set([
  "setup",
  "verification",
  "agentic",
  "completion",
]);

// ============================================================================
// Handler Implementation
// ============================================================================

export class PromptHandler implements StepOutputHandler<PromptStepOutput> {
  readonly stepType: StepOutputType = "prompt";
  readonly displayName = "Prompt";
  readonly description = "AI prompt execution with model response and optional background session";

  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): PromptStepOutput {
    const raw = (rawOutput || {}) as RawPromptOutput;
    const config = (stepConfig || {}) as PromptStepConfig;

    const responseText = raw.response_text ?? raw.response ?? raw.data?.response ?? undefined;
    const outputText = raw.output_text ?? raw.output ?? raw.data?.output ?? undefined;
    const prompt = config.content ?? config.prompt ?? "";

    return {
      id: generateStepOutputId(),
      step_type: "prompt",
      step_id: metadata?.step_id,
      step_name: metadata?.step_name || this.truncatePrompt(prompt) || "Prompt",
      executed_at: metadata?.started_at || new Date().toISOString(),
      duration_ms: metadata?.duration_ms ?? 0,
      success: metadata?.success ?? true,
      error: metadata?.error,
      prompt,
      prompt_id: config.prompt_id,
      provider: raw.provider ?? config.provider,
      model: raw.model ?? config.model,
      phase: this.normalizePhase(raw.phase ?? config.phase),
      response_text: responseText,
      output_text: outputText,
      session_id: raw.session_id,
      task_run_id: raw.task_run_id,
      log_file: raw.log_file,
      extractions: raw.extractions,
      criterion_ids: raw.criterion_ids ?? config.criterion_ids,
      is_summary_step: raw.is_summary_step ?? config.is_summary_step,
    };
  }

  summarizeForAI(output: PromptStepOutput): string {
    const lines: string[] = [];

    lines.push(`### Prompt: ${output.step_name}`);

    const statusEmoji = output.success ? "✅" : "❌";
    lines.push(`- **Status:** ${statusEmoji} ${output.success ? "Completed" : "Failed"}`);

    if (output.phase) {
      lines.push(`- **Phase:** ${output.phase}`);
    }

    if (output.provider || output.model) {
      const parts: string[] = [];
      if (output.provider) parts.push(`provider=\`${output.provider}\``);
      if (output.model) parts.push(`model=\`${output.model}\``);
      lines.push(`- **Runtime:** ${parts.join(", ")}`);
    }

    if (output.prompt_id) {
      lines.push(`- **Saved Prompt ID:** \`${output.prompt_id}\``);
    }

    if (output.session_id) {
      lines.push(`- **Session:** \`${output.session_id}\``);
    }

    if (output.task_run_id) {
      lines.push(`- **Task Run:** \`${output.task_run_id}\``);
    }

    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }

    if (output.is_summary_step) {
      lines.push(`- **Summary Step:** yes`);
    }

    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }

    // Prompt body
    if (output.prompt && output.prompt.trim()) {
      lines.push("");
      lines.push("**Prompt:**");
      lines.push("```");
      lines.push(this.truncateBlock(output.prompt, 4000));
      lines.push("```");
    }

    // Response text (final model output)
    if (output.response_text && output.response_text.trim()) {
      lines.push("");
      lines.push("**Response:**");
      lines.push("```");
      lines.push(this.truncateBlock(output.response_text, 4000));
      lines.push("```");
    }

    // Immediate output (synchronous runs)
    if (
      output.output_text &&
      output.output_text.trim() &&
      output.output_text !== output.response_text
    ) {
      lines.push("");
      lines.push("**Output:**");
      lines.push("```");
      lines.push(this.truncateBlock(output.output_text, 4000));
      lines.push("```");
    }

    // Extractions
    if (output.extractions && Object.keys(output.extractions).length > 0) {
      lines.push("");
      lines.push("**Extracted Variables:**");
      for (const [key, value] of Object.entries(output.extractions)) {
        const valueStr = typeof value === "object" ? JSON.stringify(value) : String(value);
        lines.push(`- \`${key}\`: ${this.truncateValue(valueStr)}`);
      }
    }

    // Criterion IDs verified by this step
    if (output.criterion_ids && output.criterion_ids.length > 0) {
      lines.push("");
      lines.push(`**Verifies Criteria:** ${output.criterion_ids.join(", ")}`);
    }

    return lines.join("\n");
  }

  toTestConfig(output: PromptStepOutput): TestAutoPopulateConfig | null {
    if (!output.prompt && !output.prompt_id) {
      // Nothing actionable to replay in a test.
      return null;
    }

    const config: Record<string, unknown> = {};

    if (output.prompt) config.prompt = output.prompt;
    if (output.prompt_id) config.prompt_id = output.prompt_id;
    if (output.provider) config.provider = output.provider;
    if (output.model) config.model = output.model;
    if (output.phase) config.phase = output.phase;
    if (output.criterion_ids && output.criterion_ids.length > 0) {
      config.criterion_ids = output.criterion_ids;
    }
    if (output.is_summary_step !== undefined) {
      config.is_summary_step = output.is_summary_step;
    }

    return {
      config_key: "prompt_config",
      config_value: config,
      description: `Prompt config for: ${this.truncatePrompt(output.prompt || output.prompt_id || "")}`,
    };
  }

  getAssertableFields(output: PromptStepOutput): AssertableField[] {
    const fields: AssertableField[] = [];

    fields.push({
      path: "success",
      name: "Prompt Success",
      description: "Whether the prompt completed without error",
      type: "boolean",
      example_value: output.success,
      suggested_assertions: ["Assert prompt succeeded"],
    });

    if (output.response_text !== undefined) {
      fields.push({
        path: "response_text",
        name: "Response Text",
        description: "Final response text from the model",
        type: "string",
        example_value: this.truncateValue(output.response_text),
        suggested_assertions: [
          "Assert response contains expected substring",
          "Assert response matches pattern",
          "Parse response as JSON and validate",
        ],
      });
    }

    if (output.output_text !== undefined && output.output_text !== output.response_text) {
      fields.push({
        path: "output_text",
        name: "Output Text",
        description: "Immediate / streaming output from a synchronous run",
        type: "string",
        example_value: this.truncateValue(output.output_text),
        suggested_assertions: ["Assert output contains expected substring"],
      });
    }

    if (output.session_id) {
      fields.push({
        path: "session_id",
        name: "Session ID",
        description: "ID of the background AI session, if one was spawned",
        type: "string",
        example_value: output.session_id,
        suggested_assertions: ["Assert a session was created"],
      });
    }

    if (output.task_run_id) {
      fields.push({
        path: "task_run_id",
        name: "Task Run ID",
        description: "ID of the created task run",
        type: "string",
        example_value: output.task_run_id,
        suggested_assertions: ["Assert a task run was created"],
      });
    }

    if (output.duration_ms !== undefined) {
      fields.push({
        path: "duration_ms",
        name: "Prompt Duration",
        description: "Time taken for the prompt in milliseconds",
        type: "number",
        example_value: output.duration_ms,
        suggested_assertions: [
          "Assert prompt completes within time limit",
          "Record as performance metric",
        ],
      });
    }

    if (output.extractions) {
      for (const [key, value] of Object.entries(output.extractions)) {
        fields.push({
          path: `extractions.${key}`,
          name: `Extracted: ${key}`,
          description: `Variable extracted from the prompt response: ${key}`,
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
   * Truncate a prompt body for display as a step name / description.
   */
  private truncatePrompt(prompt: string): string {
    const firstLine = prompt.split(/\r?\n/)[0] ?? "";
    if (firstLine.length <= 50) return firstLine;
    return firstLine.substring(0, 47) + "...";
  }

  /**
   * Truncate a code-block body using the shared 4000-char budget.
   */
  private truncateBlock(value: string, limit: number): string {
    if (value.length <= limit) return value;
    return value.substring(0, limit) + "\n... (truncated)";
  }

  /**
   * Truncate an inline value for display in a bullet line.
   */
  private truncateValue(value: string): string {
    if (value.length <= 200) return value;
    return value.substring(0, 197) + "...";
  }

  /**
   * Normalize a phase string into the `PromptStepPhase` union or drop it.
   */
  private normalizePhase(phase: string | undefined): PromptStepPhase | undefined {
    if (!phase) return undefined;
    return KNOWN_PHASES.has(phase as PromptStepPhase) ? (phase as PromptStepPhase) : undefined;
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

export const promptHandler = new PromptHandler();
