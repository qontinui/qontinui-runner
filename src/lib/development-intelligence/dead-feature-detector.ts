/**
 * Dead Feature Detection
 *
 * Cross-references spec freshness with git activity to identify features
 * that may be abandoned, deprecated, or have drifted from their specs.
 */

const API_BASE = "http://localhost:9876";

export interface FeatureHealth {
  page: string;
  route: string;
  componentPath: string;
  status: "active" | "stale" | "abandoned" | "spec-drift";
  lastCodeChange: string;
  lastSpecChange: string;
  codeCommitCount30d: number;
  specAge: number;
  codeAge: number;
  staleness: number;
  signals: string[];
}

export interface FeatureHealthResult {
  features: FeatureHealth[];
  summary: {
    total: number;
    active: number;
    stale: number;
    abandoned: number;
    specDrift: number;
  };
}

export async function analyzeFeatureHealth(projectPath: string): Promise<FeatureHealthResult> {
  const response = await fetch(`${API_BASE}/development-intelligence/feature-health`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ project_path: projectPath }),
  });

  if (!response.ok) {
    throw new Error(`Feature health analysis failed: ${response.statusText}`);
  }

  const result = await response.json();
  return result.data;
}

/**
 * Classification thresholds (in days).
 */
const THRESHOLDS = {
  activeCodeDays: 30,
  activeSpecDays: 60,
  staleDays: 60,
  abandonedDays: 90,
  specDriftCodeDays: 30,
  specDriftSpecDays: 90,
} as const;

/**
 * Classify feature health status from dates and activity.
 */
export function classifyFeature(input: {
  lastCodeChange: Date;
  lastSpecChange: Date;
  codeCommitCount30d: number;
  hasTestFiles: boolean;
  isDeprecatedInArchSpec: boolean;
  now?: Date;
}): {
  status: FeatureHealth["status"];
  staleness: number;
  signals: string[];
} {
  const now = input.now ?? new Date();
  const codeAge = daysBetween(input.lastCodeChange, now);
  const specAge = daysBetween(input.lastSpecChange, now);
  const signals: string[] = [];

  if (input.isDeprecatedInArchSpec) {
    signals.push("Architecture spec lists this as deprecated");
  }

  // Classify
  let status: FeatureHealth["status"];

  if (codeAge <= THRESHOLDS.activeCodeDays && specAge <= THRESHOLDS.activeSpecDays) {
    status = "active";
  } else if (codeAge <= THRESHOLDS.specDriftCodeDays && specAge > THRESHOLDS.specDriftSpecDays) {
    status = "spec-drift";
    signals.push(
      `Component modified ${input.codeCommitCount30d} times in 30 days but spec unchanged for ${Math.round(specAge / 30)} months`,
    );
  } else if (codeAge > THRESHOLDS.abandonedDays && !input.hasTestFiles) {
    status = "abandoned";
    signals.push(
      `No git commits touching this component since ${formatDate(input.lastCodeChange)}`,
    );
    if (!input.hasTestFiles) {
      signals.push("No test files reference this component");
    }
  } else if (codeAge > THRESHOLDS.staleDays) {
    status = "stale";
    signals.push(`No code changes in ${Math.round(codeAge)} days`);
  } else {
    status = "active";
  }

  // Compute staleness score (0-1, higher = more stale)
  const staleness = Math.min(1, (codeAge / 180 + specAge / 180) / 2);

  return { status, staleness, signals };
}

/**
 * Get the status color for visualization.
 */
export function getStatusColor(status: FeatureHealth["status"]): string {
  switch (status) {
    case "active":
      return "#10B981";
    case "stale":
      return "#F59E0B";
    case "spec-drift":
      return "#F97316";
    case "abandoned":
      return "#EF4444";
  }
}

/**
 * Get status label for display.
 */
export function getStatusLabel(status: FeatureHealth["status"]): string {
  switch (status) {
    case "active":
      return "Active";
    case "stale":
      return "Stale";
    case "spec-drift":
      return "Spec Drift";
    case "abandoned":
      return "Abandoned";
  }
}

/**
 * Group features by status for Kanban display.
 */
export function groupByStatus(
  features: FeatureHealth[],
): Record<FeatureHealth["status"], FeatureHealth[]> {
  const result: Record<FeatureHealth["status"], FeatureHealth[]> = {
    active: [],
    stale: [],
    "spec-drift": [],
    abandoned: [],
  };

  for (const feature of features) {
    result[feature.status].push(feature);
  }

  // Sort each group by staleness descending
  for (const group of Object.values(result)) {
    group.sort((a, b) => b.staleness - a.staleness);
  }

  return result;
}

/**
 * Get summary statistics for the pie chart.
 */
export function getStatusDistribution(
  features: FeatureHealth[],
): Array<{ name: string; value: number; color: string }> {
  const grouped = groupByStatus(features);
  return [
    {
      name: "Active",
      value: grouped.active.length,
      color: getStatusColor("active"),
    },
    {
      name: "Stale",
      value: grouped.stale.length,
      color: getStatusColor("stale"),
    },
    {
      name: "Spec Drift",
      value: grouped["spec-drift"].length,
      color: getStatusColor("spec-drift"),
    },
    {
      name: "Abandoned",
      value: grouped.abandoned.length,
      color: getStatusColor("abandoned"),
    },
  ].filter((d) => d.value > 0);
}

function daysBetween(a: Date, b: Date): number {
  return Math.abs(b.getTime() - a.getTime()) / (1000 * 60 * 60 * 24);
}

function formatDate(d: Date): string {
  return d.toLocaleDateString("en-US", {
    month: "short",
    year: "numeric",
  });
}
