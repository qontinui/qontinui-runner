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
    [1023, "1023 B output"],
    [1024, "1 KB output"],
    [67_036, "65 KB output"],
    [376_950, "368 KB output"],
    [1024 * 1024, "1.0 MB output"],
    [5 * 1024 * 1024 + 512 * 1024, "5.5 MB output"],
  ])("%s bytes -> %s", (bytes, expected) => {
    expect(formatTotalOutput(bytes)).toBe(expected);
  });
});

describe("TotalOutputLabel", () => {
  it("names the total, never a line count", () => {
    const html = renderToStaticMarkup(<TotalOutputLabel totalBytes={376_950} />);
    expect(html).toContain(">368 KB output<");
    expect(html).toContain("376950 bytes");
    expect(html).not.toMatch(/\blines\b/);
  });

  it("renders nothing while the total is unknown", () => {
    expect(renderToStaticMarkup(<TotalOutputLabel totalBytes={undefined} />)).toBe("");
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
