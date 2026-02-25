/**
 * Shared elapsed time hook using a single module-level interval.
 * All consumers subscribe to the same 1-second tick, preventing
 * multiple independent intervals from causing excessive re-renders.
 */

import { useSyncExternalStore } from "react";

// Module-level state
let currentTime = Date.now();
let listeners = new Set<() => void>();
let intervalId: ReturnType<typeof setInterval> | null = null;

function startTimer() {
  if (intervalId !== null) return;
  intervalId = setInterval(() => {
    currentTime = Date.now();
    listeners.forEach((listener) => listener());
  }, 1000);
}

function stopTimer() {
  if (intervalId === null) return;
  clearInterval(intervalId);
  intervalId = null;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (listeners.size === 1) startTimer();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) stopTimer();
  };
}

function getSnapshot(): number {
  return currentTime;
}

/**
 * Hook that returns elapsed seconds since the given start time.
 * Uses a single shared interval across all consumers.
 * Returns 0 if startTime is null.
 */
export function useSharedElapsedTime(startTime: number | null): number {
  const now = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  if (startTime === null) return 0;
  return Math.floor((now - startTime) / 1000);
}
