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
      title: step.type === "navigate" ? `Navigating to ${step.target ?? "page"}` : "Performing action",
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

  // Open tutorial when execution starts
  useEffect(() => {
    if (!explain || phase !== "executing" || !planSteps?.length || openedRef.current) return;
    openedRef.current = true;
    const tutorial = buildTutorial(planSummary ?? "Runner action", planSteps);
    openTutorial(tutorial);
  }, [explain, phase, planSteps, planSummary, openTutorial]);

  // Advance tutorial step as execution progresses
  useEffect(() => {
    if (!explain || !isOpen || progress.currentIndex <= 0) return;
    nextStep();
  }, [explain, isOpen, progress.currentIndex, nextStep]);

  // Close tutorial when done
  useEffect(() => {
    if (phase === "done" || phase === "error" || phase === "idle") {
      if (openedRef.current && isOpen) {
        closeTutorial();
      }
      openedRef.current = false;
    }
  }, [phase, isOpen, closeTutorial]);
}
