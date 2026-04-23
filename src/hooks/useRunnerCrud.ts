/**
 * useRunnerCrud
 *
 * Generic CRUD hook for any resource path against the runner API.
 * Provides standard list, create, update, delete operations plus
 * a runAction helper for triggering resource-specific actions.
 */

import { useState, useEffect, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface UseRunnerCrudOptions {
  resourcePath: string;
  autoLoad?: boolean;
}

export interface UseRunnerCrudReturn<T> {
  items: T[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  create: (data: Partial<T>) => Promise<T | null>;
  update: (id: string, data: Partial<T>) => Promise<T | null>;
  remove: (id: string) => Promise<boolean>;
  runAction: (id: string, action: string) => Promise<unknown>;
}

/**
 * Make an API request to the runner backend.
 */
async function apiRequest<T>(method: string, endpoint: string, body?: unknown): Promise<T | null> {
  const options: RequestInit = {
    method,
    headers: { "Content-Type": "application/json" },
  };

  if (body) {
    options.body = JSON.stringify(body);
  }

  const response = await tracedFetch(`${getApiBase()}${endpoint}`, options);
  const result: ApiResponse<T> = await response.json();

  if (!result.success) {
    throw new Error(result.error || "API request failed");
  }

  return result.data ?? null;
}

/**
 * Generic CRUD hook for runner resources.
 */
export function useRunnerCrud<T extends { id: string }>(
  options: UseRunnerCrudOptions,
): UseRunnerCrudReturn<T> {
  const { resourcePath, autoLoad = true } = options;

  const [items, setItems] = useState<T[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await apiRequest<T[]>("GET", resourcePath);
      setItems(result || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load items");
    } finally {
      setLoading(false);
    }
  }, [resourcePath]);

  const create = useCallback(
    async (data: Partial<T>): Promise<T | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<T>("POST", resourcePath, data);
        if (result) {
          setItems((prev) => [...prev, result]);
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to create item");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [resourcePath],
  );

  const update = useCallback(
    async (id: string, data: Partial<T>): Promise<T | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<T>("PUT", `${resourcePath}/${id}`, data);
        if (result) {
          setItems((prev) => prev.map((item) => (item.id === id ? result : item)));
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to update item");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [resourcePath],
  );

  const remove = useCallback(
    async (id: string): Promise<boolean> => {
      setLoading(true);
      setError(null);
      try {
        await apiRequest("DELETE", `${resourcePath}/${id}`);
        setItems((prev) => prev.filter((item) => item.id !== id));
        return true;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to delete item");
        return false;
      } finally {
        setLoading(false);
      }
    },
    [resourcePath],
  );

  const runAction = useCallback(
    async (id: string, action: string): Promise<unknown> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<unknown>("POST", `${resourcePath}/${id}/${action}`);
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : `Failed to run ${action}`);
        return null;
      } finally {
        setLoading(false);
      }
    },
    [resourcePath],
  );

  // Initial auto-load. Wrap refresh() in async IIFE so its synchronous
  // setState calls aren't the first statement reached from the effect body
  // (react-hooks/set-state-in-effect).
  useEffect(() => {
    if (!autoLoad) return;
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await refresh();
    })();
    return () => {
      cancelled = true;
    };
  }, [autoLoad, refresh]);

  return {
    items,
    loading,
    error,
    refresh,
    create,
    update,
    remove,
    runAction,
  };
}
