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
    currentState = state as SessionState;
    currentTaskRunId = taskRunId;
    currentSessionId = sessionId;
    notifySubscribers();
  });
}

/**
 * Hook to track the current interactive Claude session state.
 *
 * @param filterTaskRunId - Optional task run ID to filter events for.
 * @returns Session state info for the given task run.
 */
export function useSessionState(filterTaskRunId?: string | null) {
  const [state, setState] = useState<SessionState>(
    filterTaskRunId && filterTaskRunId === currentTaskRunId ? currentState : null,
  );
  const [sessionId, setSessionId] = useState<string | null>(
    filterTaskRunId && filterTaskRunId === currentTaskRunId ? currentSessionId : null,
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
    canSendMessage,
    canInterrupt,
    isActive,
  };
}
