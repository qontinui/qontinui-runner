/**
 * Clock-free ownership, against a fake engine whose state the hook can only
 * reach through the SDK's own two calls: `wasHidden` is read BEFORE
 * `setTextVisibility`; `onlyIfVisible` leaves an already-hidden page untouched
 * (no SDK call, so no TTL re-arm); `query` reads, with no page-state change,
 * and waits for a paint fence only when a restore may still be unpainted.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";

let hidden = false;
let devControls = true;
let lastRestore: { restoredAt: number } | null = null;
/** When set, the show's answer verbatim: an SDK read the hook must not trust. */
let showOverride: unknown = undefined;
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
    if (!body.hidden && showOverride !== undefined) return showOverride;
    if (!body.hidden) {
      // The SDK's show: a real restore when text was hidden, else the last one.
      if (changed) lastRestore = { restoredAt: Date.now() };
      return { success: true, hidden: false, changed, restore: lastRestore, timestamp: 1_000 };
    }
    return { success: true, hidden: true, changed, hiddenAt: 1_000, timestamp: 1_000 };
  },
}));

import { RECENT_RESTORE_MS, runTextVisibilityRequest } from "./textVisibilityRequest";

const paint = async () => "animation_frame" as const;

beforeEach(() => {
  hidden = false;
  devControls = true;
  lastRestore = null;
  showOverride = undefined;
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

  it("query on a hidden page reads textHidden around the fence, changing no state", async () => {
    hidden = true;
    const reply = await runTextVisibilityRequest({ query: true }, paint);
    expect(calls).toEqual(["textHidden", "textHidden"]);
    expect(bodies).toEqual([]);
    expect(reply).toEqual({
      success: true,
      data: { success: true, query: true, hidden: true, paintFence: "animation_frame" },
    });
  });

  it.each([
    ["no restore ever", null],
    ["a restore older than the window", -(RECENT_RESTORE_MS + 1)],
  ])("query with %s answers at once, with no fence to wait for", async (_, age) => {
    if (age !== null) lastRestore = { restoredAt: Date.now() + age };
    const waitForPaint = vi.fn(paint);
    const reply = await runTextVisibilityRequest({ query: true }, waitForPaint);
    expect(waitForPaint).not.toHaveBeenCalled();
    // The last restore is read through the SDK's no-op show: nothing written.
    expect(calls).toEqual(["textHidden", "set:false"]);
    expect(hidden).toBe(false);
    expect(reply).toEqual({
      success: true,
      data: { success: true, query: true, hidden: false, paintFence: "not_needed" },
    });
  });

  it.each([
    ["a refused read", { success: false, code: "INVALID_REQUEST", error: "x" }],
    [
      "a read that changed the page",
      { success: true, hidden: false, changed: true, restore: null },
    ],
    ["a restore with no restoredAt", { success: true, hidden: false, changed: false, restore: {} }],
    [
      "a NaN restoredAt",
      { success: true, hidden: false, changed: false, restore: { restoredAt: Number.NaN } },
    ],
  ])("query fails closed on %s: it waits for the fence", async (_, read) => {
    showOverride = read;
    const waitForPaint = vi.fn(paint);
    const reply = await runTextVisibilityRequest({ query: true }, waitForPaint);
    expect(waitForPaint).toHaveBeenCalledTimes(1);
    expect(reply).toMatchObject({ data: { paintFence: "animation_frame" } });
  });

  it("query after a recent restore waits for the fence, and passes a timed-out one on", async () => {
    lastRestore = { restoredAt: Date.now() - 100 };
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

  it("query answers hidden: null when the state differs before and after the fence", async () => {
    hidden = true;
    const reply = await runTextVisibilityRequest({ query: true }, async () => {
      hidden = false; // a restore written while the probe waited
      return "animation_frame";
    });
    expect(reply).toMatchObject({ data: { hidden: null } });
  });

  it.each([false, true])(
    "query with dev controls off answers textHidden() (%s) with no fence",
    async (h) => {
      devControls = false;
      hidden = h;
      const waitForPaint = vi.fn(paint);
      const reply = await runTextVisibilityRequest({ query: true }, waitForPaint);
      expect(waitForPaint).not.toHaveBeenCalled();
      expect(bodies).toEqual([]);
      expect(reply).toEqual({
        success: true,
        data: {
          success: true,
          query: true,
          hidden: h,
          paintFence: "not_needed",
          devControls: false,
        },
      });
    },
  );
});
