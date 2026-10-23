/**
 * The reply's ownership stamps: `receivedAt` is taken BEFORE the SDK call (so a
 * hide this request creates has `hiddenAt >= receivedAt`), and `answeredAt`
 * after the paint wait. Both from the webview's own `Date.now()`.
 */

import { describe, it, expect, vi, afterEach } from "vitest";

let clock = 0;
const sdkCalls: number[] = [];

vi.mock("@qontinui/ui-bridge/core", () => ({ devControlsEnabled: () => true }));
vi.mock("@qontinui/ui-bridge/control", () => ({
  // A hide that creates its state now, stamped on the same clock.
  setTextVisibility: () => {
    const hiddenAt = Date.now();
    sdkCalls.push(hiddenAt);
    return { success: true, hidden: true, changed: true, hiddenAt, timestamp: hiddenAt };
  },
}));

import { runTextVisibilityRequest } from "./textVisibilityRequest";

afterEach(() => {
  vi.restoreAllMocks();
  sdkCalls.length = 0;
});

describe("text visibility reply stamps", () => {
  it("stamps receivedAt before the SDK call and answeredAt after the paint wait", async () => {
    clock = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => ++clock);
    let paintedAt = 0;
    const reply = await runTextVisibilityRequest({ hidden: true }, async () => {
      paintedAt = Date.now();
      return "animation_frame";
    });
    const data = reply.data as { receivedAt: number; answeredAt: number; hiddenAt: number };
    expect(sdkCalls).toHaveLength(1);
    expect(data.receivedAt).toBeLessThan(data.hiddenAt);
    expect(data.answeredAt).toBeGreaterThan(paintedAt);
    expect(paintedAt).toBeGreaterThan(data.hiddenAt);
  });
});
