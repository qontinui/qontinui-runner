import { describe, it, expect } from "vitest";

import { formatRelativeActivity } from "./relativeTime";

const NOW = Date.UTC(2026, 6, 29, 12, 0, 0);
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

describe("formatRelativeActivity", () => {
  it("returns null when there is no timestamp", () => {
    // The card renders its own "Not opened yet" copy — the formatter must not
    // invent wording for the absent case, because the detail view words it
    // differently.
    expect(formatRelativeActivity(undefined, NOW)).toBeNull();
    expect(formatRelativeActivity(null, NOW)).toBeNull();
  });

  it("returns null for a non-finite stamp rather than rendering NaN", () => {
    expect(formatRelativeActivity(Number.NaN, NOW)).toBeNull();
    expect(formatRelativeActivity(Number.POSITIVE_INFINITY, NOW)).toBeNull();
  });

  it("clamps a future timestamp to 'just now' instead of a negative age", () => {
    // Clock skew between the Postgres row and the webview is real; an hour of
    // it must not render "-1 hours ago".
    expect(formatRelativeActivity(NOW + 5 * HOUR, NOW)).toBe("just now");
  });

  it("uses coarser buckets as the age grows", () => {
    expect(formatRelativeActivity(NOW - 30_000, NOW)).toBe("just now");
    expect(formatRelativeActivity(NOW - 5 * MIN, NOW)).toBe("5 minutes ago");
    expect(formatRelativeActivity(NOW - 3 * HOUR, NOW)).toBe("3 hours ago");
    expect(formatRelativeActivity(NOW - 2 * DAY, NOW)).toBe("2 days ago");
    expect(formatRelativeActivity(NOW - 20 * DAY, NOW)).toBe("2 weeks ago");
    expect(formatRelativeActivity(NOW - 200 * DAY, NOW)).toBe("6 months ago");
    expect(formatRelativeActivity(NOW - 800 * DAY, NOW)).toBe("2 years ago");
  });

  it("singularises exactly one unit", () => {
    expect(formatRelativeActivity(NOW - 1 * MIN, NOW)).toBe("1 minute ago");
    expect(formatRelativeActivity(NOW - 1 * HOUR, NOW)).toBe("1 hour ago");
    expect(formatRelativeActivity(NOW - 1 * DAY, NOW)).toBe("1 day ago");
    expect(formatRelativeActivity(NOW - 7 * DAY, NOW)).toBe("1 week ago");
  });

  it("does not skip a bucket at the boundaries", () => {
    // 59 minutes is still minutes; 60 becomes an hour. An off-by-one here
    // shows up as "0 hours ago", which is the bug this pins.
    expect(formatRelativeActivity(NOW - 59 * MIN, NOW)).toBe("59 minutes ago");
    expect(formatRelativeActivity(NOW - 60 * MIN, NOW)).toBe("1 hour ago");
    expect(formatRelativeActivity(NOW - 23 * HOUR, NOW)).toBe("23 hours ago");
    expect(formatRelativeActivity(NOW - 24 * HOUR, NOW)).toBe("1 day ago");
    expect(formatRelativeActivity(NOW - 6 * DAY, NOW)).toBe("6 days ago");
  });
});
