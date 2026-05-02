import { useState, useCallback, useRef } from "react";
import { useUIBridge, type ControlActionRequest } from "@qontinui/ui-bridge";
import { tracedFetch } from "@/lib/traced-fetch";
import { getApiBase } from "@/lib/runner-api";
import { isValidTabId, type MainTabId } from "@/components/app/tab-types";
import { buildPageCatalog } from "./pageCatalog";

export interface PlanStep {
  type: "navigate" | "action";
  target?: string;
  instruction?: string;
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
                throw new Error(
                  `Step ${i + 1} failed: ${result.error ?? result.errorCode ?? "action failed"} — could not ${step.instruction}`,
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

            // P18B — pipeline-phase poll for the two known phase-changing
            // triggers. After clicking Analyze or Generate, the
            // ProjectCoordinator's `data-pipeline-phase` must transition out
            // of "idle" within 30s; staying at idle means the click reached
            // the DOM but the React handler didn't fire (button gated,
            // double-click guard, project not selected, etc).
            if (targetId && PIPELINE_TRIGGER_IDS.has(targetId)) {
              await awaitPipelinePhaseTransition(i + 1, targetId);
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
const PIPELINE_TRIGGER_IDS = new Set<string>([
  "ui-bridge-analyze-button",
  "ui-bridge-generate-button",
]);

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
async function awaitPipelinePhaseTransition(
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
