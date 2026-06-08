/**
 * SessionRecoveryBanner tests — Phase 4 honesty-label logic.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom /
 * @testing-library — see `LaunchMenu.test.tsx`), so we exercise the
 * load-bearing PURE helpers the banner renders from, asserting the UX
 * honesty gate: a lossy resume, a peer-conflict / degraded claim, and a
 * kept-for-manual WIP are surfaced verbatim and flagged `degraded`, while a
 * fully-clean lossless resume is NOT. The JSX glue is straightforward and is
 * exercised manually via UI Bridge.
 */

import { describe, it, expect } from "vitest";
import {
  fidelityLabel,
  isLossy,
  isDegraded,
  claimLabel,
  wipLabel,
} from "./SessionRecoveryBanner";
import type { SessionRecoveryOutcome } from "@/hooks/useSessionRecovery";

function outcome(over: Partial<SessionRecoveryOutcome>): SessionRecoveryOutcome {
  return {
    taskRunId: "t1",
    title: "Session",
    fidelity: "lossless",
    turnsCovered: 0,
    claimStatus: "reacquired",
    wipStatus: "none",
    ...over,
  };
}

describe("fidelityLabel", () => {
  it("labels a lossless resume as fully restored", () => {
    expect(fidelityLabel(outcome({ fidelity: "lossless" }))).toBe("fully restored");
  });

  it("labels a lossy summary with the HONEST turn count (plural)", () => {
    expect(fidelityLabel(outcome({ fidelity: "summary", turnsCovered: 6 }))).toBe(
      "resumed from last 6 turns",
    );
  });

  it("uses the singular for a one-turn summary", () => {
    expect(fidelityLabel(outcome({ fidelity: "summary", turnsCovered: 1 }))).toBe(
      "resumed from last 1 turn",
    );
  });
});

describe("isLossy", () => {
  it("flags a summary fidelity as lossy", () => {
    expect(isLossy(outcome({ fidelity: "summary", turnsCovered: 3 }))).toBe(true);
    expect(isLossy(outcome({ fidelity: "lossless" }))).toBe(false);
  });
});

describe("claimLabel", () => {
  it("returns null for a clean re-acquire (no callout)", () => {
    expect(claimLabel("reacquired")).toBeNull();
    expect(claimLabel("none")).toBeNull();
  });

  it("surfaces a peer-conflict honestly", () => {
    expect(claimLabel("peer-conflict")).toMatch(/another agent/i);
  });

  it("surfaces a degraded claim honestly", () => {
    expect(claimLabel("degraded")).toMatch(/not re-acquired/i);
  });
});

describe("wipLabel", () => {
  it("returns null when no WIP", () => {
    expect(wipLabel("none")).toBeNull();
  });

  it("confirms restored WIP", () => {
    expect(wipLabel("restored")).toMatch(/restored/i);
  });

  it("surfaces kept-for-manual WIP honestly with the ref path", () => {
    expect(wipLabel("kept-for-manual")).toMatch(/refs\/wip/i);
  });
});

describe("isDegraded — honesty gate", () => {
  it("is false for a fully-clean lossless + reacquired + no-WIP session", () => {
    expect(isDegraded(outcome({}))).toBe(false);
  });

  it("is true on a lossy resume", () => {
    expect(isDegraded(outcome({ fidelity: "summary", turnsCovered: 2 }))).toBe(true);
  });

  it("is true on a peer-conflict claim", () => {
    expect(isDegraded(outcome({ claimStatus: "peer-conflict" }))).toBe(true);
  });

  it("is true on a degraded claim", () => {
    expect(isDegraded(outcome({ claimStatus: "degraded" }))).toBe(true);
  });

  it("is true on kept-for-manual WIP", () => {
    expect(isDegraded(outcome({ wipStatus: "kept-for-manual" }))).toBe(true);
  });
});
