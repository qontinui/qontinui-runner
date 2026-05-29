/**
 * Pure-helper tests for the command-action registry.
 *
 * Runs under vitest's `environment: "node"` config (no jsdom) — exercises
 * the module-singleton store directly, matching the repo precedent in
 * `FileOwnershipHeatmap.test.ts` and `useCommitState.test.ts`. The React
 * hook (`useCommandAction`) is a thin wrapper around `register`/
 * `unregister` and is covered transitively by Phase 2's CommandBar tests.
 *
 * Coverage:
 *   1. register + getById round-trip
 *   2. register replaces on id collision (strict-mode/fast-refresh
 *      safety) without throwing
 *   3. unregister removes the entry and notifies subscribers
 *   4. getBySlash matches primary slash AND aliases
 *   5. subscribe fires on register / unregister / replace
 *   6. unsubscribe stops notifications and doesn't leak listeners
 *   7. getAll returns a stable reference between mutations (cheap
 *      identity check for `useSyncExternalStore` consumers)
 *   8. unregister of unknown id is a silent no-op (no throw, no notify)
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  __resetForTest,
  getAll,
  getById,
  getBySlash,
  register,
  subscribe,
  unregister,
} from "./registry";
import type { CommandAction } from "./types";

afterEach(() => {
  __resetForTest();
});

const action = (overrides: Partial<CommandAction> = {}): CommandAction => ({
  id: "test.action",
  slash: "/test",
  label: "Test action",
  description: "for the registry spec",
  handler: async () => ({ ok: true, value: "ran" }),
  ...overrides,
});

describe("registry — register / lookup", () => {
  it("registers an action and returns it via getById", () => {
    register(action());
    expect(getById("test.action")?.slash).toBe("/test");
  });

  it("replaces in place on id collision rather than throwing", () => {
    register(action({ label: "first" }));
    register(action({ label: "second" }));
    expect(getById("test.action")?.label).toBe("second");
    expect(getAll().length).toBe(1);
  });

  it("getBySlash finds by primary slash", () => {
    register(action());
    expect(getBySlash("/test")?.id).toBe("test.action");
    expect(getBySlash("/missing")).toBeUndefined();
  });

  it("getBySlash finds by alias", () => {
    register(action({ aliases: ["/t", "/run-test"] }));
    expect(getBySlash("/t")?.id).toBe("test.action");
    expect(getBySlash("/run-test")?.id).toBe("test.action");
  });

  it("unregister removes the entry; subsequent lookup returns undefined", () => {
    const dispose = register(action());
    expect(getById("test.action")).toBeDefined();
    dispose();
    expect(getById("test.action")).toBeUndefined();
  });

  it("unregister of an unknown id is a silent no-op", () => {
    const listener = vi.fn();
    subscribe(listener);
    unregister("never.registered");
    expect(listener).not.toHaveBeenCalled();
  });
});

describe("registry — subscribe", () => {
  it("fires on register, unregister, and replace", () => {
    const listener = vi.fn();
    subscribe(listener);
    register(action());
    register(action({ label: "replaced" }));
    unregister("test.action");
    expect(listener).toHaveBeenCalledTimes(3);
  });

  it("returned unsubscribe stops notifications", () => {
    const listener = vi.fn();
    const off = subscribe(listener);
    off();
    register(action());
    expect(listener).not.toHaveBeenCalled();
  });
});

describe("registry — getAll snapshot identity", () => {
  it("returns a stable reference between mutations", () => {
    register(action());
    const first = getAll();
    const second = getAll();
    expect(first).toBe(second);
  });

  it("rebuilds the snapshot on mutation", () => {
    register(action());
    const before = getAll();
    register(action({ id: "test.second", slash: "/second" }));
    const after = getAll();
    expect(after).not.toBe(before);
    expect(after).toHaveLength(2);
  });
});
