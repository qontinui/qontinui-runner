/**
 * useSessionState Hook
 *
 * Subscribes to claude-session-state events from the Tauri backend
 * to track the interactive Claude session lifecycle.
 */

import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

/**
 * Session states matching the Rust SessionState enum.
 */
export type SessionState =
  | "created"
  | "initializing"
  | "ready"
  | "processing"
  | "interrupting"
  | "closing"
  | "closed"
  | null;

/**
 * Session state event payload from the backend.
 */
interface SessionStateEvent {
  taskRunId: string;
  sessionId: string;
  state: string;
  /** True on the first state event emitted for a session auto-resumed after runner restart. */
  resumed?: boolean;
}

/**
 * Module-level state store (survives component re-mounts).
 */
let currentState: SessionState = null;
let currentTaskRunId: string | null = null;
let currentSessionId: string | null = null;
const subscribers = new Set<() => void>();

function notifySubscribers() {
  for (const fn of subscribers) {
    fn();
  }
}

// Set up the global event listener once
let listenerInitialized = false;
function ensureListener() {
  if (listenerInitialized) return;
  listenerInitialized = true;

  listen<SessionStateEvent>("claude-session-state", (event) => {
    const { taskRunId, sessionId, state } = event.payload;
    if (state === "closed") {
      // Reset module-level state so stale sessions don't leak across pages
      currentState = null;
      currentTaskRunId = null;
      currentSessionId = null;
    } else {
      currentState = state as SessionState;
      currentTaskRunId = taskRunId;
      currentSessionId = sessionId;
    }
    notifySubscribers();
  });
}

/**
 * Hook to track the current interactive Claude session state.
 *
 * @param filterTaskRunId - Optional task run ID to filter events for.
 *   Pass `undefined`/omit to accept all session state events (recommended
 *   when the dashboard task ID may differ from the Rust executor's ID).
 * @returns Session state info including the event's own taskRunId.
 */
export function useSessionState(filterTaskRunId?: string | null) {
  const [state, setState] = useState<SessionState>(
    filterTaskRunId && filterTaskRunId === currentTaskRunId ? currentState : null,
  );
  const [sessionId, setSessionId] = useState<string | null>(
    filterTaskRunId && filterTaskRunId === currentTaskRunId ? currentSessionId : null,
  );
  const [taskRunId, setTaskRunId] = useState<string | null>(
    filterTaskRunId && filterTaskRunId === currentTaskRunId ? currentTaskRunId : null,
  );

  useEffect(() => {
    ensureListener();

    const handler = () => {
      // If filtering by task run ID, only update when it matches
      if (filterTaskRunId && currentTaskRunId !== filterTaskRunId) {
        return;
      }
      setState(currentState);
      setSessionId(currentSessionId);
      setTaskRunId(currentTaskRunId);
    };

    subscribers.add(handler);

    // Sync initial state
    handler();

    return () => {
      subscribers.delete(handler);
    };
  }, [filterTaskRunId]);

  const canSendMessage = state === "ready" || state === "processing";
  const canInterrupt = state === "processing";
  const isActive = state !== null && state !== "closed" && state !== "closing";

  return {
    state,
    sessionId,
    /** The task run ID from the session state event (may differ from dashboard task ID). */
    taskRunId,
    canSendMessage,
    canInterrupt,
    isActive,
  };
}
