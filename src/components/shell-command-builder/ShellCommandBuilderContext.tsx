/**
 * Shell Command Builder Context
 *
 * Provides state management for the shell command builder.
 */

import { createContext, useContext, useState, useCallback, useEffect, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ShellCommand,
  ExecuteShellCommandResult,
  CreateShellCommandInput,
  UpdateShellCommandInput,
} from "./types";

interface ShellCommandBuilderState {
  shellCommands: ShellCommand[];
  selectedCommandId: string | null;
  isLoading: boolean;
  isSaving: boolean;
  isDirty: boolean;
  isExecuting: boolean;
  lastExecutionResult: ExecuteShellCommandResult | null;
  error: string | null;
}

interface ShellCommandBuilderContextValue extends ShellCommandBuilderState {
  // Selection
  selectCommand: (id: string | null) => void;
  selectedCommand: ShellCommand | null;

  // CRUD
  loadCommands: () => Promise<void>;
  createCommand: (input: CreateShellCommandInput) => Promise<ShellCommand | null>;
  updateCommand: (id: string, input: UpdateShellCommandInput) => Promise<ShellCommand | null>;
  deleteCommand: (id: string) => Promise<boolean>;

  // Execution
  executeCommand: (command: ShellCommand) => Promise<ExecuteShellCommandResult | null>;

  // State
  setDirty: (dirty: boolean) => void;
  clearError: () => void;
  clearExecutionResult: () => void;
}

const ShellCommandBuilderContext = createContext<ShellCommandBuilderContextValue | null>(null);

interface ShellCommandBuilderProviderProps {
  children: ReactNode;
  onLog?: (level: string, message: string) => void;
}

export function ShellCommandBuilderProvider({ children, onLog }: ShellCommandBuilderProviderProps) {
  const [state, setState] = useState<ShellCommandBuilderState>({
    shellCommands: [],
    selectedCommandId: null,
    isLoading: false,
    isSaving: false,
    isDirty: false,
    isExecuting: false,
    lastExecutionResult: null,
    error: null,
  });

  const log = useCallback(
    (level: string, message: string) => {
      console.log(`[ShellCommandBuilder] ${level}: ${message}`);
      onLog?.(level, message);
    },
    [onLog],
  );

  // Load shell commands from backend
  const loadCommands = useCallback(async () => {
    setState((s) => ({ ...s, isLoading: true, error: null }));
    try {
      const response = await invoke<{ success: boolean; data?: ShellCommand[]; message?: string }>(
        "list_shell_commands",
        { enabledOnly: false },
      );
      if (response.success && response.data) {
        setState((s) => ({ ...s, shellCommands: response.data!, isLoading: false }));
        log("info", `Loaded ${response.data.length} shell commands`);
      } else {
        setState((s) => ({
          ...s,
          isLoading: false,
          error: response.message || "Failed to load shell commands",
        }));
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setState((s) => ({ ...s, isLoading: false, error: message }));
      log("error", `Failed to load shell commands: ${message}`);
    }
  }, [log]);

  // Load commands on mount
  useEffect(() => {
    loadCommands();
  }, [loadCommands]);

  // Select a command
  const selectCommand = useCallback((id: string | null) => {
    setState((s) => ({
      ...s,
      selectedCommandId: id,
      isDirty: false,
      lastExecutionResult: null,
    }));
  }, []);

  // Get selected command
  const selectedCommand = state.selectedCommandId
    ? (state.shellCommands.find((c) => c.id === state.selectedCommandId) ?? null)
    : null;

  // Create a new shell command
  const createCommand = useCallback(
    async (input: CreateShellCommandInput): Promise<ShellCommand | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: ShellCommand; message?: string }>(
          "create_shell_command",
          { input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            shellCommands: [response.data!, ...s.shellCommands],
            selectedCommandId: response.data!.id,
            isSaving: false,
            isDirty: false,
          }));
          log("info", `Created shell command: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to create shell command",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to create shell command: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Update an existing shell command
  const updateCommand = useCallback(
    async (id: string, input: UpdateShellCommandInput): Promise<ShellCommand | null> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; data?: ShellCommand; message?: string }>(
          "update_shell_command",
          { id, input },
        );
        if (response.success && response.data) {
          setState((s) => ({
            ...s,
            shellCommands: s.shellCommands.map((c) => (c.id === id ? response.data! : c)),
            isSaving: false,
            isDirty: false,
          }));
          log("info", `Updated shell command: ${response.data.name}`);
          return response.data;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to update shell command",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to update shell command: ${message}`);
        return null;
      }
    },
    [log],
  );

  // Delete a shell command
  const deleteCommand = useCallback(
    async (id: string): Promise<boolean> => {
      setState((s) => ({ ...s, isSaving: true, error: null }));
      try {
        const response = await invoke<{ success: boolean; message?: string }>(
          "delete_shell_command",
          {
            id,
          },
        );
        if (response.success) {
          setState((s) => ({
            ...s,
            shellCommands: s.shellCommands.filter((c) => c.id !== id),
            selectedCommandId: s.selectedCommandId === id ? null : s.selectedCommandId,
            isSaving: false,
          }));
          log("info", `Deleted shell command: ${id}`);
          return true;
        } else {
          setState((s) => ({
            ...s,
            isSaving: false,
            error: response.message || "Failed to delete shell command",
          }));
          return false;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isSaving: false, error: message }));
        log("error", `Failed to delete shell command: ${message}`);
        return false;
      }
    },
    [log],
  );

  // Execute a shell command
  const executeCommand = useCallback(
    async (command: ShellCommand): Promise<ExecuteShellCommandResult | null> => {
      log("info", `Executing shell command: ${command.name}`);
      setState((s) => ({ ...s, isExecuting: true, lastExecutionResult: null, error: null }));
      try {
        const response = await invoke<{
          success: boolean;
          result?: ExecuteShellCommandResult;
          message?: string;
        }>("execute_shell_command", {
          id: command.id,
          command: command.command,
          workingDirectory: command.working_directory,
          timeoutSeconds: command.timeout_seconds,
        });

        if (response.result) {
          setState((s) => ({ ...s, lastExecutionResult: response.result!, isExecuting: false }));
          if (response.result.success) {
            log(
              "info",
              `Command completed successfully: ${command.name} (${response.result.duration_ms}ms)`,
            );
          } else {
            log(
              "warn",
              `Command failed: ${command.name} (exit code: ${response.result.exit_code})`,
            );
          }
          return response.result;
        } else {
          setState((s) => ({
            ...s,
            isExecuting: false,
            error: response.message || "Failed to execute command",
          }));
          return null;
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setState((s) => ({ ...s, isExecuting: false, error: message }));
        log("error", `Failed to execute command: ${message}`);
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

  // Clear execution result
  const clearExecutionResult = useCallback(() => {
    setState((s) => ({ ...s, lastExecutionResult: null }));
  }, []);

  const value: ShellCommandBuilderContextValue = {
    ...state,
    selectCommand,
    selectedCommand,
    loadCommands,
    createCommand,
    updateCommand,
    deleteCommand,
    executeCommand,
    setDirty,
    clearError,
    clearExecutionResult,
  };

  return (
    <ShellCommandBuilderContext.Provider value={value}>
      {children}
    </ShellCommandBuilderContext.Provider>
  );
}

export function useShellCommandBuilder() {
  const context = useContext(ShellCommandBuilderContext);
  if (!context) {
    throw new Error("useShellCommandBuilder must be used within a ShellCommandBuilderProvider");
  }
  return context;
}
