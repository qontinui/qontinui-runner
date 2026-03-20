/**
 * useAIOperations — AI search and execute sub-hook.
 *
 * Stateless API calls for AI-powered element search and instruction execution.
 */

import { useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseAIOperationsReturn {
  aiSearch: (query: string) => Promise<unknown>;
  aiExecute: (instruction: string) => Promise<unknown>;
}

export function useAIOperations(): UseAIOperationsReturn {
  const aiSearch = useCallback(async (query: string): Promise<unknown> => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/ai/search`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query }),
      });
      return await resp.json();
    } catch (e) {
      return { success: false, error: e instanceof Error ? e.message : "AI search failed" };
    }
  }, []);

  const aiExecute = useCallback(async (instruction: string): Promise<unknown> => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/ai/execute`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ instruction }),
      });
      return await resp.json();
    } catch (e) {
      return { success: false, error: e instanceof Error ? e.message : "AI execute failed" };
    }
  }, []);

  return { aiSearch, aiExecute };
}
