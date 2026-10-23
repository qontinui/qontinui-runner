/**
 * WebGL comes back on the pane a reveal shows (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 2 / D2), entered where the
 * page enters it: REAL `XtermBackend`s open and claim their slots in the REAL
 * `webglContextLru`, and the reveal is the `setWebglSlotVisible` call
 * `TerminalInstance` makes from its `visible` prop. Only xterm and its addons
 * are stand-ins (no canvas under vitest); each records the addons its terminal
 * loaded, so the renderer a pane ends up on is read from what the backend did.
 *
 * Tmux work mode keeps every off-screen session mounted, so a busy page holds
 * more panes than the WebGL cap; the parked ones are evicted to Canvas. A swap
 * that landed on such a pane and left it on Canvas would degrade exactly the
 * session the operator just asked to look at.
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

describe("XtermBackend under the WebGL cap", () => {
  it("revealing an evicted pane restores its WebGL renderer, evicting a parked pane instead", () => {
    const container = { querySelectorAll: () => [] } as unknown as HTMLElement;
    // One pane more than the cap: opening the last evicts the oldest (pane 0).
    for (let i = 0; i <= MAX_WEBGL_PANES; i++) {
      new XtermBackend({ instanceKey: `pane-${i}` }).open(container);
    }
    expect(liveRenderer(0)).toBe("canvas");
    expect(liveRenderer(MAX_WEBGL_PANES)).toBe("webgl");

    // Pane 1 is parked off-screen; pane 0 is swapped on screen.
    setWebglSlotVisible("pane-1", false);
    setWebglSlotVisible("pane-0", true);

    expect(liveRenderer(0)).toBe("webgl");
    expect(liveRenderer(1)).toBe("canvas");
    expect(liveRenderer(2)).toBe("webgl");
  });
});
