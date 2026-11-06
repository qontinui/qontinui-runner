// @vitest-environment jsdom
/**
 * The `zone-layout-picker` bridge actions `/layout` and `/triage-sessions` drive
 * from outside the runner. `select-layout` hands the set to the registry, so
 * what is pinned here is what the action decides itself: which page it targets,
 * the refusals, and the fields its callers read back.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Action = { id: string; handler: (params?: unknown) => unknown };

const { registration, callRegistry, pages } = vi.hoisted(() => ({
  registration: { actions: [] as Array<{ id: string; handler: (params?: unknown) => unknown }> },
  callRegistry: vi.fn(async () => undefined),
  pages: new Map<string, unknown>(),
}));

vi.mock("@qontinui/ui-bridge", () => ({
  useUIComponent: (opts: { actions: Action[] }) => {
    registration.actions = opts.actions;
  },
  UIBridgeComponentScope: ({ children }: { children: unknown }) => children,
}));
vi.mock("./commands", async (orig) => ({
  ...(await orig<object>()),
  callRegistry,
}));
const { autoTrim } = vi.hoisted(() => ({ autoTrim: { enabled: true } }));
vi.mock("./zoneAutoTrim", async (orig) => ({
  ...(await orig<object>()),
  readAutoTrimEnabled: () => autoTrim.enabled,
}));
vi.mock("./contexts", () => ({
  usePageSessionGetter: () => (id: string) => pages.get(id) ?? null,
  useTerminalSession: () => pages.get("page-1"),
}));

import { ZoneLayoutPicker } from "./ZoneLayoutPicker";

function page(
  pageId: string,
  layoutId: string,
  zones: number,
  tabCount: number,
  workMode: "grid" | "tmux" = "grid",
  layoutOrigin?: "auto" | "operator",
) {
  return {
    pageId,
    tabs: Array.from({ length: tabCount }, (_, i) => ({ id: `${pageId}-${i}` })),
    zoneLayout: {
      layoutId,
      layout: { zones: Array.from({ length: zones }, () => ({})) },
      maximizedZone: null,
      workMode,
      layoutOrigin,
    },
  };
}

let container: HTMLDivElement;
let root: Root;

const action = (id: string) => {
  const found = registration.actions.find((a) => a.id === id);
  if (!found) throw new Error(`no action ${id}`);
  return found;
};
const select = (params: unknown) => action("select-layout").handler(params);
const read = (params: unknown) => action("get-page-layout").handler(params);

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  callRegistry.mockClear();
  autoTrim.enabled = true;
  pages.clear();
  pages.set("page-1", page("page-1", "quad", 4, 2));
  pages.set("page-2", page("page-2", "six-pack", 6, 5, "grid", "auto"));
  pages.set("page-op", page("page-op", "quad", 4, 2, "grid", "operator"));
  pages.set("page-tmux", page("page-tmux", "single", 1, 4, "tmux"));
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() =>
    root.render(
      createElement(ZoneLayoutPicker, {
        currentLayoutId: "quad",
        onSelectLayout: () => {},
        tabCount: 2,
      }),
    ),
  );
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("select-layout", () => {
  it("targets the named page and settles against that page's tabs", async () => {
    await expect(select({ layoutId: "2", pageId: "page-2" })).resolves.toEqual({
      requested: "split",
      layoutId: "six-pack",
      pageId: "page-2",
      tabCount: 5,
      leftTmux: false,
      origin: "operator",
    });
    expect(callRegistry).toHaveBeenCalledWith("terminal.layout", {
      preset: "2",
      page: "page-2",
    });
  });

  it("with no pageId targets the page on screen and sends no page", async () => {
    await expect(select({ layoutId: "quad" })).resolves.toMatchObject({
      layoutId: "quad",
      pageId: "page-1",
      tabCount: 2,
    });
    expect(callRegistry).toHaveBeenCalledWith("terminal.layout", { preset: "quad" });
  });

  it.each(["", "   "])("refuses a blank pageId %j and sets nothing", async (pageId) => {
    await expect(select({ layoutId: "quad", pageId })).rejects.toThrow(/pageId needs a page id/);
    expect(callRegistry).not.toHaveBeenCalled();
  });

  it("says when the request took the page out of tmux mode", async () => {
    // As the real set does, so the action has to have read the mode beforehand.
    callRegistry.mockImplementationOnce(async () => {
      pages.set("page-tmux", page("page-tmux", "quad", 4, 4));
    });
    await expect(select({ layoutId: "4", pageId: "page-tmux" })).resolves.toMatchObject({
      layoutId: "quad",
      leftTmux: true,
    });
  });

  it("forwards origin auto to the registry and reports it", async () => {
    await expect(
      select({ layoutId: "split", pageId: "page-2", origin: "auto" }),
    ).resolves.toMatchObject({ origin: "auto" });
    expect(callRegistry).toHaveBeenCalledWith("terminal.layout", {
      preset: "split",
      page: "page-2",
      origin: "auto",
    });
  });

  it("sends no origin for an explicit operator request", async () => {
    await expect(select({ layoutId: "quad", origin: "Operator" })).resolves.toMatchObject({
      origin: "operator",
    });
    expect(callRegistry).toHaveBeenCalledWith("terminal.layout", { preset: "quad" });
  });

  it.each([["restore"], [""], [{}], [7]])(
    "refuses an origin %j that names neither, before reaching the registry",
    async (origin) => {
      // The schema guard refuses a non-text value synchronously; the handler
      // refuses a text one that names neither. Both must stop before the set.
      await expect(async () => select({ layoutId: "quad", origin })).rejects.toThrow(/origin/);
      expect(callRegistry).not.toHaveBeenCalled();
    },
  );

  it("passes the registry's refusal of an unknown page through", async () => {
    callRegistry.mockRejectedValueOnce(new Error("no terminal page with id nope"));
    await expect(select({ layoutId: "quad", pageId: "nope" })).rejects.toThrow(/nope/);
  });

  it("refuses a layout that resolves to nothing before reaching the registry", async () => {
    await expect(select({ layoutId: "bogus" })).rejects.toThrow(/Unknown layoutId/);
    expect(callRegistry).not.toHaveBeenCalled();
  });
});

describe("get-page-layout", () => {
  it("reads the named page, not the page on screen", () => {
    expect(read({ pageId: "page-2" })).toEqual({
      pageId: "page-2",
      layoutId: "six-pack",
      zoneCount: 6,
      tabCount: 5,
      maximizedZone: null,
      workMode: "grid",
      layoutOrigin: "auto",
      autoTrim: true,
    });
  });

  it("reports an operator page's origin", () => {
    expect(read({ pageId: "page-op" })).toMatchObject({ layoutOrigin: "operator" });
  });

  it('reports a page with no stored origin as "legacy", not operator', () => {
    expect(read({ pageId: "page-1" })).toMatchObject({ layoutOrigin: "legacy" });
  });

  it("reports the auto-trim preference as this instance holds it", () => {
    autoTrim.enabled = false;
    expect(read({ pageId: "page-2" })).toMatchObject({ autoTrim: false });
  });

  it("names the page on screen when given no pageId", () => {
    expect(read({})).toMatchObject({ pageId: "page-1", layoutId: "quad", zoneCount: 4 });
  });

  it("reports a tmux page's work mode", () => {
    expect(read({ pageId: "page-tmux" })).toMatchObject({ zoneCount: 1, workMode: "tmux" });
  });

  it("refuses an unknown page", () => {
    expect(() => read({ pageId: "nope" })).toThrow(/No terminal page with id "nope"/);
  });

  it.each(["", "   "])(
    "refuses a blank pageId %j rather than reading the active page",
    (pageId) => {
      expect(() => read({ pageId })).toThrow(/pageId needs a page id/);
    },
  );
});
