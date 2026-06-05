/**
 * Phase 3 / F1 — unit tests for the synthetic-tab inertness guards.
 *
 * `isSyntheticTab` / `isInjectedSession` are the predicates that session-level
 * process-control ops (resume / promote / commit) consult to no-op against a
 * debug-injected fake. These tests pin the detection logic across both
 * projection modes (short-circuit + tab-backed) and both call shapes
 * (TerminalTab object + bare id string).
 */

import { describe, it, expect } from "vitest";
import { isSyntheticTab, isInjectedSession } from "./syntheticTabs";
import type { TerminalTab } from "./useTerminalManager";
import type { TranscriptSession } from "./useTranscriptSessions";

function tab(partial: Partial<TerminalTab> & Pick<TerminalTab, "id">): TerminalTab {
  return {
    title: "t",
    pid: null,
    isAlive: true,
    exitCode: null,
    ...partial,
  } as TerminalTab;
}

function transcript(
  partial: Partial<TranscriptSession> = {},
): Pick<TranscriptSession, "injected_live_status" | "injected_tab"> {
  return {
    injected_live_status: undefined,
    injected_tab: undefined,
    ...partial,
  };
}

describe("isSyntheticTab", () => {
  it("is true for a tab carrying the __synthetic flag", () => {
    expect(isSyntheticTab(tab({ id: "synthetic-abc", __synthetic: true }))).toBe(true);
  });

  it("is true for a tab whose id carries the synthetic- prefix even without the flag", () => {
    expect(isSyntheticTab(tab({ id: "synthetic-abc" }))).toBe(true);
  });

  it("is false for a real tab", () => {
    expect(isSyntheticTab(tab({ id: "terminal-1" }))).toBe(false);
  });

  it("string form: keys on the synthetic- prefix", () => {
    expect(isSyntheticTab("synthetic-xyz")).toBe(true);
    expect(isSyntheticTab("real-session-id")).toBe(false);
  });

  it("is false for null / undefined", () => {
    expect(isSyntheticTab(null)).toBe(false);
    expect(isSyntheticTab(undefined)).toBe(false);
  });
});

describe("isInjectedSession", () => {
  it("is true for a short-circuit fake (injected_live_status, no tab)", () => {
    expect(
      isInjectedSession({
        zoneTabId: null,
        _transcript: transcript({ injected_live_status: "active-in-zone" }),
      }),
    ).toBe(true);
  });

  it("is true for a tab-backed fake (injected_tab + synthetic zoneTabId)", () => {
    expect(
      isInjectedSession({
        zoneTabId: "synthetic-idle-1",
        _transcript: transcript({
          injected_tab: { is_alive: true, exit_code: null, quiet_ms: 61_000 },
        }),
      }),
    ).toBe(true);
  });

  it("is true when only the synthetic zoneTabId pointer is present", () => {
    expect(
      isInjectedSession({
        zoneTabId: "synthetic-orphan",
        _transcript: transcript(),
      }),
    ).toBe(true);
  });

  it("is false for a real session (no injected fields, real tab)", () => {
    expect(
      isInjectedSession({
        zoneTabId: "terminal-3",
        _transcript: transcript(),
      }),
    ).toBe(false);
  });

  it("is false for a real dormant transcript with no tab", () => {
    expect(
      isInjectedSession({ zoneTabId: null, _transcript: transcript() }),
    ).toBe(false);
  });
});
