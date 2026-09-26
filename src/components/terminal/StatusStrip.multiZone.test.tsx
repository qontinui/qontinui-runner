/**
 * `StatusStrip` — the reported scenario, end to end.
 *
 * THE DEFECT: two live PTY tabs with no Claude session attached to either, and
 * the whole status surface refused to render. `hasContent` read a UNIONed
 * `errorCount` (see `unionErrorCount`) right beside a bare
 * `isMultiZone = sessionCount > 1`, and `sessionCount` comes from the
 * Claude-session bucketing — so every input to the auto-hide gate scored 0 on a
 * page that visibly had two terminals in it.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom, no
 * `@testing-library/react` — see `StewardControl.test.tsx`), so the render goes
 * through `react-dom/server` exactly as `StreamingMessageView.test.tsx` does.
 * `renderToStaticMarkup` runs no effects, which is all this case needs: the
 * auto-hide gate is evaluated during the initial render.
 *
 * ## The precondition is asserted FIRST, deliberately
 *
 * The 2026-08-23 vet of this fix found the terminal-session roster reading
 * `"[]"` while two PTYs were alive — HTTP-created terminals were not in `tabs`
 * at all. That blocker has since landed (`record_close_checked` + the
 * `record_open` supersede), but a test that only asserts `hasContent` would
 * pass against a no-op fix if the input ever went empty again. So every case
 * below proves the tab-derived input is non-empty BEFORE it asserts on the
 * output — through `buildTerminalSessionRoster`, the same projection the page
 * publishes as `[data-page-element=terminal-session-roster]`.
 */

import { describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// ---------------------------------------------------------------------------
// Module mocks — hoisted above the component import.
// ---------------------------------------------------------------------------

const sessionValue: Record<string, unknown> = {};

vi.mock("./contexts", () => ({
  useTerminalSession: () => sessionValue,
  useZoneMetadata: () => ({
    labelsAndTags: {
      activeTagFilters: new Set<string>(),
      setActiveTagFilters: () => {},
    },
  }),
}));

vi.mock("./useTerminalHotStore", () => ({
  useHotField: () => ({}),
}));

vi.mock("@/hooks/useWrapperTools", () => ({
  useWrapperTools: () => ({
    tools: [],
    routes: [],
    wrappers: [],
    loading: false,
    error: null,
    refresh: () => {},
    dispatch: () => {},
  }),
}));

vi.mock("./BatchActions", () => ({ BatchActions: () => null }));
vi.mock("./MinimapToggle", () => ({ MinimapToggle: () => null }));

import { StatusStrip } from "./StatusStrip";
import { buildTerminalSessionRoster, type RosterTab } from "./terminalSessionRoster";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** Two PTY tabs, both alive — the reported page. */
const TWO_LIVE_TABS: RosterTab[] = [
  { id: "term-a", title: "PowerShell", isAlive: true, exitCode: null },
  { id: "term-b", title: "bash", isAlive: true, exitCode: null },
];

const zoneLayout = {
  layout: { zones: [{}, {}] },
  assignments: { 0: "term-a", 1: "term-b" },
  focusedZone: 0,
  maximizedZone: null,
  setFocusedZone: () => {},
  setMaximizedZone: () => {},
  focusNextNeedsInput: () => {},
  // The minimap's own predicate, deliberately independent of the strip's.
  isMultiZone: true,
};

/**
 * Every attention signal at zero and no plan loaded, so `isMultiSession` (the
 * strip's local, formerly misnamed `isMultiZone`) is the ONLY thing that can
 * keep the strip on screen.
 */
function mountScenario(
  tabs: RosterTab[],
  claudeSessionCount: number,
  working: { workingCount: number; externalWorkingCount: number } = {
    workingCount: 0,
    externalWorkingCount: 0,
  },
) {
  Object.assign(sessionValue, {
    tabs,
    sessionStates: {},
    pageId: "page-1",
    zoneLayout,
    workflowGen: { planFileName: null, isPlanLoading: false },
    sessionManager: {
      claudeSessionCount,
      needsInputCount: 0,
      errorCount: 0,
      ...working,
      completedCount: 0,
      idleCount: 0,
    },
  });
  return renderToStaticMarkup(<StatusStrip />);
}

describe("StatusStrip auto-hide gate", () => {
  it("renders for two live PTY tabs with zero Claude sessions — THE DEFECT", () => {
    // PRECONDITION, asserted before anything about the output: the tab-derived
    // input this fix reads is genuinely non-empty and genuinely live. Without
    // this, a regression emptying `tabs` would make the fix a silent no-op and
    // the assertion below would still pass for the wrong reason.
    const roster = buildTerminalSessionRoster(TWO_LIVE_TABS, zoneLayout.assignments, {});
    expect(roster).toHaveLength(2);
    expect(roster.every((r) => r.isAlive)).toBe(true);

    const html = mountScenario(TWO_LIVE_TABS, 0);

    // `hasContent` is true -> the strip renders instead of returning null.
    expect(html).toContain('data-page-element="status-strip"');
    // ...and it reports the number it is gated on, not the 0 the Claude-session
    // bucketing would have shown.
    expect(html).toContain("2 sessions");
    expect(html).not.toContain("0 sessions");
  });

  it("still hides on a genuinely empty page", () => {
    // The auto-hide principle survives the fix: no tabs, no sessions, no
    // signals -> nothing on screen.
    expect(buildTerminalSessionRoster([], {}, {})).toHaveLength(0);
    expect(mountScenario([], 0)).toBe("");
  });

  it("still hides for a single live tab with one session", () => {
    const one: RosterTab[] = [TWO_LIVE_TABS[0]];
    const roster = buildTerminalSessionRoster(one, zoneLayout.assignments, {});
    expect(roster).toHaveLength(1);

    expect(mountScenario(one, 1)).toBe("");
  });

  it("does not reopen the strip for two tabs whose PTYs both exited", () => {
    const dead: RosterTab[] = [
      { id: "term-a", title: "PowerShell", isAlive: false, exitCode: 0 },
      { id: "term-b", title: "bash", isAlive: false, exitCode: 1 },
    ];
    // Precondition: the roster does list them — they are present but dead, so
    // this case really is exercising the liveness filter and not an empty list.
    const roster = buildTerminalSessionRoster(dead, zoneLayout.assignments, {});
    expect(roster).toHaveLength(2);
    expect(roster.every((r) => r.isAlive)).toBe(false);

    expect(mountScenario(dead, 0)).toBe("");
  });

  it("keeps counting Claude sessions with no tab in this window", () => {
    // The union reads below neither input: two external sessions, no tabs.
    expect(buildTerminalSessionRoster([], {}, {})).toHaveLength(0);
    const html = mountScenario([], 2);
    expect(html).toContain('data-page-element="status-strip"');
    expect(html).toContain("2 sessions");
    // The tooltip must not claim external sessions are "on this page" — only
    // the live-terminal half of the union is scoped to this window.
    expect(html).toContain("2 sessions — 2 Claude (incl. external), 0 live terminals on this page");
    expect(html).not.toContain("2 sessions on this page");
  });
});

describe("StatusStrip working count (UI-4)", () => {
  /** The strip's visible text, tags stripped and whitespace collapsed. */
  function text(html: string): string {
    return html
      .replace(/<[^>]*>/g, " ")
      .replace(/\s+/g, " ")
      .trim();
  }

  it("headlines the page's workers and reports external sessions apart — THE DEFECT", () => {
    // One worker in a zone on this page, four other `claude` processes on the
    // box. `workingCount` buckets all five; the strip is the PAGE's status.
    const html = mountScenario(TWO_LIVE_TABS, 5, { workingCount: 5, externalWorkingCount: 4 });
    expect(text(html)).toContain("1 working +4 external");
    expect(text(html)).not.toContain("5 working");
  });

  it("omits the external suffix when every worker is on this page", () => {
    const html = mountScenario(TWO_LIVE_TABS, 2, { workingCount: 2, externalWorkingCount: 0 });
    expect(text(html)).toContain("2 working");
    expect(text(html)).not.toContain("external");
  });

  it("still reports external workers when none run on this page", () => {
    const html = mountScenario(TWO_LIVE_TABS, 3, { workingCount: 3, externalWorkingCount: 3 });
    expect(text(html)).toContain("0 working +3 external");
  });
});
