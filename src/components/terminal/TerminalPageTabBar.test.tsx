/**
 * Tab-strip overflow: with many pages the tabs must scroll while the controls
 * stay pinned. Measured on a live runner with 21 pages before this split: tabs
 * 18-20 and every control (add, reorganize, close-empty, pop-out) rendered past
 * a ~1900px viewport, unreachable. The vitest environment is `node` with no
 * DOM, so the structure is checked on `renderToStaticMarkup` output.
 */

import { describe, it, expect, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

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
