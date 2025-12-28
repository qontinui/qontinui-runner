/**
 * FindingsTracker Service
 *
 * Comprehensive service for tracking categorized findings from AI analysis.
 * Parses structured output markers like [FINDING:category:severity] from AI output.
 * Manages execution reports and user input requests.
 */

import type {
  Finding,
  FindingSeverity,
  FindingStatus,
  ActionType,
  UserInputRequest,
  ExecutionReport,
  ReportSummary,
  PhaseInfo,
  ParsedFinding,
  CodeContext,
} from "../types/findings";
import { getCategoryById, BUILT_IN_CATEGORIES } from "./FindingCategories";

/** Event types emitted by FindingsTracker */
export type FindingsTrackerEventType =
  | "finding_detected"
  | "finding_updated"
  | "finding_resolved"
  | "finding_removed"
  | "findings_cleared"
  | "report_created"
  | "report_updated"
  | "input_requested"
  | "input_received";

export interface FindingsTrackerEvent {
  type: FindingsTrackerEventType;
  finding?: Finding;
  findings?: Finding[];
  report?: ExecutionReport;
  inputRequest?: UserInputRequest;
}

type FindingsTrackerListener = (event: FindingsTrackerEvent) => void;

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
const _CONTEXT_PATTERN =
  /^Context:\s*([\s\S]*?)(?=^(?:Title|File|Line|Question|Options|Resolution|Description):|\[\/FINDING\]|$)/m;

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
  private currentFindingMeta: {
    categoryId: string;
    severity: FindingSeverity;
    needsInput: boolean;
  } | null = null;

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
   */
  processLine(line: string): Finding | null {
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
    return Array.from(this.findings.values());
  }

  /**
   * Get findings for the current session
   */
  getSessionFindings(): Finding[] {
    return this.getAllFindings().filter((f) => f.sourceSessionId === this.currentSessionId);
  }

  /**
   * Get findings by category
   */
  getFindingsByCategory(categoryId: string): Finding[] {
    return this.getAllFindings().filter((f) => f.categoryId === categoryId);
  }

  /**
   * Get findings needing user input
   */
  getFindingsNeedingInput(): Finding[] {
    return this.getAllFindings().filter((f) => f.status === "needs_input" && f.pendingQuestion);
  }

  /**
   * Get actionable findings
   */
  getActionableFindings(): Finding[] {
    return this.getAllFindings().filter(
      (f) => f.actionable && f.status !== "resolved" && f.status !== "wont_fix",
    );
  }

  // ==================== Execution Reports ====================

  /**
   * Start a new execution report
   */
  startReport(promptName: string, promptId?: string): ExecutionReport {
    const report: ExecutionReport = {
      id: crypto.randomUUID(),
      sessionId: this.currentSessionId,
      promptName,
      promptId,
      startedAt: Date.now(),
      status: "running",
      findings: [],
      summary: this.createEmptySummary(),
      pendingInputs: [],
      phases: [
        {
          phase: 1,
          startedAt: Date.now(),
          findingsDetected: 0,
          findingsResolved: 0,
        },
      ],
    };

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
    report.findings = sessionFindings;
    report.summary = this.calculateSummary(sessionFindings);
    report.pendingInputs = sessionFindings
      .filter((f) => f.pendingQuestion)
      .map((f) => f.pendingQuestion!);

    // Update phase info
    const currentPhase = report.phases[report.phases.length - 1];
    if (currentPhase) {
      currentPhase.findingsDetected = sessionFindings.filter(
        (f) => f.status === "detected" || f.status === "in_progress",
      ).length;
      currentPhase.findingsResolved = sessionFindings.filter((f) => f.status === "resolved").length;
    }

    // Update status based on pending inputs
    if (report.pendingInputs.length > 0 && report.status === "running") {
      report.status = "paused_for_input";
    }

    this.emit({ type: "report_updated", report });
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

    report.status = status;
    report.completedAt = Date.now();
    report.duration = report.completedAt - report.startedAt;

    // Complete current phase
    const currentPhase = report.phases[report.phases.length - 1];
    if (currentPhase) {
      currentPhase.completedAt = Date.now();
    }

    this.updateReport(this.currentReportId);
    this.currentReportId = null;

    console.log(`[FindingsTracker] Report completed: ${report.promptName} (${report.id})`);
    return report;
  }

  /**
   * Start a new phase in the current report
   */
  startNewPhase(): PhaseInfo | null {
    if (!this.currentReportId) return null;

    const report = this.reports.get(this.currentReportId);
    if (!report) return null;

    // Complete current phase
    const currentPhase = report.phases[report.phases.length - 1];
    if (currentPhase) {
      currentPhase.completedAt = Date.now();
    }

    // Start new phase
    const newPhase: PhaseInfo = {
      phase: report.phases.length + 1,
      startedAt: Date.now(),
      findingsDetected: 0,
      findingsResolved: 0,
    };

    report.phases.push(newPhase);
    report.status = "running";

    this.emit({ type: "report_updated", report });
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

  // ==================== Summary Helpers ====================

  /**
   * Create an empty summary
   */
  private createEmptySummary(): ReportSummary {
    const byCategory: Record<string, { count: number; actionable: number; resolved: number }> = {};
    for (const cat of BUILT_IN_CATEGORIES) {
      byCategory[cat.id] = { count: 0, actionable: 0, resolved: 0 };
    }

    return {
      totalFindings: 0,
      byCategory,
      bySeverity: { critical: 0, high: 0, medium: 0, low: 0, info: 0 },
      byStatus: {
        detected: 0,
        in_progress: 0,
        needs_input: 0,
        resolved: 0,
        wont_fix: 0,
        deferred: 0,
      },
      actionable: 0,
      needsUserInput: 0,
      autoFixed: 0,
      informational: 0,
    };
  }

  /**
   * Calculate summary from findings
   */
  private calculateSummary(findings: Finding[]): ReportSummary {
    const summary = this.createEmptySummary();
    summary.totalFindings = findings.length;

    for (const finding of findings) {
      // By category
      if (!summary.byCategory[finding.categoryId]) {
        summary.byCategory[finding.categoryId] = { count: 0, actionable: 0, resolved: 0 };
      }
      summary.byCategory[finding.categoryId].count++;
      if (finding.actionable) {
        summary.byCategory[finding.categoryId].actionable++;
      }
      if (finding.status === "resolved") {
        summary.byCategory[finding.categoryId].resolved++;
      }

      // By severity
      summary.bySeverity[finding.severity]++;

      // By status
      summary.byStatus[finding.status]++;

      // Action type counts
      if (finding.actionable) {
        summary.actionable++;
      }
      if (finding.status === "needs_input") {
        summary.needsUserInput++;
      }
      if (finding.actionType === "auto_fix" && finding.status === "resolved") {
        summary.autoFixed++;
      }
      if (finding.actionType === "informational") {
        summary.informational++;
      }
    }

    return summary;
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
    return this.getAllFindings().filter((f) => f.status !== "resolved" && f.status !== "wont_fix")
      .length;
  }
}

// Export singleton instance
export const findingsTracker = FindingsTracker.getInstance();
