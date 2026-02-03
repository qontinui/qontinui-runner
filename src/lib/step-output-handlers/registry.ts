/**
 * Step Output Handler Registry
 *
 * A singleton registry that manages step output handlers.
 * Handlers can be registered for different step types and retrieved
 * for processing step outputs.
 */

import type { StepOutput, StepOutputType } from "../../types/step-output";
import type { StepOutputHandler, TestAutoPopulateConfig, AssertableField } from "./types";

// ============================================================================
// Registry Implementation
// ============================================================================

/**
 * Registry for step output handlers.
 * Provides type-safe handler registration and retrieval.
 */
class StepOutputHandlerRegistry {
  private handlers: Map<StepOutputType, StepOutputHandler> = new Map();
  private initialized = false;

  /**
   * Register a handler for a step type.
   * Throws if a handler is already registered for the type.
   */
  register<T extends StepOutput>(handler: StepOutputHandler<T>): void {
    if (this.handlers.has(handler.stepType)) {
      console.warn(`Handler already registered for step type: ${handler.stepType}. Overwriting.`);
    }
    this.handlers.set(handler.stepType, handler as StepOutputHandler);
  }

  /**
   * Get a handler for a step type.
   * Returns undefined if no handler is registered.
   */
  get<T extends StepOutput>(stepType: StepOutputType): StepOutputHandler<T> | undefined {
    return this.handlers.get(stepType) as StepOutputHandler<T> | undefined;
  }

  /**
   * Check if a handler exists for a step type.
   */
  has(stepType: StepOutputType): boolean {
    return this.handlers.has(stepType);
  }

  /**
   * Get all registered handlers.
   */
  getAll(): StepOutputHandler[] {
    return Array.from(this.handlers.values());
  }

  /**
   * Get all registered step types.
   */
  getRegisteredTypes(): StepOutputType[] {
    return Array.from(this.handlers.keys());
  }

  /**
   * Process a step output to generate test config.
   * Returns null if no handler exists or handler returns null.
   */
  toTestConfig(output: StepOutput): TestAutoPopulateConfig | null {
    const handler = this.get(output.step_type);
    if (!handler) return null;
    return handler.toTestConfig(output);
  }

  /**
   * Process a step output to generate AI summary.
   * Returns a generic summary if no handler exists.
   */
  summarizeForAI(output: StepOutput): string {
    const handler = this.get(output.step_type);
    if (!handler) {
      return this.genericSummary(output);
    }
    return handler.summarizeForAI(output);
  }

  /**
   * Get assertable fields for a step output.
   * Returns empty array if no handler exists.
   */
  getAssertableFields(output: StepOutput): AssertableField[] {
    const handler = this.get(output.step_type);
    if (!handler) return [];
    return handler.getAssertableFields(output);
  }

  /**
   * Generate a generic summary for outputs without handlers.
   */
  private genericSummary(output: StepOutput): string {
    const lines: string[] = [];
    lines.push(`### ${output.step_name || "Step Output"}`);
    lines.push(`- **Type:** ${output.step_type}`);
    lines.push(`- **Success:** ${output.success ? "Yes" : "No"}`);
    if (output.duration_ms !== undefined) {
      lines.push(`- **Duration:** ${output.duration_ms}ms`);
    }
    if (output.error) {
      lines.push(`- **Error:** ${output.error}`);
    }
    return lines.join("\n");
  }

  /**
   * Mark the registry as initialized.
   * Called after all handlers are registered.
   */
  markInitialized(): void {
    this.initialized = true;
  }

  /**
   * Check if the registry has been initialized.
   */
  isInitialized(): boolean {
    return this.initialized;
  }

  /**
   * Clear all handlers (useful for testing).
   */
  clear(): void {
    this.handlers.clear();
    this.initialized = false;
  }
}

// ============================================================================
// Singleton Export
// ============================================================================

/**
 * Global step output handler registry singleton.
 */
export const stepOutputRegistry = new StepOutputHandlerRegistry();

// ============================================================================
// Utility Functions
// ============================================================================

/**
 * Process multiple step outputs and collect test configs.
 * Useful when auto-populating test config from collected analyses.
 */
export function collectTestConfigs(outputs: StepOutput[]): TestAutoPopulateConfig[] {
  const configs: TestAutoPopulateConfig[] = [];
  for (const output of outputs) {
    const config = stepOutputRegistry.toTestConfig(output);
    if (config) {
      configs.push(config);
    }
  }
  return configs;
}

/**
 * Generate combined AI summary for multiple step outputs.
 */
export function summarizeOutputsForAI(outputs: StepOutput[]): string {
  if (outputs.length === 0) return "";

  const lines: string[] = [];
  lines.push("## Step Outputs (Ground Truth)\n");
  lines.push(`**${outputs.length} step outputs collected:**\n`);

  // Group by type
  const byType = new Map<StepOutputType, StepOutput[]>();
  for (const output of outputs) {
    const existing = byType.get(output.step_type) || [];
    existing.push(output);
    byType.set(output.step_type, existing);
  }

  // Summary of types
  for (const [type, typeOutputs] of byType) {
    const handler = stepOutputRegistry.get(type);
    const displayName = handler?.displayName || type;
    lines.push(`- ${typeOutputs.length} ${displayName} outputs`);
  }
  lines.push("");

  // Detailed summaries
  for (const output of outputs) {
    lines.push(stepOutputRegistry.summarizeForAI(output));
    lines.push("");
  }

  return lines.join("\n");
}

/**
 * Get all assertable fields from multiple outputs.
 */
export function collectAssertableFields(outputs: StepOutput[]): AssertableField[] {
  const fields: AssertableField[] = [];
  for (const output of outputs) {
    const outputFields = stepOutputRegistry.getAssertableFields(output);
    // Prefix field paths with output ID for uniqueness
    for (const field of outputFields) {
      fields.push({
        ...field,
        path: `${output.id}.${field.path}`,
        name: `[${output.step_name}] ${field.name}`,
      });
    }
  }
  return fields;
}
