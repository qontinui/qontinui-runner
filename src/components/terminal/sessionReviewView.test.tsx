/**
 * The review surface's decisions (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 4). The vitest environment is `node` with no React Testing Library, so
 * — as `HiddenWorkersChip.test.tsx` does — the decisions are tested on their
 * pure functions and the markup contracts with `renderToStaticMarkup`.
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DiffView, FileChangesPanel, type HunkReview } from "./FileChangesView";
import { ReviewSendBar, SessionReviewBadge, SessionReviewToggle } from "./SessionReviewPanel";
import { hunkKeysForFile, parseMarker } from "./sessionReview";
import type { ReviewNoteRow, SessionReview } from "./sessionReviewApi";
import {
  attachedNotes,
  canInsert,
  composeForSend,
  initialSendBar,
  isOrphaned,
  liveKeysOf,
  pendingNotes,
  readKeySet,
  reviewBadgeState,
  scrolledPast,
  sendBarReducer,
  sendFailureKind,
  sendResultLabel,
  sentNotes,
  shouldAutoMarkRead,
  visibleNotes,
  type SessionReviewRead,
} from "./sessionReviewView";
import type { SessionReviewHandle } from "./useSessionReview";
import {
  diffHunks,
  type FileChangesRead,
  type SessionFileChange,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

const SESSION = "11111111-2222-3333-4444-555555555555";

function change(partial: Partial<SessionFileChange>): SessionFileChange {
  return {
    filePath: "/repo/src/a.ts",
    status: "modified",
    before: "one\ntwo\nthree\n",
    after: "one\nTWO\nthree\n",
    beforeBytes: 14,
    afterBytes: 14,
    beforeSha256: "a",
    afterSha256: "b",
    truncated: false,
    takenAt: null,
    detail: null,
    beforeSource: "git_base",
    baseKind: "head",
    baseSha: "0123456789abcdef",
    ...partial,
  };
}

function response(files: SessionFileChange[]): SessionFileChangesResponse {
  return {
    sessionId: SESSION,
    files,
    filesTruncated: false,
    omittedFiles: 0,
    baseKind: "head",
    baseSha: "0123456789abcdef",
    readAtMs: 0,
  };
}

function note(partial: Partial<ReviewNoteRow>): ReviewNoteRow {
  return {
    id: "n1",
    sessionId: SESSION,
    filePath: "/repo/src/a.ts",
    hunkKey: "k1",
    hunkHeader: "@@ -1,3 +1,3 @@",
    excerpt: "-two\n+TWO",
    body: "why uppercase?",
    state: "pending",
    marker: null,
    createdAt: "2026-10-03T00:00:00.000Z",
    submittedAt: null,
    confirmedAt: null,
    target: null,
    ...partial,
  };
}

function reviewOf(readKeys: string[], notes: ReviewNoteRow[] = []): SessionReview {
  return {
    sessionId: SESSION,
    readHunks: readKeys.map((hunkKey) => ({ hunkKey, filePath: "/repo/src/a.ts", readAt: "" })),
    notes,
  };
}

const okChanges = (files: SessionFileChange[]): FileChangesRead => ({
  status: "ok",
  response: response(files),
});
const okReview = (r: SessionReview): SessionReviewRead => ({ status: "ok", review: r });

/** The key of the one hunk `change()` produces. */
function onlyKey(c: SessionFileChange = change({})): string {
  return hunkKeysForFile(c.filePath, diffHunks(c) ?? [])[0];
}

describe("reviewBadgeState — three distinct renders", () => {
  it("is `none` when there are no hunks to review", () => {
    expect(reviewBadgeState(okChanges([]), okReview(reviewOf([])))).toEqual({ kind: "none" });
    expect(
      reviewBadgeState(
        okChanges([change({ status: "unchanged", after: "one\ntwo\nthree\n" })]),
        okReview(reviewOf([])),
      ),
    ).toEqual({ kind: "none" });
  });

  it("counts unread hunks — including `0 unread` once all are read", () => {
    const state = reviewBadgeState(okChanges([change({})]), okReview(reviewOf([])));
    expect(state).toMatchObject({
      kind: "count",
      unread: 1,
      total: 1,
      text: "1 unread",
      stale: false,
    });
    const read = reviewBadgeState(okChanges([change({})]), okReview(reviewOf([onlyKey()])));
    expect(read).toMatchObject({ kind: "count", unread: 0, text: "0 unread" });
  });

  it("renders a capped change list as a lower bound, never a bare `0 unread`", () => {
    const capped = (files: SessionFileChange[], omittedFiles: number): FileChangesRead => ({
      status: "ok",
      response: { ...response(files), filesTruncated: true, omittedFiles },
    });
    const read = reviewBadgeState(capped([change({})], 37), okReview(reviewOf([onlyKey()])));
    expect(read).toMatchObject({ kind: "count", unread: 0, lowerBound: true, text: "≥ 0 unread" });
    if (read.kind === "count") {
      expect(read.title).toContain("37 more changed files were not read");
    }
    const unread = reviewBadgeState(capped([change({})], 1), okReview(reviewOf([])));
    expect(unread).toMatchObject({ kind: "count", text: "≥ 1 unread" });
    if (unread.kind === "count") expect(unread.title).toContain("1 more changed file was not read");

    // Nothing countable in what WAS read: unknown, never `none`.
    const empty = reviewBadgeState(capped([], 400), okReview(reviewOf([])));
    expect(empty.kind).toBe("unknown");
    if (empty.kind === "unknown") expect(empty.title).toContain("400 more changed files");

    // An uncapped list keeps the exact count.
    expect(reviewBadgeState(okChanges([change({})]), okReview(reviewOf([])))).toMatchObject({
      lowerBound: false,
      text: "1 unread",
    });
  });

  it("is `?` when the change list was never read", () => {
    const failed: FileChangesRead = { status: "error", error: "HTTP 500", atMs: 0, previous: null };
    const state = reviewBadgeState(failed, okReview(reviewOf([])));
    expect(state.kind).toBe("unknown");
    expect(state.kind === "unknown" && state.title).toContain("HTTP 500");
    expect(
      reviewBadgeState({ status: "loading", previous: null }, okReview(reviewOf([]))).kind,
    ).toBe("unknown");
  });

  it("is `?` — never a count — when hunks exist but the read state could not be read", () => {
    const review: SessionReviewRead = {
      status: "error",
      error: "store down",
      atMs: 0,
      previous: null,
    };
    const state = reviewBadgeState(okChanges([change({})]), review);
    expect(state.kind).toBe("unknown");
    expect(state.kind === "unknown" && state.title).toContain("store down");
  });

  it("is `?` when every changed file failed to diff, not `none`", () => {
    const state = reviewBadgeState(
      okChanges([change({ status: "unreadable", detail: "blob missing" })]),
      okReview(reviewOf([])),
    );
    expect(state.kind).toBe("unknown");
  });

  it("keeps the last good count up, marked stale, when a refresh fails", () => {
    const changes: FileChangesRead = {
      status: "error",
      error: "boom",
      atMs: 0,
      previous: response([change({})]),
    };
    const state = reviewBadgeState(changes, okReview(reviewOf([])));
    expect(state).toMatchObject({ kind: "count", unread: 1, stale: true });
  });

  it("names undiffable files in the tooltip without counting them", () => {
    const state = reviewBadgeState(
      okChanges([change({}), change({ filePath: "/repo/b.bin", status: "binary" })]),
      okReview(reviewOf([])),
    );
    expect(state).toMatchObject({ kind: "count", unread: 1, unknownFiles: 1 });
    expect(state.kind === "count" && state.title).toContain("could not be diffed");
  });

  it("renders the three states distinctly", () => {
    expect(renderToStaticMarkup(<SessionReviewBadge state={{ kind: "none" }} />)).toBe("");
    const unknown = renderToStaticMarkup(
      <SessionReviewBadge state={{ kind: "unknown", title: "UNKNOWN — x" }} />,
    );
    expect(unknown).toContain('data-review-badge="unknown"');
    expect(unknown).toContain(">?</button>");
    expect(unknown).toContain('data-ui-bridge-id="terminal.review-badge"');
    const count = renderToStaticMarkup(
      <SessionReviewBadge
        state={reviewBadgeState(okChanges([change({})]), okReview(reviewOf([])))}
      />,
    );
    expect(count).toContain('data-review-badge="count"');
    expect(count).toContain("1 unread");
  });

  it("puts the badge next to the review toggle", () => {
    const html = renderToStaticMarkup(
      <SessionReviewToggle
        open={false}
        onToggle={() => {}}
        badge={{ kind: "unknown", title: "t" }}
      />,
    );
    expect(html).toContain('data-ui-bridge-id="terminal.review-toggle"');
    expect(html.indexOf("terminal.review-toggle")).toBeLessThan(
      html.indexOf("terminal.review-badge"),
    );
  });
});

describe("note selectors", () => {
  const notes = [
    note({ id: "p", state: "pending" }),
    note({ id: "a", state: "attached" }),
    note({ id: "d", state: "discarded" }),
    note({ id: "s1", state: "submitted", submittedAt: "2026-10-03T01:00:00Z" }),
    note({ id: "s2", state: "confirmed", submittedAt: "2026-10-03T02:00:00Z" }),
    note({ id: "u", state: "unknown", submittedAt: "2026-10-03T00:30:00Z" }),
  ];
  const r = reviewOf(["k9"], notes);

  it("hides discarded notes everywhere", () => {
    expect(visibleNotes(r).map((n) => n.id)).not.toContain("d");
  });

  it("splits drafts, chips and the sent history", () => {
    expect(pendingNotes(r).map((n) => n.id)).toEqual(["p"]);
    expect(attachedNotes(r).map((n) => n.id)).toEqual(["a"]);
    expect(sentNotes(r).map((n) => n.id)).toEqual(["s2", "s1", "u"]);
  });

  it("an unread review is empty, never a fabricated read set", () => {
    expect(readKeySet(null).size).toBe(0);
    expect(visibleNotes(null)).toEqual([]);
    expect([...readKeySet(r)]).toEqual(["k9"]);
  });

  it("calls a note orphaned only against a diff that was actually read", () => {
    const n = note({ hunkKey: "gone" });
    expect(isOrphaned(n, null)).toBe(false);
    expect(isOrphaned(n, new Set(["k1"]))).toBe(true);
    expect(isOrphaned(note({}), new Set(["k1"]))).toBe(false);
    expect(liveKeysOf(null)).toBeNull();
    expect(liveKeysOf(response([change({})]))?.has(onlyKey())).toBe(true);
  });
});

describe("mark-read decisions", () => {
  it("a hunk is scrolled past only when its bottom went above the root's top", () => {
    expect(scrolledPast({ isIntersecting: false, bottom: 90, rootTop: 100 })).toBe(true);
    expect(scrolledPast({ isIntersecting: false, bottom: 100, rootTop: 100 })).toBe(true);
    // Below the fold, never reached.
    expect(scrolledPast({ isIntersecting: false, bottom: 400, rootTop: 100 })).toBe(false);
    // Still in view.
    expect(scrolledPast({ isIntersecting: true, bottom: 90, rootTop: 100 })).toBe(false);
    // No root bounds reported: no claim.
    expect(scrolledPast({ isIntersecting: false, bottom: 0, rootTop: null })).toBe(false);
  });

  it("never re-marks a hunk the operator explicitly marked unread", () => {
    const ctx = {
      readKeys: new Set<string>(),
      manuallyUnread: new Set(["k"]),
      inFlight: new Set<string>(),
    };
    expect(shouldAutoMarkRead("k", ctx)).toBe(false);
    expect(shouldAutoMarkRead("other", ctx)).toBe(true);
  });

  it("skips hunks already read or already being marked", () => {
    const empty = new Set<string>();
    expect(
      shouldAutoMarkRead("k", { readKeys: new Set(["k"]), manuallyUnread: empty, inFlight: empty }),
    ).toBe(false);
    expect(
      shouldAutoMarkRead("k", { readKeys: empty, manuallyUnread: empty, inFlight: new Set(["k"]) }),
    ).toBe(false);
  });
});

describe("composeForSend", () => {
  const attached = [note({ id: "a", state: "attached", hunkKey: "k1" })];

  it("is null with no attached notes — the route requires one", () => {
    expect(
      composeForSend({
        sessionId: SESSION,
        attached: [],
        freeText: "hi",
        salt: "s",
        liveKeys: null,
      }),
    ).toBeNull();
  });

  it("is deterministic in its inputs, so the preview IS the sent text", () => {
    const a = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "x",
      salt: "s",
      liveKeys: new Set(["k1"]),
    });
    const b = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "x",
      salt: "s",
      liveKeys: new Set(["k1"]),
    });
    expect(a?.text).toBe(b?.text);
    expect(parseMarker(a?.text ?? "")).toBe(a?.marker);
    expect(a?.text.split("\n")[0]).toBe(`[review ${a?.marker}]`);
    expect(a?.text).toContain("why uppercase?");
    expect(a?.text).toContain("x");
  });

  it("a new salt gives a new marker", () => {
    const a = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "",
      salt: "s1",
      liveKeys: null,
    });
    const b = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "",
      salt: "s2",
      liveKeys: null,
    });
    expect(a?.marker).not.toBe(b?.marker);
  });

  it("labels an orphaned note when the diff no longer has its hunk", () => {
    const out = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "",
      salt: "s",
      liveKeys: new Set(["other"]),
    });
    expect(out?.orphanedNoteIds).toEqual(["a"]);
    expect(out?.text).toContain("orphaned");
  });

  it("labels nothing orphaned when no diff has been read", () => {
    const out = composeForSend({
      sessionId: SESSION,
      attached,
      freeText: "",
      salt: "s",
      liveKeys: null,
    });
    expect(out?.orphanedNoteIds).toEqual([]);
  });
});

describe("sendBarReducer", () => {
  const start = initialSendBar("salt-1");

  it("refuses a second delivery while one is in flight", () => {
    const busy = sendBarReducer(start, { type: "start", mode: "send" });
    expect(busy.busy).toBe("send");
    expect(sendBarReducer(busy, { type: "start", mode: "insert" })).toBe(busy);
  });

  it("a send consumes the free text and rotates the salt", () => {
    const typed = sendBarReducer(start, { type: "setText", text: "also fix tests" });
    const busy = sendBarReducer(typed, { type: "start", mode: "send" });
    const done = sendBarReducer(busy, {
      type: "succeeded",
      mode: "send",
      marker: "abcd1234",
      sanitized: false,
      atMs: 0,
      nextSalt: "salt-2",
    });
    expect(done).toMatchObject({ freeText: "", salt: "salt-2", busy: null });
    expect(done.result).toMatchObject({ kind: "sent", marker: "abcd1234" });
  });

  it("an insert keeps the free text — the notes stay attached and the text is the operator's", () => {
    const typed = sendBarReducer(start, { type: "setText", text: "keep me" });
    const done = sendBarReducer(sendBarReducer(typed, { type: "start", mode: "insert" }), {
      type: "succeeded",
      mode: "insert",
      marker: "abcd1234",
      sanitized: null,
      atMs: 0,
      nextSalt: "salt-2",
    });
    expect(done.freeText).toBe("keep me");
    expect(done.salt).toBe("salt-2");
    expect(done.result?.kind).toBe("inserted");
  });

  const failure = (code: string | null, message: string, noteIds: string[] = []) => ({
    type: "failed" as const,
    mode: "send" as const,
    message,
    atMs: 0,
    code,
    marker: "abcd1234",
    noteIds,
    nextSalt: "salt-2",
  });

  it("a failure keeps everything and names the error", () => {
    const typed = sendBarReducer(start, { type: "setText", text: "t" });
    const failed = sendBarReducer(
      sendBarReducer(typed, { type: "start", mode: "send" }),
      failure("target_not_found", "HTTP 404 target_not_found"),
    );
    expect(failed).toMatchObject({ freeText: "t", salt: "salt-1", busy: null });
    expect(failed.result).toMatchObject({ kind: "error", partial: false });
    expect(sendResultLabel(failed.result!)).toContain("target_not_found");
    expect(sendResultLabel(failed.result!)).toContain("the notes stay attached");
  });

  it("renders delivered-but-unrecorded as DELIVERED with a do-not-resend warning", () => {
    expect(sendFailureKind("delivered_unrecorded")).toBe("deliveredUnrecorded");
    const typed = sendBarReducer(start, { type: "setText", text: "t" });
    const done = sendBarReducer(
      sendBarReducer(typed, { type: "start", mode: "send" }),
      failure("delivered_unrecorded", "HTTP 500 delivered_unrecorded: …", ["n1", "n2"]),
    );
    // The text went out: free text consumed and the marker spent, as on a send.
    expect(done).toMatchObject({ freeText: "", salt: "salt-2", busy: null });
    expect(done.result).toMatchObject({ kind: "deliveredUnrecorded", noteIds: ["n1", "n2"] });
    const label = sendResultLabel(done.result!);
    expect(label).toContain("DELIVERED");
    expect(label).toContain("Do NOT send them again");
    expect(label).not.toContain("failed");
    expect(label).not.toContain("stay attached");
  });

  it("says a partial write may have left the text in the input box", () => {
    expect(sendFailureKind("delivery_partial")).toBe("partial");
    expect(sendFailureKind("delivery_failed")).toBe("failed");
    expect(sendFailureKind(null)).toBe("failed");
    const typed = sendBarReducer(start, { type: "setText", text: "t" });
    const broke = sendBarReducer(
      sendBarReducer(typed, { type: "start", mode: "send" }),
      failure("delivery_partial", "HTTP 502 delivery_partial: the submit enter failed"),
    );
    expect(broke).toMatchObject({ freeText: "t", salt: "salt-1" });
    expect(broke.result).toMatchObject({ kind: "error", partial: true });
    expect(sendResultLabel(broke.result!)).toContain("may already be in the session's input box");
  });

  it("says delivery is the operator's after an insert, and awaits the transcript after a send", () => {
    expect(sendResultLabel({ kind: "inserted", marker: "m", sanitized: null, atMs: 0 })).toContain(
      "sending it is yours",
    );
    expect(sendResultLabel({ kind: "sent", marker: "m", sanitized: true, atMs: 0 })).toContain(
      "neutralized",
    );
  });

  it("offers Insert only for a PTY target", () => {
    expect(canInsert({ terminalId: "t" })).toBe(true);
    expect(canInsert({ taskRunId: "r" })).toBe(false);
  });
});

describe("markup contracts", () => {
  const binding = (overrides: Partial<HunkReview> = {}): HunkReview => ({
    readKeys: new Set<string>(),
    notes: [],
    onToggleRead: () => {},
    onScrolledPast: () => {},
    onAddNote: () => {},
    draftKey: null,
    draftSlot: null,
    ...overrides,
  });

  it("stamps each hunk, its read toggle and its add-note button", () => {
    const c = change({});
    const key = onlyKey(c);
    const html = renderToStaticMarkup(
      <DiffView hunks={diffHunks(c) ?? []} filePath={c.filePath} review={binding()} />,
    );
    expect(html).toContain(`data-ui-bridge-id="terminal.review-hunk.${key.slice(0, 12)}"`);
    expect(html).toContain('data-hunk-read="unread"');
    expect(html).toContain(`terminal.review-hunk-read.${key.slice(0, 12)}`);
    expect(html).toContain(`terminal.review-add-note.${key.slice(0, 12)}`);
  });

  it("renders an UNKNOWN read state as `?`, not as unread", () => {
    const c = change({});
    const html = renderToStaticMarkup(
      <DiffView
        hunks={diffHunks(c) ?? []}
        filePath={c.filePath}
        review={binding({ readKeys: null })}
      />,
    );
    expect(html).toContain('data-hunk-read="unknown"');
    expect(html).not.toContain('data-hunk-read="unread"');
  });

  it("renders the plain diff with no review props exactly as a plain diff", () => {
    const c = change({});
    const html = renderToStaticMarkup(<DiffView hunks={diffHunks(c) ?? []} />);
    expect(html).not.toContain("terminal.review-");
    expect(html).toContain("+TWO");
  });

  it("shows the base the diff was taken against", () => {
    const html = renderToStaticMarkup(
      <FileChangesPanel read={okChanges([change({})])} onRefresh={() => {}} />,
    );
    expect(html).toContain("vs HEAD 012345678 — committed work not shown");
  });

  const handle = (r: SessionReview | null, files: SessionFileChange[]): SessionReviewHandle => ({
    sessionId: SESSION,
    changes: okChanges(files),
    review: r ? okReview(r) : { status: "error", error: "down", atMs: 0, previous: null },
    refresh: () => {},
    refreshChanges: () => {},
    markRead: async () => {},
    addNote: async () => note({}),
    patchNote: async () => note({}),
    deliver: async () => {
      throw new Error("not in a static render");
    },
  });

  it("the send bar shows chips, Send and Insert for a PTY target", () => {
    const key = onlyKey();
    const r = reviewOf([], [note({ id: "a1", state: "attached", hunkKey: key })]);
    const html = renderToStaticMarkup(
      <ReviewSendBar handle={handle(r, [change({})])} target={{ terminalId: "term-1" }} />,
    );
    expect(html).toContain('data-ui-bridge-id="terminal.review-chip.a1"');
    expect(html).toContain('data-ui-bridge-id="terminal.review-chip-detach.a1"');
    expect(html).toContain('data-ui-bridge-id="terminal.review-send"');
    expect(html).toContain('data-ui-bridge-id="terminal.review-insert"');
    expect(html).toContain("delivery is yours");
    expect(html).not.toContain('data-note-orphaned="true"');
  });

  it("labels an orphaned chip and offers no Insert to a worker", () => {
    const r = reviewOf([], [note({ id: "a1", state: "attached", hunkKey: "gone" })]);
    const html = renderToStaticMarkup(
      <ReviewSendBar handle={handle(r, [change({})])} target={{ taskRunId: "run-1" }} />,
    );
    expect(html).toContain('data-note-orphaned="true"');
    expect(html).toContain("orphaned");
    expect(html).not.toContain("terminal.review-insert");
  });

  it("says UNKNOWN, not 'no notes', when the review store could not be read", () => {
    const html = renderToStaticMarkup(
      <ReviewSendBar handle={handle(null, [change({})])} target={{ terminalId: "term-1" }} />,
    );
    expect(html).toContain("UNKNOWN");
    expect(html).not.toContain("No notes attached");
  });
});
