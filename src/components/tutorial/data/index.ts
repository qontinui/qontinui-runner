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
import { knowledgeGraphTutorial } from "./knowledge-graph";
import { opikIntegrationTutorial } from "./opik-integration";
import { workflowOrchestrationTutorial } from "./workflow-orchestration";
import { graphqlInfrastructureTutorial } from "./graphql-infrastructure";
import { knowledgeAcquisitionTutorial } from "./knowledge-acquisition";
import { actionPlanTutorial } from "./action-plan";
import { observationMemoryTutorial } from "./observation-memory";
import { blameAttributionTutorial } from "./blame-attribution";
import { multimodalVisionTutorial } from "./multimodal-vision";
import { traceAnalysisTutorial } from "./trace-analysis";
import { activityTimelineTutorial } from "./activity-timeline";
import { metaPromptOptimizerTutorial } from "./meta-prompt-optimizer";
import { proceduralSkillsTutorial } from "./procedural-skills";
import { qLearningRouterTutorial } from "./q-learning-router";
import { prmTrainingPipelineTutorial } from "./prm-training-pipeline";
import { adaptiveLearningTutorial } from "./adaptive-learning";

/**
 * All available tutorials
 * Order matters - featured/recommended tutorials should come first
 */
export const tutorials: Tutorial[] = [
  gettingStartedTutorial,
  promptWorkflowTutorial, // Featured - AI prompt workflows
  workflowExecutionTutorial,
  aiAnalysisTutorial,
  knowledgeGraphTutorial,
  opikIntegrationTutorial, // Featured - LLM observability & evaluation
  workflowOrchestrationTutorial, // Featured - Inngest-inspired orchestration
  graphqlInfrastructureTutorial, // Architecture - full-stack GraphQL system
  knowledgeAcquisitionTutorial, // Featured - Knowledge acquisition system
  actionPlanTutorial, // Featured - Structured action plan API
  observationMemoryTutorial, // Featured - Cross-session AI knowledge (Engram-inspired)
  blameAttributionTutorial, // Featured - Blame attribution & convergence
  multimodalVisionTutorial, // Featured - Multimodal vision pipeline
  traceAnalysisTutorial, // Featured - Trace analysis & critical path
  activityTimelineTutorial, // Featured - Screenpipe-inspired capture history & watchers
  metaPromptOptimizerTutorial, // Featured - LLM-based prompt rewriting with canary A/B testing
  proceduralSkillsTutorial, // Featured - Hermes-agent-inspired self-improving procedural memory
  qLearningRouterTutorial, // Featured - Q-learning architecture routing for autoresearch
  prmTrainingPipelineTutorial, // Featured - PRM training pipeline (Plan 14)
  adaptiveLearningTutorial, // Featured - Adaptive learning & prompt evolution (Plan 15)
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
  return tutorials.filter((t) => t.tags?.includes("featured"));
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
export { workflowExecutionTutorial } from "./workflow-execution";
export { aiAnalysisTutorial } from "./ai-analysis";
export { knowledgeGraphTutorial } from "./knowledge-graph";
export { workflowOrchestrationTutorial } from "./workflow-orchestration";
export { opikIntegrationTutorial } from "./opik-integration";
export { knowledgeAcquisitionTutorial } from "./knowledge-acquisition";
export { graphqlInfrastructureTutorial } from "./graphql-infrastructure";
export { actionPlanTutorial } from "./action-plan";
export { observationMemoryTutorial } from "./observation-memory";
export { blameAttributionTutorial } from "./blame-attribution";
export { multimodalVisionTutorial } from "./multimodal-vision";
export { traceAnalysisTutorial } from "./trace-analysis";
export { metaPromptOptimizerTutorial } from "./meta-prompt-optimizer";
export { qLearningRouterTutorial } from "./q-learning-router";
export { prmTrainingPipelineTutorial } from "./prm-training-pipeline";
export { adaptiveLearningTutorial } from "./adaptive-learning";
