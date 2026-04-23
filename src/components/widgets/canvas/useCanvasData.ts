/**
 * useCanvasData Hook
 *
 * Fetches and manages data for the Canvas widget.
 * Subscribes to "canvas-update" Tauri events for real-time updates
 * and polls GET /canvas/panels on mount for reconnection.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CanvasData, CanvasPanel, CanvasUpdateEvent } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

const POLL_INTERVAL_MS = 5000;

/**
 * Hook that provides canvas panel data for the widget.
 */
export function useCanvasData(): CanvasData {
  const [panelMap, setPanelMap] = useState<Map<string, CanvasPanel>>(new Map());
  const [isLoading, setIsLoading] = useState(true);
  const [error] = useState<string | null>(null);

  // Fetch all panels from API (for reconnection/initial load)
  const fetchPanels = useCallback(async () => {
    try {
      const response = await tracedFetch(`${getApiBase()}/canvas/panels`);
      if (response.ok) {
        const result = await response.json();
        if (result.success && result.data) {
          const newMap = new Map<string, CanvasPanel>();
          for (const panel of result.data as CanvasPanel[]) {
            newMap.set(panel.panel_id, panel);
          }
          setPanelMap(newMap);
        }
      }
    } catch {
      // API may not be available — silently ignore
    } finally {
      setIsLoading(false);
    }
  }, []);

  // Initial fetch and polling. Wrap fetchPanels in async IIFE so its
  // synchronous setIsLoading(false) / setPanelMap isn't the first statement
  // reached from the effect body (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await fetchPanels();
    })();
    const interval = setInterval(fetchPanels, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [fetchPanels]);

  // Subscribe to Tauri canvas-update events for real-time updates
  useEffect(() => {
    const unlisten = listen<CanvasUpdateEvent>("canvas-update", (event) => {
      const update = event.payload;

      // The payload may be wrapped in { event_type, data }
      const raw = update as unknown as Record<string, unknown>;
      const data = raw.data
        ? (raw.data as unknown as CanvasUpdateEvent)
        : (update as CanvasUpdateEvent);

      setPanelMap((prev) => {
        const next = new Map(prev);

        switch (data.action) {
          case "create":
          case "update":
            if (data.panel) {
              next.set(data.panel_id, data.panel);
            }
            break;
          case "delete":
            next.delete(data.panel_id);
            break;
          case "clear":
            next.clear();
            break;
        }

        return next;
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Sort panels by priority
  const panels = useMemo(() => {
    const sorted = Array.from(panelMap.values());
    sorted.sort((a, b) => (a.priority ?? 50) - (b.priority ?? 50));
    return sorted;
  }, [panelMap]);

  return {
    panels,
    panelCount: panels.length,
    isLoading,
    error,
  };
}

export default useCanvasData;
