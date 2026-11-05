/**
 * Which raw `localStorage` keys belong to THIS instance under a prefix — the
 * match `removeByPrefix` and `entriesByPrefix` (the page prune's Undo) use.
 */

import { describe, expect, it } from "vitest";
import { isOwnKeyUnderPrefix } from "./instance-storage";

describe("isOwnKeyUnderPrefix", () => {
  const prefix = "page:abc:";

  it("on the primary (9876), takes bare keys and leaves other instances' keys", () => {
    expect(isOwnKeyUnderPrefix("page:abc:qontinui-zone-layout", prefix, 9876)).toBe(true);
    expect(isOwnKeyUnderPrefix("page:abc:qontinui-zone-layout:9877", prefix, 9876)).toBe(false);
  });

  it("on a secondary, takes its own port's keys only", () => {
    expect(isOwnKeyUnderPrefix("page:abc:qontinui-zone-layout:9877", prefix, 9877)).toBe(true);
    expect(isOwnKeyUnderPrefix("page:abc:qontinui-zone-layout:9878", prefix, 9877)).toBe(false);
    expect(isOwnKeyUnderPrefix("page:abc:qontinui-zone-layout", prefix, 9877)).toBe(false);
  });

  it("matches a prefix only at the start of the key", () => {
    expect(isOwnKeyUnderPrefix("x-page:abc:layout", prefix, 9876)).toBe(false);
    expect(isOwnKeyUnderPrefix("page:abcd:layout", prefix, 9876)).toBe(false);
  });
});
