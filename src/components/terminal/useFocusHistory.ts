import { useCallback, useEffect, useRef, useState } from "react";

export function useFocusHistory(
  focusedZone: number,
  setFocusedZone: (z: number) => void,
): {
  goBack: () => void;
  goForward: () => void;
  canGoBack: boolean;
  canGoForward: boolean;
} {
  const focusHistoryRef = useRef<number[]>([]);
  const focusHistoryIndexRef = useRef(-1);
  const isNavigatingHistoryRef = useRef(false);

  // Track zone changes that aren't caused by history navigation
  const [, setTick] = useState(0);

  useEffect(() => {
    if (isNavigatingHistoryRef.current) {
      isNavigatingHistoryRef.current = false;
      return;
    }
    const history = focusHistoryRef.current;
    // Don't push if same as current position in history
    if (history[focusHistoryIndexRef.current] === focusedZone) return;

    // Truncate forward history
    focusHistoryRef.current = history.slice(0, focusHistoryIndexRef.current + 1);
    focusHistoryRef.current.push(focusedZone);

    // Cap at 20 entries
    if (focusHistoryRef.current.length > 20) {
      focusHistoryRef.current = focusHistoryRef.current.slice(-20);
    }
    focusHistoryIndexRef.current = focusHistoryRef.current.length - 1;
    setTick((t) => t + 1);
  }, [focusedZone]);

  const goBack = useCallback(() => {
    if (focusHistoryIndexRef.current > 0) {
      focusHistoryIndexRef.current -= 1;
      isNavigatingHistoryRef.current = true;
      setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
      setTick((t) => t + 1);
    }
  }, [setFocusedZone]);

  const goForward = useCallback(() => {
    if (focusHistoryIndexRef.current < focusHistoryRef.current.length - 1) {
      focusHistoryIndexRef.current += 1;
      isNavigatingHistoryRef.current = true;
      setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
      setTick((t) => t + 1);
    }
  }, [setFocusedZone]);

  const canGoBack = focusHistoryIndexRef.current > 0;
  const canGoForward = focusHistoryIndexRef.current < focusHistoryRef.current.length - 1;

  return { goBack, goForward, canGoBack, canGoForward };
}
