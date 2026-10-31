/**
 * The runner's session roster — the rebuild-safe session ledger, read from the
 * UI (plan `2026-10-04-runner-session-roster-restore-picker`, Phase 2).
 *
 * Hand-written mirror of the serde types in
 * `src-tauri/src/session/session_ledger.rs` (camelCase, `Option<T>` → `T |
 * null`). The key set of every type is pinned on BOTH sides against
 * `src/lib/__golden__/session-ledger-keys.json`, so a field added on one side
 * only fails a test rather than drifting silently.
 *
 * The two Tauri commands share their builders with the HTTP routes
 * `GET /control/sessions/ledger` and `POST /control/sessions/ledger/capture`,
 * so this module and those routes always return the same document.
 */

import { invoke } from "@tauri-apps/api/core";

/** `match` | `partial` | `mismatch` | `unknown` — the restore-census vocabulary. */
export type LedgerVerdict = "match" | "partial" | "mismatch" | "unknown";

/**
 * What became of one prior session: it is open (or was restored) again, it
 * did not come back and was unfinished, or it was marked finished and is not
 * expected back (never counted as missing).
 */
export type LedgerOutcomeClass = "back" | "missing" | "finished" | "closed-by-user";

/**
 * Why a `missing` session is missing:
 * - `no-attempt` — it was resumable and nothing brought it back;
 * - `not-restorable` — it was never identity-restorable;
 * - `restorability-unknown` — could not be determined; NOT evidence either way.
 */
export type LedgerMissingReason = "no-attempt" | "not-restorable" | "restorability-unknown";

/** One session on the roster, as it stood when a ledger was captured. */
export interface LedgerEntry {
  claudeSessionId: string;
  terminalId: string;
  pageId: string;
  zoneIndex: number;
  /** The runner tab label (often just `"claude"`). */
  title: string | null;
  /** The in-provider session name (`/rename`, or Claude Code's auto-name). */
  sessionName: string | null;
  /** `"derived"` = Claude Code's auto-name; anything else (or null) = operator-chosen. */
  nameSource: string | null;
  accountLabel: string | null;
  /** The `CLAUDE_CONFIG_DIR` the session ran under; null = unknown account. */
  configDir: string | null;
  /**
   * The account root RESOLVED at capture: `configDir`, else the one config dir
   * holding the transcript. null = unknown (or a ledger predating the field).
   */
  resumeConfigDir: string | null;
  /** The shared copy-able resume line at capture; null when the account or the dir is unknown. */
  resumeCommand: string | null;
  /** `"claude"`, `"gemini"`, … — null only in a ledger written before the field existed. */
  provider: string | null;
  workingDir: string | null;
  /** Unix millis, as of the last ledger WRITE (not refreshed on quiet ticks). */
  lastSeenAt: number | null;
  /** Marked finished at capture time. */
  finished: boolean;
  /** Resumable at capture: true / false / null = could not be determined. */
  restorable: boolean | null;
  worktreePath: string | null;
  planSlug: string | null;
  workUnitId: string | null;
  /** Verbatim custody `wip_state`; `"captured"` = WIP snapshotted to `wipRef`. */
  wipState: string | null;
  wipRef: string | null;
  custodySessionMismatch: boolean;
}

/** One captured ledger — the live roster, or one retained generation. */
export interface SessionLedger {
  ledgerVersion: number;
  /** Unix millis of capture. */
  capturedAtMs: number;
  capturedAt: string;
  /** `boot-latch` | `poll` | `pre-rebuild` | `operator` | `read` | a caller label. */
  reason: string;
  /** Unix millis at which the capturing runner process booted. */
  bootAtMs: number | null;
  shutdownAt: number | null;
  /** null = this process never classified its boot (UNKNOWN, not false). */
  cleanShutdown: boolean | null;
  sessions: LedgerEntry[];
}

/**
 * The account a TYPED `--resume` runs under (Rust `session_ledger::ResumeAccount`):
 * - `known && configDir` — set `CLAUDE_CONFIG_DIR` to it;
 * - `known && !configDir` — the default home (`~/.claude`): set no `CLAUDE_CONFIG_DIR`;
 * - `!known` — unknown: the operator must choose. Never resumed under the default.
 */
export interface ResumeAccount {
  known: boolean;
  configDir: string | null;
}

/** One prior session and what became of it. */
export interface LedgerOutcome {
  claudeSessionId: string;
  terminalId: string;
  pageId: string;
  zoneIndex: number;
  /** The name to show — see {@link displayNameOf}. */
  displayName: string;
  sessionName: string | null;
  nameSource: string | null;
  title: string | null;
  accountLabel: string | null;
  configDir: string | null;
  /** `configDir` present and non-blank; false ⇒ a resume would run under the default account. */
  configDirKnown: boolean;
  provider: string | null;
  workingDir: string | null;
  worktreePath: string | null;
  planSlug: string | null;
  workUnitId: string | null;
  wipState: string | null;
  wipRef: string | null;
  custodySessionMismatch: boolean;
  /** Unix millis — see {@link LedgerEntry.lastSeenAt}. */
  lastSeenAt: number | null;
  restorable: boolean | null;
  /** Finished as of NOW when the registry still holds the row, else as captured. */
  finished: boolean;
  outcome: LedgerOutcomeClass;
  /** Set only for `missing`. */
  reason: LedgerMissingReason | null;
  /**
   * The directory a resume opens in — the recorded `workingDir` (Claude Code
   * scopes sessions by the exact launch dir), else the worktree root. THE rule,
   * computed once in Rust (`session_ledger::resume_dir_of`) for the copy line
   * and the one-click resume alike.
   */
  resumeDir: string | null;
  /** `cd "<dir>" && CLAUDE_CONFIG_DIR="<root>" claude --resume <id>`; null when the account is unknown — never guessed. */
  resumeCommand: string | null;
  /** The account a one-click resume types — see {@link ResumeAccount}. */
  resumeAccount: ResumeAccount;
}

/** One retained prior boot's ledger, diffed on its own against what is back now. */
export interface LedgerGeneration {
  /** File name inside the runner's ledger directory. */
  file: string;
  /** Unix millis of the boot that retired it (the restart that ended it). */
  rotatedAtMs: number;
  /** Unix millis at which the process that wrote it booted. */
  bootAtMs: number | null;
  /**
   * This is the generation THIS runner boot retired — the roster that was up
   * when it started. Set on `generations[0]` only, and only when this boot
   * retained one; never inferred from `rotatedAtMs`, which is a wall-clock
   * stamp a clock stepped backwards can mis-order.
   */
  thisBoot: boolean;
  capturedAtMs: number;
  capturedAt: string;
  reason: string;
  cleanShutdown: boolean | null;
  sessionCount: number;
  verdict: LedgerVerdict;
  returned: LedgerOutcome[];
  missing: LedgerOutcome[];
  finished: LedgerOutcome[];
  /** Unfinished sessions the operator has since closed on purpose — not expected back. */
  closedByUser: LedgerOutcome[];
}

/** The roster report — `session_ledger_report` / `GET /control/sessions/ledger`. */
export interface LedgerReport {
  status: "ok" | "unavailable";
  /** Present whenever status is not `ok` or the verdict is `unknown`. */
  reason: string | null;
  generatedAt: number;
  priorCapturedAt: string | null;
  priorReason: string | null;
  /** The prior boot's roster (the newest generation). */
  expected: LedgerEntry[];
  returned: LedgerOutcome[];
  /** Unfinished sessions that did not come back. */
  missing: LedgerOutcome[];
  /** Finished sessions that did not come back — not expected back. */
  finished: LedgerOutcome[];
  /** Unfinished sessions the operator has since closed on purpose — not expected back. */
  closedByUser: LedgerOutcome[];
  verdict: LedgerVerdict;
  /** The roster right now — what would come back if the runner restarted. */
  current: SessionLedger;
  /** Every retained prior generation, newest first. */
  generations: LedgerGeneration[];
  /**
   * Unix millis at which THIS runner process last wrote the saved roster (what
   * the next boot reports against); null = none written yet this boot. Written
   * only on change, so an old instant with `savedMatchesCurrent` is current.
   */
  savedAtMs: number | null;
  /** The saved roster equals the current one; false when nothing is saved yet. */
  savedMatchesCurrent: boolean;
  note: string;
}

/** What `session_ledger_capture` did. */
export interface LedgerCapture {
  /** A write landed; false = unchanged (or an empty capture refused), not a failure. */
  persisted: boolean;
  path: string;
  ledger: SessionLedger;
}

/**
 * THE display-name rule, mirroring `session_ledger::display_name` in Rust:
 * an operator-chosen (or source-less) `sessionName` wins; a `derived`
 * auto-name falls back behind `title`; blanks count as absent; with neither,
 * `claude <id8>`.
 */
export function displayNameOf(entry: {
  claudeSessionId: string;
  sessionName: string | null;
  nameSource: string | null;
  title: string | null;
}): string {
  const nonblank = (s: string | null): string | null => {
    const t = s?.trim();
    return t ? t : null;
  };
  const name = nonblank(entry.sessionName);
  const title = nonblank(entry.title);
  const preferred = entry.nameSource === "derived" ? (title ?? name) : (name ?? title);
  return preferred ?? `claude ${[...entry.claudeSessionId].slice(0, 8).join("")}`;
}

/** Read the roster report (prior generations vs what is back, plus the current roster). */
export function getSessionLedgerReport(): Promise<LedgerReport> {
  return invoke<LedgerReport>("session_ledger_report");
}

/** "Capture now": persist the current roster with reason `operator`. */
export function captureSessionLedger(): Promise<LedgerCapture> {
  return invoke<LedgerCapture>("session_ledger_capture");
}

/**
 * Mark a session's WORK finished (or unfinish it) — Tauri
 * `terminal_session_set_finished`, the twin of `POST /sessions/{id}/finish`.
 * Metadata only: the process is never touched; a finished session is simply
 * not offered for restore.
 *
 * `changed: false` is the backend's no-op answer: the id is unknown
 * (`success: false`), or the marker was already in that state (`success: true`
 * with `data.changed: "none"` — the backend answers the finish route's body,
 * `FinishOutcome::response_json`). It is NOT a failure to retry — re-read the
 * roster to see which.
 */
export async function setSessionFinished(
  claudeSessionId: string,
  finished: boolean,
  reason?: string,
): Promise<{ changed: boolean; message: string | null }> {
  const res = await invoke<{
    success: boolean;
    message?: string | null;
    data?: { changed?: string } | null;
  }>("terminal_session_set_finished", {
    claudeSessionId,
    finished,
    reason: reason ?? null,
  });
  const changed = res.success && res.data?.changed !== "none";
  return { changed, message: res.message ?? null };
}
