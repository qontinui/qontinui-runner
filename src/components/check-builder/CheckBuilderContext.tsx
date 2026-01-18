/**
 * Check Builder Context
 *
 * Provides state management for the check builder.
 */

import { createContext, useContext, useState, useCallback, useEffect, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Check, CheckExecutionResult, CreateCheckInput, UpdateCheckInput } from "./types";

interface CheckBuilderState {
  checks: Check[];
  selectedCheckId: string | null;
  isLoading: boolean;
  isSaving: boolean;
  isDirty: boolean;
  lastExecutionResult: CheckExecutionResult | null;
  error: string | null;
}

interface CheckBuilderContextValue extends CheckBuilderState {
  // Selection
  selectCheck: (id: string | null) => void;
  selectedCheck: Check | null;

  // CRUD
  loadChecks: () => Promise<void>;
  createCheck: (input: CreateCheckInput) => Promise<Check | null>;
  updateCheck: (id: string, input: UpdateCheckInput) => Promise<Check | null>;
  deleteCheck: (id: string) => Promise<boolean>;

  // Execution
  executeCheck: (check: Check) => Promise<CheckExecutionResult | null>;

  // State
  setDirty: (dirty: boolean) => void;
  clearError: () => void;
}

const CheckBuilderContext = createContext<CheckBuilderContextValue | null>(null);

interface CheckBuilderProviderProps {
  children: ReactNode;
  onLog?: (level: string, message: string) => void;
}

export function CheckBuilderProvider({ children, onLog }: CheckBuilderProviderProps) {
  const [state, setState] = useState<CheckBuilderState>({
    checks: [],
    selectedCheckId: null,
    isLoading: false,
    isSaving: false,
    isDirty: false,
    lastExecutionResult: null,
    error: null,
  });

  const log = useCallback(
    (level: string, message: string) => {
      console.log(`[CheckBuilder] ${level}: ${message}`);
      onLog?.(level, message);
    },
    [onLog],
  );

  // Load checks from backend
  const loadChecks = useCallback(async () => {
    setState((s) => ({ ...s, isLoading: true, error: null }));
    try {
      const response = await invoke<{ success: boolean; data?: Check[]; message?: string }>(
        "list_checks",
        { enabledOnly: false },
      );
      if (response.success && response.data) {
        setState((s) => ({ ...s, checks: response.data!, isLoading: false }));
        log("info", `Loaded ${response.data.length} checks`);
      } else {
        setState((s) => ({
          ...s,
          isLoading: false,
          error: response.message || "Failed to load checks",
        }));
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, isLoading: false, error: message }));
      log("error", `Failed to load checks: ${message}`);
    }
  }, [log]);

  // Load checks on mount
  useEffect(() => {
    loadChecks();
  }, [loadChecks]);

  // Select a check
  const selectCheck = useCallback((id: string | null) => {
    setState((s) => ({
      ...s,
      selectedCheckId: id,
      isDirty: false,
      lastExecutionResult: null,
    }));
  }, []);

  // Get selected check
  const selectedCheck = state.selectedCheckId
    ? (state.checks.find((c) => c.id === state.selectedCheckId) ?? null)
    : null;

  // Create a new check
  const createCheck = useCallback(
    async (input: CreateCheckInput): Promise<Check | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: Check; message?: string }>(
          "create_check",
          { input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            checks: [response.data!, ...s.checks],
            selectedCheckId: response.data!.id,
            isSaving: false,
            isDirty: false,
          }));
          log("info", `Created check: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to create check",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to create check: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Update an existing check
  const updateCheck = useCallback(
    async (id: string, input: UpdateCheckInput): Promise<Check | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: Check; message?: string }>(
          "update_check",
          { id, input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            checks: s.checks.map((c) => (c.id === id ? response.data! : c)),
            isSaving: false,
            isDirty: false,
          }));
          log("info", `Updated check: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to update check",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to update check: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Delete a check
  const deleteCheck = useCallback(
    async (id: string): Promise<boolean> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; message?: string }>("delete_check", {
          id,
        });
        if (response.success) {
          setState((s) => ({
            ...s,
            checks: s.checks.filter((c) => c.id !== id),
            selectedCheckId: s.selectedCheckId === id ? null : s.selectedCheckId,
            isSaving: false,
          }));
          log("info", `Deleted check: ${id}`);
          return true;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to delete check",
          }));
          return false;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to delete check: ${message}`);
        return false;
      }
    },
    [log],
  );

  // Execute a check
  const executeCheck = useCallback(
    async (check: Check): Promise<CheckExecutionResult | null> => {
      log("info", `Executing check: ${check.name}`);
      try {
        const checkDefinition = {
          id: check.id,
          name: check.name,
          check_type: check.check_type,
          tool: check.tool,
          command: check.command,
          working_directory: check.working_directory,
          config_path: check.config_path,
          auto_fix: check.auto_fix,
          fail_on_warning: check.fail_on_warning,
          timeout_seconds: check.timeout_seconds,
          is_critical: check.is_critical,
        };

        const response = await invoke<{
          success: boolean;
          result: CheckExecutionResult;
        }>("execute_code_check", { checkDefinition });

        setState((s) => ({ ...s, lastExecutionResult: response.result }));

        if (response.success) {
          log("info", `Check passed: ${check.name}`);
        } else {
          log("warn", `Check failed: ${check.name} (${response.result.issues_found} issues)`);
        }

        return response.result;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        log("error", `Failed to execute check: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Set dirty state
  const setDirty = useCallback((dirty: boolean) => {
    setState((s) => ({ ...s, isDirty: dirty }));
  }, []);

  // Clear error
  const clearError = useCallback(() => {
    setState((s) => ({ ...s, error: null }));
  }, []);

  const value: CheckBuilderContextValue = {
    ...state,
    selectCheck,
    selectedCheck,
    loadChecks,
    createCheck,
    updateCheck,
    deleteCheck,
    executeCheck,
    setDirty,
    clearError,
  };

  return <CheckBuilderContext.Provider value={value}>{children}</CheckBuilderContext.Provider>;
}

export function useCheckBuilder() {
  const context = useContext(CheckBuilderContext);
  if (!context) {
    throw new Error("useCheckBuilder must be used within a CheckBuilderProvider");
  }
  return context;
}
