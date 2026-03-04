import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  WorkflowTrigger,
  TriggerHistoryEntry,
  TriggerSystemStatus,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from "../types/triggers";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/** Toast notification data */
export interface TriggerToast {
  id: string;
  type: "success" | "error" | "info";
  message: string;
}

async function apiRequest<T>(
  path: string,
  options?: RequestInit,
): Promise<{ data: T | null; error: string | null }> {
  try {
    const response = await tracedFetch(`${getApiBase()}${path}`, {
      headers: { "Content-Type": "application/json" },
      ...options,
    });
    const json: ApiResponse<T> = await response.json();
    if (json.success && json.data !== undefined) {
      return { data: json.data, error: null };
    }
    const errorMsg = json.error ?? "Unknown error";
    console.error(`API error: ${errorMsg}`);
    return { data: null, error: errorMsg };
  } catch (err) {
    const errorMsg = err instanceof Error ? err.message : "Network error";
    console.error(`API request failed: ${path}`, err);
    return { data: null, error: errorMsg };
  }
}

export interface HistoryFilters {
  action?: string;
  since?: string;
  until?: string;
}

export interface UseTriggersReturn {
  // State
  triggers: WorkflowTrigger[];
  selectedTrigger: WorkflowTrigger | null;
  triggerHistory: TriggerHistoryEntry[];
  status: TriggerSystemStatus | null;
  loading: boolean;
  error: string | null;
  toasts: TriggerToast[];
  operationLoading: Record<string, boolean>;

  // Actions
  loadTriggers: () => Promise<void>;
  loadStatus: () => Promise<void>;
  createTrigger: (request: CreateTriggerRequest) => Promise<WorkflowTrigger | null>;
  updateTrigger: (id: string, request: UpdateTriggerRequest) => Promise<WorkflowTrigger | null>;
  deleteTrigger: (id: string) => Promise<boolean>;
  setEnabled: (id: string, enabled: boolean) => Promise<boolean>;
  testTrigger: (id: string) => Promise<Record<string, unknown> | null>;
  loadHistory: (id: string, filters?: HistoryFilters) => Promise<void>;
  selectTrigger: (trigger: WorkflowTrigger | null) => void;
  refresh: () => Promise<void>;
  dismissToast: (id: string) => void;
}

let toastCounter = 0;

export function useTriggers(autoRefreshMs = 30000): UseTriggersReturn {
  const [triggers, setTriggers] = useState<WorkflowTrigger[]>([]);
  const [selectedTrigger, setSelectedTrigger] = useState<WorkflowTrigger | null>(null);
  const [triggerHistory, setTriggerHistory] = useState<TriggerHistoryEntry[]>([]);
  const [status, setStatus] = useState<TriggerSystemStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [toasts, setToasts] = useState<TriggerToast[]>([]);
  const [operationLoading, setOperationLoading] = useState<Record<string, boolean>>({});

  const addToast = useCallback((type: TriggerToast["type"], message: string) => {
    const id = `toast-${++toastCounter}`;
    setToasts((prev) => [...prev, { id, type, message }]);
    // Auto-dismiss after 4 seconds
    setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== id));
    }, 4000);
  }, []);

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const setOpLoading = useCallback((key: string, value: boolean) => {
    setOperationLoading((prev) => ({ ...prev, [key]: value }));
  }, []);

  const loadTriggers = useCallback(async () => {
    const { data } = await apiRequest<WorkflowTrigger[]>("/triggers");
    if (data) {
      setTriggers(data);
      setError(null);
    }
  }, []);

  const loadStatus = useCallback(async () => {
    const { data } = await apiRequest<TriggerSystemStatus>("/triggers/status");
    if (data) {
      setStatus(data);
    }
  }, []);

  const createTrigger = useCallback(
    async (request: CreateTriggerRequest): Promise<WorkflowTrigger | null> => {
      setOpLoading("create", true);
      const { data, error: err } = await apiRequest<WorkflowTrigger>("/triggers", {
        method: "POST",
        body: JSON.stringify(request),
      });
      setOpLoading("create", false);
      if (data) {
        addToast("success", `Trigger "${data.name}" created`);
        await loadTriggers();
      } else {
        addToast("error", `Failed to create trigger: ${err}`);
      }
      return data;
    },
    [loadTriggers, addToast, setOpLoading],
  );

  const updateTrigger = useCallback(
    async (id: string, request: UpdateTriggerRequest): Promise<WorkflowTrigger | null> => {
      setOpLoading(`update-${id}`, true);
      const { data, error: err } = await apiRequest<WorkflowTrigger>(`/triggers/${id}`, {
        method: "PUT",
        body: JSON.stringify(request),
      });
      setOpLoading(`update-${id}`, false);
      if (data) {
        addToast("success", `Trigger "${data.name}" updated`);
        await loadTriggers();
      } else {
        addToast("error", `Failed to update trigger: ${err}`);
      }
      return data;
    },
    [loadTriggers, addToast, setOpLoading],
  );

  const deleteTrigger = useCallback(
    async (id: string): Promise<boolean> => {
      setOpLoading(`delete-${id}`, true);
      const { data, error: err } = await apiRequest<unknown>(`/triggers/${id}`, {
        method: "DELETE",
      });
      setOpLoading(`delete-${id}`, false);
      if (data !== null) {
        addToast("success", "Trigger deleted");
        await loadTriggers();
        if (selectedTrigger?.id === id) {
          setSelectedTrigger(null);
          setTriggerHistory([]);
        }
        return true;
      }
      addToast("error", `Failed to delete trigger: ${err}`);
      return false;
    },
    [loadTriggers, selectedTrigger, addToast, setOpLoading],
  );

  const setEnabled = useCallback(
    async (id: string, enabled: boolean): Promise<boolean> => {
      setOpLoading(`toggle-${id}`, true);
      const { data, error: err } = await apiRequest<unknown>(`/triggers/${id}/enabled`, {
        method: "PUT",
        body: JSON.stringify({ enabled }),
      });
      setOpLoading(`toggle-${id}`, false);
      if (data !== null) {
        addToast("info", `Trigger ${enabled ? "enabled" : "disabled"}`);
        await loadTriggers();
        return true;
      }
      addToast("error", `Failed to ${enabled ? "enable" : "disable"} trigger: ${err}`);
      return false;
    },
    [loadTriggers, addToast, setOpLoading],
  );

  const testTrigger = useCallback(
    async (id: string): Promise<Record<string, unknown> | null> => {
      setOpLoading(`test-${id}`, true);
      const { data, error: err } = await apiRequest<Record<string, unknown>>(
        `/triggers/${id}/test`,
        {
          method: "POST",
        },
      );
      setOpLoading(`test-${id}`, false);
      if (!data) {
        addToast("error", `Test failed: ${err}`);
      }
      return data;
    },
    [addToast, setOpLoading],
  );

  const loadHistory = useCallback(async (id: string, filters?: HistoryFilters) => {
    const params = new URLSearchParams();
    if (filters?.action) params.set("action", filters.action);
    if (filters?.since) params.set("since", filters.since);
    if (filters?.until) params.set("until", filters.until);
    const qs = params.toString();
    const path = `/triggers/${id}/history${qs ? `?${qs}` : ""}`;
    const { data } = await apiRequest<TriggerHistoryEntry[]>(path);
    if (data) {
      setTriggerHistory(data);
    }
  }, []);

  const selectTrigger = useCallback(
    (trigger: WorkflowTrigger | null) => {
      setSelectedTrigger(trigger);
      if (trigger) {
        loadHistory(trigger.id);
      } else {
        setTriggerHistory([]);
      }
    },
    [loadHistory],
  );

  const refresh = useCallback(async () => {
    setLoading(true);
    await Promise.all([loadTriggers(), loadStatus()]);
    setLoading(false);
  }, [loadTriggers, loadStatus]);

  // Initial load
  useEffect(() => {
    refresh();
  }, [refresh]);

  // Auto-refresh
  useEffect(() => {
    if (autoRefreshMs <= 0) return;
    const interval = setInterval(refresh, autoRefreshMs);
    return () => clearInterval(interval);
  }, [autoRefreshMs, refresh]);

  // Real-time Tauri event subscription for immediate updates when triggers fire
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    listen<{ task_run_id: string; status: string }>("task-run-update", (event) => {
      const { status: taskStatus } = event.payload;
      // Refresh triggers when a task run completes/fails (could update trigger_count, last_triggered_at)
      if (taskStatus === "completed" || taskStatus === "failed") {
        refreshRef.current();
      }
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  return {
    triggers,
    selectedTrigger,
    triggerHistory,
    status,
    loading,
    error,
    toasts,
    operationLoading,
    loadTriggers,
    loadStatus,
    createTrigger,
    updateTrigger,
    deleteTrigger,
    setEnabled,
    testTrigger,
    loadHistory,
    selectTrigger,
    refresh,
    dismissToast,
  };
}
