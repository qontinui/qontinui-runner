/**
 * The Session Manager header with the query bar mounted (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4): the grammar box, the chip row with counts and the pickers replace
 * the old activity chips, substring search box and account buttons, and the
 * header root carries the query in its dataset.
 *
 * `environment: "node"` with no DOM library in the shared `node_modules`, so —
 * as `SessionQueryBar.test.tsx` does — markup is locked with
 * `renderToStaticMarkup`, and a chip click is driven through the bar's
 * registered `toggle-chip` action (the same `toggleChip` the button's
 * `onClick` calls).
 */

import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { components } = vi.hoisted(() => ({
  components: [] as {
    id: string;
    actions: { id: string; effect: string; handler: (p?: unknown) => unknown }[];
  }[],
}));

vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: () => ({ ref: () => {} }),
  useUIComponent: (opts: (typeof components)[number]) => {
    components.push(opts);
  },
  UIBridgeComponentScope: ({ children }: { children: ReactNode }) => children,
}));

import { SessionManagerHeader, SESSION_MANAGER_HEADER_IDS } from "./SessionManagerHeader";
import { SESSION_QUERY_COMPONENT_ID, type SessionQueryBarProps } from "./SessionQueryBar";
import { known, unknownFact } from "./sessionFacts";
import { NOW, facts } from "./sessionFacts.testkit";
import { parseQuery } from "./sessionQuery";
import { QUERY_CHIPS, chipCounts } from "./sessionQueryChips";
import { SESSION_ACTIVITIES, type SessionActivity } from "./sessionStatusModel";

beforeEach(() => {
  components.length = 0;
});

const ROWS = [
  facts({ rowId: "a", activity: known("needs-you") }),
  facts({ rowId: "b", activity: known("needs-you") }),
  facts({ rowId: "c", activity: known("working"), pinned: known(true) }),
  facts({ rowId: "d", activity: unknownFact("no digest yet") }),
];

function queryBar(query: string, over: Partial<SessionQueryBarProps> = {}): SessionQueryBarProps {
  return {
    query,
    onQueryChange: vi.fn(),
    parsed: parseQuery(query),
    chipCounts: chipCounts(ROWS, { now: NOW }),
    accounts: ["gmail", "hotmail"],
    groups: [],
    savedViews: [],
    onSaveView: vi.fn(),
    onDeleteView: vi.fn(),
    ...over,
  };
}

function render(bar: SessionQueryBarProps): string {
  const activityCounts = Object.fromEntries(SESSION_ACTIVITIES.map((a) => [a, 0])) as Record<
    SessionActivity,
    number
  >;
  activityCounts["needs-you"] = 2;
  return renderToStaticMarkup(
    <SessionManagerHeader
      loading={false}
      queryBar={bar}
      groupBy="groups"
      onGroupByChange={() => {}}
      sortBy="recent"
      onSortByChange={() => {}}
      accountUsage={[]}
      onRefresh={() => {}}
      activityCounts={activityCounts}
      totalCount={4}
    />,
  );
}

function toggleChipAction() {
  const bar = components.find((c) => c.id === SESSION_QUERY_COMPONENT_ID);
  if (!bar) throw new Error("the query bar did not register — it is not mounted");
  const action = bar.actions.find((a) => a.id === "toggle-chip");
  if (!action) throw new Error("no toggle-chip action");
  return action;
}

describe("SessionManagerHeader — the query bar is mounted", () => {
  it("renders the grammar input and every chip, each with its count", () => {
    const html = render(queryBar(""));
    expect(html).toContain('data-ui-bridge-id="terminal.session-org.query-input"');
    for (const chip of QUERY_CHIPS) {
      expect(html).toContain(`data-ui-bridge-id="terminal.session-org.query-chip-${chip.key}"`);
    }
    // Needs you: 2 match, the undecidable row is counted apart.
    expect(html).toMatch(/query-chip-needs-you"[^>]*data-count="2"[^>]*data-unknown-count="1"/);
    expect(html).toMatch(/query-chip-pinned"[^>]*data-count="1"/);
    expect(html).toContain("Needs you<span");
  });

  it("offers the account and group pickers", () => {
    const html = render(queryBar(""));
    expect(html).toContain('data-ui-bridge-id="terminal.session-org.query-account-picker"');
    expect(html).toContain('data-ui-bridge-id="terminal.session-org.query-group-picker"');
    expect(html).toContain('<option value="gmail">gmail</option>');
  });

  it("the old substring search box, activity chips and account buttons are gone", () => {
    const html = render(queryBar(""));
    for (const retired of [
      "terminal.session-manager-search",
      "terminal.session-manager-filter-needs-you",
      "terminal.session-manager-filter-clear",
      "terminal.session-manager-account-",
      "data-status-filter",
      "data-account-filter",
    ]) {
      expect(html, retired).not.toContain(retired);
    }
    expect(Object.keys(SESSION_MANAGER_HEADER_IDS).sort()).toEqual([
      "groupBy",
      "refresh",
      "root",
      "sortBy",
    ]);
  });

  it("a chip click toggles its token in the query, both ways", () => {
    const on = queryBar("repo:coord");
    render(on);
    toggleChipAction().handler({ key: "needs-you" });
    expect(on.onQueryChange).toHaveBeenCalledWith("repo:coord is:needs-you");

    components.length = 0;
    const off = queryBar("repo:coord is:needs-you");
    const html = render(off);
    expect(html).toMatch(/query-chip-needs-you"[^>]*aria-pressed="true"/);
    toggleChipAction().handler({ key: "needs-you" });
    expect(off.onQueryChange).toHaveBeenCalledWith("repo:coord");
  });

  it("a typed token lights its chip; the header root carries the query", () => {
    const html = render(queryBar("is:pinned"));
    expect(html).toMatch(/query-chip-pinned"[^>]*aria-pressed="true"/);
    expect(html).toMatch(/query-chip-working"[^>]*aria-pressed="false"/);
    expect(html).toMatch(
      new RegExp(`data-ui-bridge-id="${SESSION_MANAGER_HEADER_IDS.root}"[^>]*data-query="is:pinned"`),
    );
  });

  it("a parse error is visible and says nothing is filtered", () => {
    const html = render(queryBar("colour:red"));
    expect(html).toContain('data-query-ok="false"');
    expect(html).toContain('role="alert"');
    expect(html).toContain('data-code="unknown-field"');
    expect(html).toContain("nothing is filtered until this is fixed");
  });

  it("shows how many rows the filter shows and how many it could not evaluate", () => {
    const html = render(queryBar("is:needs-you", { summary: { matched: 2, unknown: 1, total: 4 } }));
    expect(html).toContain("2 of 4 · 1 unknown");
  });
});
