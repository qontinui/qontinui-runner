/**
 * Zone header output figure (plan
 * `2026-10-22-runner-terminal-overlays-cover-zone-content-and-controls`,
 * Phase 2 / F4). The header used to read `{n} lines` off the capped tail
 * preview (`lastOutputLines`), so every busy pane said `20 lines` however much
 * it had written. It now shows the session's TOTAL output, labelled as such,
 * and nothing when no total has been observed.
 *
 * `environment: "node"`: the pure formatter and the small `TotalOutputLabel`
 * component are rendered with `renderToStaticMarkup`; the ZoneCell → ZoneLabel
 * wiring is asserted by reading the source, as `sessionInfoMountSites.test.ts`
 * does.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { formatTotalOutput } from "./outputSize";
import { TotalOutputLabel } from "./ZoneLabel";

const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

describe("formatTotalOutput", () => {
  it.each([
    [undefined, null],
    [0, null],
    [-5, null],
    [Number.NaN, null],
    [512, "512 B output"],
    [999, "999 B output"],
    [1000, "1 KB output"],
    [67_036, "67 KB output"],
    // The plan's measured pane: 376 950 bytes is the "377 KB" it names.
    [376_950, "377 KB output"],
    [999_499, "999 KB output"],
    [999_500, "1.0 MB output"],
    [1_000_000, "1.0 MB output"],
    [5_500_000, "5.5 MB output"],
  ])("%s bytes -> %s", (bytes, expected) => {
    expect(formatTotalOutput(bytes)).toBe(expected);
  });
});

describe("TotalOutputLabel", () => {
  it("names the total, never a line count", () => {
    const html = renderToStaticMarkup(<TotalOutputLabel totalBytes={376_950} />);
    expect(html).toContain(">377 KB output<");
    expect(html).toContain("376950 bytes");
    expect(html).not.toMatch(/\blines\b/);
  });

  it("renders nothing while the total is unknown", () => {
    expect(renderToStaticMarkup(<TotalOutputLabel totalBytes={undefined} />)).toBe("");
  });
});

describe("event streams -> hot store wiring", () => {
  const ctx = read("../contexts/TerminalSessionContext.tsx");

  it("the output tap records each chunk's end offset and publishes it on flush", () => {
    expect(ctx).toContain("const total = chunkEndOffset(payload.offset, bytes.length);");
    expect(ctx).toContain("pendingTotals.set(tid, Math.max(pendingTotals.get(tid) ?? 0, total));");
    expect(ctx).toMatch(
      /for \(const \[tabId, total\] of pendingTotals\) \{\s*hotStore\.noteTabTotalBytes\(tabId, total\);/,
    );
    expect(ctx).toContain("pendingTotals.clear();");
  });

  it("the activity digest (unwatched tabs) feeds the same field", () => {
    expect(ctx).toContain(
      "hotStore.noteTabTotalBytes(payload.terminalId, payload.totalBytesProduced);",
    );
  });
});

describe("ZoneCell -> ZoneLabel wiring", () => {
  const cell = read("./ZoneCell.tsx");
  const label = read("./ZoneLabel.tsx");

  it("feeds the header the hot total, not the tail preview", () => {
    expect(cell).toContain("totalOutputBytes={hot.totalBytes}");
    expect(cell).not.toContain("outputLineCount=");
    expect(cell).not.toContain("outputByteSize=");
  });

  it("the header renders the total-output label and no line count", () => {
    expect(label).toContain("<TotalOutputLabel totalBytes={totalOutputBytes} />");
    expect(label).not.toContain("outputLineCount");
  });
});
