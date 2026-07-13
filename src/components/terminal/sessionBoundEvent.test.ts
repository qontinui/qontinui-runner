/**
 * Decision table for the `session-bound` tab-stamp (pure core). The stakes:
 * a wrong stamp mislabels durability; a missed stamp leaves the dishonest
 * "ephemeral" tag this channel exists to fix.
 */

import { describe, it, expect } from "vitest";
import { applySessionBound, type SessionBoundPayload } from "./sessionBoundEvent";

function payload(overrides: Partial<SessionBoundPayload> = {}): SessionBoundPayload {
  return {
    terminalId: "term-1",
    sessionId: "sess-1",
    configDir: "C:/claude/.claude-gmail",
    origin: "observed",
    confirmed: true,
    ...overrides,
  };
}

describe("applySessionBound", () => {
  it("stamps the matching unbound tab (id + config dir)", () => {
    const tabs = [{ id: "term-0" }, { id: "term-1" }];
    expect(applySessionBound(tabs, payload())).toEqual({
      tabId: "term-1",
      claudeSessionId: "sess-1",
      claudeConfigDir: "C:/claude/.claude-gmail",
    });
  });

  it("empty configDir maps to undefined, never an empty path", () => {
    const tabs = [{ id: "term-1" }];
    expect(applySessionBound(tabs, payload({ configDir: "" }))?.claudeConfigDir).toBeUndefined();
  });

  it("no matching tab (closed / other window) ⇒ no-op", () => {
    expect(applySessionBound([{ id: "term-9" }], payload())).toBeNull();
  });

  it("already-bound tab is never re-stamped (first writer wins)", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "pinned-earlier" }];
    expect(applySessionBound(tabs, payload())).toBeNull();
  });

  it("malformed payload (missing ids) ⇒ no-op", () => {
    const tabs = [{ id: "term-1" }, { id: "" }];
    expect(applySessionBound(tabs, payload({ sessionId: "" }))).toBeNull();
    expect(applySessionBound(tabs, payload({ terminalId: "" }))).toBeNull();
  });
});
