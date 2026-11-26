/**
 * `answerPing` — the pong leg that must not depend on the runner's `app-rt`
 * (plan 2026-10-19-runner-ui-wedge-signal-fires-when-the-app-runtime-is-starved-by-process-spawns).
 *
 * A JS `emit` is an async Tauri command, run on the Tauri async runtime, which
 * the runner pins to `app-rt`. When `app-rt` starves, that promise never
 * settles. These tests pin that the HTTP leg answers first and that a hung
 * emit can never swallow the pong.
 */
import { describe, it, expect, vi } from "vitest";
import { answerPing } from "./utils";

describe("answerPing", () => {
  it("answers over HTTP with event provenance and never touches the Tauri emit when HTTP lands", async () => {
    const http = vi.fn().mockResolvedValue(true);
    const emit = vi.fn();
    await expect(answerPing("main", emit, http)).resolves.toBe("http");
    expect(http).toHaveBeenCalledWith("event", "main");
    expect(emit).not.toHaveBeenCalled();
  });

  it("is not held by an emit that never settles (the starved app-rt case)", async () => {
    const http = vi.fn().mockResolvedValue(true);
    const neverSettles = vi.fn(() => new Promise<void>(() => {}));
    const result = await Promise.race([
      answerPing("main", neverSettles, http),
      new Promise((resolve) => setTimeout(() => resolve("held"), 200)),
    ]);
    expect(result).toBe("http");
  });

  it("falls back to the Tauri event, labeled, when HTTP fails", async () => {
    const http = vi.fn().mockResolvedValue(false);
    const emit = vi.fn().mockResolvedValue(undefined);
    await expect(answerPing("terminal-1", emit, http)).resolves.toBe("event");
    expect(http.mock.invocationCallOrder[0]).toBeLessThan(emit.mock.invocationCallOrder[0]);
    expect(emit.mock.calls[0][0]).toMatchObject({ label: "terminal-1" });
  });

  it("falls back to the Tauri event when the HTTP leg never settles (a stalled API runtime)", async () => {
    const neverSettles = vi.fn(() => new Promise<boolean>(() => {}));
    const emit = vi.fn().mockResolvedValue(undefined);
    await expect(answerPing("main", emit, neverSettles, 20)).resolves.toBe("event");
    expect(emit).toHaveBeenCalledTimes(1);
  });

  it("treats an HTTP leg that rejects as failed and falls back", async () => {
    const http = vi.fn().mockRejectedValue(new Error("network"));
    const emit = vi.fn().mockResolvedValue(undefined);
    await expect(answerPing("main", emit, http)).resolves.toBe("event");
  });

  it("reports failed when both legs hang, instead of leaving the handler pending", async () => {
    const neverHttp = vi.fn(() => new Promise<boolean>(() => {}));
    const neverEmit = vi.fn(() => new Promise<void>(() => {}));
    await expect(answerPing("main", neverEmit, neverHttp, 20)).resolves.toBe("failed");
  });

  it("treats an HTTP leg that throws synchronously as failed and falls back", async () => {
    const http = vi.fn(() => {
      throw new Error("sync");
    });
    const emit = vi.fn().mockResolvedValue(undefined);
    await expect(answerPing("main", emit, http)).resolves.toBe("event");
  });

  it("reports failed when both legs fail", async () => {
    const http = vi.fn().mockResolvedValue(false);
    const emit = vi.fn().mockRejectedValue(new Error("ipc down"));
    await expect(answerPing("main", emit, http)).resolves.toBe("failed");
  });
});
