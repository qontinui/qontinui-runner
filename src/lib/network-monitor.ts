/**
 * Network Request Monitoring
 *
 * Intercepts fetch requests to track:
 * - Request count per minute
 * - Latency distribution
 * - Endpoint frequency
 * - Polling frequency analysis
 *
 * Only active in development mode.
 */

import { getPerformanceMonitor } from "./performance";
import { getErrorMessage } from "./utils";

// ============================================================================
// Types
// ============================================================================

export interface RequestRecord {
  id: string;
  url: string;
  method: string;
  startTime: number;
  endTime?: number;
  latency?: number;
  status?: number;
  error?: string;
}

export interface EndpointStats {
  url: string;
  requestCount: number;
  averageLatency: number;
  lastRequestTime: number;
  requestsPerMinute: number;
}

export interface NetworkMonitorStats {
  totalRequests: number;
  activeRequests: number;
  requestsPerMinute: number;
  averageLatency: number;
  errorRate: number;
  endpointStats: Map<string, EndpointStats>;
  recentRequests: RequestRecord[];
}

// ============================================================================
// Network Monitor Class
// ============================================================================

/**
 * Monitors network requests by wrapping the fetch API.
 */
export class NetworkMonitor {
  private static instance: NetworkMonitor | null = null;
  private enabled: boolean;
  private originalFetch: typeof fetch | null = null;
  private requestIdCounter = 0;

  // Request tracking
  private activeRequests: Map<string, RequestRecord> = new Map();
  private completedRequests: RequestRecord[] = [];
  private endpointStats: Map<string, EndpointStats> = new Map();

  // Subscribers
  private subscribers: Set<(stats: NetworkMonitorStats) => void> = new Set();

  private constructor() {
    this.enabled = import.meta.env.DEV;

    if (this.enabled) {
      this.interceptFetch();
    }
  }

  static getInstance(): NetworkMonitor {
    if (!NetworkMonitor.instance) {
      NetworkMonitor.instance = new NetworkMonitor();
    }
    return NetworkMonitor.instance;
  }

  // ============================================================================
  // Fetch Interception
  // ============================================================================

  private interceptFetch(): void {
    if (this.originalFetch) return; // Already intercepted

    this.originalFetch = window.fetch.bind(window);

    window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const requestId = this.generateRequestId();
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      const method = init?.method || "GET";

      // Add X-Request-ID header for trace correlation
      const headers = new Headers(init?.headers);
      if (!headers.has("X-Request-ID")) {
        headers.set("X-Request-ID", requestId);
      }
      const enhancedInit = { ...init, headers };

      // Start tracking
      this.startRequest(requestId, url, method);

      try {
        const response = await this.originalFetch!(input, enhancedInit);

        // End tracking with success
        this.endRequest(requestId, response.status);

        return response;
      } catch (error) {
        // End tracking with error
        this.endRequest(requestId, 0, getErrorMessage(error));
        throw error;
      }
    };
  }

  private generateRequestId(): string {
    return `req-${++this.requestIdCounter}-${Date.now()}`;
  }

  private startRequest(id: string, url: string, method: string): void {
    const record: RequestRecord = {
      id,
      url,
      method,
      startTime: performance.now(),
    };

    this.activeRequests.set(id, record);

    // Track in performance monitor
    getPerformanceMonitor().startNetworkRequest(id);

    this.notifySubscribers();
  }

  private endRequest(id: string, status: number, error?: string): void {
    const record = this.activeRequests.get(id);
    if (!record) return;

    record.endTime = performance.now();
    record.latency = record.endTime - record.startTime;
    record.status = status;
    record.error = error;

    this.activeRequests.delete(id);
    this.completedRequests.push(record);

    // Track in performance monitor
    getPerformanceMonitor().endNetworkRequest(id);

    // Update endpoint stats
    this.updateEndpointStats(record);

    // Keep only last 5 minutes of requests
    const fiveMinutesAgo = Date.now() - 5 * 60 * 1000;
    this.completedRequests = this.completedRequests.filter(
      (r) => r.startTime > fiveMinutesAgo - this.getStartTimeOffset(),
    );

    this.notifySubscribers();
  }

  private getStartTimeOffset(): number {
    // Offset from performance.now() to Date.now() at page load
    return Date.now() - performance.now();
  }

  private updateEndpointStats(record: RequestRecord): void {
    const normalizedUrl = this.normalizeUrl(record.url);
    const existing = this.endpointStats.get(normalizedUrl);
    const now = Date.now();

    if (existing) {
      existing.requestCount++;
      existing.averageLatency =
        (existing.averageLatency * (existing.requestCount - 1) + (record.latency || 0)) /
        existing.requestCount;
      existing.lastRequestTime = now;
      existing.requestsPerMinute = this.calculateRequestsPerMinute(normalizedUrl);
    } else {
      this.endpointStats.set(normalizedUrl, {
        url: normalizedUrl,
        requestCount: 1,
        averageLatency: record.latency || 0,
        lastRequestTime: now,
        requestsPerMinute: 1,
      });
    }
  }

  private normalizeUrl(url: string): string {
    try {
      const parsed = new URL(url, window.location.origin);
      // Remove query params and hash for grouping
      return `${parsed.pathname}`;
    } catch {
      return url;
    }
  }

  private calculateRequestsPerMinute(normalizedUrl: string): number {
    const oneMinuteAgo = performance.now() - 60 * 1000;
    return this.completedRequests.filter(
      (r) => this.normalizeUrl(r.url) === normalizedUrl && r.startTime > oneMinuteAgo,
    ).length;
  }

  // ============================================================================
  // Statistics
  // ============================================================================

  /**
   * Get current network monitoring stats
   */
  getStats(): NetworkMonitorStats {
    const now = performance.now();
    const oneMinuteAgo = now - 60 * 1000;

    const recentRequests = this.completedRequests.filter((r) => r.startTime > oneMinuteAgo);
    const latencies = this.completedRequests
      .filter((r) => r.latency !== undefined)
      .map((r) => r.latency!);

    const errorCount = this.completedRequests.filter(
      (r) => r.error || (r.status && r.status >= 400),
    ).length;

    return {
      totalRequests: this.completedRequests.length,
      activeRequests: this.activeRequests.size,
      requestsPerMinute: recentRequests.length,
      averageLatency:
        latencies.length > 0 ? latencies.reduce((a, b) => a + b, 0) / latencies.length : 0,
      errorRate: this.completedRequests.length > 0 ? errorCount / this.completedRequests.length : 0,
      endpointStats: new Map(this.endpointStats),
      recentRequests: [...this.completedRequests].slice(-50),
    };
  }

  /**
   * Get polling analysis for endpoints
   */
  getPollingAnalysis(): {
    endpoint: string;
    estimatedInterval: number;
    requestsPerMinute: number;
  }[] {
    const analysis: {
      endpoint: string;
      estimatedInterval: number;
      requestsPerMinute: number;
    }[] = [];

    this.endpointStats.forEach((stats, endpoint) => {
      if (stats.requestCount > 2) {
        // Find requests for this endpoint
        const endpointRequests = this.completedRequests
          .filter((r) => this.normalizeUrl(r.url) === endpoint)
          .sort((a, b) => a.startTime - b.startTime);

        if (endpointRequests.length >= 2) {
          // Calculate average interval between requests
          const intervals: number[] = [];
          for (let i = 1; i < endpointRequests.length; i++) {
            intervals.push(endpointRequests[i].startTime - endpointRequests[i - 1].startTime);
          }

          const avgInterval = intervals.reduce((a, b) => a + b, 0) / intervals.length;

          analysis.push({
            endpoint,
            estimatedInterval: Math.round(avgInterval),
            requestsPerMinute: stats.requestsPerMinute,
          });
        }
      }
    });

    return analysis.sort((a, b) => b.requestsPerMinute - a.requestsPerMinute);
  }

  // ============================================================================
  // Subscription
  // ============================================================================

  /**
   * Subscribe to stats updates
   */
  subscribe(callback: (stats: NetworkMonitorStats) => void): () => void {
    this.subscribers.add(callback);
    return () => this.subscribers.delete(callback);
  }

  private notifySubscribers(): void {
    if (this.subscribers.size === 0) return;
    const stats = this.getStats();
    this.subscribers.forEach((cb) => cb(stats));
  }

  // ============================================================================
  // Cleanup
  // ============================================================================

  /**
   * Reset all stats
   */
  reset(): void {
    this.completedRequests = [];
    this.endpointStats.clear();
    this.notifySubscribers();
  }

  /**
   * Restore original fetch and cleanup
   */
  destroy(): void {
    if (this.originalFetch) {
      window.fetch = this.originalFetch;
      this.originalFetch = null;
    }
    this.subscribers.clear();
    NetworkMonitor.instance = null;
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
 * Get the global network monitor instance
 */
export function getNetworkMonitor(): NetworkMonitor {
  return NetworkMonitor.getInstance();
}

export default NetworkMonitor;
