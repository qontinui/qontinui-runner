/**
 * `sessionReadyToClose` — Chrome's Declutter for sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). A row whose `is:closable` is UNKNOWN is counted, never offered.
 */

import { describe, expect, it } from "vitest";

import { known, unknownFact, type PrFact, type SessionFacts } from "./sessionFacts";
import { facts } from "./sessionFacts.testkit";
import { closableReason, readyToClose, readyToCloseBanner } from "./sessionReadyToClose";

const pr = (ci: PrFact["ci"]): PrFact => ({
  repo: "qontinui/qontinui-runner",
  number: 7,
  ci,
  authorActs: false,
  reason: null,
});

const finished = (over: Partial<SessionFacts> = {}) =>
  facts({ finished: known(true), prs: known([pr("green")]), ...over });

describe("closableReason", () => {
  it("names the finished marker and green PRs", () => {
    expect(closableReason(finished())).toBe("Finished · PRs green");
    expect(closableReason(finished({ prs: known([]) }))).toBe("Finished");
    expect(closableReason(finished({ prs: known([pr("landed"), pr("pending")]) }))).toBe(
      "Finished",
    );
  });

  it("names a clean exit", () => {
    expect(
      closableReason(
        facts({ activity: known("exited"), exitCode: 0, finished: known(false), prs: known([]) }),
      ),
    ).toBe("Exited 0");
  });
});

describe("readyToClose", () => {
  interface Row {
    id: string;
    tab: boolean;
    f?: SessionFacts;
  }
  const rows: Row[] = [
    { id: "done", tab: true, f: finished() },
    { id: "working", tab: true, f: facts({ activity: known("working") }) },
    { id: "no-tab", tab: false, f: finished() },
    { id: "unhydrated", tab: true },
    {
      id: "undecidable",
      tab: true,
      f: facts({ finished: unknownFact("no marker source"), activity: unknownFact("no probe") }),
    },
    {
      id: "exited",
      tab: true,
      f: facts({ activity: known("exited"), exitCode: 0, finished: known(false), prs: known([]) }),
    },
    {
      id: "owed",
      tab: true,
      f: finished({ prs: known([{ ...pr("red"), authorActs: true }]) }),
    },
  ];
  const result = readyToClose(
    rows,
    (r) => r.f,
    (r) => r.tab,
  );

  it("offers exactly the closable rows with a tab here, in order, with their reason", () => {
    expect(result.ready.map((r) => [r.row.id, r.reason])).toEqual([
      ["done", "Finished · PRs green"],
      ["exited", "Exited 0"],
    ]);
  });

  it("counts the rows it could not evaluate instead of hiding them", () => {
    expect(result.unknownCount).toBe(2);
  });
});

describe("readyToCloseBanner", () => {
  it("is absent with nothing ready, singular/plural otherwise, and states unknowns", () => {
    expect(readyToCloseBanner(0, 3)).toBeNull();
    expect(readyToCloseBanner(1, 0)).toBe("1 session is finished");
    expect(readyToCloseBanner(4, 2)).toBe("4 sessions are finished · 2 could not be evaluated");
  });
});
