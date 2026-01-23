/**
 * Step Output Handler Types
 *
 * Defines the interface for step output handlers and related configuration types.
 */

import type { StepOutput, StepOutputType } from "../../types/step-output";

// ============================================================================
// Test Auto-Population Configuration
// ============================================================================

/**
 * Configuration for auto-populating test config from step output.
 * Used when saving a test draft to automatically include relevant data.
 */
export interface TestAutoPopulateConfig {
  /** The key in the test config object where this data should be stored */
  config_key: string;
  /** The value to store (typically structured data from the step output) */
  config_value: unknown;
  /** Human-readable description of what was auto-populated */
  description: string;
}

// ============================================================================
// Assertable Fields
// ============================================================================

/**
 * A field from a step output that can be used in assertions.
 * Helps the AI generator understand what can be verified.
 */
export interface AssertableField {
  /** JSONPath or dot-notation path to the field */
  path: string;
  /** Human-readable name for the field */
  name: string;
  /** Description of what the field represents */
  description: string;
  /** Expected data type */
  type: "string" | "number" | "boolean" | "array" | "object" | "null" | "any";
  /** Example value (for AI context) */
  example_value?: unknown;
  /** Suggested assertions for this field */
  suggested_assertions?: string[];
}

// ============================================================================
// Step Output Handler Interface
// ============================================================================

/**
 * Handler for a specific step output type.
 *
 * Each handler knows how to:
 * - Parse raw output into a typed StepOutput
 * - Summarize the output for AI prompts
 * - Convert output to test configuration
 * - List assertable fields for verification
 */
export interface StepOutputHandler<T extends StepOutput = StepOutput> {
  /** The step type this handler processes */
  readonly stepType: StepOutputType;

  /** Human-readable name for this step type */
  readonly displayName: string;

  /** Short description of what this step type produces */
  readonly description: string;

  /**
   * Parse raw output data into a typed StepOutput.
   *
   * @param rawOutput - The raw output from step execution
   * @param stepConfig - The step configuration (if available)
   * @param metadata - Additional metadata (step_id, step_name, etc.)
   * @returns Typed step output
   */
  parseOutput(
    rawOutput: unknown,
    stepConfig?: unknown,
    metadata?: StepOutputMetadata,
  ): T;

  /**
   * Generate a human-readable summary for AI prompts.
   * This should provide enough context for the AI to understand
   * what happened and what can be verified.
   *
   * @param output - The parsed step output
   * @returns Markdown-formatted summary string
   */
  summarizeForAI(output: T): string;

  /**
   * Generate test configuration from the step output.
   * This is used to auto-populate test config when saving drafts.
   *
   * @param output - The parsed step output
   * @returns Test auto-population config, or null if not applicable
   */
  toTestConfig(output: T): TestAutoPopulateConfig | null;

  /**
   * List fields that can be used in assertions.
   * This helps the AI generator understand what can be verified.
   *
   * @param output - The parsed step output
   * @returns Array of assertable fields
   */
  getAssertableFields(output: T): AssertableField[];
}

// ============================================================================
// Metadata for Step Output Creation
// ============================================================================

/**
 * Metadata passed when creating a step output.
 */
export interface StepOutputMetadata {
  /** ID of the step that produced this output */
  step_id?: string;
  /** Human-readable step name */
  step_name?: string;
  /** Execution start time */
  started_at?: string;
  /** Execution duration in milliseconds */
  duration_ms?: number;
  /** Whether execution succeeded */
  success?: boolean;
  /** Error message if failed */
  error?: string;
}

// ============================================================================
// Handler Registration
// ============================================================================

/**
 * Type for handler constructor/factory function.
 */
export type StepOutputHandlerFactory<T extends StepOutput = StepOutput> = () => StepOutputHandler<T>;
