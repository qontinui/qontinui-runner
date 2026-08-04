/**
 * singleton-listener tests — exactly one Tauri listen() per webview
 * regardless of mount count / async-cleanup races (the PR #473 round-2
 * accumulation bug).
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

// --- Mock Tauri event API -------------------------------------------------
// `listen` resolves asynchronously to an unlisten fn — exactly the shape
// that makes the naive useEffect cleanup race. We record each call so we
// can assert how many real listeners were installed.
const listenCalls: Array<{ event: string; cb: (e: unknown) => void }> = [];
const unlistenSpies: Array<ReturnType<typeof vi.fn>> = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: unknown) => void) => {
    listenCalls.push({ event, cb });
    const unlisten = vi.fn();
    unlistenSpies.push(unlisten);
    // Resolve on a microtask to mimic the real async race window.
    await Promise.resolve();
    return unlisten;
  }),
}));

import {
  acquireSingletonListener,
  _activeSubscriptionCount,
  _resetSingletonRegistry,
} from "./singleton-listener";

const EVT = "ui-bridge:test-request";

beforeEach(() => {
  listenCalls.length = 0;
  unlistenSpies.length = 0;
  _resetSingletonRegistry();
});

describe("acquireSingletonListener", () => {
  it("installs exactly one real listener for N concurrent mounts", async () => {
    const h1 = vi.fn();
    const h2 = vi.fn();
    const h3 = vi.fn();
    const r1 = acquireSingletonListener(EVT, h1);
    const r2 = acquireSingletonListener(EVT, h2);
    const r3 = acquireSingletonListener(EVT, h3);

    // Let listen() promises settle.
    await Promise.resolve();
    await Promise.resolve();

    expect(listenCalls).toHaveLength(1);
    expect(_activeSubscriptionCount()).toBe(1);

    // Cleanup all consumers.
    r1();
    r2();
    r3();
  });

  it("fans out to EVERY live handler without re-subscribing", async () => {
    const first = vi.fn();
    const release1 = acquireSingletonListener(EVT, first);
    await Promise.resolve();

    const second = vi.fn();
    const release2 = acquireSingletonListener(EVT, second);
    await Promise.resolve();

    // Still one underlying listener.
    expect(listenCalls).toHaveLength(1);

    // Fire the registered trampoline.
    const evt = { payload: { request_id: "x" } };
    listenCalls[0].cb(evt);

    // Both consumers see it — the single-slot design that dropped `first`
    // is what made this primitive unusable for `terminal-output`.
    expect(first).toHaveBeenCalledWith(evt);
    expect(second).toHaveBeenCalledWith(evt);

    // A released handler stops receiving; the survivor keeps the listener.
    release1();
    listenCalls[0].cb(evt);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(2);

    release2();
  });

  it("isolates a throwing handler from its siblings", async () => {
    const boom = vi.fn(() => {
      throw new Error("handler blew up");
    });
    const sibling = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const r1 = acquireSingletonListener(EVT, boom);
    const r2 = acquireSingletonListener(EVT, sibling);
    await Promise.resolve();

    expect(() => listenCalls[0].cb({ payload: {} })).not.toThrow();
    expect(sibling).toHaveBeenCalledTimes(1);
    expect(consoleError).toHaveBeenCalled();

    consoleError.mockRestore();
    r1();
    r2();
  });

  it("simulates the StrictMode mount→unmount→remount race without leaking", async () => {
    // Mount #1 (acquire) then immediately unmount (release) BEFORE the
    // listen() promise resolves — this is the exact race that leaked a
    // listener in the old code.
    const release1 = acquireSingletonListener(EVT, vi.fn());
    release1(); // refCount 1 -> 0 while listenPromise still pending

    // Remount #2 (StrictMode's second mount).
    const release2 = acquireSingletonListener(EVT, vi.fn());

    // Drain microtasks: first listen() resolves; its teardown must NOT
    // unlisten the listener the remount now relies on.
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    // The first subscription's teardown deletes the registry entry, so the
    // remount creates a fresh one — at most a single live subscription.
    expect(_activeSubscriptionCount()).toBe(1);

    release2();
    await Promise.resolve();
    await Promise.resolve();
    expect(_activeSubscriptionCount()).toBe(0);
  });

  it("tears down only when the last consumer releases", async () => {
    const a = acquireSingletonListener(EVT, vi.fn());
    const b = acquireSingletonListener(EVT, vi.fn());
    await Promise.resolve();
    await Promise.resolve();

    a();
    expect(_activeSubscriptionCount()).toBe(1); // b still holds it

    b();
    await Promise.resolve();
    await Promise.resolve();
    expect(_activeSubscriptionCount()).toBe(0);
    // The single unlisten was eventually invoked.
    expect(unlistenSpies[0]).toHaveBeenCalledTimes(1);
  });

  it("release is idempotent", async () => {
    const release = acquireSingletonListener(EVT, vi.fn());
    await Promise.resolve();
    release();
    release(); // no-op, must not throw or double-decrement
    await Promise.resolve();
    expect(_activeSubscriptionCount()).toBe(0);
  });
});
