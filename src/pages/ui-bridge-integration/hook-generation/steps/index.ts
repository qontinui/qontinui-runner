/**
 * STEP_HANDLERS — the composition point of the hook-generation step machine.
 *
 * Typed `Record<IntegrationStep, StepHandler>`, so a step without a handler is
 * a compile error.
 */

import { architectureSpecStep } from "./architectureSpec";
import { explainerStep } from "./explainer";
import { hooksStep } from "./hooks";
import { pageArchitectureDiagramStep } from "./pageArchitectureDiagram";
import { pageDemoScriptStep } from "./pageDemoScript";
import { pageRegistrationsStep } from "./pageRegistrations";
import { pageSpecStep } from "./pageSpec";
import { pageTutorialStep } from "./pageTutorial";
import type { IntegrationStep, StepHandler } from "./types";

export const STEP_HANDLERS: Record<IntegrationStep, StepHandler> = {
  hooks: hooksStep,
  "architecture-spec": architectureSpecStep,
  "page-registrations": pageRegistrationsStep,
  "page-spec": pageSpecStep,
  "page-tutorial": pageTutorialStep,
  "page-architecture-diagram": pageArchitectureDiagramStep,
  "page-demo-script": pageDemoScriptStep,
  "explainer-index": explainerStep,
  "explainer-cluster": explainerStep,
  "explainer-page": explainerStep,
};
