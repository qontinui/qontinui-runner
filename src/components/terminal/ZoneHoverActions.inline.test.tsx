/**
 * `ZoneHoverActions` in its two modes. The vitest env is `node` with no React
 * Testing Library, so the markup is checked with `renderToStaticMarkup` and the
 * three contexts it reads are mocked. Locks the maximized header's contract:
 * the always-visible cluster (`inline`) must not carry the hover-overlay
 * classes, must show Restore (not Maximize) for the maximized zone, and must
 * keep the full button set; the default mode keeps the hover overlay.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const state = { maximizedZone: null as number | null };

vi.mock("./contexts", () => ({
  useTerminalSession: () => ({
    zoneLayout: {
      assignments: { 0: "tab-0", 1: "tab-1" },
      get maximizedZone() {
        return state.maximizedZone;
      },
      toggleMaximize: () => {},
    },
    closeTerminal: () => {},
    sessionStates: { "tab-0": "completed", "tab-1": "working" },
  }),
  useTransitionEffects: () => ({ handleRestartInZone: () => {} }),
  useZoneMetadata: () => ({ labelsAndTags: { zoneLabels: {}, setZoneLabel: () => {} } }),
}));
vi.mock("./contexts/WindowAssignmentsContext", () => ({
  useWindowAssignments: () => ({ ownerOf: () => "main" }),
}));
vi.mock("./useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({
    popOutTab: async () => "term-2",
    moveTabToWindow: async () => {},
    listWindows: async () => [],
  }),
}));

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { ZoneHoverActions, hoverActionsSpanPx } from "./ZoneHoverActions";
import {
  ZONE_HEADER_HOVER_RESERVE_CLASS,
  ZONE_HEADER_HOVER_RESERVE_PX,
} from "./zone-grid/headerReserve";

const render = (props: { zoneIdx: number; inline?: boolean }) =>
  renderToStaticMarkup(<ZoneHoverActions labelZone={props.zoneIdx} {...props} />);

describe("ZoneHoverActions", () => {
  it("default mode is the hover-revealed overlay", () => {
    state.maximizedZone = null;
    const html = render({ zoneIdx: 0 });
    expect(html).toContain("absolute top-1 right-1");
    expect(html).toContain("opacity-0");
    expect(html).toContain("group-hover:opacity-100");
  });

  it("inline mode is a static, always-visible cluster", () => {
    state.maximizedZone = 0;
    const html = render({ zoneIdx: 0, inline: true });
    expect(html).toContain('data-page-element="zone-hover-actions"');
    expect(html).not.toContain("absolute top-1 right-1");
    expect(html).not.toContain("opacity-0");
    expect(html).not.toContain("pointer-events-none");
  });

  it("shows Restore, not Maximize, for the maximized zone", () => {
    state.maximizedZone = 0;
    const html = render({ zoneIdx: 0, inline: true });
    expect(html).toContain('aria-label="Restore zone"');
    expect(html).not.toContain('aria-label="Maximize zone"');
  });

  it("shows Maximize for a zone that is not the maximized one", () => {
    state.maximizedZone = 1;
    const html = render({ zoneIdx: 0 });
    expect(html).toContain('aria-label="Maximize zone"');
  });

  it("keeps the full button set in inline mode", () => {
    state.maximizedZone = 0;
    const html = render({ zoneIdx: 0, inline: true });
    for (const label of [
      "Restore zone",
      "Restart session",
      "Edit zone label",
      "Export zone output",
      "Send terminal to a window",
      "Close session",
    ]) {
      expect(html).toContain(`aria-label="${label}"`);
    }
  });

  it("gates Restart on the session state", () => {
    state.maximizedZone = 0;
    // tab-0 is `completed`: restart is enabled.
    expect(render({ zoneIdx: 0, inline: true })).toContain('aria-disabled="false"');
    // tab-1 is `working`: restart is disabled.
    expect(render({ zoneIdx: 1, inline: true })).toContain('aria-disabled="true"');
  });
});

/**
 * Plan `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls`
 * Phase 2 / F3: the revealed overlay must not cover the zone header's cwd
 * label. The header pads its right end by `ZONE_HEADER_HOVER_RESERVE_PX` while
 * the cell is hovered; these tests tie that number to the cluster as rendered,
 * so adding a button (or widening one) without growing the reserve fails here.
 */
describe("header reserve for the hover overlay", () => {
  it("covers the widest overlay this component renders", () => {
    // Zone 0, not maximized, a labelable zone: every button is offered.
    state.maximizedZone = null;
    const html = render({ zoneIdx: 0 });
    const buttons = (html.match(/<button/g) ?? []).length;
    expect(buttons).toBe(6);
    // The classes `hoverActionsSpanPx` is derived from. Widening a button or
    // the cluster's padding changes one of these and fails here.
    expect(html).toContain("absolute top-1 right-1");
    expect(html).toContain("flex items-center gap-0.5 px-1 py-0.5 bg-[#13141f]/90 border ");
    expect((html.match(/class="p-1 rounded/g) ?? []).length).toBe(buttons);
    expect((html.match(/ w-3 h-3"/g) ?? []).length).toBe(buttons);
    expect(hoverActionsSpanPx(buttons)).toBe(144);
    expect(hoverActionsSpanPx(buttons)).toBeLessThanOrEqual(ZONE_HEADER_HOVER_RESERVE_PX);
  });

  it("the class carries the same number and applies on group hover", () => {
    expect(ZONE_HEADER_HOVER_RESERVE_CLASS.split(" ")).toContain(
      `group-hover:pr-[${ZONE_HEADER_HOVER_RESERVE_PX}px]`,
    );
  });

  it("the zone header applies the reserve and keeps the cwd label reachable", () => {
    const label = readFileSync(
      fileURLToPath(new URL("./zone-grid/ZoneLabel.tsx", import.meta.url)),
      "utf8",
    );
    expect(label).toContain("${ZONE_HEADER_HOVER_RESERVE_CLASS}");
    // The cwd span carries the full path as its title and takes pointer
    // events, or the title could never show.
    const marker = label.indexOf("data-zone-cwd-label");
    expect(marker).toBeGreaterThan(200);
    const cwd = label.slice(marker - 200, marker);
    expect(cwd).toContain("title={tab.workingDir}");
    expect(cwd).not.toContain("pointer-events-none");
  });

  it("the overlay is anchored where the reserve assumes", () => {
    state.maximizedZone = null;
    expect(render({ zoneIdx: 0 })).toContain("absolute top-1 right-1");
  });
});
