import { describe, it, expect, vi } from "vitest";
import { writePtyById, uint8ToBase64 } from "./writePtyById";
import { TERMINAL_EXITED, TERMINAL_WRITE_FAILED } from "./terminalWriteResult";

describe("writePtyById", () => {
  it("writes by id with no mounted pane and reports the byte count", async () => {
    const invoker = vi.fn(async () => ({ success: true }));
    const res = await writePtyById("abc", "hi\n", null, invoker);
    expect(res).toEqual({ success: true, bytes: 3 });
    expect(invoker).toHaveBeenCalledWith("terminal_write", {
      terminalId: "abc",
      data: uint8ToBase64(new TextEncoder().encode("hi\n")),
    });
  });

  it("refuses a write to an exited pane WITHOUT touching the IPC", async () => {
    // The negative case that matters: a virtualized pane's automation action
    // must not answer `success: true` for input that reached no process.
    const invoker = vi.fn(async () => ({ success: true }));
    const res = await writePtyById("abc", "hi", { exitCode: 137 }, invoker);
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.code).toBe(TERMINAL_EXITED);
      expect(res.exitCode).toBe(137);
    }
    expect(invoker).not.toHaveBeenCalled();
  });

  it("distinguishes an IPC failure on a live pane from an exited one", async () => {
    const invoker = () => Promise.reject(new Error("channel closed"));
    const res = await writePtyById("abc", "hi", null, invoker);
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.code).toBe(TERMINAL_WRITE_FAILED);
      expect(res.error).toContain("channel closed");
      expect(res.hint).toContain("IPC/backend failure");
    }
  });

  it("base64-encodes a large payload without a stack overflow", () => {
    const big = new Uint8Array(200_000).fill(65);
    expect(uint8ToBase64(big)).toBe(btoa("A".repeat(200_000)));
  });
});
