// @vitest-environment jsdom
/**
 * The zone header row, rendered with every item present, against
 * `headerControlsMinPx`'s item table (plan
 * `2026-10-22-runner-zone-header-total-output-seed-and-narrow-zone-chips`
 * Phase 2). Every direct child of the row either holds its width (`shrink-0`,
 * one entry in `HEADER_FIXED_ITEMS_PX`) or truncates (one entry in
 * `HEADER_TRUNCATING_FLOORS_PX`). A new `shrink-0` item, or one that stops
 * truncating, changes the counts and fails here — so the collapse width in
 * `headerReserve.ts` cannot silently fall behind the header.
 *
 * A remote tab's row adds `RemoteTabControls`' items, budgeted by
 * `HEADER_REMOTE_FLOORS_PX`. `TenantBadge` (multi-tenant devices) is stubbed to
 * nothing: `headerControlsMinPx` does not budget it.
 */

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  // The remote "earlier output" action fails, which leaves its inline note up;
  // every other read stays pending.
  invoke: vi.fn((cmd: string) =>
    cmd === "terminal_remote_history_load"
      ? Promise.resolve({ success: false, message: "no earlier output" })
      : new Promise(() => {}),
  ),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@qontinui/ui-bridge", () => ({ useUIElement: () => ({ ref: { current: null } }) }));
vi.mock("../TenantBadge", () => ({ TenantBadge: () => null }));
vi.mock("../contexts/TerminalSessionContext", () => ({ useTerminalSession: () => ({}) }));
vi.mock("../useRemoteAttachWaiting", () => ({ useRemoteAttachWaiting: () => ({}) }));
vi.mock("../useRemoteInteractivity", () => ({ useRemoteInteractivity: () => [{}, 0] }));
vi.mock("../remoteTabs", async (orig) => ({
  ...(await orig<typeof import("../remoteTabs")>()),
  remoteInteractivityFooter: () => ({
    summary: "last output received 2s ago · keystrokes sent, none acknowledged yet",
    note: "input not acknowledged",
    noteKind: "warning",
  }),
}));
vi.mock("../useTerminalWindowActions", () => ({
  useTerminalWindowActions: () => ({ popOutTab: () => Promise.resolve() }),
}));
vi.mock("../displayTitle", () => ({
  useDisplayTitle: () => "Session name",
  TabTitle: () => null,
}));
vi.mock("../sessionDurability", async (orig) => ({
  ...(await orig<typeof import("../sessionDurability")>()),
  isNonDurablePty: () => true,
}));

import { ZoneLabel } from "./ZoneLabel";
import { SessionReviewToggle } from "../SessionReviewPanel";
import {
  HEADER_FIXED_ITEMS_PX,
  HEADER_REMOTE_FLOORS_PX,
  HEADER_TRUNCATING_FLOORS_PX,
  headerControlsMinPx,
} from "../ZoneHoverActions";
import type { TerminalTab } from "../useTerminalManager";

let container: HTMLDivElement;
let root: Root;

const tab = {
  id: "t1",
  title: "Session name",
  workingDir: "/home/u/project",
  claudeSessionId: "abcdef12-0000-0000-0000-000000000000",
} as unknown as TerminalTab;
const other = { ...tab, id: "t2", claudeSessionId: undefined } as unknown as TerminalTab;
const remoteTab = {
  ...tab,
  isAlive: true,
  remote: {
    deviceId: "dev-1234-5678",
    deviceLabel: "far box",
    sessionId: "s1",
    historyAvailable: true,
  },
} as unknown as TerminalTab;

function renderRow(zoneLabel?: string, shown: TerminalTab = tab): HTMLElement {
  act(() => {
    root.render(
      createElement(ZoneLabel, {
        tab: shown,
        state: "working",
        zoneIndex: 0,
        allTabs: [tab, other],
        assignments: { 0: "t1" },
        sessionStates: {},
        onAssignTab: () => {},
        onTogglePin: () => {},
        zoneLabel,
        onSetZoneLabel: () => {},
        onScrollToBottom: () => {},
        totalOutputBytes: 376_950,
        onToggleFilter: () => {},
        onTogglePrompts: () => {},
        reviewSlot: createElement(SessionReviewToggle, {
          open: false,
          onToggle: () => {},
          badge: {
            kind: "count",
            unread: 3,
            total: 3,
            unknownFiles: 0,
            lowerBound: false,
            text: "3 unread",
            title: "3 unread files",
            stale: false,
          } as never,
        }),
        position: { index: 2, count: 5 },
      }),
    );
  });
  return container.querySelector<HTMLElement>('[data-zone-index="0"]')!;
}

const classes = (el: Element) => (el.getAttribute("class") ?? "").split(/\s+/);
const holdsWidth = (el: Element) => classes(el).includes("shrink-0");
const truncates = (el: Element) =>
  classes(el).some((c) => c === "truncate" || /^min-w-(\d|\[)/.test(c));

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("zone header row against headerControlsMinPx", () => {
  it("every item either holds its width (budgeted) or truncates", () => {
    const row = renderRow();
    const children = Array.from(row.children);
    const fixed = children.filter(holdsWidth);
    const rest = children.filter((el) => !holdsWidth(el));
    expect(fixed).toHaveLength(Object.keys(HEADER_FIXED_ITEMS_PX).length);
    for (const el of rest) expect(truncates(el), el.outerHTML.slice(0, 120)).toBe(true);
    expect(rest).toHaveLength(Object.keys(HEADER_TRUNCATING_FLOORS_PX).length);
    // One gap per adjacent pair of the items the table budgets.
    expect(children).toHaveLength(
      Object.keys(HEADER_FIXED_ITEMS_PX).length + Object.keys(HEADER_TRUNCATING_FLOORS_PX).length,
    );
    expect(headerControlsMinPx()).toBe(256);
  });

  it("a remote tab's row adds only truncating items, each budgeted", async () => {
    let row = renderRow(undefined, remoteTab);
    // Fail the "earlier output" action so its inline note shows too.
    const earlier = row.querySelector<HTMLButtonElement>(
      '[data-ui-bridge-id="terminal.remote-history.t1"]',
    )!;
    await act(async () => {
      earlier.click();
      await Promise.resolve();
    });
    row = container.querySelector<HTMLElement>('[data-zone-index="0"]')!;
    const children = Array.from(row.children);
    expect(children.filter(holdsWidth)).toHaveLength(Object.keys(HEADER_FIXED_ITEMS_PX).length);
    const rest = children.filter((el) => !holdsWidth(el));
    for (const el of rest) expect(truncates(el), el.outerHTML.slice(0, 120)).toBe(true);
    expect(rest).toHaveLength(
      Object.keys(HEADER_TRUNCATING_FLOORS_PX).length + Object.keys(HEADER_REMOTE_FLOORS_PX).length,
    );
    // The device badge and the action keep their full text in their title.
    const badge = row.querySelector('[data-ui-bridge-id="terminal.remote-badge.t1"]')!;
    expect(badge.getAttribute("title")).toContain("dev-1234-5678");
    expect(classes(earlier)).toContain("min-w-[18px]");
    expect(earlier.getAttribute("title")).toBeTruthy();
    expect(row.querySelector('[data-ui-bridge-id="terminal.remote-note.t1"]')?.textContent).toBe(
      "no earlier output",
    );
    expect(headerControlsMinPx(true)).toBe(312);
  });

  it("the session-info count is capped so its budget holds", () => {
    const row = renderRow();
    const count = row.querySelector("[data-session-info-summary] span")!;
    expect(classes(count)).toEqual(expect.arrayContaining(["max-w-[2ch]", "truncate"]));
  });

  it("the row keeps the gap and padding the table and reserve assume", () => {
    const cls = classes(renderRow());
    expect(cls).toContain("gap-1.5");
    expect(cls).toContain("px-2");
    expect(cls).toContain("group-hover:pr-[148px]");
  });

  it("the total-output label truncates and keeps its full figure in its title", () => {
    const total = renderRow().querySelector("[data-zone-total-output]")!;
    expect(holdsWidth(total)).toBe(false);
    expect(classes(total)).toEqual(expect.arrayContaining(["min-w-0", "truncate"]));
    expect(total.getAttribute("title")).toContain("376950 bytes");
  });

  it("the tmux position chip truncates and keeps the position in its title", () => {
    const pos = renderRow().querySelector("[data-tmux-position]")!;
    expect(holdsWidth(pos)).toBe(false);
    expect(classes(pos)).toEqual(expect.arrayContaining(["min-w-0", "truncate"]));
    expect(pos.getAttribute("title")).toContain("session 2 of 5");
  });

  it("a set zone label truncates and keeps the label in its title", () => {
    const row = renderRow("a fairly long zone label, with tags");
    const chip = Array.from(row.children).find((el) =>
      el.textContent?.startsWith("a fairly long"),
    )!;
    expect(holdsWidth(chip)).toBe(false);
    expect(classes(chip)).toEqual(expect.arrayContaining(["min-w-0", "truncate"]));
    expect(chip.getAttribute("title")).toContain("a fairly long zone label, with tags");
  });
});
