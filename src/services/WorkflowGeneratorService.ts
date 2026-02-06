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
    const states = stateIds ? config.states.filter((s) => stateIds.includes(s.id)) : config.states;

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
    const contextContent = includeContexts && contexts ? this.buildContextContent(contexts) : "";

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
            lines.push(
              `- ${img.name}${ocrInfo}${patternCount > 1 ? ` (${patternCount} variations)` : ""}`,
            );
          });
        }

        // Regions
        if (state.regions && state.regions.length > 0) {
          lines.push("**Regions:**");
          state.regions.forEach((region) => {
            const searchFlag = region.isSearchRegion ? " [search region]" : "";
            lines.push(
              `- ${region.name}: ${region.width}x${region.height} at (${region.x}, ${region.y})${searchFlag}`,
            );
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

  // ===========================================================================
  // Single State Generation Methods
  // ===========================================================================

  /**
   * Options for generating steps for a single state
   */
  // (Defined as interface below)

  /**
   * Generate verification steps for a single state.
   *
   * Creates multiple verification approaches based on the state's properties:
   * - StateStep: Navigate to and verify the state is active
   * - TestStep (qontinui_vision): Visual verification using state images
   * - TestStep (playwright): Browser-based verification for web states
   *
   * @param state - The state to generate verification for
   * @param options - Generation options
   * @returns Array of verification steps
   */
  generateVerificationForState(
    state: GeneratorState,
    options: SingleStateGeneratorOptions = {},
  ): VerificationStep[] {
    const {
      includeStateStep = true,
      includeVisionTest = true,
      includePlaywrightTest = false,
      timeoutSeconds = 30,
    } = options;

    const steps: VerificationStep[] = [];

    // 1. StateStep - Basic state navigation/verification
    if (includeStateStep) {
      steps.push({
        id: generateStepId(),
        type: "state",
        phase: "verification",
        name: `Verify: ${state.name}`,
        state_id: state.id,
        state_name: state.name,
        timeout_seconds: timeoutSeconds,
      } as StateStep);
    }

    // 2. Vision Test - Visual verification using state images
    if (includeVisionTest && state.stateImages && state.stateImages.length > 0) {
      steps.push({
        id: generateStepId(),
        type: "test",
        phase: "verification",
        name: `Vision Test: ${state.name}`,
        test_type: "qontinui_vision",
        // The vision test will use state images from the loaded config
      } as VerificationStep);
    }

    // 3. Playwright Test - Browser-based verification
    if (includePlaywrightTest) {
      const playwrightCode = this.generatePlaywrightTestForState(state);
      steps.push({
        id: generateStepId(),
        type: "test",
        phase: "verification",
        name: `Playwright Test: ${state.name}`,
        test_type: "playwright",
        code: playwrightCode,
      } as VerificationStep);
    }

    return steps;
  }

  /**
   * Generate agentic instruction (prompt) for a single state.
   *
   * Creates an AI prompt with semantic context to help the AI understand
   * how to navigate to or interact with the state.
   *
   * @param state - The state to generate instruction for
   * @param options - Generation options
   * @returns A PromptStep with semantic context
   */
  generateAgenticInstructionForState(
    state: GeneratorState,
    options: SingleStateAgenticOptions = {},
  ): PromptStep {
    const {
      taskDescription,
      includeNavigationGuidance = true,
      includeInteractionGuidance = true,
      includeErrorHandling = true,
      additionalContext,
    } = options;

    const semanticContext = this.buildSemanticContextForSingleState(state);

    let content = `You are working with a specific application state.

## Target State: ${state.name}
${state.description ? `\n**Description:** ${state.description}\n` : ""}
${taskDescription ? `\n## Task\n${taskDescription}\n` : ""}
## State Details
${semanticContext}
`;

    if (includeNavigationGuidance) {
      content += `
## Navigation Guidance
${this.generateNavigationGuidance(state)}
`;
    }

    if (includeInteractionGuidance && this.hasInteractiveElements(state)) {
      content += `
## Interaction Guidance
${this.generateInteractionGuidance(state)}
`;
    }

    if (includeErrorHandling) {
      content += `
## Error Handling
- If the expected visual elements are not found, check if a loading screen or modal is blocking
- If text verification fails, the page content may have changed - take a screenshot and analyze
- If navigation fails, try alternative paths or wait for animations to complete
`;
    }

    if (additionalContext) {
      content += `
## Additional Context
${additionalContext}
`;
    }

    return {
      id: generateStepId(),
      type: "prompt",
      phase: "agentic",
      name: `Navigate to: ${state.name}`,
      content,
    };
  }

  /**
   * Generate both verification steps and agentic instruction for a single state.
   *
   * This is a convenience method that combines generateVerificationForState
   * and generateAgenticInstructionForState.
   *
   * @param state - The state to generate steps for
   * @param options - Combined generation options
   * @returns Object containing verification steps and agentic prompt
   */
  generateStepsForState(
    state: GeneratorState,
    options: SingleStateGeneratorOptions & SingleStateAgenticOptions = {},
  ): {
    verificationSteps: VerificationStep[];
    agenticStep: PromptStep;
  } {
    return {
      verificationSteps: this.generateVerificationForState(state, options),
      agenticStep: this.generateAgenticInstructionForState(state, options),
    };
  }

  /**
   * Build semantic context for a single state (more detailed than multi-state).
   */
  private buildSemanticContextForSingleState(state: GeneratorState): string {
    const sections: string[] = [];

    // Visual Elements with full details
    if (state.stateImages && state.stateImages.length > 0) {
      sections.push("### Visual Elements");
      state.stateImages.forEach((img, idx) => {
        const details: string[] = [`**${idx + 1}. ${img.name}**`];
        if (img.ocrText) {
          details.push(`   - Text content: "${img.ocrText}"`);
        }
        if (img.patterns && img.patterns.length > 0) {
          details.push(`   - Patterns: ${img.patterns.length} variation(s)`);
          img.patterns.forEach((p) => {
            if (p.name) details.push(`     - ${p.name}`);
          });
        }
        if (img.searchMode) {
          details.push(`   - Search mode: ${img.searchMode}`);
        }
        sections.push(details.join("\n"));
      });
    }

    // Regions with descriptions
    if (state.regions && state.regions.length > 0) {
      sections.push("\n### Screen Regions");
      state.regions.forEach((region) => {
        const regionType = region.isSearchRegion ? " (search region)" : "";
        sections.push(
          `- **${region.name}**${regionType}: ${region.width}x${region.height} pixels at position (${region.x}, ${region.y})`,
        );
      });
    }

    // Locations (click targets)
    if (state.locations && state.locations.length > 0) {
      sections.push("\n### Click Targets");
      state.locations.forEach((loc) => {
        sections.push(`- **${loc.name}**: Click at coordinates (${loc.x}, ${loc.y})`);
      });
    }

    // Strings with semantic meaning
    if (state.strings && state.strings.length > 0) {
      sections.push("\n### Text Elements");
      state.strings.forEach((str) => {
        const purposes: string[] = [];
        if (str.identifier) purposes.push("identifies this state");
        if (str.inputText) purposes.push("text to enter");
        if (str.expectedText) purposes.push("expected on screen");
        const purposeStr = purposes.length > 0 ? ` (${purposes.join(", ")})` : "";
        sections.push(`- **${str.name}**${purposeStr}: "${str.value}"`);
      });
    }

    // State flags
    const flags: string[] = [];
    if (state.isInitial) flags.push("This is an **initial state** (starting point)");
    if (state.isFinal) flags.push("This is a **final state** (goal/end state)");
    if (flags.length > 0) {
      sections.push("\n### State Properties");
      flags.forEach((f) => sections.push(`- ${f}`));
    }

    return sections.join("\n");
  }

  /**
   * Generate navigation guidance based on state properties.
   */
  private generateNavigationGuidance(state: GeneratorState): string {
    const guidance: string[] = [];

    if (state.stateImages && state.stateImages.length > 0) {
      guidance.push(
        `- Look for these visual elements to confirm you're in the "${state.name}" state:`,
      );
      state.stateImages.slice(0, 3).forEach((img) => {
        guidance.push(`  - ${img.name}${img.ocrText ? ` (contains "${img.ocrText}")` : ""}`);
      });
      if (state.stateImages.length > 3) {
        guidance.push(`  - ...and ${state.stateImages.length - 3} more elements`);
      }
    }

    if (state.strings) {
      const identifiers = state.strings.filter((s) => s.identifier);
      if (identifiers.length > 0) {
        guidance.push("- Verify these text identifiers are visible:");
        identifiers.forEach((s) => guidance.push(`  - "${s.value}"`));
      }
    }

    if (state.isInitial) {
      guidance.push("- This is the starting state - it should be visible after app launch");
    }

    if (state.isFinal) {
      guidance.push("- This is a goal state - reaching it indicates success");
    }

    return guidance.length > 0
      ? guidance.join("\n")
      : "- Navigate to this state using the application's UI";
  }

  /**
   * Generate interaction guidance for states with interactive elements.
   */
  private generateInteractionGuidance(state: GeneratorState): string {
    const guidance: string[] = [];

    // Input fields
    const inputStrings = state.strings?.filter((s) => s.inputText) || [];
    if (inputStrings.length > 0) {
      guidance.push("**Input Fields:**");
      inputStrings.forEach((s) => {
        guidance.push(`- Enter "${s.value}" in the ${s.name} field`);
      });
    }

    // Click targets
    if (state.locations && state.locations.length > 0) {
      guidance.push("\n**Clickable Elements:**");
      state.locations.forEach((loc) => {
        guidance.push(`- Click on "${loc.name}" at position (${loc.x}, ${loc.y})`);
      });
    }

    // Regions that might be interactive
    const interactiveRegions = state.regions?.filter((r) => !r.isSearchRegion) || [];
    if (interactiveRegions.length > 0) {
      guidance.push("\n**Interactive Regions:**");
      interactiveRegions.forEach((r) => {
        guidance.push(`- "${r.name}" area at (${r.x}, ${r.y})`);
      });
    }

    return guidance.join("\n");
  }

  /**
   * Check if a state has interactive elements.
   */
  private hasInteractiveElements(state: GeneratorState): boolean {
    return (
      (state.locations && state.locations.length > 0) ||
      (state.strings && state.strings.some((s) => s.inputText)) ||
      (state.regions && state.regions.some((r) => !r.isSearchRegion))
    );
  }

  /**
   * Generate Playwright test code for verifying a state.
   */
  private generatePlaywrightTestForState(state: GeneratorState): string {
    const assertions: string[] = [];

    // Add text assertions for identifier strings
    const identifiers = state.strings?.filter((s) => s.identifier || s.expectedText) || [];
    identifiers.forEach((str) => {
      assertions.push(
        `  // Verify "${str.name}" text is visible`,
        `  await expect(page.getByText('${str.value.replace(/'/g, "\\'")}')).toBeVisible();`,
      );
    });

    // Add assertions for state images with OCR text
    const ocrImages = state.stateImages?.filter((img) => img.ocrText) || [];
    ocrImages.forEach((img) => {
      assertions.push(
        `  // Verify "${img.name}" element is visible`,
        `  await expect(page.getByText('${img.ocrText!.replace(/'/g, "\\'")}')).toBeVisible();`,
      );
    });

    // If no specific assertions, add a basic page check
    if (assertions.length === 0) {
      assertions.push(
        `  // Basic page verification`,
        `  await expect(page).toHaveTitle(/.*/);`,
        `  // TODO: Add specific assertions for "${state.name}" state`,
      );
    }

    return `// Auto-generated Playwright test for state: ${state.name}
// ${state.description || "Verifies the state is correctly displayed"}

import { test, expect } from '@playwright/test';

test('verify ${state.name} state', async ({ page }) => {
${assertions.join("\n")}
});`;
  }
}

/**
 * Options for generating steps for a single state
 */
export interface SingleStateGeneratorOptions {
  /** Include a StateStep for navigation/verification (default: true) */
  includeStateStep?: boolean;
  /** Include a qontinui_vision test step (default: true) */
  includeVisionTest?: boolean;
  /** Include a Playwright test step (default: false) */
  includePlaywrightTest?: boolean;
  /** Timeout for verification in seconds (default: 30) */
  timeoutSeconds?: number;
}

/**
 * Options for generating agentic instructions for a single state
 */
export interface SingleStateAgenticOptions {
  /** Custom task description to include in the prompt */
  taskDescription?: string;
  /** Include navigation guidance (default: true) */
  includeNavigationGuidance?: boolean;
  /** Include interaction guidance for interactive elements (default: true) */
  includeInteractionGuidance?: boolean;
  /** Include error handling tips (default: true) */
  includeErrorHandling?: boolean;
  /** Additional context to append to the prompt */
  additionalContext?: string;
}

/**
 * Singleton instance for use throughout the app.
 */
export const workflowGeneratorService = new WorkflowGeneratorServiceClass();

/**
 * Export class for testing
 */
export { WorkflowGeneratorServiceClass as WorkflowGeneratorService };
