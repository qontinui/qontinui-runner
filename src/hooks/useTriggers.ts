import { useCallback, useEffect, useState } from "react";
import type {
  WorkflowTrigger,
  TriggerHistoryEntry,
  TriggerSystemStatus,
  CreateTriggerRequest,
  UpdateTriggerRequest,
} from "../types/triggers";

const API_BASE = "http://localhost:9876";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

async function apiRequest<T>(path: string, options?: RequestInit): Promise<T | null> {
  try {
    const response = await fetch(`${API_BASE}${path}`, {
      headers: { "Content-Type": "application/json" },
      ...options,
    });
    const json: ApiResponse<T> = await response.json();
    if (json.success && json.data !== undefined) {
      return json.data;
    }
    if (json.error) {
      console.error(`API error: ${json.error}`);
    }
    return null;
  } catch (err) {
    console.error(`API request failed: ${path}`, err);
    return null;
  }
}

export interface UseTriggersReturn {
  // State
  triggers: WorkflowTrigger[];
  selectedTrigger: WorkflowTrigger | null;
  triggerHistory: TriggerHistoryEntry[];
  status: TriggerSystemStatus | null;
  loading: boolean;
  error: string | null;

  // Actions
  loadTriggers: () => Promise<void>;
  loadStatus: () => Promise<void>;
  createTrigger: (request: CreateTriggerRequest) => Promise<WorkflowTrigger | null>;
  updateTrigger: (id: string, request: UpdateTriggerRequest) => Promise<WorkflowTrigger | null>;
  deleteTrigger: (id: string) => Promise<boolean>;
  setEnabled: (id: string, enabled: boolean) => Promise<boolean>;
  testTrigger: (id: string) => Promise<Record<string, unknown> | null>;
  loadHistory: (id: string) => Promise<void>;
  selectTrigger: (trigger: WorkflowTrigger | null) => void;
  refresh: () => Promise<void>;
}

export function useTriggers(autoRefreshMs = 30000): UseTriggersReturn {
  const [triggers, setTriggers] = useState<WorkflowTrigger[]>([]);
  const [selectedTrigger, setSelectedTrigger] = useState<WorkflowTrigger | null>(null);
  const [triggerHistory, setTriggerHistory] = useState<TriggerHistoryEntry[]>([]);
  const [status, setStatus] = useState<TriggerSystemStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadTriggers = useCallback(async () => {
    const data = await apiRequest<WorkflowTrigger[]>("/triggers");
    if (data) {
      setTriggers(data);
      setError(null);
    }
  }, []);

  const loadStatus = useCallback(async () => {
    const data = await apiRequest<TriggerSystemStatus>("/triggers/status");
    if (data) {
      setStatus(data);
    }
  }, []);

  const createTrigger = useCallback(
    async (request: CreateTriggerRequest): Promise<WorkflowTrigger | null> => {
      const data = await apiRequest<WorkflowTrigger>("/triggers", {
        method: "POST",
        body: JSON.stringify(request),
      });
      if (data) {
        await loadTriggers();
      }
      return data;
    },
    [loadTriggers],
  );

  const updateTrigger = useCallback(
    async (id: string, request: UpdateTriggerRequest): Promise<WorkflowTrigger | null> => {
      const data = await apiRequest<WorkflowTrigger>(`/triggers/${id}`, {
        method: "PUT",
        body: JSON.stringify(request),
      });
      if (data) {
        await loadTriggers();
      }
      return data;
    },
    [loadTriggers],
  );

  const deleteTrigger = useCallback(
    async (id: string): Promise<boolean> => {
      const data = await apiRequest<unknown>(`/triggers/${id}`, {
        method: "DELETE",
      });
      if (data !== null) {
        await loadTriggers();
        if (selectedTrigger?.id === id) {
          setSelectedTrigger(null);
          setTriggerHistory([]);
        }
        return true;
      }
      return false;
    },
    [loadTriggers, selectedTrigger],
  );

  const setEnabled = useCallback(
    async (id: string, enabled: boolean): Promise<boolean> => {
      const data = await apiRequest<unknown>(`/triggers/${id}/enabled`, {
        method: "PUT",
        body: JSON.stringify({ enabled }),
      });
      if (data !== null) {
        await loadTriggers();
        return true;
      }
      return false;
    },
    [loadTriggers],
  );

  const testTrigger = useCallback(async (id: string): Promise<Record<string, unknown> | null> => {
    return apiRequest<Record<string, unknown>>(`/triggers/${id}/test`, {
      method: "POST",
    });
  }, []);

  const loadHistory = useCallback(async (id: string) => {
    const data = await apiRequest<TriggerHistoryEntry[]>(`/triggers/${id}/history`);
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

  return {
    triggers,
    selectedTrigger,
    triggerHistory,
    status,
    loading,
    error,
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
  };
}
