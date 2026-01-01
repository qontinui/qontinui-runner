/**
 * UnifiedReportService
 *
 * Provides a unified interface for accessing findings and issues.
 * Aggregates data from both FindingsTracker and IssueTracker,
 * presenting a consistent API for UI components.
 *
 * This service is part of the migration path from IssueTracker to FindingsTracker.
 * It allows UI components to use a single API while both systems are active.
 */

import type { Finding, FindingSeverity, FindingStatus, ReportSummary } from "../types/findings";
import type { DetectedIssue, IssueSessionSummary } from "../types/issues";
import { findingsTracker } from "./FindingsTracker";
import { issueTracker } from "./IssueTracker";
import { sessionManager, type SessionContext } from "./SessionManager";

// ============================================================================
// Types
// ============================================================================

/**
 * A unified item that can represent either a Finding or an Issue
 */
export interface UnifiedReportItem {
  id: string;
  type: "finding" | "issue";
  categoryId: string;
  severity: UnifiedSeverity;
  status: UnifiedStatus;
  title: string;
  description: string;
  sessionId?: string;
  detectedAt: number;
  resolvedAt?: number;
  resolution?: string;
  file?: string;
  line?: number;
  actionable: boolean;
  needsInput: boolean;
  // Original data for type-specific operations
  originalFinding?: Finding;
  originalIssue?: DetectedIssue;
}

/**
 * Unified severity that maps both Finding and Issue severities
 */
export type UnifiedSeverity = "critical" | "high" | "medium" | "low" | "info";

/**
 * Unified status that maps both Finding and Issue statuses
 */
export type UnifiedStatus =
  | "detected"
  | "in_progress"
  | "needs_input"
  | "resolved"
  | "skipped"
  | "wont_fix"
  | "deferred";

/**
 * Unified summary statistics
 */
export interface UnifiedReportSummary {
  sessionId: string;
  total: number;
  findings: number;
  issues: number;
  byStatus: Record<UnifiedStatus, number>;
  bySeverity: Record<UnifiedSeverity, number>;
  actionable: number;
  needsInput: number;
  resolved: number;
}

/**
 * Event types for unified report changes
 */
export type UnifiedReportEventType =
  | "item_added"
  | "item_updated"
  | "item_resolved"
  | "item_removed"
  | "items_cleared"
  | "session_changed";

export interface UnifiedReportEvent {
  type: UnifiedReportEventType;
  item?: UnifiedReportItem;
  items?: UnifiedReportItem[];
  session?: SessionContext | null;
}

type UnifiedReportListener = (event: UnifiedReportEvent) => void;

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Convert an Issue severity to unified severity
 */
function normalizeIssueSeverity(severity: string): UnifiedSeverity {
  const map: Record<string, UnifiedSeverity> = {
    critical: "critical",
    high: "high",
    medium: "medium",
    low: "low",
  };
  return map[severity] || "medium";
}

/**
 * Convert a Finding severity to unified severity
 */
function normalizeFindingSeverity(severity: FindingSeverity): UnifiedSeverity {
  return severity as UnifiedSeverity;
}

/**
 * Convert an Issue status to unified status
 */
function normalizeIssueStatus(status: string): UnifiedStatus {
  const map: Record<string, UnifiedStatus> = {
    detected: "detected",
    in_progress: "in_progress",
    resolved: "resolved",
    skipped: "skipped",
  };
  return map[status] || "detected";
}

/**
 * Convert a Finding status to unified status
 */
function normalizeFindingStatus(status: FindingStatus): UnifiedStatus {
  return status as UnifiedStatus;
}

/**
 * Convert an Issue to a UnifiedReportItem
 */
function issueToUnified(issue: DetectedIssue): UnifiedReportItem {
  return {
    id: `issue-${issue.id}`,
    type: "issue",
    categoryId: issue.type, // Map issue type to category
    severity: normalizeIssueSeverity(issue.severity),
    status: normalizeIssueStatus(issue.status),
    title: issue.title,
    description: issue.description,
    sessionId: issue.session_id,
    detectedAt: issue.detected_at,
    resolvedAt: issue.resolved_at,
    resolution: issue.resolution,
    file: issue.file,
    line: issue.line,
    actionable: issue.status !== "resolved" && issue.status !== "skipped",
    needsInput: false, // Issues don't have the needs_input concept
    originalIssue: issue,
  };
}

/**
 * Convert a Finding to a UnifiedReportItem
 */
function findingToUnified(finding: Finding): UnifiedReportItem {
  return {
    id: `finding-${finding.id}`,
    type: "finding",
    categoryId: finding.categoryId,
    severity: normalizeFindingSeverity(finding.severity),
    status: normalizeFindingStatus(finding.status),
    title: finding.title,
    description: finding.description,
    sessionId: finding.sourceSessionId,
    detectedAt: finding.detectedAt,
    resolvedAt: finding.resolvedAt,
    resolution: finding.resolution,
    file: finding.codeContext?.file,
    line: finding.codeContext?.line,
    actionable: finding.actionable,
    needsInput: finding.status === "needs_input",
    originalFinding: finding,
  };
}

// ============================================================================
// UnifiedReportService Class
// ============================================================================

export class UnifiedReportService {
  private static instance: UnifiedReportService;
  private listeners: Set<UnifiedReportListener> = new Set();
  private unsubscribers: Array<() => void> = [];

  private constructor() {
    this.setupSubscriptions();
  }

  /**
   * Get the singleton instance
   */
  static getInstance(): UnifiedReportService {
    if (!UnifiedReportService.instance) {
      UnifiedReportService.instance = new UnifiedReportService();
    }
    return UnifiedReportService.instance;
  }

  /**
   * Setup subscriptions to underlying services
   */
  private setupSubscriptions(): void {
    // Subscribe to FindingsTracker events
    this.unsubscribers.push(
      findingsTracker.subscribe((event) => {
        if (event.finding) {
          const unifiedItem = findingToUnified(event.finding);
          this.emit({
            type: this.mapFindingsEventType(event.type),
            item: unifiedItem,
          });
        } else if (event.type === "findings_cleared") {
          this.emit({ type: "items_cleared" });
        }
      }),
    );

    // Subscribe to IssueTracker events
    this.unsubscribers.push(
      issueTracker.subscribe((event) => {
        if (event.issue) {
          const unifiedItem = issueToUnified(event.issue);
          this.emit({
            type: this.mapIssueEventType(event.type),
            item: unifiedItem,
          });
        } else if (event.type === "issues_cleared") {
          this.emit({ type: "items_cleared" });
        }
      }),
    );

    // Subscribe to SessionManager events
    this.unsubscribers.push(
      sessionManager.subscribe((session, _previous) => {
        this.emit({
          type: "session_changed",
          session,
        });
      }),
    );
  }

  /**
   * Map FindingsTracker event types to unified event types
   */
  private mapFindingsEventType(type: string): UnifiedReportEventType {
    const map: Record<string, UnifiedReportEventType> = {
      finding_detected: "item_added",
      finding_updated: "item_updated",
      finding_resolved: "item_resolved",
      finding_removed: "item_removed",
      findings_cleared: "items_cleared",
    };
    return map[type] || "item_updated";
  }

  /**
   * Map IssueTracker event types to unified event types
   */
  private mapIssueEventType(type: string): UnifiedReportEventType {
    const map: Record<string, UnifiedReportEventType> = {
      issue_detected: "item_added",
      issue_updated: "item_updated",
      issue_resolved: "item_resolved",
      issue_removed: "item_removed",
      issues_cleared: "items_cleared",
    };
    return map[type] || "item_updated";
  }

  // ==========================================================================
  // Public API - Queries
  // ==========================================================================

  /**
   * Get the current session
   */
  getCurrentSession(): SessionContext | null {
    return sessionManager.getCurrentSession();
  }

  /**
   * Get all items (findings + issues) for the current session
   */
  getSessionItems(): UnifiedReportItem[] {
    const findings = findingsTracker.getSessionFindings().map(findingToUnified);
    const issues = issueTracker.getSessionIssues().map(issueToUnified);
    return [...findings, ...issues].sort((a, b) => b.detectedAt - a.detectedAt);
  }

  /**
   * Get all findings for the current session
   */
  getSessionFindings(): UnifiedReportItem[] {
    return findingsTracker.getSessionFindings().map(findingToUnified);
  }

  /**
   * Get all issues for the current session
   */
  getSessionIssues(): UnifiedReportItem[] {
    return issueTracker.getSessionIssues().map(issueToUnified);
  }

  /**
   * Get items filtered by status
   */
  getItemsByStatus(status: UnifiedStatus): UnifiedReportItem[] {
    return this.getSessionItems().filter((item) => item.status === status);
  }

  /**
   * Get items filtered by severity
   */
  getItemsBySeverity(severity: UnifiedSeverity): UnifiedReportItem[] {
    return this.getSessionItems().filter((item) => item.severity === severity);
  }

  /**
   * Get actionable items (not resolved/skipped)
   */
  getActionableItems(): UnifiedReportItem[] {
    return this.getSessionItems().filter((item) => item.actionable);
  }

  /**
   * Get items needing user input
   */
  getItemsNeedingInput(): UnifiedReportItem[] {
    return this.getSessionItems().filter((item) => item.needsInput);
  }

  /**
   * Get a unified summary for the current session
   */
  getSessionSummary(): UnifiedReportSummary {
    const session = sessionManager.getCurrentSession();
    const items = this.getSessionItems();

    const summary: UnifiedReportSummary = {
      sessionId: session?.id || "",
      total: items.length,
      findings: items.filter((i) => i.type === "finding").length,
      issues: items.filter((i) => i.type === "issue").length,
      byStatus: {
        detected: 0,
        in_progress: 0,
        needs_input: 0,
        resolved: 0,
        skipped: 0,
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

    for (const item of items) {
      summary.byStatus[item.status]++;
      summary.bySeverity[item.severity]++;
      if (item.actionable) summary.actionable++;
      if (item.needsInput) summary.needsInput++;
      if (item.status === "resolved") summary.resolved++;
    }

    return summary;
  }

  /**
   * Get total item count for the current session
   */
  get count(): number {
    return findingsTracker.count + issueTracker.count;
  }

  /**
   * Get unresolved item count
   */
  get unresolvedCount(): number {
    return this.getActionableItems().length;
  }

  // ==========================================================================
  // Public API - Actions
  // ==========================================================================

  /**
   * Resolve an item by its unified ID
   */
  resolveItem(unifiedId: string, resolution: string): boolean {
    if (unifiedId.startsWith("finding-")) {
      const findingId = unifiedId.replace("finding-", "");
      const result = findingsTracker.resolveFinding(findingId, resolution);
      return result !== null;
    } else if (unifiedId.startsWith("issue-")) {
      const issueId = unifiedId.replace("issue-", "");
      const result = issueTracker.resolveIssue(issueId, resolution);
      return result !== null;
    }
    return false;
  }

  /**
   * Update an item's status
   */
  updateItemStatus(unifiedId: string, status: UnifiedStatus): boolean {
    if (unifiedId.startsWith("finding-")) {
      const findingId = unifiedId.replace("finding-", "");
      const result = findingsTracker.updateFindingStatus(findingId, status as FindingStatus);
      return result !== null;
    } else if (unifiedId.startsWith("issue-")) {
      const issueId = unifiedId.replace("issue-", "");
      // IssueTracker has a more limited status type
      const issueStatus = status as "detected" | "in_progress" | "resolved" | "skipped";
      const result = issueTracker.updateIssueStatus(issueId, issueStatus);
      return result !== null;
    }
    return false;
  }

  /**
   * Provide user response for a finding needing input
   */
  provideUserResponse(unifiedId: string, response: string): boolean {
    if (unifiedId.startsWith("finding-")) {
      const findingId = unifiedId.replace("finding-", "");
      const result = findingsTracker.provideUserResponse(findingId, response);
      return result !== null;
    }
    // Issues don't support user input
    return false;
  }

  /**
   * Clear all items for the current session
   */
  clearSession(): void {
    findingsTracker.clearSession();
    issueTracker.clearSession();
  }

  // ==========================================================================
  // Event Subscription
  // ==========================================================================

  /**
   * Subscribe to unified report events
   */
  subscribe(listener: UnifiedReportListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /**
   * Emit an event to all listeners
   */
  private emit(event: UnifiedReportEvent): void {
    this.listeners.forEach((listener) => {
      try {
        listener(event);
      } catch (error) {
        console.error("[UnifiedReportService] Listener error:", error);
      }
    });
  }

  // ==========================================================================
  // Cleanup
  // ==========================================================================

  /**
   * Cleanup subscriptions (for testing)
   */
  dispose(): void {
    this.unsubscribers.forEach((unsub) => unsub());
    this.unsubscribers = [];
    this.listeners.clear();
  }
}

// ============================================================================
// Singleton Export
// ============================================================================

export const unifiedReportService = UnifiedReportService.getInstance();
