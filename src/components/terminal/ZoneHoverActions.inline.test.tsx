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

const state = { maximizedZone: null as number | null, remote: false };

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
    get tabs() {
      return state.remote
        ? [
            {
              id: "tab-0",
              title: "t",
              remote: { deviceId: "dev-1234", deviceLabel: "box", sessionId: "s" },
            },
          ]
        : [];
    },
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

import { ZoneHoverActions, headerControlsMinPx, hoverActionsSpanPx } from "./ZoneHoverActions";
import {
  ZONE_CELL_CONTAINER_CLASS,
  ZONE_HEADER_COLLAPSED_RESERVE_PX,
  ZONE_HEADER_COLLAPSED_RESERVE_REMOTE_PX,
  ZONE_HEADER_HOVER_RESERVE_CLASS,
  ZONE_HEADER_HOVER_RESERVE_PX,
  ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS,
  ZONE_HEADER_HOVER_RESERVE_REMOTE_PX,
  ZONE_HOVER_COLLAPSE_BELOW_PX,
  ZONE_HOVER_COLLAPSE_BELOW_REMOTE_PX,
  ZONE_HOVER_COLLAPSED_ONLY_CLASS,
  ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS,
  ZONE_HOVER_FULL_ONLY_CLASS,
  ZONE_HOVER_FULL_ONLY_REMOTE_CLASS,
  zoneHeaderReserveClass,
} from "./zone-grid/headerReserve";

const render = (props: { zoneIdx: number; inline?: boolean }) =>
  renderToStaticMarkup(<ZoneHoverActions labelZone={props.zoneIdx} {...props} />);

/** The five actions the overflow menu stands in for in a narrow zone. */
const COLLAPSIBLE_ACTIONS = [
  "Maximize zone",
  "Restart session",
  "Edit zone label",
  "Export zone output",
  "Send terminal to a window",
] as const;
/** Actions carried by a wrapper div (they own a dropdown) rather than the button itself. */
const WRAPPED = new Set(["Export zone output", "Send terminal to a window"]);

const readSource = (rel: string) =>
  readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

/** The opening tag of the element carrying `marker`. */
const openingTag = (html: string, marker: string, tag = "<button") => {
  const at = html.indexOf(marker);
  expect(at).toBeGreaterThan(-1);
  return html.slice(html.lastIndexOf(tag, at), html.indexOf(">", at) + 1);
};

/** The overflow wrapper: its whole element, menu included (balanced `<div>`s). */
const overflowRegion = (html: string) => {
  const at = html.indexOf("data-zone-hover-overflow");
  expect(at).toBeGreaterThan(-1);
  const start = html.lastIndexOf("<div", at);
  const tags = /<div[\s>]|<\/div>/g;
  tags.lastIndex = start;
  let depth = 0;
  for (let m = tags.exec(html); m; m = tags.exec(html)) {
    depth += m[0] === "</div>" ? -1 : 1;
    if (depth === 0) return html.slice(start, m.index + m[0].length);
  }
  throw new Error("unbalanced overflow wrapper");
};

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
 * label. The header pads its right end by the reserve while the cell is
 * hovered; these tests tie that number to the cluster as rendered, so adding a
 * button (or widening one) without growing the reserve fails here.
 */
describe("header reserve for the hover overlay", () => {
  it("covers the widest overlay this component renders", () => {
    // Zone 0, not maximized, a labelable zone: every button is offered.
    state.maximizedZone = null;
    state.remote = false;
    const html = render({ zoneIdx: 0 });
    // Everything outside the narrow-zone overflow wrapper (display:none at this width).
    const overflow = overflowRegion(html);
    const full = html.replace(overflow, "");
    const buttons = (full.match(/<button/g) ?? []).length;
    expect(buttons).toBe(6);
    // The classes `hoverActionsSpanPx` is derived from. Widening a button or
    // the cluster's padding changes one of these and fails here.
    expect(full).toContain("absolute top-1 right-1");
    expect(full).toContain("flex items-center gap-0.5 px-1 py-0.5 bg-[#13141f]/90 border ");
    expect((full.match(/class="p-1 rounded/g) ?? []).length).toBe(buttons);
    expect((full.match(/ w-3 h-3"/g) ?? []).length).toBe(buttons);
    expect(hoverActionsSpanPx(buttons)).toBe(144);
    expect(hoverActionsSpanPx(buttons) + 4).toBe(ZONE_HEADER_HOVER_RESERVE_PX);
  });

  it("a remote tab's seventh button has its own reserve", () => {
    state.maximizedZone = null;
    state.remote = true;
    const html = render({ zoneIdx: 0 });
    state.remote = false;
    const full = html.replace(overflowRegion(html), "");
    expect(full).toContain('aria-label="End on remote session"');
    expect((full.match(/<button/g) ?? []).length).toBe(7);
    expect(hoverActionsSpanPx(7) + 4).toBe(ZONE_HEADER_HOVER_RESERVE_REMOTE_PX);
  });

  it("the classes carry the same numbers and apply on group hover", () => {
    expect(ZONE_HEADER_HOVER_RESERVE_CLASS.split(" ")).toContain(
      `group-hover:pr-[${ZONE_HEADER_HOVER_RESERVE_PX}px]`,
    );
    expect(ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS.split(" ")).toContain(
      `group-hover:pr-[${ZONE_HEADER_HOVER_RESERVE_REMOTE_PX}px]`,
    );
    expect(zoneHeaderReserveClass(false)).toBe(ZONE_HEADER_HOVER_RESERVE_CLASS);
    expect(zoneHeaderReserveClass(true)).toBe(ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS);
  });

  it("the zone header applies the reserve for its tab and keeps the cwd label reachable", () => {
    const label = readSource("./zone-grid/ZoneLabel.tsx");
    expect(label).toContain("${zoneHeaderReserveClass(!!tab.remote)}");
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

/**
 * Plan `2026-10-22-runner-zone-header-total-output-seed-and-narrow-zone-chips`
 * Phase 2: in a narrow zone the header's non-truncating controls must not sit
 * under the revealed cluster. Below the collapse width (a container query on
 * the cell) five actions move into one overflow menu and the reserve shrinks to
 * the collapsed cluster (overflow + Close); the text chips truncate instead of
 * holding width. `zoneLabelRow.test.tsx` ties `headerControlsMinPx`'s item table
 * to the header as rendered.
 */
describe("narrow zones collapse the hover cluster", () => {
  it("the collapse width is left padding + the header's minimum + the full reserve", () => {
    expect(headerControlsMinPx()).toBe(256);
    expect(headerControlsMinPx(true)).toBe(322);
    // The hover padding replaces only the row's right `px-2`; the left 8 stays.
    expect(ZONE_HOVER_COLLAPSE_BELOW_PX).toBe(
      8 + headerControlsMinPx() + ZONE_HEADER_HOVER_RESERVE_PX,
    );
    expect(ZONE_HOVER_COLLAPSE_BELOW_REMOTE_PX).toBe(
      8 + headerControlsMinPx(true) + ZONE_HEADER_HOVER_RESERVE_REMOTE_PX,
    );
  });

  it("the collapsed reserve covers the overflow button and Close (and a remote End)", () => {
    expect(ZONE_HEADER_COLLAPSED_RESERVE_PX).toBe(hoverActionsSpanPx(2) + 4);
    expect(ZONE_HEADER_COLLAPSED_RESERVE_REMOTE_PX).toBe(hoverActionsSpanPx(3) + 4);
    // Known limit (headerReserve.ts): with every header item present, the
    // collapsed layout still needs this much; narrower zones overlap.
    expect(8 + headerControlsMinPx() + ZONE_HEADER_COLLAPSED_RESERVE_PX).toBe(324);
    expect(8 + headerControlsMinPx(true) + ZONE_HEADER_COLLAPSED_RESERVE_REMOTE_PX).toBe(412);
    expect(8 + headerControlsMinPx() + ZONE_HEADER_COLLAPSED_RESERVE_PX).toBeLessThan(
      ZONE_HOVER_COLLAPSE_BELOW_PX,
    );
  });

  it("the classes carry the same numbers against the named zone container", () => {
    const t = ZONE_HOVER_COLLAPSE_BELOW_PX;
    const tr = ZONE_HOVER_COLLAPSE_BELOW_REMOTE_PX;
    expect(ZONE_CELL_CONTAINER_CLASS).toBe("@container/zone");
    expect(ZONE_HEADER_HOVER_RESERVE_CLASS.split(" ")).toContain(
      `@max-[${t}px]/zone:group-hover:pr-[${ZONE_HEADER_COLLAPSED_RESERVE_PX}px]`,
    );
    expect(ZONE_HEADER_HOVER_RESERVE_REMOTE_CLASS.split(" ")).toContain(
      `@max-[${tr}px]/zone:group-hover:pr-[${ZONE_HEADER_COLLAPSED_RESERVE_REMOTE_PX}px]`,
    );
    expect(ZONE_HOVER_FULL_ONLY_CLASS).toBe(`@max-[${t}px]/zone:hidden`);
    expect(ZONE_HOVER_FULL_ONLY_REMOTE_CLASS).toBe(`@max-[${tr}px]/zone:hidden`);
    expect(ZONE_HOVER_COLLAPSED_ONLY_CLASS.split(" ")).toEqual([
      "hidden",
      `@max-[${t}px]/zone:flex`,
    ]);
    expect(ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS.split(" ")).toEqual([
      "hidden",
      `@max-[${tr}px]/zone:flex`,
    ]);
  });

  it("the zone cell is the named container", () => {
    const cell = readSource("./zone-grid/ZoneCell.tsx");
    expect(cell).toMatch(/className=\{`group \$\{ZONE_CELL_CONTAINER_CLASS\} relative/);
  });

  it("hides each collapsible action, on its own tag, below the collapse width", () => {
    state.maximizedZone = null;
    state.remote = false;
    const html = render({ zoneIdx: 0 });
    for (const label of COLLAPSIBLE_ACTIONS) {
      const tag = WRAPPED.has(label)
        ? openingTag(html, `aria-label="${label}"`, '<div class="relative')
        : openingTag(html, `aria-label="${label}"`);
      expect(tag, label).toContain(ZONE_HOVER_FULL_ONLY_CLASS);
    }
  });

  it("keeps Close, and its UI Bridge id, visible in the collapsed cluster", () => {
    state.maximizedZone = null;
    const close = openingTag(render({ zoneIdx: 0 }), 'aria-label="Close session"');
    expect(close).toContain('data-ui-bridge-id="terminal.close-session-tab-0"');
    expect(close).not.toContain("/zone:hidden");
  });

  it("a remote tab uses the remote collapse width and keeps its End button", () => {
    state.maximizedZone = null;
    state.remote = true;
    const html = render({ zoneIdx: 0 });
    state.remote = false;
    expect(openingTag(html, 'aria-label="Restart session"')).toContain(
      ZONE_HOVER_FULL_ONLY_REMOTE_CLASS,
    );
    expect(openingTag(html, "data-zone-hover-overflow", "<div")).toContain(
      ZONE_HOVER_COLLAPSED_ONLY_REMOTE_CLASS,
    );
    expect(openingTag(html, 'aria-label="End on remote session"')).not.toContain("/zone:hidden");
  });

  it("collapses to exactly one overflow button whose menu holds the five by visible text", () => {
    state.maximizedZone = null;
    state.remote = false;
    const html = render({ zoneIdx: 0 });
    const overflow = overflowRegion(html);
    expect(openingTag(html, "data-zone-hover-overflow", "<div")).toContain(
      ZONE_HOVER_COLLAPSED_ONLY_CLASS,
    );
    expect((html.match(/aria-haspopup="menu"/g) ?? []).length).toBe(1);
    const menu = overflow.slice(overflow.indexOf('role="menu"'));
    expect(openingTag(overflow, 'role="menu"', "<div")).toContain('hidden=""');
    const items = menu.match(/role="menuitem"[^>]*>(?:<svg[\s\S]*?<\/svg>)?([^<]*)</g) ?? [];
    expect(items.map((m) => m.replace(/[\s\S]*>([^<]*)<$/, "$1"))).toEqual([
      ...COLLAPSIBLE_ACTIONS,
    ]);
    // Menu items are named by their text: no aria-label to collide with the
    // (hidden) full-cluster buttons' names.
    expect(menu.slice(0, menu.indexOf("</div>"))).not.toMatch(/role="menuitem"[^>]*aria-label=/);
  });

  it("the maximized header's inline cluster never collapses", () => {
    state.maximizedZone = 0;
    const html = render({ zoneIdx: 0, inline: true });
    expect(html).not.toContain("data-zone-hover-overflow");
    expect(html).not.toContain("/zone:hidden");
  });
});
