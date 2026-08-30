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
    providerReported: false,
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

  it("a weaker (observed) bind never re-stamps a tab that already has an id", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "pinned-earlier" }];
    expect(applySessionBound(tabs, payload())).toBeNull();
  });

  it("a reconciled bind never re-stamps either — inference must not clobber", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "pinned-earlier" }];
    expect(applySessionBound(tabs, payload({ origin: "reconciled" }))).toBeNull();
  });

  /**
   * The defect this branch exists for, measured live 2026-08-29 on the primary
   * runner: the tab held the spawn-time PREDICTION `a20acdbb…` while the PTY
   * behind it was authoritatively bound to `44aadb3e…`. The dropdown reads
   * `tab.claudeSessionId`, so it asked about an id no record was ever written
   * under and rendered `unavailable — session_not_found` for a session with a
   * complete 5-opened/5-landed ledger. Bailing on mere PRESENCE made that
   * prediction permanent.
   */
  it("a PROVIDER-REPORTED bind corrects a stale predicted id", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "a20acdbb-predicted" }];
    expect(
      applySessionBound(tabs, payload({ origin: "authoritative", providerReported: true })),
    ).toEqual({
      tabId: "term-1",
      claudeSessionId: "sess-1",
      claudeConfigDir: "C:/claude/.claude-gmail",
    });
  });

  /**
   * The account must survive the correction. `record_session_open_into` keeps a
   * known `config_dir` when the provider's hook omits one — the hook does not
   * always report an account — so a bind can legitimately arrive with an empty
   * `configDir`. The listener applies this update by SPREAD, so passing
   * `undefined` through would blank a populated `claudeConfigDir` and
   * `rememberSessionId` would persist the blank. Harmless while a bind could
   * only stamp an unbound tab; reachable the moment a provider-reported bind
   * can correct a populated one.
   */
  it("a correction with no configDir keeps the account the tab already knew", () => {
    const tabs = [
      {
        id: "term-1",
        claudeSessionId: "a20acdbb-predicted",
        claudeConfigDir: "C:/claude/.claude-gmail",
      },
    ];
    expect(
      applySessionBound(tabs, payload({ configDir: "", providerReported: true }))?.claudeConfigDir,
    ).toBe("C:/claude/.claude-gmail");
  });

  it("a correction that DOES carry an account still moves it — that is the fix working", () => {
    const tabs = [
      {
        id: "term-1",
        claudeSessionId: "a20acdbb-predicted",
        claudeConfigDir: "C:/claude/.claude-gmail",
      },
    ];
    expect(
      applySessionBound(
        tabs,
        payload({ configDir: "C:/claude/.claude-paktis", providerReported: true }),
      )?.claudeConfigDir,
    ).toBe("C:/claude/.claude-paktis");
  });

  /**
   * The hazard that makes `providerReported` — not the `origin` grade — the
   * gate. Reconcile's rung-2 bind lifts its id from the anchor process's typed
   * `--session-id`, which IS the runner's prediction, yet grades it
   * `authoritative`. Gating corrections on grade would let that bind overwrite
   * a true id with the guess, reinstating the same defect in the opposite
   * direction.
   */
  it("an inferred 'authoritative' bind (reconcile rung 2) must NOT overwrite a tab id", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "true-id-from-hook" }];
    expect(applySessionBound(tabs, payload({ origin: "authoritative" }))).toBeNull();
    expect(
      applySessionBound(tabs, payload({ origin: "authoritative", providerReported: false })),
    ).toBeNull();
  });

  it("an agreeing bind is a no-op at every grade, provider-reported or not", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "sess-1" }];
    for (const origin of ["observed", "reconciled", "authoritative"]) {
      expect(applySessionBound(tabs, payload({ origin }))).toBeNull();
      expect(applySessionBound(tabs, payload({ origin, providerReported: true }))).toBeNull();
    }
  });

  it("an UNBOUND tab is still stamped by any grade, provider-reported or not", () => {
    const tabs = [{ id: "term-1" }];
    for (const origin of ["observed", "reconciled", "authoritative"]) {
      expect(applySessionBound(tabs, payload({ origin }))?.claudeSessionId).toBe("sess-1");
    }
  });

  /**
   * Guards the wire contract with `emit_session_bound_for_open` in
   * `install_effects_producer/mod.rs`. An omitted field (an older backend, or a
   * serde rename that missed this side) must read as the SAFE value — no
   * correction — rather than silently licensing one.
   */
  it("a missing providerReported is treated as false, never as permission", () => {
    const tabs = [{ id: "term-1", claudeSessionId: "stale" }];
    const p = payload({ origin: "authoritative" });
    delete (p as { providerReported?: boolean }).providerReported;
    expect(applySessionBound(tabs, p)).toBeNull();
  });

  it("malformed payload (missing ids) ⇒ no-op", () => {
    const tabs = [{ id: "term-1" }, { id: "" }];
    expect(applySessionBound(tabs, payload({ sessionId: "" }))).toBeNull();
    expect(applySessionBound(tabs, payload({ terminalId: "" }))).toBeNull();
  });
});
