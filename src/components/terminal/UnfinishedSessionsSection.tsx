import { useCallback, useMemo, useState } from "react";
import { AlertTriangle, RotateCcw } from "lucide-react";
import type { PastSession } from "./usePastSessions";
import {
  finishMarkReported,
  resumeUnfinished,
  unfinishedReadState,
  unfinishedSessions,
  type ResumeVerdict,
} from "./unfinishedSessions";

export const UNFINISHED_SECTION_ELEMENT = "unfinished-sessions";
export const UNFINISHED_RESUME_ALL_ID = "terminal.unfinished-resume-all";
export const unfinishedResumeId = (claudeSessionId: string) =>
  `terminal.unfinished-resume.${claudeSessionId}`;

interface Props {
  sessions: readonly PastSession[];
  loaded: boolean;
  error: string | null;
  /** Re-read history after a resume attempt settles. */
  onSettled: () => void;
}

/**
 * "Unfinished" view: closed sessions on this device whose work was never marked
 * finished, each with a Resume that calls the backend door and shows its
 * per-id verdict. History unread/errored renders as UNKNOWN, never as "none".
 */
export function UnfinishedSessionsSection({ sessions, loaded, error, onSettled }: Props) {
  const rows = useMemo(() => unfinishedSessions(sessions), [sessions]);
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
              <button
                data-ui-bridge-id={UNFINISHED_RESUME_ALL_ID}
                disabled={busy.size > 0}
                onClick={() => void run(rows.map((r) => r.claudeSessionId))}
                className="mb-1 px-2 py-0.5 rounded text-[10px] bg-[#2a2d3d] text-[#c0caf5] disabled:opacity-50"
              >
                Resume all ({rows.length})
              </button>
              <ul className="space-y-1">
                {rows.map((r) => {
                  const id = r.claudeSessionId;
                  const v = verdicts.get(id);
                  return (
                    <li
                      key={id}
                      className="flex items-center gap-2"
                      data-page-element="unfinished-session-row"
                    >
                      <span className="flex-1 truncate text-[#c0caf5]" title={r.workingDir ?? id}>
                        {r.resumeName || id}
                        {!finishMarkReported(r) && (
                          <span className="ml-1 text-[#565f89]">(finish mark not reported)</span>
                        )}
                      </span>
                      {v && (
                        <span
                          className={
                            v.outcome === "resumed"
                              ? "text-[#9ece6a]"
                              : v.outcome === "failed"
                                ? "text-[#f7768e]"
                                : "text-[#e0af68]"
                          }
                        >
                          {v.verdict}
                        </span>
                      )}
                      <button
                        data-ui-bridge-id={unfinishedResumeId(id)}
                        disabled={busy.has(id)}
                        onClick={() => void run([id])}
                        className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] disabled:opacity-50"
                        title="Resume this session"
                      >
                        <RotateCcw className="w-3 h-3" />
                      </button>
                    </li>
                  );
                })}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}
