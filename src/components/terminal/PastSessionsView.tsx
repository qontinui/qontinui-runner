import { useMemo, useState, useCallback } from "react";
import { TerminalSquare, Copy, Check, RefreshCw, Layers, AlertTriangle } from "lucide-react";
import { usePastSessions, type PastSession } from "./usePastSessions";

/**
 * Max cards rendered per cohort before a "Show N more" expander. A single crash
 * cohort can hold 80+ stranded sessions; rendering every card eagerly across
 * several cohorts is what would jank the panel, so each cohort renders a capped
 * window and reveals the rest on demand. Lightweight cap-render rather than a
 * full windowing dependency — the cohort count is small and each card is cheap.
 */
const COHORT_RENDER_CAP = 30;

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
interface Cohort {
  cohortId: number;
  sessions: PastSession[];
  /** Earliest `lastSeenAt` in the cohort (the group's timestamp). */
  earliest: number;
  /** Latest `lastSeenAt` in the cohort (sort key, newest-first). */
  latest: number;
}

/** Group past sessions by `cohortId`, newest cohort first. */
function groupByCohort(sessions: PastSession[]): Cohort[] {
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

/** One past-session card: headline name + badges + copy/resume actions. */
function PastSessionCard({
  session,
  onResumePastSession,
}: {
  session: PastSession;
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

  return (
    <div className="group relative border-l-2 border-l-transparent hover:bg-[#1a1b26] px-3 py-2">
      {/* Row 1: name + account badge */}
      <div className="flex items-center gap-1.5 mb-0.5">
        <span
          className="w-2 h-2 rounded-full shrink-0"
          style={{ backgroundColor: closed ? "#565f89" : "#9ece6a" }}
          title={closed ? `closed${session.closeReason ? ` — ${session.closeReason}` : ""}` : "open"}
        />
        <span
          className="text-xs text-[#c0caf5] font-medium truncate flex-1"
          title={`${session.resumeName}\n${session.claudeSessionId}`}
        >
          {session.resumeName || "(unnamed session)"}
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
          onClick={handleCopy}
          className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25 transition-colors"
          title="Copy the resume command to the clipboard"
        >
          {copied ? <Check className="w-3 h-3" /> : <Copy className="w-3 h-3" />}
          {copied ? "Copied!" : "Copy command"}
        </button>
        <button
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
  const { sessions, loading, error, refresh } = usePastSessions();
  const cohorts = useMemo(() => groupByCohort(sessions), [sessions]);
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
    <div className="flex-1 flex flex-col min-h-0">
      {/* Sub-header: count + refresh */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d]">
        <Layers className="w-3 h-3 text-[#565f89]" />
        <span className="text-[10px] text-[#565f89] font-medium">
          {sessions.length} previous session{sessions.length !== 1 ? "s" : ""}
          {cohorts.length > 1 ? ` · ${cohorts.length} cohorts` : ""}
        </span>
        <div className="flex-1" />
        <button
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
              onClick={refresh}
              className="mt-2 block mx-auto px-2 py-0.5 rounded text-[10px] bg-[#f7768e]/15 text-[#f7768e] hover:bg-[#f7768e]/25 transition-colors"
            >
              Retry
            </button>
          </div>
        ) : sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-[#565f89] text-xs">
            No previous sessions found
          </div>
        ) : (
          cohorts.map((cohort) => {
            const isExpanded = expanded.has(cohort.cohortId);
            const visible = isExpanded
              ? cohort.sessions
              : cohort.sessions.slice(0, COHORT_RENDER_CAP);
            const hidden = cohort.sessions.length - visible.length;
            return (
              <div key={cohort.cohortId}>
                {/* Cohort header: "12 sessions · Jul 19, 12:19 AM" */}
                <div className="flex items-center gap-2 px-3 pt-3 pb-1">
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                  <span className="text-[10px] font-medium text-[#565f89] whitespace-nowrap">
                    {cohort.sessions.length} session{cohort.sessions.length !== 1 ? "s" : ""}
                    <span className="ml-1 text-[#414868]">&middot; {formatWhen(cohort.earliest)}</span>
                  </span>
                  <div className="flex-1 h-px bg-[#2a2d3d]" />
                </div>

                {visible.map((session) => (
                  <PastSessionCard
                    key={session.claudeSessionId}
                    session={session}
                    onResumePastSession={onResumePastSession}
                  />
                ))}

                {hidden > 0 && (
                  <button
                    onClick={() => toggleExpand(cohort.cohortId)}
                    className="w-full px-3 py-1.5 text-[10px] text-[#7aa2f7] hover:bg-[#1a1b26] transition-colors text-center"
                  >
                    Show {hidden} more
                  </button>
                )}
                {isExpanded && cohort.sessions.length > COHORT_RENDER_CAP && (
                  <button
                    onClick={() => toggleExpand(cohort.cohortId)}
                    className="w-full px-3 py-1.5 text-[10px] text-[#565f89] hover:bg-[#1a1b26] transition-colors text-center"
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
