/**
 * The deriver, and the five properties it inherits from `spawnVerdict`.
 *
 * `spawnVerdict.test.ts` still owns the spawn-specific contract (it is the
 * adapter's own spec, and the three call sites depend on its `string[]`
 * payload). This file owns the GENERAL one, so that a future call site can
 * rely on the normalisation and the produced-vs-requested comparison without
 * reading the spawn adapter to find out what they are.
 *
 * Runs under vitest's `environment: "node"` — the module is pure, which is
 * property 1 and is what makes this file possible at all.
 */

import { describe, expect, it } from "vitest";

import {
  countOf,
  deriveVerdict,
  describeReport,
  effect,
  isEffectReport,
  pluralize,
  stateEffect,
  type EffectReport,
} from "./verdict";

describe("countOf — property 2, normalise the untrustworthy return", () => {
  it("folds every non-answer to ZERO rather than to success", () => {
    // These are the shapes an effect on this page actually hands back when it
    // bailed. Every one of them used to be laundered into a `✓`.
    for (const nothing of [undefined, null, "", "3", NaN, Infinity, -1, {}, () => 2]) {
      expect(countOf(nothing), String(nothing)).toBe(0);
    }
  });

  it("reads a list as its length", () => {
    expect(countOf([])).toBe(0);
    expect(countOf(["a", "b", "c"])).toBe(3);
  });

  it("reads a boolean as the minimum useful signal a toggle can give", () => {
    expect(countOf(true)).toBe(1);
    expect(countOf(false)).toBe(0);
  });

  it("reads the named count fields off a report bag", () => {
    expect(countOf({ affected: 4 })).toBe(4);
    expect(countOf({ delivered: 2 })).toBe(2);
    expect(countOf({ changed: true })).toBe(1);
    expect(countOf({ moved: 0 })).toBe(0);
    expect(countOf({ count: 7 })).toBe(7);
  });

  it("reads a Set/Map as its size — `/select-by-state` builds a Set", () => {
    expect(countOf(new Set([1, 2]))).toBe(2);
    expect(countOf(new Map())).toBe(0);
  });

  it("FLOORS a fractional count instead of trusting it", () => {
    // `3 < 2.7` is false, which is how `/spawn 2.7` created three terminals
    // and reported success. A fractional affected count would put that same
    // comparison back into every command.
    expect(countOf(2.7)).toBe(2);
  });
});

describe("deriveVerdict — properties 3, 4 and 5", () => {
  it("FAILS on a shortfall and names both sides", () => {
    const v = deriveVerdict({
      produced: ["a"],
      requested: 3,
      verb: "approved",
      noun: "session",
      code: "approve-failed",
    });
    expect(v.ok).toBe(false);
    if (v.ok) throw new Error("unreachable");
    expect(v.code).toBe("approve-failed");
    expect(v.message).toBe("approved 1 of 3 sessions");
  });

  it("FAILS when the effect reported nothing at all against a target", () => {
    const v = deriveVerdict({ produced: undefined, requested: 1, verb: "closed", noun: "session" });
    expect(v.ok).toBe(false);
    if (v.ok) throw new Error("unreachable");
    // A caller that names no code still gets a stable machine-readable one.
    expect(v.code).toBe("effect-fell-short");
    expect(v.message).toBe("closed 0 of 1 session");
  });

  it("does not fail an effect that over-delivers", () => {
    expect(deriveVerdict({ produced: 5, requested: 2, verb: "moved", noun: "zone" }).ok).toBe(true);
  });

  it("succeeds with ZERO when no target was named — the no-op arm", () => {
    // `/tag-clear` on an empty filter set. Legitimately nothing to do, so it
    // is a success carrying an honest zero rather than an error. The status
    // line, not the result, is what renders it differently.
    const v = deriveVerdict({ produced: 0, verb: "cleared", noun: "tag filter" });
    expect(v.ok).toBe(true);
    if (!v.ok) throw new Error("unreachable");
    expect(v.value).toEqual({ verb: "cleared", noun: "tag filter", affected: 0 });
  });

  it("carries the parameterised noun through — property 5", () => {
    const v = deriveVerdict({
      produced: [],
      requested: 2,
      verb: "spawned",
      noun: "AI session",
      nounPlural: "AI sessions",
    });
    if (v.ok) throw new Error("unreachable");
    expect(v.message).toContain("AI sessions");
  });

  it("omits absent optional fields rather than writing undefined into them", () => {
    const v = deriveVerdict({ produced: 1, verb: "closed", noun: "session" });
    if (!v.ok) throw new Error("unreachable");
    expect(Object.keys(v.value as EffectReport).sort()).toEqual(["affected", "noun", "verb"]);
  });
});

describe("isEffectReport — the renderer's structural gate", () => {
  it("accepts a real report and rejects everything the old handlers returned", () => {
    expect(isEffectReport(effect("closed", "session", 1))).toBe(true);
    // The pre-phase payloads, which must keep rendering as the bare check-mark
    // rather than crashing or being half-read.
    for (const other of [undefined, null, ["tab-1"], { approved: 3 }, { runId: "r" }, 3, "x"]) {
      expect(isEffectReport(other), JSON.stringify(other) ?? "undefined").toBe(false);
    }
  });
});

describe("describeReport — the operator-facing sentence", () => {
  it("reads correctly in the affirmative, the partial and the zero", () => {
    expect(describeReport(effect("approved", "session", 3))).toBe("approved 3 sessions");
    expect(describeReport(effect("closed", "session", 1))).toBe("closed 1 session");
    expect(describeReport(effect("approved", "session", 2, { requested: 3 }))).toBe(
      "approved 2 of 3 sessions",
    );
    expect(describeReport(effect("selected", "zone", 0))).toBe("no zones selected");
  });

  it("does not say `of` when the effect met its target exactly", () => {
    expect(describeReport(effect("approved", "session", 3, { requested: 3 }))).toBe(
      "approved 3 sessions",
    );
  });

  it("reads a state report as a state, not as a count", () => {
    // `enabled 1 focus mode` is worse than the bare check-mark it replaced.
    expect(describeReport(stateEffect("enabled", "focus mode", true))).toBe("enabled focus mode");
    expect(describeReport(stateEffect("muted", "sound", false))).toBe("sound was already muted");
  });

  it("appends detail when the handler supplied a reason", () => {
    expect(describeReport(effect("changed", "layout", 0, { detail: "already quad" }))).toBe(
      "no layouts changed — already quad",
    );
  });

  it("honours an irregular plural", () => {
    expect(pluralize({ noun: "entry", nounPlural: "entries" }, 2)).toBe("entries");
    expect(pluralize({ noun: "entry", nounPlural: "entries" }, 1)).toBe("entry");
  });
});
