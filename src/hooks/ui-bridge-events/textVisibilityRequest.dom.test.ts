// @vitest-environment jsdom
/**
 * The opted-in arm against a real (jsdom) document: a hide that changes the
 * page waits for the paint fence before it answers, hides the glyphs, and the
 * restore puts the page back exactly.
 */

import { describe, it, expect, afterEach, vi } from "vitest";
import { enableDevControls } from "@qontinui/ui-bridge/core";
import { textHidden } from "@qontinui/ui-bridge/control";
import { PAINT_FENCE_TIMEOUT_MS, runTextVisibilityRequest } from "./textVisibilityRequest";

const KEY = Symbol.for("@qontinui/ui-bridge/dev-controls");

afterEach(async () => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  // Never leave the shared hide state behind for another test.
  await runTextVisibilityRequest({ hidden: false }, async () => "timeout");
  delete (globalThis as unknown as Record<symbol, unknown>)[KEY];
  document.body.innerHTML = "";
});

describe("text visibility in the runner webview (DOM)", () => {
  it("hides, waits for a paint, and restores the page exactly", async () => {
    enableDevControls("provider-feature");
    document.body.innerHTML = '<h1 style="color: red">Hero heading</h1>';
    const heading = document.querySelector("h1") as HTMLElement;
    const before = heading.getAttribute("style");

    const order: string[] = [];
    const waitForPaint = vi.fn(async () => {
      order.push(`paint:${heading.style.getPropertyValue("color")}`);
      return "animation_frame" as const;
    });

    const hide = await runTextVisibilityRequest(
      { requestId: "r", type: "text_visibility", hidden: true, ttlMs: 15000 },
      waitForPaint,
    );
    expect(hide).toMatchObject({
      success: true,
      data: { success: true, hidden: true, changed: true, paintFence: "animation_frame" },
    });
    // The fence ran AFTER the styles were written.
    expect(order).toEqual(["paint:transparent"]);

    const again = await runTextVisibilityRequest({ hidden: true }, waitForPaint);
    expect(again).toMatchObject({ success: true, data: { changed: false } });
    expect(again.data).not.toHaveProperty("paintFence");
    expect(waitForPaint).toHaveBeenCalledTimes(1);

    const show = await runTextVisibilityRequest({ hidden: false }, waitForPaint);
    expect(show).toMatchObject({
      success: true,
      data: { hidden: false, changed: true, restore: { verified: true } },
    });
    expect(heading.getAttribute("style")).toBe(before);
    expect(waitForPaint).toHaveBeenCalledTimes(2);
  });

  it("an explicit re-hide keeps the first hide's hiddenAt and reports wasHidden", async () => {
    enableDevControls("provider-feature");
    document.body.innerHTML = "<p>Body copy</p>";
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    const first = await runTextVisibilityRequest({ hidden: true }, async () => "timeout");
    expect(first).toMatchObject({ data: { changed: true, wasHidden: false, hiddenAt: 1_000 } });

    // A later millisecond: the re-hide (the direct route's, no onlyIfVisible)
    // reaches the engine, which keeps the original hiddenAt.
    now.mockReturnValue(5_000);
    const again = await runTextVisibilityRequest({ hidden: true }, async () => "timeout");
    expect(again).toMatchObject({
      success: true,
      data: { hidden: true, changed: false, wasHidden: true, hiddenAt: 1_000 },
    });
    expect((again.data as { hiddenAt: number }).hiddenAt).toBeLessThan(Date.now());
  });

  it("the arm's onlyIfVisible on a hidden page leaves that hide's TTL alone", async () => {
    vi.useFakeTimers();
    enableDevControls("provider-feature");
    document.body.innerHTML = "<p>Body copy</p>";
    // Another caller's hide, 1 s TTL.
    await runTextVisibilityRequest({ hidden: true, ttlMs: 1_000 }, async () => "timeout");
    vi.advanceTimersByTime(600);

    const probe = await runTextVisibilityRequest({ query: true }, async () => "animation_frame");
    expect(probe).toEqual({
      success: true,
      data: { success: true, query: true, hidden: true, paintFence: "animation_frame" },
    });
    const arm = await runTextVisibilityRequest(
      { hidden: true, ttlMs: 15_000, onlyIfVisible: true },
      async () => "timeout",
    );
    expect(arm).toEqual({
      success: true,
      data: { success: true, hidden: true, changed: false, wasHidden: true },
    });

    // The other caller's TTL fires when it always would have: not re-armed.
    vi.advanceTimersByTime(500);
    expect(textHidden()).toBe(false);
  });

  it("the probe reads the real engine's last restore: recent waits, old does not", async () => {
    enableDevControls("provider-feature");
    document.body.innerHTML = "<p>Body copy</p>";
    const now = vi.spyOn(Date, "now").mockReturnValue(10_000);
    await runTextVisibilityRequest({ hidden: true }, async () => "timeout");
    await runTextVisibilityRequest({ hidden: false }, async () => "timeout");

    now.mockReturnValue(10_100);
    const recent = vi.fn(async () => "animation_frame" as const);
    const waited = await runTextVisibilityRequest({ query: true }, recent);
    expect(recent).toHaveBeenCalledTimes(1);
    expect(waited).toMatchObject({ data: { hidden: false, paintFence: "animation_frame" } });

    now.mockReturnValue(10_000 + PAINT_FENCE_TIMEOUT_MS + 1);
    const old = vi.fn(async () => "animation_frame" as const);
    const atOnce = await runTextVisibilityRequest({ query: true }, old);
    expect(old).not.toHaveBeenCalled();
    expect(atOnce).toMatchObject({ data: { hidden: false, paintFence: "not_needed" } });
    expect(textHidden()).toBe(false);
  });
});
