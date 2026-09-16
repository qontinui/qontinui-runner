/**
 * Pure logic behind the Conductor worker cell's "Changes" pane: pairing the
 * pre-edit snapshot with the current file into hunks, and naming honestly
 * when no diff can be shown.
 */

import { describe, it, expect } from "vitest";
import {
  changedCountLabel,
  countChanged,
  diffHunks,
  diffStat,
  noDiffReason,
  orderChanges,
  shortPath,
  type FileChangesRead,
  type SessionFileChange,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

function change(partial: Partial<SessionFileChange>): SessionFileChange {
  return {
    filePath: "/repo/src/a.ts",
    status: "modified",
    before: null,
    after: null,
    beforeBytes: null,
    afterBytes: null,
    beforeSha256: null,
    afterSha256: null,
    truncated: false,
    takenAt: null,
    detail: null,
    ...partial,
  };
}

describe("diffHunks", () => {
  it("renders a modified file as unified hunks with add/del/ctx lines", () => {
    const hunks = diffHunks(
      change({ before: "a\nb\nc\n", after: "a\nB\nc\nd\n", beforeBytes: 6, afterBytes: 8 }),
    );
    expect(hunks).not.toBeNull();
    expect(hunks!.length).toBe(1);
    expect(hunks![0].header).toBe("@@ -1,3 +1,4 @@");
    const kinds = hunks![0].lines.map((l) => `${l.kind}:${l.text}`);
    expect(kinds).toEqual(["ctx:a", "del:b", "add:B", "ctx:c", "add:d"]);
    expect(diffStat(hunks)).toEqual({ additions: 2, deletions: 1 });
  });

  it("diffs a created file from empty and a deleted file to empty", () => {
    const created = diffHunks(change({ status: "created", before: null, after: "x\ny\n" }));
    expect(diffStat(created)).toEqual({ additions: 2, deletions: 0 });
    const deleted = diffHunks(change({ status: "deleted", before: "x\n", after: null }));
    expect(diffStat(deleted)).toEqual({ additions: 0, deletions: 1 });
  });

  it("offers no diff where one would be a lie", () => {
    expect(diffHunks(change({ status: "binary" }))).toBeNull();
    expect(diffHunks(change({ status: "unreadable", detail: "blob missing" }))).toBeNull();
    expect(diffHunks(change({ status: "unchanged", before: "a", after: "a" }))).toBeNull();
    expect(diffHunks(change({ status: "modified", truncated: true }))).toBeNull();
    // `modified` with a side missing (not `created`/`deleted`) is not diffable.
    expect(diffHunks(change({ status: "modified", before: "a", after: null }))).toBeNull();
    expect(diffStat(null)).toEqual({ additions: 0, deletions: 0 });
  });
});

describe("noDiffReason", () => {
  it("names UNKNOWN for an unreadable side, with the backend's detail", () => {
    expect(noDiffReason(change({ status: "unreadable", detail: "current file unreadable: EIO" }))).toBe(
      "UNKNOWN — current file unreadable: EIO",
    );
    expect(noDiffReason(change({ status: "unreadable" }))).toContain("UNKNOWN");
  });

  it("explains binary, unchanged and oversized rows", () => {
    expect(noDiffReason(change({ status: "binary" }))).toContain("binary");
    expect(noDiffReason(change({ status: "unchanged" }))).toContain("identical");
    expect(
      noDiffReason(change({ truncated: true, beforeBytes: 300 * 1024, afterBytes: 2048 })),
    ).toBe("too large to diff (300 KB → 2 KB)");
  });

  it("returns null when a diff can be shown", () => {
    expect(noDiffReason(change({ before: "a", after: "b" }))).toBeNull();
    expect(noDiffReason(change({ status: "created", after: "a" }))).toBeNull();
  });
});

describe("ordering and counting", () => {
  it("lists unreadable first, unchanged last, and counts everything but unchanged", () => {
    const files = [
      change({ filePath: "/u.ts", status: "unchanged" }),
      change({ filePath: "/m.ts", status: "modified" }),
      change({ filePath: "/x.ts", status: "unreadable" }),
      change({ filePath: "/c.ts", status: "created" }),
    ];
    expect(orderChanges(files).map((c) => c.filePath)).toEqual(["/x.ts", "/m.ts", "/c.ts", "/u.ts"]);
    expect(countChanged(files)).toBe(3);
    // Input untouched.
    expect(files[0].filePath).toBe("/u.ts");
  });

  it("shortPath keeps the last two components on either separator", () => {
    expect(shortPath("/home/x/repo/src/lib/a.ts")).toBe("lib/a.ts");
    expect(shortPath("C:\\repo\\src\\a.ts")).toBe("src/a.ts");
    expect(shortPath("a.ts")).toBe("a.ts");
  });
});

describe("changedCountLabel", () => {
  const response = (files: SessionFileChange[]): SessionFileChangesResponse => ({
    sessionId: "w1",
    files,
    filesTruncated: false,
    omittedFiles: 0,
    readAtMs: 0,
  });
  const two = response([
    change({ filePath: "/a.ts", status: "modified" }),
    change({ filePath: "/b.ts", status: "created" }),
    change({ filePath: "/c.ts", status: "unchanged" }),
  ]);

  it("reports the live count for a settled read", () => {
    expect(changedCountLabel({ status: "ok", response: two })).toEqual({ text: "2", stale: false });
  });

  it("keeps the count the panel is still showing when the fresh read FAILED", () => {
    // The panel renders `previous` under a "may be stale" banner, so a bare
    // "?" on the tab contradicted the rows immediately below it.
    const read: FileChangesRead = {
      status: "error",
      error: "HTTP 500",
      atMs: 0,
      previous: two,
    };
    const label = changedCountLabel(read);
    expect(label.text).toBe("2 stale");
    expect(label.stale).toBe(true);
    expect(label.title).toContain("HTTP 500");
  });

  it("keeps the previous count visible during an in-flight refresh", () => {
    const label = changedCountLabel({ status: "loading", previous: two });
    expect(label.text).toBe("2 stale");
    expect(label.stale).toBe(true);
  });

  it("is ? ONLY when nothing has ever been read — and says why", () => {
    const never = changedCountLabel({ status: "loading", previous: null });
    expect(never).toMatchObject({ text: "?", stale: true });
    expect(never.title).toContain("UNKNOWN");
    const failedFirst = changedCountLabel({
      status: "error",
      error: "boom",
      atMs: 0,
      previous: null,
    });
    expect(failedFirst.text).toBe("?");
    expect(failedFirst.title).toContain("boom");
  });
});
