/**
 * useFindings Hook
 *
 * React hook for accessing the FindingsTracker.
 * Provides reactive state updates when findings change.
 *
 * @example
 * ```tsx
 * function MyComponent() {
 *   const {
 *     items,
 *     summary,
 *     session,
 *     resolveItem,
 *     updateItemStatus,
 *     clearSession,
 *   } = useFindings();
 *
 *   return (
 *     <div>
 *       <h2>Session: {session?.name}</h2>
 *       <p>Total items: {summary.total}</p>
 *       {items.map(item => (
 *         <div key={item.id}>
 *           {item.title} - {item.status}
 *           <button onClick={() => resolveItem(item.id, 'Fixed')}>
 *             Resolve
 *           </button>
 *         </div>
 *       ))}
 *     </div>
 *   );
 * }
 * ```
 */

import { useState, useEffect, useCallback } from "react";
import { findingsTracker } from "../services/FindingsTracker";
import { sessionManager, type SessionContext } from "../services/SessionManager";
import type { Finding, FindingStatus, FindingSeverity } from "../types/findings";

/** Status types for findings */
export type FindingStatusFilter =
  | "detected"
  | "in_progress"
  | "needs_input"
  | "resolved"
  | "wont_fix"
  | "deferred";

export interface UseUnifiedReportOptions {
  /** Filter by status */
  statusFilter?: FindingStatusFilter | FindingStatusFilter[];
  /** Filter by severity */
  severityFilter?: FindingSeverity | FindingSeverity[];
  /** Only show actionable items */
  actionableOnly?: boolean;
  /** Only show items needing input */
  needsInputOnly?: boolean;
}

export interface FindingsSummary {
  sessionId: string;
  total: number;
  byStatus: Record<FindingStatus, number>;
  bySeverity: Record<FindingSeverity, number>;
  actionable: number;
  needsInput: number;
  resolved: number;
}

export interface UseUnifiedReportResult {
  /** Current session context */
  session: SessionContext | null;
  /** All findings for the current session */
  items: Finding[];
  /** Summary statistics */
  summary: FindingsSummary;
  /** Total count */
  count: number;
  /** Unresolved count */
  unresolvedCount: number;
  /** Whether there are any items */
  hasItems: boolean;
  /** Whether there are actionable items */
  hasActionable: boolean;
  /** Whether any items need user input */
  hasNeedsInput: boolean;
  /** Resolve a finding */
  resolveItem: (id: string, resolution: string) => boolean;
  /** Update a finding's status */
  updateItemStatus: (id: string, status: FindingStatus) => boolean;
  /** Provide user response for a finding needing input */
  provideUserResponse: (id: string, response: string) => boolean;
  /** Clear all findings for the session */
  clearSession: () => void;
  /** Force refresh the data */
  refresh: () => void;
}

/**
 * Calculate summary statistics from findings
 */
function calculateSummary(findings: Finding[], sessionId: string): FindingsSummary {
  const summary: FindingsSummary = {
    sessionId,
    total: findings.length,
    byStatus: {
      detected: 0,
      in_progress: 0,
      needs_input: 0,
      resolved: 0,
      wont_fix: 0,
      deferred: 0,
    },
    bySeverity: {
      critical: 0,
      high: 0,
      medium: 0,
      low: 0,
      info: 0,
    },
    actionable: 0,
    needsInput: 0,
    resolved: 0,
  };

  for (const finding of findings) {
    summary.byStatus[finding.status]++;
    summary.bySeverity[finding.severity]++;
    if (finding.actionable && finding.status !== "resolved" && finding.status !== "wont_fix") {
      summary.actionable++;
    }
    if (finding.status === "needs_input") {
      summary.needsInput++;
    }
    if (finding.status === "resolved") {
      summary.resolved++;
    }
  }

  return summary;
}

/**
 * Hook for accessing findings data reactively
 */
export function useUnifiedReport(options: UseUnifiedReportOptions = {}): UseUnifiedReportResult {
  const { statusFilter, severityFilter, actionableOnly = false, needsInputOnly = false } = options;

  // State
  const [items, setItems] = useState<Finding[]>([]);
  const [session, setSession] = useState<SessionContext | null>(sessionManager.getCurrentSession());
  const [refreshCounter, setRefreshCounter] = useState(0);

  // Refresh function
  const refresh = useCallback(() => {
    setRefreshCounter((c) => c + 1);
  }, []);

  // Load and filter items
  useEffect(() => {
    let allItems = findingsTracker.getSessionFindings();

    // Apply status filter
    if (statusFilter) {
      const statuses = Array.isArray(statusFilter) ? statusFilter : [statusFilter];
      allItems = allItems.filter((item) => statuses.includes(item.status));
    }

    // Apply severity filter
    if (severityFilter) {
      const severities = Array.isArray(severityFilter) ? severityFilter : [severityFilter];
      allItems = allItems.filter((item) => severities.includes(item.severity));
    }

    // Apply actionable filter
    if (actionableOnly) {
      allItems = allItems.filter((item) => item.actionable);
    }

    // Apply needs input filter
    if (needsInputOnly) {
      allItems = allItems.filter((item) => item.status === "needs_input");
    }

    setItems(allItems);
    setSession(sessionManager.getCurrentSession());
  }, [refreshCounter, statusFilter, severityFilter, actionableOnly, needsInputOnly]);

  // Subscribe to findings changes
  useEffect(() => {
    const unsubscribe = findingsTracker.subscribe(() => {
      refresh();
    });

    return unsubscribe;
  }, [refresh]);

  // Subscribe to session changes
  useEffect(() => {
    const unsubscribe = sessionManager.subscribe((newSession) => {
      setSession(newSession);
      refresh();
    });

    return unsubscribe;
  }, [refresh]);

  // Computed values
  const sessionId = session?.id || "";
  const summary = calculateSummary(items, sessionId);
  const count = items.length;
  const unresolvedCount = items.filter(
    (item) => item.actionable && item.status !== "resolved" && item.status !== "wont_fix",
  ).length;
  const hasItems = count > 0;
  const hasActionable = unresolvedCount > 0;
  const hasNeedsInput = items.some((item) => item.status === "needs_input");

  // Action callbacks
  const resolveItem = useCallback((id: string, resolution: string): boolean => {
    const result = findingsTracker.resolveFinding(id, resolution);
    return result !== null;
  }, []);

  const updateItemStatus = useCallback((id: string, status: FindingStatus): boolean => {
    const result = findingsTracker.updateFindingStatus(id, status);
    return result !== null;
  }, []);

  const provideUserResponse = useCallback((id: string, response: string): boolean => {
    const result = findingsTracker.provideUserResponse(id, response);
    return result !== null;
  }, []);

  const clearSession = useCallback(() => {
    findingsTracker.clearSession();
  }, []);

  return {
    session,
    items,
    summary,
    count,
    unresolvedCount,
    hasItems,
    hasActionable,
    hasNeedsInput,
    resolveItem,
    updateItemStatus,
    provideUserResponse,
    clearSession,
    refresh,
  };
}

/**
 * Hook for accessing findings (alias for useUnifiedReport)
 */
export function useFindings(options: UseUnifiedReportOptions = {}): UseUnifiedReportResult {
  return useUnifiedReport(options);
}
