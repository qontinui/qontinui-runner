/**
 * The Session Manager sidebar's open state survives reload, per page and per
 * window — in tmux work mode it is the only navigator, so a reload must never
 * bring the operator back to one pane with it closed (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

const store = vi.hoisted(() => new Map<string, unknown>());
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: (key: string, fallback: unknown) => (store.has(key) ? store.get(key) : fallback),
    setJSON: (key: string, value: unknown) => {
      store.set(key, value);
    },
  },
}));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import {
  DEFAULT_SHOW_SIDEBAR,
  parseShowSidebar,
  sessionSidebarStorageKey,
  usePersistedShowSidebar,
} from "./sessionSidebarPersistence";
import { zoneLayoutStorageKey } from "./useZoneLayout";

describe("sessionSidebarStorageKey", () => {
  it("uses the zone layout's per-(page, window) key shape", () => {
    expect(sessionSidebarStorageKey("default")).toBe("qontinui-session-sidebar");
    expect(sessionSidebarStorageKey("p1")).toBe("page:p1:qontinui-session-sidebar");
    expect(sessionSidebarStorageKey("p1", "term-2")).toBe(
      "page:p1:qontinui-session-sidebar:window:term-2",
    );
    // Same namespacing as the layout it sits beside.
    expect(zoneLayoutStorageKey("p1", "term-2")).toBe("page:p1:qontinui-zone-layout:window:term-2");
  });
});

describe("parseShowSidebar", () => {
  it("keeps today's default (closed) when nothing is stored", () => {
    expect(DEFAULT_SHOW_SIDEBAR).toBe(false);
    expect(parseShowSidebar(null)).toBe(false);
    expect(parseShowSidebar(undefined)).toBe(false);
  });

  it("reads a stored boolean as itself", () => {
    expect(parseShowSidebar(true)).toBe(true);
    expect(parseShowSidebar(false)).toBe(false);
  });

  it("falls back to the default and logs a value that is not a boolean", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(parseShowSidebar("yes")).toBe(false);
    expect(warn).toHaveBeenCalledWith(
      '[sessionSidebar] persisted showSidebar "yes" is not a boolean; using false',
    );
    warn.mockRestore();
  });
});

describe("usePersistedShowSidebar", () => {
  beforeEach(() => {
    store.clear();
    harness.reset();
  });
  afterEach(() => harness.reset());

  const mount = (pageId: string, windowLabel?: string) =>
    harness.renderSettled(() => {
      harness.beginRender();
      return usePersistedShowSidebar(pageId, windowLabel);
    });

  it("writes every change to the page/window key and reloads it open", () => {
    const [initial, setOpen] = mount("p1");
    expect(initial).toBe(false);
    setOpen(true);
    let [open] = mount("p1");
    expect(open).toBe(true);
    expect(store.get("page:p1:qontinui-session-sidebar")).toBe(true);

    // A reload is a fresh mount reading the stored value.
    harness.reset();
    [open] = mount("p1");
    expect(open).toBe(true);
  });

  it("keeps each page and each window separate", () => {
    store.set("page:p1:qontinui-session-sidebar", true);
    expect(mount("p1")[0]).toBe(true);
    harness.reset();
    expect(mount("p2")[0]).toBe(false);
    harness.reset();
    expect(mount("p1", "term-2")[0]).toBe(false);
  });

  it("writes a close back too, so a reload does not resurrect an open sidebar", () => {
    store.set("page:p1:qontinui-session-sidebar", true);
    const [, setOpen] = mount("p1");
    setOpen(false);
    mount("p1");
    expect(store.get("page:p1:qontinui-session-sidebar")).toBe(false);
  });
});
