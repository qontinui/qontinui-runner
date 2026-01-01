/**
 * FindingsQuery Module
 *
 * Handles filtering and querying findings.
 * Provides pure functions for finding operations.
 */

import type { Finding, FindingSeverity, FindingStatus } from "../types/findings";

/**
 * Get all findings from a Map as an array
 */
export function getAllFindings(findingsMap: Map<string, Finding>): Finding[] {
  return Array.from(findingsMap.values());
}

/**
 * Get findings for a specific session
 */
export function getSessionFindings(
  findingsMap: Map<string, Finding>,
  sessionId: string,
): Finding[] {
  return getAllFindings(findingsMap).filter((f) => f.sourceSessionId === sessionId);
}

/**
 * Get findings by category
 */
export function getFindingsByCategory(
  findingsMap: Map<string, Finding>,
  categoryId: string,
): Finding[] {
  return getAllFindings(findingsMap).filter((f) => f.categoryId === categoryId);
}

/**
 * Get findings by status
 */
export function getFindingsByStatus(
  findingsMap: Map<string, Finding>,
  status: FindingStatus,
): Finding[] {
  return getAllFindings(findingsMap).filter((f) => f.status === status);
}

/**
 * Get findings by severity
 */
export function getFindingsBySeverity(
  findingsMap: Map<string, Finding>,
  severity: FindingSeverity,
): Finding[] {
  return getAllFindings(findingsMap).filter((f) => f.severity === severity);
}

/**
 * Get findings needing user input
 */
export function getFindingsNeedingInput(findingsMap: Map<string, Finding>): Finding[] {
  return getAllFindings(findingsMap).filter((f) => f.status === "needs_input" && f.pendingQuestion);
}

/**
 * Get actionable findings (not resolved or wont_fix)
 */
export function getActionableFindings(findingsMap: Map<string, Finding>): Finding[] {
  return getAllFindings(findingsMap).filter(
    (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
  );
}

/**
 * Get unresolved findings count
 */
export function getUnresolvedCount(findingsMap: Map<string, Finding>): number {
  return getAllFindings(findingsMap).filter(
    (f) => f.status !== "resolved" && f.status !== "wont_fix",
  ).length;
}

/**
 * Filter findings by multiple criteria
 */
export function filterFindings(
  findingsMap: Map<string, Finding>,
  filters: {
    sessionId?: string;
    categoryId?: string;
    status?: FindingStatus | FindingStatus[];
    severity?: FindingSeverity | FindingSeverity[];
    actionable?: boolean;
    hasInput?: boolean;
  },
): Finding[] {
  let results = getAllFindings(findingsMap);

  if (filters.sessionId) {
    results = results.filter((f) => f.sourceSessionId === filters.sessionId);
  }

  if (filters.categoryId) {
    results = results.filter((f) => f.categoryId === filters.categoryId);
  }

  if (filters.status) {
    const statuses = Array.isArray(filters.status) ? filters.status : [filters.status];
    results = results.filter((f) => statuses.includes(f.status));
  }

  if (filters.severity) {
    const severities = Array.isArray(filters.severity) ? filters.severity : [filters.severity];
    results = results.filter((f) => severities.includes(f.severity));
  }

  if (filters.actionable !== undefined) {
    results = results.filter((f) => f.actionable === filters.actionable);
  }

  if (filters.hasInput !== undefined) {
    results = results.filter((f) =>
      filters.hasInput ? f.pendingQuestion !== undefined : f.pendingQuestion === undefined,
    );
  }

  return results;
}

/**
 * Sort findings by various criteria
 */
export function sortFindings(
  findings: Finding[],
  sortBy: "detectedAt" | "severity" | "status" | "category" = "detectedAt",
  order: "asc" | "desc" = "desc",
): Finding[] {
  const severityOrder: Record<FindingSeverity, number> = {
    critical: 5,
    high: 4,
    medium: 3,
    low: 2,
    info: 1,
  };

  const statusOrder: Record<FindingStatus, number> = {
    needs_input: 6,
    detected: 5,
    in_progress: 4,
    deferred: 3,
    wont_fix: 2,
    resolved: 1,
  };

  const sorted = [...findings].sort((a, b) => {
    let comparison = 0;

    switch (sortBy) {
      case "detectedAt":
        comparison = a.detectedAt - b.detectedAt;
        break;
      case "severity":
        comparison = severityOrder[a.severity] - severityOrder[b.severity];
        break;
      case "status":
        comparison = statusOrder[a.status] - statusOrder[b.status];
        break;
      case "category":
        comparison = a.categoryId.localeCompare(b.categoryId);
        break;
    }

    return order === "asc" ? comparison : -comparison;
  });

  return sorted;
}

/**
 * Group findings by a field
 */
export function groupFindings<K extends keyof Finding>(
  findings: Finding[],
  groupBy: K,
): Map<Finding[K], Finding[]> {
  const groups = new Map<Finding[K], Finding[]>();

  for (const finding of findings) {
    const key = finding[groupBy];
    const existing = groups.get(key) || [];
    existing.push(finding);
    groups.set(key, existing);
  }

  return groups;
}
