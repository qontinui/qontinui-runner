/**
 * useAutoScroll Hook
 *
 * Manages auto-scrolling behavior for log viewers.
 * Automatically scrolls to bottom when new content is added.
 */

import { useEffect, RefObject } from "react";

export interface UseAutoScrollOptions {
  enabled: boolean;
  containerRef: RefObject<HTMLDivElement>;
  dependencies: unknown[];
}

/**
 * Hook for auto-scrolling a container to the bottom
 */
export function useAutoScroll({ enabled, containerRef, dependencies }: UseAutoScrollOptions): void {
  useEffect(() => {
    if (enabled && containerRef.current) {
      const scrollContainer = containerRef.current;
      requestAnimationFrame(() => {
        scrollContainer.scrollTop = scrollContainer.scrollHeight;
      });
    }
  }, dependencies);
}
