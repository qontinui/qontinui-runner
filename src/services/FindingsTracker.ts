/**
 * FindingsTracker Service
 *
 * Comprehensive service for tracking categorized findings from AI analysis.
 * Parses structured output markers like [FINDING:category:severity] from AI output.
 * Manages execution reports and user input requests.
 *
 * This file has been refactored to use modules from src/findings/.
 * It maintains backward compatibility while delegating to focused modules.
 */

import type {
  Finding,
  FindingSeverity,
  FindingStatus,
  ActionType,
  UserInputRequest,
  ExecutionReport,
  PhaseInfo,
  ParsedFinding,
  CodeContext,
} from "../types/findings";
import { getCategoryById, BUILT_IN_CATEGORIES } from "./FindingCategories";

// Import from refactored modules
import {
  persistFindingsData,
  loadFindingsData,
  archiveSession,
} from "../findings/FindingsPersistence";
import {
  getAllFindings as queryGetAllFindings,
  getSessionFindings as queryGetSessionFindings,
  getFindingsByCategory as queryGetFindingsByCategory,
  getFindingsNeedingInput as queryGetFindingsNeedingInput,
  getActionableFindings as queryGetActionableFindings,
  getUnresolvedCount as queryGetUnresolvedCount,
} from "../findings/FindingsQuery";
import { calculateSummary, createEmptySummary } from "../findings/FindingsExport";
import {
  createReport,
  updateReportWithFindings,
  completeReport as completeReportSync,
  startNewPhase as startNewPhaseSync,
} from "../findings/FindingsSync";
import type {
  SessionHistoryEntry,
  FindingsTrackerEventType,
  FindingsTrackerEvent,
  FindingsTrackerListener,
  FindingParsingMeta,
} from "../findings/types";

// Re-export types for backward compatibility
export type { FindingsTrackerEventType, FindingsTrackerEvent, SessionHistoryEntry };

/**
 * Regex patterns for parsing structured finding markers
 *
 * Format: [FINDING:category:severity] or [FINDING:category:severity:needs_input]
 * Followed by structured content until [/FINDING]
 */
const FINDING_START_PATTERN = /\[FINDING:(\w+):(\w+)(?::needs_input)?\]/;
const FINDING_END_PATTERN = /\[\/FINDING\]/;

/**
 * Content field patterns within a finding block
 */
const TITLE_PATTERN = /^Title:\s*(.+)$/m;
const DESCRIPTION_PATTERN =
  /^Description:\s*([\s\S]*?)(?=^(?:Title|File|Line|Question|Options|Resolution|Context):|\[\/FINDING\]|$)/m;
const FILE_PATTERN = /^File:\s*(.+)$/m;
const LINE_PATTERN = /^Line:\s*(\d+)$/m;
const QUESTION_PATTERN = /^Question:\s*(.+)$/m;
const OPTIONS_PATTERN = /^Options:\s*(.+)$/m;
const RESOLUTION_PATTERN = /^Resolution:\s*(.+)$/m;

/**
 * Legacy issue marker patterns (for backward compatibility)
 */
const LEGACY_ISSUE_DETECTED_PATTERN = /\[ISSUE:DETECTED\]/;

/**
 * Parse severity string to typed severity
 */
function parseSeverity(severity: string): FindingSeverity {
  const validSeverities: FindingSeverity[] = ["critical", "high", "medium", "low", "info"];
  const lower = severity.toLowerCase() as FindingSeverity;
  return validSeverities.includes(lower) ? lower : "medium";
}

/**
 * Map category ID to appropriate action type
 */
function getActionTypeForCategory(categoryId: string): ActionType {
  const category = getCategoryById(categoryId);
  return category?.defaultActionType ?? "auto_fix";
}

/**
 * Parse a finding block content into structured data
 */
function parseBlockContent(
  content: string,
  categoryId: string,
  severity: FindingSeverity,
  needsInput: boolean,
): ParsedFinding {
  const titleMatch = content.match(TITLE_PATTERN);
  const descriptionMatch = content.match(DESCRIPTION_PATTERN);
  const fileMatch = content.match(FILE_PATTERN);
  const lineMatch = content.match(LINE_PATTERN);
  const questionMatch = content.match(QUESTION_PATTERN);
  const optionsMatch = content.match(OPTIONS_PATTERN);
  const resolutionMatch = content.match(RESOLUTION_PATTERN);

  return {
    categoryId,
    severity,
    title: titleMatch?.[1]?.trim() ?? "Untitled Finding",
    description: descriptionMatch?.[1]?.trim() ?? content.trim(),
    needsInput,
    question: questionMatch?.[1]?.trim(),
    options: optionsMatch?.[1]?.split("|").map((o) => o.trim()),
    file: fileMatch?.[1]?.trim(),
    line: lineMatch ? parseInt(lineMatch[1], 10) : undefined,
    resolution: resolutionMatch?.[1]?.trim(),
  };
}

export class FindingsTracker {
  private static instance: FindingsTracker | null = null;

  private findings: Map<string, Finding> = new Map();
  private reports: Map<string, ExecutionReport> = new Map();
  private listeners: Set<FindingsTrackerListener> = new Set();
  private currentSessionId: string = "";
  private currentReportId: string | null = null;

  // Buffer for multi-line finding parsing
  private parsingBuffer: string = "";
  private currentFindingMeta: FindingParsingMeta | null = null;

  // Session history storage
  private sessionHistory: SessionHistoryEntry[] = [];

  private constructor() {
    // Private constructor for singleton
  }

  /** Get the singleton instance */
  static getInstance(): FindingsTracker {
    if (!FindingsTracker.instance) {
      FindingsTracker.instance = new FindingsTracker();
    }
    return FindingsTracker.instance;
  }

  /** Reset the singleton (useful for testing) */
  static resetInstance(): void {
    FindingsTracker.instance = null;
  }

  /** Set the current session ID */
  setSessionId(sessionId: string): void {
    this.currentSessionId = sessionId;
  }

  /** Get the current session ID */
  getSessionId(): string {
    return this.currentSessionId;
  }

  /** Subscribe to finding events */
  subscribe(listener: FindingsTrackerListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Emit an event to all listeners */
  private emit(event: FindingsTrackerEvent): void {
    this.listeners.forEach((listener) => {
      try {
        listener(event);
      } catch (error) {
        console.error("[FindingsTracker] Listener error:", error);
      }
    });
  }

  /**
   * Process an AI output line for finding markers
   * Handles both new structured format and legacy issue markers
   *
   * NOTE: The input may contain multiple lines separated by \n (when streaming
   * from Claude, entire paragraphs may arrive as a single "line" with embedded newlines).
   * This method splits by newlines and processes each sub-line individually.
   */
  processLine(text: string): Finding | null {
    // Split by newlines to handle multi-line input
    // (Claude often sends entire paragraphs with embedded \n characters)
    const lines = text.split("\n");
    let lastFinding: Finding | null = null;

    for (const line of lines) {
      const finding = this.processSingleLine(line);
      if (finding) {
        lastFinding = finding;
      }
    }

    return lastFinding;
  }

  /**
   * Process a single line of AI output for finding markers
   */
  private processSingleLine(line: string): Finding | null {
    // Check if we're currently parsing a multi-line finding block
    if (this.currentFindingMeta) {
      // Check for end marker
      if (FINDING_END_PATTERN.test(line)) {
        // Parse the accumulated buffer
        const parsed = parseBlockContent(
          this.parsingBuffer,
          this.currentFindingMeta.categoryId,
          this.currentFindingMeta.severity,
          this.currentFindingMeta.needsInput,
        );
        const finding = this.addFinding(parsed);

        // Reset parsing state
        this.parsingBuffer = "";
        this.currentFindingMeta = null;

        return finding;
      } else {
        // Accumulate content
        this.parsingBuffer += line + "\n";
        return null;
      }
    }

    // Check for start of new finding block
    const startMatch = line.match(FINDING_START_PATTERN);
    if (startMatch) {
      const [, categoryId, severity] = startMatch;
      const needsInput = line.includes(":needs_input]");

      // Start buffering
      this.currentFindingMeta = {
        categoryId,
        severity: parseSeverity(severity),
        needsInput,
      };

      // Get content after the marker on this line
      const markerEnd = line.indexOf("]") + 1;
      const restOfLine = line.substring(markerEnd).trim();

      // Check if this is a single-line finding (has end marker on same line)
      if (FINDING_END_PATTERN.test(restOfLine)) {
        const content = restOfLine.replace(FINDING_END_PATTERN, "").trim();
        const parsed = parseBlockContent(content, categoryId, parseSeverity(severity), needsInput);
        const finding = this.addFinding(parsed);
        this.currentFindingMeta = null;
        return finding;
      }

      this.parsingBuffer = restOfLine + "\n";
      return null;
    }

    // Check for legacy [ISSUE:DETECTED] markers (backward compatibility)
    if (LEGACY_ISSUE_DETECTED_PATTERN.test(line)) {
      const contextAfterMarker = line.split("[ISSUE:DETECTED]")[1]?.trim() || "";
      const cleanContext = contextAfterMarker.replace(/^\s*\{[\s\S]*$/, "").trim();

      const parsed: ParsedFinding = {
        categoryId: "code_bug",
        severity: "medium",
        title: cleanContext || "Issue detected",
        description: line,
        needsInput: false,
      };

      return this.addFinding(parsed);
    }

    return null;
  }

  /**
   * Add a new finding from parsed data
   */
  addFinding(parsed: ParsedFinding): Finding {
    const actionType = parsed.needsInput
      ? "needs_user_input"
      : getActionTypeForCategory(parsed.categoryId);

    const codeContext: CodeContext | undefined =
      parsed.file || parsed.line
        ? {
            file: parsed.file,
            line: parsed.line,
          }
        : undefined;

    const finding: Finding = {
      id: crypto.randomUUID(),
      categoryId: parsed.categoryId,
      severity: parsed.severity,
      status: parsed.needsInput ? "needs_input" : "detected",
      title: parsed.title,
      description: parsed.description,
      sourceSessionId: this.currentSessionId,
      detectedAt: Date.now(),
      actionType,
      actionable: actionType !== "informational",
      codeContext,
      resolution: parsed.resolution,
    };

    // Create user input request if needed
    if (parsed.needsInput && parsed.question) {
      finding.pendingQuestion = {
        id: crypto.randomUUID(),
        findingId: finding.id,
        question: parsed.question,
        inputType: parsed.options ? "choice" : "text",
        options: parsed.options?.map((opt) => ({
          value: opt,
          label: opt,
        })),
        required: true,
      };
    }

    this.findings.set(finding.id, finding);
    this.emit({ type: "finding_detected", finding });

    // Update current report if one exists
    if (this.currentReportId) {
      this.updateReport(this.currentReportId);
    }

    console.log(
      `[FindingsTracker] Finding detected: [${parsed.categoryId}] ${finding.title} (${finding.id})`,
    );

    return finding;
  }

  /**
   * Create a finding directly (for programmatic use)
   */
  createFinding(params: {
    categoryId: string;
    severity: FindingSeverity;
    title: string;
    description: string;
    codeContext?: CodeContext;
    promptName?: string;
  }): Finding {
    const actionType = getActionTypeForCategory(params.categoryId);

    const finding: Finding = {
      id: crypto.randomUUID(),
      categoryId: params.categoryId,
      severity: params.severity,
      status: "detected",
      title: params.title,
      description: params.description,
      sourceSessionId: this.currentSessionId,
      sourcePromptName: params.promptName,
      detectedAt: Date.now(),
      actionType,
      actionable: actionType !== "informational",
      codeContext: params.codeContext,
    };

    this.findings.set(finding.id, finding);
    this.emit({ type: "finding_detected", finding });

    if (this.currentReportId) {
      this.updateReport(this.currentReportId);
    }

    return finding;
  }

  /**
   * Update a finding's status
   */
  updateFindingStatus(id: string, status: FindingStatus): Finding | null {
    const finding = this.findings.get(id);
    if (!finding) {
      console.warn(`[FindingsTracker] Finding not found: ${id}`);
      return null;
    }

    finding.status = status;
    if (status === "resolved") {
      finding.resolvedAt = Date.now();
    }

    this.emit({ type: "finding_updated", finding });

    if (this.currentReportId) {
      this.updateReport(this.currentReportId);
    }

    console.log(`[FindingsTracker] Finding ${id} status updated to: ${status}`);
    return finding;
  }

  /**
   * Resolve a finding with a resolution description
   */
  resolveFinding(id: string, resolution: string): Finding | null {
    const finding = this.findings.get(id);
    if (!finding) {
      console.warn(`[FindingsTracker] Finding not found: ${id}`);
      return null;
    }

    finding.status = "resolved";
    finding.resolution = resolution;
    finding.resolvedAt = Date.now();

    this.emit({ type: "finding_resolved", finding });

    if (this.currentReportId) {
      this.updateReport(this.currentReportId);
    }

    console.log(`[FindingsTracker] Finding resolved: ${finding.title} (${id})`);
    return finding;
  }

  /**
   * Provide user response for a finding
   */
  provideUserResponse(findingId: string, response: string): Finding | null {
    const finding = this.findings.get(findingId);
    if (!finding) {
      console.warn(`[FindingsTracker] Finding not found: ${findingId}`);
      return null;
    }

    finding.userResponse = response;
    finding.status = "detected"; // Ready for processing now
    finding.pendingQuestion = undefined;

    this.emit({
      type: "input_received",
      finding,
    });

    console.log(`[FindingsTracker] User response received for: ${finding.title}`);
    return finding;
  }

  /**
   * Remove a finding
   */
  removeFinding(id: string): boolean {
    const finding = this.findings.get(id);
    if (!finding) {
      console.warn(`[FindingsTracker] Finding not found for removal: ${id}`);
      return false;
    }

    this.findings.delete(id);
    this.emit({ type: "finding_removed", finding });

    if (this.currentReportId) {
      this.updateReport(this.currentReportId);
    }

    console.log(`[FindingsTracker] Finding removed: ${finding.title} (${id})`);
    return true;
  }

  /**
   * Get a finding by ID
   */
  getFinding(id: string): Finding | undefined {
    return this.findings.get(id);
  }

  /**
   * Get all findings
   */
  getAllFindings(): Finding[] {
    return queryGetAllFindings(this.findings);
  }

  /**
   * Get findings for the current session
   */
  getSessionFindings(): Finding[] {
    return queryGetSessionFindings(this.findings, this.currentSessionId);
  }

  /**
   * Get findings by category
   */
  getFindingsByCategory(categoryId: string): Finding[] {
    return queryGetFindingsByCategory(this.findings, categoryId);
  }

  /**
   * Get findings needing user input
   */
  getFindingsNeedingInput(): Finding[] {
    return queryGetFindingsNeedingInput(this.findings);
  }

  /**
   * Get actionable findings
   */
  getActionableFindings(): Finding[] {
    return queryGetActionableFindings(this.findings);
  }

  // ==================== Execution Reports ====================

  /**
   * Start a new execution report
   */
  startReport(promptName: string, promptId?: string): ExecutionReport {
    const report = createReport(this.currentSessionId, promptName, promptId);

    this.reports.set(report.id, report);
    this.currentReportId = report.id;

    this.emit({ type: "report_created", report });
    console.log(`[FindingsTracker] Report started: ${promptName} (${report.id})`);

    return report;
  }

  /**
   * Update the current report with latest findings
   */
  private updateReport(reportId: string): void {
    const report = this.reports.get(reportId);
    if (!report) return;

    // Get findings for this session
    const sessionFindings = this.getSessionFindings();
    const updatedReport = updateReportWithFindings(report, sessionFindings);

    this.reports.set(reportId, updatedReport);
    this.emit({ type: "report_updated", report: updatedReport });
  }

  /**
   * Complete the current report
   */
  completeReport(
    status: "completed" | "failed" | "cancelled" = "completed",
  ): ExecutionReport | null {
    if (!this.currentReportId) {
      console.warn("[FindingsTracker] No active report to complete");
      return null;
    }

    const report = this.reports.get(this.currentReportId);
    if (!report) return null;

    const completedReport = completeReportSync(report, status);
    this.reports.set(this.currentReportId, completedReport);

    // Update with final findings
    this.updateReport(this.currentReportId);

    const finalReport = this.reports.get(this.currentReportId)!;
    this.currentReportId = null;

    console.log(
      `[FindingsTracker] Report completed: ${finalReport.promptName} (${finalReport.id})`,
    );
    return finalReport;
  }

  /**
   * Start a new phase in the current report
   */
  startNewPhase(): PhaseInfo | null {
    if (!this.currentReportId) return null;

    const report = this.reports.get(this.currentReportId);
    if (!report) return null;

    const { report: updatedReport, phase: newPhase } = startNewPhaseSync(report);
    this.reports.set(this.currentReportId, updatedReport);

    this.emit({ type: "report_updated", report: updatedReport });
    return newPhase;
  }

  /**
   * Get the current report
   */
  getCurrentReport(): ExecutionReport | null {
    return this.currentReportId ? (this.reports.get(this.currentReportId) ?? null) : null;
  }

  /**
   * Get a report by ID
   */
  getReport(id: string): ExecutionReport | undefined {
    return this.reports.get(id);
  }

  /**
   * Get all reports
   */
  getAllReports(): ExecutionReport[] {
    return Array.from(this.reports.values());
  }

  /**
   * Get reports for the current session
   */
  getSessionReports(): ExecutionReport[] {
    return this.getAllReports().filter((r) => r.sessionId === this.currentSessionId);
  }

  // ==================== Session Management ====================

  /**
   * Clear all findings
   */
  clearAll(): void {
    const findings = this.getAllFindings();
    this.findings.clear();
    this.emit({ type: "findings_cleared", findings });
    console.log("[FindingsTracker] All findings cleared");
  }

  /**
   * Clear findings for the current session
   */
  clearSession(): void {
    const sessionFindings = this.getSessionFindings();
    for (const finding of sessionFindings) {
      this.findings.delete(finding.id);
    }
    this.emit({ type: "findings_cleared", findings: sessionFindings });
    console.log(`[FindingsTracker] Session findings cleared: ${sessionFindings.length}`);
  }

  /**
   * Start a new session
   */
  startNewSession(sessionId?: string): void {
    this.clearSession();
    this.currentSessionId = sessionId || crypto.randomUUID();
    this.currentReportId = null;
    this.parsingBuffer = "";
    this.currentFindingMeta = null;
    console.log(`[FindingsTracker] New session started: ${this.currentSessionId}`);
  }

  /**
   * Get finding count
   */
  get count(): number {
    return this.findings.size;
  }

  /**
   * Get unresolved count
   */
  get unresolvedCount(): number {
    return queryGetUnresolvedCount(this.findings);
  }

  // ==================== Persistence ====================

  /**
   * Get session history from the store
   * Returns completed sessions with their findings summaries
   */
  getSessionHistory(): SessionHistoryEntry[] {
    return this.sessionHistory;
  }

  /**
   * Archive the current session to history
   * Called when a session ends (completes, fails, or times out)
   */
  archiveCurrentSession(
    status: "completed" | "failed" | "timeout" | "context_limit" = "completed",
  ): void {
    const currentReport = this.getCurrentReport();
    if (!currentReport) {
      console.log("[FindingsTracker] No current report to archive");
      return;
    }

    const sessionFindings = this.getSessionFindings();
    this.sessionHistory = archiveSession(
      currentReport,
      this.currentSessionId,
      sessionFindings,
      this.sessionHistory,
      status,
    );

    // Persist to disk
    this.persistToDisk();

    console.log(
      `[FindingsTracker] Session archived: ${currentReport.promptName} (${currentReport.id}) - ${status}`,
    );
  }

  /**
   * Persist findings and session history to disk
   */
  private async persistToDisk(): Promise<void> {
    try {
      await persistFindingsData({
        currentSessionId: this.currentSessionId,
        findings: this.getAllFindings(),
        reports: this.getAllReports(),
        sessionHistory: this.sessionHistory,
      });
      console.log("[FindingsTracker] Persisted to disk");
    } catch (error) {
      console.error("[FindingsTracker] Failed to persist to disk:", error);
    }
  }

  /**
   * Load findings and session history from disk
   */
  async loadFromDisk(): Promise<void> {
    try {
      const data = await loadFindingsData();

      if (!data) {
        console.log("[FindingsTracker] No persisted data found");
        return;
      }

      // Restore session history
      this.sessionHistory = data.sessionHistory || [];

      // Optionally restore findings from last session
      // (commented out - usually we want a fresh start)
      // for (const finding of data.findings) {
      //   this.findings.set(finding.id, finding);
      // }

      console.log(
        `[FindingsTracker] Loaded from disk: ${this.sessionHistory.length} sessions in history`,
      );
    } catch (error) {
      console.error("[FindingsTracker] Failed to load from disk:", error);
    }
  }
}

// Export singleton instance
export const findingsTracker = FindingsTracker.getInstance();
