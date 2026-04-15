import { useState, useCallback, useRef } from "react";
import { useUIBridge } from "ui-bridge";
import { tracedFetch } from "@/lib/traced-fetch";
import { getApiBase } from "@/lib/runner-api";

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
        // Step 1: Get the action plan from backend
        const resp = await tracedFetch(`${getApiBase()}/prompt-home/plan`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ prompt, explain }),
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
            // Use state machine navigation with pathfinding
            if (sm?.stateMachine?.navigateTo) {
              await sm.stateMachine.navigateTo(step.target);
            }
            // Wait for page to settle
            await new Promise((r) => setTimeout(r, 500));
          } else if (step.type === "action" && step.instruction) {
            // Use NLActionExecutor on current page DOM
            const { NLActionExecutor } = await import("ui-bridge/ai");
            const discovered = await bridge.discover({ includeHidden: false });
            const executor = new NLActionExecutor();
            executor.updateElements(discovered.elements);
            executor.setActionExecutor(bridge as never);
            await executor.execute({ instruction: step.instruction });
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
