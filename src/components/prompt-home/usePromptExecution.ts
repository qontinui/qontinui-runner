import { useState, useCallback, useRef } from "react";
import { useUIBridge } from "@qontinui/ui-bridge";
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
            // Use NLActionExecutor on current page DOM
            const { NLActionExecutor } = await import("@qontinui/ui-bridge/ai");
            const discovered = await bridge.discover({ includeHidden: false });
            const executor = new NLActionExecutor();
            executor.updateElements(discovered.elements);
            executor.setActionExecutor(bridge as never);

            // P18A — capture pre-action state for "<verb> element <id>" forms.
            // The id-routing path takes a deterministic registry lookup (no
            // fuzzy text match), so a state snapshot before/after the action
            // is meaningful. Free-text instructions go through fuzzy search
            // and may be re-resolved between calls, so we skip the toggle
            // check there and rely on the action executor's own success flag.
            const idMatch = step.instruction.match(/^(?:click|check|uncheck) element ([\w-]+)$/i);
            const targetId = idMatch ? idMatch[1] : null;
            const preState = targetId ? bridge.getElementState(targetId) : undefined;

            const result = await executor.execute({ instruction: step.instruction });
            if (!result.success) {
              throw new Error(
                `Step ${i + 1} failed: ${result.error ?? result.errorCode ?? "action failed"} — could not ${step.instruction}`,
              );
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
  const intervalMs = 200;
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const root = document.querySelector<HTMLElement>("[data-pipeline-phase]");
    const phase = root?.dataset.pipelinePhase;
    if (phase === "failed") {
      throw new Error(
        `Step ${stepNumber} failed: pipeline reported failure (phase=failed) after clicking ${triggerId}`,
      );
    }
    if (phase && phase !== "idle") {
      return;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error(
    `Step ${stepNumber} failed: pipeline phase did not transition out of idle within ${timeoutMs / 1000} s after clicking ${triggerId} — handler may not have fired`,
  );
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
