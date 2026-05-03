/**
 * CoveragePanel — Section 11, Phase C2.
 *
 * Visualizes regression coverage for a single suite by combining two sources:
 *
 *   1. Static coverage — `coverageOf(ir, suite)` from `ui-bridge-auto/regression`
 *      tells us which IR states / transitions the *suite* covers, regardless of
 *      whether the suite has actually run.
 *
 *   2. Runtime exercise log — the `assertion_executions` table in PostgreSQL,
 *      queried via the Phase A2 Tauri command `query_assertion_executions_for_suite`.
 *      Cross-referenced against the suite via `coverageDiff(suite, log)` to
 *      surface assertions that the suite *defines* but no run has *exercised*.
 *
 * The panel is a presentational shell — all coverage logic lives in the pure
 * `coverageDiff` / `coverageOf` functions. Empty states cover both "no runs
 * yet" and "100% coverage". Determinism: the lists are sorted by the pure
 * functions, so the rendered order is stable across refetches.
 *
 * Wire-up: this component is intentionally caller-agnostic. The caller fetches
 * the suite + IR (via existing Tauri commands or in-memory state) and passes
 * them in — the panel only owns the exercise-log fetch.
 */

import { useCallback, useMemo } from "react";
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Loader2,
  RefreshCw,
  Shield,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";

import {
  coverageDiff,
  coverageOf,
  type AssertionExecution,
  type CoverageDiffReport,
  type CoverageReport,
  type RegressionSuite,
} from "@qontinui/ui-bridge-auto/regression";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";

// ---------------------------------------------------------------------------
// Tauri row shape — must match `crate::database::pg::regression::AssertionExecutionRow`.
// Snake-cased because the Rust struct does not set `#[serde(rename_all)]`.
// Kept narrow on purpose: `id`, `run_id`, `id_seq`, etc. are not consulted by
// the panel and are tolerated as `[k: string]: unknown` extras.
// ---------------------------------------------------------------------------

export interface AssertionExecutionRow {
  case_id: string;
  assertion_id: string;
  started_at: string;
  status: string;
  assertion_kind: string;
  duration_ms: number;
  failure_kind?: string | null;
  error_message?: string | null;
}

/**
 * Adapt the Rust-side snake_case row shape onto the camelCase
 * `AssertionExecution` contract that `coverageDiff` consumes.
 *
 * Pure / deterministic — no time, no I/O. Exposed (rather than inlined into
 * the component body) so the test suite can lock the field-mapping down
 * without spinning up jsdom.
 */
export function exerciseLogToAssertionExecutions(
  rows: AssertionExecutionRow[],
): AssertionExecution[] {
  return rows.map((r) => ({
    caseId: r.case_id,
    assertionId: r.assertion_id,
    executedAt: r.started_at,
  }));
}

// ---------------------------------------------------------------------------
// Pure stat-derivation — split out for direct test coverage.
// ---------------------------------------------------------------------------

export interface CoverageStats {
  /** From `coverageOf(ir, suite)` — what the suite *covers* in the IR. */
  staticCovered: number;
  staticTotal: number;
  /** From `coverageDiff(suite, log).stats` — what the runs *exercised*. */
  runtimeExercised: number;
  runtimeTotal: number;
  /** `coverageDiff.stats.uncoveredRatio`, rendered as a 0-100 integer percent. */
  uncoveredPercent: number;
}

export function deriveCoverageStats(
  staticCoverage: CoverageReport,
  diff: CoverageDiffReport,
): CoverageStats {
  return {
    staticCovered: staticCoverage.transitionsCovered,
    staticTotal: staticCoverage.totalTransitions,
    runtimeExercised: diff.stats.exercisedAssertions,
    runtimeTotal: diff.stats.totalAssertions,
    // `uncoveredRatio` is in [0, 1]; render as a whole-number percent for the
    // dashboard pill. Math.round (not floor/ceil) — 0.495 → 50%, 0.494 → 49%.
    uncoveredPercent: Math.round(diff.stats.uncoveredRatio * 100),
  };
}

// ---------------------------------------------------------------------------
// Grouping helpers — pure, exposed for tests.
// ---------------------------------------------------------------------------

export interface UnexercisedGroup {
  caseId: string;
  rows: Array<{ assertionId: string; assertionKind: string }>;
}

/**
 * Group `coverageDiff.unexercisedAssertions` by `caseId`, attaching the
 * assertion-kind from the suite when known. The `coverageDiff` output is
 * already sorted by `(caseId, assertionId)`, so iterating in order produces
 * stably-ordered groups.
 */
export function groupUnexercisedByCase(
  suite: RegressionSuite,
  diff: CoverageDiffReport,
): UnexercisedGroup[] {
  // Build `{ "case|assertion-id" -> kind }` from the suite. The id derivation
  // mirrors the convention `coverageDiff` uses internally (see
  // `_assertionIdOfForTesting` in coverage-diff.ts) — we re-derive here rather
  // than importing the private helper, so this stays robust if the helper is
  // renamed.
  const kindByKey = new Map<string, string>();
  for (const c of suite.cases) {
    for (const a of c.assertions) {
      let assertionId: string;
      switch (a.kind) {
        case "state-active":
          assertionId = `state-active:${a.phase}:${a.stateId}`;
          break;
        case "action-target-resolves":
          assertionId = `action-target-resolves:${a.transitionId}#${a.actionIndex}`;
          break;
        case "visual-gate":
          assertionId = `visual-gate:${a.stateId}`;
          break;
        case "overlay":
          assertionId = a.assertionId;
          break;
      }
      kindByKey.set(`${c.id}|${assertionId}`, a.kind);
    }
  }

  const groups = new Map<string, UnexercisedGroup>();
  for (const ref of diff.unexercisedAssertions) {
    let g = groups.get(ref.caseId);
    if (!g) {
      g = { caseId: ref.caseId, rows: [] };
      groups.set(ref.caseId, g);
    }
    g.rows.push({
      assertionId: ref.assertionId,
      assertionKind: kindByKey.get(`${ref.caseId}|${ref.assertionId}`) ?? "unknown",
    });
  }
  return Array.from(groups.values());
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export interface CoveragePanelProps {
  /** The suite UUID — keys both static coverage and exercise-log retrieval. */
  suiteId: string;
  /** Loaded suite (caller fetches it). */
  suite: RegressionSuite;
  /** Loaded IR (caller fetches it). */
  ir: IRDocument;
}

export function CoveragePanel(props: CoveragePanelProps): React.JSX.Element {
  const { suiteId, suite, ir } = props;

  // Live exercise-log fetch. The query key includes the suiteId so swapping
  // suites does not bleed cached rows from the previous one.
  const {
    data: rawExerciseLog,
    isLoading,
    isError,
    error,
    refetch,
    isFetching,
  } = useQuery<AssertionExecutionRow[]>({
    queryKey: ["regression-coverage", "exercise-log", suiteId],
    queryFn: () =>
      invoke<AssertionExecutionRow[]>("query_assertion_executions_for_suite", {
        suiteId,
      }),
  });

  const exerciseLog = useMemo(
    () => exerciseLogToAssertionExecutions(rawExerciseLog ?? []),
    [rawExerciseLog],
  );

  const diff = useMemo(() => coverageDiff(suite, exerciseLog), [suite, exerciseLog]);
  const staticCoverage = useMemo(() => coverageOf(ir, suite), [ir, suite]);
  const stats = useMemo(
    () => deriveCoverageStats(staticCoverage, diff),
    [staticCoverage, diff],
  );

  const unexercisedGroups = useMemo(
    () => groupUnexercisedByCase(suite, diff),
    [suite, diff],
  );

  const handleCopyAssertion = useCallback((assertionId: string) => {
    // `navigator.clipboard` is available in Tauri's webview. We swallow the
    // promise rejection — copy is a nice-to-have, not load-bearing.
    void navigator.clipboard?.writeText(assertionId).catch(() => {
      // no-op
    });
  }, []);

  const hasNoRuns = (rawExerciseLog?.length ?? 0) === 0;
  const isFullyCovered =
    !hasNoRuns &&
    diff.unexercisedAssertions.length === 0 &&
    diff.uncoveredTransitions.length === 0;

  // Truncate the suite id for display — full id available via tooltip.
  const truncatedSuiteId =
    suiteId.length > 12 ? `${suiteId.slice(0, 8)}…${suiteId.slice(-4)}` : suiteId;

  return (
    <div className="p-4">
      <div className="max-w-4xl mx-auto space-y-4">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div className="min-w-0">
            <h2 className="text-base font-semibold text-text-primary">Regression Coverage</h2>
            <p className="text-xs text-text-muted">
              Suite{" "}
              <code className="text-xs bg-muted px-1 py-0.5 rounded" title={suiteId}>
                {truncatedSuiteId}
              </code>
            </p>
          </div>
          <button
            onClick={() => void refetch()}
            disabled={isFetching}
            className="inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium border border-border-primary text-text-primary hover:bg-surface-primary disabled:opacity-50"
          >
            {isFetching ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
            Refresh
          </button>
        </div>

        {/* Loading state */}
        {isLoading && (
          <div className="rounded-lg border border-border-primary bg-surface-secondary p-6 flex items-center justify-center gap-2 text-sm text-text-muted">
            <Loader2 className="size-4 animate-spin" />
            Loading exercise log…
          </div>
        )}

        {/* Error state */}
        {isError && (
          <div className="rounded-lg border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-400">
            <div className="flex items-start gap-2">
              <AlertCircle className="size-4 mt-0.5 shrink-0" />
              <div>
                <div className="font-medium">Failed to load exercise log</div>
                <pre className="mt-1 whitespace-pre-wrap text-xs text-red-300">
                  {error instanceof Error ? error.message : String(error)}
                </pre>
              </div>
            </div>
          </div>
        )}

        {/* Stats row — always render once we have data, even if empty */}
        {!isLoading && !isError && (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="rounded-lg border border-border-primary bg-surface-secondary p-3">
              <div className="flex items-center gap-2 text-xs text-text-muted">
                <Shield className="size-3.5" />
                Static coverage
              </div>
              <div className="mt-1 text-lg font-semibold text-text-primary">
                {stats.staticCovered}/{stats.staticTotal}
                <span className="ml-1 text-xs font-normal text-text-muted">transitions</span>
              </div>
            </div>
            <div className="rounded-lg border border-border-primary bg-surface-secondary p-3">
              <div className="flex items-center gap-2 text-xs text-text-muted">
                <Activity className="size-3.5" />
                Runtime exercise
              </div>
              <div className="mt-1 text-lg font-semibold text-text-primary">
                {stats.runtimeExercised}/{stats.runtimeTotal}
                <span className="ml-1 text-xs font-normal text-text-muted">assertions</span>
              </div>
            </div>
            <div className="rounded-lg border border-border-primary bg-surface-secondary p-3">
              <div className="flex items-center gap-2 text-xs text-text-muted">
                <AlertCircle className="size-3.5" />
                Uncovered ratio
              </div>
              <div className="mt-1 text-lg font-semibold text-text-primary">
                {stats.uncoveredPercent}%
              </div>
            </div>
          </div>
        )}

        {/* Empty state — no runs */}
        {!isLoading && !isError && hasNoRuns && (
          <div className="rounded-lg border border-border-primary bg-surface-secondary p-6 text-center text-sm text-text-muted">
            No runs recorded yet — execute the suite via the{" "}
            <span className="font-medium text-text-primary">Regression Run</span> page.
          </div>
        )}

        {/* Empty state — fully covered */}
        {!isLoading && !isError && isFullyCovered && (
          <div className="rounded-lg border border-green-500/40 bg-green-500/10 p-4 flex items-center gap-2 text-sm text-green-400">
            <CheckCircle2 className="size-4" />
            <span>
              <strong className="text-green-300">100% coverage.</strong> Every assertion in the
              suite has been exercised by at least one run.
            </span>
          </div>
        )}

        {/* Two-column body — un-exercised assertions + un-covered transitions */}
        {!isLoading && !isError && !hasNoRuns && !isFullyCovered && (
          <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {/* Un-exercised assertions */}
            <div className="rounded-lg border border-border-primary bg-surface-secondary p-3">
              <div className="mb-2 flex items-center justify-between">
                <h3 className="text-sm font-medium text-text-primary">Un-exercised assertions</h3>
                <span className="text-xs text-text-muted">
                  {diff.unexercisedAssertions.length}
                </span>
              </div>
              {diff.unexercisedAssertions.length === 0 ? (
                <p className="text-xs text-text-muted">
                  Every assertion has been exercised by at least one run.
                </p>
              ) : (
                <div className="space-y-3 max-h-96 overflow-y-auto">
                  {unexercisedGroups.map((g) => (
                    <div key={g.caseId} className="space-y-1">
                      <div
                        className="text-xs font-mono text-text-muted truncate"
                        title={g.caseId}
                      >
                        case: {g.caseId}
                      </div>
                      <ul className="space-y-1">
                        {g.rows.map((r) => (
                          <li key={r.assertionId}>
                            <button
                              onClick={() => handleCopyAssertion(r.assertionId)}
                              title="Click to copy assertion id"
                              className="w-full text-left rounded-md border border-border-primary bg-surface-primary p-2 text-xs hover:bg-surface-hover focus:outline-hidden focus:ring-2 focus:ring-brand-primary"
                            >
                              <div className="flex items-center justify-between gap-2">
                                <span className="font-mono truncate">{r.assertionId}</span>
                                <span className="shrink-0 rounded-full border border-border-primary px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-text-muted">
                                  {r.assertionKind}
                                </span>
                              </div>
                            </button>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ))}
                </div>
              )}
            </div>

            {/* Un-covered transitions */}
            <div className="rounded-lg border border-border-primary bg-surface-secondary p-3">
              <div className="mb-2 flex items-center justify-between">
                <h3 className="text-sm font-medium text-text-primary">Un-covered transitions</h3>
                <span className="text-xs text-text-muted">
                  {diff.uncoveredTransitions.length}
                </span>
              </div>
              {diff.uncoveredTransitions.length === 0 ? (
                <p className="text-xs text-text-muted">
                  Every case has had at least one assertion fire.
                </p>
              ) : (
                <ul className="space-y-1 max-h-96 overflow-y-auto">
                  {diff.uncoveredTransitions.map((t) => (
                    <li
                      key={`${t.caseId}|${t.transitionId}`}
                      className="rounded-md border border-border-primary bg-surface-primary p-2 text-xs"
                    >
                      <div
                        className="font-mono text-text-muted truncate"
                        title={t.caseId}
                      >
                        case: {t.caseId}
                      </div>
                      <div className="font-mono text-text-primary truncate" title={t.transitionId}>
                        transition: {t.transitionId}
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default CoveragePanel;
