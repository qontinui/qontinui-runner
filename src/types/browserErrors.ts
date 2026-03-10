/**
 * browserErrors.ts
 *
 * Type definitions for browser-side error data fetched from the
 * UI Bridge SDK console endpoints.
 *
 * These types mirror the shapes returned by the runner proxy at:
 *   GET /ui-bridge/sdk/console/health
 *   GET /ui-bridge/sdk/console/browser-events
 */

// =============================================================================
// Health Report Types
// =============================================================================

/** Overall health status of the connected app */
export type BrowserHealthStatus = "healthy" | "degraded" | "broken";

/** Severity classification from the UI Bridge error severity system */
export type BrowserErrorSeverity = "crash" | "error" | "warning" | "noise";

/** Health report returned by /ui-bridge/sdk/console/health */
export interface BrowserHealthReport {
  /** Overall status */
  status: BrowserHealthStatus;
  /** Numeric score 0-100 (100 = perfectly healthy) */
  score: number;
  /** Human-readable summary */
  summary: string;
  /** Breakdown of recent events by severity */
  breakdown: {
    crashes: number;
    errors: number;
    warnings: number;
  };
  /** Error rate: errors per minute over the assessment window */
  errorRate: number;
  /** Most critical recent issue, if any */
  topIssue?: {
    message: string;
    severity: BrowserErrorSeverity;
    timestamp: number;
  };
  /** Assessment window in milliseconds */
  windowMs: number;
  /** Timestamp of this assessment */
  timestamp: number;
}

// =============================================================================
// Browser Event Types
// =============================================================================

/** A captured browser event from the UI Bridge SDK */
export interface BrowserCapturedEvent {
  /** Event type discriminator */
  type: string;
  /** When the event was captured (unix ms) */
  timestamp: number;
  /** Page URL when the event occurred */
  url: string;
  /** Console message (for console events) */
  message?: string;
  /** Console level (for console events) */
  level?: string;
  /** Stack trace (for error events) */
  stack?: string;
  /** HTTP method (for network events) */
  method?: string;
  /** Request URL (for network events) */
  requestUrl?: string;
  /** HTTP status code (for network events) */
  status?: number;
  /** Error message (for network/react-error events) */
  errorMessage?: string;
  /** Tag name (for resource-error events) */
  tagName?: string;
  /** Resource URL (for resource-error events) */
  resourceUrl?: string;
  /** Duration in ms (for long-task events) */
  durationMs?: number;
  /** Component stack (for react-error events) */
  componentStack?: string;
}

/** A deduplicated event group */
export interface FingerprintedEvent {
  /** Unique fingerprint for this error group */
  fingerprint: string;
  /** Number of occurrences */
  count: number;
  /** First occurrence timestamp */
  firstSeen: number;
  /** Last occurrence timestamp */
  lastSeen: number;
  /** Representative event */
  event: BrowserCapturedEvent;
}

/** Browser events response from /ui-bridge/sdk/console/browser-events */
export interface BrowserEventsResponse {
  /** Raw events (filtered by params) */
  events: BrowserCapturedEvent[];
  /** Total count of returned events */
  count: number;
  /** Deduplicated event groups (only when deduplicate=true) */
  deduplicated?: FingerprintedEvent[];
  /** Number of unique fingerprints (only when deduplicate=true) */
  uniqueCount?: number;
}

// =============================================================================
// Error Snapshot Types
// =============================================================================

/** Page state captured at the time of an error */
export interface ErrorSnapshotPageState {
  url: string;
  title: string;
  elementCount: number;
  /** Text from elements matching common error patterns */
  visibleErrors: string[];
}

/** An error snapshot captured automatically when a significant error occurs */
export interface ErrorSnapshot {
  id: string;
  error: {
    message: string;
    severity: BrowserErrorSeverity;
    fingerprint: string;
    sourceLocation?: string;
    stack?: string;
    timestamp: number;
  };
  pageState: ErrorSnapshotPageState;
  recentActions: string[];
  capturedAt: number;
}

// =============================================================================
// Composite Report Type
// =============================================================================

/** Composite browser error report combining health, events, and snapshots */
export interface BrowserErrorReport {
  health: BrowserHealthReport;
  recentErrors: BrowserCapturedEvent[];
  snapshots: ErrorSnapshot[];
}
