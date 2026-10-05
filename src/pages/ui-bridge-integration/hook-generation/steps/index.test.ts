/**
 * STEP_HANDLERS is the composition point of the step machine (plan D2): it
 * must cover exactly the ten `IntegrationStep`s, and a missing step must be a
 * compile error rather than a silent no-op at runtime.
 */

import { describe, it, expect } from "vitest";

import { STEP_HANDLERS } from "./index";
import { explainerStep } from "./explainer";
import type { IntegrationStep, StepHandler } from "./types";

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

  it("is typed so that a missing step is a type error (checked by tsc)", () => {
    // The composition point satisfies the exhaustive Record…
    const full = STEP_HANDLERS satisfies Record<IntegrationStep, StepHandler>;
    // …and dropping any one key does not. `pnpm typecheck` fails if this
    // `@ts-expect-error` ever stops being needed.
    const { hooks: _dropped, ...withoutHooks } = full;
    // @ts-expect-error — Property 'hooks' is missing in type
    const incomplete: Record<IntegrationStep, StepHandler> = withoutHooks;
    expect(Object.keys(incomplete)).not.toContain("hooks");
  });
});
