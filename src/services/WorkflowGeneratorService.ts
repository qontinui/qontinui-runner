/**
 * WorkflowGeneratorService
 *
 * Generates unified workflows from state machine configurations.
 * Converts states into verification steps and builds semantic context
 * for agentic AI phases.
 */

import type {
  UnifiedWorkflow,
  SetupStep,
  VerificationStep,
  AgenticStep,
  CompletionStep,
  StateStep,
  PromptStep,
} from "../types/unified-workflow";
import { generateStepId, createSummaryStep } from "../types/unified-workflow";

// =============================================================================
// Types from loaded configuration
// =============================================================================

/**
 * State image from the state machine configuration
 */
export interface GeneratorStateImage {
  id: string;
  name: string;
  patterns?: Array<{
    id: string;
    name?: string;
    imageId?: string;
  }>;
  ocrText?: string;
  searchMode?: string;
}

/**
 * State region from the state machine configuration
 */
export interface GeneratorStateRegion {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  isSearchRegion?: boolean;
}

/**
 * State location from the state machine configuration
 */
export interface GeneratorStateLocation {
  id: string;
  name: string;
  x: number;
  y: number;
}

/**
 * State string from the state machine configuration
 */
export interface GeneratorStateString {
  id: string;
  name: string;
  value: string;
  identifier?: boolean;
  inputText?: boolean;
  expectedText?: boolean;
}

/**
 * Full state from the state machine configuration
 */
export interface GeneratorState {
  id: string;
  name: string;
  description?: string;
  stateImages?: GeneratorStateImage[];
  regions?: GeneratorStateRegion[];
  locations?: GeneratorStateLocation[];
  strings?: GeneratorStateString[];
  isInitial?: boolean;
  isFinal?: boolean;
  position?: { x: number; y: number };
}

/**
 * AI context from the configuration
 */
export interface GeneratorContext {
  id: string;
  name: string;
  content: string;
  category?: string;
  tags?: string[];
}

/**
 * Full configuration for workflow generation
 */
export interface GeneratorConfig {
  states: GeneratorState[];
  contexts?: GeneratorContext[];
  metadata?: {
    name?: string;
    description?: string;
  };
}

/**
 * Options for workflow generation
 */
export interface GeneratorOptions {
  /** Name for the generated workflow */
  workflowName?: string;
  /** Description for the generated workflow */
  workflowDescription?: string;
  /** Include all states or only specific ones */
  stateIds?: string[];
  /** Include contexts in the agentic prompt */
  includeContexts?: boolean;
  /** Maximum iterations for the agentic phase */
  maxIterations?: number;
  /** Timeout per state verification (seconds) */
  stateTimeout?: number;
  /** Include a setup step to navigate to initial state */
  includeSetupNavigation?: boolean;
  /** Include the AI summary step in completion */
  includeSummary?: boolean;
  /** Category for the workflow */
  category?: string;
}

// =============================================================================
// Service Implementation
// =============================================================================

class WorkflowGeneratorServiceClass {
  /**
   * Generate a unified workflow from a state machine configuration.
   *
   * @param config - The loaded state machine configuration
   * @param options - Generation options
   * @returns A complete UnifiedWorkflow ready for use
   */
  generateFromStateMachine(
    config: GeneratorConfig,
    options: GeneratorOptions = {},
  ): UnifiedWorkflow {
    const {
      workflowName = "State Machine Verification Workflow",
      workflowDescription = "Auto-generated workflow that verifies all states in the state machine",
      stateIds,
      includeContexts = true,
      maxIterations = 10,
      stateTimeout = 30,
      includeSetupNavigation = true,
      includeSummary = true,
      category = "Main",
    } = options;

    // Filter states if specific IDs provided
    const states = stateIds
      ? config.states.filter((s) => stateIds.includes(s.id))
      : config.states;

    // Find initial state(s)
    const initialStates = states.filter((s) => s.isInitial);
    const initialState = initialStates.length > 0 ? initialStates[0] : states[0];

    // Generate setup steps
    const setupSteps: SetupStep[] = [];
    if (includeSetupNavigation && initialState) {
      setupSteps.push(this.createStateStep(initialState, "setup", stateTimeout));
    }

    // Generate verification steps for each state
    const verificationSteps: VerificationStep[] = states.map((state) =>
      this.createStateStep(state, "verification", stateTimeout),
    );

    // Generate agentic step with semantic context
    const agenticSteps: AgenticStep[] = [
      this.createAgenticPrompt(states, config.contexts, includeContexts),
    ];

    // Generate completion steps
    const completionSteps: CompletionStep[] = [];
    if (includeSummary) {
      completionSteps.push(createSummaryStep() as CompletionStep);
    }

    const now = new Date().toISOString();

    return {
      id: generateStepId(),
      name: workflowName,
      description: workflowDescription,
      setup_steps: setupSteps,
      verification_steps: verificationSteps,
      agentic_steps: agenticSteps,
      completion_steps: completionSteps,
      max_iterations: maxIterations,
      category,
      tags: ["auto-generated", "state-machine"],
      created_at: now,
      modified_at: now,
    };
  }

  /**
   * Create a StateStep for navigation/verification.
   */
  private createStateStep(
    state: GeneratorState,
    phase: "setup" | "verification" | "completion",
    timeoutSeconds: number,
  ): StateStep {
    return {
      id: generateStepId(),
      type: "state",
      phase,
      name: `Verify: ${state.name}`,
      state_id: state.id,
      state_name: state.name,
      timeout_seconds: timeoutSeconds,
      is_blocking: phase === "verification",
    } as StateStep;
  }

  /**
   * Create the agentic prompt with semantic context from states.
   */
  private createAgenticPrompt(
    states: GeneratorState[],
    contexts?: GeneratorContext[],
    includeContexts?: boolean,
  ): AgenticStep {
    const semanticContext = this.buildSemanticContext(states);
    const contextContent =
      includeContexts && contexts ? this.buildContextContent(contexts) : "";

    const content = `You are automating an application using a state machine.
The verification phase found issues reaching one or more expected states.

## Your Task
Navigate the application to reach the expected states described below.
Use the visual elements and semantic descriptions to guide your actions.

## State Machine Context
${semanticContext}
${contextContent ? `\n## AI Knowledge Contexts\n${contextContent}` : ""}

## Guidelines
1. Use the state descriptions to understand what each screen represents
2. Look for the visual elements (images) described for each state
3. Use the regions and locations as reference points for interactions
4. Check for expected text (strings) to verify you're in the right state
5. If a state is marked as "initial", it's the starting point
6. If a state is marked as "final", it's a goal state

Fix any navigation issues to successfully reach all expected states.`;

    return {
      id: generateStepId(),
      type: "prompt",
      phase: "agentic",
      name: "Navigate State Machine",
      content,
    } as PromptStep;
  }

  /**
   * Build semantic context string from states.
   */
  private buildSemanticContext(states: GeneratorState[]): string {
    return states
      .map((state) => {
        const lines: string[] = [];

        // State header with flags
        const flags: string[] = [];
        if (state.isInitial) flags.push("INITIAL");
        if (state.isFinal) flags.push("FINAL");
        const flagStr = flags.length > 0 ? ` [${flags.join(", ")}]` : "";
        lines.push(`### ${state.name}${flagStr}`);

        // Description
        if (state.description) {
          lines.push(`**Description:** ${state.description}`);
        }

        // State Images (visual identifiers)
        if (state.stateImages && state.stateImages.length > 0) {
          lines.push("**Visual Elements:**");
          state.stateImages.forEach((img) => {
            const patternCount = img.patterns?.length || 0;
            const ocrInfo = img.ocrText ? ` (OCR: "${img.ocrText}")` : "";
            lines.push(`- ${img.name}${ocrInfo}${patternCount > 1 ? ` (${patternCount} variations)` : ""}`);
          });
        }

        // Regions
        if (state.regions && state.regions.length > 0) {
          lines.push("**Regions:**");
          state.regions.forEach((region) => {
            const searchFlag = region.isSearchRegion ? " [search region]" : "";
            lines.push(`- ${region.name}: ${region.width}x${region.height} at (${region.x}, ${region.y})${searchFlag}`);
          });
        }

        // Locations (click targets)
        if (state.locations && state.locations.length > 0) {
          lines.push("**Click Targets:**");
          state.locations.forEach((loc) => {
            lines.push(`- ${loc.name}: (${loc.x}, ${loc.y})`);
          });
        }

        // Strings (text values)
        if (state.strings && state.strings.length > 0) {
          lines.push("**Text Values:**");
          state.strings.forEach((str) => {
            const flags: string[] = [];
            if (str.identifier) flags.push("OCR identifier");
            if (str.inputText) flags.push("input text");
            if (str.expectedText) flags.push("expected text");
            const flagStr = flags.length > 0 ? ` (${flags.join(", ")})` : "";
            lines.push(`- ${str.name}: "${str.value}"${flagStr}`);
          });
        }

        return lines.join("\n");
      })
      .join("\n\n");
  }

  /**
   * Build context content from AI contexts.
   */
  private buildContextContent(contexts: GeneratorContext[]): string {
    return contexts
      .map((ctx) => {
        const header = ctx.category ? `### ${ctx.name} (${ctx.category})` : `### ${ctx.name}`;
        return `${header}\n${ctx.content}`;
      })
      .join("\n\n");
  }

  /**
   * Extract context IDs to include in the workflow.
   */
  extractContextIds(contexts: GeneratorContext[]): string[] {
    return contexts.map((c) => c.id);
  }

  /**
   * Validate a configuration has enough data for generation.
   */
  validateConfig(config: GeneratorConfig): { valid: boolean; errors: string[] } {
    const errors: string[] = [];

    if (!config.states || config.states.length === 0) {
      errors.push("No states found in configuration");
    }

    // Check for states without names
    const unnamedStates = config.states.filter((s) => !s.name);
    if (unnamedStates.length > 0) {
      errors.push(`${unnamedStates.length} state(s) missing names`);
    }

    return {
      valid: errors.length === 0,
      errors,
    };
  }
}

/**
 * Singleton instance for use throughout the app.
 */
export const workflowGeneratorService = new WorkflowGeneratorServiceClass();

/**
 * Export class for testing
 */
export { WorkflowGeneratorServiceClass as WorkflowGeneratorService };
