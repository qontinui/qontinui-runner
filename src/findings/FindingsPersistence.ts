/**
 * FindingsPersistence Module
 *
 * Handles saving and loading findings data to/from disk.
 * Uses Tauri's invoke API for filesystem operations.
 */

import type { Finding, ExecutionReport } from "../types/findings";
import type { PersistedFindingsData, SessionHistoryEntry } from "./types";

/**
 * Save findings data to disk via Tauri
 */
export async function persistFindingsData(data: {
  currentSessionId: string;
  findings: Finding[];
  reports: ExecutionReport[];
  sessionHistory: SessionHistoryEntry[];
}): Promise<void> {
  try {
    const persistedData: PersistedFindingsData = {
      version: 1,
      savedAt: Date.now(),
      currentSessionId: data.currentSessionId,
      findings: data.findings,
      reports: data.reports,
      sessionHistory: data.sessionHistory,
    };

    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("save_findings_data", { data: JSON.stringify(persistedData, null, 2) });
    console.log("[FindingsPersistence] Persisted to disk");
  } catch (error) {
    console.error("[FindingsPersistence] Failed to persist to disk:", error);
    throw error;
  }
}

/**
 * Load findings data from disk via Tauri
 */
export async function loadFindingsData(): Promise<PersistedFindingsData | null> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const jsonStr = await invoke<string>("load_findings_data");

    if (!jsonStr) {
      console.log("[FindingsPersistence] No persisted data found");
      return null;
    }

    const data: PersistedFindingsData = JSON.parse(jsonStr);
    console.log(
      `[FindingsPersistence] Loaded from disk: ${data.sessionHistory?.length || 0} sessions in history`,
    );
    return data;
  } catch (error) {
    console.error("[FindingsPersistence] Failed to load from disk:", error);
    return null;
  }
}

/**
 * Archive a session to history
 * Returns the updated session history array (limited to last 50 entries)
 */
export function archiveSession(
  currentReport: ExecutionReport,
  sessionId: string,
  sessionFindings: Finding[],
  existingHistory: SessionHistoryEntry[],
  status: "completed" | "failed" | "timeout" | "context_limit" = "completed",
): SessionHistoryEntry[] {
  const entry: SessionHistoryEntry = {
    id: currentReport.id,
    sessionId,
    promptName: currentReport.promptName,
    startedAt: currentReport.startedAt,
    completedAt: Date.now(),
    duration: Date.now() - currentReport.startedAt,
    status,
    phases: currentReport.phases.length,
    findings: sessionFindings,
    summary: {
      total: sessionFindings.length,
      resolved: sessionFindings.filter((f) => f.status === "resolved").length,
      pending: sessionFindings.filter((f) => f.status === "detected" || f.status === "in_progress")
        .length,
      bySeverity: {
        critical: sessionFindings.filter((f) => f.severity === "critical").length,
        high: sessionFindings.filter((f) => f.severity === "high").length,
        medium: sessionFindings.filter((f) => f.severity === "medium").length,
        low: sessionFindings.filter((f) => f.severity === "low").length,
        info: sessionFindings.filter((f) => f.severity === "info").length,
      },
      byCategory: sessionFindings.reduce(
        (acc, f) => {
          acc[f.categoryId] = (acc[f.categoryId] || 0) + 1;
          return acc;
        },
        {} as Record<string, number>,
      ),
    },
  };

  // Add to history (keep last 50 sessions)
  const newHistory = [entry, ...existingHistory];
  if (newHistory.length > 50) {
    return newHistory.slice(0, 50);
  }
  return newHistory;
}
