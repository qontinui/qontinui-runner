// @vitest-environment jsdom
/**
 * The opted-in arm against a real (jsdom) document: a hide that changes the
 * page waits for the paint fence before it answers, hides the glyphs, and the
 * restore puts the page back exactly.
 */

import { describe, it, expect, afterEach, vi } from "vitest";
import { enableDevControls } from "@qontinui/ui-bridge/core";
import { runTextVisibilityRequest } from "./textVisibilityRequest";

const KEY = Symbol.for("@qontinui/ui-bridge/dev-controls");

afterEach(async () => {
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
});
