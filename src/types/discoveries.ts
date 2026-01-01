/**
 * TypeScript types for the Discovery Push mechanism.
 * Mirrors the Rust types in src-tauri/src/discoveries/
 */

/**
 * Generic response wrapper for discovery commands.
 */
export interface DiscoveryResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/**
 * Types of discoveries that can be detected.
 */
export type DiscoveryType =
  | "new_element"
  | "new_transition"
  | "timing_update"
  | "flaky_detection"
  | "unexpected_element";

/**
 * Evidence supporting a discovery.
 */
export interface DiscoveryEvidence {
  runs_observed: number;
  first_seen: string;
  last_seen: string;
  confidence_avg?: number;
  sample_screenshots?: string[];
}

/**
 * Payload for a discovery.
 */
export interface DiscoveryPayload {
  runner_id: string;
  runner_name?: string;
  project_id: string;
  config_id: string;
  config_name?: string;
  discovery_type: DiscoveryType;
  title: string;
  description: string;
  confidence: number;
  runs_observed: number;
  evidence: DiscoveryEvidence;
  suggested_changes?: unknown;
}

/**
 * A pending discovery awaiting sync.
 */
export interface PendingDiscovery {
  id: string;
  payload: DiscoveryPayload;
  created_at: string;
  last_attempt?: string;
  attempt_count: number;
  error?: string;
}

/**
 * Preview of a discovery for list display.
 */
export interface DiscoveryPreview {
  id: string;
  discovery_type: string;
  title: string;
  confidence: number;
  runs_observed: number;
  created_at: string;
  attempt_count: number;
  last_error?: string;
}

/**
 * Summary of pending discoveries for the UI.
 */
export interface DiscoverySummary {
  pending_count: number;
  ready_for_sync: number;
  can_sync: boolean;
  recent: DiscoveryPreview[];
}

/**
 * Current sync status.
 */
export interface SyncStatus {
  pending_count: number;
  ready_for_retry: number;
  authenticated: boolean;
}

/**
 * Result of a sync operation.
 */
export interface SyncResult {
  sent: number;
  failed: number;
  errors: string[];
  remaining: number;
}

// Helper functions

/**
 * Get human-readable discovery type label.
 */
export function getDiscoveryTypeLabel(type: DiscoveryType): string {
  const labels: Record<DiscoveryType, string> = {
    new_element: "New Element",
    new_transition: "New Transition",
    timing_update: "Timing Update",
    flaky_detection: "Flaky Detection",
    unexpected_element: "Unexpected Element",
  };
  return labels[type] || type;
}

/**
 * Get discovery type icon color.
 */
export function getDiscoveryTypeColor(type: DiscoveryType): string {
  const colors: Record<DiscoveryType, string> = {
    new_element: "text-green-500",
    new_transition: "text-blue-500",
    timing_update: "text-yellow-500",
    flaky_detection: "text-orange-500",
    unexpected_element: "text-red-500",
  };
  return colors[type] || "text-muted-foreground";
}

/**
 * Format confidence as percentage.
 */
export function formatConfidence(confidence: number): string {
  return `${Math.round(confidence * 100)}%`;
}
