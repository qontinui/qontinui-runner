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

import { ZoneHoverActions } from "./ZoneHoverActions";

const render = (props: { zoneIdx: number; inline?: boolean }) =>
  renderToStaticMarkup(<ZoneHoverActions {...props} />);

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
