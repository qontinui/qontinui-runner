import { useEffect, useRef } from "react";
import { useTutorial } from "@/contexts/TutorialContext";
import { getPageDescription } from "@/lib/explain-mode/spec-explainer";
import type { Tutorial, TutorialStep } from "@/types/tutorial";
import type { PlanStep, StepProgress, ExecutionPhase } from "./usePromptExecution";

function buildTutorialSteps(steps: PlanStep[]): TutorialStep[] {
  return steps.map((step, i) => {
    let content = step.explanation;
    // Enrich navigation steps with spec-based page descriptions
    if (step.type === "navigate" && step.target) {
      const pageDesc = getPageDescription(step.target);
      if (pageDesc) {
        content = `${content}\n\n*${pageDesc}*`;
      }
    }
    return {
      id: `explain-step-${i}`,
      title:
        step.type === "navigate" ? `Navigating to ${step.target ?? "page"}` : "Performing action",
      content,
      action: step.instruction,
      estimatedDuration: 1,
    };
  });
}

function buildTutorial(summary: string, steps: PlanStep[]): Tutorial {
  return {
    id: `explain-${Date.now()}`,
    title: summary,
    description: "Explaining what the runner is doing step by step",
    duration: `${steps.length} steps`,
    difficulty: "beginner",
    steps: buildTutorialSteps(steps),
    mode: "contextual",
    category: "Explain Mode",
  };
}

export function useExplainModeTutorial(
  explain: boolean,
  phase: ExecutionPhase,
  progress: StepProgress,
  planSteps: PlanStep[] | undefined,
  planSummary: string | undefined,
) {
  const { openTutorial, nextStep, closeTutorial, isOpen } = useTutorial();
  const openedRef = useRef(false);
  // Tracks the last progress.currentIndex we've already advanced the tutorial
  // for, so re-renders (which change the nextStep reference and re-fire the
  // effect below) don't call nextStep twice for the same index. Calling it
  // twice when we're already on the last tutorial step triggers
  // completeTutorial() — which closes the modal.
  const lastAdvancedIdxRef = useRef(-1);

  // Open tutorial when execution starts
  useEffect(() => {
    if (!explain || phase !== "executing" || !planSteps?.length || openedRef.current) return;
    openedRef.current = true;
    lastAdvancedIdxRef.current = -1;
    const tutorial = buildTutorial(planSummary ?? "Runner action", planSteps);
    openTutorial(tutorial);
  }, [explain, phase, planSteps, planSummary, openTutorial]);

  // Advance tutorial step as execution progresses.
  // Dedupe by progress.currentIndex so we only fire nextStep once per plan
  // step even if the effect re-runs (e.g. nextStep reference changes after a
  // tutorial state update). Also stop once we've reached the last step so
  // the tutorial doesn't auto-complete when execution finishes.
  useEffect(() => {
    if (!explain || !isOpen || progress.currentIndex <= 0) return;
    if (progress.total > 0 && progress.currentIndex >= progress.total) return;
    if (progress.currentIndex === lastAdvancedIdxRef.current) return;
    lastAdvancedIdxRef.current = progress.currentIndex;
    nextStep();
  }, [explain, isOpen, progress.currentIndex, progress.total, nextStep]);

  // Tutorial lifecycle:
  //   - On "error": close the tutorial — its content describes the plan that
  //     was interrupted, so leaving it open would be misleading.
  //   - On "planning": a new submit is starting; reset the open-guard so the
  //     next "executing" render opens a fresh tutorial (replacing any modal
  //     the user left open from the previous run).
  //   - On "done" / "idle": do NOT auto-close. The overlay auto-dismisses 4s
  //     after done, but the tutorial should stay open until the user dismisses
  //     it — otherwise short plans flash the modal and defeat explain mode.
  useEffect(() => {
    if (phase === "error") {
      if (openedRef.current && isOpen) {
        closeTutorial();
      }
      openedRef.current = false;
    } else if (phase === "planning") {
      openedRef.current = false;
    }
  }, [phase, isOpen, closeTutorial]);

  // Keep the open-guard in sync when the user manually dismisses the tutorial.
  useEffect(() => {
    if (!isOpen) openedRef.current = false;
  }, [isOpen]);
}
