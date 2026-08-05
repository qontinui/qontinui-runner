/**
 * terminalEventDemux tests — one `listen()` per window per event name, fanned
 * out to per-terminal handlers (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 2 / A3).
 *
 * The regression these lock down: every mounted `TerminalInstance` used to
 * install its OWN global `terminal-output` listener and filter by id AFTER
 * delivery, so one busy pane cost ~(M mounted + P pages) × W windows IPC
 * dispatches per chunk.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

// --- Mock Tauri event API (same shape as singleton-listener.test.ts) -------
const listenCalls: Array<{ event: string; cb: (e: unknown) => void }> = [];
const unlistenSpies: Array<ReturnType<typeof vi.fn>> = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: unknown) => void) => {
    listenCalls.push({ event, cb });
    const unlisten = vi.fn();
    unlistenSpies.push(unlisten);
    await Promise.resolve();
    return unlisten;
  }),
}));

import {
  registerTerminalOutputHandler,
  registerTerminalExitHandler,
  subscribeTerminalOutputStream,
  __terminalDemuxStats,
  __resetTerminalDemuxForTest,
} from "./terminalEventDemux";
import { _activeSubscriptionCount } from "@/hooks/ui-bridge-events/singleton-listener";

/** Fire the mocked listener for one event name. */
function emit(eventName: string, payload: unknown): void {
  for (const call of listenCalls) {
    if (call.event === eventName) call.cb({ payload });
  }
}

function outputEvent(terminalId: string, data = "", offset?: number) {
  return { terminalId, data, offset };
}

beforeEach(() => {
  __resetTerminalDemuxForTest();
  listenCalls.length = 0;
  unlistenSpies.length = 0;
});

describe("terminal-output demux", () => {
  it("installs ONE listener for N terminals and routes only to the addressee", async () => {
    const a = vi.fn();
    const b = vi.fn();
    const c = vi.fn();
    registerTerminalOutputHandler("term-a", a);
    registerTerminalOutputHandler("term-b", b);
    registerTerminalOutputHandler("term-c", c);
    await Promise.resolve();

    expect(listenCalls.filter((l) => l.event === "terminal-output")).toHaveLength(1);
    expect(__terminalDemuxStats().output).toEqual({
      terminals: 3,
      handlers: 3,
      listening: true,
    });

    emit("terminal-output", outputEvent("term-b", "aGk="));

    // One JS delivery for one chunk — not one per mounted pane.
    expect(a).not.toHaveBeenCalled();
    expect(c).not.toHaveBeenCalled();
    expect(b).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledWith({ terminalId: "term-b", data: "aGk=", offset: undefined });
  });

  it("delivers to every handler registered for the same terminal", async () => {
    // Transient double-mount (a tab moving between zones/windows).
    const first = vi.fn();
    const second = vi.fn();
    registerTerminalOutputHandler("term-a", first);
    registerTerminalOutputHandler("term-a", second);
    await Promise.resolve();

    expect(listenCalls.filter((l) => l.event === "terminal-output")).toHaveLength(1);
    emit("terminal-output", outputEvent("term-a"));
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
  });

  it("drops the shared listener only when the LAST handler unregisters", async () => {
    const unregA = registerTerminalOutputHandler("term-a", vi.fn());
    const unregB = registerTerminalOutputHandler("term-b", vi.fn());
    await Promise.resolve();
    await Promise.resolve();

    unregA();
    expect(__terminalDemuxStats().output).toEqual({
      terminals: 1,
      handlers: 1,
      listening: true,
    });
    expect(unlistenSpies[0]).not.toHaveBeenCalled();

    unregB();
    expect(__terminalDemuxStats().output.listening).toBe(false);
    await Promise.resolve();
    await Promise.resolve();
    expect(unlistenSpies[0]).toHaveBeenCalledTimes(1);
  });

  it("unregister is idempotent and a re-register re-installs the listener", async () => {
    const handler = vi.fn();
    const unreg = registerTerminalOutputHandler("term-a", handler);
    await Promise.resolve();
    unreg();
    unreg();
    await Promise.resolve();
    await Promise.resolve();

    registerTerminalOutputHandler("term-a", handler);
    await Promise.resolve();
    expect(listenCalls.filter((l) => l.event === "terminal-output")).toHaveLength(2);
    // Exactly one is live: the released one was unlistened.
    expect(unlistenSpies[0]).toHaveBeenCalledTimes(1);
    expect(__terminalDemuxStats().output.listening).toBe(true);
  });

  it("isolates a throwing handler from the other terminals", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    registerTerminalOutputHandler("term-a", () => {
      throw new Error("pane blew up");
    });
    const sibling = vi.fn();
    registerTerminalOutputHandler("term-a", sibling);
    await Promise.resolve();

    expect(() => emit("terminal-output", outputEvent("term-a"))).not.toThrow();
    expect(sibling).toHaveBeenCalledTimes(1);
    consoleError.mockRestore();
  });

  it("shares its single listener with the page-wide output tap", async () => {
    const pane = vi.fn();
    const tap = vi.fn();
    const unregPane = registerTerminalOutputHandler("term-a", pane);
    const releaseTap = subscribeTerminalOutputStream(tap);
    await Promise.resolve();

    // The whole point of the handler-SET extension to singleton-listener: the
    // demux and the tap coexist behind ONE listen(), instead of the later
    // acquirer silently overwriting the earlier one.
    expect(listenCalls.filter((l) => l.event === "terminal-output")).toHaveLength(1);
    expect(_activeSubscriptionCount()).toBe(1);

    emit("terminal-output", outputEvent("term-a"));
    emit("terminal-output", outputEvent("term-zzz"));

    expect(pane).toHaveBeenCalledTimes(1); // demuxed: only its own terminal
    expect(tap).toHaveBeenCalledTimes(2); // broadcast: every terminal

    unregPane();
    releaseTap();
  });
});

describe("terminal-exit demux", () => {
  it("routes exits per terminal on its own single listener", async () => {
    const a = vi.fn();
    const b = vi.fn();
    registerTerminalExitHandler("term-a", a);
    registerTerminalExitHandler("term-b", b);
    await Promise.resolve();

    expect(listenCalls.filter((l) => l.event === "terminal-exit")).toHaveLength(1);
    emit("terminal-exit", { terminalId: "term-a", exitCode: 3 });
    expect(a).toHaveBeenCalledWith({ terminalId: "term-a", exitCode: 3 });
    expect(b).not.toHaveBeenCalled();
  });

  it("is refcounted independently of the output demux", async () => {
    const unregOut = registerTerminalOutputHandler("term-a", vi.fn());
    const unregExit = registerTerminalExitHandler("term-a", vi.fn());
    await Promise.resolve();

    unregExit();
    expect(__terminalDemuxStats().exit.listening).toBe(false);
    expect(__terminalDemuxStats().output.listening).toBe(true);
    unregOut();
  });
});
