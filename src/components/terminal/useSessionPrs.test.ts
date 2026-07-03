/**
 * Tests for the pure helpers behind the zone-header PR dropdown: ordering
 * (unmerged first — the actionable rows the operator opens the dropdown
 * for), the all-merged roll-up driving the green/red trigger symbol, and
 * the GitHub-link guard for coord's bare-repo-name skew. The hook itself
 * needs a DOM + Tauri IPC; the helpers are pure (vitest `node` env, same
 * precedent as the sibling ZoneLabel spec tests).
 */

import { describe, it, expect } from "vitest";
import {
  sortPrsUnmergedFirst,
  summarizePrs,
  prGithubUrl,
  prLabel,
  type SessionPr,
} from "./useSessionPrs";

function pr(overrides: Partial<SessionPr> & Pick<SessionPr, "pr_number" | "merged">): SessionPr {
  return {
    repo: "qontinui/qontinui-runner",
    branch: "feat/x",
    pr_state: overrides.merged ? "merged" : "open",
    merged_at: null,
    ...overrides,
  };
}

describe("sortPrsUnmergedFirst", () => {
  it("puts unmerged PRs above merged ones, newest first within each group", () => {
    const sorted = sortPrsUnmergedFirst([
      pr({ pr_number: 660, merged: true }),
      pr({ pr_number: 670, merged: false }),
      pr({ pr_number: 665, merged: true }),
      pr({ pr_number: 650, merged: false }),
    ]);
    expect(sorted.map((p) => p.pr_number)).toEqual([670, 650, 665, 660]);
    expect(sorted.map((p) => p.merged)).toEqual([false, false, true, true]);
  });

  it("does not mutate its input", () => {
    const input = [pr({ pr_number: 1, merged: true }), pr({ pr_number: 2, merged: false })];
    sortPrsUnmergedFirst(input);
    expect(input.map((p) => p.pr_number)).toEqual([1, 2]);
  });
});

describe("summarizePrs", () => {
  it("is green (allMerged) only when every PR merged", () => {
    expect(
      summarizePrs([pr({ pr_number: 1, merged: true }), pr({ pr_number: 2, merged: true })]),
    ).toEqual({ total: 2, unmerged: 0, allMerged: true });
    expect(
      summarizePrs([pr({ pr_number: 1, merged: true }), pr({ pr_number: 2, merged: false })]),
    ).toEqual({ total: 2, unmerged: 1, allMerged: false });
  });

  it("is vacuously green on an empty list (dropdown hides itself anyway)", () => {
    expect(summarizePrs([])).toEqual({ total: 0, unmerged: 0, allMerged: true });
  });
});

describe("prGithubUrl / prLabel", () => {
  it("links owner/name repos and shows the bare-name label", () => {
    const p = pr({ pr_number: 663, merged: false });
    expect(prGithubUrl(p)).toBe("https://github.com/qontinui/qontinui-runner/pull/663");
    expect(prLabel(p)).toBe("qontinui-runner#663");
  });

  it("refuses a link for coord's bare checkout-name skew", () => {
    const p = pr({ pr_number: 12, merged: false, repo: "qontinui-runner" });
    expect(prGithubUrl(p)).toBeNull();
    expect(prLabel(p)).toBe("qontinui-runner#12");
  });
});
