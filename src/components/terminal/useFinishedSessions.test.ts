/**
 * Tests for the wire parsing behind the Terminal page's FINISHED band.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library (see `useCommitState.test.ts`), so the hook's polling shell is
 * verified through the UI Bridge and its pure halves are asserted here.
 */

import { describe, it, expect } from "vitest";
import { parseFinishedStates, sameFinishedStates } from "./useFinishedSessions";

describe("parseFinishedStates", () => {
  it("reads the command's `{ sessions }` envelope", () => {
    expect(
      parseFinishedStates({
        sessions: {
          a: { verdict: "finished", source: "coord", finishedAt: 5 },
          b: { verdict: "not_finished" },
          c: { verdict: "unknown" },
        },
        coord: { degraded: false, note: "" },
      }),
    ).toEqual({
      a: { verdict: "finished", source: "coord", finishedAt: 5 },
      b: { verdict: "not_finished" },
      c: { verdict: "unknown" },
    });
  });

  it("drops an unrecognised verdict so it reads as UNKNOWN, not as either arm", () => {
    const out = parseFinishedStates({
      sessions: { a: { verdict: "done" }, b: null, c: { verdict: 1 } },
    });
    expect(out).toEqual({});
  });

  it("keeps the verdict but not a malformed source or timestamp", () => {
    expect(
      parseFinishedStates({
        sessions: { a: { verdict: "finished", source: "remote", finishedAt: "5" } },
      }),
    ).toEqual({ a: { verdict: "finished" } });
  });

  it("returns empty for anything that is not an envelope", () => {
    for (const data of [undefined, null, [], "x", { sessions: "x" }]) {
      expect(parseFinishedStates(data)).toEqual({});
    }
  });
});

describe("sameFinishedStates", () => {
  const a = { s1: { verdict: "finished" as const, source: "local" as const, finishedAt: 1 } };

  it("is true for structurally equal maps, so an idle poll keeps identity", () => {
    expect(sameFinishedStates(a, { s1: { ...a.s1 } })).toBe(true);
  });

  it("notices a verdict, source, timestamp or key change", () => {
    expect(sameFinishedStates(a, { s1: { ...a.s1, verdict: "unknown" } })).toBe(false);
    expect(sameFinishedStates(a, { s1: { ...a.s1, source: "both" } })).toBe(false);
    expect(sameFinishedStates(a, { s1: { ...a.s1, finishedAt: 2 } })).toBe(false);
    expect(sameFinishedStates(a, { s2: a.s1 })).toBe(false);
    expect(sameFinishedStates(a, {})).toBe(false);
  });
});
