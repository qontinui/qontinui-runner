import { useEffect, useState, type RefObject } from "react";

/**
 * Observed pixel height of an element, or 0 while unmeasured/disabled.
 *
 * `enabled` gates the observer so the common case — every zone with its
 * prompts panel closed — allocates nothing. 0 is deliberately "unknown"
 * rather than "zero-height": callers treat it as unconstrained, since a
 * layout that briefly assumed no room would flash the wrong shape on mount.
 */
export function useElementHeight(ref: RefObject<HTMLElement | null>, enabled: boolean): number {
  const [height, setHeight] = useState(0);

  useEffect(() => {
    if (!enabled) return;
    const el = ref.current;
    if (!el) return;
    // No synchronous seed measurement: ResizeObserver delivers the element's
    // current size on `observe()`, so an initial `setHeight` in the effect body
    // would be a redundant cascading render for the same number.
    const observer = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.height;
      if (typeof next === "number") setHeight(next);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref, enabled]);

  return height;
}
