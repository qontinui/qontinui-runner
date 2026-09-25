import { describe, expect, it } from "vitest";

import {
  UNPAIRED_ERROR,
  UnpairedError,
  bearerFromDeviceToken,
  deviceTokenArgs,
} from "./ciRunnerDeviceToken";

const A = "6b1f4b0e-1111-4000-8000-000000000001";
const B = "91ffaa20-2222-4000-8000-000000000002";

describe("deviceTokenArgs (R2)", () => {
  it("names the default tenant only when the runner holds more than one", () => {
    expect(deviceTokenArgs([A, B], A)).toEqual({ tenantId: A });
  });

  it("omits the tenant on a single-tenant or unknown-candidates runner", () => {
    expect(deviceTokenArgs([A], A)).toEqual({});
    expect(deviceTokenArgs([], A)).toEqual({});
  });

  it("omits the tenant when no default is known yet", () => {
    expect(deviceTokenArgs([A, B], null)).toEqual({});
  });
});

describe("bearerFromDeviceToken (R2)", () => {
  it("returns the bearer header for a token", () => {
    expect(bearerFromDeviceToken("h.p.s", {})).toBe("Bearer h.p.s");
  });

  it("keeps the unpaired message for a null answer to a tenant-less call", () => {
    expect(() => bearerFromDeviceToken(null, {})).toThrow(UnpairedError);
    expect(() => bearerFromDeviceToken(null, {})).toThrow(UNPAIRED_ERROR);
  });

  it("names the tenant for a null answer to a named-tenant call", () => {
    let err: unknown;
    try {
      bearerFromDeviceToken(null, { tenantId: B });
    } catch (e) {
      err = e;
    }
    expect(err).toBeInstanceOf(UnpairedError);
    expect((err as Error).message).toContain(`no usable coord credential for tenant ${B}`);
    expect((err as Error).message).not.toBe(UNPAIRED_ERROR);
  });
});
