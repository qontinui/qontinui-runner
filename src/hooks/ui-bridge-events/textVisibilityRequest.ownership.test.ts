/**
 * Clock-free ownership, against a fake engine whose state the hook can only
 * reach through the SDK's own two calls: `wasHidden` is read BEFORE
 * `setTextVisibility`; `onlyIfVisible` leaves an already-hidden page untouched
 * (no SDK call, so no TTL re-arm); `query` reads, with no page-state change,
 * and answers only after a paint fence.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

let hidden = false;
let devControls = true;
const calls: string[] = [];
const bodies: unknown[] = [];

vi.mock("@qontinui/ui-bridge/core", () => ({ devControlsEnabled: () => devControls }));
vi.mock("@qontinui/ui-bridge/control", () => ({
  textHidden: () => {
    calls.push("textHidden");
    return hidden;
  },
  setTextVisibility: (body: { hidden: boolean }) => {
    calls.push(`set:${body.hidden}`);
    bodies.push(body);
    const changed = hidden !== body.hidden;
    hidden = body.hidden;
    return { success: true, hidden: body.hidden, changed, hiddenAt: 1_000, timestamp: 1_000 };
  },
}));

import { runTextVisibilityRequest } from "./textVisibilityRequest";

const paint = async () => "animation_frame" as const;

beforeEach(() => {
  hidden = false;
  devControls = true;
  calls.length = 0;
  bodies.length = 0;
});

describe("text visibility ownership (clock-free)", () => {
  it("reads wasHidden before the SDK call: a hide this request made says false", async () => {
    const reply = await runTextVisibilityRequest({ hidden: true, onlyIfVisible: true }, paint);
    expect(calls).toEqual(["textHidden", "set:true"]);
    expect(reply).toMatchObject({ success: true, data: { changed: true, wasHidden: false } });
  });

  it("onlyIfVisible on an already-hidden page answers without calling the SDK", async () => {
    hidden = true;
    const reply = await runTextVisibilityRequest({ hidden: true, onlyIfVisible: true }, paint);
    expect(calls).toEqual(["textHidden"]);
    expect(reply).toEqual({
      success: true,
      data: { success: true, hidden: true, changed: false, wasHidden: true },
    });
  });

  it("an explicit re-hide (the direct route, no onlyIfVisible) still reaches the SDK", async () => {
    hidden = true;
    const reply = await runTextVisibilityRequest({ hidden: true }, paint);
    expect(calls).toEqual(["textHidden", "set:true"]);
    expect(reply).toMatchObject({ success: true, data: { changed: false, wasHidden: true } });
  });

  it("never forwards the arm-only fields to the SDK", async () => {
    await runTextVisibilityRequest(
      { requestId: "r", hidden: true, ttlMs: 15000, onlyIfVisible: true },
      paint,
    );
    expect(bodies).toEqual([{ hidden: true, ttlMs: 15000 }]);
  });

  it.each([true, false])(
    "query reads textHidden (%s) around the fence, changing no state",
    async (h) => {
      hidden = h;
      const reply = await runTextVisibilityRequest({ query: true }, paint);
      expect(calls).toEqual(["textHidden", "textHidden"]);
      expect(bodies).toEqual([]);
      expect(reply).toEqual({
        success: true,
        data: { success: true, query: true, hidden: h, paintFence: "animation_frame" },
      });
    },
  );

  it("query answers only after the paint fence, and passes a timed-out fence on", async () => {
    let release: (fence: "animation_frame" | "timeout") => void = () => {};
    const fence = new Promise<"animation_frame" | "timeout">((r) => (release = r));
    let settled = false;
    const pending = runTextVisibilityRequest({ query: true }, () => fence).then((r) => {
      settled = true;
      return r;
    });
    for (let i = 0; i < 5; i++) await Promise.resolve();
    expect(settled).toBe(false);
    release("timeout");
    expect(await pending).toEqual({
      success: true,
      data: { success: true, query: true, hidden: false, paintFence: "timeout" },
    });
  });

  it("query answers hidden: null when the state moved during the fence", async () => {
    hidden = true;
    const reply = await runTextVisibilityRequest({ query: true }, async () => {
      hidden = false; // a restore written while the probe waited
      return "animation_frame";
    });
    expect(reply).toMatchObject({ data: { hidden: null } });
  });

  it("query with dev controls off: nothing can be hidden, no fence is needed", async () => {
    devControls = false;
    const waitForPaint = vi.fn(paint);
    const reply = await runTextVisibilityRequest({ query: true }, waitForPaint);
    expect(waitForPaint).not.toHaveBeenCalled();
    expect(reply).toEqual({
      success: true,
      data: { success: true, query: true, hidden: false, paintFence: "not_needed" },
    });
  });
});
