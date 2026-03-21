/* eslint-disable no-console */
/**
 * Performance Report Generator
 *
 * Generates comprehensive performance reports from collected metrics.
 * Supports JSON export and console summary.
 *
 * Only active in development mode.
 */

import { getPerformanceMonitor, type AggregatedMetrics } from "./performance";
import { getNetworkMonitor, type NetworkMonitorStats } from "./network-monitor";

// ============================================================================
// Types
// ============================================================================

export interface PerformanceReport {
  generated: string;
  uptime: number;
  summary: ReportSummary;
  network: NetworkReport;
  renders: RenderReport;
  webSocket: WebSocketReport;
  virtualScroll: VirtualScrollReport;
  memory: MemoryReport | null;
  pollingAnalysis: PollingAnalysisReport;
  recommendations: string[];
}

export interface ReportSummary {
  totalNetworkRequests: number;
  requestsPerMinute: number;
  averageLatency: number;
  totalRenders: number;
  averageRenderTime: number;
  webSocketMessages: number;
  webSocketLatency: number;
  memoryUsageMb: number | null;
}

export interface NetworkReport {
  totalRequests: number;
  activeRequests: number;
  requestsPerMinute: number;
  averageLatency: number;
  maxLatency: number;
  minLatency: number;
  errorRate: number;
  endpointBreakdown: { endpoint: string; count: number; avgLatency: number }[];
}

export interface RenderReport {
  totalComponents: number;
  totalRenders: number;
  averageRenderTime: number;
  slowestComponents: { name: string; avgTime: number; renderCount: number }[];
  mostFrequentRenders: { name: string; count: number; avgTime: number }[];
}

export interface WebSocketReport {
  messagesReceived: number;
  averageLatency: number;
  lastLatency: number;
  reconnectCount: number;
  isConnected: boolean;
  messageRate: number; // messages per minute
}

export interface VirtualScrollReport {
  visibleItems: number;
  totalItems: number;
  visibleRatio: number;
  scrollFps: number;
  memoryEfficiency: number; // estimated memory saved
}

export interface MemoryReport {
  usedHeapMb: number;
  totalHeapMb: number;
  utilizationPercent: number;
}

export interface PollingAnalysisReport {
  endpoints: { endpoint: string; estimatedIntervalMs: number; requestsPerMinute: number }[];
  totalPollingRequests: number;
  potentialSavings: string;
}

// ============================================================================
// Report Generator
// ============================================================================

/**
 * Generate a comprehensive performance report
 */
export function generatePerformanceReport(): PerformanceReport {
  const monitor = getPerformanceMonitor();
  const networkMonitor = getNetworkMonitor();

  const metrics = monitor.getMetrics();
  const networkStats = networkMonitor.getStats();
  const pollingAnalysis = networkMonitor.getPollingAnalysis();

  // Calculate render statistics
  const renderStats = calculateRenderStats(metrics);

  // Calculate WebSocket message rate
  const wsMessageRate =
    metrics.uptime > 0 ? metrics.webSocket.messagesReceived / (metrics.uptime / 60000) : 0;

  // Generate recommendations
  const recommendations = generateRecommendations(metrics, networkStats, pollingAnalysis);

  const report: PerformanceReport = {
    generated: new Date().toISOString(),
    uptime: metrics.uptime,
    summary: {
      totalNetworkRequests: networkStats.totalRequests,
      requestsPerMinute: networkStats.requestsPerMinute,
      averageLatency: Math.round(networkStats.averageLatency * 100) / 100,
      totalRenders: renderStats.totalRenders,
      averageRenderTime: Math.round(renderStats.averageRenderTime * 100) / 100,
      webSocketMessages: metrics.webSocket.messagesReceived,
      webSocketLatency: Math.round(metrics.webSocket.averageLatency * 100) / 100,
      memoryUsageMb: metrics.memory
        ? Math.round((metrics.memory.usedJSHeapSize / 1024 / 1024) * 100) / 100
        : null,
    },
    network: {
      totalRequests: networkStats.totalRequests,
      activeRequests: networkStats.activeRequests,
      requestsPerMinute: networkStats.requestsPerMinute,
      averageLatency: Math.round(networkStats.averageLatency * 100) / 100,
      maxLatency: Math.round(metrics.network.maxLatency * 100) / 100,
      minLatency: Math.round(metrics.network.minLatency * 100) / 100,
      errorRate: Math.round(networkStats.errorRate * 10000) / 100,
      endpointBreakdown: Array.from(networkStats.endpointStats.entries())
        .map(([endpoint, stats]) => ({
          endpoint,
          count: stats.requestCount,
          avgLatency: Math.round(stats.averageLatency * 100) / 100,
        }))
        .sort((a, b) => b.count - a.count)
        .slice(0, 10),
    },
    renders: {
      totalComponents: metrics.renders.size,
      totalRenders: renderStats.totalRenders,
      averageRenderTime: Math.round(renderStats.averageRenderTime * 100) / 100,
      slowestComponents: renderStats.slowestComponents,
      mostFrequentRenders: renderStats.mostFrequentRenders,
    },
    webSocket: {
      messagesReceived: metrics.webSocket.messagesReceived,
      averageLatency: Math.round(metrics.webSocket.averageLatency * 100) / 100,
      lastLatency: Math.round(metrics.webSocket.lastLatency * 100) / 100,
      reconnectCount: metrics.webSocket.reconnectCount,
      isConnected: metrics.webSocket.isConnected,
      messageRate: Math.round(wsMessageRate * 100) / 100,
    },
    virtualScroll: {
      visibleItems: metrics.virtualScroll.visibleItems,
      totalItems: metrics.virtualScroll.totalItems,
      visibleRatio: Math.round(metrics.virtualScroll.visibleRatio * 10000) / 100,
      scrollFps: metrics.virtualScroll.scrollFps,
      memoryEfficiency: calculateMemoryEfficiency(metrics.virtualScroll),
    },
    memory: metrics.memory
      ? {
          usedHeapMb: Math.round((metrics.memory.usedJSHeapSize / 1024 / 1024) * 100) / 100,
          totalHeapMb: Math.round((metrics.memory.totalJSHeapSize / 1024 / 1024) * 100) / 100,
          utilizationPercent: Math.round(metrics.memory.heapUtilization * 10000) / 100,
        }
      : null,
    pollingAnalysis: {
      endpoints: pollingAnalysis.map((p) => ({
        endpoint: p.endpoint,
        estimatedIntervalMs: p.estimatedInterval,
        requestsPerMinute: p.requestsPerMinute,
      })),
      totalPollingRequests: pollingAnalysis.reduce((sum, p) => sum + p.requestsPerMinute, 0),
      potentialSavings: calculatePotentialSavings(pollingAnalysis),
    },
    recommendations,
  };

  return report;
}

// ============================================================================
// Helper Functions
// ============================================================================

function calculateRenderStats(metrics: AggregatedMetrics): {
  totalRenders: number;
  averageRenderTime: number;
  slowestComponents: { name: string; avgTime: number; renderCount: number }[];
  mostFrequentRenders: { name: string; count: number; avgTime: number }[];
} {
  const renders = Array.from(metrics.renders.values());
  const totalRenders = renders.reduce((sum, r) => sum + r.renderCount, 0);
  const totalTime = renders.reduce((sum, r) => sum + r.totalRenderTime, 0);

  return {
    totalRenders,
    averageRenderTime: totalRenders > 0 ? totalTime / totalRenders : 0,
    slowestComponents: [...renders]
      .sort((a, b) => b.averageRenderTime - a.averageRenderTime)
      .slice(0, 5)
      .map((r) => ({
        name: r.componentName,
        avgTime: Math.round(r.averageRenderTime * 100) / 100,
        renderCount: r.renderCount,
      })),
    mostFrequentRenders: [...renders]
      .sort((a, b) => b.renderCount - a.renderCount)
      .slice(0, 5)
      .map((r) => ({
        name: r.componentName,
        count: r.renderCount,
        avgTime: Math.round(r.averageRenderTime * 100) / 100,
      })),
  };
}

function calculateMemoryEfficiency(scrollMetrics: {
  visibleItems: number;
  totalItems: number;
}): number {
  if (scrollMetrics.totalItems === 0) return 0;

  // Memory efficiency = percentage of items NOT rendered
  const unrenderedRatio =
    (scrollMetrics.totalItems - scrollMetrics.visibleItems) / scrollMetrics.totalItems;
  return Math.round(unrenderedRatio * 10000) / 100;
}

function calculatePotentialSavings(
  pollingAnalysis: { endpoint: string; estimatedInterval: number; requestsPerMinute: number }[],
): string {
  // Estimate how many requests could be saved by using WebSocket
  const totalPollingPerMinute = pollingAnalysis.reduce((sum, p) => sum + p.requestsPerMinute, 0);

  if (totalPollingPerMinute === 0) {
    return "No polling detected";
  }

  // Assume WebSocket reduces to ~1 connection
  const savings = totalPollingPerMinute - 1;
  const savingsPercent = Math.round((savings / totalPollingPerMinute) * 100);

  return `~${savingsPercent}% reduction possible (${Math.round(savings)} req/min saved)`;
}

function generateRecommendations(
  metrics: AggregatedMetrics,
  networkStats: NetworkMonitorStats,
  pollingAnalysis: { endpoint: string; estimatedInterval: number; requestsPerMinute: number }[],
): string[] {
  const recommendations: string[] = [];

  // Network recommendations
  if (networkStats.requestsPerMinute > 60) {
    recommendations.push(
      `High request rate (${networkStats.requestsPerMinute}/min). Consider reducing polling frequency or using WebSocket.`,
    );
  }

  if (networkStats.averageLatency > 500) {
    recommendations.push(
      `High average latency (${Math.round(networkStats.averageLatency)}ms). Check network conditions or backend performance.`,
    );
  }

  if (networkStats.errorRate > 0.05) {
    recommendations.push(
      `High error rate (${Math.round(networkStats.errorRate * 100)}%). Investigate failing endpoints.`,
    );
  }

  // Polling recommendations
  const frequentPolling = pollingAnalysis.filter((p) => p.estimatedInterval < 5000);
  if (frequentPolling.length > 0) {
    recommendations.push(
      `${frequentPolling.length} endpoints polling faster than 5s. Consider WebSocket for real-time updates.`,
    );
  }

  // Render recommendations
  const renders = Array.from(metrics.renders.values());
  const slowRenders = renders.filter((r) => r.averageRenderTime > 16);
  if (slowRenders.length > 0) {
    recommendations.push(
      `${slowRenders.length} components rendering slower than 16ms. Consider memoization or virtualization.`,
    );
  }

  const frequentRenders = renders.filter((r) => r.renderCount > 100);
  if (frequentRenders.length > 0) {
    recommendations.push(
      `${frequentRenders.length} components with 100+ renders. Check for unnecessary re-renders.`,
    );
  }

  // Virtual scroll recommendations
  if (metrics.virtualScroll.totalItems > 100 && metrics.virtualScroll.visibleRatio > 0.5) {
    recommendations.push(
      `Virtual scrolling only rendering ${Math.round(metrics.virtualScroll.visibleRatio * 100)}% of items. Check virtualization settings.`,
    );
  }

  if (metrics.virtualScroll.scrollFps < 30 && metrics.virtualScroll.scrollFps > 0) {
    recommendations.push(
      `Scroll FPS is ${metrics.virtualScroll.scrollFps}. Consider reducing item complexity or increasing overscan.`,
    );
  }

  // WebSocket recommendations
  if (metrics.webSocket.reconnectCount > 5) {
    recommendations.push(
      `WebSocket reconnected ${metrics.webSocket.reconnectCount} times. Check connection stability.`,
    );
  }

  // Memory recommendations
  if (metrics.memory && metrics.memory.heapUtilization > 0.8) {
    recommendations.push(
      `High memory utilization (${Math.round(metrics.memory.heapUtilization * 100)}%). Consider cleaning up unused data.`,
    );
  }

  if (recommendations.length === 0) {
    recommendations.push("No performance issues detected. Keep monitoring!");
  }

  return recommendations;
}

// ============================================================================
// Export Functions
// ============================================================================

/**
 * Export performance report as JSON
 */
export function exportReportAsJson(): string {
  const report = generatePerformanceReport();
  return JSON.stringify(report, null, 2);
}

/**
 * Download performance report as JSON file
 */
export function downloadReport(): void {
  const json = exportReportAsJson();
  const blob = new Blob([json], { type: "application/json" });
  const url = URL.createObjectURL(blob);

  const a = document.createElement("a");
  a.href = url;
  a.download = `performance-report-${new Date().toISOString().slice(0, 19).replace(/:/g, "-")}.json`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

/**
 * Log performance summary to console
 */
export function logPerformanceSummary(): void {
  if (!import.meta.env.DEV) return;

  const report = generatePerformanceReport();

  console.group("%c Performance Report", "font-weight: bold; color: #4CAF50;");

  console.log("%c Summary", "font-weight: bold; color: #2196F3;");
  console.table(report.summary);

  console.log("%c Network", "font-weight: bold; color: #FF9800;");
  console.log(`  Requests/min: ${report.network.requestsPerMinute}`);
  console.log(`  Avg latency: ${report.network.averageLatency}ms`);
  console.log(`  Error rate: ${report.network.errorRate}%`);

  console.log("%c Renders", "font-weight: bold; color: #9C27B0;");
  console.log(`  Total renders: ${report.renders.totalRenders}`);
  console.log(`  Avg render time: ${report.renders.averageRenderTime}ms`);

  if (report.renders.slowestComponents.length > 0) {
    console.log("  Slowest components:");
    console.table(report.renders.slowestComponents);
  }

  console.log("%c WebSocket", "font-weight: bold; color: #00BCD4;");
  console.log(`  Connected: ${report.webSocket.isConnected}`);
  console.log(`  Messages: ${report.webSocket.messagesReceived}`);
  console.log(`  Avg latency: ${report.webSocket.averageLatency}ms`);
  console.log(`  Reconnects: ${report.webSocket.reconnectCount}`);

  console.log("%c Virtual Scroll", "font-weight: bold; color: #E91E63;");
  console.log(`  Visible: ${report.virtualScroll.visibleItems}/${report.virtualScroll.totalItems}`);
  console.log(`  Memory efficiency: ${report.virtualScroll.memoryEfficiency}%`);
  console.log(`  Scroll FPS: ${report.virtualScroll.scrollFps}`);

  if (report.pollingAnalysis.endpoints.length > 0) {
    console.log("%c Polling Analysis", "font-weight: bold; color: #795548;");
    console.table(report.pollingAnalysis.endpoints);
    console.log(`  Potential savings: ${report.pollingAnalysis.potentialSavings}`);
  }

  console.log("%c Recommendations", "font-weight: bold; color: #F44336;");
  report.recommendations.forEach((r, i) => console.log(`  ${i + 1}. ${r}`));

  console.groupEnd();
}

export default generatePerformanceReport;
