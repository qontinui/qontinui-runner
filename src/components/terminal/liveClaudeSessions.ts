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
