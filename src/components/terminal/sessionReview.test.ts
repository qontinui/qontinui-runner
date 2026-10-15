import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import {
  EXCERPT_MAX_LINES,
  ORPHAN_LABEL,
  canTransition,
  composeReviewPrompt,
  excerptForHunk,
  hunkContentHash,
  hunkKey,
  hunkKeysForFile,
  liveHunkKeys,
  markerFor,
  markerLine,
  noteTransition,
  parseMarker,
  reviewCounts,
  sha256Hex,
  truncateExcerpt,
  type ReviewNote,
  type ReviewNoteEvent,
  type ReviewNoteState,
} from "./sessionReview";
import type { DiffHunk, DiffLine } from "./workerFileChanges";

const add = (text: string): DiffLine => ({ kind: "add", text });
const del = (text: string): DiffLine => ({ kind: "del", text });
const ctx = (text: string): DiffLine => ({ kind: "ctx", text });
const hunk = (header: string, lines: DiffLine[]): DiffHunk => ({ header, lines });

function note(overrides: Partial<ReviewNote> = {}): ReviewNote {
  return {
    id: "n1",
    sessionId: "s1",
    filePath: "src/a.ts",
    hunkKey: "k-0",
    hunkHeader: "@@ -1,3 +1,4 @@",
    excerpt: " ctx\n-old\n+new",
    body: "why this?",
    state: "pending",
    marker: null,
    createdAt: "2026-10-03T00:00:00Z",
    submittedAt: null,
    confirmedAt: null,
    ...overrides,
  };
}

describe("sha256Hex", () => {
  it.each([
    "",
    "abc",
    "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
    "x".repeat(55),
    "x".repeat(56),
    "x".repeat(64),
    "x".repeat(1000),
    "naïve — ünïcödé 🎉",
  ])("matches node crypto for %j", (input) => {
    expect(sha256Hex(input)).toBe(createHash("sha256").update(input, "utf8").digest("hex"));
  });
});

describe("hunk keys", () => {
  const base = hunk("@@ -10,4 +10,4 @@", [ctx("a"), del("old"), add("new"), ctx("b")]);

  it("survive a line shift and different context", () => {
    const shifted = hunk("@@ -42,4 +45,4 @@", [ctx("x"), del("old"), add("new"), ctx("y")]);
    expect(hunkKey("src/a.ts", shifted)).toBe(hunkKey("src/a.ts", base));
  });

  it("change when the hunk's content changes", () => {
    const edited = hunk("@@ -10,4 +10,4 @@", [ctx("a"), del("old"), add("newer"), ctx("b")]);
    expect(hunkKey("src/a.ts", edited)).not.toBe(hunkKey("src/a.ts", base));
  });

  it("change when an add becomes a del of the same text", () => {
    const flipped = hunk("@@ -10,4 +10,4 @@", [ctx("a"), add("old"), del("new"), ctx("b")]);
    expect(hunkKey("src/a.ts", flipped)).not.toBe(hunkKey("src/a.ts", base));
  });

  it("differ across files", () => {
    expect(hunkKey("src/b.ts", base)).not.toBe(hunkKey("src/a.ts", base));
  });

  it("disambiguate byte-identical twins in one file with an ordinal", () => {
    const twin = hunk("@@ -90,4 +90,4 @@", [ctx("z"), del("old"), add("new"), ctx("w")]);
    const other = hunk("@@ -50,2 +50,2 @@", [del("p"), add("q")]);
    const keys = hunkKeysForFile("src/a.ts", [base, other, twin]);
    expect(new Set(keys).size).toBe(3);
    const hash = hunkContentHash("src/a.ts", base);
    expect(keys[0]).toBe(`${hash}-0`);
    expect(keys[2]).toBe(`${hash}-1`);
    // A lone hunk keeps its key when a twin appears after it.
    expect(hunkKeysForFile("src/a.ts", [base])[0]).toBe(keys[0]);
    expect(hunkKey("src/a.ts", base)).toBe(keys[0]);
  });

  it("marking one twin read does not mark the other", () => {
    const twin = hunk("@@ -90,4 +90,4 @@", [del("old"), add("new")]);
    const files = [{ filePath: "src/a.ts", hunks: [base, twin] }];
    const [first] = hunkKeysForFile("src/a.ts", [base, twin]);
    expect(reviewCounts(files, new Set([first]))).toEqual({ unread: 1, total: 2, unknown: 0 });
  });
});

describe("reviewCounts", () => {
  const h1 = hunk("@@ -1 +1 @@", [del("a"), add("b")]);
  const h2 = hunk("@@ -9 +9 @@", [del("c"), add("d")]);

  it("counts null diffs as unknown, never toward total", () => {
    const files = [
      { filePath: "a.ts", hunks: [h1, h2] },
      { filePath: "bin.png", hunks: null, status: "binary" as const },
      { filePath: "gone.ts", hunks: null },
    ];
    const read = new Set(hunkKeysForFile("a.ts", [h1]));
    expect(reviewCounts(files, read)).toEqual({ unread: 1, total: 2, unknown: 2 });
  });

  it("an all-unknown session is not zero hunks", () => {
    const counts = reviewCounts([{ filePath: "x", hunks: null, status: "unreadable" }], new Set());
    expect(counts).toEqual({ unread: 0, total: 0, unknown: 1 });
  });

  it("an unchanged file is genuinely zero, not unknown", () => {
    const counts = reviewCounts([{ filePath: "x", hunks: null, status: "unchanged" }], new Set());
    expect(counts).toEqual({ unread: 0, total: 0, unknown: 0 });
  });

  it("a content edit makes a read hunk unread again", () => {
    const read = new Set(hunkKeysForFile("a.ts", [h1]));
    const edited = hunk("@@ -1 +1 @@", [del("a"), add("b2")]);
    expect(reviewCounts([{ filePath: "a.ts", hunks: [edited] }], read).unread).toBe(1);
  });
});

describe("noteTransition", () => {
  const STATES: ReviewNoteState[] = [
    "pending",
    "attached",
    "submitted",
    "confirmed",
    "discarded",
    "unknown",
  ];
  const EVENTS: ReviewNoteEvent[] = [
    { type: "attach" },
    { type: "detach" },
    { type: "discard" },
    { type: "edit", body: "edited" },
    { type: "submit", marker: "deadbeef", at: "2026-10-03T01:00:00Z" },
    { type: "confirm", marker: "deadbeef", at: "2026-10-03T02:00:00Z" },
    { type: "sessionEnded" },
  ];
  const LEGAL = new Set([
    "pending:attach",
    "attached:detach",
    "pending:discard",
    "attached:discard",
    "pending:edit",
    "attached:edit",
    "attached:submit",
    "submitted:confirm",
    "unknown:confirm",
    "submitted:sessionEnded",
  ]);

  for (const state of STATES) {
    for (const event of EVENTS) {
      const edge = `${state}:${event.type}`;
      if (LEGAL.has(edge)) continue;
      it(`refuses ${edge}`, () => {
        const before = note({ state, marker: state === "pending" ? null : "deadbeef" });
        const result = noteTransition(before, event);
        expect(result.ok).toBe(false);
        if (!result.ok) {
          expect(result.refusal.from).toBe(state);
          expect(result.refusal.event).toBe(event.type);
          expect(result.refusal.reason).toMatch(/not a legal edge/);
        }
        expect(canTransition(state, event.type)).toBe(false);
        expect(before.state).toBe(state);
      });
    }
  }

  it("walks the happy path pending → attached → submitted → confirmed", () => {
    let n = note();
    const step = (e: ReviewNoteEvent): void => {
      const r = noteTransition(n, e);
      if (!r.ok) throw new Error(r.refusal.reason);
      n = r.note;
    };
    step({ type: "attach" });
    expect(n.state).toBe("attached");
    step({ type: "submit", marker: "0badf00d", at: "t1" });
    expect(n).toMatchObject({ state: "submitted", marker: "0badf00d", submittedAt: "t1" });
    step({ type: "confirm", marker: "0badf00d", at: "t2" });
    expect(n).toMatchObject({ state: "confirmed", confirmedAt: "t2" });
  });

  it("detach returns to pending; discard from pending and attached", () => {
    const attached = note({ state: "attached" });
    const detached = noteTransition(attached, { type: "detach" });
    expect(detached.ok && detached.note.state).toBe("pending");
    for (const state of ["pending", "attached"] as const) {
      const r = noteTransition(note({ state }), { type: "discard" });
      expect(r.ok && r.note.state).toBe("discarded");
    }
  });

  it("settles an unconfirmed submitted note to unknown when the session ends", () => {
    const r = noteTransition(note({ state: "submitted", marker: "deadbeef" }), {
      type: "sessionEnded",
    });
    expect(r.ok && r.note.state).toBe("unknown");
  });

  it("confirms an unknown note on a later sighting of its marker — the server's edge too", () => {
    const r = noteTransition(note({ state: "unknown", marker: "deadbeef" }), {
      type: "confirm",
      marker: "deadbeef",
      at: "t3",
    });
    expect(r.ok && r.note).toMatchObject({ state: "confirmed", confirmedAt: "t3" });
    const foreign = noteTransition(note({ state: "unknown", marker: "deadbeef" }), {
      type: "confirm",
      marker: "cafef00d",
      at: "t3",
    });
    expect(foreign.ok).toBe(false);
  });

  it("refuses confirmation by a different marker", () => {
    const r = noteTransition(note({ state: "submitted", marker: "deadbeef" }), {
      type: "confirm",
      marker: "cafef00d",
      at: "t",
    });
    expect(r.ok).toBe(false);
  });

  it("refuses a submit carrying a malformed marker", () => {
    const r = noteTransition(note({ state: "attached" }), {
      type: "submit",
      marker: "XYZ",
      at: "t",
    });
    expect(r.ok).toBe(false);
  });

  it("edits the body without changing state", () => {
    const r = noteTransition(note({ state: "attached" }), { type: "edit", body: "new body" });
    expect(r.ok && r.note).toMatchObject({ state: "attached", body: "new body" });
  });
});

describe("excerpts", () => {
  it("passes through an excerpt of at most 12 lines", () => {
    const text = Array.from({ length: EXCERPT_MAX_LINES }, (_, i) => `+l${i}`).join("\n");
    expect(truncateExcerpt(text)).toBe(text);
  });

  it("truncates to 12 lines, the last naming how many were cut", () => {
    const text = Array.from({ length: 30 }, (_, i) => `+l${i}`).join("\n");
    const out = truncateExcerpt(text).split("\n");
    expect(out).toHaveLength(EXCERPT_MAX_LINES);
    expect(out[10]).toBe("+l10");
    expect(out[11]).toBe("… 19 more lines");
  });

  it("renders a hunk in unified-diff form", () => {
    expect(excerptForHunk(hunk("@@", [ctx("a"), del("b"), add("c")]))).toBe(" a\n-b\n+c");
  });
});

describe("composeReviewPrompt", () => {
  const h = hunk("@@ -1,2 +1,2 @@", [ctx("keep"), del("old"), add("new")]);
  const [liveKey] = hunkKeysForFile("src/a.ts", [h]);
  const live = liveHunkKeys([{ filePath: "src/a.ts", hunks: [h] }]);
  const marker = markerFor({ sessionId: "s1", noteIds: ["n1"], freeText: "", salt: "1" });

  it("round-trips its marker and starts with the marker line", () => {
    const composed = composeReviewPrompt([note({ hunkKey: liveKey })], "also run tests", {
      marker,
      liveKeys: live,
    });
    expect(composed.text.split("\n")[0]).toBe(markerLine(marker));
    expect(parseMarker(composed.text)).toBe(marker);
    expect(parseMarker(`> pasted:\n${composed.text}`)).toBe(marker);
    expect(composed.text).toContain("1. src/a.ts @@ -1,3 +1,4 @@");
    expect(composed.text).toContain("Comment: why this?");
    expect(composed.text.endsWith("also run tests")).toBe(true);
    expect(composed.orphanedNoteIds).toEqual([]);
  });

  it("is deterministic", () => {
    const notes = [note({ hunkKey: liveKey }), note({ id: "n2", hunkKey: "gone-0" })];
    const a = composeReviewPrompt(notes, "x", { marker, liveKeys: live });
    const b = composeReviewPrompt(notes, "x", { marker, liveKeys: live });
    expect(a).toEqual(b);
  });

  it("labels orphaned notes and composes them from the stored excerpt", () => {
    const orphan = note({ id: "n2", hunkKey: "gone-0", excerpt: "-stored\n+excerpt" });
    const composed = composeReviewPrompt([note({ hunkKey: liveKey }), orphan], "", {
      marker,
      liveKeys: live,
    });
    expect(composed.orphanedNoteIds).toEqual(["n2"]);
    expect(composed.text).toContain(`2. src/a.ts @@ -1,3 +1,4 @@ ${ORPHAN_LABEL}`);
    expect(composed.text).toContain("-stored\n+excerpt");
    expect(composed.text.match(/orphaned/g)).toHaveLength(1);
  });

  it("caps each excerpt at 12 lines", () => {
    const long = Array.from({ length: 40 }, (_, i) => `+line${i}`).join("\n");
    const composed = composeReviewPrompt([note({ excerpt: long, hunkKey: liveKey })], "", {
      marker,
      liveKeys: live,
    });
    expect(composed.text).toContain("+line10\n… 29 more lines");
    expect(composed.text).not.toContain("+line11");
  });

  it("fences an excerpt containing backticks with a longer fence", () => {
    const composed = composeReviewPrompt([note({ excerpt: "+```js", hunkKey: liveKey })], "", {
      marker,
      liveKeys: live,
    });
    expect(composed.text).toContain("````diff\n+```js\n````");
  });

  it("refuses a malformed marker", () => {
    expect(() => composeReviewPrompt([], "", { marker: "nothex!!", liveKeys: live })).toThrow();
  });
});

describe("markers", () => {
  it("are 8 hex, deterministic, and salted", () => {
    const input = { sessionId: "s", noteIds: ["a", "b"], freeText: "t", salt: "1" };
    const m = markerFor(input);
    expect(m).toMatch(/^[0-9a-f]{8}$/);
    expect(markerFor(input)).toBe(m);
    expect(markerFor({ ...input, salt: "2" })).not.toBe(m);
    expect(parseMarker(markerLine(m))).toBe(m);
  });

  it("parseMarker returns null when absent or malformed", () => {
    expect(parseMarker("hello")).toBeNull();
    expect(parseMarker("[review DEADBEEF]")).toBeNull();
    expect(parseMarker("[review abc]")).toBeNull();
  });
});
