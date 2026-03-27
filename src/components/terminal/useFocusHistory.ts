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

  // State-tracked navigation bounds so render can derive canGoBack/canGoForward
  // without reading ref.current during render.
  const [navState, setNavState] = useState({ index: -1, length: 0 });

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
    setNavState({ index: focusHistoryIndexRef.current, length: focusHistoryRef.current.length });
  }, [focusedZone]);

  const goBack = useCallback(() => {
    if (focusHistoryIndexRef.current > 0) {
      focusHistoryIndexRef.current -= 1;
      isNavigatingHistoryRef.current = true;
      setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
      setNavState({ index: focusHistoryIndexRef.current, length: focusHistoryRef.current.length });
    }
  }, [setFocusedZone]);

  const goForward = useCallback(() => {
    if (focusHistoryIndexRef.current < focusHistoryRef.current.length - 1) {
      focusHistoryIndexRef.current += 1;
      isNavigatingHistoryRef.current = true;
      setFocusedZone(focusHistoryRef.current[focusHistoryIndexRef.current]);
      setNavState({ index: focusHistoryIndexRef.current, length: focusHistoryRef.current.length });
    }
  }, [setFocusedZone]);

  const canGoBack = navState.index > 0;
  const canGoForward = navState.index < navState.length - 1;

  return { goBack, goForward, canGoBack, canGoForward };
}
