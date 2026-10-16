/**
 * STEP_HANDLERS is the composition point of the step machine (plan D2): it
 * must cover exactly the ten `IntegrationStep`s.
 *
 * Exhaustiveness is a compile-time property enforced by `steps/index.ts` itself
 * (typed `Record<IntegrationStep, StepHandler>`, checked by `pnpm typecheck`);
 * test files are excluded from tsc, so this file only checks the runtime keys.
 */

import { describe, it, expect } from "vitest";

import { STEP_HANDLERS } from "./index";
import { explainerStep } from "./explainer";
import type { IntegrationStep } from "./types";

// `IntegrationStep` is a type-only union with no runtime value to derive this
// from, so the expected keys are listed explicitly.
const ALL_STEPS: IntegrationStep[] = [
  "hooks",
  "architecture-spec",
  "page-registrations",
  "page-spec",
  "page-tutorial",
  "page-architecture-diagram",
  "page-demo-script",
  "explainer-index",
  "explainer-cluster",
  "explainer-page",
];

describe("STEP_HANDLERS", () => {
  it("has exactly the ten IntegrationStep keys, each a function", () => {
    expect(Object.keys(STEP_HANDLERS).sort()).toEqual([...ALL_STEPS].sort());
    for (const step of ALL_STEPS) expect(typeof STEP_HANDLERS[step]).toBe("function");
  });

  it("routes the three explainer-* steps to the one explainer handler", () => {
    expect(STEP_HANDLERS["explainer-index"]).toBe(explainerStep);
    expect(STEP_HANDLERS["explainer-cluster"]).toBe(explainerStep);
    expect(STEP_HANDLERS["explainer-page"]).toBe(explainerStep);
  });
});
