/**
 * Client-side view of Claude Code's own live-session registry, read through the
 * `terminal_claude_session_list_live` Tauri command.
 *
 * This is the ONLY source for the name an operator actually sees — the string
 * in the session window and in `/resume`. The sibling `PastSession.resumeName`
 * (from `usePastSessions`) is transcript-derived and is a different value:
 * measured 2026-07-23, it covered 33 of 80 live sessions and matched the real
 * window name on only 11 of those 33.
 *
 * Live processes only — Claude Code deletes the backing file when a process
 * exits. Use this for "what is open right now"; use `usePastSessions` for
 * history.
 *
 * Keys are camelCase, matching the Rust command's serde output exactly.
 */
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";

export interface LiveClaudeSessionAccount {
  /** Account label, e.g. `"paktis"`. */
  label: string;
  /** CLI wrapper for that account, e.g. `"clp"`. */
  wrapper: string;
}

export interface LiveClaudeSession {
  /** The `--resume` key. */
  sessionId: string;
  /** Name shown in the session window and in `/resume`. Ground truth. */
  name: string;
  /**
   * Provenance of {@link name}, verbatim from the registry file.
   *
   * `"derived"` marks Claude Code's OWN `<dir>-<2hex>` auto-derivation
   * (`qontinui-root-ec`, `qontinui-coord-88`) — strictly *worse* than the
   * transcript/plan-title names the runner's cards already show, so it must
   * never preempt them. Absent (the common case — 12 of 17 named rows on the
   * operator's box on 2026-07-28) means an operator `/rename`, which does win.
   *
   * Optional rather than `string | null` on purpose: a runner binary predating
   * the field emits no key at all, and that skew must read as "operator-named",
   * matching the absent-key majority.
   *
   * @see {@link isOperatorNamed}
   */
  nameSource?: string | null;
  /** OS process id that reported this entry. */
  pid: number;
  account: LiveClaudeSessionAccount;
  /** Launch directory, forward-slashed so it drops into a shell command. */
  workingDir: string;
  /** Claude Code's self-reported status (`idle` / `busy` / `waiting` / …). */
  status: string;
  /** `"interactive"` for real windows. */
  kind: string;
  startedAt: number;
  updatedAt: number;
  /**
   * Ready-to-run `cd "<dir>" && CLAUDE_CONFIG_DIR="<config>" claude --resume
   * <id>` (Rust `session_ledger::resume_command_for`). `null` when the row
   * carries no cwd — a resume from the wrong dir finds nothing.
   */
  resumeCommand: string | null;
}

/**
 * STRICT payload extraction: the command's `data` must be a bare array or an
 * object whose `sessions` key is an array — anything else returns `null`
 * ("shape not understood"), never `[]`.
 *
 * The liveness oracle (`fetchLiveClaudeSessionIds`) depends on this polarity:
 * a misshapen payload (e.g. `{}` after a Rust-side rename) that degraded to
 * `[]` would read as "definitively no live sessions" and let the restore path
 * respawn — fork — every session. Display callers who want the tolerant
 * degrade use [`extractLiveSessions`].
 */
export function extractLiveSessionsStrict(data: unknown): LiveClaudeSession[] | null {
  if (Array.isArray(data)) return data as LiveClaudeSession[];
  if (data && typeof data === "object") {
    const inner = (data as { sessions?: unknown }).sessions;
    if (Array.isArray(inner)) return inner as LiveClaudeSession[];
  }
  return null;
}

/**
 * Normalize the command's `data` payload into a `LiveClaudeSession[]`.
 *
 * The command wraps its payload as `{ sessions: [...] }`; a bare array is
 * accepted too. Anything else degrades to `[]` — callers iterate this during
 * render, so a shape surprise must never throw. (Same contract, and the same
 * hard-won reason, as `usePastSessions.extractSessions`.) DISPLAY-only:
 * liveness decisions must use [`extractLiveSessionsStrict`], where an
 * unrecognized shape is indeterminate, not empty.
 */
export function extractLiveSessions(data: unknown): LiveClaudeSession[] {
  return extractLiveSessionsStrict(data) ?? [];
}

/**
 * The one `nameSource` value that disqualifies a registry name from preempting
 * an existing name. Everything else — including an absent key — is trusted.
 */
export const DERIVED_NAME_SOURCE = "derived";

/**
 * Is this row's `name` one the operator chose (or was launched with), rather
 * than Claude Code's cwd-slug auto-derivation?
 *
 * The gate is deliberately one-sided: only the exact string `"derived"` is
 * rejected. A future CLI variant we do not know about is treated as trustworthy
 * because the failure mode of the other default — silently ignoring real
 * operator names — is the bug this whole plan exists to fix, whereas the worst
 * case here is showing a name we could have shown anyway.
 *
 * A blank name never qualifies: it would blank out a card that has a perfectly
 * good fallback. The `typeof` guard is deliberate: this runs during render over
 * a payload that reached us
 * through an unchecked cast, so a version skew must degrade to "no registry
 * name" rather than throw.
 */
export function isOperatorNamed(session: Pick<LiveClaudeSession, "name" | "nameSource">): boolean {
  if (typeof session.name !== "string" || session.name.trim().length === 0) return false;
  return session.nameSource !== DERIVED_NAME_SOURCE;
}

/**
 * Build the `sessionId → window name` lookup the session cards consult.
 *
 * Only operator-chosen names are included, so a miss means "no trustworthy
 * registry name" and the caller falls through to whatever it shows today —
 * which is exactly the required behaviour for BOTH a `derived` row and a closed
 * session (the registry only ever describes live processes, so a closed session
 * is always a miss and always keeps its `resumeName`).
 *
 * Several live processes may share one session id (22 of 80 on 2026-07-23), so
 * one of them has to be chosen. **The most recently updated wins** — that is
 * the window the operator is actually working in, and it is the only tiebreak
 * that survives the event this feature exists to track. Picking the first row
 * in `read_live_sessions` order (account → name → pid) looks stable but is
 * keyed on the *name*, so renaming `"aaa"` to `"zzz2"` alongside a sibling
 * `"zzz"` would re-sort the pair and make the card show the SIBLING's name —
 * the rename appearing to do nothing, or worse, to pick the wrong window.
 *
 * Equal `updatedAt` (including the 0 the Rust reader substitutes when the
 * registry omits the field) keeps the first row, i.e. `read_live_sessions`
 * order, so the result is still deterministic across polls.
 *
 * Names are trimmed: the gate tolerates surrounding whitespace, but the cards
 * render into a truncating single-line span where padding is visible.
 */
/**
 * Recency key for the duplicate-`sessionId` tiebreak.
 *
 * Falls back `updatedAt` → `startedAt` → `0`. The fallback is load-bearing, not
 * defensive padding: the Rust reader substitutes `0` for a missing key, so if a
 * CLI build ever stops emitting `updatedAt`, EVERY row ties at 0 and the winner
 * silently reverts to `read_live_sessions` order — account, then **name**, then
 * pid. Ordering by name is exactly the bug this tiebreak exists to prevent (a
 * rename re-sorts the pair and hands the card a sibling's name), so a fleet-wide
 * missing field would reintroduce it invisibly. `startedAt` is already modelled
 * end to end and is always present on a row that has a pid, so it is a free
 * second anchor.
 */
function recencyOf(s: LiveClaudeSession): number {
  return s.updatedAt || s.startedAt || 0;
}

export function registryNamesBySessionId(
  sessions: readonly LiveClaudeSession[],
): Map<string, string> {
  const bestById = new Map<string, LiveClaudeSession>();
  for (const s of sessions) {
    if (!s.sessionId || !isOperatorNamed(s)) continue;
    const held = bestById.get(s.sessionId);
    if (held === undefined || recencyOf(s) > recencyOf(held)) {
      bestById.set(s.sessionId, s);
    }
  }
  const byId = new Map<string, string>();
  for (const [sessionId, s] of bestById) {
    byId.set(sessionId, s.name.trim());
  }
  return byId;
}

/**
 * What one read of the live registry saw.
 *
 * `ids` is every session id a live Claude Code process hosts, from every
 * `sessions/` dir the runner could read. `partial` is true when the read could
 * not see everything — a `sessions/` dir exists and could not be listed, or a
 * live process's record could not be read (the command's `unlistable` and
 * `unreadableRecords`). While it is, an id MISSING from `ids` may still be
 * live; an id PRESENT in it is live either way.
 */
export interface LiveSessionRead {
  ids: ReadonlySet<string>;
  partial: boolean;
}

/**
 * Does the command's `data` say the read was incomplete? A runner binary
 * predating the two keys sends neither, and is read as complete, exactly as it
 * was before. A key that is present but not an array is a shape we do not
 * understand, so it reads as incomplete.
 */
function readIsPartial(data: unknown): boolean {
  if (!data || typeof data !== "object" || Array.isArray(data)) return false;
  const gaps = data as { unlistable?: unknown; unreadableRecords?: unknown };
  return [gaps.unlistable, gaps.unreadableRecords].some(
    (gap) => gap !== undefined && (!Array.isArray(gap) || gap.length > 0),
  );
}

/**
 * Read the live registry once through `terminal_claude_session_list_live`
 * (each entry is PID-verified against the live process table on the Rust side
 * — `session::claude_session_registry::read_live_sessions`).
 *
 * Returns `null` when the registry could not be read at all (invoke failed,
 * unsuccessful response, missing or unrecognized payload). A read that
 * succeeded but could not see everything is NOT `null`: it is `partial`, and
 * each caller decides what that means for it — see
 * {@link fetchLiveClaudeSessionIds} and {@link fetchVisibleLiveClaudeSessionIds}.
 */
export async function fetchLiveClaudeSessionRead(): Promise<LiveSessionRead | null> {
  let resp: CommandResponse;
  try {
    resp = await invoke<CommandResponse>("terminal_claude_session_list_live");
  } catch (err) {
    console.warn("[liveClaudeSessions] terminal_claude_session_list_live failed:", err);
    return null;
  }
  // `data` is always present on a successful response; its absence means we
  // did not understand the payload — indeterminate, not empty.
  if (!resp?.success || resp.data == null) return null;
  // Strict shape check: a payload that is neither an array nor `{sessions:
  // [...]}` (renamed key, `{}`, version skew) is INDETERMINATE — flowing it
  // through the tolerant extractor would yield [] = "definitively empty" and
  // green-light respawning everything.
  const sessions = extractLiveSessionsStrict(resp.data);
  if (sessions === null) {
    console.warn(
      "[liveClaudeSessions] terminal_claude_session_list_live returned an unrecognized payload shape — treating liveness as indeterminate",
    );
    return null;
  }
  const ids = new Set<string>();
  for (const s of sessions) {
    if (s && typeof s.sessionId === "string" && s.sessionId) ids.add(s.sessionId);
  }
  return { ids, partial: readIsPartial(resp.data) };
}

/**
 * The STRICT liveness oracle: the set of session ids some live Claude Code
 * process is already hosting, or `null` when that cannot be known in full.
 *
 * The unattended restore paths (the cold-restore drain, the sweep recheck, the
 * profile resume) consult this BEFORE typing `claude --resume <id>`: an id in
 * this set is already alive, and resuming it again forks the session — a
 * second live process on the same transcript (measured 2026-07-23: 22 of 80
 * live ids had >1 live process). They decide "not live" from an id's ABSENCE,
 * so a PARTIAL read is `null` here too — the missing id may sit in the dir that
 * could not be read.
 *
 * Callers MUST treat `null` as indeterminate and FAIL CLOSED toward
 * not-respawning — never as "nothing alive". A missing tab is recoverable; a
 * duplicate live session corrupts the transcript.
 */
export async function fetchLiveClaudeSessionIds(): Promise<ReadonlySet<string> | null> {
  const read = await fetchLiveClaudeSessionRead();
  if (!read || read.partial) {
    if (read) {
      console.warn(
        "[liveClaudeSessions] the live-session registry could not be read in full — treating liveness as indeterminate",
      );
    }
    return null;
  }
  return read.ids;
}

/**
 * Every session id the registry could SEE live, even when the read was
 * partial; `null` only when it could not be read at all.
 *
 * For an operator's click (Resume in a new tab, Resume unfinished): those
 * paths refuse an id that is visibly live and otherwise leave the decision to
 * the resume door, so a partial read must still refuse every session it shows
 * live rather than collapse to `null`, which they read as "go ahead".
 */
export async function fetchVisibleLiveClaudeSessionIds(): Promise<ReadonlySet<string> | null> {
  const read = await fetchLiveClaudeSessionRead();
  return read?.ids ?? null;
}
