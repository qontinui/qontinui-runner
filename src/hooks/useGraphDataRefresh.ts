/**
 * useGraphDataRefresh
 *
 * Subscribes to runner events via the GraphQL subscription infrastructure
 * and automatically invalidates graph analytics queries when relevant events
 * occur (task completion, finding detection/resolution, workflow generation).
 *
 * This supplements the existing polling (staleTime 30s, refetchInterval 60s)
 * with event-driven cache invalidation so the UI updates within seconds of
 * data changes rather than waiting for the next poll cycle.
 *
 * Polling is kept as a fallback until the subscription path is proven reliable.
 */

import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRunnerEvents } from "./graphql/subscriptions";
import { graphAnalyticsKeys } from "./useGraphAnalytics";

/** Event type names that indicate graph data may have changed. */
const GRAPH_RELEVANT_TYPENAMES = new Set([
  "TaskRunUpdateEvent",
  "FindingDetectedEvent",
  "FindingResolvedEvent",
]);

/** Generic event types (from GenericRunnerEvent.eventType) that affect graph data. */
const GRAPH_RELEVANT_GENERIC_EVENTS = new Set([
  "task_run_completed",
  "reflection_completed",
  "workflow_generated",
  "workflow_version_created",
  "pipeline_phase_completed",
  "cross_run_pattern_detected",
]);

/** Debounce window in ms to batch rapid-fire invalidations. */
const INVALIDATION_DEBOUNCE_MS = 500;

/**
 * Subscribes to runner events and auto-refreshes graph analytics data
 * when relevant events arrive. Call this once at the app level (e.g., in App.tsx).
 *
 * @param skip - Skip the subscription (e.g., when not authenticated)
 */
export function useGraphDataRefresh(skip = false) {
  const queryClient = useQueryClient();
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Subscribe to all runner events (no filter — we filter client-side
  // to avoid missing new event types added later).
  const { data } = useRunnerEvents(undefined, skip);

  useEffect(() => {
    if (!data?.runnerEvents) return;

    const event = data.runnerEvents;
    let isRelevant = false;

    // Check by GraphQL union typename
    if (event.__typename && GRAPH_RELEVANT_TYPENAMES.has(event.__typename)) {
      // For TaskRunUpdateEvent, only invalidate on terminal statuses
      if (event.__typename === "TaskRunUpdateEvent") {
        const status = event.status?.toLowerCase();
        if (status === "completed" || status === "failed" || status === "stopped") {
          isRelevant = true;
        }
      } else {
        isRelevant = true;
      }
    }

    // Check GenericRunnerEvent.eventType
    if (
      event.__typename === "GenericRunnerEvent" &&
      event.eventType &&
      GRAPH_RELEVANT_GENERIC_EVENTS.has(event.eventType)
    ) {
      isRelevant = true;
    }

    if (!isRelevant) return;

    // Debounce: if multiple events arrive in quick succession (e.g., a batch
    // of findings resolved), only invalidate once after the burst settles.
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    debounceRef.current = setTimeout(() => {
      queryClient.invalidateQueries({ queryKey: graphAnalyticsKeys.all });
      debounceRef.current = null;
    }, INVALIDATION_DEBOUNCE_MS);
  }, [data, queryClient]);

  // Cleanup timeout on unmount
  useEffect(() => {
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, []);
}
