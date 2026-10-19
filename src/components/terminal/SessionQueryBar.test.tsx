/**
 * The query bar and the Unknown tail as rendered markup, plus the bar's UI
 * Bridge actions driven directly.
 *
 * `environment: "node"` with no DOM library in the shared `node_modules`, so —
 * as `SessionGroupSuggestionsDialog.test.tsx` does — markup is locked with
 * `renderToStaticMarkup` and the UI Bridge SDK is replaced by a recorder.
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

import {
  SessionQueryBar,
  SESSION_QUERY_COMPONENT_ID,
  type SessionQueryBarProps,
} from "./SessionQueryBar";
import { SessionQueryUnknownTail } from "./SessionQueryUnknownTail";
import { known, unknownFact } from "./sessionFacts";
import { NOW, facts } from "./sessionFacts.testkit";
import { filterSessions, parseQuery } from "./sessionQuery";
import { chipCounts } from "./sessionQueryChips";

beforeEach(() => {
  components.length = 0;
});

function props(over: Partial<SessionQueryBarProps> = {}): SessionQueryBarProps {
  const query = over.query ?? "";
  return {
    query,
    onQueryChange: vi.fn(),
    parsed: parseQuery(query),
    chipCounts: chipCounts(
      [facts({ activity: known("needs-you") }), facts({ activity: unknownFact("no digest") })],
      { now: NOW },
    ),
    accounts: ["gmail", "hotmail"],
    groups: [],
    savedViews: [{ name: "Triage", query: "is:needs-you", savedAt: 0 }],
    onSaveView: vi.fn(),
    onDeleteView: vi.fn(),
    ...over,
  };
}

function actions() {
  const c = components.find((x) => x.id === SESSION_QUERY_COMPONENT_ID);
  if (!c) throw new Error("component not registered");
  return Object.fromEntries(c.actions.map((a) => [a.id, a]));
}

describe("SessionQueryBar markup", () => {
  it("renders each chip with its count, unknowns in data, and lights the typed token", () => {
    const html = renderToStaticMarkup(<SessionQueryBar {...props({ query: "is:needs-you" })} />);
    expect(html).toContain('data-ui-bridge-id="terminal.session-org.query-chip-needs-you"');
    expect(html).toMatch(
      /query-chip-needs-you"[^>]*data-count="1"[^>]*data-unknown-count="1"[^>]*aria-pressed="true"/,
    );
    expect(html).toMatch(/query-chip-working"[^>]*aria-pressed="false"/);
    expect(html).toContain('data-query-ok="true"');
  });

  it("highlights tokens inline", () => {
    const html = renderToStaticMarkup(
      <SessionQueryBar {...props({ query: 'repo:coord "a b" OR -is:idle' })} />,
    );
    expect(html).toContain('data-kind="field-name"');
    expect(html).toContain('data-kind="field-value"');
    expect(html).toContain('data-kind="phrase"');
    expect(html).toContain('data-kind="operator"');
    expect(html).toContain('data-kind="negate"');
  });

  it("shows a parse error as a visible message and marks the term", () => {
    const html = renderToStaticMarkup(<SessionQueryBar {...props({ query: "colour:red" })} />);
    expect(html).toContain('data-query-ok="false"');
    expect(html).toContain('role="alert"');
    expect(html).toContain('data-code="unknown-field"');
    expect(html).toContain("nothing is filtered until this is fixed");
    expect(html).toContain('data-kind="error"');
  });

  it("an unknown org shows saved views and groups as unknown, never as none", () => {
    const html = renderToStaticMarkup(
      <SessionQueryBar
        {...props({ groups: null, savedViews: null, orgUnavailableReason: "file unreadable" })}
      />,
    );
    expect(html).toContain("Saved views unknown: file unreadable");
    expect(html).toContain("Groups unknown");
    expect(html).not.toContain("Save view");
  });

  it("renders saved views as chips and the summary with its unknown count", () => {
    const html = renderToStaticMarkup(
      <SessionQueryBar
        {...props({ query: "is:needs-you", summary: { matched: 3, unknown: 2, total: 9 } })}
      />,
    );
    expect(html).toMatch(/query-saved-view-triage"[^>]*aria-pressed="true"/);
    expect(html).toContain('data-effect="write"');
    expect(html).toContain("3 of 9 · 2 unknown");
  });
});

describe("SessionQueryBar actions", () => {
  it("declares effects: filter edits read, view edits write", () => {
    renderToStaticMarkup(<SessionQueryBar {...props()} />);
    const a = actions();
    expect(a["set-query"].effect).toBe("read");
    expect(a["toggle-chip"].effect).toBe("read");
    expect(a["save-view"].effect).toBe("write");
    expect(a["delete-view"].effect).toBe("write");
  });

  it("toggle-chip edits the query; save-view refuses an empty query", () => {
    const p = props({ query: "repo:coord" });
    renderToStaticMarkup(<SessionQueryBar {...p} />);
    const a = actions();
    a["toggle-chip"].handler({ key: "pinned" });
    expect(p.onQueryChange).toHaveBeenCalledWith("repo:coord is:pinned");
    a["save-view"].handler({ name: "Coord" });
    expect(p.onSaveView).toHaveBeenCalledWith("Coord", "repo:coord");

    components.length = 0;
    const empty = props({ query: "" });
    renderToStaticMarkup(<SessionQueryBar {...empty} />);
    expect(() => actions()["save-view"].handler({ name: "x" })).toThrow("The query is empty");
    expect(empty.onSaveView).not.toHaveBeenCalled();
  });
});

describe("SessionQueryUnknownTail", () => {
  it("shows the count of rows the filter could not evaluate, and nothing when there are none", () => {
    const rows = [
      facts({ rowId: "a", activity: known("needs-you") }),
      facts({ rowId: "b", activity: unknownFact("no digest yet") }),
      facts({ rowId: "c", activity: known("working") }),
    ];
    const r = filterSessions("is:needs-you", rows, { now: NOW });
    if (!r.ok) throw new Error("parse");
    const html = renderToStaticMarkup(
      <SessionQueryUnknownTail
        unknown={r.unknown}
        renderRow={(f) => <i>{f.rowId}</i>}
        defaultOpen
      />,
    );
    expect(html).toContain("Unknown for this filter (1)");
    expect(html).toContain('data-unknown-reason="no digest yet"');
    expect(html).toContain("<i>b</i>");
    expect(
      renderToStaticMarkup(<SessionQueryUnknownTail unknown={[]} renderRow={() => null} />),
    ).toBe("");
  });
});
