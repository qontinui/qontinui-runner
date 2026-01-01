/**
 * useUnifiedReport Hook
 *
 * React hook for accessing the UnifiedReportService.
 * Provides reactive state updates when findings/issues change.
 *
 * This hook makes it easy for UI components to migrate from
 * using issueTracker/findingsTracker directly to the unified API.
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
 *   } = useUnifiedReport();
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

import { useState, useEffect, useCallback, useMemo } from "react";
import {
  unifiedReportService,
  type UnifiedReportItem,
  type UnifiedReportSummary,
  type UnifiedStatus,
} from "../services/UnifiedReportService";
import type { SessionContext } from "../services/SessionManager";

export interface UseUnifiedReportOptions {
  /** Filter to only show findings */
  findingsOnly?: boolean;
  /** Filter to only show issues */
  issuesOnly?: boolean;
  /** Filter by status */
  statusFilter?: UnifiedStatus | UnifiedStatus[];
  /** Filter by severity */
  severityFilter?: string | string[];
  /** Only show actionable items */
  actionableOnly?: boolean;
  /** Only show items needing input */
  needsInputOnly?: boolean;
}

export interface UseUnifiedReportResult {
  /** Current session context */
  session: SessionContext | null;
  /** All items (findings + issues) for the current session */
  items: UnifiedReportItem[];
  /** Summary statistics */
  summary: UnifiedReportSummary;
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
  /** Resolve an item */
  resolveItem: (id: string, resolution: string) => boolean;
  /** Update an item's status */
  updateItemStatus: (id: string, status: UnifiedStatus) => boolean;
  /** Provide user response for an item needing input */
  provideUserResponse: (id: string, response: string) => boolean;
  /** Clear all items for the session */
  clearSession: () => void;
  /** Force refresh the data */
  refresh: () => void;
}

/**
 * Hook for accessing unified report data reactively
 */
export function useUnifiedReport(options: UseUnifiedReportOptions = {}): UseUnifiedReportResult {
  const {
    findingsOnly = false,
    issuesOnly = false,
    statusFilter,
    severityFilter,
    actionableOnly = false,
    needsInputOnly = false,
  } = options;

  // State
  const [items, setItems] = useState<UnifiedReportItem[]>([]);
  const [summary, setSummary] = useState<UnifiedReportSummary>(
    unifiedReportService.getSessionSummary(),
  );
  const [session, setSession] = useState<SessionContext | null>(
    unifiedReportService.getCurrentSession(),
  );
  const [refreshCounter, setRefreshCounter] = useState(0);

  // Refresh function
  const refresh = useCallback(() => {
    setRefreshCounter((c) => c + 1);
  }, []);

  // Load and filter items
  useEffect(() => {
    let allItems = unifiedReportService.getSessionItems();

    // Apply type filter
    if (findingsOnly) {
      allItems = allItems.filter((item) => item.type === "finding");
    } else if (issuesOnly) {
      allItems = allItems.filter((item) => item.type === "issue");
    }

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
      allItems = allItems.filter((item) => item.needsInput);
    }

    setItems(allItems);
    setSummary(unifiedReportService.getSessionSummary());
    setSession(unifiedReportService.getCurrentSession());
  }, [
    refreshCounter,
    findingsOnly,
    issuesOnly,
    statusFilter,
    severityFilter,
    actionableOnly,
    needsInputOnly,
  ]);

  // Subscribe to changes
  useEffect(() => {
    const unsubscribe = unifiedReportService.subscribe((event) => {
      // Update session on session change
      if (event.type === "session_changed") {
        setSession(event.session || null);
      }
      // Refresh on any item change
      refresh();
    });

    return unsubscribe;
  }, [refresh]);

  // Computed values
  const count = items.length;
  const unresolvedCount = items.filter((item) => item.actionable).length;
  const hasItems = count > 0;
  const hasActionable = unresolvedCount > 0;
  const hasNeedsInput = items.some((item) => item.needsInput);

  // Action callbacks
  const resolveItem = useCallback((id: string, resolution: string): boolean => {
    const result = unifiedReportService.resolveItem(id, resolution);
    return result;
  }, []);

  const updateItemStatus = useCallback((id: string, status: UnifiedStatus): boolean => {
    const result = unifiedReportService.updateItemStatus(id, status);
    return result;
  }, []);

  const provideUserResponse = useCallback((id: string, response: string): boolean => {
    const result = unifiedReportService.provideUserResponse(id, response);
    return result;
  }, []);

  const clearSession = useCallback(() => {
    unifiedReportService.clearSession();
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
 * Hook for accessing only findings
 */
export function useFindings(
  options: Omit<UseUnifiedReportOptions, "findingsOnly" | "issuesOnly"> = {},
): UseUnifiedReportResult {
  return useUnifiedReport({ ...options, findingsOnly: true });
}

/**
 * Hook for accessing only issues
 */
export function useIssues(
  options: Omit<UseUnifiedReportOptions, "findingsOnly" | "issuesOnly"> = {},
): UseUnifiedReportResult {
  return useUnifiedReport({ ...options, issuesOnly: true });
}
