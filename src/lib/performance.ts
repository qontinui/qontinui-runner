/**
 * Performance Monitoring System
 *
 * Tracks metrics for evaluating optimization impact:
 * - Network requests (count, latency)
 * - Render times
 * - Memory usage
 * - Custom measurements
 *
 * Uses the browser's Performance API for accurate timing.
 * Only active in development mode.
 */

// ============================================================================
// Types
// ============================================================================

export interface PerformanceMeasure {
  name: string;
  startTime: number;
  duration: number;
  timestamp: number;
}

export interface NetworkMetrics {
  totalRequests: number;
  requestsPerMinute: number;
  averageLatency: number;
  maxLatency: number;
  minLatency: number;
  latencyHistory: number[];
}

export interface RenderMetrics {
  componentName: string;
  renderCount: number;
  averageRenderTime: number;
  lastRenderTime: number;
  totalRenderTime: number;
}

export interface WebSocketMetrics {
  messagesReceived: number;
  averageLatency: number;
  lastLatency: number;
  reconnectCount: number;
  isConnected: boolean;
}

export interface VirtualScrollMetrics {
  visibleItems: number;
  totalItems: number;
  visibleRatio: number;
  scrollFps: number;
  lastScrollTime: number;
}

export interface MemoryMetrics {
  usedJSHeapSize: number;
  totalJSHeapSize: number;
  heapUtilization: number;
  timestamp: number;
}

export interface AggregatedMetrics {
  network: NetworkMetrics;
  renders: Map<string, RenderMetrics>;
  webSocket: WebSocketMetrics;
  virtualScroll: VirtualScrollMetrics;
  memory: MemoryMetrics | null;
  customMeasures: PerformanceMeasure[];
  startTime: number;
  uptime: number;
}

// ============================================================================
// Performance Monitor Class
// ============================================================================

/**
 * Central performance monitoring class.
 * Singleton pattern - use PerformanceMonitor.getInstance()
 */
export class PerformanceMonitor {
  private static instance: PerformanceMonitor | null = null;
  private enabled: boolean;
  private startTime: number;

  // Measurement tracking
  private activeMeasurements: Map<string, number> = new Map();
  private completedMeasures: PerformanceMeasure[] = [];

  // Network tracking
  private networkRequests: { timestamp: number; latency: number }[] = [];
  private networkStartTimes: Map<string, number> = new Map();

  // Render tracking
  private renderMetrics: Map<string, RenderMetrics> = new Map();

  // WebSocket tracking
  private wsMessagesReceived = 0;
  private wsLatencies: number[] = [];
  private wsReconnectCount = 0;
  private wsConnected = false;
  private wsLastMessageTime = 0;

  // Virtual scroll tracking
  private scrollMetrics: VirtualScrollMetrics = {
    visibleItems: 0,
    totalItems: 0,
    visibleRatio: 0,
    scrollFps: 0,
    lastScrollTime: 0,
  };
  private scrollFrameTimes: number[] = [];

  // Memory tracking
  private memorySnapshots: MemoryMetrics[] = [];
  private memoryInterval: ReturnType<typeof setInterval> | null = null;

  // Subscribers for real-time updates
  private subscribers: Set<(metrics: AggregatedMetrics) => void> = new Set();

  private constructor() {
    this.enabled = import.meta.env.DEV;
    this.startTime = Date.now();

    if (this.enabled) {
      this.startMemoryTracking();
    }
  }

  static getInstance(): PerformanceMonitor {
    if (!PerformanceMonitor.instance) {
      PerformanceMonitor.instance = new PerformanceMonitor();
    }
    return PerformanceMonitor.instance;
  }

  // ============================================================================
  // Core Measurement Methods
  // ============================================================================

  /**
   * Start a named measurement
   */
  startMeasure(name: string): void {
    if (!this.enabled) return;

    const markName = `perf-start-${name}`;
    try {
      performance.mark(markName);
      this.activeMeasurements.set(name, performance.now());
    } catch (_e) {
      // Performance API may not be available in all contexts
      this.activeMeasurements.set(name, Date.now());
    }
  }

  /**
   * End a named measurement and record the duration
   */
  endMeasure(name: string): number | null {
    if (!this.enabled) return null;

    const startTime = this.activeMeasurements.get(name);
    if (startTime === undefined) {
      console.warn(`[PerformanceMonitor] No active measurement found for: ${name}`);
      return null;
    }

    const endTime = performance.now();
    const duration = endTime - startTime;

    try {
      const startMarkName = `perf-start-${name}`;
      const endMarkName = `perf-end-${name}`;
      performance.mark(endMarkName);
      performance.measure(name, startMarkName, endMarkName);
    } catch (_e) {
      // Fallback if Performance API marks fail
    }

    this.activeMeasurements.delete(name);

    const measure: PerformanceMeasure = {
      name,
      startTime,
      duration,
      timestamp: Date.now(),
    };

    this.completedMeasures.push(measure);

    // Keep only last 1000 measures
    if (this.completedMeasures.length > 1000) {
      this.completedMeasures = this.completedMeasures.slice(-500);
    }

    this.notifySubscribers();
    return duration;
  }

  /**
   * Measure a function execution time
   */
  async measureAsync<T>(name: string, fn: () => Promise<T>): Promise<T> {
    this.startMeasure(name);
    try {
      return await fn();
    } finally {
      this.endMeasure(name);
    }
  }

  /**
   * Measure synchronous function execution time
   */
  measureSync<T>(name: string, fn: () => T): T {
    this.startMeasure(name);
    try {
      return fn();
    } finally {
      this.endMeasure(name);
    }
  }

  // ============================================================================
  // Network Tracking
  // ============================================================================

  /**
   * Start tracking a network request
   */
  startNetworkRequest(requestId: string): void {
    if (!this.enabled) return;
    this.networkStartTimes.set(requestId, performance.now());
  }

  /**
   * End tracking a network request
   */
  endNetworkRequest(requestId: string): void {
    if (!this.enabled) return;

    const startTime = this.networkStartTimes.get(requestId);
    if (startTime === undefined) return;

    const latency = performance.now() - startTime;
    this.networkStartTimes.delete(requestId);

    this.networkRequests.push({
      timestamp: Date.now(),
      latency,
    });

    // Keep only last 5 minutes of requests
    const fiveMinutesAgo = Date.now() - 5 * 60 * 1000;
    this.networkRequests = this.networkRequests.filter((r) => r.timestamp > fiveMinutesAgo);

    this.notifySubscribers();
  }

  /**
   * Record a network request with known latency
   */
  recordNetworkRequest(latency: number): void {
    if (!this.enabled) return;

    this.networkRequests.push({
      timestamp: Date.now(),
      latency,
    });

    // Keep only last 5 minutes of requests
    const fiveMinutesAgo = Date.now() - 5 * 60 * 1000;
    this.networkRequests = this.networkRequests.filter((r) => r.timestamp > fiveMinutesAgo);

    this.notifySubscribers();
  }

  // ============================================================================
  // Render Tracking
  // ============================================================================

  /**
   * Record a component render
   */
  recordRender(componentName: string, renderTime: number): void {
    if (!this.enabled) return;

    const existing = this.renderMetrics.get(componentName);
    if (existing) {
      existing.renderCount++;
      existing.totalRenderTime += renderTime;
      existing.averageRenderTime = existing.totalRenderTime / existing.renderCount;
      existing.lastRenderTime = renderTime;
    } else {
      this.renderMetrics.set(componentName, {
        componentName,
        renderCount: 1,
        averageRenderTime: renderTime,
        lastRenderTime: renderTime,
        totalRenderTime: renderTime,
      });
    }

    this.notifySubscribers();
  }

  // ============================================================================
  // WebSocket Tracking
  // ============================================================================

  /**
   * Record a WebSocket message received
   */
  recordWebSocketMessage(latency?: number): void {
    if (!this.enabled) return;

    this.wsMessagesReceived++;
    this.wsLastMessageTime = Date.now();

    if (latency !== undefined) {
      this.wsLatencies.push(latency);
      // Keep only last 100 latencies
      if (this.wsLatencies.length > 100) {
        this.wsLatencies = this.wsLatencies.slice(-50);
      }
    }

    this.notifySubscribers();
  }

  /**
   * Record WebSocket connection state
   */
  setWebSocketConnected(connected: boolean): void {
    if (!this.enabled) return;

    if (!connected && this.wsConnected) {
      this.wsReconnectCount++;
    }
    this.wsConnected = connected;
    this.notifySubscribers();
  }

  // ============================================================================
  // Virtual Scroll Tracking
  // ============================================================================

  /**
   * Update virtual scroll metrics
   */
  updateVirtualScrollMetrics(visibleItems: number, totalItems: number): void {
    if (!this.enabled) return;

    this.scrollMetrics.visibleItems = visibleItems;
    this.scrollMetrics.totalItems = totalItems;
    this.scrollMetrics.visibleRatio = totalItems > 0 ? visibleItems / totalItems : 0;

    this.notifySubscribers();
  }

  /**
   * Record a scroll frame for FPS calculation
   */
  recordScrollFrame(): void {
    if (!this.enabled) return;

    const now = performance.now();
    this.scrollFrameTimes.push(now);
    this.scrollMetrics.lastScrollTime = now;

    // Keep only last second of frame times
    const oneSecondAgo = now - 1000;
    this.scrollFrameTimes = this.scrollFrameTimes.filter((t) => t > oneSecondAgo);
    this.scrollMetrics.scrollFps = this.scrollFrameTimes.length;

    this.notifySubscribers();
  }

  // ============================================================================
  // Memory Tracking
  // ============================================================================

  private startMemoryTracking(): void {
    // Check if memory API is available
    if (!("memory" in performance)) {
      return;
    }

    this.memoryInterval = setInterval(() => {
      this.captureMemorySnapshot();
    }, 5000); // Every 5 seconds
  }

  private captureMemorySnapshot(): void {
    // @ts-expect-error - memory is a non-standard API
    const memory = performance.memory;
    if (!memory) return;

    const snapshot: MemoryMetrics = {
      usedJSHeapSize: memory.usedJSHeapSize,
      totalJSHeapSize: memory.totalJSHeapSize,
      heapUtilization: memory.usedJSHeapSize / memory.totalJSHeapSize,
      timestamp: Date.now(),
    };

    this.memorySnapshots.push(snapshot);

    // Keep only last 5 minutes
    const fiveMinutesAgo = Date.now() - 5 * 60 * 1000;
    this.memorySnapshots = this.memorySnapshots.filter((s) => s.timestamp > fiveMinutesAgo);

    this.notifySubscribers();
  }

  // ============================================================================
  // Metrics Aggregation
  // ============================================================================

  /**
   * Get all aggregated metrics
   */
  getMetrics(): AggregatedMetrics {
    const now = Date.now();
    const oneMinuteAgo = now - 60 * 1000;

    // Calculate network metrics
    const recentRequests = this.networkRequests.filter((r) => r.timestamp > oneMinuteAgo);
    const latencies = this.networkRequests.map((r) => r.latency);

    const networkMetrics: NetworkMetrics = {
      totalRequests: this.networkRequests.length,
      requestsPerMinute: recentRequests.length,
      averageLatency:
        latencies.length > 0 ? latencies.reduce((a, b) => a + b, 0) / latencies.length : 0,
      maxLatency: latencies.length > 0 ? Math.max(...latencies) : 0,
      minLatency: latencies.length > 0 ? Math.min(...latencies) : 0,
      latencyHistory: latencies.slice(-50),
    };

    // Calculate WebSocket metrics
    const wsMetrics: WebSocketMetrics = {
      messagesReceived: this.wsMessagesReceived,
      averageLatency:
        this.wsLatencies.length > 0
          ? this.wsLatencies.reduce((a, b) => a + b, 0) / this.wsLatencies.length
          : 0,
      lastLatency: this.wsLatencies.length > 0 ? this.wsLatencies[this.wsLatencies.length - 1] : 0,
      reconnectCount: this.wsReconnectCount,
      isConnected: this.wsConnected,
    };

    // Get latest memory snapshot
    const latestMemory =
      this.memorySnapshots.length > 0
        ? this.memorySnapshots[this.memorySnapshots.length - 1]
        : null;

    return {
      network: networkMetrics,
      renders: new Map(this.renderMetrics),
      webSocket: wsMetrics,
      virtualScroll: { ...this.scrollMetrics },
      memory: latestMemory,
      customMeasures: [...this.completedMeasures],
      startTime: this.startTime,
      uptime: now - this.startTime,
    };
  }

  // ============================================================================
  // Subscription
  // ============================================================================

  /**
   * Subscribe to metrics updates
   */
  subscribe(callback: (metrics: AggregatedMetrics) => void): () => void {
    this.subscribers.add(callback);
    return () => this.subscribers.delete(callback);
  }

  private notifySubscribers(): void {
    if (this.subscribers.size === 0) return;
    const metrics = this.getMetrics();
    this.subscribers.forEach((cb) => cb(metrics));
  }

  // ============================================================================
  // Reset and Cleanup
  // ============================================================================

  /**
   * Reset all metrics
   */
  reset(): void {
    this.activeMeasurements.clear();
    this.completedMeasures = [];
    this.networkRequests = [];
    this.networkStartTimes.clear();
    this.renderMetrics.clear();
    this.wsMessagesReceived = 0;
    this.wsLatencies = [];
    this.wsReconnectCount = 0;
    this.scrollMetrics = {
      visibleItems: 0,
      totalItems: 0,
      visibleRatio: 0,
      scrollFps: 0,
      lastScrollTime: 0,
    };
    this.scrollFrameTimes = [];
    this.memorySnapshots = [];
    this.startTime = Date.now();

    this.notifySubscribers();
  }

  /**
   * Cleanup resources
   */
  destroy(): void {
    if (this.memoryInterval) {
      clearInterval(this.memoryInterval);
      this.memoryInterval = null;
    }
    this.subscribers.clear();
    PerformanceMonitor.instance = null;
  }

  /**
   * Check if monitoring is enabled
   */
  isEnabled(): boolean {
    return this.enabled;
  }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/**
 * Get the global performance monitor instance
 */
export function getPerformanceMonitor(): PerformanceMonitor {
  return PerformanceMonitor.getInstance();
}

/**
 * Quick measure function
 */
export function measure<T>(name: string, fn: () => T): T {
  return getPerformanceMonitor().measureSync(name, fn);
}

/**
 * Quick async measure function
 */
export async function measureAsync<T>(name: string, fn: () => Promise<T>): Promise<T> {
  return getPerformanceMonitor().measureAsync(name, fn);
}

export default PerformanceMonitor;
