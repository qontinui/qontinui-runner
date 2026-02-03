/**
 * Library Barrel Export
 *
 * Central export point for utility libraries.
 */

// Core utilities
export { cn } from "./utils";
export { APP_VERSION } from "./appInfo";

// Performance monitoring (dev mode only)
export { PerformanceMonitor, getPerformanceMonitor, measure, measureAsync } from "./performance";
export type {
  PerformanceMeasure,
  NetworkMetrics,
  RenderMetrics,
  WebSocketMetrics,
  VirtualScrollMetrics,
  MemoryMetrics,
  AggregatedMetrics,
} from "./performance";

export { NetworkMonitor, getNetworkMonitor } from "./network-monitor";
export type { RequestRecord, EndpointStats, NetworkMonitorStats } from "./network-monitor";

export {
  generatePerformanceReport,
  exportReportAsJson,
  downloadReport,
  logPerformanceSummary,
} from "./performance-report";
export type {
  PerformanceReport,
  ReportSummary,
  NetworkReport,
  RenderReport,
  WebSocketReport,
  VirtualScrollReport,
  MemoryReport,
  PollingAnalysisReport,
} from "./performance-report";
