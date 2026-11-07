/**
 * The bulk result list, rendered with `renderToStaticMarkup` (the runner's
 * vitest config is `environment: "node"`, no jsdom).
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { BulkEndResultList, bulkEndSummary, type BulkEndItem } from "./CloseAllFinishedDialog";
import { describeEndResult } from "./remoteSessionEndView";
import type { RemoteSessionEndResult } from "./remoteTabs";

const items: BulkEndItem[] = [
  { sessionId: "a", deviceId: "d1", deviceLabel: "spaceship", sessionLabel: "plan-a" },
  { sessionId: "b", deviceId: "d1", deviceLabel: "spaceship", sessionLabel: "plan-b" },
  { sessionId: "c", deviceId: "d2", deviceLabel: "laptop", sessionLabel: "plan-c" },
  { sessionId: "d", deviceId: "d2", deviceLabel: "laptop", sessionLabel: "plan-d" },
  { sessionId: "e", deviceId: "d2", deviceLabel: "laptop", sessionLabel: "plan-e" },
];

const r = (outcome: RemoteSessionEndResult["outcome"], reason: string | null = null) =>
  describeEndResult({
    outcome,
    deviceId: "d",
    sessionId: "s",
    terminalId: null,
    via: outcome === "ended" ? "graceful" : null,
    reason,
    grantSource: "minted",
  });

describe("BulkEndResultList", () => {
  const results = {
    a: r("ended"),
    b: r("refused", "unsent draft"),
    c: r("still_running"),
    d: r("not_found"),
    e: r("unknown", "timed out after 90s"),
  };
  const html = renderToStaticMarkup(<BulkEndResultList items={items} results={results} />);

  it("renders one stamped row per session with its outcome", () => {
    for (const [id, outcome] of [
      ["a", "ended"],
      ["b", "refused"],
      ["c", "still_running"],
      ["d", "not_found"],
      ["e", "unknown"],
    ]) {
      expect(html).toContain(
        `data-ui-bridge-id="terminal.fleet-close-all-finished-result.${id}" data-end-outcome="${outcome}"`,
      );
    }
  });

  it("shows the refusal reason and 'already gone'", () => {
    expect(html).toContain("unsent draft");
    expect(html).toContain("already gone");
    expect(html).toContain("still running");
  });

  it("never renders the unknown row as ended", () => {
    const eRow = html.slice(html.indexOf("result.e"));
    expect(eRow).toContain(">unknown<");
    expect(eRow).not.toContain(">ended<");
    expect(eRow).toContain("timed out after 90s");
  });

  it("an unsettled row is pending, not ended", () => {
    const pending = renderToStaticMarkup(<BulkEndResultList items={items} results={{}} />);
    expect(pending).not.toContain(">ended<");
    expect(pending).toContain('data-end-outcome="pending"');
  });

  it("the confirm list names device and session without a status", () => {
    const confirm = renderToStaticMarkup(
      <BulkEndResultList items={items} results={{}} showStatus={false} />,
    );
    expect(confirm).toContain("spaceship");
    expect(confirm).toContain("plan-c");
    expect(confirm).not.toContain("…");
  });

  it("the summary tallies by rendered outcome", () => {
    expect(bulkEndSummary(items, results)).toBe(
      "5 of 5 answered — 1 ended, 1 refused, 1 still running, 1 already gone, 1 unknown.",
    );
    expect(bulkEndSummary(items, { a: r("unknown") })).toBe("1 of 5 answered — 1 unknown.");
  });
});
