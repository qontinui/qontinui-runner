/**
 * useFindingsData Hook
 *
 * React hook for fetching and managing findings data for the dashboard widget.
 * Polls the findings API and computes aggregated statistics.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import type { Finding, FindingsData, FindingSeverity, FindingStatus } from "./types";
import { getSeverityColors, getStatusColors } from "@/design-system";

/** Polling interval for findings data (5 seconds - less frequent than other widgets) */
const POLL_INTERVAL_MS = 5000;

/** API endpoint for findings */
const FINDINGS_API_URL = "http://localhost:9876/findings/summary";

/**
 * Backend finding format (from Rust API).
 */
interface BackendFinding {
  id: string;
  taskRunId: string;
  sessionNum: number;
  categoryId: string;
  severity: string;
  status: string;
  actionType: string;
  title: string;
  description: string;
  resolution?: string;
  codeContext?: {
    file?: string;
    line?: number;
    column?: number;
    snippet?: string;
  };
  signatureHash: string;
  userInput?: {
    question: string;
    inputType: string;
    options?: string[];
  };
  userResponse?: string;
  detectedAt: string;
  resolvedAt?: string;
  resolvedInSession?: number;
  updatedAt: string;
}

/**
 * Map backend finding to frontend Finding type.
 */
function mapBackendFinding(backend: BackendFinding): Finding {
  return {
    id: backend.id,
    category: (backend.categoryId || "code_bug") as Finding["category"],
    severity: (backend.severity || "medium") as FindingSeverity,
    status: (backend.status || "detected") as FindingStatus,
    title: backend.title,
    description: backend.description,
    filePath: backend.codeContext?.file,
    lineNumber: backend.codeContext?.line,
    suggestedFix: backend.resolution,
    detectedAt: backend.detectedAt ? new Date(backend.detectedAt).getTime() : Date.now(),
  };
}

/**
 * Compute aggregated counts from findings array.
 */
function computeAggregates(findings: Finding[]): {
  bySeverity: Record<FindingSeverity, number>;
  byStatus: Record<FindingStatus, number>;
} {
  const bySeverity: Record<FindingSeverity, number> = {
    critical: 0,
    high: 0,
    medium: 0,
    low: 0,
    info: 0,
  };

  const byStatus: Record<FindingStatus, number> = {
    detected: 0,
    in_progress: 0,
    needs_input: 0,
    resolved: 0,
  };

  for (const finding of findings) {
    bySeverity[finding.severity]++;
    byStatus[finding.status]++;
  }

  return { bySeverity, byStatus };
}

/**
 * Sort findings by severity (most severe first), then by detection time (newest first).
 */
function sortFindingsBySeverity(findings: Finding[]): Finding[] {
  const severityOrder: Record<FindingSeverity, number> = {
    critical: 0,
    high: 1,
    medium: 2,
    low: 3,
    info: 4,
  };

  return [...findings].sort((a, b) => {
    const severityDiff = severityOrder[a.severity] - severityOrder[b.severity];
    if (severityDiff !== 0) {
      return severityDiff;
    }
    // Same severity: sort by detection time (newest first)
    return b.detectedAt - a.detectedAt;
  });
}

/**
 * Hook for accessing findings data.
 * Polls the API and returns aggregated findings data.
 */
export function useFindingsData(): FindingsData {
  const [findings, setFindings] = useState<Finding[]>([]);

  const fetchFindings = useCallback(async () => {
    try {
      const response = await fetch(FINDINGS_API_URL);
      if (!response.ok) {
        // API not available or error - return empty data
        setFindings([]);
        return;
      }

      const data = await response.json();
      // API returns { success: true, data: { findings: BackendFinding[], ... } }
      const backendFindings: BackendFinding[] = data?.data?.findings || data?.findings || [];

      // Map backend findings to frontend format
      const mappedFindings = backendFindings.map(mapBackendFinding);
      setFindings(mappedFindings);
    } catch {
      // Network error or API not available
      setFindings([]);
    }
  }, []);

  // Initial fetch and polling
  useEffect(() => {
    fetchFindings();

    const interval = setInterval(fetchFindings, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [fetchFindings]);

  // Compute aggregated data
  const data = useMemo((): FindingsData => {
    const sortedFindings = sortFindingsBySeverity(findings);
    const { bySeverity, byStatus } = computeAggregates(findings);

    return {
      findings: sortedFindings,
      totalCount: findings.length,
      bySeverity,
      byStatus,
    };
  }, [findings]);

  return data;
}

/**
 * Get actionable findings count (excludes resolved and info-level).
 */
export function getActionableFindingsCount(data: FindingsData): number {
  return data.totalCount - data.byStatus.resolved - data.bySeverity.info;
}

/**
 * Get severity display configuration.
 */
export function getSeverityConfig(severity: FindingSeverity): {
  label: string;
  color: string;
  bgColor: string;
  borderColor: string;
} {
  const severityColors = getSeverityColors(severity);
  const labels: Record<FindingSeverity, string> = {
    critical: "Critical",
    high: "High",
    medium: "Medium",
    low: "Low",
    info: "Info",
  };

  return {
    label: labels[severity],
    color: severityColors.text,
    bgColor: severityColors.bg,
    borderColor: severityColors.border,
  };
}

/**
 * Get category display configuration.
 */
export function getCategoryConfig(category: string): {
  label: string;
  icon: string;
} {
  const configs: Record<string, { label: string; icon: string }> = {
    code_bug: { label: "Bug", icon: "Bug" },
    security: { label: "Security", icon: "Shield" },
    performance: { label: "Performance", icon: "Zap" },
    todo: { label: "TODO", icon: "CheckCircle" },
    enhancement: { label: "Enhancement", icon: "Sparkles" },
    config_issue: { label: "Config", icon: "Settings" },
    test_issue: { label: "Test", icon: "FlaskConical" },
    documentation: { label: "Docs", icon: "FileText" },
  };

  return configs[category] ?? { label: category, icon: "AlertTriangle" };
}

/**
 * Get status display configuration.
 */
export function getStatusConfig(status: FindingStatus): {
  label: string;
  color: string;
} {
  const labels: Record<FindingStatus, string> = {
    detected: "Detected",
    in_progress: "In Progress",
    needs_input: "Needs Input",
    resolved: "Resolved",
  };

  // Map finding status to design system status
  const statusMap: Record<FindingStatus, string> = {
    detected: "warning",
    in_progress: "pending",
    needs_input: "paused",
    resolved: "success",
  };

  const statusColors = getStatusColors(statusMap[status]);

  return {
    label: labels[status],
    color: statusColors.text,
  };
}
