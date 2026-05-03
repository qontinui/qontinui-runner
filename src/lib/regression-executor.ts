/**
 * TS-side regression executor (Section 11, Phase A3).
 *
 * Walks a serialized `RegressionSuite` against a live UI Bridge registry,
 * dispatches each `RegressionAssertion` against the appropriate runtime
 * primitive (`executeQuery` for action targets, `findFirst` per
 * `requiredElement` for state-active, `ScreenshotAssertionManager` for
 * visual gates, the three built-in overlay handlers for overlay assertions),
 * builds a `RegressionRunResult`, and feeds it through the pure `diagnose`
 * function from `@qontinui/ui-bridge-auto/diagnosis`. Every result is
 * persisted via the Tauri commands shipped in Phase A2.
 *
 * Design notes:
 *   - The executor is the single point that adapts the spec-only suite
 *     against live page state. The library never calls back into the runner.
 *   - Each persistence write is a thin Tauri `invoke` — no caching, no
 *     dedup. The plan explicitly says persistence is append-only for now.
 *   - Determinism is the library's responsibility (sorting, canonical JSON);
 *     the executor only produces inputs in caller-supplied order.
 *   - The `assertionId` keying rule mirrors `state/self-diagnosis.ts:assertionIdOf`:
 *     overlay assertions reuse `OverlayAssertion.assertionId`; the others
 *     are derived deterministically from `(caseId, kind, secondary-key)`.
 */

import { invoke } from "@tauri-apps/api/core";

import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";
import type {
  RegressionSuite,
  RegressionCase,
  RegressionAssertion,
  ActionTargetResolvesAssertion,
  OverlayAssertion,
  StateActiveAssertion,
  VisualGateAssertion,
} from "@qontinui/ui-bridge-auto/regression";
import { serializeSuite } from "@qontinui/ui-bridge-auto/regression";
import {
  diagnose,
  type RegressionFailure,
  type RegressionRunResult,
  type SelfDiagnosis,
} from "@qontinui/ui-bridge-auto/diagnosis";
import { findFirst, executeQuery, type RegistryLike } from "@qontinui/ui-bridge-auto/runtime";
import { ScreenshotAssertionManager, TauriBaselineStore } from "@qontinui/ui-bridge-auto/visual";
import type { GitCommitRef, DriftReport } from "@qontinui/ui-bridge-auto/drift";
import type { DriftContext } from "@qontinui/ui-bridge-auto/drift";

import { createPgMemorySink } from "./regression-memory-sink";

// `RecordingSession` and `FragilityScore` flow through `DriftContext`. We
// derive their types from `DriftContext` itself rather than importing them
// directly from the upstream package — the runner ships an ambient stub for
// the bare `@qontinui/ui-bridge-auto` module that shadows the real exports,
// so deriving via `DriftContext[K]` is the only way to land on the same
// types the engine actually consumes.
type RecordingSession = DriftContext["session"];
type FragilityScore = NonNullable<DriftContext["priors"]>[number];

// ---------------------------------------------------------------------------
// Public API — types
// ---------------------------------------------------------------------------

/** Status of a single assertion execution row. */
export type AssertionExecutionStatus = "pass" | "fail" | "skip";

/**
 * Per-assertion exercise row that flows into the
 * `record_assertion_executions_batch` Tauri command. Field names match the
 * Rust shape (snake_case) defined in Phase A2.
 */
export interface AssertionExecutionRow {
  id: string;
  /**
   * Set after the run is recorded. Empty during the per-assertion walk,
   * filled in once `record_regression_run` returns the run UUID.
   */
  run_id: string;
  case_id: string;
  assertion_id: string;
  assertion_kind: RegressionAssertion["kind"];
  status: AssertionExecutionStatus;
  /** ISO 8601 string. */
  started_at: string;
  duration_ms: number;
  /** Only populated when `status === "fail"`. */
  failure_kind?: string;
  /** Only populated when `status === "fail"`. */
  failure_evidence_json?: unknown;
  /** Only populated when `status === "fail"`. */
  error_message?: string;
}

/** Inputs for {@link runRegressionSuite}. */
export interface RunRegressionSuiteOptions {
  /** Suite to execute. */
  suite: RegressionSuite;
  /** Pre-loaded IR document keyed to the suite. */
  ir: IRDocument;
  /** Live UI Bridge registry (page DOM). */
  registry: RegistryLike;
  /**
   * Source IR doc identifier — used to scope persistence + recall the IR
   * later. Treated opaquely by the executor; passed through to
   * `save_regression_suite`.
   */
  irDocId: string;
  /**
   * Logical run identifier (caller-supplied; e.g. workflow execution id).
   * Passed through to `record_regression_run` and threaded into the
   * resulting `RegressionRunResult.runId`.
   */
  runId: string;
  /**
   * Recent commits since `suite.generatedAt`, file-overlap filtered (see
   * Decision #4 of Section 11). The caller does the filtering — typically
   * via `filterCommitsByIrOverlap`.
   */
  recentCommits: GitCommitRef[];
  /** Active recording session — required by `DriftContext`. */
  session: RecordingSession;
  /** Optional spec drift inputs. */
  specDrift?: DriftReport;
  /** Optional visual drift inputs. */
  visualDrift?: DriftReport;
  /** Optional fragility priors. */
  priors?: FragilityScore[];
  /**
   * Optional already-constructed `ScreenshotAssertionManager`. When omitted,
   * the executor builds one wired to a `TauriBaselineStore`. Tests inject a
   * stub via this option to avoid invoking Tauri.
   */
  screenshotManager?: ScreenshotAssertionManager;
  /**
   * Optional `Date.now()` override used purely for stamping `started_at`
   * / `completed_at`. Tests fix this to keep snapshots stable.
   */
  now?: () => number;
}

/** Output of {@link runRegressionSuite}. */
export interface RunRegressionSuiteResult {
  /** UUID returned by `save_regression_suite`. */
  suiteDbId: string;
  /** UUID returned by `record_regression_run`. */
  runDbId: string;
  /** Aggregated pure-data run result fed into `diagnose`. */
  runResult: RegressionRunResult;
  /** `null` when the run had zero failures (no diagnosis needed). */
  diagnosis: SelfDiagnosis | null;
}

// ---------------------------------------------------------------------------
// Internal — assertionId derivation
// ---------------------------------------------------------------------------

/**
 * Derive a stable assertion id matching the rule used by
 * `state/self-diagnosis.ts:assertionIdOf` (overlay assertions reuse their
 * own `assertionId`; the others key off `(caseId, kind, secondary)`).
 */
function assertionIdOf(caseId: string, a: RegressionAssertion): string {
  switch (a.kind) {
    case "state-active":
      return `${caseId}#state-active#${a.phase}#${a.stateId}`;
    case "action-target-resolves":
      return `${caseId}#action-target-resolves#${a.actionIndex}`;
    case "visual-gate":
      return `${caseId}#visual-gate#${a.stateId}`;
    case "overlay":
      return a.assertionId;
  }
}

// ---------------------------------------------------------------------------
// Internal — assertion dispatch
// ---------------------------------------------------------------------------

interface AssertionOutcome {
  status: AssertionExecutionStatus;
  /** When `status === "fail"`, the message that lands on the failure row. */
  message?: string;
  /** When `status === "fail"`, the observed value threaded into `RegressionFailure.observed`. */
  observed?: unknown;
  /** When `status === "fail"`, a coarse failure-kind tag for diagnostics. */
  failureKind?: string;
}

/**
 * State-active dispatch: every required element must be findable
 * unambiguously. The IR's `requiredElements` array supplies the queries;
 * the assertion's `requiredElementIds` indexes into it.
 *
 * "Unambiguous" here means `findFirst` returns a non-null match. We do NOT
 * gate on `ambiguities.length === 0` — the suite is a spec, not a uniqueness
 * test; ambiguity reporting belongs to the discovery layer.
 */
function evaluateStateActive(
  assertion: StateActiveAssertion,
  ir: IRDocument,
  registry: RegistryLike,
): AssertionOutcome {
  const state = ir.states.find((s) => s.id === assertion.stateId);
  if (!state) {
    return {
      status: "fail",
      failureKind: "state-not-in-ir",
      message: `State "${assertion.stateId}" not found in IR`,
      observed: null,
    };
  }

  const elements = registry.getAllElements();
  const missing: number[] = [];
  for (const idx of assertion.requiredElementIds) {
    const criteria = state.requiredElements[idx];
    if (!criteria) {
      // Defensive: a stale suite may carry indices the IR no longer has.
      missing.push(idx);
      continue;
    }
    const result = findFirst(elements, criteria);
    if (!result.match) missing.push(idx);
  }

  if (missing.length === 0) {
    return { status: "pass" };
  }

  return {
    status: "fail",
    failureKind: "required-element-missing",
    message: `State "${assertion.stateId}" missing required elements at indices [${missing.join(", ")}]`,
    observed: { missingRequiredElementIndices: missing },
  };
}

/**
 * Action-target dispatch: the action's target must resolve to exactly one
 * element. Multiple matches → fail (the runtime would have to disambiguate
 * before clicking). Zero matches → fail.
 */
function evaluateActionTargetResolves(
  assertion: ActionTargetResolvesAssertion,
  registry: RegistryLike,
): AssertionOutcome {
  const elements = registry.getAllElements();
  const results = executeQuery(elements, assertion.targetCriteria);
  if (results.length === 1) return { status: "pass" };
  if (results.length === 0) {
    return {
      status: "fail",
      failureKind: "action-target-unresolvable",
      message: `Action target for transition "${assertion.transitionId}" action ${assertion.actionIndex} resolved to 0 elements`,
      observed: { matchCount: 0 },
    };
  }
  return {
    status: "fail",
    failureKind: "action-target-ambiguous",
    message: `Action target for transition "${assertion.transitionId}" action ${assertion.actionIndex} resolved to ${results.length} elements`,
    observed: {
      matchCount: results.length,
      candidateIds: results.map((r) => r.id),
    },
  };
}

/**
 * Visual-gate dispatch: load the baseline by `baselineKey` and assert the
 * captured element matches. The element is resolved against the IR's first
 * required element of the named state — the suite carries no element id
 * by itself (visual gates are scoped to a state, not an element).
 *
 * If no required element can be resolved, the gate is skipped (status =
 * "skip") rather than failed: visual gates depend on the prior state-active
 * assertion already passing; failing the gate when the state itself is
 * broken would noise up the diagnosis without adding signal.
 */
async function evaluateVisualGate(
  assertion: VisualGateAssertion,
  ir: IRDocument,
  registry: RegistryLike,
  manager: ScreenshotAssertionManager,
): Promise<AssertionOutcome> {
  const state = ir.states.find((s) => s.id === assertion.stateId);
  if (!state || state.requiredElements.length === 0) {
    return {
      status: "skip",
    };
  }
  const elements = registry.getAllElements();
  const found = findFirst(elements, state.requiredElements[0]!);
  if (!found.match) {
    return {
      status: "skip",
    };
  }
  // Resolve the actual HTMLElement from the registry to feed the manager.
  const liveEl = elements.find((e) => e.id === found.match!.id);
  if (!liveEl) {
    return { status: "skip" };
  }

  try {
    const result = await manager.assertMatchesBaseline(liveEl.id, liveEl.element, {
      baselineKey: assertion.baselineKey,
    });
    if (result.pass) return { status: "pass" };
    return {
      status: "fail",
      failureKind: "visual-gate-mismatch",
      message:
        result.error ??
        `Visual gate failed: ${result.diffPercentage.toFixed(2)}% diff against baseline "${assertion.baselineKey}"`,
      observed: result,
    };
  } catch (err) {
    return {
      status: "fail",
      failureKind: "visual-gate-error",
      message: `Visual gate threw: ${err instanceof Error ? err.message : String(err)}`,
      observed: { error: String(err) },
    };
  }
}

/**
 * Overlay dispatch: each of the three built-in overlay ids has its own
 * runtime semantics. The overlay payload is opaque to the generator but
 * structurally known here.
 *
 * For now the executor implements a structural smoke check per overlay:
 *   - `visibility`  → require the element resolves and is `visible`.
 *   - `token`       → require the element resolves; full design-token
 *                     evaluation needs a registry the executor doesn't carry,
 *                     so this is a "resolves" check until the registry is
 *                     threaded through.
 *   - `cross-check` → require the action's target resolves; full OCR
 *                     cross-check needs an OCR provider supplied at execute
 *                     time and is wired in by the consumer when needed.
 *
 * Unknown overlay ids are skipped — the suite remains forward-compatible
 * with overlays this executor doesn't recognize yet.
 */
function evaluateOverlay(
  assertion: OverlayAssertion,
  ir: IRDocument,
  registry: RegistryLike,
): AssertionOutcome {
  const payload = assertion.payload;
  switch (assertion.overlayId) {
    case "visibility":
    case "token": {
      const stateId = typeof payload.stateId === "string" ? payload.stateId : null;
      const idx =
        typeof payload.requiredElementIndex === "number" ? payload.requiredElementIndex : null;
      if (stateId === null || idx === null) {
        return {
          status: "fail",
          failureKind: "overlay-payload-malformed",
          message: `Overlay "${assertion.overlayId}" payload missing stateId/requiredElementIndex`,
          observed: payload,
        };
      }
      const state = ir.states.find((s) => s.id === stateId);
      const criteria = state?.requiredElements[idx];
      if (!criteria) {
        return {
          status: "fail",
          failureKind: "overlay-criteria-not-in-ir",
          message: `Overlay "${assertion.overlayId}" references state ${stateId}#${idx} not in IR`,
          observed: { stateId, idx },
        };
      }
      const elements = registry.getAllElements();
      const result = findFirst(elements, criteria);
      if (!result.match) {
        return {
          status: "fail",
          failureKind: `overlay-${assertion.overlayId}-element-not-found`,
          message: `Overlay "${assertion.overlayId}" target ${stateId}#${idx} not in registry`,
          observed: { stateId, idx },
        };
      }
      if (assertion.overlayId === "visibility") {
        const live = elements.find((e) => e.id === result.match!.id);
        const visible = live?.getState().visible !== false;
        if (!visible) {
          return {
            status: "fail",
            failureKind: "overlay-visibility-not-visible",
            message: `Overlay "visibility" target ${stateId}#${idx} resolved but is not visible`,
            observed: { stateId, idx, elementId: result.match.id },
          };
        }
      }
      return { status: "pass" };
    }
    case "cross-check": {
      const transitionId = typeof payload.transitionId === "string" ? payload.transitionId : null;
      const actionIndex = typeof payload.actionIndex === "number" ? payload.actionIndex : null;
      if (transitionId === null || actionIndex === null) {
        return {
          status: "fail",
          failureKind: "overlay-payload-malformed",
          message: `Overlay "cross-check" payload missing transitionId/actionIndex`,
          observed: payload,
        };
      }
      const transition = ir.transitions.find((t) => t.id === transitionId);
      const action = transition?.actions[actionIndex];
      if (!action) {
        return {
          status: "fail",
          failureKind: "overlay-cross-check-action-not-in-ir",
          message: `Overlay "cross-check" references transition ${transitionId}#${actionIndex} not in IR`,
          observed: { transitionId, actionIndex },
        };
      }
      const elements = registry.getAllElements();
      const result = findFirst(elements, action.target);
      if (!result.match) {
        return {
          status: "fail",
          failureKind: "overlay-cross-check-element-not-found",
          message: `Overlay "cross-check" target ${transitionId}#${actionIndex} not in registry`,
          observed: { transitionId, actionIndex },
        };
      }
      return { status: "pass" };
    }
    default:
      // Forward-compatible: unknown overlay ids skip cleanly.
      return { status: "skip" };
  }
}

// ---------------------------------------------------------------------------
// Public API — runner
// ---------------------------------------------------------------------------

/**
 * Execute a regression suite against the live registry. Persists the suite,
 * the run summary, the per-assertion exercise rows, and (when the run
 * failed) a self-diagnosis memo. Returns the persisted UUIDs alongside the
 * pure run result + diagnosis so the caller can show or re-query.
 */
export async function runRegressionSuite(
  options: RunRegressionSuiteOptions,
): Promise<RunRegressionSuiteResult> {
  const {
    suite,
    ir,
    registry,
    irDocId,
    runId,
    recentCommits,
    session,
    specDrift,
    visualDrift,
    priors,
    screenshotManager,
    now,
  } = options;

  const clock = now ?? Date.now;
  const startedAtMs = clock();
  const startedAtIso = new Date(startedAtMs).toISOString();

  // Default the screenshot manager to the runner's TauriBaselineStore-backed
  // implementation. Tests inject a stub via `options.screenshotManager`.
  const manager = screenshotManager ?? new ScreenshotAssertionManager(new TauriBaselineStore());

  // Step 1 — persist the suite. Append-only by design.
  const suiteDbId = await invoke<string>("save_regression_suite", {
    suiteJson: JSON.parse(serializeSuite(suite)),
    irDocId,
  });

  // Step 2 — walk every case + every assertion, dispatching on `kind`.
  const failures: RegressionFailure[] = [];
  const executionsBuffer: AssertionExecutionRow[] = [];
  let passed = 0;
  let failed = 0;

  for (const c of suite.cases as RegressionCase[]) {
    for (const assertion of c.assertions) {
      const aId = assertionIdOf(c.id, assertion);
      const aStartedMs = clock();
      const aStartedIso = new Date(aStartedMs).toISOString();

      let outcome: AssertionOutcome;
      try {
        switch (assertion.kind) {
          case "state-active":
            outcome = evaluateStateActive(assertion, ir, registry);
            break;
          case "action-target-resolves":
            outcome = evaluateActionTargetResolves(assertion, registry);
            break;
          case "visual-gate":
            outcome = await evaluateVisualGate(assertion, ir, registry, manager);
            break;
          case "overlay":
            outcome = evaluateOverlay(assertion, ir, registry);
            break;
        }
      } catch (err) {
        outcome = {
          status: "fail",
          failureKind: "executor-threw",
          message: `Executor threw: ${err instanceof Error ? err.message : String(err)}`,
          observed: { error: String(err) },
        };
      }

      const durationMs = clock() - aStartedMs;
      const row: AssertionExecutionRow = {
        id: cryptoRandomUUID(),
        run_id: "", // patched in after `record_regression_run` returns the run UUID
        case_id: c.id,
        assertion_id: aId,
        assertion_kind: assertion.kind,
        status: outcome.status,
        started_at: aStartedIso,
        duration_ms: durationMs,
      };
      if (outcome.status === "fail") {
        row.failure_kind = outcome.failureKind;
        row.failure_evidence_json = outcome.observed ?? null;
        row.error_message = outcome.message;
        failed += 1;
        failures.push({
          caseId: c.id,
          assertionId: aId,
          assertion,
          message: outcome.message ?? "(no message)",
          observed: outcome.observed,
        });
      } else if (outcome.status === "pass") {
        passed += 1;
      }
      // `skip` rows are recorded but not counted toward pass/failed totals.
      executionsBuffer.push(row);
    }
  }

  const completedAtMs = clock();
  const completedAtIso = new Date(completedAtMs).toISOString();

  const runResult: RegressionRunResult = {
    suiteId: suite.id,
    runId,
    passed,
    failed,
    failures,
  };

  // Step 3 — record the run, getting back the persisted UUID.
  const runDbId = await invoke<string>("record_regression_run", {
    suiteId: suiteDbId,
    runId,
    passed,
    failed,
    startedAt: startedAtIso,
    completedAt: completedAtIso,
    runResultJson: runResult,
  });

  // Step 4 — patch run id onto each row, batch-record.
  for (const row of executionsBuffer) row.run_id = runDbId;
  await invoke<number>("record_assertion_executions_batch", {
    executions: executionsBuffer,
  });

  // Step 5 — diagnose if there were failures, persist via the PG MemorySink.
  let diagnosis: SelfDiagnosis | null = null;
  if (failed > 0) {
    const driftContext: DriftContext = {
      session,
      commits: recentCommits,
      ir,
    };
    if (priors !== undefined) driftContext.priors = priors;
    if (specDrift !== undefined) driftContext.specDrift = specDrift;
    if (visualDrift !== undefined) driftContext.visualDrift = visualDrift;

    diagnosis = diagnose(runResult, driftContext);
    const sink = createPgMemorySink(runDbId);
    sink.record(diagnosis);
    // Wait for the persistence write so the caller knows the diagnosis row
    // has landed before returning. `sink.lastWrite` is non-null here because
    // we just called `record`.
    if (sink.lastWrite !== null) await sink.lastWrite;
  }

  return { suiteDbId, runDbId, runResult, diagnosis };
}

// ---------------------------------------------------------------------------
// Internal — UUID helper
// ---------------------------------------------------------------------------

/**
 * `crypto.randomUUID` shim. Avoids hard-coding `crypto` on the global so
 * tests with a `crypto`-less environment can still build the executor;
 * vitest + happy-dom + node both expose `crypto.randomUUID`.
 */
function cryptoRandomUUID(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto?.randomUUID) return g.crypto.randomUUID();
  // Fallback — not cryptographically strong, only used when the host runtime
  // doesn't expose `crypto.randomUUID`. The id only needs to be unique within
  // a batch of executions, not globally cryptographic.
  return `exec-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
