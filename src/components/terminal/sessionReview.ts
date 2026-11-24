/**
 * The operator's review of a session's code changes, as pure functions.
 *
 * Plan `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 2. Four concerns, none of which touches React, Tauri or the network:
 *
 * - **Hunk identity** (`hunkKey`, `hunkKeysForFile`). A hunk is keyed on WHAT
 *   it changes — the file path plus its added/removed line texts — and never
 *   on WHERE: the `@@` header and the context lines are excluded, so an
 *   unrelated edit above it that shifts its line numbers keeps the key, while
 *   an edit to the hunk's own content produces a new key and the hunk reads as
 *   unread again ("changed since you read it"). Two byte-identical hunks in one
 *   file would share a content key, so every key carries an ordinal among the
 *   equal ones; marking one read never marks its twin.
 * - **Counts** (`reviewCounts`). A file whose diff could not be produced is
 *   UNKNOWN, never zero hunks — it counts toward `unknown`, not `total`.
 * - **The note lifecycle** (`noteTransition`). Design decision 3:
 *   `pending → attached → submitted → confirmed`, plus `attached → pending`
 *   (detach), `pending|attached → discarded`, and `submitted → unknown` when the
 *   session ends before the marker is seen. A total function: an illegal edge
 *   is a typed refusal, never a throw and never a silent no-op. The runner's
 *   routes (Phase 3) re-validate every edge; this reducer is a convenience for
 *   the UI, not the authority.
 * - **The composed prompt** (`composeReviewPrompt`, `markerFor`,
 *   `parseMarker`). Deterministic text whose first line is a `[review <8-hex>]`
 *   marker, so a later transcript read can tell the prompt ARRIVED rather than
 *   only that the write returned.
 *
 * The vitest environment is `node` with no React Testing Library (see
 * `hiddenWorkerReducer.ts`), which is why this behaviour lives here rather than
 * inside a hook.
 *
 * SHA-256 is a small synchronous implementation in this file: nothing in the
 * frontend already hashes, `crypto.subtle` is async-only (which would force
 * every render-time key computation through a promise) and is absent outside a
 * secure context, and a dependency for ~40 lines is not worth taking. The test
 * pins it against node's `crypto` on the standard vectors.
 */

import type { DiffHunk, SessionFileChange } from "./workerFileChanges";

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

const SHA256_K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

/** Lowercase hex SHA-256 of `text`'s UTF-8 bytes. Synchronous and pure. */
export function sha256Hex(text: string): string {
  const data = new TextEncoder().encode(text);
  const bitLength = data.length * 8;
  // Message + 0x80 + zero padding + 64-bit big-endian length, to a 64-byte multiple.
  const padded = new Uint8Array((((data.length + 9 + 63) >> 6) << 6) >>> 0);
  padded.set(data);
  padded[data.length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor(bitLength / 0x1_0000_0000), false);
  view.setUint32(padded.length - 4, bitLength >>> 0, false);

  const h = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const w = new Uint32Array(64);
  const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));

  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(offset + i * 4, false);
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + SHA256_K[i] + w[i]) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0] + a) >>> 0;
    h[1] = (h[1] + b) >>> 0;
    h[2] = (h[2] + c) >>> 0;
    h[3] = (h[3] + d) >>> 0;
    h[4] = (h[4] + e) >>> 0;
    h[5] = (h[5] + f) >>> 0;
    h[6] = (h[6] + g) >>> 0;
    h[7] = (h[7] + hh) >>> 0;
  }
  return Array.from(h, (x) => x.toString(16).padStart(8, "0")).join("");
}

// ---------------------------------------------------------------------------
// Hunk identity
// ---------------------------------------------------------------------------

/**
 * The content hash a hunk's key is built from: the path plus every `add` /
 * `del` line (kind and text), in order. Header and context lines are excluded
 * on purpose — see the module docs. The encoding is JSON so no choice of path
 * or line text can make two different hunks serialise identically.
 */
export function hunkContentHash(filePath: string, hunk: DiffHunk): string {
  const changed = hunk.lines
    .filter((line) => line.kind !== "ctx")
    .map((line) => [line.kind, line.text]);
  return sha256Hex(JSON.stringify([filePath, changed]));
}

/**
 * A hunk's review key: its content hash plus its ordinal among the hunks in
 * the same file that share that hash (`0` for the first, `1` for its first
 * byte-identical twin, …). The ordinal is ALWAYS present, so a lone hunk's key
 * does not change when a twin later appears below it.
 *
 * Callers keying every hunk of a file should use `hunkKeysForFile`, which
 * computes the ordinals; this form is for a caller that already knows one.
 */
export function hunkKey(filePath: string, hunk: DiffHunk, ordinal = 0): string {
  return `${hunkContentHash(filePath, hunk)}-${ordinal}`;
}

/**
 * Keys for every hunk of one file, in hunk order, distinct even for
 * byte-identical twins.
 *
 * Memoised per `hunks` ARRAY (and path): `diffHunks` hands every caller the
 * same array for the same row, so the badge, the file row and the diff view
 * share one SHA-256 pass instead of one each per render. The result is shared
 * — treat it as read-only.
 */
export function hunkKeysForFile(filePath: string, hunks: readonly DiffHunk[]): readonly string[] {
  const cached = HUNK_KEYS_CACHE.get(hunks);
  if (cached && cached.filePath === filePath) return cached.keys;
  const seen = new Map<string, number>();
  const keys = hunks.map((hunk) => {
    const hash = hunkContentHash(filePath, hunk);
    const ordinal = seen.get(hash) ?? 0;
    seen.set(hash, ordinal + 1);
    return `${hash}-${ordinal}`;
  });
  HUNK_KEYS_CACHE.set(hunks, { filePath, keys });
  return keys;
}

const HUNK_KEYS_CACHE = new WeakMap<
  readonly DiffHunk[],
  { filePath: string; keys: readonly string[] }
>();

// ---------------------------------------------------------------------------
// Counts
// ---------------------------------------------------------------------------

/**
 * One file as the review sees it. `hunks: null` means no diff could be
 * produced (binary, unreadable, over a size bound) — UNKNOWN. The one null that
 * is NOT unknown is an `unchanged` file, which genuinely has no hunks; pass its
 * `status` so it is not misread (`diffHunks` returns `null` for it too).
 */
export interface ReviewFile {
  filePath: string;
  hunks: readonly DiffHunk[] | null;
  status?: SessionFileChange["status"];
}

export interface ReviewCounts {
  /** Hunks whose key is not in the read set. */
  unread: number;
  /** Hunks across every file whose diff could be produced. */
  total: number;
  /** Files whose diff could not be produced — their hunk count is UNKNOWN. */
  unknown: number;
}

export function reviewCounts(
  files: readonly ReviewFile[],
  readKeys: ReadonlySet<string>,
): ReviewCounts {
  const counts: ReviewCounts = { unread: 0, total: 0, unknown: 0 };
  for (const file of files) {
    if (file.hunks === null) {
      if (file.status !== "unchanged") counts.unknown += 1;
      continue;
    }
    for (const key of hunkKeysForFile(file.filePath, file.hunks)) {
      counts.total += 1;
      if (!readKeys.has(key)) counts.unread += 1;
    }
  }
  return counts;
}

/** Every hunk key present in `files` — the set an orphaned note is judged against. */
export function liveHunkKeys(files: readonly ReviewFile[]): Set<string> {
  const keys = new Set<string>();
  for (const file of files) {
    if (!file.hunks) continue;
    for (const key of hunkKeysForFile(file.filePath, file.hunks)) keys.add(key);
  }
  return keys;
}

// ---------------------------------------------------------------------------
// Notes and their lifecycle
// ---------------------------------------------------------------------------

export type ReviewNoteState =
  | "pending"
  | "attached"
  | "submitted"
  | "confirmed"
  | "discarded"
  | "unknown";

/**
 * Mirrors a `project.session_review_notes` row (Phase 3), camelCase on the
 * wire. Timestamps are ISO-8601 strings. `excerpt` is the hunk text as it was
 * when the note was written, so a note whose hunk later disappears can still be
 * composed (orphaned) rather than dropped.
 */
export interface ReviewNote {
  id: string;
  sessionId: string;
  filePath: string;
  hunkKey: string;
  hunkHeader: string;
  excerpt: string;
  body: string;
  state: ReviewNoteState;
  marker: string | null;
  createdAt: string;
  submittedAt: string | null;
  confirmedAt: string | null;
}

export type ReviewNoteEvent =
  | { type: "attach" }
  | { type: "detach" }
  | { type: "discard" }
  | { type: "edit"; body: string }
  /** The prompt write returned Ok. `marker` is the marker the sent text carried. */
  | { type: "submit"; marker: string; at: string }
  /** A user prompt containing `marker` was observed in the session transcript. */
  | { type: "confirm"; marker: string; at: string }
  /** The session ended while the note was still unconfirmed. */
  | { type: "sessionEnded" };

export type ReviewNoteEventType = ReviewNoteEvent["type"];

export interface NoteTransitionRefusal {
  from: ReviewNoteState;
  event: ReviewNoteEventType;
  reason: string;
}

export type NoteTransitionResult =
  | { ok: true; note: ReviewNote }
  | { ok: false; refusal: NoteTransitionRefusal };

/** States each event may leave from. Anything else is an illegal edge. */
const LEGAL_FROM: Record<ReviewNoteEventType, readonly ReviewNoteState[]> = {
  attach: ["pending"],
  detach: ["attached"],
  discard: ["pending", "attached"],
  edit: ["pending", "attached"],
  submit: ["attached"],
  // `unknown → confirmed`: a marker seen after the session ended (or after a
  // runner restart settled the note) is positive evidence it arrived.
  confirm: ["submitted", "unknown"],
  sessionEnded: ["submitted"],
};

/** Whether `event` is a legal edge out of `state`. */
export function canTransition(state: ReviewNoteState, event: ReviewNoteEventType): boolean {
  return LEGAL_FROM[event].includes(state);
}

function refuse(
  from: ReviewNoteState,
  event: ReviewNoteEventType,
  reason: string,
): NoteTransitionResult {
  return { ok: false, refusal: { from, event, reason } };
}

/**
 * Apply one lifecycle event. Total: every (state, event) pair returns either
 * the new note or a typed refusal naming the edge. Pure — `note` is not
 * mutated.
 */
export function noteTransition(note: ReviewNote, event: ReviewNoteEvent): NoteTransitionResult {
  const from = note.state;
  if (!canTransition(from, event.type)) {
    return refuse(
      from,
      event.type,
      `"${event.type}" is not a legal edge out of "${from}" (legal from: ${LEGAL_FROM[
        event.type
      ].join(", ")})`,
    );
  }
  switch (event.type) {
    case "attach":
      return { ok: true, note: { ...note, state: "attached" } };
    case "detach":
      return { ok: true, note: { ...note, state: "pending" } };
    case "discard":
      return { ok: true, note: { ...note, state: "discarded" } };
    case "edit":
      return { ok: true, note: { ...note, body: event.body } };
    case "submit":
      if (parseMarker(`[review ${event.marker}]`) !== event.marker) {
        return refuse(from, event.type, `"${event.marker}" is not an 8-hex review marker`);
      }
      return {
        ok: true,
        note: { ...note, state: "submitted", marker: event.marker, submittedAt: event.at },
      };
    case "confirm":
      // Confirmation is evidence the prompt THIS note went out in arrived; a
      // different marker is some other prompt and proves nothing about it.
      if (event.marker !== note.marker) {
        return refuse(
          from,
          event.type,
          `observed marker "${event.marker}" is not this note's marker "${note.marker ?? "<none>"}"`,
        );
      }
      return { ok: true, note: { ...note, state: "confirmed", confirmedAt: event.at } };
    case "sessionEnded":
      return { ok: true, note: { ...note, state: "unknown" } };
  }
}

// ---------------------------------------------------------------------------
// Excerpts
// ---------------------------------------------------------------------------

/** Longest excerpt a composed prompt carries per note, truncation line included. */
export const EXCERPT_MAX_LINES = 12;

/** Cap `excerpt` at `EXCERPT_MAX_LINES` lines; the last kept line then names how many were cut. */
export function truncateExcerpt(excerpt: string, maxLines = EXCERPT_MAX_LINES): string {
  const lines = excerpt.split("\n");
  if (lines.length <= maxLines) return excerpt;
  const kept = lines.slice(0, maxLines - 1);
  const cut = lines.length - kept.length;
  return [...kept, `… ${cut} more line${cut === 1 ? "" : "s"}`].join("\n");
}

/**
 * The excerpt stored on a new note: the hunk's lines in unified-diff form
 * (`+`, `-`, or a space before each), capped like `truncateExcerpt`.
 */
export function excerptForHunk(hunk: DiffHunk, maxLines = EXCERPT_MAX_LINES): string {
  const text = hunk.lines
    .map((line) => `${line.kind === "add" ? "+" : line.kind === "del" ? "-" : " "}${line.text}`)
    .join("\n");
  return truncateExcerpt(text, maxLines);
}

// ---------------------------------------------------------------------------
// Marker and composed prompt
// ---------------------------------------------------------------------------

const MARKER_RE = /\[review ([0-9a-f]{8})\]/;

/** The marker line a composed prompt starts with. */
export function markerLine(marker: string): string {
  return `[review ${marker}]`;
}

/**
 * The 8-hex marker for one send. Deterministic in its inputs; `salt` is what
 * keeps two sends of the same notes and text apart (the caller passes a send
 * id or a timestamp), so one send's confirmation cannot be mistaken for
 * another's.
 */
export function markerFor(input: {
  sessionId: string;
  noteIds: readonly string[];
  freeText: string;
  salt: string;
}): string {
  return sha256Hex(
    JSON.stringify([input.sessionId, [...input.noteIds], input.freeText, input.salt]),
  ).slice(0, 8);
}

/** The first review marker in `text`, or `null`. Matches anywhere, since a transcript may prefix the prompt. */
export function parseMarker(text: string): string | null {
  return MARKER_RE.exec(text)?.[1] ?? null;
}

/** A fence longer than any backtick run in `text`, so an excerpt can never close it early. */
function fenceFor(text: string): string {
  let longest = 0;
  for (const run of text.match(/`+/g) ?? []) longest = Math.max(longest, run.length);
  return "`".repeat(Math.max(3, longest + 1));
}

export interface ComposedReviewPrompt {
  /** The exact text that will be sent. */
  text: string;
  marker: string;
  /** Notes composed from their stored excerpt because their hunk is no longer in the diff. */
  orphanedNoteIds: string[];
}

export const ORPHAN_LABEL =
  "(orphaned: this hunk is no longer in the current diff — excerpt as it was when the note was written)";

/**
 * The prompt a review send delivers, deterministic in its inputs.
 *
 * Line 1 is the marker. Then, per note in the order given: its number, path
 * and hunk header (labelled orphaned when `liveKeys` no longer holds its hunk
 * key), its excerpt (≤ `EXCERPT_MAX_LINES` lines, fenced), and the operator's
 * comment. Then the free text, when there is any. Orphaned notes are composed
 * from their stored excerpt — never dropped.
 *
 * `notes` is composed as given; choosing which notes go out (the attached
 * chips) is the caller's.
 */
export function composeReviewPrompt(
  notes: readonly ReviewNote[],
  freeText: string,
  options: { marker: string; liveKeys: ReadonlySet<string> },
): ComposedReviewPrompt {
  const { marker, liveKeys } = options;
  if (parseMarker(markerLine(marker)) !== marker) {
    throw new Error(`composeReviewPrompt: "${marker}" is not an 8-hex review marker`);
  }
  const orphanedNoteIds: string[] = [];
  const sections: string[] = [markerLine(marker)];
  if (notes.length > 0) {
    sections.push(
      `Review notes on this session's changes (${notes.length} note${notes.length === 1 ? "" : "s"}):`,
    );
  }
  notes.forEach((note, i) => {
    const orphaned = !liveKeys.has(note.hunkKey);
    if (orphaned) orphanedNoteIds.push(note.id);
    const excerpt = truncateExcerpt(note.excerpt);
    const fence = fenceFor(excerpt);
    const heading = `${i + 1}. ${note.filePath} ${note.hunkHeader}${orphaned ? ` ${ORPHAN_LABEL}` : ""}`;
    const comment = note.body.trim().split("\n").join("\n   ");
    sections.push([heading, `${fence}diff`, excerpt, fence, `Comment: ${comment}`].join("\n"));
  });
  const trimmed = freeText.trim();
  if (trimmed) sections.push(trimmed);
  return { text: sections.join("\n\n"), marker, orphanedNoteIds };
}
