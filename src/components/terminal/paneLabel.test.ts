/**
 * Pane label (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4 / D3).
 *
 * The composition is pinned to `__fixtures__/pane-label.json`, the same table
 * `src-tauri/src/session/pane_label.rs` reads, so the Rust and TS twins cannot
 * drift.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import {
  conversationLabel,
  conversationLabelParts,
  id8,
  NO_CONVERSATION_BOUND,
  paneLabel,
  UNNAMED,
} from "./paneLabel";
import { resolveDisplayTitle, resolvePaneLabel } from "./displayTitle";
import { relayActivityTitle } from "./activityTitleRelay";

interface FixtureCase {
  case: string;
  name: string | null;
  claudeSessionId: string | null;
  label: string;
}

const FIXTURE_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "__fixtures__/pane-label.json",
);
const CASES: FixtureCase[] = (
  JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as { cases: FixtureCase[] }
).cases;

const LEAD_ID = "9b40e22d-deec-415c-9379-5f85ae994ad8";

describe("conversationLabel — the shared fixture table", () => {
  it("the table is non-trivial (a table that parsed to nothing would pass vacuously)", () => {
    expect(CASES.length).toBeGreaterThanOrEqual(8);
  });

  it.each(CASES)("$case", (c) => {
    expect(conversationLabel(c.name, c.claudeSessionId)).toBe(c.label);
  });
});

describe("conversationLabel — the named arms", () => {
  it("name + id", () => {
    expect(conversationLabel("maxim", LEAD_ID)).toBe("maxim · 9b40e22d");
  });

  it("no name", () => {
    expect(conversationLabel(undefined, LEAD_ID)).toBe(`${UNNAMED} · 9b40e22d`);
  });

  it("no id", () => {
    expect(conversationLabel("maxim", undefined)).toBe(NO_CONVERSATION_BOUND);
    expect(conversationLabelParts("maxim", "  ")).toBeNull();
  });

  it("a long name keeps the `· <id8>` suffix as its own part", () => {
    const long = "x".repeat(500);
    const parts = conversationLabelParts(long, LEAD_ID);
    expect(parts).toEqual({ name: long, suffix: " · 9b40e22d" });
    expect(conversationLabel(long, LEAD_ID).endsWith(" · 9b40e22d")).toBe(true);
  });

  it("id8 counts code points, not UTF-16 units", () => {
    expect(id8("😀😀😀😀😀😀😀😀😀")).toBe("😀😀😀😀😀😀😀😀");
  });
});

describe("paneLabel — identity first, activity second", () => {
  it("a conversation pane leads with <name> · <id8> and shows the activity as secondary", () => {
    expect(
      paneLabel({
        conversationName: "maxim",
        claudeSessionId: LEAD_ID,
        activityTitle: "Fix Phase 5b round 11",
        shellTitle: "ignored",
      }),
    ).toEqual({
      primary: "maxim · 9b40e22d",
      name: "maxim",
      idSuffix: " · 9b40e22d",
      secondary: "Fix Phase 5b round 11",
    });
  });

  it("a blank or duplicate activity is no secondary line", () => {
    const base = { conversationName: "maxim", claudeSessionId: LEAD_ID, shellTitle: "x" };
    expect(paneLabel({ ...base, activityTitle: "  " }).secondary).toBeUndefined();
    expect(paneLabel({ ...base, activityTitle: "maxim · 9b40e22d" }).secondary).toBeUndefined();
  });

  it("a plain shell keeps its shell title as the primary line", () => {
    expect(
      paneLabel({
        claudeSessionId: undefined,
        activityTitle: "vim notes.md",
        shellTitle: "vim notes.md",
      }),
    ).toEqual({ primary: "vim notes.md" });
  });
});

describe("resolvePaneLabel over a tab", () => {
  const tab = {
    title: "claude 9b40e22d",
    workingDir: "D:/qontinui-root",
    claudeSessionId: LEAD_ID,
  };

  it("the live registry name leads, and the OSC activity never replaces it", () => {
    const label = resolvePaneLabel({
      tab: { ...tab, activityTitle: "Fix Phase 5b round 11" },
      registryNames: new Map([[LEAD_ID, "maxim"]]),
    });
    expect(label.primary).toBe("maxim · 9b40e22d");
    expect(label.secondary).toBe("Fix Phase 5b round 11");
  });

  it("falls back to the spawn name, then the non-path title, then (unnamed)", () => {
    expect(resolvePaneLabel({ tab: { ...tab, spawnName: "impl-x" } }).primary).toBe(
      "impl-x · 9b40e22d",
    );
    expect(resolvePaneLabel({ tab }).primary).toBe("claude 9b40e22d · 9b40e22d");
    expect(resolvePaneLabel({ tab: { ...tab, title: "D:/qontinui-root" } }).primary).toBe(
      "(unnamed) · 9b40e22d",
    );
  });

  it("an activity title is never the conversation's name", () => {
    const label = resolvePaneLabel({
      tab: { ...tab, title: "", activityTitle: "Fix Phase 5b round 11" },
    });
    expect(label.primary).toBe("(unnamed) · 9b40e22d");
  });

  it("a plain shell titles from its OSC title, as it did before D3", () => {
    const shell = { title: "Terminal 2", workingDir: "C:/repo", activityTitle: "htop" };
    expect(resolvePaneLabel({ tab: shell })).toEqual({ primary: "htop" });
    // A path-shaped OSC title (a shell prompt's cwd) is still kept off it.
    expect(resolvePaneLabel({ tab: { ...shell, activityTitle: "C:/repo" } })).toEqual({
      primary: "Terminal 2",
    });
  });

  it("resolveDisplayTitle never titles a conversation from its activity", () => {
    expect(
      resolveDisplayTitle({ tab: { ...tab, title: "", activityTitle: "Fix Phase 5b round 11" } }),
    ).toBe("Terminal");
  });
});

describe("the OSC relay sets activityTitle and never renames the tab", () => {
  it("updates activityTitle and invokes terminal_set_title with the trimmed title", async () => {
    const updateTab = vi.fn();
    const invokeFn = vi.fn().mockResolvedValue(undefined);
    relayActivityTitle(updateTab, "t-1", "  Fix Phase 5b round 11 ", invokeFn);
    expect(updateTab).toHaveBeenCalledWith("t-1", { activityTitle: "Fix Phase 5b round 11" });
    // Only the activity field: `title` (the identity) is never in the update.
    expect(Object.keys(updateTab.mock.calls[0][1])).toEqual(["activityTitle"]);
    expect(invokeFn).toHaveBeenCalledWith("terminal_set_title", {
      terminalId: "t-1",
      title: "Fix Phase 5b round 11",
    });
  });

  it("a blank title does nothing", () => {
    const updateTab = vi.fn();
    const invokeFn = vi.fn().mockResolvedValue(undefined);
    relayActivityTitle(updateTab, "t-1", "   ", invokeFn);
    expect(updateTab).not.toHaveBeenCalled();
    expect(invokeFn).not.toHaveBeenCalled();
  });

  it("ZoneGrid's onTitleChange routes through the relay, not renameTab", () => {
    const src = readFileSync(
      resolve(dirname(fileURLToPath(import.meta.url)), "ZoneGrid.tsx"),
      "utf8",
    );
    const start = src.indexOf("const onTitleChange = useCallback(");
    expect(start).toBeGreaterThan(-1);
    const body = src.slice(start, src.indexOf(");", start) + 2);
    expect(body).toContain("relayActivityTitle(updateTab");
    expect(body).not.toContain("renameTab");
  });
});
