import { describe, expect, it } from "vitest";

import { STATUS_SUCCESS_TTL_MS, renderCommandStatus, statusTtlMs } from "./verdict";

describe("statusTtlMs", () => {
  it("expires a bare success verdict (the `/spawn-ai ✓` case)", () => {
    const status = renderCommandStatus("/spawn-ai", undefined);
    expect(status.text).toBe("/spawn-ai ✓");
    expect(statusTtlMs(status.kind)).toBe(STATUS_SUCCESS_TTL_MS);
  });

  it("expires a no-op verdict", () => {
    expect(statusTtlMs("noop")).toBe(STATUS_SUCCESS_TTL_MS);
  });

  it("holds an error verdict until replaced or dismissed", () => {
    expect(statusTtlMs("error")).toBeNull();
  });
});
