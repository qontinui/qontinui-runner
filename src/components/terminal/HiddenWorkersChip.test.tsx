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
    expect(source).toContain("<HiddenWorkersChip");
    expect(source).toContain("hidden={hiddenWorkers}");
    expect(source).toContain("restoreHiddenWorkers()");
  });
});
