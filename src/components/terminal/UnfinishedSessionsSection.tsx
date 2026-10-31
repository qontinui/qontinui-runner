import { useCallback, useMemo, useState } from "react";
import { AlertTriangle, RotateCcw } from "lucide-react";
import type { PastSession } from "./usePastSessions";
import {
  finishMarkReported,
  resumableIds,
  resumeUnfinished,
  unfinishedReadState,
  unfinishedSessions,
  type ResumeVerdict,
} from "./unfinishedSessions";

export const UNFINISHED_SECTION_ELEMENT = "unfinished-sessions";
export const UNFINISHED_RESUME_ALL_ID = "terminal.unfinished-resume-all";
export const unfinishedResumeId = (claudeSessionId: string) =>
  `terminal.unfinished-resume.${claudeSessionId}`;
/** Stamps the row so what it says (its verdict included) reaches the UI Bridge snapshot. */
export const unfinishedRowId = (claudeSessionId: string) =>
  `terminal.unfinished-row.${claudeSessionId}`;

interface Props {
  sessions: readonly PastSession[];
  loaded: boolean;
  error: string | null;
  /** Why a row cannot be resumed, or `null`. The same rule a past-session card applies. */
  resumeBlockerOf: (s: PastSession) => string | null;
  /** Re-read history after a resume attempt settles. */
  onSettled: () => void;
}

interface RowProps {
  session: PastSession;
  blocker: string | null;
  verdict: ResumeVerdict | undefined;
  busy: boolean;
  onResume: () => void;
}

export function UnfinishedSessionRow({ session, blocker, verdict, busy, onResume }: RowProps) {
  const id = session.claudeSessionId;
  return (
    <li
      className="flex items-center gap-2"
      data-page-element="unfinished-session-row"
      data-ui-bridge-id={unfinishedRowId(id)}
    >
      <span className="flex-1 truncate text-[#c0caf5]" title={session.workingDir ?? id}>
        {session.resumeName || id}
        {!finishMarkReported(session) && (
          <span className="ml-1 text-[#565f89]">(finish mark not reported)</span>
        )}
      </span>
      {verdict && (
        <span
          className={
            verdict.outcome === "resumed"
              ? "text-[#9ece6a]"
              : verdict.outcome === "failed"
                ? "text-[#f7768e]"
                : "text-[#e0af68]"
          }
        >
          {verdict.verdict}
        </span>
      )}
      <button
        data-ui-bridge-id={unfinishedResumeId(id)}
        disabled={busy || blocker !== null}
        onClick={onResume}
        className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] disabled:opacity-50"
        title={blocker ?? "Resume this session"}
      >
        <RotateCcw className="w-3 h-3" />
      </button>
    </li>
  );
}

/**
 * "Unfinished" view: closed sessions on this device whose work was never marked
 * finished, each with a Resume that calls the backend door and shows its
 * per-id verdict. History unread/errored renders as UNKNOWN, never as "none".
 */
export function UnfinishedSessionsSection({
  sessions,
  loaded,
  error,
  resumeBlockerOf,
  onSettled,
}: Props) {
  const rows = useMemo(() => unfinishedSessions(sessions), [sessions]);
  const resumable = useMemo(() => resumableIds(rows, resumeBlockerOf), [rows, resumeBlockerOf]);
  const state = unfinishedReadState({ loaded, error, rows });
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const [verdicts, setVerdicts] = useState<ReadonlyMap<string, ResumeVerdict>>(new Map());

  const run = useCallback(
    async (ids: string[]) => {
      setBusy((p) => new Set([...p, ...ids]));
      const result = await resumeUnfinished(ids);
      setVerdicts((p) => {
        const n = new Map(p);
        for (const v of result) n.set(v.id, v);
        return n;
      });
      setBusy((p) => {
        const n = new Set(p);
        for (const id of ids) n.delete(id);
        return n;
      });
      onSettled();
    },
    [onSettled],
  );

  const label = state === "unknown" ? "Unfinished (unknown)" : `Unfinished (${rows.length})`;

  return (
    <div data-page-element={UNFINISHED_SECTION_ELEMENT} className="border-b border-[#2a2d3d]">
      <button
        data-ui-bridge-id="terminal.unfinished-toggle"
        onClick={() => setOpen((o) => !o)}
        className="w-full text-left px-3 py-1.5 text-[10px] font-medium text-[#565f89] hover:text-[#c0caf5]"
      >
        {open ? "▾" : "▸"} {label}
      </button>
      {open && (
        <div className="px-3 pb-2 text-xs">
          {state === "unknown" && (
            <div className="text-[#e0af68]">
              <AlertTriangle className="inline w-3 h-3 mr-1" />
              Unfinished sessions could not be read{error ? `: ${error}` : " yet"}. This is not the
              same as there being none.
            </div>
          )}
          {state === "empty" && (
            <div className="text-[#565f89]">
              No closed sessions with unfinished work on this device.
            </div>
          )}
          {state === "rows" && (
            <>
              <div className="mb-1 flex items-center gap-2">
                <button
                  data-ui-bridge-id={UNFINISHED_RESUME_ALL_ID}
                  disabled={busy.size > 0 || resumable.length === 0}
                  onClick={() => void run(resumable)}
                  className="px-2 py-0.5 rounded text-[10px] bg-[#2a2d3d] text-[#c0caf5] disabled:opacity-50"
                >
                  Resume all ({resumable.length})
                </button>
                {/* Name what the bulk action leaves out. */}
                {resumable.length < rows.length && (
                  <span className="text-[10px] text-[#565f89]">
                    {rows.length - resumable.length} cannot be resumed
                  </span>
                )}
              </div>
              <ul className="space-y-1">
                {rows.map((r) => (
                  <UnfinishedSessionRow
                    key={r.claudeSessionId}
                    session={r}
                    blocker={resumeBlockerOf(r)}
                    verdict={verdicts.get(r.claudeSessionId)}
                    busy={busy.has(r.claudeSessionId)}
                    onResume={() => void run([r.claudeSessionId])}
                  />
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}
