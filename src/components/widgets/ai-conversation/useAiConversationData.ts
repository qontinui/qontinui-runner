/**
 * useAiConversationData Hook
 *
 * React hook for accessing AI conversation data for the dashboard widget.
 * Subscribes to LogStore updates and transforms data for display.
 *
 * IMPORTANT: Filters logs by taskRunId to ensure proper isolation when
 * multiple workflows are running simultaneously.
 */

import { useState, useEffect, useMemo } from "react";
import { logManager } from "@/managers";
import { useCurrentTaskRunId, useTaskStartTime } from "@/contexts/TaskContext";
import type { AiConversationData, AiOutputEntry } from "./types";

/** Threshold in milliseconds to consider AI as "thinking" based on recent activity */
const THINKING_THRESHOLD_MS = 5000;

/**
 * Filter entries to only show entries for a specific task run.
 * This ensures proper isolation when multiple workflows run simultaneously.
 *
 * When no taskRunId is specified (idle state), falls back to showing entries
 * from the most recent task run so users can review completed conversations.
 */
function filterByTaskRunId(
  allEntries: AiOutputEntry[] | undefined,
  taskRunId: string | null,
): AiOutputEntry[] {
  // Defensive: handle undefined or null input
  if (!allEntries || allEntries.length === 0) return [];

  // If taskRunId specified, filter to that specific task run
  if (taskRunId) {
    return allEntries.filter((entry) => entry.taskRunId === taskRunId);
  }

  // No taskRunId specified (idle state) — show the most recent task run's entries
  // so users can still view conversations after a workflow completes
  const lastEntry = allEntries[allEntries.length - 1];
  const lastTaskRunId = lastEntry?.taskRunId;
  if (lastTaskRunId) {
    return allEntries.filter((entry) => entry.taskRunId === lastTaskRunId);
  }

  // No task run IDs on any entries — return all entries
  return allEntries;
}

/**
 * Filter entries to only show the most recent session within a task.
 * This ensures we don't show data from previous AI sessions.
 */
function filterToCurrentSession(allEntries: AiOutputEntry[] | undefined): AiOutputEntry[] {
  // Defensive: handle undefined or null input
  if (!allEntries || allEntries.length === 0) return [];

  // Find the most recent session ID
  const lastEntry = allEntries[allEntries.length - 1];
  const currentSessionId = lastEntry?.sessionId;

  // If no session ID, return all entries (backward compatibility)
  if (!currentSessionId) return allEntries;

  // Filter to only entries from the current session
  return allEntries.filter((entry) => entry.sessionId === currentSessionId);
}

/**
 * Remove consecutive duplicate messages (same source and content).
 * This handles cases where prompts are logged multiple times.
 */
function deduplicateEntries(entries: AiOutputEntry[] | undefined): AiOutputEntry[] {
  // Defensive: handle undefined or null input
  if (!entries || entries.length === 0) return [];

  const result: AiOutputEntry[] = [entries[0]];

  for (let i = 1; i < entries.length; i++) {
    const prev = result[result.length - 1];
    const curr = entries[i];

    // Skip if same source and same content as previous entry
    if (prev.source === curr.source && prev.line === curr.line) {
      continue;
    }

    result.push(curr);
  }

  return result;
}

/**
 * Default empty data to return on errors.
 */
const EMPTY_DATA: AiConversationData = {
  entries: [],
  currentSession: null,
  isThinking: false,
  lastMessage: null,
  messageCount: 0,
};

/**
 * Hook for accessing AI conversation data.
 * Returns transformed data suitable for widget display.
 * Only shows entries from the current task run (not other tasks' data).
 *
 * IMPORTANT: Filters by taskRunId to ensure proper isolation when
 * multiple workflows are running simultaneously.
 */
export function useAiConversationData(): AiConversationData {
  const currentTaskRunId = useCurrentTaskRunId();
  const _taskStartTime = useTaskStartTime();
  const [allEntries, setAllEntries] = useState<AiOutputEntry[]>([]);

  // Subscribe to log changes
  useEffect(() => {
    // Defensive: check if logManager exists
    if (!logManager) {
      console.warn("useAiConversationData: logManager is undefined");
      return;
    }

    // Initial fetch - defensive coding for undefined
    try {
      const logs = logManager.getAiOutputLogs?.() ?? [];
      // eslint-disable-next-line react-hooks/set-state-in-effect -- initial data fetch on mount
      setAllEntries(logs);
    } catch (e) {
      console.error("useAiConversationData: Error fetching logs:", e);
      setAllEntries([]);
    }

    // Subscribe to changes
    const unsubscribe = logManager.subscribe?.(() => {
      try {
        const updatedLogs = logManager.getAiOutputLogs?.() ?? [];
        setAllEntries(updatedLogs);
      } catch (e) {
        console.error("useAiConversationData: Error in subscription:", e);
      }
    });

    return () => unsubscribe?.();
  }, []);

  // Filter by taskRunId, then to current session, and deduplicate
  const entries = useMemo(() => {
    try {
      // Filter to entries for this specific task run (critical for multi-workflow isolation)
      const taskEntries = filterByTaskRunId(allEntries, currentTaskRunId);

      // Then filter to current session within this task and deduplicate
      const sessionFiltered = filterToCurrentSession(taskEntries);
      return deduplicateEntries(sessionFiltered);
    } catch (e) {
      console.error("useAiConversationData: Error filtering entries:", e);
      return [];
    }
  }, [allEntries, currentTaskRunId]);

  // Track current time for "is thinking" computation; refresh when entries change
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- sync time snapshot when entries update
    setNow(Date.now());
  }, [entries]);

  // Compute derived state
  const data = useMemo((): AiConversationData => {
    try {
      const safeEntries = entries ?? [];
      const messageCount = safeEntries.length;
      const lastEntry = safeEntries.length > 0 ? safeEntries[safeEntries.length - 1] : null;

      // Determine current session from the most recent entry
      const currentSession = lastEntry?.sessionId ?? null;

      // Determine if AI is thinking based on recent activity
      // If the last entry was recent and from "response", AI might still be processing
      const lastTimestamp = lastEntry?.timestamp ?? 0;
      const timeSinceLastMessage = now - lastTimestamp;
      const isThinking =
        lastEntry?.source === "response" && timeSinceLastMessage < THINKING_THRESHOLD_MS;

      // Get the last message text (truncated for summary display)
      const lastMessage = lastEntry?.line ?? null;

      return {
        entries: safeEntries,
        currentSession,
        isThinking,
        lastMessage,
        messageCount,
      };
    } catch (e) {
      console.error("useAiConversationData: Error computing data:", e);
      return EMPTY_DATA;
    }
  }, [entries, now]);

  return data;
}

/**
 * Get entries filtered by session ID.
 */
export function filterEntriesBySession(
  entries: AiOutputEntry[],
  sessionId: string | null,
): AiOutputEntry[] {
  if (!sessionId) {
    return entries;
  }
  return entries.filter((entry) => entry.sessionId === sessionId);
}

/**
 * Group entries by their source (prompt vs response) for chat-style display.
 */
export function groupEntriesBySpeaker(entries: AiOutputEntry[]): {
  groups: Array<{
    source: string;
    entries: AiOutputEntry[];
    timestamp: number;
  }>;
} {
  const groups: Array<{
    source: string;
    entries: AiOutputEntry[];
    timestamp: number;
  }> = [];

  let currentGroup: { source: string; entries: AiOutputEntry[]; timestamp: number } | null = null;

  for (const entry of entries) {
    if (!currentGroup || currentGroup.source !== entry.source) {
      // Start a new group
      currentGroup = {
        source: entry.source,
        entries: [entry],
        timestamp: entry.timestamp,
      };
      groups.push(currentGroup);
    } else {
      // Add to existing group
      currentGroup.entries.push(entry);
    }
  }

  return { groups };
}
