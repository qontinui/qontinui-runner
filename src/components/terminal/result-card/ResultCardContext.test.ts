/**
 * Tests for the result-card surface.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no React
 * Testing Library) — see HoldingLockBanner.test.tsx / useNow1Hz.test.tsx for
 * the precedent. We CANNOT render the provider, so we:
 *   1. Tripwire-import `ResultCardProvider`, `useResultCard`,
 *      `resultCardReducer` so an accidental rename fails here (and so the
 *      `.tsx` participates in `tsc --noEmit`).
 *   2. Exercise the EXPORTED pure reducer directly — that's the state machine
 *      the provider consumes via `useReducer`.
 */

import { describe, it, expect } from "vitest";

import {
  ResultCardProvider,
  useResultCard,
  resultCardReducer,
  type ResultCardSpec,
} from "./ResultCardContext";

// Tripwire: importing the provider + hook must succeed. They can't be invoked
// outside a React render — that's why the reducer below is the unit under test.
void ResultCardProvider;
void useResultCard;

describe("result-card surface (tripwire)", () => {
  it("exports the provider, hook, and reducer by their canonical names", () => {
    expect(typeof ResultCardProvider).toBe("function");
    expect(typeof useResultCard).toBe("function");
    expect(typeof resultCardReducer).toBe("function");
  });
});

describe("resultCardReducer", () => {
  const specA: ResultCardSpec = {
    title: "Metrics",
    subtitle: "session a",
    sections: [{ rows: [{ label: "sessions", value: "3" }] }],
  };
  const specB: ResultCardSpec = {
    title: "Doc",
    sections: [{ heading: "Files", rows: [{ label: "README.md", value: "ok" }] }],
  };

  it("'show' from null sets the spec", () => {
    expect(resultCardReducer(null, { type: "show", spec: specA })).toBe(specA);
  });

  it("'show' over an existing card replaces it", () => {
    const next = resultCardReducer(specA, { type: "show", spec: specB });
    expect(next).toBe(specB);
    expect(next).not.toBe(specA);
  });

  it("'dismiss' from a shown card returns null", () => {
    expect(resultCardReducer(specA, { type: "dismiss" })).toBeNull();
  });

  it("'dismiss' from null stays null (no-op)", () => {
    expect(resultCardReducer(null, { type: "dismiss" })).toBeNull();
  });
});
