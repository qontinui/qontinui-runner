/**
 * Coverage Gap Analysis
 *
 * Compares page specs (what SHOULD be tested) against actual test files
 * (what IS tested). Surfaces pages with rich specs but few/no tests.
 */

const API_BASE = "http://localhost:9876";

export interface CoverageGap {
  page: string;
  specAssertionCount: number;
  criticalAssertionCount: number;
  testFileCount: number;
  testAssertionCount: number;
  coverageScore: number;
  gaps: GapDetail[];
}

export interface GapDetail {
  specGroupId: string;
  specGroupName: string;
  category: string;
  assertionCount: number;
  hasCorrespondingTest: boolean;
  severity: "critical" | "warning" | "info";
}

export interface CoverageAnalysisResult {
  gaps: CoverageGap[];
  summary: {
    totalPages: number;
    wellCovered: number;
    partiallyCovered: number;
    uncovered: number;
    averageCoverageScore: number;
  };
}

interface SpecFile {
  id: string;
  metadata: {
    component: string;
    pageId: string;
    pageUrl: string;
    tags?: string[];
  };
  groups: SpecGroup[];
}

interface SpecGroup {
  id: string;
  name: string;
  category: string;
  assertions: SpecAssertion[];
}

interface SpecAssertion {
  id: string;
  category: string;
  severity: "critical" | "warning" | "info";
  enabled: boolean;
}

export async function analyzeCoverage(projectPath: string): Promise<CoverageAnalysisResult> {
  const response = await fetch(`${API_BASE}/development-intelligence/coverage-analysis`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ project_path: projectPath }),
  });

  if (!response.ok) {
    throw new Error(`Coverage analysis failed: ${response.statusText}`);
  }

  const result = await response.json();
  return result.data;
}

/**
 * Client-side coverage analysis from pre-loaded spec data.
 * Used when specs are already available (avoids extra round-trip).
 */
export function analyzeSpecCoverage(
  specs: SpecFile[],
  testFiles: TestFileInfo[],
): CoverageAnalysisResult {
  const gaps: CoverageGap[] = [];

  for (const spec of specs) {
    const component = spec.metadata.component;
    const pageId = spec.metadata.pageId;

    // Find test files referencing this page
    const matchingTests = testFiles.filter(
      (tf) =>
        tf.referencedComponents.includes(component) ||
        tf.referencedComponents.includes(pageId) ||
        tf.filePath.toLowerCase().includes(pageId.toLowerCase()),
    );

    const totalAssertions = spec.groups.reduce(
      (sum, g) => sum + g.assertions.filter((a) => a.enabled).length,
      0,
    );

    const criticalAssertions = spec.groups.reduce(
      (sum, g) => sum + g.assertions.filter((a) => a.enabled && a.severity === "critical").length,
      0,
    );

    const totalTestAssertions = matchingTests.reduce((sum, tf) => sum + tf.assertionCount, 0);

    // Analyze per-group coverage
    const groupGaps: GapDetail[] = spec.groups.map((group) => {
      const enabledAssertions = group.assertions.filter((a) => a.enabled);
      const groupKeywords = extractKeywords(group.name);
      const hasTest = matchingTests.some((tf) =>
        groupKeywords.some((kw) => tf.content.toLowerCase().includes(kw.toLowerCase())),
      );

      const maxSeverity = enabledAssertions.reduce<"critical" | "warning" | "info">((max, a) => {
        const order = { critical: 3, warning: 2, info: 1 };
        return order[a.severity] > order[max] ? a.severity : max;
      }, "info");

      return {
        specGroupId: group.id,
        specGroupName: group.name,
        category: group.category,
        assertionCount: enabledAssertions.length,
        hasCorrespondingTest: hasTest,
        severity: maxSeverity,
      };
    });

    const coverageScore =
      totalAssertions > 0 ? Math.min(1, totalTestAssertions / totalAssertions) : 0;

    gaps.push({
      page: pageId,
      specAssertionCount: totalAssertions,
      criticalAssertionCount: criticalAssertions,
      testFileCount: matchingTests.length,
      testAssertionCount: totalTestAssertions,
      coverageScore,
      gaps: groupGaps.filter((g) => !g.hasCorrespondingTest),
    });
  }

  // Sort by gap severity (most critical gaps first)
  gaps.sort((a, b) => {
    const aCriticalGaps = a.gaps.filter((g) => g.severity === "critical").length;
    const bCriticalGaps = b.gaps.filter((g) => g.severity === "critical").length;
    if (aCriticalGaps !== bCriticalGaps) return bCriticalGaps - aCriticalGaps;
    return a.coverageScore - b.coverageScore;
  });

  const wellCovered = gaps.filter((g) => g.coverageScore > 0.7).length;
  const partiallyCovered = gaps.filter(
    (g) => g.coverageScore > 0.3 && g.coverageScore <= 0.7,
  ).length;
  const uncovered = gaps.filter((g) => g.coverageScore <= 0.3).length;
  const averageCoverageScore =
    gaps.length > 0 ? gaps.reduce((sum, g) => sum + g.coverageScore, 0) / gaps.length : 0;

  return {
    gaps,
    summary: {
      totalPages: gaps.length,
      wellCovered,
      partiallyCovered,
      uncovered,
      averageCoverageScore,
    },
  };
}

export interface TestFileInfo {
  filePath: string;
  content: string;
  assertionCount: number;
  referencedComponents: string[];
}

/**
 * Count test assertions in file content.
 */
export function countTestAssertions(content: string): number {
  const patterns = [/\bexpect\s*\(/g, /\bassert\s*[.(]/g, /\bit\s*\(/g, /\btest\s*\(/g];
  return patterns.reduce((count, pattern) => {
    const matches = content.match(pattern);
    return count + (matches?.length ?? 0);
  }, 0);
}

/**
 * Extract keywords from a group name for fuzzy test matching.
 */
function extractKeywords(groupName: string): string[] {
  return groupName
    .replace(/[^a-zA-Z0-9\s]/g, " ")
    .split(/\s+/)
    .filter((w) => w.length > 3)
    .map((w) => w.toLowerCase());
}

/**
 * Get the coverage tier color for a score.
 */
export function getCoverageTierColor(score: number): string {
  if (score > 0.7) return "#10B981"; // green
  if (score > 0.3) return "#F59E0B"; // yellow
  return "#EF4444"; // red
}

/**
 * Get the coverage tier label for a score.
 */
export function getCoverageTier(score: number): "good" | "partial" | "uncovered" {
  if (score > 0.7) return "good";
  if (score > 0.3) return "partial";
  return "uncovered";
}
