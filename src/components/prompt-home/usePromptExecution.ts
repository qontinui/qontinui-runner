import { useState, useCallback, useRef } from "react";
import { useUIBridge, type ControlActionRequest } from "@qontinui/ui-bridge";
import { tracedFetch } from "@/lib/traced-fetch";
import { getApiBase } from "@/lib/runner-api";
import { isValidTabId, type MainTabId } from "@/components/app/tab-types";
import { buildPageCatalog } from "./pageCatalog";

export interface PlanStep {
  type: "navigate" | "action" | "component-action";
  target?: string;
  instruction?: string;
  /** For `component-action` steps: the registered `useUIComponent` id. */
  componentId?: string;
  /** For `component-action` steps: the action id on that component. */
  actionId?: string;
  /** For `component-action` steps: optional params passed to the handler. */
  params?: Record<string, unknown>;
  explanation: string;
}

export interface PromptPlan {
  summary: string;
  steps: PlanStep[];
}

export type ExecutionPhase = "idle" | "planning" | "executing" | "done" | "error";

export type ErrorKind = "auth-required" | "generic" | null;

export interface StepProgress {
  currentIndex: number;
  total: number;
  currentStep: PlanStep | null;
}

/**
 * Thrown when the planner endpoint returns 401/403 — the runner's JWT auth
 * has expired. The catch block uses `instanceof` to distinguish this from
 * generic planning failures so the UI can surface a sign-in affordance.
 */
class AuthRequiredError extends Error {
  readonly kind = "auth-required" as const;
  constructor(message = "Sign-in required to run prompts.") {
    super(message);
    this.name = "AuthRequiredError";
  }
}

interface UsePromptExecutionReturn {
  phase: ExecutionPhase;
  plan: PromptPlan | null;
  progress: StepProgress;
  error: string | null;
  errorKind: ErrorKind;
  lastPrompt: string | null;
  submit: (prompt: string, explain: boolean) => Promise<void>;
  retry: () => void;
  reset: () => void;
}

export function usePromptExecution(): UsePromptExecutionReturn {
  const bridge = useUIBridge();
  const [phase, setPhase] = useState<ExecutionPhase>("idle");
  const [plan, setPlan] = useState<PromptPlan | null>(null);
  const [progress, setProgress] = useState<StepProgress>({
    currentIndex: -1,
    total: 0,
    currentStep: null,
  });
  const [error, setError] = useState<string | null>(null);
  const [errorKind, setErrorKind] = useState<ErrorKind>(null);
  const [lastPrompt, setLastPrompt] = useState<string | null>(null);
  const lastExplainRef = useRef(false);
  const abortRef = useRef(false);
  const runningRef = useRef(false);

  const reset = useCallback(() => {
    abortRef.current = true;
    setPhase("idle");
    setPlan(null);
    setProgress({ currentIndex: -1, total: 0, currentStep: null });
    setError(null);
    setErrorKind(null);
  }, []);

  const submit = useCallback(
    async (prompt: string, explain: boolean) => {
      if (runningRef.current) return;
      runningRef.current = true;
      abortRef.current = false;
      setLastPrompt(prompt);
      lastExplainRef.current = explain;
      setError(null);
      setErrorKind(null);
      setPhase("planning");

      try {
        // Step 1: Get the action plan from backend.
        // Include a catalog of real element labels from loaded specs so the
        // planner names actual buttons instead of hallucinating generic ones.
        const pageCatalog = buildPageCatalog();
        const resp = await tracedFetch(`${getApiBase()}/prompt-home/plan`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ prompt, explain, pageCatalog }),
        });

        if (!resp.ok) {
          // Auth failures (typically expired JWT) need a distinct UI affordance
          // so the user can re-authenticate. Tag them before falling into the
          // generic error path.
          if (resp.status === 401 || resp.status === 403) {
            // Drain the body so the connection can be released; ignore content.
            try {
              await resp.text();
            } catch {
              /* ignore */
            }
            throw new AuthRequiredError();
          }
          let errorMsg = `Planning failed (${resp.status})`;
          try {
            const json = await resp.json();
            errorMsg = json.error || errorMsg;
          } catch {
            /* non-JSON error body */
          }
          throw new Error(errorMsg);
        }

        const json = await resp.json();
        if (!json.success) {
          throw new Error(json.error || "Planning failed");
        }

        const planData = json.data as PromptPlan;
        setPlan(planData);

        if (planData.steps.length === 0) {
          setPhase("done");
          return;
        }

        setProgress({
          currentIndex: 0,
          total: planData.steps.length,
          currentStep: planData.steps[0],
        });
        setPhase("executing");

        // Step 2: Execute each step
        const sm = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
          | { stateMachine?: StateMachineAPI }
          | undefined;

        for (let i = 0; i < planData.steps.length; i++) {
          if (abortRef.current) return;

          const step = planData.steps[i];
          setProgress({ currentIndex: i, total: planData.steps.length, currentStep: step });

          if (step.type === "navigate" && step.target) {
            const navResult = await navigateToTarget(step.target, sm);
            if (!navResult.ok) {
              throw new Error(`Step ${i + 1} failed: ${navResult.error}`);
            }
            // Wait for page to settle
            await new Promise((r) => setTimeout(r, 500));
          } else if (step.type === "component-action") {
            // Component actions are registered programmatic affordances
            // (useUIComponent) — NOT clickable elements. The canonical example
            // is `terminal-launch-menu` / `create-best-account`, the REAL
            // "open an AI session" spawn affordance (there is no "Claude Code"
            // button to click). Dispatch straight through the bridge, the same
            // path the WS `execute_component_action` handler uses.
            if (!step.componentId || !step.actionId) {
              throw new Error(
                `Step ${i + 1} failed: component-action step missing componentId or actionId (componentId="${step.componentId ?? ""}", actionId="${step.actionId ?? ""}")`,
              );
            }
            // D-RACE1 — bridge a post-navigate async-mount race. A preceding
            // `navigate` step switches the active tab, but the target page's
            // `useUIComponent` registration (e.g. `terminal-launch-menu`) runs
            // in a mount effect that may not have fired by the time this step
            // dispatches — `executeComponentAction` then 404s with
            // "Component … not found. Components are only available when their
            // page is active." The fixed 500ms post-navigate settle above is
            // not enough on a clean run (first navigation, heavy page tree).
            // Poll the live registry for the component to appear before
            // invoking; only surface the not-found error after the bounded
            // wait elapses. This is generic — it covers ANY component reached
            // immediately after a navigate, not just the terminal launcher.
            await waitForComponentRegistered(bridge, step.componentId, i + 1);
            const compResult = await bridge.executeComponentAction(step.componentId, {
              action: step.actionId,
              params: step.params,
            });
            if (!compResult.success) {
              throw new Error(
                `Step ${i + 1} failed: ${compResult.error ?? "component action failed"} — could not invoke ${step.componentId}/${step.actionId}`,
              );
            }
            // Give the spawn handler time to mount the new pane (a Claude Code
            // session takes a beat to render its banner).
            await new Promise((r) => setTimeout(r, 500));
          } else if (step.type === "action" && step.instruction) {
            // P20 — direct dispatch for "<verb> element <id>" forms.
            //
            // The NL path (NLActionExecutor) takes a discover() snapshot,
            // updates a SearchEngine, then resolves by id and forwards to
            // bridge.executeAction. When the id is already known, that detour
            // is wasted work AND introduces a subtle staleness window: the
            // discovered RegisteredElement DOM ref can be a node React has
            // since replaced, and clicks on detached nodes succeed silently
            // (no error, no toggle). The disclosure-toggle regression in P20
            // was reproduced from exactly that path. The HTTP handler that
            // backs the direct SDK call doesn't take that detour — it dispatches
            // straight against the live registry — and worked correctly in
            // isolation. Bypassing the executor for known-id instructions
            // converges both paths onto the same dispatch and removes the
            // staleness window.
            //
            // The free-text NL path (fuzzy search) remains as the fallback
            // for instructions that don't match the deterministic regex set.
            const directIdMatch = parseDirectIdInstruction(step.instruction);
            const targetId = directIdMatch?.targetId ?? null;
            const preState = targetId ? bridge.getElementState(targetId) : undefined;

            if (directIdMatch) {
              // Auto-scroll registered targets into view before acting.
              // The SDK's wait-for-conditions rejects elements whose rect
              // doesn't intersect the viewport, and PageSelectionPanel's
              // checkboxes / Generate button live ~1700px down the page —
              // far below a typical 800px viewport. Without this, the
              // executor times out at "Timeout waiting for conditions
              // after 5000ms" on every off-screen registered target.
              // Best-effort: ignore failures so a non-existent target still
              // surfaces via the action call's own error.
              try {
                await bridge.executeAction(directIdMatch.targetId, {
                  action: "scrollIntoView",
                });
              } catch {
                /* fall through — primary action will report the real error */
              }
              // Direct path — same code path as Path A (HTTP handler).
              const directResult = await bridge.executeAction(
                directIdMatch.targetId,
                directIdMatch.actionRequest,
              );
              if (!directResult.success) {
                throw new Error(
                  `Step ${i + 1} failed: ${directResult.error ?? "action failed"} — could not ${step.instruction}`,
                );
              }
            } else {
              // Free-text fallback — full NL pipeline with fuzzy search.
              const { NLActionExecutor } = await import("@qontinui/ui-bridge/ai");
              const discovered = await bridge.discover({ includeHidden: false });
              const executor = new NLActionExecutor();
              executor.updateElements(discovered.elements);
              executor.setActionExecutor(bridge as never);

              const result = await executor.execute({ instruction: step.instruction });
              if (!result.success) {
                // D-NL2 — no-match observability. NLActionExecutor returns a
                // bestMatch-null failure with an opaque message that hides WHY
                // it gave up: the planner named a label/element that is not in
                // the discovered set (planner-target drift). Surface the
                // attempted instruction + the candidate labels the executor
                // actually saw so the drift is diagnosable from the step error
                // alone, without re-running with a debugger.
                const candidates = summarizeCandidateLabels(discovered.elements);
                throw new Error(
                  `Step ${i + 1} failed: ${result.error ?? result.errorCode ?? "action failed"} — could not ${step.instruction}. ` +
                    `No element matched (chosen: none). ${candidates}`,
                );
              }
            }

            // Wait for action to settle before observing post-state. The
            // 300ms here is the same flat settle the executor used before
            // P18A; the new effect checks run AFTER this so they see the
            // settled state, not the in-flight transition.
            await new Promise((r) => setTimeout(r, 300));

            // P18A — reject silent toggle no-ops. If the registered element
            // exposes `checked` (checkbox) or `ariaExpanded` (disclosure),
            // it MUST flip after a click/check action. Buttons that expose
            // neither field fall through to P18B (pipeline-phase poll) when
            // applicable, or just rely on the success flag otherwise.
            if (targetId && preState) {
              const postState = bridge.getElementState(targetId);
              const checkedToggled =
                preState.checked !== undefined &&
                postState?.checked !== undefined &&
                preState.checked !== postState.checked;
              const expandedToggled =
                preState.ariaExpanded !== undefined &&
                postState?.ariaExpanded !== undefined &&
                preState.ariaExpanded !== postState.ariaExpanded;
              const hadObservableField =
                preState.checked !== undefined || preState.ariaExpanded !== undefined;
              if (hadObservableField && !checkedToggled && !expandedToggled) {
                throw new Error(
                  `Step ${i + 1} failed: action had no observable effect on "${targetId}" — element may be disabled, gated, or off-screen (instruction: "${step.instruction}")`,
                );
              }
            }

            // P18B/P21 — post-action effect verification for the integration
            // flow's two phase-changing buttons. These click the Advanced
            // SourceIntegrationPanel's Analyze button or the PageSelectionPanel's
            // Generate button — neither updates ProjectCoordinator's
            // data-pipeline-phase (that attribute only tracks the OUTER
            // one-click "Integrate this Project" flow). Instead, wait for the
            // next-step's target element to appear in the registered DOM:
            // Analyze success → PageSelectionPanel mounts → ui-bridge-generate-button
            //                   registers, proving Analyze + Discover ran.
            // Generate success → preview panel renders → Apply/Discard buttons
            //                   appear.
            const postActionWait = POST_ACTION_WAITS.get(targetId ?? "");
            if (postActionWait) {
              await awaitElementRegistered(i + 1, targetId!, postActionWait);
            }
          }
        }

        if (abortRef.current) return;

        setProgress({
          currentIndex: planData.steps.length,
          total: planData.steps.length,
          currentStep: null,
        });
        setPhase("done");
      } catch (err) {
        if (abortRef.current) return;
        const msg = err instanceof Error ? err.message : "An error occurred";
        setError(msg);
        setErrorKind(err instanceof AuthRequiredError ? "auth-required" : "generic");
        setPhase("error");
      } finally {
        runningRef.current = false;
      }
    },
    [bridge],
  );

  const retry = useCallback(() => {
    if (lastPrompt && !runningRef.current) {
      void submit(lastPrompt, lastExplainRef.current);
    }
  }, [lastPrompt, submit]);

  return { phase, plan, progress, error, errorKind, lastPrompt, submit, retry, reset };
}

/**
 * P18B trigger set — element ids whose click is known to drive the
 * ProjectCoordinator's `data-pipeline-phase` attribute. Adding new entries
 * here opts those clicks into the 30s phase-transition gate.
 */
const _PIPELINE_TRIGGER_IDS = new Set<string>([
  "ui-bridge-analyze-button",
  "ui-bridge-generate-button",
]);

/**
 * Minimal shape of a discovered element we read for the D-NL2 no-match
 * diagnostic. Matches the relevant fields of the SDK's `DiscoveredElement`
 * without importing the full type (keeps this helper decoupled).
 */
interface DiscoveredLike {
  id?: string;
  label?: string;
  accessibleName?: string;
  type?: string;
}

/**
 * D-NL2 — build a human-readable list of the candidate element labels the NL
 * executor actually saw, for inclusion in a no-match step error. When the
 * planner names a label/element that doesn't exist (target drift), the raw
 * executor error is opaque ("bestMatch null"); listing what WAS available makes
 * the drift obvious. Caps the list so the error stays bounded.
 */
function summarizeCandidateLabels(elements: readonly DiscoveredLike[]): string {
  const MAX = 25;
  const labels = elements
    .map((e) => e.label ?? e.accessibleName ?? e.id ?? "")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  // De-dup while preserving order.
  const seen = new Set<string>();
  const unique: string[] = [];
  for (const l of labels) {
    if (!seen.has(l)) {
      seen.add(l);
      unique.push(l);
    }
  }
  if (unique.length === 0) {
    return "No interactive elements were discovered on the current page (is the right page loaded?).";
  }
  const shown = unique.slice(0, MAX);
  const overflow = unique.length > MAX ? ` (+${unique.length - MAX} more)` : "";
  return `Candidate labels on this page (${unique.length}): ${shown.map((l) => `"${l}"`).join(", ")}${overflow}.`;
}

/** The object returned by `useUIBridge` — typed structurally so this helper
 * stays decoupled from the SDK's exported type name. We only read
 * `getComponent`, the synchronous registry lookup that returns the component
 * record (or undefined when it isn't currently registered). */
type BridgeLike = Pick<ReturnType<typeof useUIBridge>, "getComponent">;

/**
 * D-RACE1 — bounded wait for a `useUIComponent` registration to appear in the
 * live registry after a navigate. `executeComponentAction` looks the registry
 * up exactly once and 404s if the target page's mount effect hasn't run yet;
 * on a clean run (first navigation to a heavy page like the terminal
 * workspace) the registration lands a few hundred ms after the tab becomes
 * active — well after the fixed 500ms post-navigate settle. Poll
 * `bridge.getComponent(id)` on a short interval until it resolves, then return
 * so the caller can dispatch. If it never registers within `timeoutMs`, throw
 * a clear, actionable step error (the caller would otherwise hit the SDK's
 * opaque "Component not found" with no indication a race was the cause).
 *
 * Generic by design: any component reached immediately after a navigate
 * benefits, not just `terminal-launch-menu`.
 */
async function waitForComponentRegistered(
  bridge: BridgeLike,
  componentId: string,
  stepNumber: number,
  timeoutMs = 8000,
  pollMs = 100,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  // Fast path — already registered (e.g. the page was already active from an
  // earlier navigation), skip the poll entirely.
  if (bridge.getComponent(componentId)) return;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, pollMs));
    if (bridge.getComponent(componentId)) return;
  }
  throw new Error(
    `Step ${stepNumber} failed: component "${componentId}" never registered within ` +
      `${Math.round(timeoutMs / 1000)} s after navigation — the page that owns it may not have ` +
      `mounted, or its useUIComponent registration is gated on a UI state (e.g. a menu being open) ` +
      `that headless automation can't reach.`,
  );
}

/**
 * Result of parsing a direct-id instruction. When non-null, the caller
 * dispatches `bridge.executeAction(targetId, actionRequest)` directly,
 * bypassing the NL search pipeline entirely.
 */
interface DirectIdInstruction {
  targetId: string;
  actionRequest: ControlActionRequest;
}

/**
 * Standard wait options applied to every direct-dispatch action. Mirrors
 * the defaults NLActionExecutor uses internally so the two paths share
 * identical pre-action gating semantics.
 */
const DIRECT_WAIT_OPTIONS = { visible: true, enabled: true, timeout: 5000 };

/**
 * Parse instructions of the form `<verb> element <id>` (and the typed
 * variant `type "<text>" in element <id>`) into a deterministic
 * dispatch payload.
 *
 * Returns null for free-text instructions that don't match the regex set —
 * those fall through to the NL fuzzy-search pipeline.
 *
 * Verb → standardAction map (matches NLActionExecutor's actionMap):
 *   click   → click
 *   check   → check
 *   uncheck → uncheck
 *   type    → type   (with `params: {text, clear: true}` mirroring the
 *                     "type means replace, not append" semantics the NL
 *                     path enforces; see nl-action-executor.ts:382-383)
 */
function parseDirectIdInstruction(instruction: string): DirectIdInstruction | null {
  const trimmed = instruction.trim();

  // click | check | uncheck — single id, no params
  const verbMatch = trimmed.match(/^(click|check|uncheck) element ([\w-]+)$/i);
  if (verbMatch) {
    const verb = verbMatch[1].toLowerCase() as "click" | "check" | "uncheck";
    return {
      targetId: verbMatch[2],
      actionRequest: { action: verb, waitOptions: DIRECT_WAIT_OPTIONS },
    };
  }

  // type "<text>" in element <id>  (single or double quotes)
  const typeMatch = trimmed.match(/^type ['"]([^'"]+)['"] in element ([\w-]+)$/i);
  if (typeMatch) {
    return {
      targetId: typeMatch[2],
      actionRequest: {
        action: "type",
        params: { text: typeMatch[1], clear: true },
        waitOptions: DIRECT_WAIT_OPTIONS,
      },
    };
  }

  return null;
}

/**
 * Poll the ProjectCoordinator's `data-pipeline-phase` attribute for a
 * transition out of "idle" after clicking a known phase-changing trigger.
 *
 * Resolves on the first non-idle, non-failed phase observed (e.g.
 * "analyzing", "integrating", "discovering", "generating", "generated",
 * "applied", "no-pages"). Throws if it stays at "idle" past `timeoutMs`,
 * or transitions to "failed" at any point.
 */
interface PostActionWait {
  /** Element id we expect to appear in the live DOM after the trigger. */
  expectId: string;
  /** Plain-English description for the timeout error. */
  description: string;
  timeoutMs?: number;
}

/**
 * Map from trigger element id to the post-action UI signal we expect.
 * Used by P18B/P21: after an action that fires a long-running handler,
 * wait for the handler's visible effect (next-step target element appearing)
 * rather than relying on a phase attribute that may not be wired up.
 */
const POST_ACTION_WAITS: Map<string, PostActionWait> = new Map([
  [
    "ui-bridge-analyze-button",
    {
      expectId: "ui-bridge-generate-button",
      description: "PageSelectionPanel did not mount (Analyze + Discover did not produce pages)",
      timeoutMs: 30000,
    },
  ],
  // Note: ui-bridge-generate-button intentionally has NO post-action wait.
  // Generate kicks off a 5-15 min AI session; the executor's job is to fire
  // the click, not block until completion. The BackgroundTaskPill (P19A)
  // handles the long-running visibility, and the user fire-and-forgets.
]);

/**
 * Wait for `expectId` to be present in the document's registered-element
 * registry (as `[data-ui-bridge-id="..."]`) within `timeoutMs`. MutationObserver
 * captures every DOM change without polling, so it doesn't miss fast appearances.
 *
 * Throws if the element doesn't appear within the timeout — that's the loud
 * "your trigger click had no observable effect" signal.
 */
async function awaitElementRegistered(
  stepNumber: number,
  triggerId: string,
  wait: PostActionWait,
): Promise<void> {
  const timeoutMs = wait.timeoutMs ?? 30000;
  const selector = `[data-ui-bridge-id="${wait.expectId}"]`;

  // Already present? Skip the observer entirely.
  if (document.querySelector(selector)) return;

  return new Promise<void>((resolve, reject) => {
    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    const observer = new MutationObserver(() => {
      if (document.querySelector(selector)) {
        observer.disconnect();
        if (timeoutHandle !== null) clearTimeout(timeoutHandle);
        resolve();
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });

    timeoutHandle = setTimeout(() => {
      observer.disconnect();
      reject(
        new Error(
          `Step ${stepNumber} failed: ${wait.description} — expected element "${wait.expectId}" did not appear within ${Math.round(timeoutMs / 1000)} s after clicking ${triggerId}`,
        ),
      );
    }, timeoutMs);
  });
}

// Kept for reference; no longer called by the executor. The data-pipeline-phase
// attribute lives on ProjectCoordinator and only tracks the one-click "Integrate
// this Project" flow — not the Advanced/SourceIntegrationPanel/PageSelectionPanel
// path the home-prompt planner actually drives.
async function _awaitPipelinePhaseTransition(
  stepNumber: number,
  triggerId: string,
  timeoutMs = 30000,
): Promise<void> {
  // MutationObserver-based — captures every transition, including ones that
  // complete faster than a poll interval. The previous polling implementation
  // fired a false-positive timeout on small projects whose analyze→discover
  // cycle settled in under 200ms (P21).
  //
  // Success conditions (any non-idle phase observed at any point):
  // - "analyzing", "integrating", "discovering", "generating" — transient
  //   phases proving the handler fired. Resolve as soon as we see one.
  // - "no-pages", "discovered", "generated", "applied" — terminal-success
  //   phases. Resolve immediately.
  //
  // Failure conditions:
  // - "failed" observed at any point — throw fast.
  // - Timeout with no non-idle phase ever observed — throw "handler may not
  //   have fired".
  //
  // We also check the initial value once at start to handle the case where
  // the phase is already non-idle before the observer wires up (rare but
  // possible if the trigger handler is synchronous).
  const SUCCESS_PHASES = new Set([
    "analyzing",
    "integrating",
    "discovering",
    "generating",
    "no-pages",
    "discovered",
    "generated",
    "applied",
  ]);

  return new Promise((resolve, reject) => {
    const root = document.querySelector<HTMLElement>("[data-pipeline-phase]");
    if (!root) {
      // No coordinator on the page — nothing to observe. Don't block the
      // executor; treat as a soft-pass since P18A already validated the
      // click reached its target.
      resolve();
      return;
    }

    const checkPhase = (phase: string | undefined): "success" | "fail" | "pending" => {
      if (phase === "failed") return "fail";
      if (phase && SUCCESS_PHASES.has(phase)) return "success";
      return "pending";
    };

    // Check initial state — handler may have fired synchronously.
    const initial = checkPhase(root.dataset.pipelinePhase);
    if (initial === "success") {
      resolve();
      return;
    }
    if (initial === "fail") {
      reject(
        new Error(
          `Step ${stepNumber} failed: pipeline reported failure (phase=failed) after clicking ${triggerId}`,
        ),
      );
      return;
    }

    let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
    const observer = new MutationObserver(() => {
      const phase = root.dataset.pipelinePhase;
      const verdict = checkPhase(phase);
      if (verdict === "success") {
        observer.disconnect();
        if (timeoutHandle !== null) clearTimeout(timeoutHandle);
        resolve();
      } else if (verdict === "fail") {
        observer.disconnect();
        if (timeoutHandle !== null) clearTimeout(timeoutHandle);
        reject(
          new Error(
            `Step ${stepNumber} failed: pipeline reported failure (phase=failed) after clicking ${triggerId}`,
          ),
        );
      }
    });

    observer.observe(root, { attributes: true, attributeFilter: ["data-pipeline-phase"] });

    timeoutHandle = setTimeout(() => {
      observer.disconnect();
      reject(
        new Error(
          `Step ${stepNumber} failed: pipeline phase did not transition out of idle within ${timeoutMs / 1000} s after clicking ${triggerId} — handler may not have fired`,
        ),
      );
    }, timeoutMs);
  });
}

interface StateMachineAPI {
  navigateTo(stateId: string): Promise<unknown>;
  findPath(from: string, to: string): unknown;
  getActiveStates(): string[];
}

type NavResult = { ok: true } | { ok: false; error: string };

async function navigateToTarget(
  target: string,
  sm: { stateMachine?: StateMachineAPI } | undefined,
): Promise<NavResult> {
  // Resolve target → tab id. Planner uses "page-<slug>" convention; the runner's
  // PAGE_TO_TAB / MainTabId map uses the bare slug.
  const tabId = (target.startsWith("page-") ? target.slice(5) : target) as MainTabId;

  // Preferred path: state machine navigation, which runs compiled transitions
  // (active-state updates, side effects, multi-hop pathfinding). Only works
  // once the state machine has been compiled — visit the Specs / State Machine
  // tab once to populate it. Verify the switch by polling getActiveStates().
  if (sm?.stateMachine?.navigateTo) {
    try {
      await sm.stateMachine.navigateTo(target);
      await new Promise((r) => setTimeout(r, 500));
      if (sm.stateMachine.getActiveStates().includes(target)) {
        return { ok: true };
      }
      // Navigation completed without throwing but the active state didn't
      // update — fall through to event-dispatch fallback.
    } catch {
      // Fall through.
    }
  }

  // Fallback: dispatch the same `ui-bridge-set-tab` event the sidebar uses.
  // Works without a compiled state machine (essential on fresh runners).
  if (!isValidTabId(tabId)) {
    return {
      ok: false,
      error: `unknown navigation target "${target}" — no tab matches "${tabId}". The runner's planner page list may be out of date.`,
    };
  }
  window.dispatchEvent(new CustomEvent("ui-bridge-set-tab", { detail: { tab: tabId } }));
  return { ok: true };
}
