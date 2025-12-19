/**
 * IssueTracker Service
 *
 * Singleton service that tracks issues detected during AI-assisted automation sessions.
 * Parses AI output for structured markers like [ISSUE:DETECTED], [ISSUE:RESOLVED], etc.
 */

import {
  DetectedIssue,
  IssueDetectedPayload,
  IssueResolvedPayload,
  IssueProgressPayload,
  IssueStatus,
  IssueSessionSummary,
} from "../types/issues";
import {
  verificationService,
  VerificationPendingMarker,
  VerificationCompletedMarker,
  VerificationFailedMarker,
  RunnerRestartMarker,
} from "./VerificationService";

/** Event types emitted by IssueTracker */
export type IssueTrackerEventType =
  | "issue_detected"
  | "issue_updated"
  | "issue_resolved"
  | "issues_cleared";

export interface IssueTrackerEvent {
  type: IssueTrackerEventType;
  issue?: DetectedIssue;
  issues?: DetectedIssue[];
}

type IssueTrackerListener = (event: IssueTrackerEvent) => void;

/** Regex patterns for parsing issue markers */
// Pattern with JSON payload
const ISSUE_DETECTED_WITH_JSON_PATTERN = /\[ISSUE:DETECTED\]\s*(\{[\s\S]*?\})/;
// Simple pattern without JSON (just the marker)
const ISSUE_DETECTED_SIMPLE_PATTERN = /\[ISSUE:DETECTED\](?!\s*\{)/;
const ISSUE_RESOLVED_PATTERN = /\[ISSUE:RESOLVED\]\s*(\{[\s\S]*?\})/;
const ISSUE_PROGRESS_PATTERN = /\[ISSUE:PROGRESS\]\s*(\{[\s\S]*?\})/;

/** Regex patterns for parsing verification markers */
const VERIFICATION_PENDING_PATTERN = /\[VERIFICATION:PENDING\]\s*(\{[\s\S]*?\})/;
const VERIFICATION_COMPLETED_PATTERN = /\[VERIFICATION:COMPLETED\]\s*(\{[\s\S]*?\})/;
const VERIFICATION_FAILED_PATTERN = /\[VERIFICATION:FAILED\]\s*(\{[\s\S]*?\})/;
const RUNNER_RESTART_PATTERN = /\[RUNNER:RESTART\]\s*(\{[\s\S]*?\})/;

export class IssueTracker {
  private static instance: IssueTracker | null = null;

  private issues: Map<string, DetectedIssue> = new Map();
  private listeners: Set<IssueTrackerListener> = new Set();
  private currentSessionId: string = "";

  private constructor() {
    // Private constructor for singleton
  }

  /** Get the singleton instance */
  static getInstance(): IssueTracker {
    if (!IssueTracker.instance) {
      IssueTracker.instance = new IssueTracker();
    }
    return IssueTracker.instance;
  }

  /** Reset the singleton (useful for testing) */
  static resetInstance(): void {
    IssueTracker.instance = null;
  }

  /** Set the current session ID */
  setSessionId(sessionId: string): void {
    this.currentSessionId = sessionId;
  }

  /** Get the current session ID */
  getSessionId(): string {
    return this.currentSessionId;
  }

  /** Subscribe to issue events */
  subscribe(listener: IssueTrackerListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Emit an event to all listeners */
  private emit(event: IssueTrackerEvent): void {
    this.listeners.forEach((listener) => {
      try {
        listener(event);
      } catch (error) {
        console.error("[IssueTracker] Listener error:", error);
      }
    });
  }

  /**
   * Process an AI output line for issue markers
   * @param line The AI output line to parse
   * @returns The detected issue if one was found, or null
   */
  processLine(line: string): DetectedIssue | null {
    // Check for [ISSUE:DETECTED] marker
    if (line.includes("[ISSUE:DETECTED]")) {
      // Try to extract and parse JSON payload
      const detectedWithJsonMatch = line.match(ISSUE_DETECTED_WITH_JSON_PATTERN);
      if (detectedWithJsonMatch) {
        try {
          const payload = JSON.parse(detectedWithJsonMatch[1]) as IssueDetectedPayload;
          return this.addIssue(payload);
        } catch {
          // JSON parse failed - fall through to create simple issue
          console.warn("[IssueTracker] JSON parse failed, creating simple issue");
        }
      }

      // No valid JSON found, or JSON parse failed - create simple issue
      const contextAfterMarker = line.split("[ISSUE:DETECTED]")[1]?.trim() || "";
      // Remove any malformed JSON from the title
      const cleanContext = contextAfterMarker.replace(/^\s*\{[\s\S]*$/, "").trim();
      const payload: IssueDetectedPayload = {
        type: "error",
        severity: "medium",
        title: cleanContext || "Issue detected",
        description: line,
        source: {
          type: "ai_analysis",
          description: "Detected from AI output marker",
        },
      };
      return this.addIssue(payload);
    }

    // Check for [ISSUE:RESOLVED]
    const resolvedMatch = line.match(ISSUE_RESOLVED_PATTERN);
    if (resolvedMatch) {
      try {
        const payload = JSON.parse(resolvedMatch[1]) as IssueResolvedPayload;
        return this.resolveIssue(payload.id, payload.resolution);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse ISSUE:RESOLVED:", error);
      }
    }

    // Check for [ISSUE:PROGRESS]
    const progressMatch = line.match(ISSUE_PROGRESS_PATTERN);
    if (progressMatch) {
      try {
        const payload = JSON.parse(progressMatch[1]) as IssueProgressPayload;
        return this.updateIssueStatus(payload.id, payload.status);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse ISSUE:PROGRESS:", error);
      }
    }

    // Check for [VERIFICATION:PENDING] - AI wants to save verification state
    const verificationPendingMatch = line.match(VERIFICATION_PENDING_PATTERN);
    if (verificationPendingMatch) {
      try {
        const payload = JSON.parse(verificationPendingMatch[1]) as VerificationPendingMarker;
        console.log("[IssueTracker] Verification pending detected:", payload.reason);
        verificationService.savePendingVerification(payload);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse VERIFICATION:PENDING:", error);
      }
    }

    // Check for [VERIFICATION:COMPLETED] - AI confirms fix worked
    const verificationCompletedMatch = line.match(VERIFICATION_COMPLETED_PATTERN);
    if (verificationCompletedMatch) {
      try {
        const payload = JSON.parse(verificationCompletedMatch[1]) as VerificationCompletedMarker;
        console.log("[IssueTracker] Verification completed:", payload.message);
        verificationService.handleVerificationCompleted(payload);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse VERIFICATION:COMPLETED:", error);
      }
    }

    // Check for [VERIFICATION:FAILED] - AI reports fix didn't work
    const verificationFailedMatch = line.match(VERIFICATION_FAILED_PATTERN);
    if (verificationFailedMatch) {
      try {
        const payload = JSON.parse(verificationFailedMatch[1]) as VerificationFailedMarker;
        console.log("[IssueTracker] Verification failed:", payload.message);
        verificationService.handleVerificationFailed(payload);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse VERIFICATION:FAILED:", error);
      }
    }

    // Check for [RUNNER:RESTART] - AI wants to restart the runner
    const restartMatch = line.match(RUNNER_RESTART_PATTERN);
    if (restartMatch) {
      try {
        const payload = JSON.parse(restartMatch[1]) as RunnerRestartMarker;
        console.log("[IssueTracker] Runner restart requested:", payload.reason);
        verificationService.triggerRestart(payload);
      } catch (error) {
        console.error("[IssueTracker] Failed to parse RUNNER:RESTART:", error);
      }
    }

    return null;
  }

  /**
   * Add a new detected issue
   */
  addIssue(payload: IssueDetectedPayload): DetectedIssue {
    const issue: DetectedIssue = {
      id: crypto.randomUUID(),
      session_id: this.currentSessionId,
      type: payload.type,
      severity: payload.severity,
      title: payload.title,
      description: payload.description,
      file: payload.file,
      line: payload.line,
      source: payload.source,
      status: "detected",
      detected_at: Date.now(),
    };

    this.issues.set(issue.id, issue);
    this.emit({ type: "issue_detected", issue });

    console.log(`[IssueTracker] Issue detected: ${issue.title} (${issue.id})`);
    return issue;
  }

  /**
   * Update an issue's status
   */
  updateIssueStatus(id: string, status: IssueStatus): DetectedIssue | null {
    const issue = this.issues.get(id);
    if (!issue) {
      console.warn(`[IssueTracker] Issue not found: ${id}`);
      return null;
    }

    issue.status = status;
    if (status === "resolved") {
      issue.resolved_at = Date.now();
    }

    this.emit({ type: "issue_updated", issue });
    console.log(`[IssueTracker] Issue ${id} status updated to: ${status}`);
    return issue;
  }

  /**
   * Resolve an issue with a resolution description
   */
  resolveIssue(id: string, resolution: string): DetectedIssue | null {
    const issue = this.issues.get(id);
    if (!issue) {
      console.warn(`[IssueTracker] Issue not found: ${id}`);
      return null;
    }

    issue.status = "resolved";
    issue.resolution = resolution;
    issue.resolved_at = Date.now();

    this.emit({ type: "issue_resolved", issue });
    console.log(`[IssueTracker] Issue resolved: ${issue.title} (${id})`);
    return issue;
  }

  /**
   * Skip an issue (mark as won't fix)
   */
  skipIssue(id: string): DetectedIssue | null {
    return this.updateIssueStatus(id, "skipped");
  }

  /**
   * Get an issue by ID
   */
  getIssue(id: string): DetectedIssue | undefined {
    return this.issues.get(id);
  }

  /**
   * Get all issues
   */
  getAllIssues(): DetectedIssue[] {
    return Array.from(this.issues.values());
  }

  /**
   * Get issues for the current session
   */
  getSessionIssues(): DetectedIssue[] {
    return this.getAllIssues().filter((issue) => issue.session_id === this.currentSessionId);
  }

  /**
   * Get unresolved issues
   */
  getUnresolvedIssues(): DetectedIssue[] {
    return this.getAllIssues().filter(
      (issue) => issue.status === "detected" || issue.status === "in_progress",
    );
  }

  /**
   * Get session summary statistics
   */
  getSessionSummary(): IssueSessionSummary {
    const sessionIssues = this.getSessionIssues();

    const summary: IssueSessionSummary = {
      session_id: this.currentSessionId,
      total: sessionIssues.length,
      by_status: {
        detected: 0,
        in_progress: 0,
        resolved: 0,
        skipped: 0,
      },
      by_severity: {
        critical: 0,
        high: 0,
        medium: 0,
        low: 0,
      },
    };

    for (const issue of sessionIssues) {
      summary.by_status[issue.status]++;
      summary.by_severity[issue.severity]++;
    }

    return summary;
  }

  /**
   * Clear all issues
   */
  clearAll(): void {
    const issues = this.getAllIssues();
    this.issues.clear();
    this.emit({ type: "issues_cleared", issues });
    console.log("[IssueTracker] All issues cleared");
  }

  /**
   * Clear issues for the current session
   */
  clearSession(): void {
    const sessionIssues = this.getSessionIssues();
    for (const issue of sessionIssues) {
      this.issues.delete(issue.id);
    }
    this.emit({ type: "issues_cleared", issues: sessionIssues });
    console.log(`[IssueTracker] Session issues cleared: ${sessionIssues.length}`);
  }

  /**
   * Start a new session (clears previous session's issues)
   */
  startNewSession(sessionId?: string): void {
    this.clearSession();
    this.currentSessionId = sessionId || crypto.randomUUID();
    console.log(`[IssueTracker] New session started: ${this.currentSessionId}`);
  }

  /**
   * Get issue count
   */
  get count(): number {
    return this.issues.size;
  }

  /**
   * Get unresolved count
   */
  get unresolvedCount(): number {
    return this.getUnresolvedIssues().length;
  }
}

// Export singleton instance
export const issueTracker = IssueTracker.getInstance();
