/**
 * useConfigState
 *
 * Hook for managing configuration state.
 * Responsibility: Manage React state for configuration, workflows, and loading status.
 */

import { useState, useCallback } from "react";
import type { Config, Workflow } from "./types";

interface UseConfigStateReturn {
  config: Config | null;
  setConfig: (config: Config | null) => void;
  configLoaded: boolean;
  setConfigLoaded: (loaded: boolean) => void;
  workflows: Workflow[];
  setWorkflows: (workflows: Workflow[]) => void;
  automationEnabledCategories: string[];
  setAutomationEnabledCategories: (categories: string[]) => void;
  updateConfigAndWorkflows: (config: Config, workflows: Workflow[], categories: string[]) => void;
}

/**
 * Hook to manage configuration state
 */
export function useConfigState(): UseConfigStateReturn {
  const [config, setConfig] = useState<Config | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [automationEnabledCategories, setAutomationEnabledCategories] = useState<string[]>([]);

  /**
   * Update config, workflows, and categories together, and mark as loaded
   */
  const updateConfigAndWorkflows = useCallback(
    (newConfig: Config, newWorkflows: Workflow[], categories: string[]) => {
      setConfig(newConfig);
      setWorkflows(newWorkflows);
      setAutomationEnabledCategories(categories);
      setConfigLoaded(true);
    },
    [],
  );

  return {
    config,
    setConfig,
    configLoaded,
    setConfigLoaded,
    workflows,
    setWorkflows,
    automationEnabledCategories,
    setAutomationEnabledCategories,
    updateConfigAndWorkflows,
  };
}
