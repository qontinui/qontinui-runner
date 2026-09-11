/**
 * Tests for the `PathsSettings` pure helpers.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so — as
 * `resourceGuardHelpers.test.ts` does — we test the exported pure helpers
 * rather than rendering.
 *
 * The cases that matter are the ones on the WIRE BOUNDARY: an unset path is
 * absent, never `""`; the untouched fields round-trip; and "configured" vs "in
 * effect" is a comparison of paths, not of strings.
 */

import { describe, expect, it } from "vitest";

import {
  PATH_FIELDS,
  buildPathSettingsPayload,
  canonicalPath,
  divergenceKind,
  draftsAreDirty,
  draftsFrom,
  formatRefAge,
  normalizePathInput,
  planScanStatusLabel,
  resolvedDiffers,
  scanSourceStatus,
  type PathSettings,
  type ScanDivergenceView,
} from "./pathsSettingsHelpers";

const SAVED: PathSettings = {
  plans_dir: "/home/me/qontinui-dev-notes/plans",
  plans_archive_dir: "/home/me/qontinui-dev-notes/plans/archive",
  workspace_root: "/home/me/qontinui-root",
  strict_mode: true,
};

describe("normalizePathInput — blank is unset", () => {
  it("turns an empty or whitespace-only box into undefined", () => {
    expect(normalizePathInput("")).toBeUndefined();
    expect(normalizePathInput("   \t")).toBeUndefined();
    expect(normalizePathInput(null)).toBeUndefined();
    expect(normalizePathInput(undefined)).toBeUndefined();
  });

  it("trims a pasted path and otherwise leaves it alone", () => {
    expect(normalizePathInput("  C:\\Users\\me\\plans  ")).toBe("C:\\Users\\me\\plans");
    expect(normalizePathInput("/srv/plans/")).toBe("/srv/plans/");
  });
});

describe("canonicalPath — one spelling for comparison", () => {
  it("folds backslashes and trailing separators", () => {
    expect(canonicalPath("C:\\Users\\me\\plans\\")).toBe("C:/Users/me/plans");
    expect(canonicalPath("/srv/plans///")).toBe("/srv/plans");
  });

  it("upper-cases a Windows drive letter", () => {
    expect(canonicalPath("c:/users/me")).toBe("C:/users/me");
  });

  it("keeps a bare root's one separator", () => {
    expect(canonicalPath("/")).toBe("/");
    expect(canonicalPath("C:\\")).toBe("C:/");
    expect(canonicalPath("c:/")).toBe("C:/");
  });
});

describe("resolvedDiffers — configured vs. in effect", () => {
  it("agrees across the spellings a path picks up in Rust", () => {
    expect(resolvedDiffers("C:\\Users\\me\\plans\\", "C:/Users/me/plans")).toBe(false);
    expect(resolvedDiffers("/srv/plans/", "/srv/plans")).toBe(false);
    expect(resolvedDiffers("  /srv/plans", "/srv/plans")).toBe(false);
  });

  it("treats absent on both sides as agreement", () => {
    expect(resolvedDiffers(undefined, null)).toBe(false);
    expect(resolvedDiffers("", null)).toBe(false);
    expect(resolvedDiffers(undefined, "")).toBe(false);
  });

  it("flags absent on exactly one side", () => {
    expect(resolvedDiffers(undefined, "/from/env")).toBe(true);
    expect(resolvedDiffers("/configured", null)).toBe(true);
  });

  it("flags two genuinely different directories", () => {
    expect(resolvedDiffers("/a/plans", "/b/plans")).toBe(true);
    // A parent is not its child.
    expect(resolvedDiffers("/a", "/a/plans")).toBe(true);
  });
});

describe("divergenceKind — why they differ", () => {
  it("is none when they agree", () => {
    for (const field of PATH_FIELDS) {
      expect(divergenceKind(field, "/x", "/x/")).toBe("none");
      expect(divergenceKind(field, undefined, null)).toBe("none");
    }
  });

  it("names the env override / ancestor walk for workspace_root", () => {
    expect(divergenceKind("workspace_root", "/configured", "/from/env")).toBe("override");
    expect(divergenceKind("workspace_root", undefined, "/from/ancestor/walk")).toBe("fallback");
  });

  it("calls an unset dev_logs_dir a fallback, not a discrepancy", () => {
    expect(divergenceKind("dev_logs_dir", undefined, "/home/me/.local/share/qontinui/logs")).toBe(
      "fallback",
    );
    // A CONFIGURED value the process has not picked up is a lag, not a fallback.
    expect(divergenceKind("dev_logs_dir", "/new/logs", "/old/logs")).toBe("restart");
  });

  it("calls a plan-corpus difference a scan-interval lag", () => {
    expect(divergenceKind("plans_dir", "/new/plans", "/old/plans")).toBe("lag");
    expect(divergenceKind("plans_dir", "/new/plans", null)).toBe("lag");
    expect(divergenceKind("prompts_dir", undefined, "/still/scanned")).toBe("lag");
  });
});

describe("draftsFrom / buildPathSettingsPayload — the wire boundary", () => {
  it("renders an unset field as an empty box", () => {
    expect(draftsFrom(SAVED)).toEqual({
      plans_dir: "/home/me/qontinui-dev-notes/plans",
      prompts_dir: "",
      workspace_root: "/home/me/qontinui-root",
      dev_logs_dir: "",
    });
  });

  it("sends a blank box as an ABSENT key, never as an empty string", () => {
    const payload = buildPathSettingsPayload(SAVED, {
      ...draftsFrom(SAVED),
      plans_dir: "",
      workspace_root: "   ",
    });
    expect("plans_dir" in payload).toBe(false);
    expect("workspace_root" in payload).toBe(false);
    expect("prompts_dir" in payload).toBe(false);
    expect("dev_logs_dir" in payload).toBe(false);
    expect(JSON.stringify(payload)).not.toContain('""');
  });

  it("round-trips the fields the panel does not edit, untouched", () => {
    // `plans_archive_dir` (being removed by PR #1288, not shown) and
    // `strict_mode` (a behaviour flag, belongs elsewhere) must survive a save
    // exactly as loaded — a panel that dropped them would be a silent reset.
    const payload = buildPathSettingsPayload(SAVED, {
      ...draftsFrom(SAVED),
      prompts_dir: "/home/me/qontinui-dev-notes/plans/prompts",
    });
    expect(payload.plans_archive_dir).toBe(SAVED.plans_archive_dir);
    expect(payload.strict_mode).toBe(true);
    expect(payload.prompts_dir).toBe("/home/me/qontinui-dev-notes/plans/prompts");
    expect(payload.plans_dir).toBe(SAVED.plans_dir);

    const falseStrict = buildPathSettingsPayload(
      { ...SAVED, strict_mode: false },
      draftsFrom(SAVED),
    );
    expect(falseStrict.strict_mode).toBe(false);
  });

  it("trims what it does send", () => {
    const payload = buildPathSettingsPayload(SAVED, {
      ...draftsFrom(SAVED),
      dev_logs_dir: "  /var/log/qontinui  ",
    });
    expect(payload.dev_logs_dir).toBe("/var/log/qontinui");
  });

  it("does not mutate the loaded struct", () => {
    const before = JSON.stringify(SAVED);
    buildPathSettingsPayload(SAVED, { ...draftsFrom(SAVED), plans_dir: "" });
    expect(JSON.stringify(SAVED)).toBe(before);
  });
});

describe("draftsAreDirty", () => {
  it("is clean straight after a load", () => {
    expect(draftsAreDirty(SAVED, draftsFrom(SAVED))).toBe(false);
  });

  it("ignores whitespace that would not be persisted", () => {
    expect(draftsAreDirty(SAVED, { ...draftsFrom(SAVED), prompts_dir: "   " })).toBe(false);
    expect(
      draftsAreDirty(SAVED, { ...draftsFrom(SAVED), plans_dir: `  ${SAVED.plans_dir}  ` }),
    ).toBe(false);
  });

  it("sees a cleared field and a new value", () => {
    expect(draftsAreDirty(SAVED, { ...draftsFrom(SAVED), plans_dir: "" })).toBe(true);
    expect(draftsAreDirty(SAVED, { ...draftsFrom(SAVED), prompts_dir: "/p" })).toBe(true);
  });
});

describe("planScanStatusLabel", () => {
  it("says off when the tier is off, whatever the count claims", () => {
    expect(planScanStatusLabel(false, null)).toBe("Plan scanning: off");
    expect(planScanStatusLabel(false, 3)).toBe("Plan scanning: off");
  });

  it("reports the root count, and a null count as unknown rather than 0", () => {
    expect(planScanStatusLabel(true, 3)).toBe("Plan scanning: on (3 scan roots)");
    expect(planScanStatusLabel(true, 1)).toBe("Plan scanning: on (1 scan root)");
    expect(planScanStatusLabel(true, 0)).toBe("Plan scanning: on (0 scan roots)");
    expect(planScanStatusLabel(true, null)).toBe("Plan scanning: on (scan roots: unknown)");
  });
});

/** A `measured` reading with the operator box's numbers against a fresh ref. */
function measured(overrides: Partial<ScanDivergenceView> = {}): ScanDivergenceView {
  return {
    state: "measured",
    plans_dir: "/home/me/qontinui-dev-notes/plans",
    repo_root: "/home/me/qontinui-dev-notes",
    default_ref: "origin/main",
    ref_sha: "a".repeat(40),
    head_sha: "b".repeat(40),
    behind: 2153,
    ahead: 11,
    ref_age_secs: 300,
    counts_are_floors: false,
    detail: null,
    ...overrides,
  };
}

/** The plans dir `measured()` reports, in effect with the tier on. */
const ACTIVE = { plans_dir: "/home/me/qontinui-dev-notes/plans", plan_tier_active: true };

function status(
  view: ScanDivergenceView | null,
  inEffect: { plans_dir: string | null; plan_tier_active: boolean } = ACTIVE,
) {
  return scanSourceStatus(view, inEffect);
}

describe("formatRefAge", () => {
  it("renders whole units, rounded down", () => {
    expect(formatRefAge(0)).toBe("0s");
    expect(formatRefAge(59)).toBe("59s");
    expect(formatRefAge(60)).toBe("1m");
    expect(formatRefAge(3599)).toBe("59m");
    expect(formatRefAge(7 * 3600)).toBe("7h");
    expect(formatRefAge(3 * 86_400 + 5)).toBe("3d");
  });
});

describe("scanSourceStatus — the floor rule reaches the panel", () => {
  it("renders a missing reading as not measured, never in step", () => {
    const s = status(null);
    expect(s.tone).toBe("unknown");
    expect(s.headline).not.toMatch(/in step/);
  });

  it("reports current counts against a fresh ref as they are", () => {
    const s = status(measured());
    expect(s.tone).toBe("warn");
    expect(s.headline).toBe("Scan source: 2153 behind origin/main, 11 ahead");
    expect(s.detail).toContain("missing from what this machine feeds the corpus");
    expect(s.detail).toContain("11 commit(s) of plan content");
    expect(s.detail).toContain("refreshed 5m earlier");
  });

  it("explains an ahead-only divergence without claiming anything is missing", () => {
    const s = status(measured({ behind: 0, ahead: 11 }));
    expect(s.tone).toBe("warn");
    expect(s.headline).toBe("Scan source: 11 ahead of origin/main");
    expect(s.detail).not.toContain("missing");
    expect(s.detail).toContain("11 commit(s) of plan content that origin/main does not carry");
  });

  it("reads 0/0 against a ref PROVEN current as in step", () => {
    const s = status(measured({ behind: 0, ahead: 0 }));
    expect(s.tone).toBe("ok");
    expect(s.headline).toBe("Scan source: in step with origin/main");
    expect(s.detail).toBe("As of this reading, origin/main had last been refreshed 5m earlier.");
  });

  it("renders stale-ref counts as a lower bound", () => {
    const s = status(measured({ ref_age_secs: 7 * 3600, counts_are_floors: true }));
    expect(s.tone).toBe("warn");
    expect(s.headline).toBe("Scan source: at least 2153 behind origin/main, up to 11 ahead");
    expect(s.detail).toContain("refreshed 7h earlier");
    expect(s.detail).toContain("ahead count may overstate");
  });

  it("never renders a floor of 0 behind as in step — stale or unknown age, any ahead", () => {
    const floors = [
      measured({ behind: 0, ahead: 0, ref_age_secs: 7 * 3600, counts_are_floors: true }),
      measured({
        behind: 0,
        ahead: 0,
        ref_age_secs: null,
        counts_are_floors: true,
        detail: "no reflog entry",
      }),
      measured({ behind: 0, ahead: 4, ref_age_secs: null, counts_are_floors: true }),
    ];
    for (const view of floors) {
      const s = status(view);
      expect(s.tone).not.toBe("ok");
      expect(s.headline).not.toMatch(/in step/);
      expect(s.headline).toContain("lower bound");
    }
    expect(status(floors[0]).tone).toBe("unknown");
    expect(status(floors[1]).detail).toContain("no reflog entry");
    expect(status(floors[2]).tone).toBe("warn");
    expect(status(floors[2]).headline).toBe(
      "Scan source: up to 4 ahead of origin/main; behind unknown (0 is a lower bound, not agreement)",
    );
  });

  it("takes the floor flag from the runner rather than re-deriving it from the age", () => {
    // A young age with the flag set still renders as a floor: the window lives
    // in Rust, and the panel does not second-guess it.
    const s = status(measured({ behind: 0, ahead: 0, ref_age_secs: 60, counts_are_floors: true }));
    expect(s.headline).not.toMatch(/in step/);
    // The wording defers to the runner's verdict rather than asserting an
    // age comparison the panel did not make.
    expect(s.detail).toContain("outside the adapter's freshness window");
    expect(s.detail).not.toMatch(/longer than/);
  });

  it("gives the non-measured states no counts", () => {
    const off = status(
      measured({
        state: "not_scanning",
        plans_dir: null,
        behind: null,
        ahead: null,
        ref_age_secs: null,
      }),
      { plans_dir: null, plan_tier_active: false },
    );
    expect(off.tone).toBe("off");
    const plain = status(
      measured({
        state: "not_a_git_work_tree",
        behind: null,
        ahead: null,
        detail: "not inside a git work tree",
      }),
    );
    expect(plain.tone).toBe("unknown");
    expect(plain.detail).toBe("not inside a git work tree");
    const unknown = status(
      measured({ state: "unknown", behind: null, ahead: null, detail: "origin/HEAD is not set" }),
    );
    expect(unknown.headline).toBe("Scan source: drift unknown");
    expect(unknown.detail).toBe("origin/HEAD is not set");
    for (const s of [off, plain, unknown]) expect(s.headline).not.toMatch(/\d+ behind/);
  });

  it("treats a measured reading missing a count as unknown rather than inventing a 0", () => {
    const s = status(measured({ ahead: null }));
    expect(s.tone).toBe("unknown");
    expect(s.headline).toBe("Scan source: drift unknown");
  });
});

describe("scanSourceStatus — a reading for another directory is not this one's", () => {
  it("reads a reading taken before a save moved the plans dir as not measured yet", () => {
    const s = status(measured({ behind: 0, ahead: 0 }), {
      plans_dir: "/elsewhere/plans",
      plan_tier_active: true,
    });
    expect(s.tone).toBe("unknown");
    expect(s.headline).toBe("Scan source: not measured yet for this directory");
  });

  it("compares the directories as paths, not strings", () => {
    const s = status(measured({ behind: 0, ahead: 0 }), {
      plans_dir: "/home/me/qontinui-dev-notes/plans/",
      plan_tier_active: true,
    });
    expect(s.tone).toBe("ok");
  });

  it("reads not_scanning with the tier just turned on as not measured yet", () => {
    const s = status(
      measured({
        state: "not_scanning",
        plans_dir: null,
        behind: null,
        ahead: null,
        ref_age_secs: null,
      }),
    );
    expect(s.tone).toBe("unknown");
    expect(s.headline).toBe("Scan source: not measured yet for this directory");
  });

  it("says nothing is scanned when the tier is off, whatever the last reading was", () => {
    expect(status(measured(), { plans_dir: null, plan_tier_active: false }).tone).toBe("off");
    expect(status(null, { plans_dir: null, plan_tier_active: false }).tone).toBe("off");
  });
});
