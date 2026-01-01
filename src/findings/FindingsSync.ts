/**
 * FindingsSync Module
 *
 * Handles report management and syncing findings with the backend.
 * Manages execution reports, phases, and report lifecycle.
 */

import type {
  Finding,
  ExecutionReport,
  ReportSummary,
  PhaseInfo,
  UserInputRequest,
  ReportStatus,
} from "../types/findings";
import { calculateSummary, createEmptySummary } from "./FindingsExport";

/**
 * Create a new execution report
 */
export function createReport(
  sessionId: string,
  promptName: string,
  promptId?: string,
): ExecutionReport {
  return {
    id: crypto.randomUUID(),
    sessionId,
    promptName,
    promptId,
    startedAt: Date.now(),
    status: "running",
    findings: [],
    summary: createEmptySummary(),
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
}

/**
 * Update a report with current session findings
 */
export function updateReportWithFindings(
  report: ExecutionReport,
  sessionFindings: Finding[],
): ExecutionReport {
  const updatedReport = { ...report };
  updatedReport.findings = sessionFindings;
  updatedReport.summary = calculateSummary(sessionFindings);
  updatedReport.pendingInputs = sessionFindings
    .filter((f) => f.pendingQuestion)
    .map((f) => f.pendingQuestion as UserInputRequest);

  // Update phase info
  const currentPhase = updatedReport.phases[updatedReport.phases.length - 1];
  if (currentPhase) {
    currentPhase.findingsDetected = sessionFindings.filter(
      (f) => f.status === "detected" || f.status === "in_progress",
    ).length;
    currentPhase.findingsResolved = sessionFindings.filter((f) => f.status === "resolved").length;
  }

  // Update status based on pending inputs
  if (updatedReport.pendingInputs.length > 0 && updatedReport.status === "running") {
    updatedReport.status = "paused_for_input";
  }

  return updatedReport;
}

/**
 * Complete a report
 */
export function completeReport(
  report: ExecutionReport,
  status: "completed" | "failed" | "cancelled" = "completed",
): ExecutionReport {
  const completedReport = { ...report };
  completedReport.status = status;
  completedReport.completedAt = Date.now();
  completedReport.duration = completedReport.completedAt - completedReport.startedAt;

  // Complete current phase
  const currentPhase = completedReport.phases[completedReport.phases.length - 1];
  if (currentPhase) {
    currentPhase.completedAt = Date.now();
  }

  return completedReport;
}

/**
 * Start a new phase in a report
 */
export function startNewPhase(report: ExecutionReport): {
  report: ExecutionReport;
  phase: PhaseInfo;
} {
  const updatedReport = { ...report };

  // Complete current phase
  const currentPhase = updatedReport.phases[updatedReport.phases.length - 1];
  if (currentPhase) {
    currentPhase.completedAt = Date.now();
  }

  // Start new phase
  const newPhase: PhaseInfo = {
    phase: updatedReport.phases.length + 1,
    startedAt: Date.now(),
    findingsDetected: 0,
    findingsResolved: 0,
  };

  updatedReport.phases = [...updatedReport.phases, newPhase];
  updatedReport.status = "running";

  return { report: updatedReport, phase: newPhase };
}

/**
 * Backend sync options
 */
export interface BackendSyncOptions {
  baseUrl: string;
  projectId: string;
  authToken?: string;
  timeout?: number;
}

/**
 * Sync result from backend
 */
export interface SyncResult {
  success: boolean;
  syncedCount: number;
  errors: string[];
}

/**
 * Sync findings to backend API
 */
export async function syncFindingsToBackend(
  findings: Finding[],
  options: BackendSyncOptions,
): Promise<SyncResult> {
  const result: SyncResult = {
    success: false,
    syncedCount: 0,
    errors: [],
  };

  if (findings.length === 0) {
    result.success = true;
    return result;
  }

  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), options.timeout || 30000);

    const response = await fetch(
      `${options.baseUrl}/api/v1/projects/${options.projectId}/findings`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(options.authToken && { Authorization: `Bearer ${options.authToken}` }),
        },
        body: JSON.stringify({ findings }),
        signal: controller.signal,
      },
    );

    clearTimeout(timeoutId);

    if (!response.ok) {
      const errorText = await response.text();
      result.errors.push(`Backend returned ${response.status}: ${errorText}`);
      return result;
    }

    const data = await response.json();
    result.success = true;
    result.syncedCount = data.syncedCount || findings.length;
    console.log(`[FindingsSync] Synced ${result.syncedCount} findings to backend`);
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    result.errors.push(`Sync failed: ${errorMessage}`);
    console.error("[FindingsSync] Backend sync error:", error);
  }

  return result;
}

/**
 * Sync a report to backend API
 */
export async function syncReportToBackend(
  report: ExecutionReport,
  options: BackendSyncOptions,
): Promise<SyncResult> {
  const result: SyncResult = {
    success: false,
    syncedCount: 0,
    errors: [],
  };

  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), options.timeout || 30000);

    const response = await fetch(
      `${options.baseUrl}/api/v1/projects/${options.projectId}/reports`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(options.authToken && { Authorization: `Bearer ${options.authToken}` }),
        },
        body: JSON.stringify({ report }),
        signal: controller.signal,
      },
    );

    clearTimeout(timeoutId);

    if (!response.ok) {
      const errorText = await response.text();
      result.errors.push(`Backend returned ${response.status}: ${errorText}`);
      return result;
    }

    result.success = true;
    result.syncedCount = 1;
    console.log(`[FindingsSync] Synced report ${report.id} to backend`);
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    result.errors.push(`Sync failed: ${errorMessage}`);
    console.error("[FindingsSync] Report sync error:", error);
  }

  return result;
}

/**
 * Check backend connectivity
 */
export async function checkBackendHealth(baseUrl: string): Promise<boolean> {
  try {
    const response = await fetch(`${baseUrl}/health`, {
      method: "GET",
      signal: AbortSignal.timeout(5000),
    });
    return response.ok;
  } catch {
    return false;
  }
}
