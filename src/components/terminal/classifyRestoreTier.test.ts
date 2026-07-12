/**
 * Phase 5 (honest capability tiers): `classifyRestoreAction` must gate
 * auto-resume on the owning provider's `restoreTier()`. A CONFIRMED
 * authoritative record only AUTO-RESUMES when its provider can deterministically
 * resume the FULL conversation by id (`restoreTier() === "full"`). A provider
 * that declares `"terminal-only"` can re-open the terminal but NOT the chat, so
 * the same confirmed-authoritative record restores TERMINAL-ONLY — never a
 * resume typed against an id the provider can't resume.
 *
 * No shipped provider declares `terminal-only` yet (Claude + Gemini are both
 * `full`), so this branch is exercised by STUBBING the descriptor registry — a
 * dedicated test file keeps the module mock isolated from the other
 * `useTerminalInitialization` suites (which assert the real Claude full-tier
 * behavior).
 */

import { describe, it, expect, vi } from "vitest";

// Stub the provider registry so we can drive both tiers deterministically.
vi.mock("./providerAdapter", () => {
  const make = (tier: "full" | "terminal-only") => ({
    provider: tier === "full" ? "full-provider" : "terminal-only-provider",
    resumeCommand: (id: string) => ["x", "--resume", id],
    handshakePatterns: () => ({ success: [], failure: [] }),
    restoreTier: () => tier,
  });
  return {
    providerDescriptorFor: (p: string | undefined) =>
      p === "terminal-only-provider" ? make("terminal-only") : make("full"),
  };
});

import { classifyRestoreAction } from "./useTerminalInitialization";

describe("classifyRestoreAction — restore-tier gate (Phase 5)", () => {
  it("CONFIRMED authoritative + FULL-tier provider ⇒ auto-resume", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "authoritative",
        confirmedAt: 1,
        provider: "full-provider",
      }),
    ).toBe("auto-resume");
  });

  it("CONFIRMED authoritative + TERMINAL-ONLY-tier provider ⇒ terminal-only (no resume)", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "authoritative",
        confirmedAt: 1,
        provider: "terminal-only-provider",
      }),
    ).toBe("terminal-only");
  });

  it("UNCONFIRMED authoritative is a phantom shell ⇒ terminal-only regardless of tier", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "authoritative",
        provider: "full-provider",
      }),
    ).toBe("terminal-only");
  });

  it("CONFIRMED observed + FULL-tier provider ⇒ auto-resume (mirrors authoritative)", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "observed",
        confirmedAt: 1,
        provider: "full-provider",
      }),
    ).toBe("auto-resume");
  });

  it("CONFIRMED observed + TERMINAL-ONLY-tier provider ⇒ terminal-only (no resume)", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "observed",
        confirmedAt: 1,
        provider: "terminal-only-provider",
      }),
    ).toBe("terminal-only");
  });

  it("UNCONFIRMED observed is a phantom shell ⇒ terminal-only regardless of tier", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "observed",
        provider: "full-provider",
      }),
    ).toBe("terminal-only");
  });

  it("reconciled origin is quarantined regardless of provider tier", () => {
    expect(
      classifyRestoreAction({
        claudeSessionId: "sess-1",
        origin: "reconciled",
        confirmedAt: 1,
        provider: "full-provider",
      }),
    ).toBe("quarantine");
  });
});
