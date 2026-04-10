/**
 * useProviderCircuitBreakers Hook
 *
 * Subscribes to real-time circuit breaker state-change events from the Rust
 * backend via Tauri events, with an initial fetch for the current snapshot.
 * Replaces the former 5-second polling approach.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProviderCircuitState } from "@/components/settings/ai-settings/types";

interface UseProviderCircuitBreakersResult {
  states: ProviderCircuitState[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  resetProvider: (providerKey: string) => Promise<void>;
}

export function useProviderCircuitBreakers(): UseProviderCircuitBreakersResult {
  const [states, setStates] = useState<ProviderCircuitState[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const result = await invoke<ProviderCircuitState[]>("get_provider_circuit_states");
      setStates(result);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // Initial fetch for current snapshot
    refresh();

    // Subscribe to real-time circuit breaker state-change events
    const unlisten = listen<ProviderCircuitState>("circuit-breaker-change", (event) => {
      setStates((prev) => {
        const idx = prev.findIndex((s) => s.provider_key === event.payload.provider_key);
        if (idx >= 0) {
          const updated = [...prev];
          updated[idx] = event.payload;
          return updated;
        }
        return [...prev, event.payload];
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);

  const resetProvider = useCallback(async (providerKey: string) => {
    await invoke("reset_provider_circuit", { providerKey });
    // The reset will emit an event that updates the state automatically
  }, []);

  return { states, loading, error, refresh, resetProvider };
}
