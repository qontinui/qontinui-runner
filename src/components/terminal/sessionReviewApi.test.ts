/**
 * The review routes' reply parsing: a body that is not the shape the route
 * promises is a typed failure (UNKNOWN to the caller), never an empty list.
 */

import { describe, it, expect } from "vitest";
import {
  errorFromReply,
  parseNoteRow,
  parseSendOutcome,
  parseSessionReview,
  parseTarget,
  ReviewApiError,
} from "./sessionReviewApi";

const rawNote = {
  id: "n1",
  sessionId: "s",
  filePath: "/r/a.ts",
  hunkKey: "k-0",
  hunkHeader: "@@ -1 +1 @@",
  excerpt: "-a\n+b",
  body: "why?",
  state: "submitted",
  marker: "abcd1234",
  createdAt: "2026-10-03T00:00:00.000Z",
  submittedAt: "2026-10-03T00:01:00.000Z",
  confirmedAt: null,
  target: { terminalId: "term-1" },
};

function expectMalformed(fn: () => unknown) {
  try {
    fn();
  } catch (err) {
    expect(err).toBeInstanceOf(ReviewApiError);
    expect((err as ReviewApiError).code).toBe("malformed");
    return;
  }
  throw new Error("expected a malformed-payload error");
}

describe("parseSessionReview", () => {
  it("reads the GET shape, discarded notes included", () => {
    const r = parseSessionReview(
      {
        sessionId: "s",
        readHunks: [{ hunkKey: "k-0", filePath: "/r/a.ts", readAt: "t" }],
        notes: [rawNote, { ...rawNote, id: "n2", state: "discarded", target: null }],
      },
      "s",
    );
    expect(r.readHunks.map((h) => h.hunkKey)).toEqual(["k-0"]);
    expect(r.notes.map((n) => n.state)).toEqual(["submitted", "discarded"]);
    expect(r.notes[0].target).toEqual({ terminalId: "term-1" });
    expect(r.notes[1].target).toBeNull();
  });

  it("refuses a body with no arrays rather than reading it as empty", () => {
    expectMalformed(() => parseSessionReview({ sessionId: "s" }, "s"));
    expectMalformed(() => parseSessionReview({ readHunks: [], notes: "x" }, "s"));
    expectMalformed(() => parseSessionReview(null, "s"));
  });

  it("refuses a note in a state the lifecycle does not have", () => {
    expectMalformed(() => parseNoteRow({ ...rawNote, state: "sent" }));
    expectMalformed(() => parseNoteRow({ ...rawNote, id: undefined }));
  });

  it("reads both target kinds and refuses anything else", () => {
    expect(parseTarget({ taskRunId: "r" })).toEqual({ taskRunId: "r" });
    expect(parseTarget(null)).toBeNull();
    expectMalformed(() => parseTarget({ pane: "x" }));
  });
});

describe("parseSendOutcome", () => {
  it("reads a send reply", () => {
    const out = parseSendOutcome({
      sessionId: "s",
      marker: "abcd1234",
      submitted: true,
      sanitized: false,
      bytes: 120,
      notes: [rawNote],
    });
    expect(out).toMatchObject({
      marker: "abcd1234",
      submitted: true,
      sanitized: false,
      bytes: 120,
    });
  });

  it("keeps a task run's absent sanitized/bytes as null, not false/0", () => {
    const out = parseSendOutcome({
      marker: "abcd1234",
      submitted: true,
      sanitized: null,
      bytes: null,
      notes: [],
    });
    expect(out.sanitized).toBeNull();
    expect(out.bytes).toBeNull();
  });

  it("refuses a reply with no marker", () => {
    expectMalformed(() => parseSendOutcome({ submitted: true, notes: [] }));
  });
});

describe("errorFromReply", () => {
  it("carries the route's typed code and note ids", () => {
    const err = errorFromReply(
      409,
      JSON.stringify({ error: "not legal", code: "illegal_transition", noteIds: ["n1"] }),
    );
    expect(err.status).toBe(409);
    expect(err.code).toBe("illegal_transition");
    expect(err.noteIds).toEqual(["n1"]);
    expect(err.message).toContain("not legal");
  });

  it("falls back to the status for a non-JSON body", () => {
    const err = errorFromReply(502, "Bad Gateway");
    expect(err.code).toBe("http_502");
    expect(err.message).toContain("Bad Gateway");
  });
});
