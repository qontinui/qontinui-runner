/**
 * Complexity Scoring & Drift Detection
 *
 * Scores each page's complexity based on quantifiable metrics from specs
 * and registrations. Tracks complexity over time via git history of spec
 * files to detect drift.
 */

const API_BASE = "http://localhost:9876";

export interface ComplexityScore {
  page: string;
  route: string;
  scores: {
    elementCount: number;
    assertionCount: number;
    interactionCount: number;
    formFieldCount: number;
    apiCallCount: number;
    stateVariableCount: number;
    componentDepth: number;
  };
  composite: number;
  tier: "simple" | "moderate" | "complex" | "critical";
  drift?: DriftInfo;
}

export interface DriftInfo {
  complexityTrend: number[];
  velocityPerWeek: number;
  lastSignificantChange: string;
  warning?: string;
}

export interface ComplexityAnalysisResult {
  scores: ComplexityScore[];
  summary: {
    totalPages: number;
    simple: number;
    moderate: number;
    complex: number;
    critical: number;
    averageComposite: number;
    driftAlerts: DriftAlert[];
  };
}

export interface DriftAlert {
  page: string;
  route: string;
  warning: string;
  velocityPerWeek: number;
  currentComposite: number;
}

export async function analyzeComplexity(projectPath: string): Promise<ComplexityAnalysisResult> {
  const response = await fetch(`${API_BASE}/development-intelligence/complexity-scores`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ project_path: projectPath }),
  });

  if (!response.ok) {
    throw new Error(`Complexity analysis failed: ${response.statusText}`);
  }

  const result = await response.json();
  return result.data;
}

/**
 * Weights for composite score calculation.
 * API calls and component depth are primary complexity drivers.
 */
const WEIGHTS = {
  elementCount: 2,
  assertionCount: 1,
  interactionCount: 3,
  formFieldCount: 2,
  apiCallCount: 4,
  stateVariableCount: 2,
  componentDepth: 5,
} as const;

/**
 * Compute composite complexity score from individual metrics.
 * Raw score is normalized to 0-100 using sigmoid-like scaling.
 */
export function computeComposite(scores: ComplexityScore["scores"]): number {
  const raw =
    scores.elementCount * WEIGHTS.elementCount +
    scores.assertionCount * WEIGHTS.assertionCount +
    scores.interactionCount * WEIGHTS.interactionCount +
    scores.formFieldCount * WEIGHTS.formFieldCount +
    scores.apiCallCount * WEIGHTS.apiCallCount +
    scores.stateVariableCount * WEIGHTS.stateVariableCount +
    scores.componentDepth * WEIGHTS.componentDepth;

  // Normalize to 0-100 using a soft cap (sigmoid-like)
  // At raw=50 → ~50, at raw=100 → ~73, at raw=200 → ~90
  return Math.round((100 * raw) / (raw + 100));
}

/**
 * Determine complexity tier from composite score.
 */
export function getTier(composite: number): "simple" | "moderate" | "complex" | "critical" {
  if (composite < 25) return "simple";
  if (composite < 50) return "moderate";
  if (composite < 75) return "complex";
  return "critical";
}

/**
 * Get tier color for visualization.
 */
export function getTierColor(tier: "simple" | "moderate" | "complex" | "critical"): string {
  switch (tier) {
    case "simple":
      return "#10B981";
    case "moderate":
      return "#F59E0B";
    case "complex":
      return "#F97316";
    case "critical":
      return "#EF4444";
  }
}

/**
 * Client-side complexity scoring from spec data.
 */
export function scoreComplexityFromSpecs(
  specs: Array<{
    metadata: { pageId: string; pageUrl: string; component: string };
    groups: Array<{
      category: string;
      assertions: Array<{ enabled: boolean; category: string }>;
    }>;
  }>,
  componentMetrics?: Map<
    string,
    { stateVariableCount: number; componentDepth: number; apiCallCount: number }
  >,
): ComplexityScore[] {
  return specs.map((spec) => {
    const enabledAssertions = spec.groups.flatMap((g) => g.assertions.filter((a) => a.enabled));

    const elementCount = spec.groups
      .filter((g) => g.category === "element-presence")
      .reduce((sum, g) => sum + g.assertions.filter((a) => a.enabled).length, 0);

    const interactionCount = enabledAssertions.filter(
      (a) => a.category === "interaction" || a.category === "behavior",
    ).length;

    const formFieldCount = enabledAssertions.filter(
      (a) =>
        a.category === "interaction" &&
        spec.groups.some(
          (g) =>
            g.category === "interaction" &&
            g.assertions.includes(a) &&
            /form|input|select|textarea|field/i.test(JSON.stringify(g)),
        ),
    ).length;

    const extra = componentMetrics?.get(spec.metadata.component);

    const scores: ComplexityScore["scores"] = {
      elementCount,
      assertionCount: enabledAssertions.length,
      interactionCount,
      formFieldCount,
      apiCallCount: extra?.apiCallCount ?? 0,
      stateVariableCount: extra?.stateVariableCount ?? 0,
      componentDepth: extra?.componentDepth ?? 0,
    };

    const composite = computeComposite(scores);

    return {
      page: spec.metadata.pageId,
      route: spec.metadata.pageUrl,
      scores,
      composite,
      tier: getTier(composite),
    };
  });
}

/**
 * Detect drift by comparing complexity trends.
 */
export function detectDrift(
  current: ComplexityScore,
  historicalComposites: number[],
  testFileChangedRecently: boolean,
): DriftInfo | undefined {
  if (historicalComposites.length < 2) return undefined;

  const trend = [...historicalComposites, current.composite];
  const recent = trend.slice(-4);
  const oldest = recent[0];
  const newest = recent[recent.length - 1];
  const changePercent = oldest > 0 ? ((newest - oldest) / oldest) * 100 : 0;
  const weeksSpan = Math.max(1, recent.length - 1);
  const velocityPerWeek = changePercent / weeksSpan;

  const lastSignificantChange = new Date().toISOString();

  let warning: string | undefined;
  if (changePercent > 30 && !testFileChangedRecently) {
    warning = `Complexity increased ${Math.round(changePercent)}% without corresponding test updates`;
  }

  return {
    complexityTrend: trend,
    velocityPerWeek: Math.round(velocityPerWeek * 10) / 10,
    lastSignificantChange,
    warning,
  };
}

/**
 * Get score breakdown as labeled entries for display.
 */
export function getScoreBreakdown(
  scores: ComplexityScore["scores"],
): Array<{ label: string; value: number; weight: number; weighted: number }> {
  return [
    {
      label: "Elements",
      value: scores.elementCount,
      weight: WEIGHTS.elementCount,
      weighted: scores.elementCount * WEIGHTS.elementCount,
    },
    {
      label: "Assertions",
      value: scores.assertionCount,
      weight: WEIGHTS.assertionCount,
      weighted: scores.assertionCount * WEIGHTS.assertionCount,
    },
    {
      label: "Interactions",
      value: scores.interactionCount,
      weight: WEIGHTS.interactionCount,
      weighted: scores.interactionCount * WEIGHTS.interactionCount,
    },
    {
      label: "Form Fields",
      value: scores.formFieldCount,
      weight: WEIGHTS.formFieldCount,
      weighted: scores.formFieldCount * WEIGHTS.formFieldCount,
    },
    {
      label: "API Calls",
      value: scores.apiCallCount,
      weight: WEIGHTS.apiCallCount,
      weighted: scores.apiCallCount * WEIGHTS.apiCallCount,
    },
    {
      label: "State Variables",
      value: scores.stateVariableCount,
      weight: WEIGHTS.stateVariableCount,
      weighted: scores.stateVariableCount * WEIGHTS.stateVariableCount,
    },
    {
      label: "Component Depth",
      value: scores.componentDepth,
      weight: WEIGHTS.componentDepth,
      weighted: scores.componentDepth * WEIGHTS.componentDepth,
    },
  ];
}
