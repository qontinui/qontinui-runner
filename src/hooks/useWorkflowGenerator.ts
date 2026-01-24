/**
 * useWorkflowGenerator
 *
 * Hook for generating unified workflows from loaded state machine configurations.
 * Fetches the full configuration from the Rust backend and uses the
 * WorkflowGeneratorService to create a complete workflow.
 */

import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  workflowGeneratorService,
  type GeneratorConfig,
  type GeneratorOptions,
} from "../services";
import type { UnifiedWorkflow } from "../types/unified-workflow";

// =============================================================================
// Types
// =============================================================================

/**
 * State from the Rust QontinuiConfig
 */
interface RustState {
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
 * Context from the Rust QontinuiConfig
 */
interface RustContext {
  id: string;
  name: string;
  content: string;
  category?: string;
  tags?: string[];
}

/**
 * Response from get_current_configuration
 */
interface RustQontinuiConfig {
  version: string;
  metadata: {
    name: string;
    description?: string;
    projectId?: string;
  };
  states: RustState[];
  contexts?: RustContext[];
  workflows?: unknown[];
  images?: unknown[];
  transitions?: unknown[];
  categories?: unknown[];
}

/**
 * Result from the generator hook
 */
export interface GeneratorResult {
  success: boolean;
  workflow?: UnifiedWorkflow;
  error?: string;
}

/**
 * Return type for the useWorkflowGenerator hook
 */
export interface UseWorkflowGeneratorReturn {
  /** Generate a workflow from the current loaded configuration */
  generate: (options?: GeneratorOptions) => Promise<GeneratorResult>;
  /** Whether generation is in progress */
  isGenerating: boolean;
  /** Last error message */
  error: string | null;
  /** Whether a configuration is available for generation */
  hasConfig: boolean;
  /** Check if config is available */
  checkConfigAvailable: () => Promise<boolean>;
  /** Get the number of states in the loaded config */
  getStateCount: () => Promise<number>;
}

// =============================================================================
// Hook Implementation
// =============================================================================

/**
 * Hook for generating unified workflows from state machine configurations.
 *
 * @example
 * ```tsx
 * const { generate, isGenerating, hasConfig } = useWorkflowGenerator();
 *
 * const handleGenerate = async () => {
 *   const result = await generate({
 *     workflowName: "My Verification Workflow",
 *     maxIterations: 15,
 *   });
 *   if (result.success && result.workflow) {
 *     setWorkflow(result.workflow);
 *   }
 * };
 * ```
 */
export function useWorkflowGenerator(): UseWorkflowGeneratorReturn {
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hasConfig, setHasConfig] = useState(false);

  /**
   * Fetch the current configuration from the Rust backend
   */
  const fetchConfig = useCallback(async (): Promise<RustQontinuiConfig | null> => {
    try {
      const config = await invoke<RustQontinuiConfig>("get_current_configuration");
      return config;
    } catch (err) {
      console.error("[WorkflowGenerator] Failed to fetch config:", err);
      return null;
    }
  }, []);

  /**
   * Convert Rust config to generator config format
   */
  const convertToGeneratorConfig = useCallback(
    (rustConfig: RustQontinuiConfig): GeneratorConfig => {
      return {
        states: rustConfig.states.map((state) => ({
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
        })),
        contexts: rustConfig.contexts?.map((ctx) => ({
          id: ctx.id,
          name: ctx.name,
          content: ctx.content,
          category: ctx.category,
          tags: ctx.tags,
        })),
        metadata: {
          name: rustConfig.metadata.name,
          description: rustConfig.metadata.description,
        },
      };
    },
    [],
  );

  /**
   * Check if a configuration is available
   */
  const checkConfigAvailable = useCallback(async (): Promise<boolean> => {
    const config = await fetchConfig();
    const available = config !== null && config.states.length > 0;
    setHasConfig(available);
    return available;
  }, [fetchConfig]);

  /**
   * Get the number of states in the current config
   */
  const getStateCount = useCallback(async (): Promise<number> => {
    const config = await fetchConfig();
    return config?.states.length ?? 0;
  }, [fetchConfig]);

  /**
   * Generate a workflow from the loaded configuration
   */
  const generate = useCallback(
    async (options: GeneratorOptions = {}): Promise<GeneratorResult> => {
      setIsGenerating(true);
      setError(null);

      try {
        // Fetch the current configuration
        const rustConfig = await fetchConfig();

        if (!rustConfig) {
          const errorMsg = "No configuration loaded. Please load a configuration first.";
          setError(errorMsg);
          return { success: false, error: errorMsg };
        }

        // Validate the configuration
        const generatorConfig = convertToGeneratorConfig(rustConfig);
        const validation = workflowGeneratorService.validateConfig(generatorConfig);

        if (!validation.valid) {
          const errorMsg = `Configuration validation failed: ${validation.errors.join(", ")}`;
          setError(errorMsg);
          return { success: false, error: errorMsg };
        }

        // Generate the workflow
        const workflow = workflowGeneratorService.generateFromStateMachine(
          generatorConfig,
          {
            workflowName:
              options.workflowName ??
              `${rustConfig.metadata.name || "State Machine"} Verification`,
            workflowDescription:
              options.workflowDescription ??
              `Auto-generated workflow to verify states in ${rustConfig.metadata.name || "the loaded configuration"}`,
            ...options,
          },
        );

        setHasConfig(true);
        return { success: true, workflow };
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : "Failed to generate workflow";
        setError(errorMsg);
        return { success: false, error: errorMsg };
      } finally {
        setIsGenerating(false);
      }
    },
    [fetchConfig, convertToGeneratorConfig],
  );

  return {
    generate,
    isGenerating,
    error,
    hasConfig,
    checkConfigAvailable,
    getStateCount,
  };
}
