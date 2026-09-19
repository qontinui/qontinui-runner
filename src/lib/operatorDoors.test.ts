import { describe, it, expect, vi, beforeEach } from "vitest";

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import { doorError, invokeOperatorDoor, toOperatorDoorReply } from "./operatorDoors";

describe("operator doors", () => {
  beforeEach(() => mockInvoke.mockReset());

  it("passes the command and args through and mirrors Response.ok", async () => {
    mockInvoke.mockResolvedValueOnce({ status: 200, body: { success: true } });
    const reply = await invokeOperatorDoor("operator_run_unified_workflow", {
      id: "wf",
      request: { monitor_index: 0 },
    });
    expect(mockInvoke).toHaveBeenCalledWith("operator_run_unified_workflow", {
      id: "wf",
      request: { monitor_index: 0 },
    });
    expect(reply).toEqual({ status: 200, ok: true, body: { success: true } });
  });

  it("keeps a refusal's status and code", () => {
    const reply = toOperatorDoorReply({
      status: 409,
      body: { success: false, error: "coord has drained this device", code: "device_drained" },
    });
    expect(reply.ok).toBe(false);
    expect(doorError(reply.body)).toBe("coord has drained this device");
  });

  it("reads no error from a body without one", () => {
    expect(doorError({ success: true })).toBeUndefined();
    expect(doorError(null)).toBeUndefined();
  });
});
