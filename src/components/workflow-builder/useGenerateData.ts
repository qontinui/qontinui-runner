/**
 * useGenerateData.ts
 *
 * Hook for fetching and managing saved prompts, contexts, and AI settings
 * used by the AiGeneratePanel.
 */

import { useState, useEffect, useMemo, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// =============================================================================
// Types
// =============================================================================

export interface SavedContext {
  id: string;
  name: string;
  scope: string;
  category?: string;
  content?: string;
}

export interface SavedPrompt {
  id: string;
  name: string;
  content: string;
  category?: string;
  tags?: string[];
}

// =============================================================================
// Hook
// =============================================================================

export function useGenerateData() {
  // Saved prompts (generation category)
  const [savedPrompts, setSavedPrompts] = useState<SavedPrompt[]>([]);

  // Saved contexts
  const [savedContexts, setSavedContexts] = useState<SavedContext[]>([]);

  // AI settings (for provider/model defaults)
  const [aiSettings, setAiSettings] = useState<Record<string, unknown> | null>(null);

  // Derived: filter prompts by Generation category
  const generationPrompts = useMemo(
    () => savedPrompts.filter((p) => p.category === "Generation"),
    [savedPrompts],
  );

  // Derived: group contexts by scope
  const contextsByScope = useMemo(
    () =>
      savedContexts.reduce(
        (acc, ctx) => {
          const scope = ctx.scope || "user";
          if (!acc[scope]) acc[scope] = [];
          acc[scope].push(ctx);
          return acc;
        },
        {} as Record<string, SavedContext[]>,
      ),
    [savedContexts],
  );

  // Fetch prompts and contexts on mount
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/prompts`, { signal: controller.signal });
        const json = await resp.json();
        if (!cancelled) setSavedPrompts(json.data ?? []);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();
    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/contexts`, { signal: controller.signal });
        const json = await resp.json();
        if (!cancelled) setSavedContexts(json.data ?? []);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Fetch AI settings on mount
  useEffect(() => {
    const controller = new AbortController();
    let cancelled = false;

    (async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/settings/ai`, {
          signal: controller.signal,
        });
        const json = await resp.json();
        if (!cancelled) setAiSettings(json.data ?? null);
      } catch {
        // Runner may not be running or request was aborted
      }
    })();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, []);

  // Save current description as a template prompt
  const handleSaveAsTemplate = useCallback(async (description: string) => {
    const trimmed = description.trim();
    if (!trimmed) return;
    try {
      const name = trimmed.length > 60 ? trimmed.substring(0, 57) + "..." : trimmed;
      await tracedFetch(`${getApiBase()}/prompts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          content: trimmed,
          category: "Generation",
          description: "",
          tags: ["user-template"],
        }),
      });
      // Refresh prompts
      const resp = await tracedFetch(`${getApiBase()}/prompts`);
      const json = await resp.json();
      setSavedPrompts(json.data ?? []);
    } catch {
      // Failed to save
    }
  }, []);

  // Delete a saved template prompt
  const handleDeleteSavedTemplate = useCallback(async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await tracedFetch(`${getApiBase()}/prompts/${id}`, { method: "DELETE" });
      setSavedPrompts((prev) => prev.filter((p) => p.id !== id));
    } catch {
      // Failed to delete
    }
  }, []);

  // Refresh contexts from the API (e.g., after importing a file)
  const refreshContexts = useCallback(async () => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/contexts`);
      const json = await resp.json();
      setSavedContexts(json.data ?? []);
    } catch {
      // Failed to refresh
    }
  }, []);

  return {
    savedPrompts,
    savedContexts,
    aiSettings,
    generationPrompts,
    contextsByScope,
    handleSaveAsTemplate,
    handleDeleteSavedTemplate,
    refreshContexts,
  };
}
