/**
 * Pane label markup (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4). `environment: "node"` with no React Testing Library, so the markup
 * is asserted through `renderToStaticMarkup` over the pure `PaneLabelText`.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { PaneLabelText } from "./PaneLabelView";
import { paneLabel } from "./paneLabel";

const LEAD_ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

/** The element whose `data-ui-id` is `id`, as its opening tag plus its text. */
function element(html: string, id: string): { tag: string; text: string } | null {
  const at = html.indexOf(`data-ui-id="${id}"`);
  if (at < 0) return null;
  const open = html.lastIndexOf("<", at);
  const close = html.indexOf(">", at);
  const end = html.indexOf("</span>", close);
  return { tag: html.slice(open, close + 1), text: html.slice(close + 1, end) };
}

describe("PaneLabelText", () => {
  it("renders the name, the id suffix and the activity as separately addressable elements", () => {
    const label = paneLabel({
      conversationName: "maxim",
      claudeSessionId: LEAD_ID,
      activityTitle: "Fix Phase 5b round 11",
      shellTitle: "x",
    });
    const html = renderToStaticMarkup(<PaneLabelText label={label} tabId="t-1" />);
    expect(element(html, "pane-label-t-1")?.tag).toContain(
      'title="maxim · 9b40e22d — Fix Phase 5b round 11"',
    );
    expect(element(html, "pane-label-id-t-1")?.text).toBe(" · 9b40e22d");
    expect(element(html, "pane-label-activity-t-1")?.text).toBe("Fix Phase 5b round 11");
    expect(html).toContain(">maxim</span>");
  });

  it("a long name truncates while the · <id8> suffix cannot", () => {
    const long = "Fix Phase 5b round 11 of the account-migration plan ".repeat(6);
    const label = paneLabel({ conversationName: long, claudeSessionId: LEAD_ID, shellTitle: "x" });
    const html = renderToStaticMarkup(<PaneLabelText label={label} tabId="t-2" />);
    // The suffix is its own element that never shrinks or wraps.
    const suffix = element(html, "pane-label-id-t-2");
    expect(suffix?.text).toBe(" · 9b40e22d");
    expect(suffix?.tag).toContain("shrink-0");
    expect(suffix?.tag).toContain("whitespace-nowrap");
    expect(suffix?.tag).not.toContain("truncate");
    // The name is the element that truncates.
    expect(html).toMatch(/<span class="truncate min-w-0">Fix Phase 5b/);
  });

  it("a plain shell renders its shell title alone, with no id or activity element", () => {
    const html = renderToStaticMarkup(
      <PaneLabelText label={paneLabel({ shellTitle: "htop" })} tabId="t-3" />,
    );
    expect(element(html, "pane-label-primary-t-3")).not.toBeNull();
    expect(html).toContain(">htop</span>");
    expect(element(html, "pane-label-id-t-3")).toBeNull();
    expect(element(html, "pane-label-activity-t-3")).toBeNull();
  });

  it("showSecondary={false} drops the activity line", () => {
    const label = paneLabel({
      conversationName: "maxim",
      claudeSessionId: LEAD_ID,
      activityTitle: "Review",
      shellTitle: "x",
    });
    const html = renderToStaticMarkup(
      <PaneLabelText label={label} tabId="t-4" showSecondary={false} />,
    );
    expect(element(html, "pane-label-activity-t-4")).toBeNull();
    expect(element(html, "pane-label-id-t-4")?.text).toBe(" · 9b40e22d");
  });
});
