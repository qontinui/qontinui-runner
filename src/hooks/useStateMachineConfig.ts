/**
 * useStateMachineConfig
 *
 * Hook for CRUD operations on state machine configs, states, and transitions
 * via Tauri IPC commands.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  StateMachineConfig,
  StateMachineConfigFull,
  StateMachineState,
  StateMachineTransition,
  StateMachineConfigCreate,
  StateMachineConfigUpdate,
  StateMachineStateCreate,
  StateMachineStateUpdate,
  StateMachineTransitionCreate,
  StateMachineTransitionUpdate,
  StateMachineExportFormat,
} from "@qontinui/shared-types";

export interface UseStateMachineConfigReturn {
  // Config list
  configs: StateMachineConfig[];
  loadConfigs: () => Promise<void>;

  // Active config
  activeConfig: StateMachineConfigFull | null;
  loadConfig: (id: string) => Promise<void>;
  setActiveConfig: (config: StateMachineConfigFull | null) => void;

  // Config CRUD
  createConfig: (req: StateMachineConfigCreate) => Promise<StateMachineConfig>;
  updateConfig: (id: string, req: StateMachineConfigUpdate) => Promise<StateMachineConfig>;
  deleteConfig: (id: string) => Promise<void>;

  // State CRUD
  createState: (req: StateMachineStateCreate) => Promise<StateMachineState>;
  updateState: (id: string, req: StateMachineStateUpdate) => Promise<StateMachineState>;
  deleteState: (id: string) => Promise<void>;

  // Transition CRUD
  createTransition: (req: StateMachineTransitionCreate) => Promise<StateMachineTransition>;
  updateTransition: (
    id: string,
    req: StateMachineTransitionUpdate,
  ) => Promise<StateMachineTransition>;
  deleteTransition: (id: string) => Promise<void>;

  // Import/Export
  importConfig: (name: string, data: StateMachineExportFormat) => Promise<StateMachineConfig>;

  // Status
  isLoading: boolean;
  error: string | null;
}

const SM_SELECTED_CONFIG_KEY = "qontinui-runner-sm-selected-config";

export function useStateMachineConfig(): UseStateMachineConfigReturn {
  const [configs, setConfigs] = useState<StateMachineConfig[]>([]);
  const [activeConfig, setActiveConfigRaw] = useState<StateMachineConfigFull | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Wrap setActiveConfig to persist selection and notify listeners
  const setActiveConfig = useCallback((config: StateMachineConfigFull | null) => {
    setActiveConfigRaw(config);
    try {
      if (config) {
        localStorage.setItem(SM_SELECTED_CONFIG_KEY, config.id);
      } else {
        localStorage.removeItem(SM_SELECTED_CONFIG_KEY);
      }
    } catch {
      /* */
    }
    window.dispatchEvent(
      new CustomEvent("sm-config-changed", { detail: { configId: config?.id ?? null } }),
    );
  }, []);

  // ---- Config list ----

  const loadConfigs = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await invoke<StateMachineConfig[]>("sm_list_configs");
      setConfigs(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsLoading(false);
    }
  }, []);

  // ---- Active config ----

  const loadConfig = useCallback(
    async (id: string) => {
      setIsLoading(true);
      setError(null);
      try {
        const result = await invoke<StateMachineConfigFull | null>("sm_get_config", { id });
        if (!result) {
          setError("Config not found");
          setActiveConfig(null);
          return;
        }
        setActiveConfig(result);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setIsLoading(false);
      }
    },
    [setActiveConfig],
  );

  // ---- Config CRUD ----

  const createConfig = useCallback(
    async (req: StateMachineConfigCreate) => {
      try {
        const result = await invoke<StateMachineConfig>("sm_create_config", { request: req });
        setConfigs((prev) => [...prev, result]);
        // Auto-load the new config (spread config fields + empty states/transitions)
        setActiveConfig({ ...result, states: [], transitions: [] });
        return result;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
        throw new Error(msg, { cause: err });
      }
    },
    [setActiveConfig],
  );

  const updateConfig = useCallback(async (id: string, req: StateMachineConfigUpdate) => {
    try {
      const result = await invoke<StateMachineConfig>("sm_update_config", { id, request: req });
      setConfigs((prev) => prev.map((c) => (c.id === id ? result : c)));
      setActiveConfigRaw((prev) => (prev && prev.id === id ? { ...prev, ...result } : prev));
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  const deleteConfig = useCallback(async (id: string) => {
    try {
      await invoke("sm_delete_config", { id });
      setConfigs((prev) => prev.filter((c) => c.id !== id));
      setActiveConfigRaw((prev) => {
        if (prev && prev.id === id) {
          try {
            localStorage.removeItem(SM_SELECTED_CONFIG_KEY);
          } catch {
            /* */
          }
          return null;
        }
        return prev;
      });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  // ---- State CRUD ----

  const createState = useCallback(
    async (req: StateMachineStateCreate) => {
      if (!activeConfig) throw new Error("No active config");
      try {
        const result = await invoke<StateMachineState>("sm_create_state", {
          configId: activeConfig.id,
          request: req,
        });
        setActiveConfigRaw((prev) => (prev ? { ...prev, states: [...prev.states, result] } : prev));
        return result;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
        throw new Error(msg, { cause: err });
      }
    },
    [activeConfig],
  );

  const updateState = useCallback(async (id: string, req: StateMachineStateUpdate) => {
    try {
      const result = await invoke<StateMachineState>("sm_update_state", { id, request: req });
      setActiveConfigRaw((prev) =>
        prev ? { ...prev, states: prev.states.map((s) => (s.id === id ? result : s)) } : prev,
      );
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  const deleteState = useCallback(async (id: string) => {
    try {
      await invoke("sm_delete_state", { id });
      setActiveConfigRaw((prev) =>
        prev ? { ...prev, states: prev.states.filter((s) => s.id !== id) } : prev,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  // ---- Transition CRUD ----

  const createTransition = useCallback(
    async (req: StateMachineTransitionCreate) => {
      if (!activeConfig) throw new Error("No active config");
      try {
        const result = await invoke<StateMachineTransition>("sm_create_transition", {
          configId: activeConfig.id,
          request: req,
        });
        setActiveConfigRaw((prev) =>
          prev ? { ...prev, transitions: [...prev.transitions, result] } : prev,
        );
        return result;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
        throw new Error(msg, { cause: err });
      }
    },
    [activeConfig],
  );

  const updateTransition = useCallback(async (id: string, req: StateMachineTransitionUpdate) => {
    try {
      const result = await invoke<StateMachineTransition>("sm_update_transition", {
        id,
        request: req,
      });
      setActiveConfigRaw((prev) =>
        prev
          ? {
              ...prev,
              transitions: prev.transitions.map((t) => (t.id === id ? result : t)),
            }
          : prev,
      );
      return result;
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  const deleteTransition = useCallback(async (id: string) => {
    try {
      await invoke("sm_delete_transition", { id });
      setActiveConfigRaw((prev) =>
        prev ? { ...prev, transitions: prev.transitions.filter((t) => t.id !== id) } : prev,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      throw new Error(msg, { cause: err });
    }
  }, []);

  // ---- Import ----

  const importConfig = useCallback(
    async (name: string, data: StateMachineExportFormat) => {
      try {
        const result = await invoke<StateMachineConfig>("sm_import_config", {
          request: { name, config: data },
        });
        // Reload the list and load the imported config
        await loadConfigs();
        await loadConfig(result.id);
        return result;
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setError(msg);
        throw new Error(msg, { cause: err });
      }
    },
    [loadConfigs, loadConfig],
  );

  // Load configs on mount, then auto-select the last used or most recent config
  useEffect(() => {
    let cancelled = false;
    (async () => {
      await loadConfigs();
    })().then(async () => {
      if (cancelled) return;
      // After loadConfigs sets state, we need to read the fresh list.
      // Because setConfigs is async React state, read from the IPC result instead.
      try {
        const freshConfigs = await invoke<StateMachineConfig[]>("sm_list_configs");
        if (cancelled || freshConfigs.length === 0) return;

        // Try to restore persisted selection
        let targetId: string | null = null;
        try {
          targetId = localStorage.getItem(SM_SELECTED_CONFIG_KEY);
        } catch {
          /* */
        }

        // Validate persisted ID still exists, otherwise pick most recent
        if (targetId && freshConfigs.some((c) => c.id === targetId)) {
          await loadConfig(targetId);
        } else {
          // Sort by created_at descending, pick the latest
          const sorted = [...freshConfigs].sort(
            (a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
          );
          await loadConfig(sorted[0]!.id);
        }
      } catch {
        // Non-critical: if auto-select fails the user can still pick manually
      }
    });
    return () => {
      cancelled = true;
    };
  }, [loadConfigs, loadConfig]);

  return {
    configs,
    loadConfigs,
    activeConfig,
    loadConfig,
    setActiveConfig,
    createConfig,
    updateConfig,
    deleteConfig,
    createState,
    updateState,
    deleteState,
    createTransition,
    updateTransition,
    deleteTransition,
    importConfig,
    isLoading,
    error,
  };
}
