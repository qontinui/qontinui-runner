/**
 * `discover` IPC → `FindRequest` (plan
 * 2026-09-20-fleet-tab-attach-loses-the-grant-push-race, item 8).
 *
 * `discover` and `find` both call `bridge.discover()`, whose executor drops
 * elements on `!options.includeHidden`. Only `find` seeded the flag, so the
 * same request answered differently on the two routes.
 */

import { describe, it, expect } from "vitest";

import { toDiscoverRequest, toFindArmRequest } from "./useDiscoveryEvents";

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

/**
 * An explicit `null` is "unset", not "false". `toFindRequest` forwards it by
 * identity and the SDK reads it through `!options.includeHidden`, so before the
 * default treated it as unset `/control/find` dropped hidden elements for
 * `{"includeHidden": null}` while `/control/discover` did not.
 */
describe("includeHidden: null defaults like an unset flag", () => {
  it("on the discover arm, for either spelling", () => {
    expect(toDiscoverRequest({ options: { includeHidden: null } }).includeHidden).toBe(true);
    expect(toDiscoverRequest({ options: { include_hidden: null } }).includeHidden).toBe(true);
  });

  it("on the find arm, for either spelling", () => {
    expect(toFindArmRequest({ includeHidden: null, text: "Save" })).toEqual({
      includeHidden: true,
      text: "Save",
    });
    expect(toFindArmRequest({ include_hidden: null }).includeHidden).toBe(true);
  });

  it("lets a snake_case false fill a camelCase null rather than lose to it", () => {
    expect(toFindArmRequest({ includeHidden: null, include_hidden: false }).includeHidden).toBe(
      false,
    );
  });

  it("still lets an explicit false win on the find arm", () => {
    expect(toFindArmRequest({ params: { includeHidden: false } }).includeHidden).toBe(false);
  });

  it("seeds the find arm when the flag is absent and drops the envelope", () => {
    expect(toFindArmRequest({ requestId: "r1", type: "find", role: "button" })).toEqual({
      includeHidden: true,
      role: "button",
    });
  });
});
