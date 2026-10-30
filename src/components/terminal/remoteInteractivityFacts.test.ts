/**
 * The Fleet picker's rendering of coord's interactivity facts (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * A2): ok / failed / unknown / ABSENT each render differently, and nothing a
 * coord did not serve is ever shown as "failed".
 *
 * The rendering decisions are pure (`remoteInteractivityFacts.ts`); the wiring
 * into the component is asserted on its source, following
 * `FleetSessionPicker.wiring.test.ts` — the runner's vitest has no DOM.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

import {
  describeFact,
  describeSurface,
  devicesToProbe,
  FACT_TONE_CLASS,
  servesInteractivity,
  type InteractivityFact,
} from "./remoteInteractivityFacts";

const AGO = (secs: number) => new Date(Date.now() - secs * 1000).toISOString();

function fact(partial: Partial<InteractivityFact>): InteractivityFact {
  return {
    state: "unknown",
    observedAt: null,
    sourceDeviceId: null,
    via: null,
    reason: "unprobed",
    ...partial,
  };
}

describe("describeFact", () => {
  it("renders ok with its age and how it was measured", () => {
    const v = describeFact(
      "read",
      fact({ state: "ok", observedAt: AGO(180), via: "traffic", reason: null }),
    );
    expect(v?.tone).toBe("ok");
    expect(v?.label).toBe("read ok 3m ago");
    expect(v?.title).toContain("via traffic");
    expect(v?.state).toBe("ok");
  });

  it("renders failed WITH the wire refusal code", () => {
    const v = describeFact(
      "write",
      fact({ state: "failed", observedAt: AGO(30), via: "probe", reason: "session_not_local" }),
    );
    expect(v?.tone).toBe("failed");
    expect(v?.label).toBe("write failed: session_not_local");
    expect(v?.title).toContain("FAILED (session_not_local)");
    expect(v?.reason).toBe("session_not_local");
  });

  it("renders unknown distinctly from failed, with its reason", () => {
    const v = describeFact("write", fact({ state: "unknown", reason: "unprobed" }));
    expect(v?.tone).toBe("unknown");
    expect(v?.tone).not.toBe("failed");
    expect(v?.label).toBe("write unknown");
    expect(v?.title).toContain("UNKNOWN — not measured yet");
    expect(FACT_TONE_CLASS.unknown).not.toBe(FACT_TONE_CLASS.failed);
    expect(FACT_TONE_CLASS.unknown).not.toBe(FACT_TONE_CLASS.ok);

    const held = describeFact("read", fact({ reason: "held_by_other_source" }));
    expect(held?.title).toContain("held by another device");
    const unreadable = describeFact("read", fact({ reason: "events_unreadable" }));
    expect(unreadable?.tone).toBe("unknown");
    expect(unreadable?.title).toContain("could not read");
  });

  it("keeps a stale fact's age rather than dropping it", () => {
    const v = describeFact(
      "read",
      fact({ state: "unknown", reason: "stale", observedAt: AGO(2 * 3600), via: "probe" }),
    );
    expect(v?.tone).toBe("unknown");
    expect(v?.label).toBe("read unknown (2h ago)");
    expect(v?.title).toContain("stale");
  });

  it("renders NOTHING for an absent or malformed fact — never failed", () => {
    expect(describeFact("read", undefined)).toBeNull();
    expect(describeFact("read", null)).toBeNull();
    expect(describeFact("read", false)).toBeNull();
    expect(describeFact("read", { state: "broken" })).toBeNull();
  });

  it("shows an unrecognised unknown reason verbatim rather than inventing one", () => {
    const v = describeFact("read", fact({ reason: "some_future_reason" }));
    expect(v?.title).toContain("some_future_reason");
  });
});

describe("describeSurface / servesInteractivity / devicesToProbe", () => {
  it("labels the four surfaces and nothing else", () => {
    expect(describeSurface("runner_pty")).toBe("runner PTY");
    expect(describeSurface("none")).toBe("not interactive");
    expect(describeSurface("not_runner_hosted")).toBe("not runner-hosted");
    expect(describeSurface("unknown")).toBe("surface unknown");
    expect(describeSurface(undefined)).toBeNull();
    expect(describeSurface("toString")).toBeNull();
  });

  it("treats a response without the flag as an older coord", () => {
    expect(servesInteractivity(null)).toBe(false);
    expect(servesInteractivity({})).toBe(false);
    expect(servesInteractivity({ interactivityEventsPresent: true })).toBe(true);
    // `false` is a coord that serves the facts but could not read them now —
    // still a coord that serves them (every fact then says events_unreadable).
    expect(servesInteractivity({ interactivityEventsPresent: false })).toBe(true);
  });

  it("probes remote devices only, once each", () => {
    expect(
      devicesToProbe([
        { deviceId: "me", isCallerDevice: true },
        { deviceId: "a", isCallerDevice: false },
        { deviceId: "a", isCallerDevice: false },
        { deviceId: "b", isCallerDevice: false },
      ]),
    ).toEqual(["a", "b"]);
  });
});

const PICKER = readFileSync(
  fileURLToPath(new URL("./FleetSessionPicker.tsx", import.meta.url)),
  "utf8",
);
const PROBE_HOOK = readFileSync(
  fileURLToPath(new URL("./useInteractivityProbe.ts", import.meta.url)),
  "utf8",
);

describe("picker wiring", () => {
  it("renders both facts per row through the pure helpers, gated on a coord that serves them", () => {
    expect(PICKER).toContain('describeFact("read", s.readableRemotely)');
    expect(PICKER).toContain('describeFact("write", s.writableRemotely)');
    expect(PICKER).toContain("describeSurface(s.interactiveSurface)");
    expect(PICKER).toContain("{interactivityServed &&");
    expect(PICKER).toContain("data-read-state={read?.state}");
    expect(PICKER).toContain("data-write-state={write?.state}");
  });

  it("probes on load with the fleet_view trigger, and only against a coord that records facts", () => {
    expect(PICKER).toContain("useInteractivityProbe(");
    expect(PICKER).toContain("interactivityServed,");
    expect(PROBE_HOOK).toContain(
      'invoke("remote_interactivity_probe", { deviceId: device, trigger: "fleet_view" })',
    );
    expect(PROBE_HOOK).toContain("if (!enabled) return;");
  });
});
