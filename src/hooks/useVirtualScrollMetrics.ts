/**
 * useVirtualScrollMetrics Hook
 *
 * Tracks virtual scrolling performance metrics:
 * - Visible vs total items
 * - Scroll frame rate (FPS)
 * - Memory efficiency
 *
 * Integrates with the central performance monitor.
 * Only active in development mode.
 */

import { useCallback, useEffect, useRef } from "react";
import { getPerformanceMonitor } from "../lib/performance";

// ============================================================================
// Types
// ============================================================================

export interface UseVirtualScrollMetricsOptions {
  /** Whether tracking is enabled (defaults to DEV mode) */
  enabled?: boolean;
  /** Throttle scroll frame recording (ms) */
  throttleMs?: number;
}

export interface UseVirtualScrollMetricsReturn {
  /** Update the visible/total item counts */
  updateItemCounts: (visibleItems: number, totalItems: number) => void;
  /** Record a scroll frame (call in onScroll handler) */
  recordScrollFrame: () => void;
  /** Get current scroll FPS */
  getScrollFps: () => number;
}

// ============================================================================
// Hook Implementation
// ============================================================================

/**
 * Hook for tracking virtual scrolling performance.
 *
 * @example
 * ```tsx
 * function VirtualList({ items }) {
 *   const { updateItemCounts, recordScrollFrame } = useVirtualScrollMetrics();
 *
 *   const handleScroll = () => {
 *     recordScrollFrame();
 *   };
 *
 *   useEffect(() => {
 *     updateItemCounts(visibleCount, items.length);
 *   }, [visibleCount, items.length]);
 *
 *   return <List onScroll={handleScroll} />;
 * }
 * ```
 */
export function useVirtualScrollMetrics(
  options: UseVirtualScrollMetricsOptions = {}
): UseVirtualScrollMetricsReturn {
  const { enabled = import.meta.env.DEV, throttleMs = 16 } = options;

  const lastScrollFrameRef = useRef<number>(0);
  const scrollFrameTimesRef = useRef<number[]>([]);

  // Update visible/total item counts
  const updateItemCounts = useCallback(
    (visibleItems: number, totalItems: number) => {
      if (!enabled) return;
      getPerformanceMonitor().updateVirtualScrollMetrics(visibleItems, totalItems);
    },
    [enabled]
  );

  // Record a scroll frame (throttled)
  const recordScrollFrame = useCallback(() => {
    if (!enabled) return;

    const now = performance.now();

    // Throttle
    if (now - lastScrollFrameRef.current < throttleMs) {
      return;
    }
    lastScrollFrameRef.current = now;

    // Record in performance monitor
    getPerformanceMonitor().recordScrollFrame();

    // Track locally for FPS calculation
    scrollFrameTimesRef.current.push(now);

    // Keep only last second
    const oneSecondAgo = now - 1000;
    scrollFrameTimesRef.current = scrollFrameTimesRef.current.filter((t) => t > oneSecondAgo);
  }, [enabled, throttleMs]);

  // Get current scroll FPS
  const getScrollFps = useCallback((): number => {
    return scrollFrameTimesRef.current.length;
  }, []);

  return {
    updateItemCounts,
    recordScrollFrame,
    getScrollFps,
  };
}

// ============================================================================
// Higher-Order Component for Virtual Lists
// ============================================================================

export interface WithVirtualScrollMetricsProps {
  __scrollMetrics?: {
    visibleItems: number;
    totalItems: number;
    scrollFps: number;
  };
}

/**
 * Create a wrapper that automatically tracks scroll metrics.
 *
 * @example
 * ```tsx
 * const TrackedVirtualList = withVirtualScrollMetrics(
 *   MyVirtualList,
 *   (props) => ({ visible: props.visibleCount, total: props.items.length })
 * );
 * ```
 */
export function createScrollMetricsTracker(
  getItems: () => { visible: number; total: number }
): {
  onScroll: () => void;
  useTracker: () => void;
} {
  let lastScrollFrame = 0;
  const throttleMs = 16;

  const onScroll = () => {
    if (!import.meta.env.DEV) return;

    const now = performance.now();
    if (now - lastScrollFrame < throttleMs) return;
    lastScrollFrame = now;

    getPerformanceMonitor().recordScrollFrame();
  };

  const useTracker = () => {
    useEffect(() => {
      if (!import.meta.env.DEV) return;

      const interval = setInterval(() => {
        const { visible, total } = getItems();
        getPerformanceMonitor().updateVirtualScrollMetrics(visible, total);
      }, 1000);

      return () => clearInterval(interval);
    }, []);
  };

  return { onScroll, useTracker };
}

export default useVirtualScrollMetrics;
