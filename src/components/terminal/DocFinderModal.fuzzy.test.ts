/**
 * The doc-finder scores file paths with the SHARED scorer.
 *
 * It used to carry a private `fuzzyScore` copy, and that copy still had
 * the overlapping bands the shared one was fixed for: word-boundary
 * `70 + query.length` against sequential `30 + max(0, 50 - spread)`, i.e.
 * 30..80 — so a sequential hit could score 80 and outrank a
 * word-boundary hit at 71, and the doc list ordered by accident. The copy
 * is DELETED rather than re-banded, because a scorer that exists twice
 * gets fixed once.
 *
 * `environment: "node"` vitest and no DOM test library in the repo, so
 * the "no second copy" half is a source assertion (precedent:
 * `SessionManagerToggle.test.ts`) and the band invariant is exercised
 * directly against the shared scorer over path-shaped corpora.
 */

import { readFileSync } from "fs";
import { join } from "path";

import { describe, expect, it } from "vitest";

import { fuzzyScore } from "./commands/fuzzy";

const SOURCE = readFileSync(join(__dirname, "DocFinderModal.tsx"), "utf8");

/** Word-boundary band floor / sequential band ceiling — see `fuzzy.ts`. */
const TIER2_MIN = 101;
const TIER3_MAX = 50;

describe("DocFinderModal — one scorer, not two", () => {
  it("declares no private fuzzyScore", () => {
    expect(SOURCE).not.toMatch(/function fuzzyScore\s*\(/);
  });

  it("imports the shared scorer", () => {
    expect(SOURCE).toMatch(/import \{ fuzzyScore \} from "\.\/commands\/fuzzy";/);
  });

  it("carries none of the deleted copy's band constants", () => {
    // `70 + q.length` and `30 + Math.max(...)` were the overlapping bands.
    expect(SOURCE).not.toContain("70 + q.length");
    expect(SOURCE).not.toContain("30 + Math.max");
  });
});

describe("shared scorer over file paths", () => {
  // Path separators the doc-finder actually sees: `/`, `\` and `.`, all
  // of which have to be word boundaries or a path scores as one long
  // token and every hit collapses into the sequential tier.
  it("treats `.`, `/` and `\\` as word boundaries", () => {
    expect(fuzzyScore("docs/api/ui-bridge.md", "dau")!.score).toBeGreaterThanOrEqual(TIER2_MIN);
    expect(fuzzyScore("docs\\api\\ui-bridge.md", "dau")!.score).toBeGreaterThanOrEqual(TIER2_MIN);
    expect(fuzzyScore("runner.architecture.json", "raj")!.score).toBeGreaterThanOrEqual(TIER2_MIN);
  });

  it("never scores a sequential hit at or above a word-boundary hit", () => {
    // The invariant the private copy violated, pinned over path corpora
    // rather than command names.
    const WORD_BOUNDARY = [
      "docs/api/ui-bridge.md",
      "src\\components\\terminal\\command-bar.tsx",
      "plans/2026-08-runner.notes.md",
      "docs/architecture/config-migration.md",
    ];
    const SEQUENTIAL = [
      "vendor/thirdparty/legacy_dump.txt",
      "notes/scratch/misc.md",
      "README.md",
      "python-bridge/handlers.py",
    ];
    for (const query of ["d", "da", "dau", "s", "sc", "ct", "ma", "am"]) {
      const wb = WORD_BOUNDARY.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> => m !== null && m.score >= TIER2_MIN,
      );
      const seq = SEQUENTIAL.map((t) => fuzzyScore(t, query)).filter(
        (m): m is NonNullable<typeof m> => m !== null && m.score < TIER2_MIN,
      );
      for (const w of wb) {
        for (const s of seq) {
          expect(s.score).toBeLessThanOrEqual(TIER3_MAX);
          expect(s.score).toBeLessThan(w.score);
        }
      }
    }
  });
});
