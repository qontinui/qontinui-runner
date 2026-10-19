/**
 * The runner-webview half of the media override answers through the SDK's
 * dev-controls gate: refused (typed) until the realm opts in — the state of
 * every runner, since every runner is an exe-mode build — and served by the
 * SDK engine once it has. Node environment (no DOM), so the opted-in arm is exercised through
 * the two commands that need no document: list and restore-all.
 */

import { describe, it, expect, afterEach } from "vitest";
import { enableDevControls } from "@qontinui/ui-bridge/core";
import {
  DEV_CONTROLS_REFUSAL,
  overrideBody,
  runMediaOverrideRequest,
} from "./mediaOverrideRequest";

const KEY = Symbol.for("@qontinui/ui-bridge/dev-controls");

afterEach(() => {
  delete (globalThis as unknown as Record<symbol, unknown>)[KEY];
});

describe("media override in the runner webview", () => {
  it("strips only the envelope metadata from the body", () => {
    expect(
      overrideBody({
        requestId: "r1",
        type: "media_override_apply",
        windowLabel: "main",
        slotId: "hero",
        src: "/c.png",
      }),
    ).toEqual({ slotId: "hero", src: "/c.png" });
  });

  it.each(["media_override_apply", "media_override_restore", "media_override_list"] as const)(
    "%s is refused, typed, while dev controls are off (every runner)",
    async (type) => {
      const reply = await runMediaOverrideRequest(type, { slotId: "hero", src: "/c.png" });
      expect(reply.success).toBe(false);
      expect(reply.data).toMatchObject({
        code: DEV_CONTROLS_REFUSAL.code,
        reason: "dev_controls_disabled",
      });
    },
  );

  it("once opted in, list and restore reach the SDK engine", async () => {
    enableDevControls("provider-feature");
    const list = await runMediaOverrideRequest("media_override_list", {});
    expect(list).toMatchObject({ success: true, data: { success: true, active: [] } });
    const restore = await runMediaOverrideRequest("media_override_restore", { requestId: "x" });
    expect(restore).toMatchObject({ success: true, data: { restored: [], remaining: 0 } });
  });
});
