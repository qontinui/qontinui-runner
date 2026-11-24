/**
 * What the Terminal page's review surface SHOWS, as pure functions — plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 4. The model itself (hunk keys, counts, the note lifecycle, the
 * composed prompt) is `sessionReview.ts`; this module turns the two reads the
 * page holds — the file-changes list and the review store — into the badge,
 * the chips, the send bar and the mark-read decisions.
 *
 * The vitest environment is `node` with no React Testing Library (see
 * `hiddenWorkerReducer.ts`), which is why none of this lives inside a hook.
 *
 * Honesty (served `ux-priorities`): a read that never succeeded is UNKNOWN —
 * the badge renders `?`, never `0 unread` and never nothing.
 */

import {
  composeReviewPrompt,
  liveHunkKeys,
  markerFor,
  reviewCounts,
  type ComposedReviewPrompt,
  type ReviewFile,
} from "./sessionReview";
import type { ReviewNoteRow, ReviewTarget, SessionReview } from "./sessionReviewApi";
import {
  diffHunks,
  type FileChangesRead,
  type SessionFileChange,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

// ---------------------------------------------------------------------------
// The review read
// ---------------------------------------------------------------------------

/**
 * The page's view of the last `GET /sessions/{id}/review`. Same three arms as
 * `FileChangesRead`: `error` keeps the last good read as `previous` (shown, and
 * labelled stale), and a read that never succeeded has nothing to show.
 */
export type SessionReviewRead =
  | { status: "loading"; previous: SessionReview | null }
  | { status: "ok"; review: SessionReview }
  | { status: "error"; error: string; atMs: number; previous: SessionReview | null };

/** The review to render: the current read, else the last good one, else `null` (UNKNOWN). */
export function shownReview(read: SessionReviewRead): SessionReview | null {
  return read.status === "ok" ? read.review : read.previous;
}

/** The change list to render, by the same rule. */
export function shownChanges(read: FileChangesRead): SessionFileChangesResponse | null {
  return read.status === "ok" ? read.response : read.previous;
}

/** Hunk keys the operator has marked read. */
export function readKeySet(review: SessionReview | null): Set<string> {
  return new Set(review ? review.readHunks.map((h) => h.hunkKey) : []);
}

/** The change list as review files (`hunks: null` = no diff could be produced). */
export function reviewFiles(files: readonly SessionFileChange[]): ReviewFile[] {
  return files.map((change) => ({
    filePath: change.filePath,
    hunks: diffHunks(change),
    status: change.status,
  }));
}

/** Every note the operator can still see (discarded ones are gone from the UI). */
export function visibleNotes(review: SessionReview | null): ReviewNoteRow[] {
  return review ? review.notes.filter((n) => n.state !== "discarded") : [];
}

/** Notes riding the next send — the send bar's chips. */
export function attachedNotes(review: SessionReview | null): ReviewNoteRow[] {
  return visibleNotes(review).filter((n) => n.state === "attached");
}

/** Notes written but not attached. */
export function pendingNotes(review: SessionReview | null): ReviewNoteRow[] {
  return visibleNotes(review).filter((n) => n.state === "pending");
}

/** Notes that went out — `submitted`, `confirmed` or `unknown`, newest first. */
export function sentNotes(review: SessionReview | null): ReviewNoteRow[] {
  return visibleNotes(review)
    .filter((n) => n.state === "submitted" || n.state === "confirmed" || n.state === "unknown")
    .sort((a, b) => (b.submittedAt ?? "").localeCompare(a.submittedAt ?? ""));
}

/** Notes on one hunk, for the marker beside its header. */
export function notesForHunk(notes: readonly ReviewNoteRow[], hunkKey: string): ReviewNoteRow[] {
  return notes.filter((n) => n.hunkKey === hunkKey);
}

/**
 * Whether a note's hunk is gone from the current diff. Only answerable when a
 * change list has been read — with none, nothing is called orphaned.
 */
export function isOrphaned(note: ReviewNoteRow, liveKeys: ReadonlySet<string> | null): boolean {
  return liveKeys !== null && !liveKeys.has(note.hunkKey);
}

/** What each sent state says in the panel. */
export function noteStateLabel(note: ReviewNoteRow): string {
  switch (note.state) {
    case "pending":
      return "draft";
    case "attached":
      return "attached to the next send";
    case "submitted":
      return "sent — not yet seen in the transcript";
    case "confirmed":
      return "confirmed in the transcript";
    case "unknown":
      return "UNKNOWN — the session ended (or the runner restarted) before it was seen arriving";
    case "discarded":
      return "discarded";
  }
}

// ---------------------------------------------------------------------------
// The tab badge
// ---------------------------------------------------------------------------

/**
 * The badge beside a tab's title. Three renders, never conflated:
 *
 * - `none` — there are no hunks to review (nothing rendered).
 * - `unknown` — the change list or the read state could not be established
 *   (`?`, with the reason in the tooltip).
 * - `count` — `n unread`, which can be `0 unread`: hunks exist and all are read.
 *   When the route capped the change list (`filesTruncated`), the files it did
 *   not read may hold more unread hunks, so the count is a LOWER BOUND and
 *   renders `≥ n unread`; and a capped list with no countable hunk is `unknown`,
 *   never `none` — nothing was established about the files it left out.
 */
export type ReviewBadgeState =
  | { kind: "none" }
  | { kind: "unknown"; title: string }
  | {
      kind: "count";
      unread: number;
      total: number;
      /** Files whose diff could not be produced — the count does not include them. */
      unknownFiles: number;
      /** The change list was capped: `unread` counts only the files it read. */
      lowerBound: boolean;
      text: string;
      title: string;
      stale: boolean;
    };

export function reviewBadgeState(
  changes: FileChangesRead,
  review: SessionReviewRead,
): ReviewBadgeState {
  const changeList = shownChanges(changes);
  if (!changeList) {
    return {
      kind: "unknown",
      title:
        changes.status === "error"
          ? `UNKNOWN — the change list could not be read: ${changes.error}`
          : "UNKNOWN — the change list has not been read yet",
    };
  }
  const files = reviewFiles(changeList.files);
  const shownRead = shownReview(review);
  const counts = reviewCounts(files, readKeySet(shownRead));
  const omitted = changeList.filesTruncated ? Math.max(changeList.omittedFiles, 0) : 0;
  const lowerBound = changeList.filesTruncated;
  const omittedText =
    omitted > 0
      ? `${omitted} more changed file${omitted === 1 ? " was" : "s were"} not read (the change list is capped)`
      : "more changed files were not read (the change list is capped)";
  if (counts.total === 0 && counts.unknown === 0 && !lowerBound) return { kind: "none" };
  if (counts.total === 0) {
    const why: string[] = [];
    if (counts.unknown > 0) {
      why.push(
        `${counts.unknown} changed file${counts.unknown === 1 ? "" : "s"} could not be diffed`,
      );
    }
    if (lowerBound) why.push(omittedText);
    return { kind: "unknown", title: `UNKNOWN — ${why.join("; ")}` };
  }
  if (!shownRead) {
    return {
      kind: "unknown",
      title:
        review.status === "error"
          ? `UNKNOWN — which hunks are read could not be read: ${review.error}`
          : "UNKNOWN — the read state has not been read yet",
    };
  }
  const stale = changes.status !== "ok" || review.status !== "ok";
  const parts = [`${counts.unread} of ${counts.total} hunk${counts.total === 1 ? "" : "s"} unread`];
  if (counts.unknown > 0) {
    parts.push(
      `${counts.unknown} more file${counts.unknown === 1 ? "" : "s"} could not be diffed and ${
        counts.unknown === 1 ? "is" : "are"
      } not counted`,
    );
  }
  if (lowerBound) parts.push(`${omittedText} — at least this many are unread`);
  if (stale) parts.push("from the last successful read — a refresh is pending or failed");
  return {
    kind: "count",
    unread: counts.unread,
    total: counts.total,
    unknownFiles: counts.unknown,
    lowerBound,
    text: `${lowerBound ? "≥ " : ""}${counts.unread} unread`,
    title: parts.join("; "),
    stale,
  };
}

// ---------------------------------------------------------------------------
// Mark-read decisions
// ---------------------------------------------------------------------------

/**
 * Whether a hunk has been scrolled PAST — its bottom edge has gone above the
 * top of the scroll container it lives in. A hunk merely scrolled out of view
 * downward (never reached) is not read. Inputs are the `IntersectionObserver`
 * entry's numbers, so this is testable without a DOM.
 */
export function scrolledPast(entry: {
  isIntersecting: boolean;
  bottom: number;
  rootTop: number | null;
}): boolean {
  if (entry.isIntersecting || entry.rootTop === null) return false;
  return entry.bottom <= entry.rootTop;
}

/**
 * Whether scrolling past a hunk marks it read. Never for a hunk already read
 * or already being marked, and never for one the operator explicitly marked
 * UNREAD in this view — reversing an explicit choice by a scroll would make
 * the toggle a lie.
 */
export function shouldAutoMarkRead(
  key: string,
  ctx: {
    readKeys: ReadonlySet<string>;
    manuallyUnread: ReadonlySet<string>;
    inFlight: ReadonlySet<string>;
  },
): boolean {
  return !ctx.readKeys.has(key) && !ctx.manuallyUnread.has(key) && !ctx.inFlight.has(key);
}

// ---------------------------------------------------------------------------
// The send bar
// ---------------------------------------------------------------------------

export type SendMode = "send" | "insert";

export type SendBarResult =
  | { kind: "sent"; marker: string; sanitized: boolean | null; atMs: number }
  | { kind: "inserted"; marker: string; sanitized: boolean | null; atMs: number }
  /**
   * The text WENT OUT but the runner could not record some notes as sent
   * (`delivered_unrecorded`). Not a failure to retry: a second send would
   * deliver the same notes twice.
   */
  | {
      kind: "deliveredUnrecorded";
      marker: string;
      noteIds: string[];
      message: string;
      atMs: number;
    }
  /**
   * Nothing was recorded sent. `partial`: the write broke after some of the
   * text reached the PTY (`delivery_partial`) — it may be sitting in the
   * session's input box, unsent.
   */
  | { kind: "error"; mode: SendMode; message: string; partial: boolean; atMs: number };

/** How a failed delivery reads, from the route's typed code. */
export type SendFailureKind = "deliveredUnrecorded" | "partial" | "failed";

/** The route's code → how the bar must present it. */
export function sendFailureKind(code: string | null | undefined): SendFailureKind {
  if (code === "delivered_unrecorded") return "deliveredUnrecorded";
  if (code === "delivery_partial") return "partial";
  return "failed";
}

export interface SendBarState {
  freeText: string;
  previewOpen: boolean;
  /**
   * What keeps two sends of the same notes and text apart in `markerFor`. Held
   * steady until a send or insert succeeds, so the preview shows the EXACT text
   * that goes out — then rotated, so the next send gets a fresh marker.
   */
  salt: string;
  busy: SendMode | null;
  result: SendBarResult | null;
}

export type SendBarAction =
  | { type: "setText"; text: string }
  | { type: "togglePreview" }
  | { type: "start"; mode: SendMode }
  | {
      type: "succeeded";
      mode: SendMode;
      marker: string;
      sanitized: boolean | null;
      atMs: number;
      nextSalt: string;
    }
  | {
      type: "failed";
      mode: SendMode;
      message: string;
      atMs: number;
      /** The route's typed code, when the refusal carried one. */
      code: string | null;
      /** The marker the attempted text carried. */
      marker: string;
      /** Notes the refusal named (for `delivered_unrecorded`, the unrecorded ones). */
      noteIds: string[];
      nextSalt: string;
    };

export function initialSendBar(salt: string): SendBarState {
  return { freeText: "", previewOpen: false, salt, busy: null, result: null };
}

export function sendBarReducer(state: SendBarState, action: SendBarAction): SendBarState {
  switch (action.type) {
    case "setText":
      return { ...state, freeText: action.text };
    case "togglePreview":
      return { ...state, previewOpen: !state.previewOpen };
    case "start":
      // One delivery at a time: a second click while one is in flight is a no-op.
      if (state.busy) return state;
      return { ...state, busy: action.mode, result: null };
    case "succeeded":
      return {
        ...state,
        busy: null,
        salt: action.nextSalt,
        // A send consumed the free text; an insert put it in the operator's
        // input box, where it is theirs — the bar keeps it, as it keeps the notes.
        freeText: action.mode === "send" ? "" : state.freeText,
        previewOpen: action.mode === "send" ? false : state.previewOpen,
        result: {
          kind: action.mode === "send" ? "sent" : "inserted",
          marker: action.marker,
          sanitized: action.sanitized,
          atMs: action.atMs,
        },
      };
    case "failed": {
      const kind = sendFailureKind(action.code);
      if (kind === "deliveredUnrecorded") {
        // The text went out: the marker is spent and the free text consumed,
        // exactly as on a clean send.
        return {
          ...state,
          busy: null,
          salt: action.nextSalt,
          freeText: "",
          previewOpen: false,
          result: {
            kind: "deliveredUnrecorded",
            marker: action.marker,
            noteIds: action.noteIds,
            message: action.message,
            atMs: action.atMs,
          },
        };
      }
      return {
        ...state,
        busy: null,
        result: {
          kind: "error",
          mode: action.mode,
          message: action.message,
          partial: kind === "partial",
          atMs: action.atMs,
        },
      };
    }
  }
}

/**
 * The exact text a send would deliver, or `null` when there is nothing to
 * send — the route requires at least one attached note.
 *
 * `liveKeys` is `null` while no change list has been read; every note is then
 * composed with its stored excerpt and none is labelled orphaned, since
 * "orphaned" is a claim about a diff nobody has seen.
 */
export function composeForSend(input: {
  sessionId: string;
  attached: readonly ReviewNoteRow[];
  freeText: string;
  salt: string;
  liveKeys: ReadonlySet<string> | null;
}): ComposedReviewPrompt | null {
  if (input.attached.length === 0) return null;
  const marker = markerFor({
    sessionId: input.sessionId,
    noteIds: input.attached.map((n) => n.id),
    freeText: input.freeText,
    salt: input.salt,
  });
  const liveKeys = input.liveKeys ?? new Set(input.attached.map((n) => n.hunkKey));
  return composeReviewPrompt(input.attached, input.freeText, { marker, liveKeys });
}

/** The live hunk keys of a change list, or `null` when none has been read. */
export function liveKeysOf(changes: SessionFileChangesResponse | null): Set<string> | null {
  return changes ? liveHunkKeys(reviewFiles(changes.files)) : null;
}

/** One line for the bar's last result. */
export function sendResultLabel(result: SendBarResult): string {
  const at = new Date(result.atMs).toLocaleTimeString();
  const neutralized =
    (result.kind === "sent" || result.kind === "inserted") && result.sanitized
      ? " (control characters in the text were neutralized)"
      : "";
  switch (result.kind) {
    case "sent":
      return `sent [review ${result.marker}] at ${at} — waiting to see it arrive in the transcript${neutralized}`;
    case "inserted":
      return `typed [review ${result.marker}] at ${at} without Enter — sending it is yours; the notes stay attached${neutralized}`;
    case "deliveredUnrecorded":
      return `sent [review ${result.marker}] at ${at} — DELIVERED, but ${result.noteIds.length} note${
        result.noteIds.length === 1 ? "" : "s"
      } could not be recorded as sent. Do NOT send ${
        result.noteIds.length === 1 ? "it" : "them"
      } again: the session already has the text. (${result.message})`;
    case "error":
      return result.partial
        ? `${result.mode === "send" ? "send" : "insert"} broke part-way at ${at}: ${result.message} — the text may already be in the session's input box, unsent; check it before sending again`
        : `${result.mode === "send" ? "send" : "insert"} failed at ${at}: ${result.message} — the notes stay attached`;
  }
}

/**
 * Whether the bar offers Insert. Only a PTY tab has an input box to type a
 * draft into; the route refuses an insert into a task run.
 */
export function canInsert(target: ReviewTarget): boolean {
  return "terminalId" in target;
}

/** A fresh salt. Not security-relevant — it only keeps markers distinct. */
export function newSalt(nowMs: number = Date.now()): string {
  return `${nowMs.toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}
