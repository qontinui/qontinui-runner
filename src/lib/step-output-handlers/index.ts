/**
 * Step Output Handlers
 *
 * This module provides a registry-based system for handling outputs from
 * different step types in automation workflows. Each handler knows how to:
 *
 * - Parse raw output into typed StepOutput
 * - Generate AI-friendly summaries for test generation
 * - Auto-populate test configuration
 * - List assertable fields for verification
 *
 * Usage:
 * ```typescript
 * import { stepOutputRegistry, commandHandler } from './step-output-handlers';
 *
 * // Get a handler for a specific step type
 * const handler = stepOutputRegistry.get('command');
 *
 * // Parse raw output
 * const output = handler.parseOutput(rawData, stepConfig);
 *
 * // Generate summary for AI
 * const summary = handler.summarizeForAI(output);
 *
 * // Get test config
 * const config = handler.toTestConfig(output);
 * ```
 */

// Export types
export type {
  StepOutputHandler,
  StepOutputMetadata,
  TestAutoPopulateConfig,
  AssertableField,
  StepOutputHandlerFactory,
} from "./types";

// Export registry
export {
  stepOutputRegistry,
  collectTestConfigs,
  summarizeOutputsForAI,
  collectAssertableFields,
} from "./registry";

// Export individual handlers
export { CommandHandler, commandHandler } from "./command-handler";
export { CheckHandler, checkHandler } from "./check-handler";

// Import for registration
import { stepOutputRegistry } from "./registry";
import { commandHandler } from "./command-handler";
import { checkHandler } from "./check-handler";

// ============================================================================
// Handler Registration
// ============================================================================

/**
 * Initialize the registry with all built-in handlers.
 * This is called automatically on module load.
 */
function initializeRegistry(): void {
  if (stepOutputRegistry.isInitialized()) {
    return;
  }

  // Register all built-in handlers
  stepOutputRegistry.register(commandHandler);
  stepOutputRegistry.register(checkHandler);

  // Backward compat: also register "shell_command" pointing to the command handler
  const shellCommandAlias = Object.create(commandHandler);
  shellCommandAlias.stepType = "shell_command";
  stepOutputRegistry.register(shellCommandAlias);

  // Mark as initialized
  stepOutputRegistry.markInitialized();
}

// Initialize on module load
initializeRegistry();

// ============================================================================
// Re-export step output types for convenience
// ============================================================================

export type {
  StepOutput,
  StepOutputType,
  BaseStepOutput,
  CommandStepOutput,
  CheckStepOutput,
  UiBridgeStepOutput,
  CheckIssue,
} from "../../types/step-output";

export {
  generateStepOutputId,
  isCommandOutput,
  isCheckOutput,
  isUiBridgeOutput,
} from "../../types/step-output";
