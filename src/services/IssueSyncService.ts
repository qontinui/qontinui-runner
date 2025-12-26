/**
 * IssueSyncService
 *
 * Service for synchronizing detected issues from the runner to the qontinui-web backend.
 * This enables issues detected during AI-assisted automation to be persisted and
 * displayed in the web Issues page.
 */

import { invoke } from "@tauri-apps/api/core";
import { issueTracker } from "./IssueTracker";
import type { DetectedIssue } from "../types/issues";

/** Response from the sync_issues_to_backend Tauri command */
export interface SyncIssuesResponse {
  synced: number;
  updated: number;
  errors: string[];
}

/** Issue format expected by the Tauri command */
interface IssueSyncItem {
  id: string;
  session_id: string;
  type: string;
  severity: string;
  title: string;
  description: string;
  file?: string;
  line?: number;
  source: {
    type: string;
    path?: string;
    line_range?: [number, number];
    description?: string;
  };
  status: string;
  resolution?: string;
  detected_at: number;
  resolved_at?: number;
}

/**
 * Convert a DetectedIssue to the format expected by the Tauri sync command
 */
function convertToSyncItem(issue: DetectedIssue): IssueSyncItem {
  return {
    id: issue.id,
    session_id: issue.session_id,
    type: issue.type,
    severity: issue.severity,
    title: issue.title,
    description: issue.description,
    file: issue.file,
    line: issue.line,
    source: {
      type: issue.source.type,
      path: issue.source.path,
      line_range: issue.source.line_range,
      description: issue.source.description,
    },
    status: issue.status,
    resolution: issue.resolution,
    detected_at: issue.detected_at,
    resolved_at: issue.resolved_at,
  };
}

/**
 * Get the currently selected project ID from localStorage
 */
function getSelectedProjectId(): string | null {
  const stored = localStorage.getItem("qontinui-selected-project");
  if (stored) {
    try {
      const parsed = JSON.parse(stored);
      return parsed.selectedProjectId || null;
    } catch {
      return null;
    }
  }
  return null;
}

/**
 * Sync all issues from the IssueTracker to the web backend
 *
 * @returns Promise with sync result including counts and any errors
 */
export async function syncIssuesToBackend(): Promise<SyncIssuesResponse> {
  const issues = issueTracker.getAllIssues();

  if (issues.length === 0) {
    console.log("[IssueSyncService] No issues to sync");
    return { synced: 0, updated: 0, errors: [] };
  }

  const projectId = getSelectedProjectId();
  console.log(
    `[IssueSyncService] Syncing ${issues.length} issues to backend (project: ${projectId || "none"})`,
  );

  const syncItems = issues.map(convertToSyncItem);

  try {
    const response = await invoke<SyncIssuesResponse>("sync_issues_to_backend", {
      issues: syncItems,
      projectId: projectId,
    });

    console.log(
      `[IssueSyncService] Sync complete: ${response.synced} new, ${response.updated} updated`,
    );

    if (response.errors.length > 0) {
      console.warn("[IssueSyncService] Sync errors:", response.errors);
    }

    return response;
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    console.error("[IssueSyncService] Sync failed:", errorMessage);

    // Return error in response format
    return {
      synced: 0,
      updated: 0,
      errors: [errorMessage],
    };
  }
}

/**
 * Sync specific issues to the backend
 *
 * @param issues - Array of issues to sync
 * @param projectId - Optional project ID to associate issues with
 * @returns Promise with sync result
 */
export async function syncSpecificIssues(
  issues: DetectedIssue[],
  projectId?: string,
): Promise<SyncIssuesResponse> {
  if (issues.length === 0) {
    return { synced: 0, updated: 0, errors: [] };
  }

  const syncItems = issues.map(convertToSyncItem);
  const resolvedProjectId = projectId || getSelectedProjectId();

  try {
    const response = await invoke<SyncIssuesResponse>("sync_issues_to_backend", {
      issues: syncItems,
      projectId: resolvedProjectId,
    });

    return response;
  } catch (error) {
    const errorMessage = error instanceof Error ? error.message : String(error);
    console.error("[IssueSyncService] Sync failed:", errorMessage);
    return {
      synced: 0,
      updated: 0,
      errors: [errorMessage],
    };
  }
}

/**
 * Sync issues for the current session only
 *
 * @returns Promise with sync result
 */
export async function syncSessionIssues(): Promise<SyncIssuesResponse> {
  const sessionIssues = issueTracker.getSessionIssues();
  return syncSpecificIssues(sessionIssues);
}

// Export as a namespace for convenient access
export const issueSyncService = {
  syncAll: syncIssuesToBackend,
  syncSession: syncSessionIssues,
  syncSpecific: syncSpecificIssues,
};
