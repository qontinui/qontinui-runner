/**
 * The session search palette's model (Chrome's tab search, plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4): one list over live, previous and recently-closed sessions, a
 * free-text match over each entry's identifying text, and the transcript
 * full-text hits merged in.
 *
 * A session appears once. Live wins over previous and recently closed, and
 * previous over recently closed — the most actionable form of the session is
 * the one Enter acts on (focus a live one, resume the others).
 *
 * Pure: no React, no Tauri.
 */

import type { SessionFacts } from "./sessionFacts";
import type { ClosedSession } from "./sessionOrgTypes";
import type { PastSession } from "./usePastSessions";
import type { TranscriptSearchResponse } from "./transcriptSearch";

export type PaletteSource = "live" | "previous" | "closed";

export const PALETTE_SOURCE_LABEL: Readonly<Record<PaletteSource, string>> = {
  live: "Live",
  previous: "Previous",
  closed: "Recently closed",
};

export interface PaletteEntry {
  /** Unique within the list: `<source>:<id>`. */
  key: string;
  source: PaletteSource;
  title: string;
  /** The second line: topic, repo, account. */
  detail: string;
  /** The Claude session id, when known — the dedupe and transcript join key. */
  claudeSessionId: string | null;
  /** Live rows: the panel row id to focus. */
  rowId: string | null;
  /** Previous / closed: the organization-store key. */
  sessionKey: string | null;
  /** Previous / closed: the ready-to-run resume command, when known. */
  resumeCommand: string | null;
  /** Epoch ms of the last activity / close, for ordering; `null` = unknown. */
  at: number | null;
  /** Everything a word may match, lowercased. */
  haystack: string;
}

export interface PaletteSources {
  live: readonly SessionFacts[];
  previous: readonly PastSession[];
  closed: readonly ClosedSession[];
}

const lower = (parts: (string | null | undefined)[]): string =>
  parts
    .filter((p): p is string => typeof p === "string" && p !== "")
    .join(" \u0000 ")
    .toLowerCase();

/** The org's `sessionKey` for a Claude session is the id itself. */
function claudeIdOfKey(key: string): string | null {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(key) ? key : null;
}

/** Build the deduplicated entry list, live first. */
export function buildPaletteEntries(src: PaletteSources): PaletteEntry[] {
  const out: PaletteEntry[] = [];
  const seen = new Set<string>();
  const claim = (id: string | null): boolean => {
    if (id === null) return true;
    if (seen.has(id)) return false;
    seen.add(id);
    return true;
  };

  for (const f of src.live) {
    if (!claim(f.claudeSessionId ?? f.sessionKey)) continue;
    const repo = f.repo.known ? f.repo.value : null;
    const account = f.account.known ? f.account.value : null;
    out.push({
      key: `live:${f.rowId}`,
      source: "live",
      title: f.title,
      detail: [f.topic, repo, account].filter(Boolean).join(" · "),
      claudeSessionId: f.claudeSessionId,
      rowId: f.rowId,
      sessionKey: f.sessionKey,
      resumeCommand: null,
      at: f.lastActivityAt.known ? f.lastActivityAt.value : null,
      haystack: lower([f.title, f.topic, f.preview, repo, account, ...f.extraText]),
    });
  }
  for (const p of src.previous) {
    if (!claim(p.claudeSessionId)) continue;
    out.push({
      key: `previous:${p.claudeSessionId}`,
      source: "previous",
      title: p.resumeName || p.claudeSessionId,
      detail: [p.workingDir, p.account.label, p.finished ? "finished" : null]
        .filter(Boolean)
        .join(" · "),
      claudeSessionId: p.claudeSessionId,
      rowId: null,
      sessionKey: p.claudeSessionId,
      resumeCommand: p.resumeCommand || null,
      at: p.lastSeenAt,
      haystack: lower([
        p.resumeName,
        p.claudeSessionId,
        p.workingDir,
        p.account.label,
        p.finishReason,
      ]),
    });
  }
  for (const c of src.closed) {
    const id = claudeIdOfKey(c.sessionKey);
    if (!claim(id ?? c.sessionKey)) continue;
    out.push({
      key: `closed:${c.sessionKey}`,
      source: "closed",
      title: c.title || c.sessionKey,
      detail: "closed",
      claudeSessionId: id,
      rowId: null,
      sessionKey: c.sessionKey,
      resumeCommand: c.resumeCmd,
      at: c.closedAt,
      haystack: lower([c.title, c.sessionKey]),
    });
  }
  return out;
}

/**
 * The palette's terms, lowercased: `"quoted phrases"` whole (an unterminated
 * quote runs to the end), everything else split on whitespace. The palette is
 * a plain search box, not the filter grammar — `repo:coord` or `-x` typed
 * here is searched as the literal text it is.
 */
export function paletteTerms(query: string): string[] {
  const terms: string[] = [];
  for (const m of query.matchAll(/"([^"]*)"?|(\S+)/g)) {
    const term = (m[1] ?? m[2] ?? "").trim().toLowerCase();
    if (term !== "") terms.push(term);
  }
  return terms;
}

const SOURCE_RANK: Readonly<Record<PaletteSource, number>> = { live: 0, previous: 1, closed: 2 };

export interface PaletteMatch {
  entry: PaletteEntry;
  /** Why it matched: its own text, or its transcript (with a snippet). */
  via: "text" | "transcript";
  snippet: string | null;
}

/**
 * Entries matching `query` — every term a substring of the entry's text — and
 * those whose transcript `transcript_search` matched, live first, then most
 * recent. The empty query lists everything.
 */
export function searchPalette(
  entries: readonly PaletteEntry[],
  query: string,
  transcript: TranscriptSearchResponse | null = null,
  limit = 50,
): PaletteMatch[] {
  const terms = paletteTerms(query);
  const snippets = new Map<string, string>();
  for (const h of transcript?.hits ?? []) snippets.set(h.sessionId, h.snippet);

  const matches: PaletteMatch[] = [];
  for (const entry of entries) {
    if (terms.every((t) => entry.haystack.includes(t))) {
      matches.push({ entry, via: "text", snippet: null });
    } else if (terms.length > 0 && entry.claudeSessionId && snippets.has(entry.claudeSessionId)) {
      matches.push({
        entry,
        via: "transcript",
        snippet: snippets.get(entry.claudeSessionId) ?? null,
      });
    }
  }
  matches.sort(
    (a, b) =>
      SOURCE_RANK[a.entry.source] - SOURCE_RANK[b.entry.source] ||
      (a.via === b.via ? 0 : a.via === "text" ? -1 : 1) ||
      (b.entry.at ?? -Infinity) - (a.entry.at ?? -Infinity),
  );
  return matches.slice(0, limit);
}

/**
 * The palette's honest note on its transcript arm: how much was searched, and
 * whether the answer is complete. `null` when no transcript search ran.
 */
export function transcriptCoverageNote(
  transcript: TranscriptSearchResponse | null,
  state: "idle" | "pending" | "failed" | "done",
  failure: string | null = null,
): string | null {
  if (state === "idle") return null;
  if (state === "pending") return "Searching transcripts…";
  if (state === "failed") return `Transcript search failed: ${failure ?? "unknown error"}`;
  if (!transcript) return null;
  const base = `Transcripts: searched ${transcript.scanned} of ${transcript.total}`;
  if (!transcript.truncated) return base;
  return `${base} — incomplete (${transcript.truncation.join(", ")}); a session missing here may still match`;
}

/** Move the highlighted index by `delta`, wrapping; -1 when the list is empty. */
export function stepSelection(index: number, delta: number, length: number): number {
  if (length === 0) return -1;
  return (((index + delta) % length) + length) % length;
}
