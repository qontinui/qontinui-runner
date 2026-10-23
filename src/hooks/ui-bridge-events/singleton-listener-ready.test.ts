/**
 * `singletonListenerReady` / `terminalOutputListening`: resolve only once the
 * Tauri `listen()` has resolved, i.e. once the runner-side listener exists.
 *
 * A terminal pane that resets the runner's flow-control counters must not do
 * so before this: bytes counted after the reset and emitted before the
 * listener exists reach nobody, and nothing ever acks them (a paused gate then
 * stays shut). `acquireSingletonListener` returns before `listen()` resolves,
 * so the readiness promise is the only honest signal.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

/** `listen()` calls whose resolution the test controls, with their callbacks. */
const pending: Array<{
  resolve: () => void;
  reject: (e: unknown) => void;
  cb: (e: unknown) => void;
}> = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    (_event: string, cb: (e: unknown) => void) =>
      new Promise<() => void>((resolve, reject) => {
        pending.push({ resolve: () => resolve(() => {}), reject, cb });
      }),
  ),
}));

import {
  acquireSingletonListener,
  singletonListenerReady,
  _resetSingletonRegistry,
} from "./singleton-listener";
import {
  registerTerminalOutputHandler,
  terminalOutputListening,
  __resetTerminalDemuxForTest,
} from "@/components/terminal/terminalEventDemux";

async function settled(p: Promise<unknown>): Promise<boolean> {
  let done = false;
  void p.then(() => (done = true));
  for (let i = 0; i < 10; i++) await Promise.resolve();
  return done;
}

beforeEach(() => {
  pending.length = 0;
  __resetTerminalDemuxForTest();
  _resetSingletonRegistry();
});

afterEach(() => {
  vi.restoreAllMocks();
});

async function flushMicrotasks(): Promise<void> {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe("singletonListenerReady", () => {
  it("resolves only after a late listen() resolves", async () => {
    const release = acquireSingletonListener("evt", () => {});
    const ready = singletonListenerReady("evt");
    expect(await settled(ready)).toBe(false);
    pending[0].resolve();
    expect(await ready).toBe(true);
    release();
  });

  it("resolves false, never hangs or rejects, when listen() and its retry fail", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(await singletonListenerReady("none")).toBe(false);
    const release = acquireSingletonListener("evt", () => {});
    const ready = singletonListenerReady("evt");
    pending[0].reject(new Error("ipc down"));
    await flushMicrotasks();
    expect(pending).toHaveLength(2); // the one retry
    pending[1].reject(new Error("still down"));
    expect(await ready).toBe(false);
    expect(pending).toHaveLength(2); // and no more
    release();
  });
});

describe("a listen() that rejected is dropped and retried", () => {
  it("the pane waiting on the FIRST attempt gets a retry, and resolves true when it lands", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const unregister = registerTerminalOutputHandler("t1", () => {});
    // The cold mount is already waiting when the first listen() fails.
    const listening = terminalOutputListening();
    pending[0].reject(new Error("ipc down"));
    await flushMicrotasks();
    expect(pending).toHaveLength(2);
    pending[1].resolve();
    expect(await listening).toBe(true);
    unregister();
  });

  it("the next acquisition listens again, and the retry reaches every handler", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const seen: string[] = [];
    const releaseA = acquireSingletonListener<string>(
      "evt",
      (e) => void seen.push(`a:${e.payload}`),
    );
    pending[0].reject(new Error("ipc down"));
    await flushMicrotasks();

    // The next acquisition retries it, rather than joining a dead listener.
    const releaseB = acquireSingletonListener<string>(
      "evt",
      (e) => void seen.push(`b:${e.payload}`),
    );
    expect(pending).toHaveLength(2);
    const ready = singletonListenerReady("evt");
    expect(pending).toHaveLength(2); // in flight: not retried twice
    pending[1].resolve();
    expect(await ready).toBe(true);
    pending[1].cb({ payload: "x" });
    expect(seen).toEqual(["a:x", "b:x"]);
    releaseA();
    releaseB();
  });

  it("does not retry for a subscription released while the probe waited", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const release = acquireSingletonListener("evt", () => {});
    const ready = singletonListenerReady("evt");
    release(); // the last handler leaves: the subscription is torn down
    pending[0].reject(new Error("ipc down"));
    expect(await ready).toBe(false);
    await flushMicrotasks();
    expect(pending).toHaveLength(1);
  });

  it("two waiters on one failing attempt share a single retry", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const release = acquireSingletonListener("evt", () => {});
    const first = singletonListenerReady("evt");
    const second = singletonListenerReady("evt");
    pending[0].reject(new Error("ipc down"));
    await flushMicrotasks();
    expect(pending).toHaveLength(2);
    pending[1].resolve();
    expect(await first).toBe(true);
    expect(await second).toBe(true);
    expect(pending).toHaveLength(2);
    release();
  });

  it("a probe after the failure starts the retry itself", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const unregister = registerTerminalOutputHandler("t1", () => {});
    pending[0].reject(new Error("ipc down"));
    await flushMicrotasks();
    const listening = terminalOutputListening();
    expect(pending).toHaveLength(2);
    pending[1].resolve();
    expect(await listening).toBe(true);
    unregister();
  });
});

describe("terminalOutputListening", () => {
  it("waits for the terminal-output listener a pane's handler registration started", async () => {
    const unregister = registerTerminalOutputHandler("t1", () => {});
    const listening = terminalOutputListening();
    expect(await settled(listening)).toBe(false);
    pending[0].resolve();
    expect(await listening).toBe(true);
    unregister();
  });
});
