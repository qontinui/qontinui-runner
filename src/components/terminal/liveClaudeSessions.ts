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
  /** Ready-to-run `cd '<dir>' && <wrapper> --resume <id>`. */
  resumeCommand: string;
}

/**
 * Normalize the command's `data` payload into a `LiveClaudeSession[]`.
 *
 * The command wraps its payload as `{ sessions: [...] }`; a bare array is
 * accepted too. Anything else degrades to `[]` — callers iterate this during
 * render, so a shape surprise must never throw. (Same contract, and the same
 * hard-won reason, as `usePastSessions.extractSessions`.)
 */
export function extractLiveSessions(data: unknown): LiveClaudeSession[] {
  if (Array.isArray(data)) return data as LiveClaudeSession[];
  if (data && typeof data === "object") {
    const inner = (data as { sessions?: unknown }).sessions;
    if (Array.isArray(inner)) return inner as LiveClaudeSession[];
  }
  return [];
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
 * good fallback. The `typeof` guard is the same defensiveness `groupByAccount`
 * applies to `account`: this runs during render over a payload that reached us
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
export function registryNamesBySessionId(
  sessions: readonly LiveClaudeSession[],
): Map<string, string> {
  const bestById = new Map<string, LiveClaudeSession>();
  for (const s of sessions) {
    if (!s.sessionId || !isOperatorNamed(s)) continue;
    const held = bestById.get(s.sessionId);
    if (held === undefined || (s.updatedAt ?? 0) > (held.updatedAt ?? 0)) {
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
 * Session ids reported by more than one live process.
 *
 * This is not a curiosity — it is the operator-visible symptom of the restore
 * duplication loop: a restore that respawns `claude --resume <id>` while the
 * previous generation is still alive leaves several live processes on one
 * session id, each with its own auto-generated `<dir>-<2hex>` name. 22 of 80
 * ids were in this state on 2026-07-23. Resuming such an id once does NOT
 * reproduce every window, so the caller must surface it rather than silently
 * collapsing the rows.
 */
export function sharedSessionIds(
  sessions: readonly LiveClaudeSession[],
): Map<string, LiveClaudeSession[]> {
  const byId = new Map<string, LiveClaudeSession[]>();
  for (const s of sessions) {
    const list = byId.get(s.sessionId) ?? [];
    list.push(s);
    byId.set(s.sessionId, list);
  }
  for (const [id, list] of byId) {
    if (list.length < 2) byId.delete(id);
  }
  return byId;
}

/** Group sessions by account label, preserving input order within a group. */
export function groupByAccount(
  sessions: readonly LiveClaudeSession[],
): Map<string, LiveClaudeSession[]> {
  const byAccount = new Map<string, LiveClaudeSession[]>();
  for (const s of sessions) {
    const label = s.account?.label || "unknown";
    const list = byAccount.get(label) ?? [];
    list.push(s);
    byAccount.set(label, list);
  }
  return byAccount;
}
