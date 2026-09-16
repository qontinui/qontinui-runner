/**
 * The "N hidden workers" chip — the affordance that makes closing a Conductor
 * worker cell REVERSIBLE.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library, so (as `UnzonedChip` / `WorkerSessionCell` do) the label contract is
 * locked on its pure helper and the markup with `renderToStaticMarkup`.
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { HiddenWorkersChip, hiddenWorkersChipLabel } from "./HiddenWorkersChip";
import type { HiddenWorker } from "./useTerminalManager";

const hidden = (partial: Partial<HiddenWorker> = {}): HiddenWorker => ({
  tabId: "trid-1",
  taskRunId: "trid-1",
  title: "worker: refactor parser",
  hiddenAtMs: 0,
  ...partial,
});

describe("hiddenWorkersChipLabel", () => {
  it("renders nothing when no worker view is hidden", () => {
    expect(hiddenWorkersChipLabel([])).toBeNull();
    expect(renderToStaticMarkup(<HiddenWorkersChip hidden={[]} onRestoreAll={() => {}} />)).toBe("");
  });

  it("counts the hidden workers and names them in the tooltip", () => {
    const label = hiddenWorkersChipLabel([hidden(), hidden({ tabId: "t2", title: "worker: tests" })]);
    expect(label?.text).toBe("2 hidden workers");
    expect(label?.title).toContain("refactor parser");
    expect(label?.title).toContain("worker: tests");
    expect(label?.title).toContain("still running");
  });

  it("uses the singular for one", () => {
    expect(hiddenWorkersChipLabel([hidden()])?.text).toBe("1 hidden worker");
  });

  it("says so when a previous restore could NOT bring a worker back", () => {
    // Honesty: a "show" click that silently did nothing is the failure the
    // affordance exists to remove, so a miss is reported rather than dropped.
    const label = hiddenWorkersChipLabel([hidden({ restoreMissedAtMs: 5 }), hidden({ tabId: "t2" })]);
    expect(label?.title).toContain("could not be re-opened");
    expect(hiddenWorkersChipLabel([hidden()])?.title).not.toContain("could not be re-opened");
  });

  it("counts only the NOT-missed workers as still running", () => {
    // Nit 7. The lead clause used to claim all N were "still running" and then
    // append that M of them could not be re-opened — a sentence contradicting
    // its own tail, and false for exactly those M. A missed worker is unknown,
    // not running.
    const label = hiddenWorkersChipLabel([
      hidden({ tabId: "t1", title: "alpha", restoreMissedAtMs: 5 }),
      hidden({ tabId: "t2", title: "beta" }),
      hidden({ tabId: "t3", title: "gamma" }),
    ]);
    expect(label?.text).toBe("3 hidden workers"); // the chip still counts all of them
    expect(label?.title).toContain("2 worker cells were closed");
    expect(label?.title).not.toContain("3 worker cells were closed");
    // The still-running clause names only the two that are.
    const stillRunningClause = label!.title.split("could not be re-opened")[0];
    expect(stillRunningClause).toContain("beta");
    expect(stillRunningClause).toContain("gamma");
    expect(stillRunningClause).not.toContain("alpha");
    // …and the miss clause names the one that is not.
    expect(label?.title).toContain("1 could not be re-opened");
    expect(label?.title).toContain("alpha");
  });

  it("claims nobody is still running when EVERY hidden worker was missed", () => {
    const label = hiddenWorkersChipLabel([
      hidden({ tabId: "t1", title: "alpha", restoreMissedAtMs: 5 }),
      hidden({ tabId: "t2", title: "beta", restoreMissedAtMs: 6 }),
    ]);
    expect(label?.title).not.toContain("still running");
    expect(label?.title).toContain("2 could not be re-opened");
    // Clicking still retries, so the affordance still says what it does.
    expect(label?.title).toContain("Click to show");
  });

  it("pluralises the article with the noun in the miss clause", () => {
    // "no longer listed as an open sessions" — the article was not pluralised
    // with the noun, and the count assertions above do not reach it.
    const two = hiddenWorkersChipLabel([
      hidden({ tabId: "t1", restoreMissedAtMs: 5 }),
      hidden({ tabId: "t2", restoreMissedAtMs: 6 }),
    ]);
    expect(two?.title).toContain("no longer listed as open sessions");
    expect(two?.title).not.toContain("an open sessions");
    const one = hiddenWorkersChipLabel([hidden({ restoreMissedAtMs: 5 })]);
    expect(one?.title).toContain("no longer listed as an open session;");
  });

  it("uses the singular consistently for one still-running worker", () => {
    const label = hiddenWorkersChipLabel([hidden()]);
    expect(label?.title).toContain("1 worker cell was closed");
    expect(label?.title).toContain("is still running");
    expect(label?.title).toContain("Click to show it again.");
  });
});

describe("HiddenWorkersChip", () => {
  it("renders a clickable pill with a stable bridge id", () => {
    const html = renderToStaticMarkup(
      <HiddenWorkersChip hidden={[hidden()]} onRestoreAll={() => {}} />,
    );
    expect(html).toContain('data-page-element="hidden-workers-chip"');
    expect(html).toContain('data-ui-bridge-id="terminal.hidden-workers-restore"');
    expect(html).toContain("1 hidden worker");
  });
});

describe("TerminalPage wiring", () => {
  const source = readFileSync(resolve(__dirname, "./TerminalPage.tsx"), "utf8");

  it("mounts the chip on the page, fed by the manager's hidden-worker state", () => {
    // Markup only. The REVERSAL's behaviour is covered by
    // `hiddenWorkerReducer.test.ts`, which exercises the actual state
    // transitions rather than the hook's source text.
    expect(source).toContain("<HiddenWorkersChip");
    expect(source).toMatch(/hidden=\{hiddenWorkers\}/);
    expect(source).toMatch(/restoreHiddenWorkers\(\s*\)/);
  });
});
