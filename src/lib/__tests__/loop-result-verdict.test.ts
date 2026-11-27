import { describe, it, expect } from "vitest";

import type { LoopResult } from "@/types/aiData";

import { loopVerdict } from "../loop-result-verdict";

function result(overrides: Partial<LoopResult>): LoopResult {
  return {
    iterations_run: 1,
    verification_passed: false,
    max_iterations_reached: false,
    critical_failure: false,
    was_stopped: false,
    iteration_results: [],
    summary: "",
    ...overrides,
  };
}

describe("loopVerdict", () => {
  it("reads a loop that checked nothing as nothing_verified, not failed", () => {
    expect(loopVerdict(result({ no_verification_steps: true }))).toBe("nothing_verified");
  });

  it("reads a result serialized before the field existed as failed", () => {
    expect(loopVerdict(result({}))).toBe("failed");
  });

  it("keeps a pass a pass", () => {
    expect(loopVerdict(result({ verification_passed: true }))).toBe("passed");
  });

  it("ranks a pass above nothing_verified and nothing_verified above critical", () => {
    expect(loopVerdict(result({ verification_passed: true, no_verification_steps: true }))).toBe(
      "passed",
    );
    expect(loopVerdict(result({ critical_failure: true, no_verification_steps: true }))).toBe(
      "nothing_verified",
    );
  });

  it("lets a stop win over every other flag", () => {
    expect(loopVerdict(result({ was_stopped: true, no_verification_steps: true }))).toBe("stopped");
  });

  it("reads max iterations and critical failure as themselves", () => {
    expect(loopVerdict(result({ max_iterations_reached: true }))).toBe("max_iterations");
    expect(loopVerdict(result({ critical_failure: true }))).toBe("critical_failure");
  });
});
