import { useMemo, useState, useCallback } from "react";
import { TerminalSquare, Copy, Check, RefreshCw, Layers, AlertTriangle } from "lucide-react";
import { usePastSessions, type PastSession } from "./usePastSessions";
import {
  refreshLiveClaudeSessionNames,
  useLiveClaudeSessionNames,
} from "./useLiveClaudeSessionNames";
import { instanceStorage } from "@/lib/instance-storage";

/**
 * Max cards rendered per cohort before a "Show N more" expander. A single crash
 * cohort can hold 80+ stranded sessions; rendering every card eagerly across
 * several cohorts is what would jank the panel, so each cohort renders a capped
 * window and reveals the rest on demand. Lightweight cap-render rather than a
 * full windowing dependency — the cohort count is small and each card is cheap.
 */
const COHORT_RENDER_CAP = 30;

/**
 * UI-Bridge addressability for this view — two DIFFERENT attributes doing two
 * DIFFERENT jobs. Do not swap them.
 *
 * **`data-page-element`** (the `PAST_SESSIONS_*_ELEMENT` constants below) is
 * NOT an SDK concept — it is a plain CSS selector consumed by
 * `POST /ui-bridge/control/page/read-value
 * {"selector":"[data-page-element=past-sessions-view]"}`, the codebase's
 * established recipe for a SCOPED REGION READ (same convention as
 * `command-bar`, `status-strip`, `terminal-session-roster`). Reading the
 * panel used to mean scraping `document.body.innerText` and slicing around a
 * substring, which also swept in unrelated terminal-grid text.
 *
 * **`data-ui-bridge-id`** (the `pastSession*Id` helpers below) pins a CONTROL
 * id for `POST /ui-bridge/control/element/<id>/action`. The SDK derives ids
 * from visible text with per-parent ordinal disambiguation, so N cards each
 * rendering "Copy command" / "Resume" collide on ordinals — and cohorts are
 * sorted newest-first by `lastSeenAt`, so those ordinals SHIFT on every
 * refresh. A hand-written stamp wins over auto-derivation and is echoed
 * verbatim as `uiBridgeId` in `GET /ui-bridge/control/snapshot`, making each
 * action addressable by its own session/cohort key instead of by position.
 */
export const PAST_SESSIONS_VIEW_ELEMENT = "past-sessions-view";
/** @see {@link PAST_SESSIONS_VIEW_ELEMENT} */
export const PAST_SESSIONS_COHORT_ELEMENT = "past-sessions-cohort";
/** @see {@link PAST_SESSIONS_VIEW_ELEMENT} */
export const PAST_SESSION_CARD_ELEMENT = "past-session-card";

/** Stable control id for the "Refresh previous sessions" button. */
export const PAST_SESSIONS_REFRESH_ID = "terminal.past-sessions-refresh";
/** Stable control id for the error-state "Retry" button. */
export const PAST_SESSIONS_RETRY_ID = "terminal.past-sessions-retry";
/** Stable control id for the "Show finished" toggle. */
export const PAST_SESSIONS_SHOW_FINISHED_ID = "terminal.past-sessions-show-finished";

/**
 * `instanceStorage` key for the show-finished preference.
 *
 * Instance-scoped like the SessionManager's four filter axes: two runners on one
 * box must not share a view preference.
 */
const SHOW_FINISHED_KEY = "terminal.past-sessions.show-finished";

/** Stable control id for one card's "Copy command" button. */
export function pastSessionCopyId(claudeSessionId: string): string {
  return `terminal.past-session-copy-${claudeSessionId}`;
}

/** Stable control id for one card's "Resume" button. */
export function pastSessionResumeId(claudeSessionId: string): string {
  return `terminal.past-session-resume-${claudeSessionId}`;
}

/**
 * Stable control id for one card's ROW CONTAINER.
 *
 * The card was addressable only as a raw CSS selector
 * (`[data-page-element=past-session-card]`), which `read-value` can scrape but
 * which never appears in `GET /ui-bridge/control/snapshot` — the scanner
 * registers interactive elements plus anything carrying `data-ui-bridge-id`,
 * and a `<div>` has neither a role nor a tag that qualifies. So everything a
 * row SAYS (name, account, page/zone, last-active, state, restore verdict) was
 * invisible to any check that reads elements rather than scraping the DOM.
 * Stamping the row registers it with its own text, keyed by session id rather
 * than by position — the same reason the per-card buttons carry hand-written
 * ids (cohorts re-sort on every refresh, so ordinals shift).
 */
export function pastSessionRowId(claudeSessionId: string): string {
  return `terminal.past-session-row-${claudeSessionId}`;
}

/**
 * The restore verdict a row should DISPLAY, or `null` when there is nothing to
 * say.
 *
 * Two distinct silences collapse to `null`, and both are correct:
 *   - `undefined` — a runner build that predates the field. Absent evidence is
 *     UNKNOWN; rendering `not-restored` for it would invent a positive claim.
 *   - `"not-restored"` — the backend's explicit "no restore was ever recorded".
 *     True of nearly every row in a healthy list, so badging it would add a
 *     chip to every card while carrying no information.
 *
 * Everything else (`resumed`, `terminal-only`, `failed`, and the in-flight
 * `pending (not yet confirmed)`) is a real verdict about a restore that was
 * actually attempted, and is shown.
 *
 * Pure + exported: the runner's vitest env is `node`, so the display rule is
 * tested here rather than through a DOM.
 */
export function pastSessionRestoreBadge(restoreStatus: string | undefined): string | null {
  const trimmed = restoreStatus?.trim();
  if (!trimmed || trimmed === "not-restored") return null;
  return trimmed;
}

/**
 * Badge colour for a restore verdict: amber for anything unresolved or failed,
 * green for a landed restore. Keyed on the RENDERED verdict, which is why the
 * in-flight `pending (not yet confirmed)` is not painted like a failure.
 */
function restoreBadgeClass(verdict: string): string {
  if (verdict === "resumed") return "bg-[#9ece6a]/15 text-[#9ece6a]";
  return "bg-[#e0af68]/15 text-[#e0af68]";
}

/**
 * Stable control id for a cohort's "Show N more" / "Collapse" button. The same
 * button carries both states, so one id covers both — and the id is keyed on
 * the cohort rather than on the label, which changes as the count changes.
 */
export function pastSessionsCohortToggleId(cohortId: number): string {
  return `terminal.past-sessions-cohort-toggle-${cohortId}`;
}

interface PastSessionsViewProps {
  /**
   * Resume a past session by id — wired to the real terminal resume path in
   * `TerminalPage` (creates a tab and queues `claude --resume <id>` with the
   * session's config dir). Undefined ⇒ Resume falls back to copying the
   * command.
   */
  onResumePastSession?: (session: PastSession) => void;
}

/** A cohort: sessions sharing a `cohortId`, plus its derived header fields. */
export interface Cohort {
  cohortId: number;
  sessions: PastSession[];
  /** Earliest `lastSeenAt` in the cohort (the group's timestamp). */
  earliest: number;
  /** Latest `lastSeenAt` in the cohort (sort key, newest-first). */
  latest: number;
}

/**
 * Group past sessions by `cohortId`, newest cohort first.
 *
 * Exported for tests: the newest-first sort is exactly why per-card control
 * ids must be session-keyed rather than ordinal-derived.
 */
export function groupByCohort(sessions: PastSession[]): Cohort[] {
  const map = new Map<number, PastSession[]>();
  for (const s of sessions) {
    const list = map.get(s.cohortId);
    if (list) {
      list.push(s);
    } else {
      map.set(s.cohortId, [s]);
    }
  }

  const cohorts: Cohort[] = [];
  for (const [cohortId, list] of map.entries()) {
    let earliest = Infinity;
    let latest = -Infinity;
    for (const s of list) {
      if (s.lastSeenAt < earliest) earliest = s.lastSeenAt;
      if (s.lastSeenAt > latest) latest = s.lastSeenAt;
    }
    cohorts.push({ cohortId, sessions: list, earliest, latest });
  }

  // Newest cohort first (by its most-recent session).
  cohorts.sort((a, b) => b.latest - a.latest);
  return cohorts;
}

function formatWhen(ms: number): string {
  if (!ms || !Number.isFinite(ms)) return "unknown time";
  try {
    return new Date(ms).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  } catch {
    return "unknown time";
  }
}

function formatTimeAgo(ms: number): string {
  if (!ms || !Number.isFinite(ms)) return "";
  const diff = Date.now() - ms;
  if (diff < 0) return "just now";
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  return formatWhen(ms);
}

/** Shown when a session has no usable name from any source. */
export const UNNAMED_PAST_SESSION = "(unnamed session)";

/**
 * Headline name for one past-session card.
 *
 * `registryNames` is the live `sessionId → window name` map from
 * {@link useLiveClaudeSessionNames}, and the precedence is deliberately
 * asymmetric between live and closed rows:
 *
 * - **Live, operator-named** → the registry name wins. `resumeName` is scraped
 *   from the transcript and matched the real window name in only 11 of 33
 *   measured cases (2026-07-23), so where the two disagree the registry is
 *   right by construction.
 * - **Live, `nameSource: "derived"`** → miss. The map excludes derived rows, so
 *   Claude Code's `qontinui-root-ec` cwd slug can never displace a real name.
 * - **Closed** → normally a miss, so the row keeps exactly the `resumeName` it
 *   renders today. This function must never regress a closed row to blank.
 *
 *   The map is keyed by `sessionId`, NOT by process, so "the registry only
 *   describes running processes" does not by itself make every closed row a
 *   miss: `/resume` reuses the same `sessionId` under a new pid, so a card
 *   sitting in `state: "closed"` whose session was resumed in another window
 *   WILL hit and take that live window name. That outcome is intended — the
 *   live name is current and correct, and the guarantee this function actually
 *   makes is "never blank, never a placeholder where a real name existed",
 *   which still holds. Do not "fix" it by gating on `state`.
 *
 * Pure and exported so the contract is testable without a DOM (the runner's
 * vitest env is `node`).
 */
export function pastSessionDisplayName(
  session: Pick<PastSession, "claudeSessionId" | "resumeName">,
  registryNames: ReadonlyMap<string, string>,
): string {
  return registryNames.get(session.claudeSessionId) || session.resumeName || UNNAMED_PAST_SESSION;
}

/**
 * Tooltip for a past-session card: the headline, the session id, and — only
 * when the registry name displaced a DIFFERENT transcript name — that
 * transcript name too.
 *
 * The two disagreed in 22 of 33 measured cases, so promoting the registry name
 * without this would make `resumeName` unreachable from the UI. The line is
 * omitted when they agree so the common case stays a two-line tooltip.
 */
export function pastSessionTooltip(
  session: Pick<PastSession, "claudeSessionId" | "resumeName">,
  registryNames: ReadonlyMap<string, string>,
): string {
  const displayName = pastSessionDisplayName(session, registryNames);
  const base = `${displayName}\n${session.claudeSessionId}`;
  if (session.resumeName && session.resumeName !== displayName) {
    return `${base}\ntranscript name: ${session.resumeName}`;
  }
  return base;
}

/** One past-session card: headline name + badges + copy/resume actions. */
function PastSessionCard({
  session,
  registryNames,
  onResumePastSession,
}: {
  session: PastSession;
  /** Live window names by session id — see {@link pastSessionDisplayName}. */
  registryNames: ReadonlyMap<string, string>;
  onResumePastSession?: (session: PastSession) => void;
}) {
  const [copied, setCopied] = useState(false);
  const resumable = session.transcriptExists && session.restorable;

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(session.resumeCommand).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }, [session.resumeCommand]);

  const handleResume = useCallback(() => {
    if (onResumePastSession && resumable) {
      onResumePastSession(session);
    } else {
      // No real resume path, or transcript gone: copy the command so the
      // operator can still resume it by hand.
      navigator.clipboard.writeText(session.resumeCommand).catch(() => {});
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }
  }, [onResumePastSession, resumable, session]);

  const closed = session.state === "closed";
  const displayName = pastSessionDisplayName(session, registryNames);
  const restoreBadge = pastSessionRestoreBadge(session.restoreStatus);

  return (
    <div
      data-page-element={PAST_SESSION_CARD_ELEMENT}
      data-ui-bridge-id={pastSessionRowId(session.claudeSessionId)}
      data-session-id={session.claudeSessionId}
      className="group relative border-l-2 border-l-transparent hover:bg-[#1a1b26] px-3 py-2"
    >
      {/* Row 1: name + account badge */}
      <div className="flex items-center gap-1.5 mb-0.5">
        <span
          className="w-2 h-2 rounded-full shrink-0"
          style={{ backgroundColor: closed ? "#565f89" : "#9ece6a" }}
          title={
            closed ? `closed${session.closeReason ? ` — ${session.closeReason}` : ""}` : "open"
          }
        />
        <span
          className="text-xs text-[#c0caf5] font-medium truncate flex-1"
          title={pastSessionTooltip(session, registryNames)}
        >
          {displayName}
        </span>
        <span
          className="px-1 py-0 rounded text-[9px] font-medium shrink-0 bg-[#7aa2f7]/10 text-[#7aa2f7]"
          title={`account ${session.account.label} · wrapper ${session.account.wrapper}`}
        >
          {session.account.label}
          <span className="text-[#7aa2f7]/60"> · {session.account.wrapper}</span>
        </span>
      </div>

      {/* Row 2: page/zone · last active · state chip */}
      <div className="flex items-center gap-1 text-[10px] text-[#414868] ml-3.5 flex-wrap">
        <span title={`page ${session.pageId}`}>{session.pageId}</span>
        {session.zoneIndex >= 0 && (
          <>
            <span>&middot;</span>
            <span>zone {session.zoneIndex}</span>
          </>
        )}
        <span>&middot;</span>
        <span title={formatWhen(session.lastSeenAt)}>{formatTimeAgo(session.lastSeenAt)}</span>
        <span
          className={`px-1 rounded text-[9px] font-medium ${
            closed ? "bg-[#565f89]/15 text-[#565f89]" : "bg-[#9ece6a]/15 text-[#9ece6a]"
          }`}
        >
          {closed ? `closed${session.closeReason ? ` · ${session.closeReason}` : ""}` : "open"}
        </span>
        {restoreBadge && (
          <span
            className={`px-1 rounded text-[9px] font-medium ${restoreBadgeClass(restoreBadge)}`}
            title={`restore: ${restoreBadge}`}
          >
            {restoreBadge}
          </span>
        )}
      </div>

      {/* Row 3: resume command (mono, copyable reference) */}
      <div
        className="mt-1 ml-3.5 text-[10px] text-[#565f89] font-mono truncate leading-tight"
        title={session.resumeCommand}
      >
        {session.resumeCommand}
      </div>

      {/* Row 4: transcript-gone honesty note */}
      {!resumable && (
        <div className="mt-0.5 ml-3.5 flex items-center gap-1 text-[9px] text-[#e0af68]/80">
          <AlertTriangle className="w-2.5 h-2.5 shrink-0" />
          <span>transcript gone — copy only</span>
        </div>
      )}

      {/* Row 5: actions */}
      <div className="mt-1.5 ml-3.5 flex items-center gap-1.5">
        <button
          data-ui-bridge-id={pastSessionCopyId(session.claudeSessionId)}
          onClick={handleCopy}
          className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25 transition-colors"
          title="Copy the resume command to the clipboard"
        >
          {copied ? <Check className="w-3 h-3" /> : <Copy className="w-3 h-3" />}
          {copied ? "Copied!" : "Copy command"}
        </button>
        <button
          data-ui-bridge-id={pastSessionResumeId(session.claudeSessionId)}
          onClick={handleResume}
          disabled={!resumable}
          className={`flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium transition-colors ${
            resumable
              ? "bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25"
              : "bg-[#414868]/20 text-[#414868] cursor-not-allowed"
          }`}
          title={
            resumable
              ? "Resume this session in a new terminal tab"
              : "Transcript no longer on disk — use Copy command instead"
          }
        >
          <TerminalSquare className="w-3 h-3" />
          Resume
        </button>
      </div>
    </div>
  );
}

/**
 * The "Previous Sessions" surface: every session that was ever on this runner,
 * grouped by crash/close cohort, with its real `/rename` name and a one-click
 * resume + copy-command action. Additive to the live SessionManager — fed by
 * its own `usePastSessions` hook.
 */
export function PastSessionsView({ onResumePastSession }: PastSessionsViewProps) {
  const { sessions, loading, error, refresh: refreshSessions } = usePastSessions();
  // Live window names, for the subset of these rows whose process is still
  // running. Closed rows are always a miss and keep their `resumeName`.
  const registryNames = useLiveClaudeSessionNames();

  // Hide FINISHED sessions by default — the whole point of the marker is that a
  // finished session is one nobody needs to look at again. The toggle keeps them
  // DISCOVERABLE rather than merely absent: a hidden row the operator cannot
  // reach would trade one confusion for another, and the count below always
  // states how many are being hidden.
  const [showFinished, setShowFinished] = useState<boolean>(
    () => instanceStorage.getItem(SHOW_FINISHED_KEY) === "1",
  );
  const toggleShowFinished = useCallback(() => {
    setShowFinished((prev) => {
      const next = !prev;
      instanceStorage.setItem(SHOW_FINISHED_KEY, next ? "1" : "0");
      return next;
    });
  }, []);

  const finishedCount = useMemo(
    () => sessions.filter((s) => s.finished).length,
    [sessions],
  );
  const visibleSessions = useMemo(
    () => (showFinished ? sessions : sessions.filter((s) => !s.finished)),
    [sessions, showFinished],
  );
  const cohorts = useMemo(() => groupByCohort(visibleSessions), [visibleSessions]);

  // Refresh reloads BOTH halves of what a card shows. Reloading only the list
  // would leave the headline names up to a poll period stale — the operator
  // clicks Refresh precisely because they just renamed something.
  const refresh = useCallback(() => {
    refreshSessions();
    void refreshLiveClaudeSessionNames();
  }, [refreshSessions]);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const toggleExpand = useCallback((cohortId: number) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(cohortId)) next.delete(cohortId);
      else next.add(cohortId);
      return next;
    });
  }, []);

  return (
    <div data-page-element={PAST_SESSIONS_VIEW_ELEMENT} className="flex-1 flex flex-col min-h-0">
      {/* Sub-header: count + refresh */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d]">
        <Layers className="w-3 h-3 text-[#565f89]" />
        <span className="text-[10px] text-[#565f89] font-medium">
          {visibleSessions.length} previous session{visibleSessions.length !== 1 ? "s" : ""}
          {cohorts.length > 1 ? ` · ${cohorts.length} cohorts` : ""}
          {/* Name the hidden set. A filtered count presented as a total is the
              same defect class the backend guards against — the operator must
              be able to see that something is being withheld. */}
          {!showFinished && finishedCount > 0 ? ` · ${finishedCount} finished hidden` : ""}
        </span>
        <div className="flex-1" />
        {(finishedCount > 0 || showFinished) && (
          <button
            data-ui-bridge-id={PAST_SESSIONS_SHOW_FINISHED_ID}
            onClick={toggleShowFinished}
            className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              showFinished
                ? "text-[#c0caf5] bg-[#2a2d3d]"
                : "text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
            }`}
            title={
              showFinished
                ? "Hide sessions marked finished"
                : `Show ${finishedCount} session${finishedCount !== 1 ? "s" : ""} marked finished`
            }
          >
            {showFinished ? "Hide finished" : "Show finished"}
          </button>
        )}
        <button
          data-ui-bridge-id={PAST_SESSIONS_REFRESH_ID}
          onClick={refresh}
          disabled={loading}
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
          title="Refresh previous sessions"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {/* Body */}
      <div className="flex-1 overflow-y-auto scrollbar-dark">
        {loading && sessions.length === 0 ? (
          <div className="flex items-center justify-center py-8 text-[#565f89] text-xs">
            <div className="w-3 h-3 border-2 border-[#565f89] border-t-transparent rounded-full animate-spin mr-2" />
            Loading previous sessions...
          </div>
        ) : error ? (
          <div className="px-3 py-8 text-center text-[#f7768e] text-xs">
            <AlertTriangle className="w-4 h-4 mx-auto mb-2" />
            {error}
            <button
              data-ui-bridge-id={PAST_SESSIONS_RETRY_ID}
              onClick={refresh}
              className="mt-2 block mx-auto px-2 py-0.5 rounded text-[10px] bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 transition-colors"
              title="Retry loading previous sessions"
            >
              Retry
            </button>
          </div>
        ) : visibleSessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">
            {/* "Everything here is finished" and "there is nothing here" are
                DIFFERENT facts, and a filtered-empty list rendered as the
                latter would tell the operator their sessions are gone. */}
            {sessions.length === 0 ? (
              "No previous sessions found"
            ) : (
              <>
                All {sessions.length} previous session{sessions.length !== 1 ? "s are" : " is"}{" "}
                marked finished.
                <button
                  onClick={toggleShowFinished}
                  className="ml-1 underline hover:text-[#c0caf5] transition-colors"
                >
                  Show them
                </button>
              </>
            )}
          </div>
        ) : (
          cohorts.map((cohort) => {
            const isExpanded = expanded.has(cohort.cohortId);
            const visible = isExpanded
              ? cohort.sessions
              : cohort.sessions.slice(0, COHORT_RENDER_CAP);
            const hidden = cohort.sessions.length - visible.length;
            return (
              <div
                key={cohort.cohortId}
                data-page-element={PAST_SESSIONS_COHORT_ELEMENT}
                data-cohort-id={cohort.cohortId}
              >
                {/* Cohort header: "12 sessions · Jul 19, 12:19 AM" */}
                <div className="flex items-center gap-2 px-3 pt-3 pb-1">
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                  <span className="text-[10px] font-medium text-[#565f89] whitespace-nowrap">
                    {cohort.sessions.length} session{cohort.sessions.length !== 1 ? "s" : ""}
                    <span className="ml-1 text-[#414868]">
                      &middot; {formatWhen(cohort.earliest)}
                    </span>
                  </span>
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                </div>

                {visible.map((session) => (
                  <PastSessionCard
                    key={session.claudeSessionId}
                    session={session}
                    registryNames={registryNames}
                    onResumePastSession={onResumePastSession}
                  />
                ))}

                {/* One logical control, two labels — so both arms carry the
                    SAME cohort-keyed id (the auto-derived one would be minted
                    from "Show N more" and change with the count). */}
                {hidden > 0 && (
                  <button
                    data-ui-bridge-id={pastSessionsCohortToggleId(cohort.cohortId)}
                    onClick={() => toggleExpand(cohort.cohortId)}
                    className="w-full px-3 py-1.5 text-[10px] text-[#7aa2f7] hover:bg-[#1a1b26] transition-colors text-center"
                    title="Show the rest of this cohort's sessions"
                  >
                    Show {hidden} more
                  </button>
                )}
                {isExpanded && cohort.sessions.length > COHORT_RENDER_CAP && (
                  <button
                    data-ui-bridge-id={pastSessionsCohortToggleId(cohort.cohortId)}
                    onClick={() => toggleExpand(cohort.cohortId)}
                    className="w-full px-3 py-1.5 text-[10px] text-[#565f89] hover:bg-[#1a1b26] transition-colors text-center"
                    title="Collapse this cohort back to its first sessions"
                  >
                    Collapse
                  </button>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
