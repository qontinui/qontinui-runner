/**
 * useStateToWorkflow
 *
 * Hook for generating verification tests and agentic instructions from
 * state machine states and adding them to the current workflow.
 *
 * Bridges the state machine configuration with the Workflow Builder,
 * enabling UI Bridge-based automation workflows.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  workflowGeneratorService,
  type GeneratorState,
  type SingleStateGeneratorOptions,
  type SingleStateAgenticOptions,
} from "../services";
import type {
  VerificationStep,
  PromptStep,
  WorkflowPhase,
  UnifiedStep,
} from "../types/unified-workflow";

// =============================================================================
// Types
// =============================================================================

/**
 * State from the Rust QontinuiConfig (full structure)
 */
interface ConfigState {
  id: string;
  name: string;
  description?: string;
  stateImages?: Array<{
    id: string;
    name: string;
    patterns?: Array<{
      id: string;
      name?: string;
      imageId?: string;
    }>;
    ocrText?: string;
    searchMode?: string;
  }>;
  regions?: Array<{
    id: string;
    name: string;
    x: number;
    y: number;
    width: number;
    height: number;
    isSearchRegion?: boolean;
  }>;
  locations?: Array<{
    id: string;
    name: string;
    x: number;
    y: number;
  }>;
  strings?: Array<{
    id: string;
    name: string;
    value: string;
    identifier?: boolean;
    inputText?: boolean;
    expectedText?: boolean;
  }>;
  isInitial?: boolean;
  isFinal?: boolean;
  position?: { x: number; y: number };
}

/**
 * Configuration loaded from backend
 */
interface LoadedConfig {
  version: string;
  metadata: {
    name: string;
    description?: string;
  };
  states: ConfigState[];
}

/**
 * Options for adding state steps to workflow
 */
export interface AddStateToWorkflowOptions
  extends SingleStateGeneratorOptions, SingleStateAgenticOptions {
  /** Add verification steps to workflow (default: true) */
  addVerificationSteps?: boolean;
  /** Add agentic instruction to workflow (default: true) */
  addAgenticStep?: boolean;
  /** Custom name prefix for generated steps */
  stepNamePrefix?: string;
}

/**
 * Result from adding state to workflow
 */
export interface AddStateToWorkflowResult {
  success: boolean;
  verificationSteps: VerificationStep[];
  agenticStep: PromptStep | null;
  error?: string;
}

/**
 * Return type for the useStateToWorkflow hook
 */
export interface UseStateToWorkflowReturn {
  /** Available states from the loaded configuration */
  states: ConfigState[];
  /** Whether states are being loaded */
  isLoading: boolean;
  /** Last error message */
  error: string | null;
  /** Whether a configuration is loaded */
  hasConfig: boolean;
  /** Refresh states from the backend */
  refreshStates: () => Promise<void>;
  /** Generate steps for a state (preview without adding) */
  generateStepsForState: (
    stateId: string,
    options?: AddStateToWorkflowOptions,
  ) => {
    verificationSteps: VerificationStep[];
    agenticStep: PromptStep;
  } | null;
  /** Generate and return steps ready for adding to workflow */
  prepareStepsForWorkflow: (
    stateId: string,
    options?: AddStateToWorkflowOptions,
  ) => {
    verificationSteps: VerificationStep[];
    agenticStep: PromptStep | null;
    addToWorkflow: (addStep: (step: UnifiedStep, phase: WorkflowPhase) => void) => void;
  } | null;
  /** Get a state by ID */
  getStateById: (stateId: string) => ConfigState | null;
  /** Get state summary for display */
  getStateSummary: (state: ConfigState) => {
    imageCount: number;
    regionCount: number;
    locationCount: number;
    stringCount: number;
    hasDescription: boolean;
    isInitial: boolean;
    isFinal: boolean;
  };
}

// =============================================================================
// Hook Implementation
// =============================================================================

/**
 * Hook for generating and adding state-based steps to workflows.
 *
 * @example
 * ```tsx
 * const { states, generateStepsForState, prepareStepsForWorkflow } = useStateToWorkflow();
 * const { addStep } = useWorkflowBuilder();
 *
 * // Preview what will be generated
 * const preview = generateStepsForState(selectedStateId, { includePlaywrightTest: true });
 *
 * // Add to workflow
 * const prepared = prepareStepsForWorkflow(selectedStateId, options);
 * if (prepared) {
 *   prepared.addToWorkflow(addStep);
 * }
 * ```
 */
export function useStateToWorkflow(): UseStateToWorkflowReturn {
  const [states, setStates] = useState<ConfigState[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasConfig, setHasConfig] = useState(false);

  /**
   * Fetch states from the loaded configuration
   */
  const refreshStates = useCallback(async () => {
    setIsLoading(true);
    setError(null);

    try {
      const config = await invoke<LoadedConfig>("get_current_configuration");

      if (config && config.states) {
        setStates(config.states);
        setHasConfig(true);
      } else {
        setStates([]);
        setHasConfig(false);
      }
    } catch (err) {
      console.error("[useStateToWorkflow] Failed to fetch config:", err);
      setError(err instanceof Error ? err.message : "Failed to load configuration");
      setStates([]);
      setHasConfig(false);
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Load states on mount
  useEffect(() => {
    refreshStates();
  }, [refreshStates]);

  /**
   * Convert ConfigState to GeneratorState format
   */
  const toGeneratorState = useCallback((state: ConfigState): GeneratorState => {
    return {
      id: state.id,
      name: state.name,
      description: state.description,
      stateImages: state.stateImages,
      regions: state.regions,
      locations: state.locations,
      strings: state.strings,
      isInitial: state.isInitial,
      isFinal: state.isFinal,
      position: state.position,
    };
  }, []);

  /**
   * Get a state by ID
   */
  const getStateById = useCallback(
    (stateId: string): ConfigState | null => {
      return states.find((s) => s.id === stateId) || null;
    },
    [states],
  );

  /**
   * Get state summary for display
   */
  const getStateSummary = useCallback(
    (state: ConfigState) => ({
      imageCount: state.stateImages?.length || 0,
      regionCount: state.regions?.length || 0,
      locationCount: state.locations?.length || 0,
      stringCount: state.strings?.length || 0,
      hasDescription: !!state.description,
      isInitial: state.isInitial || false,
      isFinal: state.isFinal || false,
    }),
    [],
  );

  /**
   * Generate steps for a state (preview without adding)
   */
  const generateStepsForState = useCallback(
    (
      stateId: string,
      options: AddStateToWorkflowOptions = {},
    ): {
      verificationSteps: VerificationStep[];
      agenticStep: PromptStep;
    } | null => {
      const state = getStateById(stateId);
      if (!state) {
        console.error("[useStateToWorkflow] State not found:", stateId);
        return null;
      }

      const generatorState = toGeneratorState(state);
      return workflowGeneratorService.generateStepsForState(generatorState, options);
    },
    [getStateById, toGeneratorState],
  );

  /**
   * Prepare steps for adding to workflow
   */
  const prepareStepsForWorkflow = useCallback(
    (
      stateId: string,
      options: AddStateToWorkflowOptions = {},
    ): {
      verificationSteps: VerificationStep[];
      agenticStep: PromptStep | null;
      addToWorkflow: (addStep: (step: UnifiedStep, phase: WorkflowPhase) => void) => void;
    } | null => {
      const {
        addVerificationSteps = true,
        addAgenticStep = true,
        stepNamePrefix,
        ...generatorOptions
      } = options;

      const state = getStateById(stateId);
      if (!state) {
        console.error("[useStateToWorkflow] State not found:", stateId);
        return null;
      }

      const generatorState = toGeneratorState(state);
      const generated = workflowGeneratorService.generateStepsForState(
        generatorState,
        generatorOptions,
      );

      // Apply name prefix if provided
      const verificationSteps = generated.verificationSteps.map((step) => ({
        ...step,
        name: stepNamePrefix ? `${stepNamePrefix}: ${step.name}` : step.name,
      }));

      const agenticStep = addAgenticStep
        ? {
            ...generated.agenticStep,
            name: stepNamePrefix
              ? `${stepNamePrefix}: ${generated.agenticStep.name}`
              : generated.agenticStep.name,
          }
        : null;

      return {
        verificationSteps: addVerificationSteps ? verificationSteps : [],
        agenticStep,
        addToWorkflow: (addStep) => {
          // Add verification steps
          if (addVerificationSteps) {
            verificationSteps.forEach((step) => {
              addStep(step, "verification");
            });
          }

          // Add agentic step
          if (addAgenticStep && agenticStep) {
            addStep(agenticStep, "agentic");
          }
        },
      };
    },
    [getStateById, toGeneratorState],
  );

  return {
    states,
    isLoading,
    error,
    hasConfig,
    refreshStates,
    generateStepsForState,
    prepareStepsForWorkflow,
    getStateById,
    getStateSummary,
  };
}
