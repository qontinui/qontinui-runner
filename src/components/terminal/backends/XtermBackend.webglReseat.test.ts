/**
 * Getting WebGL back never takes it from another VISIBLE pane (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, review fix to D2), entered where
 * the page enters it: REAL `XtermBackend`s open and claim their slots in the
 * REAL `webglContextLru`, the reveal / hide is the `setWebglSlotVisible` call
 * `TerminalInstance` makes from its `visible` prop, and the freed slot is a
 * pane's `dispose()`. Only xterm and its addons are stand-ins (no canvas under
 * vitest); each records the addons its terminal loaded, so the renderer a pane
 * ends up on is read from what the backend did.
 *
 * In a grid with more visible panes than the cap, revealing an evicted pane by
 * displacing another visible one buys nothing the operator can see and costs
 * two renderer rebuilds (and un-maximizing reveals every pane at once, so it
 * cascades). A visible evicted pane waits on Canvas and takes the next seat a
 * hiding holder or a disposed pane frees.
 *
 * Its own file (not beside `XtermBackend.webglRestore.test.ts`): the LRU is
 * module state, and this scenario needs a cap's worth of fresh panes.
 */

import { describe, expect, it, vi } from "vitest";

const xterm = vi.hoisted(() => ({
  /** Every stand-in `Terminal`, in construction order, with the addons loaded into it. */
  terminals: [] as Array<{ addons: Array<{ kind: string; disposed: boolean }> }>,
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    addons: Array<{ kind: string; disposed: boolean }> = [];
    parser = { registerOscHandler: () => ({ dispose: () => {} }) };
    constructor() {
      xterm.terminals.push(this);
    }
    loadAddon(addon: { kind: string; disposed: boolean }) {
      this.addons.push(addon);
    }
    open() {}
    registerMarker() {
      return null;
    }
    dispose() {}
  },
}));
const addon = vi.hoisted(
  () => (kind: string) =>
    class {
      kind = kind;
      disposed = false;
      onContextLoss() {}
      clearTextureAtlas() {}
      dispose() {
        this.disposed = true;
      }
    },
);
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: addon("webgl") }));
vi.mock("@xterm/addon-canvas", () => ({ CanvasAddon: addon("canvas") }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: addon("fit") }));
vi.mock("@xterm/addon-web-links", () => ({ WebLinksAddon: addon("web-links") }));
vi.mock("@xterm/addon-search", () => ({ SearchAddon: addon("search") }));

import { XtermBackend } from "./XtermBackend";
import { MAX_WEBGL_PANES, setWebglSlotVisible } from "./webglContextLru";

/** The renderer addon a terminal is drawing with now: the newest live GPU addon. */
function liveRenderer(index: number): string | undefined {
  return xterm.terminals[index].addons
    .filter((a) => (a.kind === "webgl" || a.kind === "canvas") && !a.disposed)
    .at(-1)?.kind;
}

describe("XtermBackend under the WebGL cap — every holder visible", () => {
  it("a revealed evicted pane waits on Canvas, then takes the seat a hiding or disposed pane frees", () => {
    const container = { querySelectorAll: () => [] } as unknown as HTMLElement;
    // One pane more than the cap, all on screen: opening the last evicts the
    // oldest (pane 0) to Canvas.
    const panes: XtermBackend[] = [];
    for (let i = 0; i <= MAX_WEBGL_PANES; i++) {
      const pane = new XtermBackend({ instanceKey: `seat-${i}` });
      pane.open(container);
      panes.push(pane);
    }
    expect(liveRenderer(0)).toBe("canvas");

    // Pane 0 is revealed while every holder is on screen: it waits on Canvas,
    // and no visible pane loses its WebGL renderer to it.
    setWebglSlotVisible("seat-0", true);
    expect(liveRenderer(0)).toBe("canvas");
    for (let i = 1; i <= MAX_WEBGL_PANES; i++) expect(liveRenderer(i)).toBe("webgl");

    // A holder hides: the waiting visible pane takes its seat.
    setWebglSlotVisible("seat-1", false);
    expect(liveRenderer(0)).toBe("webgl");
    expect(liveRenderer(1)).toBe("canvas");

    // Pane 1 comes back on screen with every holder visible again: it waits...
    setWebglSlotVisible("seat-1", true);
    expect(liveRenderer(1)).toBe("canvas");
    // ...until a pane is disposed and frees a seat.
    panes[2].dispose();
    expect(liveRenderer(1)).toBe("webgl");
  });
});
