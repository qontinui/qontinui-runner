/**
 * FindingsExport Module
 *
 * Handles exporting findings to various formats and calculating summaries.
 */

import type { Finding, ReportSummary, FindingSeverity, FindingStatus } from "../types/findings";
import { BUILT_IN_CATEGORIES } from "../services/FindingCategories";

/**
 * Create an empty report summary with all categories initialized
 */
export function createEmptySummary(): ReportSummary {
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
 * Calculate summary statistics from findings
 */
export function calculateSummary(findings: Finding[]): ReportSummary {
  const summary = createEmptySummary();
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

/**
 * Export format types
 */
export type ExportFormat = "json" | "markdown" | "csv";

/**
 * Export options
 */
export interface ExportOptions {
  format: ExportFormat;
  includeResolved?: boolean;
  includeMetadata?: boolean;
  groupBy?: "category" | "severity" | "status" | "none";
}

/**
 * Export findings to JSON format
 */
export function exportToJson(findings: Finding[], options?: Partial<ExportOptions>): string {
  const filtered = options?.includeResolved
    ? findings
    : findings.filter((f) => f.status !== "resolved");

  const data = {
    exportedAt: new Date().toISOString(),
    totalFindings: filtered.length,
    summary: calculateSummary(filtered),
    findings: filtered.map((f) => ({
      ...f,
      // Optionally exclude metadata
      ...(options?.includeMetadata === false && { metadata: undefined }),
    })),
  };

  return JSON.stringify(data, null, 2);
}

/**
 * Export findings to Markdown format
 */
export function exportToMarkdown(findings: Finding[], options?: Partial<ExportOptions>): string {
  const filtered = options?.includeResolved
    ? findings
    : findings.filter((f) => f.status !== "resolved");

  const summary = calculateSummary(filtered);
  const groupBy = options?.groupBy || "category";

  let md = "# Findings Report\n\n";
  md += `*Exported at: ${new Date().toISOString()}*\n\n`;

  // Summary section
  md += "## Summary\n\n";
  md += `- **Total Findings:** ${summary.totalFindings}\n`;
  md += `- **Actionable:** ${summary.actionable}\n`;
  md += `- **Needs User Input:** ${summary.needsUserInput}\n`;
  md += `- **Auto-Fixed:** ${summary.autoFixed}\n`;
  md += `- **Informational:** ${summary.informational}\n\n`;

  // Severity breakdown
  md += "### By Severity\n\n";
  md += "| Severity | Count |\n";
  md += "|----------|-------|\n";
  for (const [severity, count] of Object.entries(summary.bySeverity)) {
    if (count > 0) {
      md += `| ${severity} | ${count} |\n`;
    }
  }
  md += "\n";

  // Findings section
  md += "## Findings\n\n";

  if (groupBy === "none") {
    for (const finding of filtered) {
      md += formatFindingMarkdown(finding);
    }
  } else {
    // Group findings
    const groups = new Map<string, Finding[]>();
    for (const finding of filtered) {
      const key =
        groupBy === "category"
          ? finding.categoryId
          : groupBy === "severity"
            ? finding.severity
            : finding.status;
      const existing = groups.get(key) || [];
      existing.push(finding);
      groups.set(key, existing);
    }

    for (const [groupKey, groupFindings] of groups) {
      md += `### ${formatGroupHeader(groupKey)}\n\n`;
      for (const finding of groupFindings) {
        md += formatFindingMarkdown(finding);
      }
    }
  }

  return md;
}

/**
 * Format a single finding as Markdown
 */
function formatFindingMarkdown(finding: Finding): string {
  let md = `#### ${finding.title}\n\n`;
  md += `- **ID:** \`${finding.id}\`\n`;
  md += `- **Category:** ${finding.categoryId}\n`;
  md += `- **Severity:** ${finding.severity}\n`;
  md += `- **Status:** ${finding.status}\n`;

  if (finding.codeContext?.file) {
    md += `- **File:** \`${finding.codeContext.file}\``;
    if (finding.codeContext.line) {
      md += `:${finding.codeContext.line}`;
    }
    md += "\n";
  }

  md += `\n${finding.description}\n\n`;

  if (finding.resolution) {
    md += `**Resolution:** ${finding.resolution}\n\n`;
  }

  md += "---\n\n";
  return md;
}

/**
 * Format group header for display
 */
function formatGroupHeader(key: string): string {
  // Convert snake_case or camelCase to Title Case
  return key
    .replace(/_/g, " ")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

/**
 * Export findings to CSV format
 */
export function exportToCsv(findings: Finding[], options?: Partial<ExportOptions>): string {
  const filtered = options?.includeResolved
    ? findings
    : findings.filter((f) => f.status !== "resolved");

  const headers = [
    "ID",
    "Category",
    "Severity",
    "Status",
    "Title",
    "Description",
    "File",
    "Line",
    "Detected At",
    "Resolved At",
    "Resolution",
  ];

  const rows = filtered.map((f) => [
    f.id,
    f.categoryId,
    f.severity,
    f.status,
    escapeCsvField(f.title),
    escapeCsvField(f.description),
    f.codeContext?.file || "",
    f.codeContext?.line?.toString() || "",
    new Date(f.detectedAt).toISOString(),
    f.resolvedAt ? new Date(f.resolvedAt).toISOString() : "",
    escapeCsvField(f.resolution || ""),
  ]);

  return [headers.join(","), ...rows.map((r) => r.join(","))].join("\n");
}

/**
 * Escape a field for CSV format
 */
function escapeCsvField(field: string): string {
  if (field.includes(",") || field.includes('"') || field.includes("\n")) {
    return `"${field.replace(/"/g, '""')}"`;
  }
  return field;
}

/**
 * Export findings based on options
 */
export function exportFindings(
  findings: Finding[],
  options: ExportOptions = { format: "json" },
): string {
  switch (options.format) {
    case "json":
      return exportToJson(findings, options);
    case "markdown":
      return exportToMarkdown(findings, options);
    case "csv":
      return exportToCsv(findings, options);
    default:
      return exportToJson(findings, options);
  }
}
