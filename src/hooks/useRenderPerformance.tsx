/**
 * useRenderPerformance Hook
 *
 * Tracks component render performance using React Profiler API.
 * Measures:
 * - Render count
 * - Render time
 * - Time from data change to UI update
 *
 * Only active in development mode.
 */

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  Profiler,
  type ProfilerOnRenderCallback,
} from "react";
import { getPerformanceMonitor, type RenderMetrics } from "../lib/performance";
import { createLogger } from "@/lib/logger";

const logger = createLogger("RenderPerformance");

// ============================================================================
// Types
// ============================================================================

export interface UseRenderPerformanceOptions {
  /** Component name for tracking */
  componentName: string;
  /** Whether to track this component (defaults to DEV mode) */
  enabled?: boolean;
  /** Log renders to console */
  logToConsole?: boolean;
}

export interface UseRenderPerformanceReturn {
  /** Wrapper component for profiling children */
  ProfilerWrapper: React.FC<{ children: React.ReactNode }>;
  /** Current render metrics */
  metrics: RenderMetrics | null;
  /** Total render count */
  renderCount: number;
  /** Average render time in ms */
  averageRenderTime: number;
  /** Last render time in ms */
  lastRenderTime: number;
  /** Mark a data change for measuring time-to-update */
  markDataChange: () => void;
  /** Get time since last data change */
  getTimeSinceDataChange: () => number | null;
}

// ============================================================================
// Hook Implementation
// ============================================================================

/**
 * Hook for tracking component render performance.
 *
 * @example
 * ```tsx
 * function MyComponent({ data }) {
 *   const { ProfilerWrapper, renderCount, averageRenderTime } = useRenderPerformance({
 *     componentName: 'MyComponent'
 *   });
 *
 *   return (
 *     <ProfilerWrapper>
 *       <div>Render count: {renderCount}</div>
 *       <div>Avg render time: {averageRenderTime.toFixed(2)}ms</div>
 *       {data.map(item => <Item key={item.id} {...item} />)}
 *     </ProfilerWrapper>
 *   );
 * }
 * ```
 */
export function useRenderPerformance(
  options: UseRenderPerformanceOptions,
): UseRenderPerformanceReturn {
  const { componentName, enabled = import.meta.env.DEV, logToConsole = false } = options;

  const [metrics, setMetrics] = useState<RenderMetrics | null>(null);
  const [renderStats, setRenderStats] = useState<{
    renderCount: number;
    totalRenderTime: number;
    lastRenderTime: number;
  }>({ renderCount: 0, totalRenderTime: 0, lastRenderTime: 0 });
  const dataChangeTimeRef = useRef<number | null>(null);

  // Update local state from performance monitor
  useEffect(() => {
    if (!enabled) return;

    const monitor = getPerformanceMonitor();
    const unsubscribe = monitor.subscribe((aggregated) => {
      const componentMetrics = aggregated.renders.get(componentName);
      if (componentMetrics) {
        setMetrics(componentMetrics);
      }
    });

    return unsubscribe;
  }, [componentName, enabled]);

  // Profiler callback
  const onRender: ProfilerOnRenderCallback = useCallback(
    (
      id: string,
      phase: "mount" | "update" | "nested-update",
      actualDuration: number,
      baseDuration: number,
      startTime: number,
      commitTime: number,
    ) => {
      if (!enabled) return;

      setRenderStats((prev) => ({
        renderCount: prev.renderCount + 1,
        totalRenderTime: prev.totalRenderTime + actualDuration,
        lastRenderTime: actualDuration,
      }));

      // Record in performance monitor
      getPerformanceMonitor().recordRender(componentName, actualDuration);

      // Check for data change timing
      if (dataChangeTimeRef.current !== null) {
        const timeToUpdate = commitTime - dataChangeTimeRef.current;
        if (logToConsole) {
          logger.debug(`${componentName} time-to-update: ${timeToUpdate.toFixed(2)}ms`);
        }
        dataChangeTimeRef.current = null;
      }

      if (logToConsole) {
        logger.debug(
          `${componentName} ${phase}: ${actualDuration.toFixed(2)}ms (base: ${baseDuration.toFixed(2)}ms)`,
        );
      }
    },
    [componentName, enabled, logToConsole],
  );

  // Profiler wrapper component
  const ProfilerWrapper: React.FC<{ children: React.ReactNode }> = useCallback(
    ({ children }) => {
      if (!enabled) {
        return <>{children}</>;
      }

      return (
        <Profiler id={componentName} onRender={onRender}>
          {children}
        </Profiler>
      );
    },
    [componentName, enabled, onRender],
  );

  // Mark when data changes to measure time-to-update
  const markDataChange = useCallback(() => {
    if (!enabled) return;
    dataChangeTimeRef.current = performance.now();
  }, [enabled]);

  // Get time since last data change
  const getTimeSinceDataChange = useCallback((): number | null => {
    if (dataChangeTimeRef.current === null) return null;
    return performance.now() - dataChangeTimeRef.current;
  }, []);

  return {
    ProfilerWrapper,
    metrics,
    renderCount: renderStats.renderCount,
    averageRenderTime:
      renderStats.renderCount > 0 ? renderStats.totalRenderTime / renderStats.renderCount : 0,
    lastRenderTime: renderStats.lastRenderTime,
    markDataChange,
    getTimeSinceDataChange,
  };
}

// ============================================================================
// Batch Render Tracking
// ============================================================================

export interface BatchRenderMetrics {
  batchId: string;
  componentCount: number;
  totalRenderTime: number;
  averageRenderTime: number;
  renderTimes: { component: string; time: number }[];
}

/**
 * Track render performance across multiple components in a batch.
 * Useful for measuring list/grid rendering performance.
 */
export function useBatchRenderPerformance(batchId: string): {
  startBatch: () => void;
  endBatch: () => BatchRenderMetrics;
  recordRender: (componentName: string, renderTime: number) => void;
  currentBatch: BatchRenderMetrics | null;
} {
  const [currentBatch, setCurrentBatch] = useState<BatchRenderMetrics | null>(null);
  const renderTimesRef = useRef<{ component: string; time: number }[]>([]);
  const batchActiveRef = useRef(false);

  const startBatch = useCallback(() => {
    batchActiveRef.current = true;
    renderTimesRef.current = [];
  }, []);

  const recordRender = useCallback((componentName: string, renderTime: number) => {
    if (batchActiveRef.current) {
      renderTimesRef.current.push({ component: componentName, time: renderTime });
    }
  }, []);

  const endBatch = useCallback((): BatchRenderMetrics => {
    batchActiveRef.current = false;

    const renderTimes = [...renderTimesRef.current];
    const totalRenderTime = renderTimes.reduce((sum, r) => sum + r.time, 0);
    const metrics: BatchRenderMetrics = {
      batchId,
      componentCount: renderTimes.length,
      totalRenderTime,
      averageRenderTime: renderTimes.length > 0 ? totalRenderTime / renderTimes.length : 0,
      renderTimes,
    };

    setCurrentBatch(metrics);
    renderTimesRef.current = [];

    return metrics;
  }, [batchId]);

  return {
    startBatch,
    endBatch,
    recordRender,
    currentBatch,
  };
}

// ============================================================================
// Higher-Order Component
// ============================================================================

export interface WithRenderPerformanceProps {
  __renderMetrics?: RenderMetrics;
}

/**
 * HOC to wrap a component with render performance tracking.
 *
 * @example
 * ```tsx
 * const TrackedMyComponent = withRenderPerformance(MyComponent, 'MyComponent');
 * ```
 */
export function withRenderPerformance<P extends object>(
  WrappedComponent: React.ComponentType<P>,
  componentName: string,
): React.FC<P & WithRenderPerformanceProps> {
  const displayName = WrappedComponent.displayName || WrappedComponent.name || "Component";

  const TrackedComponent: React.FC<P & WithRenderPerformanceProps> = (props) => {
    const { ProfilerWrapper, metrics } = useRenderPerformance({ componentName });

    return (
      <ProfilerWrapper>
        <WrappedComponent {...props} __renderMetrics={metrics} />
      </ProfilerWrapper>
    );
  };

  TrackedComponent.displayName = `WithRenderPerformance(${displayName})`;

  return TrackedComponent;
}

export default useRenderPerformance;
