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
  normalizePathInput,
  planScanStatusLabel,
  resolvedDiffers,
  type PathSettings,
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
    expect(divergenceKind("dev_logs_dir", "/new/logs", "/old/logs")).toBe("lag");
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
