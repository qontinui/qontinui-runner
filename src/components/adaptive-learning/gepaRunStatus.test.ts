import { describe, it, expect } from "vitest";
import {
  GEPA_RUN_STATUSES,
  gepaStatusStyle,
  isAcceptedRun,
  isGepaRunStatus,
  type GepaRunStatus,
} from "./gepaRunStatus";

/**
 * The writer's vocabulary, spelled out independently of the module under test —
 * deriving it from `GEPA_RUN_STATUSES` would make the assertion circular. These
 * four strings are exactly what `OptimizationOutcome::status_str`
 * (`src-tauri/src/workflow_generation/gepa_optimizer.rs`) returns.
 *
 * The `satisfies` clause below documents intent but does NOT gate anything:
 * `tsconfig.json` excludes `src/**&#47;*.test.ts` and vitest runs no typecheck, so
 * this file is in no TypeScript program. Drift is caught at RUNTIME by the
 * first test instead, which compares both directions.
 */
const WRITER_VOCABULARY = Object.keys({
  accepted: 0,
  rejected: 0,
  insufficient_data: 0,
  skipped: 0,
} satisfies Record<GepaRunStatus, number>) as GepaRunStatus[];

describe("GEPA run status vocabulary", () => {
  it("covers exactly what the Rust writer emits", () => {
    expect([...GEPA_RUN_STATUSES].sort()).toEqual([...WRITER_VOCABULARY].sort());
  });

  it("does not recognise the pre-gate vocabulary the panel used to assume", () => {
    // `completed` / `pending` / `failed` / `canary` were the strings the read
    // path matched on before the held-out gate landed. Nothing writes them, so
    // treating one as a real verdict would be inventing a decision.
    for (const legacy of ["completed", "pending", "failed", "canary"]) {
      expect(isGepaRunStatus(legacy)).toBe(false);
    }
  });
});

describe("gepaStatusStyle", () => {
  it("gives every status its own colour pair", () => {
    const seen = GEPA_RUN_STATUSES.map((s) => {
      const style = gepaStatusStyle(s);
      return `${style.bg}/${style.text}`;
    });
    expect(new Set(seen).size).toBe(GEPA_RUN_STATUSES.length);
  });

  it("never collapses insufficient_data into rejected", () => {
    const undecided = gepaStatusStyle("insufficient_data");
    const rejected = gepaStatusStyle("rejected");

    expect(undecided.text).not.toBe(rejected.text);
    expect(undecided.bg).not.toBe(rejected.bg);
    expect(undecided.label).not.toBe(rejected.label);
    // The tooltip has to say so out loud — the colour alone is not an
    // explanation of "nothing was decided".
    expect(undecided.title.toLowerCase()).toContain("not a rejection");
  });

  it("labels an unknown status neutrally instead of borrowing a verdict's styling", () => {
    const unknown = gepaStatusStyle("completed");

    expect(unknown.label).toBe("completed");
    for (const status of GEPA_RUN_STATUSES) {
      const known = gepaStatusStyle(status);
      // Both halves, separately. Comparing the concatenation passes while the
      // backgrounds are shared and only the text differs — more assurance than
      // it delivers.
      expect(unknown.bg).not.toBe(known.bg);
      expect(unknown.text).not.toBe(known.text);
    }
  });

  it("falls back to a readable label for an empty status", () => {
    expect(gepaStatusStyle("").label).toBe("unknown");
  });
});

describe("isAcceptedRun", () => {
  it("is true only for accepted", () => {
    for (const status of GEPA_RUN_STATUSES) {
      expect(isAcceptedRun(status)).toBe(status === "accepted");
    }
  });

  it("does not treat an undecided run as a success", () => {
    // The whole point of the first-class InsufficientData arm: a run that could
    // not decide must never be counted alongside the ones that accepted.
    expect(isAcceptedRun("insufficient_data")).toBe(false);
  });
});
