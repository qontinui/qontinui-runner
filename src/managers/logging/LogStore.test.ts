/**
 * Tests for `LogStore`'s snapshot caching + notification batching (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 1 / §0 A2).
 *
 * Both properties are load-bearing:
 *
 *   - the getters must return a STABLE array identity between mutations, or a
 *     `useSyncExternalStore` consumer loops forever and a `setState` consumer
 *     can never bail;
 *   - notification must coalesce, or a burst of AI output produces one full
 *     app re-render per line.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { LogStore, type LogEntry } from "./LogStore";

function entry(message: string): LogEntry {
  return { id: message, timestamp: "t", level: "info", message };
}

describe("LogStore — cached snapshots", () => {
  let store: LogStore;
  beforeEach(() => {
    store = new LogStore();
  });

  it("returns the same array identity until the bucket mutates", () => {
    const first = store.getGeneralLogs();
    expect(store.getGeneralLogs()).toBe(first);

    store.addGeneralLog(entry("one"));
    const second = store.getGeneralLogs();
    expect(second).not.toBe(first);
    expect(second).toHaveLength(1);
    expect(store.getGeneralLogs()).toBe(second);
  });

  it("does not disturb sibling buckets' identities", () => {
    const images = store.getImageLogs();
    const ai = store.getAiOutputLogs();
    store.addGeneralLog(entry("one"));
    expect(store.getImageLogs()).toBe(images);
    expect(store.getAiOutputLogs()).toBe(ai);
  });

  it("reflects a mutation synchronously even though notification is deferred", () => {
    store.addGeneralLog(entry("one"));
    // No timer advance — a read immediately after the write must be fresh.
    expect(store.getGeneralLogs().map((l) => l.message)).toEqual(["one"]);
  });

  it("still hands out a fresh copy consumers cannot mutate into the store", () => {
    store.addGeneralLog(entry("one"));
    const snapshot = store.getGeneralLogs();
    snapshot.push(entry("injected"));
    // The pushed entry lives on the (already-handed-out) snapshot only; the
    // next mutation rebuilds from the private array.
    store.addGeneralLog(entry("two"));
    expect(store.getGeneralLogs().map((l) => l.message)).toEqual(["one", "two"]);
  });
});

describe("LogStore — batched notification", () => {
  let store: LogStore;
  beforeEach(() => {
    vi.useFakeTimers();
    store = new LogStore();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("coalesces a burst of mutations into ONE listener call", () => {
    let calls = 0;
    store.subscribe(() => calls++);

    for (let i = 0; i < 50; i++) store.addGeneralLog(entry(`line-${i}`));
    expect(calls).toBe(0); // nothing synchronous

    vi.advanceTimersByTime(100);
    expect(calls).toBe(1);
  });

  it("coalesces across buckets too", () => {
    let calls = 0;
    store.subscribe(() => calls++);
    store.addGeneralLog(entry("a"));
    store.addAiOutputLog({ id: "1", timestamp: 0, line: "x", source: "claude" });
    store.clearImageLogs();
    vi.advanceTimersByTime(100);
    expect(calls).toBe(1);
  });

  it("re-arms for the next window", () => {
    let calls = 0;
    store.subscribe(() => calls++);
    store.addGeneralLog(entry("a"));
    vi.advanceTimersByTime(100);
    expect(calls).toBe(1);
    store.addGeneralLog(entry("b"));
    vi.advanceTimersByTime(100);
    expect(calls).toBe(2);
  });

  it("flushNotifications drains the pending window immediately", () => {
    let calls = 0;
    store.subscribe(() => calls++);
    store.addGeneralLog(entry("a"));
    store.flushNotifications();
    expect(calls).toBe(1);
    // Timer was cleared, so advancing must not double-fire.
    vi.advanceTimersByTime(100);
    expect(calls).toBe(1);
  });

  it("flushNotifications is a no-op with nothing pending", () => {
    let calls = 0;
    store.subscribe(() => calls++);
    store.flushNotifications();
    expect(calls).toBe(0);
  });

  it("stops notifying after unsubscribe", () => {
    let calls = 0;
    const unsubscribe = store.subscribe(() => calls++);
    unsubscribe();
    store.addGeneralLog(entry("a"));
    vi.advanceTimersByTime(100);
    expect(calls).toBe(0);
  });
});
