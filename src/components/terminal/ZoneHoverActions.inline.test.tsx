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
    tabs: [],
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

import {
  ZoneHoverActions,
  HEADER_ICON_TOGGLES_MAX,
  HEADER_TRUNCATABLES_MAX,
  headerControlsMinPx,
  hoverActionsSpanPx,
} from "./ZoneHoverActions";
import {
  ZONE_CELL_CONTAINER_CLASS,
  ZONE_HEADER_COLLAPSED_RESERVE_PX,
  ZONE_HEADER_HOVER_RESERVE_CLASS,
  ZONE_HEADER_HOVER_RESERVE_PX,
  ZONE_HOVER_COLLAPSE_BELOW_PX,
  ZONE_HOVER_COLLAPSED_ONLY_CLASS,
  ZONE_HOVER_FULL_ONLY_CLASS,
} from "./zone-grid/headerReserve";

const render = (props: { zoneIdx: number; inline?: boolean }) =>
  renderToStaticMarkup(<ZoneHoverActions labelZone={props.zoneIdx} {...props} />);

const SIX_ACTIONS = [
  "Maximize zone",
  "Restart session",
  "Edit zone label",
  "Export zone output",
  "Send terminal to a window",
  "Close session",
] as const;

/** The overlay's full cluster: everything before the narrow-zone overflow wrapper. */
const fullCluster = (html: string) => {
  const at = html.indexOf("data-zone-hover-overflow");
  return at < 0 ? html : html.slice(0, html.lastIndexOf("<div", at));
};

const readSource = (rel: string) =>
  readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

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
    // The narrow-zone overflow button and its menu follow the six; they are
    // display:none at this width and covered by the collapsed reserve below.
    const html = fullCluster(render({ zoneIdx: 0 }));
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

/**
 * Plan `2026-10-22-runner-zone-header-total-output-seed-and-narrow-zone-chips`
 * Phase 2: in a narrow zone the header's non-truncating controls must not sit
 * under the revealed cluster. Below `ZONE_HOVER_COLLAPSE_BELOW_PX` (a container
 * query on the cell) the cluster collapses to one overflow button and the
 * reserve shrinks to it; the text chips truncate instead of holding width.
 */
describe("narrow zones collapse the hover cluster", () => {
  it("the collapse width is the header's minimum plus padding plus the full reserve", () => {
    expect(HEADER_ICON_TOGGLES_MAX).toBe(6);
    expect(HEADER_TRUNCATABLES_MAX).toBe(4);
    expect(headerControlsMinPx()).toBe(162);
    expect(ZONE_HOVER_COLLAPSE_BELOW_PX).toBe(
      headerControlsMinPx() + 16 + ZONE_HEADER_HOVER_RESERVE_PX,
    );
    expect(headerControlsMinPx() + 16 + ZONE_HEADER_HOVER_RESERVE_PX).toBeLessThanOrEqual(
      ZONE_HOVER_COLLAPSE_BELOW_PX,
    );
  });

  it("a 250 px zone fits every header control beside the collapsed cluster", () => {
    expect(ZONE_HEADER_COLLAPSED_RESERVE_PX).toBe(hoverActionsSpanPx(1) + 4);
    expect(headerControlsMinPx() + 16 + ZONE_HEADER_COLLAPSED_RESERVE_PX).toBeLessThanOrEqual(250);
  });

  it("the classes carry the same numbers against the named zone container", () => {
    const t = ZONE_HOVER_COLLAPSE_BELOW_PX;
    expect(ZONE_CELL_CONTAINER_CLASS).toBe("@container/zone");
    expect(ZONE_HEADER_HOVER_RESERVE_CLASS.split(" ")).toContain(
      `@max-[${t}px]/zone:group-hover:pr-[${ZONE_HEADER_COLLAPSED_RESERVE_PX}px]`,
    );
    expect(ZONE_HOVER_FULL_ONLY_CLASS).toBe(`@max-[${t}px]/zone:hidden`);
    expect(ZONE_HOVER_COLLAPSED_ONLY_CLASS.split(" ")).toEqual([
      "hidden",
      `@max-[${t}px]/zone:flex`,
    ]);
  });

  it("the zone cell is the named container", () => {
    const cell = readSource("./zone-grid/ZoneCell.tsx");
    expect(cell).toMatch(/className=\{`group \$\{ZONE_CELL_CONTAINER_CLASS\} relative/);
  });

  it("the header classes headerControlsMinPx is derived from", () => {
    const label = readSource("./zone-grid/ZoneLabel.tsx");
    // Row gap and padding, the state dot, and the icon toggles' size.
    expect(label).toContain("flex items-center gap-1.5 px-2 ${ZONE_HEADER_HOVER_RESERVE_CLASS}");
    expect(label).toContain("className={`w-1.5 h-1.5 rounded-full shrink-0");
    const toggleIcons =
      label.match(
        /<(ChevronDown|Pin|Filter|MessageSquare|ArrowDownToLine) className="w-2\.5 h-2\.5" \/>/g,
      ) ?? [];
    expect(toggleIcons.length).toBe(HEADER_ICON_TOGGLES_MAX - 1); // + the review toggle
  });

  it("the total-output label and the position chip truncate instead of holding width", () => {
    const label = readSource("./zone-grid/ZoneLabel.tsx");
    const total = label.slice(
      label.indexOf("export function TotalOutputLabel"),
      label.indexOf("data-zone-total-output"),
    );
    expect(total).toContain(
      'className="text-[9px] text-[#565f89] font-mono ml-1 min-w-0 truncate"',
    );
    expect(total).not.toContain("shrink-0");
    expect(total).toContain("title={");
    const posAt = label.indexOf("data-tmux-position");
    const position = label.slice(label.lastIndexOf("<span", posAt), posAt);
    expect(position).toContain("min-w-0 truncate");
    expect(position).not.toContain("shrink-0");
    expect(position).toContain("title={");
  });

  it("the overlay hides each of the six below the collapse width", () => {
    state.maximizedZone = null;
    const full = fullCluster(render({ zoneIdx: 0 }));
    for (const label of SIX_ACTIONS) {
      const at = full.indexOf(`aria-label="${label}"`);
      expect(at).toBeGreaterThan(-1);
      // The class sits on the button, or on the wrapper of a button with a dropdown.
      const opener = full.lastIndexOf("<button", at);
      const scope = full.slice(Math.max(0, full.lastIndexOf("<div", opener)), at);
      expect(scope).toContain(ZONE_HOVER_FULL_ONLY_CLASS);
    }
  });

  it("collapses to exactly one overflow button whose menu holds all six", () => {
    state.maximizedZone = null;
    const html = render({ zoneIdx: 0 });
    const at = html.indexOf("data-zone-hover-overflow");
    expect(at).toBeGreaterThan(-1);
    const collapsed = html.slice(html.lastIndexOf("<div", at));
    expect(collapsed).toContain(ZONE_HOVER_COLLAPSED_ONLY_CLASS);
    expect((collapsed.match(/aria-haspopup="menu"/g) ?? []).length).toBe(1);
    expect((html.match(/aria-haspopup="menu"/g) ?? []).length).toBe(1);
    expect(collapsed).toMatch(
      /aria-label="Zone actions" aria-haspopup="menu" aria-expanded="false"/,
    );
    const menu = collapsed.slice(collapsed.indexOf('role="menu"'));
    expect(menu.slice(0, menu.indexOf(">"))).toContain('hidden=""'); // closed until opened
    for (const label of SIX_ACTIONS) {
      expect(menu).toContain(`aria-label="${label}"`);
    }
    expect((menu.match(/role="menuitem"/g) ?? []).length).toBe(SIX_ACTIONS.length);
  });

  it("the maximized header's inline cluster never collapses", () => {
    state.maximizedZone = 0;
    const html = render({ zoneIdx: 0, inline: true });
    expect(html).not.toContain("data-zone-hover-overflow");
    expect(html).not.toContain(ZONE_HOVER_FULL_ONLY_CLASS);
  });
});
