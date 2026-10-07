/**
 * `discover` IPC → `FindRequest` (plan
 * 2026-09-20-fleet-tab-attach-loses-the-grant-push-race, item 8).
 *
 * `discover` and `find` both call `bridge.discover()`, whose executor drops
 * elements on `!options.includeHidden`. Only `find` seeded the flag, so the
 * same request answered differently on the two routes.
 */

import { describe, it, expect } from "vitest";

import { toDiscoverRequest } from "./useDiscoveryEvents";

describe("toDiscoverRequest", () => {
  it("seeds includeHidden: true when the caller left it unset", () => {
    expect(toDiscoverRequest({ options: { interactiveOnly: false } })).toEqual({
      includeHidden: true,
      interactiveOnly: false,
    });
  });

  it("lets an explicit includeHidden: false win over the seed", () => {
    expect(toDiscoverRequest({ options: { includeHidden: false } }).includeHidden).toBe(false);
  });

  it("folds the snake_case spelling before the seed is applied", () => {
    expect(toDiscoverRequest({ options: { include_hidden: false } })).toEqual({
      includeHidden: false,
    });
  });

  it("forwards filters beyond the six named ones by identity", () => {
    expect(toDiscoverRequest({ options: { includeMedia: true, testId: "save" } })).toEqual({
      includeHidden: true,
      includeMedia: true,
      testId: "save",
    });
  });

  it("reads the payload root when there is no options wrapper", () => {
    const payload = { requestId: "r1", type: "discover", force: true, text: "Save" };
    expect(toDiscoverRequest(payload)).toEqual({ includeHidden: true, text: "Save" });
  });
});
