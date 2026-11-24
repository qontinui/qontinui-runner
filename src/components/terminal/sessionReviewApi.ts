/**
 * HTTP client for the runner's session-review routes (`mcp/session_review.rs`,
 * plan `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 3), plus the pure parsing that decides whether a reply is a review at
 * all.
 *
 * `{sessionId}` is the same key the file-changes route takes: the tab's
 * `claudeSessionId` for a PTY tab, the task-run id for a Conductor worker.
 *
 * Honesty: every failure is a thrown {@link ReviewApiError} — a non-2xx, a
 * network error, or a body that is not the shape the route promises. Nothing
 * here ever turns a failed read into an empty note list or an empty read set;
 * the caller renders that as UNKNOWN.
 */

import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { describeThrown } from "@/lib/utils";
import type { ReviewNote, ReviewNoteState } from "./sessionReview";

/** Where a send delivers (`NoteTarget` in `mcp/session_review.rs`). */
export type ReviewTarget = { terminalId: string } | { taskRunId: string };

/** A note as the GET route returns it: `ReviewNote` plus where it was sent. */
export interface ReviewNoteRow extends ReviewNote {
  /** `null` until a send or insert names a target. */
  target: ReviewTarget | null;
}

export interface ReadHunk {
  hunkKey: string;
  filePath: string;
  readAt: string;
}

/** `GET /sessions/{id}/review`. Discarded notes are included; filter client-side. */
export interface SessionReview {
  sessionId: string;
  readHunks: ReadHunk[];
  notes: ReviewNoteRow[];
}

export interface HunkRef {
  hunkKey: string;
  filePath: string;
}

export interface NewNote {
  filePath: string;
  hunkKey: string;
  hunkHeader?: string;
  excerpt?: string;
  body: string;
}

/** The client-settable PATCH events. `submit`/`confirm`/`sessionEnded` are the server's. */
export type NotePatch =
  | { type: "attach" }
  | { type: "detach" }
  | { type: "discard" }
  | { type: "edit"; body: string };

export interface SendRequest {
  target: ReviewTarget;
  noteIds: string[];
  /** The exact composed text — its first line is the marker. */
  text: string;
}

/** `POST …/review/send` and `…/review/insert`. */
export interface SendOutcome {
  sessionId: string;
  marker: string;
  /** `true` for a send, `false` for an insert (the operator delivers it). */
  submitted: boolean;
  /** Did the PTY choke point's neutralizer change the text? `null` for a task run. */
  sanitized: boolean | null;
  bytes: number | null;
  notes: ReviewNoteRow[];
}

/**
 * A refused or failed call. `code` is the route's typed code (`illegal_transition`,
 * `target_not_found`, `delivery_failed`, …), or `network` / `malformed` for a
 * failure that never produced one.
 */
export class ReviewApiError extends Error {
  readonly status: number | null;
  readonly code: string;
  readonly noteIds: string[];

  constructor(message: string, opts: { status: number | null; code: string; noteIds?: string[] }) {
    super(message);
    this.name = "ReviewApiError";
    this.status = opts.status;
    this.code = opts.code;
    this.noteIds = opts.noteIds ?? [];
  }
}

// ---------------------------------------------------------------------------
// Pure parsing
// ---------------------------------------------------------------------------

const NOTE_STATES: readonly ReviewNoteState[] = [
  "pending",
  "attached",
  "submitted",
  "confirmed",
  "discarded",
  "unknown",
];

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function str(v: unknown): string | null {
  return typeof v === "string" ? v : null;
}

function malformed(what: string): never {
  throw new ReviewApiError(`malformed review payload: ${what}`, {
    status: null,
    code: "malformed",
  });
}

export function parseTarget(raw: unknown): ReviewTarget | null {
  if (raw === null || raw === undefined) return null;
  if (isRecord(raw)) {
    const terminalId = str(raw.terminalId);
    if (terminalId !== null) return { terminalId };
    const taskRunId = str(raw.taskRunId);
    if (taskRunId !== null) return { taskRunId };
  }
  return malformed("a note's `target` is neither {terminalId} nor {taskRunId}");
}

export function parseNoteRow(raw: unknown): ReviewNoteRow {
  if (!isRecord(raw)) return malformed("a note is not an object");
  const id = str(raw.id);
  const state = str(raw.state) as ReviewNoteState | null;
  if (!id) return malformed("a note has no `id`");
  if (!state || !NOTE_STATES.includes(state)) {
    return malformed(`note ${id} has state ${JSON.stringify(raw.state)}`);
  }
  return {
    id,
    sessionId: str(raw.sessionId) ?? "",
    filePath: str(raw.filePath) ?? "",
    hunkKey: str(raw.hunkKey) ?? "",
    hunkHeader: str(raw.hunkHeader) ?? "",
    excerpt: str(raw.excerpt) ?? "",
    body: str(raw.body) ?? "",
    state,
    marker: str(raw.marker),
    createdAt: str(raw.createdAt) ?? "",
    submittedAt: str(raw.submittedAt),
    confirmedAt: str(raw.confirmedAt),
    target: parseTarget(raw.target),
  };
}

/** Validate a `GET /sessions/{id}/review` body. Throws on anything that is not one. */
export function parseSessionReview(raw: unknown, sessionId: string): SessionReview {
  if (!isRecord(raw)) return malformed("the body is not an object");
  if (!Array.isArray(raw.readHunks)) return malformed("no `readHunks` array");
  if (!Array.isArray(raw.notes)) return malformed("no `notes` array");
  const readHunks = raw.readHunks.map((h): ReadHunk => {
    if (!isRecord(h) || !str(h.hunkKey)) return malformed("a read hunk has no `hunkKey`");
    return {
      hunkKey: h.hunkKey as string,
      filePath: str(h.filePath) ?? "",
      readAt: str(h.readAt) ?? "",
    };
  });
  return {
    sessionId: str(raw.sessionId) ?? sessionId,
    readHunks,
    notes: raw.notes.map(parseNoteRow),
  };
}

export function parseSendOutcome(raw: unknown): SendOutcome {
  if (!isRecord(raw)) return malformed("the send reply is not an object");
  const marker = str(raw.marker);
  if (!marker) return malformed("the send reply carries no `marker`");
  if (!Array.isArray(raw.notes)) return malformed("the send reply has no `notes` array");
  return {
    sessionId: str(raw.sessionId) ?? "",
    marker,
    submitted: raw.submitted === true,
    sanitized: typeof raw.sanitized === "boolean" ? raw.sanitized : null,
    bytes: typeof raw.bytes === "number" ? raw.bytes : null,
    notes: raw.notes.map(parseNoteRow),
  };
}

/** The route's error body `{error, code, noteIds?}`; every field `null`/empty when absent. */
function parseErrorBody(text: string): {
  error: string | null;
  code: string | null;
  noteIds: string[];
} {
  let parsed: unknown = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    // Not JSON — the caller keeps the raw text as the message.
  }
  const body: Record<string, unknown> = isRecord(parsed) ? parsed : {};
  const ids = body.noteIds;
  return {
    error: str(body.error),
    code: str(body.code),
    noteIds: Array.isArray(ids) ? ids.filter((n): n is string => typeof n === "string") : [],
  };
}

/** The typed error for a non-2xx reply, from the route's `{error, code, noteIds?}` body. */
export function errorFromReply(status: number, bodyText: string): ReviewApiError {
  const body = parseErrorBody(bodyText);
  const message = body.error ?? (bodyText.slice(0, 300) || `HTTP ${status}`);
  const code = body.code ?? `http_${status}`;
  return new ReviewApiError(`HTTP ${status} ${code}: ${message}`, {
    status,
    code,
    noteIds: body.noteIds,
  });
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

function reviewUrl(sessionId: string, suffix = ""): string {
  return `${getApiBase()}/sessions/${encodeURIComponent(sessionId)}/review${suffix}`;
}

async function call(url: string, init: RequestInit): Promise<unknown> {
  let resp: Response;
  try {
    resp = await tracedFetch(url, init);
  } catch (err) {
    if (init.signal?.aborted) throw err;
    throw new ReviewApiError(
      `runner unreachable: ${describeThrown(err, "network error")}`,
      { status: null, code: "network" },
    );
  }
  const text = await resp.text().catch(() => "");
  if (!resp.ok) throw errorFromReply(resp.status, text);
  try {
    return text ? (JSON.parse(text) as unknown) : null;
  } catch {
    return malformed("the reply is not JSON");
  }
}

function jsonInit(method: string, body: unknown, signal?: AbortSignal): RequestInit {
  return {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal,
  };
}

export async function fetchSessionReview(
  sessionId: string,
  signal?: AbortSignal,
): Promise<SessionReview> {
  return parseSessionReview(await call(reviewUrl(sessionId), { signal }), sessionId);
}

export async function markHunksRead(
  sessionId: string,
  hunks: HunkRef[],
  read: boolean,
): Promise<void> {
  await call(reviewUrl(sessionId, "/hunks"), jsonInit("PUT", { hunks, read }));
}

export async function createReviewNote(sessionId: string, note: NewNote): Promise<ReviewNoteRow> {
  return parseNoteRow(await call(reviewUrl(sessionId, "/notes"), jsonInit("POST", note)));
}

export async function patchReviewNote(
  sessionId: string,
  noteId: string,
  patch: NotePatch,
): Promise<ReviewNoteRow> {
  return parseNoteRow(
    await call(
      reviewUrl(sessionId, `/notes/${encodeURIComponent(noteId)}`),
      jsonInit("PATCH", patch),
    ),
  );
}

/** Submit the composed text as a turn. Notes move to `submitted` server-side. */
export async function sendReview(sessionId: string, req: SendRequest): Promise<SendOutcome> {
  return parseSendOutcome(await call(reviewUrl(sessionId, "/send"), jsonInit("POST", req)));
}

/** Type the composed text with no CR. Notes stay `attached`; delivery is the operator's. */
export async function insertReview(sessionId: string, req: SendRequest): Promise<SendOutcome> {
  return parseSendOutcome(await call(reviewUrl(sessionId, "/insert"), jsonInit("POST", req)));
}
