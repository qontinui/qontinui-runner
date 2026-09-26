import { describe, it, expect } from "vitest";
import {
  dismissKey,
  normalizeBindingGapAsks,
  visibleBindingGapAsks,
  type BindingGapAsk,
} from "./binding-gap-ask-logic";

const T = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

const raw = {
  tenant_id: T,
  detector: "binding_gaps",
  reason: "no headless path can seed this tenant's credential safely",
  message: `This device is bound to tenant ${T} but holds no credential for it.`,
  cta: "device_pair_tenant",
  command: `qontinui_profile device pair --tenant-id ${T}`,
  caveat: "pairing also makes this tenant the device's home tenant",
  first_seen: 1800000000,
};

describe("normalizeBindingGapAsks", () => {
  it("maps the refresher's payload", () => {
    const asks = normalizeBindingGapAsks([raw]);
    expect(asks).toEqual([
      {
        tenantId: T,
        command: raw.command,
        message: raw.message,
        reason: raw.reason,
        caveat: raw.caveat,
        firstSeen: 1800000000,
      },
    ]);
  });

  it("treats a missing or non-array answer as UNKNOWN, never as no asks", () => {
    expect(normalizeBindingGapAsks(null)).toBeNull();
    expect(normalizeBindingGapAsks(undefined)).toBeNull();
    expect(normalizeBindingGapAsks({})).toBeNull();
    expect(normalizeBindingGapAsks([])).toEqual([]);
  });

  it("drops entries the operator could not act on", () => {
    expect(normalizeBindingGapAsks([{ tenant_id: T }, { command: "x" }, 7, null])).toEqual([]);
  });
});

describe("visibleBindingGapAsks", () => {
  const ask = normalizeBindingGapAsks([raw])![0] as BindingGapAsk;

  it("hides an ask dismissed for this lapse, shows a new lapse again", () => {
    expect(visibleBindingGapAsks([ask], new Set())).toEqual([ask]);
    expect(visibleBindingGapAsks([ask], new Set([dismissKey(ask)]))).toEqual([]);
    const newLapse = { ...ask, firstSeen: 1800009999 };
    expect(visibleBindingGapAsks([newLapse], new Set([dismissKey(ask)]))).toEqual([newLapse]);
  });

  it("renders nothing for UNKNOWN", () => {
    expect(visibleBindingGapAsks(null, new Set())).toEqual([]);
  });
});
