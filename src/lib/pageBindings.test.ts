import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

// The project's vitest environment is "node" (no DOM). Mock the storage layer
// with an in-memory map (avoids `getApiPort`'s `window` access) and give the
// module a real EventTarget as `window` so the change-event path is exercised.
const store = new Map<string, string>();
vi.mock("./instance-storage", () => ({
  instanceStorage: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    getJSON: <T>(k: string, fallback: T): T => {
      const raw = store.get(k);
      return raw ? (JSON.parse(raw) as T) : fallback;
    },
    setJSON: (k: string, v: unknown) => void store.set(k, JSON.stringify(v)),
  },
}));

beforeAll(() => {
  if (typeof (globalThis as { window?: unknown }).window === "undefined") {
    (globalThis as { window?: unknown }).window = new EventTarget();
  }
});

afterEach(() => {
  store.clear();
});

// Imported after the mock is registered (vi.mock is hoisted).
const {
  PAGE_BINDINGS_EVENT,
  getPageBindings,
  setPageBinding,
  setPageBindings,
  subscribePageBindings,
  windowForPage,
} = await import("./pageBindings");

describe("pageBindings", () => {
  it("defaults to an empty map and null window-for-page", () => {
    expect(getPageBindings()).toEqual({});
    expect(windowForPage("page-A")).toBeNull();
  });

  it("binds and unbinds a single page", () => {
    setPageBinding("page-A", "term-1");
    expect(getPageBindings()).toEqual({ "page-A": "term-1" });
    expect(windowForPage("page-A")).toBe("term-1");

    setPageBinding("page-A", null);
    expect(getPageBindings()).toEqual({});
    expect(windowForPage("page-A")).toBeNull();
  });

  it("overwrites the whole map with setPageBindings", () => {
    setPageBinding("page-A", "term-1");
    setPageBindings({ "page-B": "term-2" });
    expect(getPageBindings()).toEqual({ "page-B": "term-2" });
  });

  it("emits a change event on mutation but not on a no-op", () => {
    const cb = vi.fn();
    const unsub = subscribePageBindings(cb);

    setPageBinding("page-A", "term-1");
    expect(cb).toHaveBeenCalledTimes(1);

    // No-op: same single binding again → no event.
    setPageBinding("page-A", "term-1");
    expect(cb).toHaveBeenCalledTimes(1);

    // No-op: identical full map → no event.
    setPageBindings({ "page-A": "term-1" });
    expect(cb).toHaveBeenCalledTimes(1);

    setPageBindings({ "page-A": "term-1", "page-B": "term-2" });
    expect(cb).toHaveBeenCalledTimes(2);

    unsub();
    setPageBinding("page-C", "term-3");
    expect(cb).toHaveBeenCalledTimes(2); // unsubscribed
  });

  it("uses the documented event name", () => {
    const cb = vi.fn();
    window.addEventListener(PAGE_BINDINGS_EVENT, cb);
    setPageBinding("page-A", "term-1");
    expect(cb).toHaveBeenCalledTimes(1);
    window.removeEventListener(PAGE_BINDINGS_EVENT, cb);
  });
});
