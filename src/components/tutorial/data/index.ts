/**
 * Tutorial Data Registry
 *
 * Central registry for all available tutorials in the runner.
 */

import type { Tutorial, TutorialFocusPage } from "../../../types/tutorial";
import { gettingStartedTutorial } from "./getting-started";
import { workflowExecutionTutorial } from "./workflow-execution";
import { aiAnalysisTutorial } from "./ai-analysis";
import { promptWorkflowTutorial } from "./prompt-workflow";
import { checkBuilderTutorial } from "./check-builder";

/**
 * All available tutorials
 * Order matters - featured/recommended tutorials should come first
 */
export const tutorials: Tutorial[] = [
  gettingStartedTutorial,
  promptWorkflowTutorial, // Featured - AI prompt workflows
  checkBuilderTutorial, // Code quality checks
  workflowExecutionTutorial,
  aiAnalysisTutorial,
];

/**
 * Get a tutorial by ID
 */
export function getTutorialById(id: string): Tutorial | undefined {
  return tutorials.find((t) => t.id === id);
}

/**
 * Get tutorials by category
 */
export function getTutorialsByCategory(category: string): Tutorial[] {
  return tutorials.filter((t) => t.category === category);
}

/**
 * Get tutorials by difficulty
 */
export function getTutorialsByDifficulty(
  difficulty: "beginner" | "intermediate" | "advanced",
): Tutorial[] {
  return tutorials.filter((t) => t.difficulty === difficulty);
}

/**
 * Get tutorials by focus page
 * Returns all tutorials that should be displayed on a specific page
 */
export function getTutorialsByFocusPage(page: TutorialFocusPage): Tutorial[] {
  return tutorials.filter((t) => t.focusPage === page);
}

/**
 * Get the recommended first tutorial for new users
 */
export function getFirstTimeTutorial(): Tutorial {
  return gettingStartedTutorial;
}

/**
 * Get featured tutorials (marked with "featured" tag)
 * These are highlighted in the UI for easy discovery
 */
export function getFeaturedTutorials(): Tutorial[] {
  return tutorials.filter((t) => t.tags.includes("featured"));
}

/**
 * Get recommended next tutorial after completing one
 */
export function getRecommendedNextTutorial(completedId: string): Tutorial | undefined {
  const completed = tutorials.find((t) => t.id === completedId);
  if (!completed) return undefined;

  // Find tutorials that have this one as a prerequisite
  const nextTutorials = tutorials.filter(
    (t) => t.prerequisites?.includes(completedId) && t.id !== completedId,
  );

  // Return the first one, or a featured tutorial if none found
  return nextTutorials[0] ?? getFeaturedTutorials().find((t) => t.id !== completedId);
}

// Re-export individual tutorials
export { gettingStartedTutorial } from "./getting-started";
export { promptWorkflowTutorial } from "./prompt-workflow";
export { checkBuilderTutorial } from "./check-builder";
export { workflowExecutionTutorial } from "./workflow-execution";
export { aiAnalysisTutorial } from "./ai-analysis";
