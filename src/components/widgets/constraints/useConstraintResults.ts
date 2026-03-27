/**
 * useConstraintResults Hook
 *
 * Subscribes to "constraint-results" Tauri IPC events and accumulates
 * per-iteration constraint evaluation results. Resets when the task run changes.
 */

import { useState, useEffect, useMemo } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useCurrentTaskRunId } from "@/contexts";
import type { ConstraintResult } from "@qontinui/shared-types/constraints";
import type { ConstraintIterationResults, ConstraintResultsData } from "./types";
import { calculateCounts, DEFAULT_CONSTRAINT_RESULTS_DATA } from "./types";

/**
 * Hook that listens for constraint-results events from the Rust backend
 * and returns accumulated constraint results data for the widget.
 */
export function useConstraintResults(): ConstraintResultsData {
  const taskRunId = useCurrentTaskRunId();
  const [iterations, setIterations] = useState<ConstraintIterationResults[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    // Reset iterations when task run changes
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription callback
    setIterations([]);

    const setup = async () => {
      unlisten = await listen(
        "constraint-results",
        (event: { payload: Record<string, unknown> }) => {
          const data = event.payload;
          // The AppEvent is serialized with serde tag="event_type", content="data"
          const inner = (data as Record<string, unknown>).data as
            | Record<string, unknown>
            | undefined;
          const eventData = inner || data;
          const eventTaskRunId = eventData.task_run_id as string;

          // Filter to current task run
          if (taskRunId && eventTaskRunId !== taskRunId) return;

          const iteration = eventData.iteration as number;
          const summary = eventData.summary as string;
          const hasBlocking = eventData.has_blocking as boolean;
          const rawResults = eventData.results as ConstraintResult[];

          const iterationResult: ConstraintIterationResults = {
            taskRunId: eventTaskRunId,
            iteration,
            summary,
            hasBlocking,
            results: Array.isArray(rawResults) ? rawResults : [],
            receivedAt: Date.now(),
          };

          setIterations((prev) => {
            // Replace if same iteration exists (e.g., constraint retry), otherwise append
            const existing = prev.findIndex((i) => i.iteration === iteration);
            if (existing >= 0) {
              const updated = [...prev];
              updated[existing] = iterationResult;
              return updated;
            }
            return [...prev, iterationResult];
          });
        },
      );
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, [taskRunId]);

  const data: ConstraintResultsData = useMemo(() => {
    if (iterations.length === 0) {
      return DEFAULT_CONSTRAINT_RESULTS_DATA;
    }

    // Sort by iteration descending (newest first)
    const sorted = [...iterations].sort((a, b) => b.iteration - a.iteration);
    const latest = sorted[0];

    return {
      iterations: sorted,
      hasData: true,
      latest,
      latestCounts: calculateCounts(latest.results),
    };
  }, [iterations]);

  return data;
}

export default useConstraintResults;
