import { describe, expect, it } from "vitest";
import { describeEffectivenessEvidence } from "./reflectionEvidence";

const written = {
  verdict: "ineffective",
  window: "task_runs.completed_at > reflection_fixes.applied_at",
  later_runs: 5,
  min_later_runs: 3,
  source_finding: true,
  source_signature_known: true,
  recurrences: 2,
  new_signature_peak_runs: 1,
  min_later_runs_for_regression: 2,
};

describe("describeEffectivenessEvidence", () => {
  it("reads the evaluator's JSON as the counts it rests on (same text as the Rust summary)", () => {
    expect(describeEffectivenessEvidence(JSON.stringify(written))).toBe(
      "ineffective after 5 later runs (needs 3): the source finding recurred in 2 of them; " +
        "the most later runs any new signature appeared in was 1 (regression at 2)",
    );
  });

  it("says why a recurrence cannot be observed", () => {
    expect(
      describeEffectivenessEvidence(JSON.stringify({ ...written, source_finding: false })),
    ).toContain("names no source finding");
    expect(
      describeEffectivenessEvidence(
        JSON.stringify({ ...written, source_signature_known: false, later_runs: 1 }),
      ),
    ).toMatch(/after 1 later run .*carries no signature/);
  });

  it("shows hand-set, partial or non-object evidence unchanged", () => {
    for (const raw of [
      "Issue recurred in run 42",
      '{"verdict":"effective","later_runs":3}',
      "42",
      "null",
      "",
    ]) {
      expect(describeEffectivenessEvidence(raw)).toBe(raw);
    }
  });
});
