/**
 * `sourceSizeLabel` — the figure beside a log source's name in the project
 * logs view.
 *
 * The runner reads only a bounded tail of a log (plan
 * `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail` Phase 2), so a
 * large file's line count is UNKNOWN (`totalLines: null`), not zero and not the
 * number of lines the tail happened to hold. The label must say what is known —
 * the file's size — and never render the unknown as a count.
 */

import { describe, expect, it } from "vitest";
import { sourceSizeLabel } from "./projectLogs";

describe("sourceSizeLabel", () => {
  it("shows the line count when the read covered the whole file", () => {
    expect(sourceSizeLabel({ totalLines: 250, fileBytes: 2048 })).toBe("250 lines");
  });

  it("shows the size, not a count, for a tail of a larger file", () => {
    expect(sourceSizeLabel({ totalLines: null, fileBytes: 2_969_567_232 })).toBe("2.8 GB");
  });

  it("an empty file is counted as zero lines: the read covered it", () => {
    expect(sourceSizeLabel({ totalLines: 0, fileBytes: 0 })).toBe("0 lines");
  });

  it("shows nothing when the file was not read", () => {
    expect(sourceSizeLabel({ totalLines: null, fileBytes: null })).toBeNull();
  });
});
