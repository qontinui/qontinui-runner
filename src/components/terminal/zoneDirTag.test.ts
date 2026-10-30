/**
 * "Auto-tag zones by working directory" reads the tab's working directory
 * (review W2). It used to parse the cwd out of `tab.title`, which worked only
 * while the OSC 0/2 relay renamed the tab to the shell's cwd — since D3 of
 * plan `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`
 * the title keeps the session's name and the OSC string is `activityTitle`.
 */
import { describe, expect, it } from "vitest";

import { zoneDirTag } from "./zoneDirTag";

describe("zoneDirTag", () => {
  it("tags by the last segment of workingDir, whatever the title says", () => {
    expect(
      zoneDirTag({
        workingDir: "D:/qontinui-root/qontinui-runner",
        title: "maxim · 9b40e22d",
        activityTitle: "Fix Phase 5b round 11",
      }),
    ).toBe("qontinui-runner");
    expect(zoneDirTag({ workingDir: "C:\\work\\repo\\", title: "Terminal 1" })).toBe("repo");
  });

  it("does NOT read a session name as a directory (the regression)", () => {
    // Pre-fix, a title like this one yielded the whole name as the tag.
    expect(zoneDirTag({ workingDir: "/home/u/proj", title: "PR #12: fix the gate" })).toBe("proj");
  });

  it("without a workingDir, parses the OSC title (activityTitle) before the tab title", () => {
    expect(zoneDirTag({ title: "Terminal 2", activityTitle: "pwsh: C:\\Users\\me\\proj" })).toBe(
      "proj",
    );
    expect(zoneDirTag({ title: "bash: /srv/app" })).toBe("app");
    // A blank OSC title falls through to the tab title; with no path segment
    // to take, the whole title is the tag (the pre-D3 rule, kept).
    expect(zoneDirTag({ title: "~/notes/", activityTitle: "  " })).toBe("~/notes/");
  });

  it("names nothing when no field carries a directory or a title", () => {
    expect(zoneDirTag({ title: "", workingDir: "" })).toBeNull();
    expect(zoneDirTag({ title: "   ", activityTitle: "" })).toBeNull();
  });
});
