/**
 * The no-match Enter gate. Found by pre-PR review of the #1301 port: Enter on
 * a line only Tier 3 can resolve painted "No command matches" during the
 * debounce, and again after a history recall, because "settled" was
 * remembered per string rather than per run.
 */

import { readFileSync } from "fs";
import { join } from "path";
import { describe, expect, it } from "vitest";
import { noMatchEnterIsInert } from "./tier3EnterGate";

const LINE = "please open something clever";

describe("noMatchEnterIsInert", () => {
  it("is inert during the debounce window (eligible, not yet answered)", () => {
    expect(
      noMatchEnterIsInert({ query: LINE, interpreting: false, eligible: true, settledFor: null }),
    ).toBe(true);
  });

  it("is inert while the subprocess call is in flight", () => {
    expect(
      noMatchEnterIsInert({ query: LINE, interpreting: true, eligible: true, settledFor: null }),
    ).toBe(true);
  });

  it("reports once Tier 3 has answered THIS line with no match", () => {
    expect(
      noMatchEnterIsInert({ query: LINE, interpreting: false, eligible: true, settledFor: LINE }),
    ).toBe(false);
  });

  it("reports at once for a line Tier 3 would never be asked about", () => {
    expect(
      noMatchEnterIsInert({ query: "zz", interpreting: false, eligible: false, settledFor: null }),
    ).toBe(false);
  });

  it("an answer for a DIFFERENT line does not settle this one", () => {
    expect(
      noMatchEnterIsInert({
        query: LINE,
        interpreting: false,
        eligible: true,
        settledFor: "other",
      }),
    ).toBe(true);
  });

  it("the component forgets the settled line whenever the query changes", () => {
    // The recall case: run a Tier-3 line, ArrowUp it back, press Enter inside
    // the new debounce. Only resetting `tier3SettledFor` alongside
    // `tier3Match` in the query effect keeps that inert. Read from source
    // because `CommandBar.tsx` cannot be imported under the node environment.
    const src = readFileSync(join(__dirname, "CommandBar.tsx"), "utf8");
    expect(src).toMatch(/setTier3Match\(null\);\s*setTier3SettledFor\(null\);/);
    expect(src).toContain("noMatchEnterIsInert({");
  });
});
