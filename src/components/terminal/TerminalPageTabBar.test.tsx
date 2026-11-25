/**
 * Tab-strip overflow: with many pages the tabs must scroll while the controls
 * stay pinned. Measured on a live runner with 21 pages before this split: tabs
 * 18-20 and every control (add, reorganize, close-empty, pop-out) rendered past
 * a ~1900px viewport, unreachable. The vitest environment is `node` with no
 * DOM, so the structure is checked on `renderToStaticMarkup` output.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

// Records every `useUIElement` call by id, so a test can read the label a
// registration carries. `renderToStaticMarkup` never attaches refs, so WHETHER
// an element is rendered (and so registered) is read from its `data-testid`,
// which sits on the same element as the ref; `PageTab.render.test.tsx` checks
// the ref attachments themselves in jsdom.
const bridge = vi.hoisted(() => ({ calls: new Map<string, { type?: string; label?: string }>() }));
vi.mock("@qontinui/ui-bridge", () => ({
  useUIElement: (opts: { id: string; type?: string; label?: string }) => {
    bridge.calls.set(opts.id, { type: opts.type, label: opts.label });
    return { ref: () => {} };
  },
}));

import type { TerminalPageConfig } from "./useTerminalPages";
import { TerminalPageTabBar } from "./TerminalPageTabBar";

const pages: TerminalPageConfig[] = Array.from({ length: 21 }, (_, i) => ({
  id: `p${i}`,
  name: `Page ${i + 1}`,
  createdAt: 0,
}));

const markup = renderToStaticMarkup(
  <TerminalPageTabBar
    pages={pages}
    activePageId="p20"
    onSelectPage={() => {}}
    onAddPage={() => {}}
    onRemovePage={() => {}}
    onRenamePage={() => {}}
    onReorganize={() => {}}
    onCloseEmptyPages={async () => 0}
    onPopOut={() => {}}
  />,
);

/** The opening tag of the element carrying this data-testid. */
const openingTag = (testId: string) => {
  const m = markup.match(new RegExp(`<div[^>]*data-testid="${testId}"[^>]*>`));
  expect(m, `no element with data-testid=${testId}`).not.toBeNull();
  return m![0];
};

describe("TerminalPageTabBar overflow layout", () => {
  it("puts the tabs in a horizontally scrolling list that can shrink", () => {
    const tag = openingTag("terminal-page-tab-list");
    expect(tag).toContain("overflow-x-auto");
    expect(tag).toContain("min-w-0");
  });

  it("pins the controls in a non-shrinking group", () => {
    expect(openingTag("terminal-page-tab-controls")).toContain("shrink-0");
  });

  it("renders every tab before the controls and every control after them", () => {
    const listAt = markup.indexOf('data-testid="terminal-page-tab-list"');
    const controlsAt = markup.indexOf('data-testid="terminal-page-tab-controls"');
    expect(listAt).toBeGreaterThanOrEqual(0);
    expect(controlsAt).toBeGreaterThan(listAt);

    const tabPositions = [...markup.matchAll(/role="tab"/g)].map((m) => m.index!);
    expect(tabPositions).toHaveLength(pages.length);
    for (const at of tabPositions) {
      expect(at).toBeGreaterThan(listAt);
      expect(at).toBeLessThan(controlsAt);
    }

    for (const label of [
      'aria-label="Add terminal page"',
      'title="Reorganize pages by topic (AI)"',
      'aria-label="Close empty tabs"',
      'aria-label="Open terminal in a new window"',
    ]) {
      expect(markup.indexOf(label), label).toBeGreaterThan(controlsAt);
    }
  });

  it("keeps tabs from squeezing below their content width", () => {
    const tabTags = [...markup.matchAll(/<button[^>]*role="tab"[^>]*>/g)].map((m) => m[0]);
    expect(tabTags).toHaveLength(pages.length);
    for (const tag of tabTags) expect(tag).toContain("shrink-0");
  });
});

const makePages = (n: number): TerminalPageConfig[] =>
  Array.from({ length: n }, (_, i) => ({ id: `p${i}`, name: `Page ${i + 1}`, createdAt: 0 }));

interface RenderOpts {
  popOut?: boolean;
  isPinned?: boolean;
}

function renderStrip(
  list: TerminalPageConfig[],
  { popOut = true, isPinned = false }: RenderOpts = {},
) {
  return renderToStaticMarkup(
    <TerminalPageTabBar
      pages={list}
      activePageId={list[0]?.id ?? ""}
      onSelectPage={() => {}}
      onAddPage={() => {}}
      onRemovePage={() => {}}
      onRenamePage={() => {}}
      onReorderPage={() => {}}
      onPopOutPage={popOut ? () => {} : undefined}
      isPinned={isPinned}
    />,
  );
}

/** Every rendered `data-testid` value, in document order. */
const testIds = (html: string) => [...html.matchAll(/data-testid="([^"]+)"/g)].map((m) => m[1]);
const NON_TAB_IDS = new Set(["terminal-page-tab-list", "terminal-page-tab-controls"]);
const tabIdsIn = (html: string) =>
  testIds(html).filter((id) => id.startsWith("terminal-page-tab-") && !NON_TAB_IDS.has(id));

/**
 * The deepest `<button>` nesting in the markup: 1 means no button opens inside
 * another. A tag-depth scan, not a DOM parse — the parser would silently
 * re-parent a nested button, hiding exactly the defect this looks for.
 */
function maxButtonDepth(html: string): number {
  let depth = 0;
  let max = 0;
  for (const m of html.matchAll(/<(\/?)button\b/g)) {
    depth += m[1] ? -1 : 1;
    max = Math.max(max, depth);
  }
  expect(depth, "unbalanced <button> tags").toBe(0);
  return max;
}

/** The opening tag of the element carrying this exact data-testid. */
const tagOf = (html: string, testId: string) => {
  const m = html.match(new RegExp(`<[a-z]+[^>]*data-testid="${testId}"[^>]*>`));
  expect(m, `no element with data-testid=${testId}`).not.toBeNull();
  return m![0];
};

beforeEach(() => bridge.calls.clear());

describe("TerminalPageTabBar page tabs: un-nested, UI-Bridge-registered controls", () => {
  it.each([0, 1, 21])("never opens a <button> inside another <button> with %i page(s)", (n) => {
    const html = renderStrip(makePages(n));
    // Not vacuous: the many-page strip really renders its tab and action buttons.
    expect([...html.matchAll(/role="tab"/g)]).toHaveLength(n);
    if (n === 21) {
      expect(testIds(html).filter((id) => id.startsWith("terminal-page-tab-popout-"))).toHaveLength(
        21,
      );
      expect(testIds(html).filter((id) => id.startsWith("terminal-page-tab-close-"))).toHaveLength(
        21,
      );
    }
    expect(maxButtonDepth(html)).toBe(1);
    // The retired shape: an interactive span standing in for a button.
    expect(html).not.toContain('role="button"');
  });

  it("registers each page's tab, pop-out and close under ids keyed on the page id", () => {
    const list = makePages(3);
    const html = renderStrip(list);
    expect(tabIdsIn(html)).toEqual(
      list.flatMap((p) => [
        `terminal-page-tab-${p.id}`,
        `terminal-page-tab-popout-${p.id}`,
        `terminal-page-tab-close-${p.id}`,
      ]),
    );
    for (const p of list) {
      expect(bridge.calls.get(`terminal-page-tab-${p.id}`)).toEqual({ type: "tab", label: p.name });
      expect(bridge.calls.get(`terminal-page-tab-popout-${p.id}`)).toEqual({
        type: "button",
        label: `Pop out ${p.name}`,
      });
      expect(bridge.calls.get(`terminal-page-tab-close-${p.id}`)).toEqual({
        type: "button",
        label: `Close ${p.name}`,
      });
      expect(tagOf(html, `terminal-page-tab-${p.id}`)).toContain('role="tab"');
      expect(tagOf(html, `terminal-page-tab-popout-${p.id}`)).toContain(
        `aria-label="Pop out ${p.name} into its own window"`,
      );
      expect(tagOf(html, `terminal-page-tab-close-${p.id}`)).toContain(
        `aria-label="Close ${p.name}"`,
      );
    }
  });

  it("renders no pop-out button when onPopOutPage is omitted", () => {
    const html = renderStrip(makePages(3), { popOut: false });
    expect(testIds(html).filter((id) => id.startsWith("terminal-page-tab-popout-"))).toEqual([]);
    expect(testIds(html).filter((id) => id.startsWith("terminal-page-tab-close-"))).toHaveLength(3);
  });

  it("renders no close button for the only page", () => {
    const html = renderStrip(makePages(1));
    expect(tabIdsIn(html)).toEqual(["terminal-page-tab-p0", "terminal-page-tab-popout-p0"]);
  });

  it("keeps the same id set across a reorder", () => {
    const list = makePages(5);
    const before = new Set(tabIdsIn(renderStrip(list)));
    const reordered = [list[3], list[0], list[4], list[2], list[1]];
    const afterHtml = renderStrip(reordered);
    expect(new Set(tabIdsIn(afterHtml))).toEqual(before);
    // The FIRST tab rendered is now p3's, under p3's id — not position 0's.
    expect(tabIdsIn(afterHtml)[0]).toBe("terminal-page-tab-p3");
  });

  it("keeps the id and updates the label across a rename", () => {
    const list = makePages(2);
    renderStrip(list);
    expect(bridge.calls.get("terminal-page-tab-p0")?.label).toBe("Page 1");
    bridge.calls.clear();
    const html = renderStrip([{ ...list[0], name: "Renamed" }, list[1]]);
    expect(tabIdsIn(html)).toContain("terminal-page-tab-p0");
    expect(bridge.calls.get("terminal-page-tab-p0")?.label).toBe("Renamed");
    expect(bridge.calls.get("terminal-page-tab-popout-p0")?.label).toBe("Pop out Renamed");
    expect(bridge.calls.get("terminal-page-tab-close-p0")?.label).toBe("Close Renamed");
  });

  it("shows the action buttons on focus within the tab, not only on hover", () => {
    const html = renderStrip(makePages(2));
    for (const id of ["terminal-page-tab-popout-p0", "terminal-page-tab-close-p0"]) {
      const tag = tagOf(html, id);
      expect(tag).toContain("group-hover:opacity-100");
      expect(tag).toContain("group-focus-within:opacity-100");
      expect(tag).toContain("focus-visible:opacity-100");
    }
    // The `group` those variants key on is the presentation wrapper around all three.
    const wrapper = html.match(
      /<div role="presentation" class="group [^"]*"[^>]*><button[^>]*data-testid="terminal-page-tab-p0"/,
    );
    expect(
      wrapper,
      "tab button is not the first child of a group presentation wrapper",
    ).not.toBeNull();
  });

  it("registers the pinned window's title with the page name, and no tab ids", () => {
    const html = renderStrip([{ id: "pinned", name: "Detached work", createdAt: 0 }], {
      isPinned: true,
    });
    expect(tagOf(html, "terminal-page-pinned-title")).toContain("<span");
    expect(html).toContain(">Detached work</span>");
    expect(bridge.calls.get("terminal-page-pinned-title")).toEqual({
      type: "generic",
      label: "Detached work",
    });
    expect(tabIdsIn(html)).toEqual([]);
    expect([...bridge.calls.keys()].filter((id) => id.startsWith("terminal-page-tab-"))).toEqual(
      [],
    );
  });
});
