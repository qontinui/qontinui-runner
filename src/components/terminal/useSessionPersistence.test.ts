/**
 * Tests for the session-persistence anti-degradation guard
 * (`isDegradingSave`) — the defense-in-depth that prevents the
 * session-restore race from clobbering a good saved Claude layout with a
 * degraded one (plain shells that haven't re-attached their
 * `claudeSessionId` yet).
 *
 * `isDegradingSave` is pure (no storage / React), so we exercise it directly.
 */

import { describe, it, expect } from "vitest";
import { isDegradingSave } from "./useSessionPersistence";
import type { SavedSessionLayout, SavedSessionConfig } from "./useSessionPersistence";

const NOW = 1_000_000_000_000;
const STALE_THRESHOLD_MS = 24 * 60 * 60 * 1000;

function session(overrides: Partial<SavedSessionConfig> = {}): SavedSessionConfig {
  return {
    title: "t",
    zoneIndex: 0,
    savedAt: NOW,
    ...overrides,
  };
}

function layout(sessions: SavedSessionConfig[], savedAt = NOW): SavedSessionLayout {
  return { layoutId: "quad", sessions, savedAt, focusedZone: 0 };
}

describe("isDegradingSave — degenerate restore-race patterns (should skip)", () => {
  it("skips when same tab count but Claude ids were lost (2 → 0)", () => {
    const existing = layout([
      session({ claudeSessionId: "sess-A" }),
      session({ claudeSessionId: "sess-B" }),
    ]);
    const incoming = layout([session(), session()]); // same 2 tabs, no ids
    expect(isDegradingSave(incoming, existing, NOW)).toBe(true);
  });

  it("skips a partial degradation (2 → 1) when tab count did not drop", () => {
    const existing = layout([
      session({ claudeSessionId: "sess-A" }),
      session({ claudeSessionId: "sess-B" }),
    ]);
    const incoming = layout([session({ claudeSessionId: "sess-A" }), session()]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(true);
  });

  it("skips when tab count GREW but Claude ids were lost", () => {
    const existing = layout([session({ claudeSessionId: "sess-A" })]);
    const incoming = layout([session(), session()]); // 2 plain tabs
    expect(isDegradingSave(incoming, existing, NOW)).toBe(true);
  });
});

describe("isDegradingSave — legitimate saves (should allow)", () => {
  it("allows when the user actually closed a Claude tab (count drops with ids)", () => {
    const existing = layout([
      session({ claudeSessionId: "sess-A" }),
      session({ claudeSessionId: "sess-B" }),
    ]);
    // One tab genuinely closed: total count dropped to 1.
    const incoming = layout([session({ claudeSessionId: "sess-A" })]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });

  it("allows when the user closed ALL Claude tabs (2 → 0 tabs total)", () => {
    const existing = layout([
      session({ claudeSessionId: "sess-A" }),
      session({ claudeSessionId: "sess-B" }),
    ]);
    const incoming = layout([]); // everything closed
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });

  it("allows an equal save (same ids, same count)", () => {
    const existing = layout([session({ claudeSessionId: "sess-A" })]);
    const incoming = layout([session({ claudeSessionId: "sess-A" })]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });

  it("allows an improvement (0 → 1 Claude id)", () => {
    const existing = layout([session()]);
    const incoming = layout([session({ claudeSessionId: "sess-A" })]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });

  it("allows when there is no existing layout", () => {
    const incoming = layout([session()]);
    expect(isDegradingSave(incoming, null, NOW)).toBe(false);
  });

  it("allows when the existing layout had zero Claude sessions", () => {
    const existing = layout([session(), session()]);
    const incoming = layout([session()]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });

  it("allows (does not guard against) a stale existing layout", () => {
    const existing = layout(
      [session({ claudeSessionId: "sess-A" }), session({ claudeSessionId: "sess-B" })],
      NOW - STALE_THRESHOLD_MS - 1, // older than the stale threshold
    );
    const incoming = layout([session(), session()]);
    expect(isDegradingSave(incoming, existing, NOW)).toBe(false);
  });
});
