/**
 * The runner-webview half of text visibility answers through the SDK's
 * dev-controls gate: refused (typed) until the realm opts in — the state of
 * every runner, since every runner is an exe-mode build — and served by the SDK
 * engine once it has. Node environment (no DOM), so the opted-in arm is
 * exercised through the engine's own answer to a page with no document, and
 * through a restore of a page that was never hidden.
 */

import { describe, it, expect, afterEach, vi } from "vitest";
import { enableDevControls } from "@qontinui/ui-bridge/core";
import {
  DEV_CONTROLS_REFUSAL,
  nextPaint,
  PAINT_FENCE_TIMEOUT_MS,
  runTextVisibilityRequest,
  textVisibilityBody,
} from "./textVisibilityRequest";

const KEY = Symbol.for("@qontinui/ui-bridge/dev-controls");

afterEach(() => {
  delete (globalThis as unknown as Record<symbol, unknown>)[KEY];
  vi.useRealTimers();
});

describe("text visibility in the runner webview", () => {
  it("strips only the envelope metadata from the body", () => {
    expect(
      textVisibilityBody({
        requestId: "r1",
        type: "text_visibility",
        windowLabel: "main",
        hidden: true,
        ttlMs: 15000,
      }),
    ).toEqual({ hidden: true, ttlMs: 15000 });
  });

  it.each([{ hidden: true }, { hidden: false }])(
    "%o is refused, typed, while dev controls are off (every runner)",
    async (body) => {
      const waitForPaint = vi.fn();
      const reply = await runTextVisibilityRequest(body, waitForPaint);
      expect(reply.success).toBe(false);
      // Spelled as literals: the wire values are the contract a caller branches on.
      expect(reply.data).toEqual({ ...DEV_CONTROLS_REFUSAL });
      expect(reply.data).toMatchObject({
        code: "capability_unavailable_in_build",
        reason: "dev_controls_disabled",
      });
      expect(waitForPaint).not.toHaveBeenCalled();
    },
  );

  it("once opted in, the SDK engine answers: its own validation and its restore", async () => {
    enableDevControls("provider-feature");
    const waitForPaint = vi.fn(async () => "animation_frame" as const);

    const invalid = await runTextVisibilityRequest({ requestId: "x", hidden: "yes" }, waitForPaint);
    expect(invalid).toMatchObject({ success: false, data: { code: "INVALID_REQUEST" } });

    // Nothing is hidden, so a restore changes nothing and waits for no paint.
    const restore = await runTextVisibilityRequest({ hidden: false }, waitForPaint);
    expect(restore).toMatchObject({
      success: true,
      data: { success: true, hidden: false, changed: false },
    });
    expect(waitForPaint).not.toHaveBeenCalled();

    // No document in this realm: the engine's typed failure, not a throw.
    const hide = await runTextVisibilityRequest({ hidden: true }, waitForPaint);
    expect(hide).toMatchObject({ success: false, data: { code: "NO_DOCUMENT" } });
  });

  it("the paint fence falls back to its timeout where no frame is painted", async () => {
    vi.useFakeTimers();
    const fence = nextPaint();
    vi.advanceTimersByTime(PAINT_FENCE_TIMEOUT_MS);
    await expect(fence).resolves.toBe("timeout");
  });

  it("the paint fence resolves on the second animation frame", async () => {
    const frames: Array<() => void> = [];
    vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
      frames.push(cb);
      return frames.length;
    });
    try {
      const fence = nextPaint();
      frames.shift()?.();
      frames.shift()?.();
      await expect(fence).resolves.toBe("animation_frame");
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
