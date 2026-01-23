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
 * import { stepOutputRegistry, apiRequestHandler } from './step-output-handlers';
 *
 * // Get a handler for a specific step type
 * const handler = stepOutputRegistry.get('api_request');
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
export { ApiRequestHandler, apiRequestHandler } from "./api-request-handler";
export { GuiActionHandler, guiActionHandler } from "./gui-action-handler";
export { ShellCommandHandler, shellCommandHandler } from "./shell-command-handler";
export { McpCallHandler, mcpCallHandler } from "./mcp-call-handler";
export { ScreenshotHandler, screenshotHandler } from "./screenshot-handler";
export { WorkflowRefHandler, workflowRefHandler } from "./workflow-ref-handler";
export { PlaywrightScriptHandler, playwrightScriptHandler } from "./playwright-script-handler";
export { StateNavigationHandler, stateNavigationHandler } from "./state-navigation-handler";
export { CheckHandler, checkHandler } from "./check-handler";

// Import for registration
import { stepOutputRegistry } from "./registry";
import { apiRequestHandler } from "./api-request-handler";
import { guiActionHandler } from "./gui-action-handler";
import { shellCommandHandler } from "./shell-command-handler";
import { mcpCallHandler } from "./mcp-call-handler";
import { screenshotHandler } from "./screenshot-handler";
import { workflowRefHandler } from "./workflow-ref-handler";
import { playwrightScriptHandler } from "./playwright-script-handler";
import { stateNavigationHandler } from "./state-navigation-handler";
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
  stepOutputRegistry.register(apiRequestHandler);
  stepOutputRegistry.register(guiActionHandler);
  stepOutputRegistry.register(shellCommandHandler);
  stepOutputRegistry.register(mcpCallHandler);
  stepOutputRegistry.register(screenshotHandler);
  stepOutputRegistry.register(workflowRefHandler);
  stepOutputRegistry.register(playwrightScriptHandler);
  stepOutputRegistry.register(stateNavigationHandler);
  stepOutputRegistry.register(checkHandler);

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
  ApiRequestStepOutput,
  GuiActionStepOutput,
  ShellCommandStepOutput,
  McpCallStepOutput,
  ScreenshotStepOutput,
  WorkflowRefStepOutput,
  PlaywrightScriptStepOutput,
  StateNavigationStepOutput,
  CheckStepOutput,
  ScreenBoundingBox,
  DetectedScreenElement,
  CheckIssue,
} from "../../types/step-output";

export {
  generateStepOutputId,
  isApiRequestOutput,
  isGuiActionOutput,
  isShellCommandOutput,
  isMcpCallOutput,
  isScreenshotOutput,
  isWorkflowRefOutput,
  isPlaywrightScriptOutput,
  isStateNavigationOutput,
  isCheckOutput,
} from "../../types/step-output";
