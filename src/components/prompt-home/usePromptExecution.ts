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

export interface StepProgress {
  currentIndex: number;
  total: number;
  currentStep: PlanStep | null;
}

interface UsePromptExecutionReturn {
  phase: ExecutionPhase;
  plan: PromptPlan | null;
  progress: StepProgress;
  error: string | null;
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
  }, []);

  const submit = useCallback(
    async (prompt: string, explain: boolean) => {
      if (runningRef.current) return;
      runningRef.current = true;
      abortRef.current = false;
      setLastPrompt(prompt);
      lastExplainRef.current = explain;
      setError(null);
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
            const result = await executor.execute({ instruction: step.instruction });
            if (!result.success) {
              throw new Error(
                `Step ${i + 1} failed: ${result.error ?? result.errorCode ?? "action failed"} — could not ${step.instruction}`,
              );
            }
            // Wait for action to settle
            await new Promise((r) => setTimeout(r, 300));
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

  return { phase, plan, progress, error, lastPrompt, submit, retry, reset };
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
