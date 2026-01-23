/**
 * Check Builder Context
 *
 * Provides state management for the check builder.
 */

import { createContext, useContext, useState, useCallback, useEffect, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  Check,
  CheckExecutionResult,
  CheckGroup,
  CheckGroupExecutionResult,
  CreateCheckGroupInput,
  CreateCheckInput,
  UpdateCheckGroupInput,
  UpdateCheckInput,
} from "./types";

interface CheckBuilderState {
  checks: Check[];
  checkGroups: CheckGroup[];
  checkGroupMembers: Record<string, string[]>; // groupId -> checkIds
  selectedCheckId: string | null;
  selectedGroupId: string | null;
  isLoading: boolean;
  isSaving: boolean;
  isDirty: boolean;
  lastExecutionResult: CheckExecutionResult | null;
  lastGroupExecutionResult: CheckGroupExecutionResult | null;
  error: string | null;
}

interface CheckBuilderContextValue extends CheckBuilderState {
  // Selection
  selectCheck: (id: string | null) => void;
  selectGroup: (id: string | null) => void;
  selectedCheck: Check | null;
  selectedGroup: CheckGroup | null;

  // Check CRUD
  loadChecks: () => Promise<void>;
  createCheck: (input: CreateCheckInput) => Promise<Check | null>;
  updateCheck: (id: string, input: UpdateCheckInput) => Promise<Check | null>;
  deleteCheck: (id: string) => Promise<boolean>;

  // Check Group CRUD
  loadCheckGroups: () => Promise<void>;
  createCheckGroup: (input: CreateCheckGroupInput) => Promise<CheckGroup | null>;
  updateCheckGroup: (id: string, input: UpdateCheckGroupInput) => Promise<CheckGroup | null>;
  deleteCheckGroup: (id: string) => Promise<boolean>;

  // Check Group Membership
  getChecksInGroup: (groupId: string) => Check[];
  setChecksInGroup: (groupId: string, checkIds: string[]) => Promise<boolean>;
  addCheckToGroup: (groupId: string, checkId: string) => Promise<boolean>;
  removeCheckFromGroup: (groupId: string, checkId: string) => Promise<boolean>;

  // Execution
  executeCheck: (check: Check) => Promise<CheckExecutionResult | null>;
  executeCheckGroup: (groupId: string) => Promise<CheckGroupExecutionResult | null>;

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
    checkGroups: [],
    checkGroupMembers: {},
    selectedCheckId: null,
    selectedGroupId: null,
    isLoading: false,
    isSaving: false,
    isDirty: false,
    lastExecutionResult: null,
    lastGroupExecutionResult: null,
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

  // ============================================================================
  // Check Group Methods
  // ============================================================================

  // Select a group
  const selectGroup = useCallback((id: string | null) => {
    setState((s) => ({
      ...s,
      selectedGroupId: id,
      lastGroupExecutionResult: null,
    }));
  }, []);

  // Get selected group
  const selectedGroup = state.selectedGroupId
    ? (state.checkGroups.find((g) => g.id === state.selectedGroupId) ?? null)
    : null;

  // Load check groups from backend
  const loadCheckGroups = useCallback(async () => {
    try {
      const response = await invoke<{ success: boolean; data?: CheckGroup[]; message?: string }>(
        "list_check_groups",
        { enabledOnly: false },
      );
      if (response.success && response.data) {
        // Load member info for each group
        const memberPromises = response.data.map(async (group) => {
          try {
            const memberResponse = await invoke<{ success: boolean; data?: Check[]; message?: string }>(
              "get_checks_in_group",
              { groupId: group.id },
            );
            return {
              groupId: group.id,
              checkIds: memberResponse.success && memberResponse.data
                ? memberResponse.data.map((c) => c.id)
                : [],
            };
          } catch {
            return { groupId: group.id, checkIds: [] };
          }
        });
        const memberResults = await Promise.all(memberPromises);
        const members: Record<string, string[]> = {};
        memberResults.forEach((r) => {
          members[r.groupId] = r.checkIds;
        });

        setState((s) => ({
          ...s,
          checkGroups: response.data!,
          checkGroupMembers: members,
        }));
        log("info", `Loaded ${response.data.length} check groups`);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      log("error", `Failed to load check groups: ${message}`);
    }
  }, [log]);

  // Load groups on mount (alongside checks)
  useEffect(() => {
    loadCheckGroups();
  }, [loadCheckGroups]);

  // Create a new check group
  const createCheckGroup = useCallback(
    async (input: CreateCheckGroupInput): Promise<CheckGroup | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: CheckGroup; message?: string }>(
          "create_check_group",
          { input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            checkGroups: [response.data!, ...s.checkGroups],
            checkGroupMembers: { ...s.checkGroupMembers, [response.data!.id]: [] },
            selectedGroupId: response.data!.id,
            isSaving: false,
          }));
          log("info", `Created check group: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to create check group",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to create check group: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Update an existing check group
  const updateCheckGroup = useCallback(
    async (id: string, input: UpdateCheckGroupInput): Promise<CheckGroup | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: CheckGroup; message?: string }>(
          "update_check_group",
          { id, input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            checkGroups: s.checkGroups.map((g) => (g.id === id ? response.data! : g)),
            isSaving: false,
          }));
          log("info", `Updated check group: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to update check group",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to update check group: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Delete a check group
  const deleteCheckGroup = useCallback(
    async (id: string): Promise<boolean> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; message?: string }>("delete_check_group", {
          id,
        });
        if (response.success) {
          setState((s) => {
            const newMembers = { ...s.checkGroupMembers };
            delete newMembers[id];
            return {
              ...s,
              checkGroups: s.checkGroups.filter((g) => g.id !== id),
              checkGroupMembers: newMembers,
              selectedGroupId: s.selectedGroupId === id ? null : s.selectedGroupId,
              isSaving: false,
            };
          });
          log("info", `Deleted check group: ${id}`);
          return true;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to delete check group",
          }));
          return false;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to delete check group: ${message}`);
        return false;
      }
    },
    [log],
  );

  // Get checks in a group (from local state)
  const getChecksInGroup = useCallback(
    (groupId: string): Check[] => {
      const checkIds = state.checkGroupMembers[groupId] || [];
      return checkIds
        .map((id) => state.checks.find((c) => c.id === id))
        .filter((c): c is Check => c !== undefined);
    },
    [state.checkGroupMembers, state.checks],
  );

  // Set checks in a group
  const setChecksInGroup = useCallback(
    async (groupId: string, checkIds: string[]): Promise<boolean> => {
      try {
        const response = await invoke<{ success: boolean; message?: string }>(
          "set_checks_in_group",
          { groupId, checkIds },
        );
        if (response.success) {
          setState((s) => ({
            ...s,
            checkGroupMembers: { ...s.checkGroupMembers, [groupId]: checkIds },
          }));
          log("info", `Set ${checkIds.length} checks in group ${groupId}`);
          return true;
        }
        return false;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        log("error", `Failed to set checks in group: ${message}`);
        return false;
      }
    },
    [log],
  );

  // Add a check to a group
  const addCheckToGroup = useCallback(
    async (groupId: string, checkId: string): Promise<boolean> => {
      const currentChecks = state.checkGroupMembers[groupId] || [];
      if (currentChecks.includes(checkId)) return true; // Already in group
      return setChecksInGroup(groupId, [...currentChecks, checkId]);
    },
    [state.checkGroupMembers, setChecksInGroup],
  );

  // Remove a check from a group
  const removeCheckFromGroup = useCallback(
    async (groupId: string, checkId: string): Promise<boolean> => {
      const currentChecks = state.checkGroupMembers[groupId] || [];
      return setChecksInGroup(groupId, currentChecks.filter((id) => id !== checkId));
    },
    [state.checkGroupMembers, setChecksInGroup],
  );

  // Execute all checks in a group
  const executeCheckGroup = useCallback(
    async (groupId: string): Promise<CheckGroupExecutionResult | null> => {
      const group = state.checkGroups.find((g) => g.id === groupId);
      if (!group) {
        log("error", `Check group not found: ${groupId}`);
        return null;
      }

      log("info", `Executing check group: ${group.name}`);
      try {
        const response = await invoke<{
          success: boolean;
          data?: CheckGroupExecutionResult;
          message?: string;
        }>("execute_check_group", { groupId });

        if (response.success && response.data) {
          setState((s) => ({ ...s, lastGroupExecutionResult: response.data! }));
          log(
            "info",
            `Check group ${group.name} completed: ${response.data.summary.passed}/${response.data.summary.total} passed`,
          );
          return response.data;
        } else {
          log("error", `Failed to execute check group: ${response.message}`);
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        log("error", `Failed to execute check group: ${message}`);
        return null;
      }
    },
    [state.checkGroups, log],
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
    // Selection
    selectCheck,
    selectGroup,
    selectedCheck,
    selectedGroup,
    // Check CRUD
    loadChecks,
    createCheck,
    updateCheck,
    deleteCheck,
    // Check Group CRUD
    loadCheckGroups,
    createCheckGroup,
    updateCheckGroup,
    deleteCheckGroup,
    // Check Group Membership
    getChecksInGroup,
    setChecksInGroup,
    addCheckToGroup,
    removeCheckFromGroup,
    // Execution
    executeCheck,
    executeCheckGroup,
    // State
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
