/**
 * RegressionRunPage
 *
 * Integration smoke surface for the Section 11 regression executor (Phase A3).
 * Lets a developer run a hardcoded `RegressionSuite` against the active page's
 * UI Bridge registry, then surfaces the resulting `RegressionRunResult` and
 * (when the run failed) the `SelfDiagnosis.correlationSummary` from the pure
 * `diagnose` function.
 *
 * Not a final UX. The expectation per the Section 11 plan is that a richer
 * panel (Phase D2 / C2) will replace this. We expose the page component as a
 * named export so future routing work can wire it into the runner shell
 * without further executor changes.
 */

import { useCallback, useState } from "react";
import { Play, Loader2, AlertTriangle, CheckCircle2 } from "lucide-react";

import { generateRegressionSuite, type RegressionSuite } from "@qontinui/ui-bridge-auto/regression";
import type { RegistryLike } from "@qontinui/ui-bridge-auto/runtime";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";

import { runRegressionSuite, type RunRegressionSuiteResult } from "@/lib/regression-executor";

// ---------------------------------------------------------------------------
// Demo IR — minimal hardcoded fixture for the smoke surface
// ---------------------------------------------------------------------------

/**
 * Tiny hardcoded IR used by the page's "Run Demo Suite" button. The point is
 * to prove the executor wires end-to-end (suite → walk → persist → diagnose);
 * the IR's content is intentionally trivial.
 */
const DEMO_IR: IRDocument = {
  version: "1.0",
  id: "regression-demo-page",
  name: "Regression Demo",
  states: [
    {
      id: "state-root",
      name: "Root",
      requiredElements: [{ tagName: "body" }],
    },
    {
      id: "state-with-button",
      name: "With Button",
      requiredElements: [{ role: "button" }],
    },
  ],
  transitions: [
    {
      id: "click-any-button",
      name: "Click any button",
      fromStates: ["state-root"],
      activateStates: ["state-with-button"],
      actions: [
        {
          type: "click",
          target: { role: "button" },
        },
      ],
    },
  ],
  initialState: "state-root",
};

// ---------------------------------------------------------------------------
// Registry adapter — pulls live elements off the global UI Bridge registry
// ---------------------------------------------------------------------------

interface UIBridgeRegistryLike {
  getAllElements(): unknown[];
  on(type: string, listener: (...args: unknown[]) => void): () => void;
}

/** Read the runner's global UI Bridge registry, if available. */
function getLiveRegistry(): RegistryLike | null {
  // The runner exposes its UI Bridge state under `window.__UI_BRIDGE__`.
  // We don't take a hard import dependency on the SDK here so the page
  // remains loadable even if the bridge isn't wired up yet.
  type WindowLike = { __UI_BRIDGE__?: { registry?: UIBridgeRegistryLike } };
  const w = window as unknown as WindowLike;
  const registry = w.__UI_BRIDGE__?.registry;
  if (!registry) return null;
  return registry as unknown as RegistryLike;
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export function RegressionRunPage(): React.JSX.Element {
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RunRegressionSuiteResult | null>(null);

  const handleRunDemo = useCallback(async () => {
    setRunning(true);
    setError(null);
    setResult(null);
    try {
      const registry = getLiveRegistry();
      if (!registry) {
        throw new Error("UI Bridge registry not available on window.__UI_BRIDGE__.registry");
      }
      const suite: RegressionSuite = generateRegressionSuite(DEMO_IR);
      const out = await runRegressionSuite({
        suite,
        ir: DEMO_IR,
        registry,
        irDocId: DEMO_IR.id,
        runId: `demo-${Date.now()}`,
        recentCommits: [],
        // Empty placeholder session — the demo doesn't need a real recording.
        session: { id: "demo-session", startedAt: Date.now(), events: [] },
      });
      setResult(out);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }, []);

  return (
    <div className="flex flex-col gap-4 p-6">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Regression Run</h1>
          <p className="text-sm text-muted-foreground">
            Section 11 smoke surface. Generates a deterministic suite from the demo IR, walks each
            assertion against the live UI Bridge registry, and persists the run + diagnosis.
          </p>
        </div>
        <button
          onClick={handleRunDemo}
          disabled={running}
          className="inline-flex items-center gap-2 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
        >
          {running ? <Loader2 className="size-4 animate-spin" /> : <Play className="size-4" />}
          Run Demo Suite
        </button>
      </header>

      {error !== null && (
        <div className="flex items-start gap-2 rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm">
          <AlertTriangle className="size-4 mt-0.5 text-destructive" />
          <div>
            <div className="font-medium text-destructive">Run failed</div>
            <pre className="mt-1 whitespace-pre-wrap text-xs">{error}</pre>
          </div>
        </div>
      )}

      {result !== null && (
        <section className="flex flex-col gap-3">
          <div className="rounded-md border bg-card p-3">
            <h2 className="mb-2 flex items-center gap-2 text-sm font-medium">
              {result.runResult.failed === 0 ? (
                <CheckCircle2 className="size-4 text-green-600" />
              ) : (
                <AlertTriangle className="size-4 text-amber-600" />
              )}
              Last run
            </h2>
            <dl className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 text-xs">
              <dt className="text-muted-foreground">Suite DB id</dt>
              <dd className="font-mono">{result.suiteDbId}</dd>
              <dt className="text-muted-foreground">Run DB id</dt>
              <dd className="font-mono">{result.runDbId}</dd>
              <dt className="text-muted-foreground">Run id</dt>
              <dd className="font-mono">{result.runResult.runId}</dd>
              <dt className="text-muted-foreground">Passed</dt>
              <dd>{result.runResult.passed}</dd>
              <dt className="text-muted-foreground">Failed</dt>
              <dd>{result.runResult.failed}</dd>
            </dl>
          </div>

          {result.diagnosis && (
            <div className="rounded-md border bg-card p-3">
              <h2 className="mb-2 text-sm font-medium">Self-diagnosis</h2>
              <p className="text-xs">{result.diagnosis.correlationSummary}</p>
              {result.diagnosis.candidateCauses.length > 0 && (
                <ol className="mt-2 list-decimal pl-4 text-xs">
                  {result.diagnosis.candidateCauses.map((cause, i) => (
                    <li key={i} className="mt-1">
                      <span className="font-mono text-muted-foreground">
                        ({cause.confidence.toFixed(2)})
                      </span>{" "}
                      {cause.hypothesis}
                    </li>
                  ))}
                </ol>
              )}
            </div>
          )}

          {result.runResult.failures.length > 0 && (
            <div className="rounded-md border bg-card p-3">
              <h2 className="mb-2 text-sm font-medium">Failures</h2>
              <ul className="text-xs">
                {result.runResult.failures.map((f) => (
                  <li
                    key={f.assertionId}
                    className="mb-2 border-b border-border/50 pb-2 last:border-b-0 last:pb-0"
                  >
                    <div className="font-mono text-muted-foreground">
                      {f.caseId} / {f.assertionId}
                    </div>
                    <div>{f.message}</div>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </section>
      )}
    </div>
  );
}

export default RegressionRunPage;
