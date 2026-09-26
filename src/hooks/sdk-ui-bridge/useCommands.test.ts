/**
 * T1 / T2 of plan
 * 2026-09-10-two-runner-call-sites-still-report-success-for-an-action-that-did-not-happen:
 * `executeAction` and `sendCommand` read the relay reply through the strict
 * reader, so a reply with no `success` key is a failure.
 *
 * The runner's vitest runs in `environment: "node"` with no renderHook
 * harness, so React's hooks are stubbed to plain pass-throughs and the hook is
 * called directly — the code under test is the real `useCommands` body.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("react", () => ({
  useState: (init: unknown) => [init, () => {}],
  useCallback: (fn: unknown) => fn,
  useRef: (init: unknown) => ({ current: init }),
}));

const tracedFetch = vi.fn();
vi.mock("@/lib/runner-api", () => ({
  getApiBase: () => "http://127.0.0.1:9876",
  tracedFetch: (...args: unknown[]) => tracedFetch(...args),
}));

import { useCommands } from "./useCommands";
import { INDETERMINATE_ACTION_ERROR } from "./actionOutcome";

function reply(body: unknown, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

function useHookUnderTest() {
  return useCommands([], async () => {}, { current: null });
}

beforeEach(() => tracedFetch.mockReset());

describe("useCommands.executeAction", () => {
  it("a reply with NO success key is a failure (was success via `!== false`)", async () => {
    tracedFetch.mockResolvedValue(reply({}));
    const r = await useHookUnderTest().executeAction("btn", "click");
    expect(r.success).toBe(false);
    expect(r.outcome).toBe("indeterminate");
    expect(r.error).toBe(INDETERMINATE_ACTION_ERROR);
  });

  it("explicit success:true succeeds; success:false fails with its error", async () => {
    tracedFetch.mockResolvedValue(reply({ success: true, data: { a: 1 } }));
    const good = await useHookUnderTest().executeAction("btn", "click");
    expect(good.success).toBe(true);
    expect(good.data).toEqual({ a: 1 });

    tracedFetch.mockResolvedValue(reply({ success: false, error: "gone" }));
    const bad = await useHookUnderTest().executeAction("btn", "click");
    expect(bad.success).toBe(false);
    expect(bad.error).toBe("gone");
  });
});

describe("useCommands.sendCommand", () => {
  it("executeAction: a reply with NO success key is a failure, body kept as data", async () => {
    tracedFetch.mockResolvedValue(reply({}));
    const r = await useHookUnderTest().sendCommand("executeAction", { elementId: "b" });
    expect(r.success).toBe(false);
    expect(r.outcome).toBe("indeterminate");
    expect(r.data).toEqual({});
  });

  it("aiExecute is an action route too: no success key is a failure", async () => {
    tracedFetch.mockResolvedValue(reply({ executedAction: "click" }));
    const r = await useHookUnderTest().sendCommand("aiExecute", { instruction: "x" });
    expect(r.success).toBe(false);
  });

  it("a READ (getSnapshot) with raw app JSON and no success key succeeds", async () => {
    tracedFetch.mockResolvedValue(reply({ elements: [] }));
    const r = await useHookUnderTest().sendCommand("getSnapshot");
    expect(r.success).toBe(true);
    expect(r.data).toEqual({ elements: [] });
  });

  it("a READ fails on an explicit success:false or a non-2xx", async () => {
    tracedFetch.mockResolvedValue(reply({ success: false, error: "no app" }));
    const bad = await useHookUnderTest().sendCommand("getSnapshot");
    expect(bad.success).toBe(false);
    expect(bad.error).toBe("no app");
    tracedFetch.mockResolvedValue(reply({ elements: [] }, 503));
    expect((await useHookUnderTest().sendCommand("getElements")).success).toBe(false);
  });

  it("explicit success:true succeeds", async () => {
    tracedFetch.mockResolvedValue(reply({ success: true, data: [1] }));
    const r = await useHookUnderTest().sendCommand("getElements");
    expect(r.success).toBe(true);
    expect(r.data).toEqual([1]);
  });
});
