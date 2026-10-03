/**
 * Fan-out runs in the status strip (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phase 7).
 *
 * One pill per ACTIVE run — `run <slug> — 2 running · 5 queued · 1 refused
 * (low memory)` — expandable to its member list with Cancel-queued,
 * Release-slot and cap ± controls. When the scheduler route cannot be read the
 * strip says UNKNOWN with the error rather than rendering nothing, because
 * "no pill" would read as "no runs". Every count and label comes from
 * `fanoutStripModel.ts`.
 *
 * The data hook is owned by `StatusStrip` (so its auto-hide gate can see
 * whether this renders) and handed in.
 */

import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  Layers,
  Minus,
  Plus,
  RefreshCw,
} from "lucide-react";

import type { FanoutResult, FanoutRunView } from "./fanoutApi";
import {
  activeFanoutRuns,
  canReleaseMember,
  cancellableCount,
  cancelledNote,
  capClampNote,
  fanoutRunSummary,
  fanoutStripVisible,
  memberNumber,
  memberStateLabel,
  nextCap,
  unknownStripText,
} from "./fanoutStripModel";
import type { FanoutRunsApi } from "./useFanoutRuns";

const STATE_COLOR: Record<string, string> = {
  queued: "#e0af68",
  admitted: "#7aa2f7",
  released: "#565f89",
  cancelled: "#414868",
  refused: "#f7768e",
};

export function FanoutStrip({ api }: { api: FanoutRunsApi }) {
  const { state } = api;
  if (!fanoutStripVisible(state)) return null;

  if (state.kind === "unknown") {
    return (
      <span
        data-ui-bridge-id="terminal.fanout-strip"
        data-fanout-state="unknown"
        className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap text-[#e0af68]"
        data-fanout-unknown-code={state.code}
        title={`The fan-out scheduler could not be read${
          state.status !== null ? ` (HTTP ${state.status})` : ""
        }: ${state.error}`}
      >
        <AlertTriangle className="w-2.5 h-2.5" />
        <span>{unknownStripText(state.error)}</span>
        <button
          type="button"
          data-ui-bridge-id="terminal.fanout-strip-refresh"
          onClick={() => void api.refresh()}
          className="p-0.5 rounded hover:bg-white/5"
          title="Read the fan-out scheduler again"
        >
          <RefreshCw className="w-2.5 h-2.5" />
        </button>
      </span>
    );
  }

  if (state.kind !== "ok") return null;
  return (
    <span
      data-ui-bridge-id="terminal.fanout-strip"
      data-fanout-state="ok"
      className="flex items-center gap-1"
    >
      {activeFanoutRuns(state.runs).map((run) => (
        <FanoutRunPill key={run.id} run={run} api={api} />
      ))}
    </span>
  );
}

function FanoutRunPill({ run, api }: { run: FanoutRunView; api: FanoutRunsApi }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ text: string; error: boolean } | null>(null);
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const runOp = async (
    op: () => Promise<FanoutResult<FanoutRunView>>,
    okText: (after: FanoutRunView) => string,
  ) => {
    setBusy(true);
    setNote(null);
    try {
      const r = await op();
      setNote(r.ok ? { text: okText(r.data), error: false } : { text: r.error, error: true });
    } finally {
      setBusy(false);
    }
  };

  const changeCap = (delta: number) => {
    const requested = nextCap(run.maxConcurrent, delta);
    if (requested === run.maxConcurrent) return;
    void (async () => {
      setBusy(true);
      setNote(null);
      try {
        const r = await api.setCap(run.id, requested);
        if (!r.ok) setNote({ text: r.error, error: true });
        else {
          const clamp = capClampNote(requested, r.data);
          setNote({ text: clamp ?? `max concurrent ${r.data.run.maxConcurrent}`, error: false });
        }
      } finally {
        setBusy(false);
      }
    })();
  };

  const cancellable = cancellableCount(run);
  const summary = fanoutRunSummary(run);

  return (
    <span
      ref={ref}
      className="relative"
      data-ui-bridge-id="terminal.fanout-strip-run"
      data-run-id={run.id}
    >
      <button
        type="button"
        data-ui-bridge-id="terminal.fanout-strip-toggle"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap text-[#bb9af7] hover:bg-white/5 transition-colors"
        title={`${summary} — max ${run.maxConcurrent} at once, in ${run.workingDir}. Click for members.`}
        aria-expanded={open}
      >
        <Layers className="w-2.5 h-2.5" />
        <span>{summary}</span>
        {open ? <ChevronDown className="w-2.5 h-2.5" /> : <ChevronRight className="w-2.5 h-2.5" />}
      </button>
      {open && (
        <div
          role="dialog"
          aria-label={`Fan-out run ${run.templateSlug ?? run.id}`}
          data-ui-bridge-id="terminal.fanout-strip-panel"
          className="absolute left-0 top-full mt-1 w-[420px] max-w-[90vw] bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden"
        >
          <div className="flex items-center gap-2 px-3 py-2 border-b border-[#2a2d3d] text-[10px]">
            <span className="text-[#565f89] uppercase tracking-wider flex-1">
              Members ({run.members.length})
            </span>
            <span className="text-[#565f89]">max at once</span>
            <button
              type="button"
              data-ui-bridge-id="terminal.fanout-strip-cap-decrease"
              disabled={busy || run.maxConcurrent <= 1}
              onClick={() => changeCap(-1)}
              className="p-0.5 rounded text-[#a9b1d6] hover:bg-white/5 disabled:opacity-40"
              title="Lower max concurrent (admitted members keep running)"
            >
              <Minus className="w-3 h-3" />
            </button>
            <span
              data-ui-bridge-id="terminal.fanout-strip-cap-value"
              className="text-[#c0caf5] font-mono"
            >
              {run.maxConcurrent}
            </span>
            <button
              type="button"
              data-ui-bridge-id="terminal.fanout-strip-cap-increase"
              disabled={busy}
              onClick={() => changeCap(1)}
              className="p-0.5 rounded text-[#a9b1d6] hover:bg-white/5 disabled:opacity-40"
              title="Raise max concurrent (clamped to the tenant's fan-out bound)"
            >
              <Plus className="w-3 h-3" />
            </button>
            <button
              type="button"
              data-ui-bridge-id="terminal.fanout-strip-cancel-queued"
              disabled={busy || cancellable === 0}
              onClick={() =>
                void runOp(
                  () => api.cancelQueued(run.id),
                  (after) => cancelledNote(run, after),
                )
              }
              className="ml-1 px-1.5 py-0.5 rounded border border-[#2a2d3d] text-[#f7768e] hover:bg-[#f7768e]/10 disabled:opacity-40"
              title="Cancel every queued or refused member. Running sessions are never touched."
            >
              Cancel queued ({cancellable})
            </button>
          </div>
          <div className="max-h-64 overflow-y-auto scrollbar-dark">
            {run.members.map((m) => (
              <div
                key={m.index}
                data-ui-bridge-id="terminal.fanout-strip-member"
                data-member-index={m.index}
                data-member-state={m.state}
                className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2d3d]/50 last:border-b-0 text-[11px]"
              >
                <span className="text-[#565f89] font-mono w-6 shrink-0">#{memberNumber(m)}</span>
                <span className="flex-1 min-w-0 truncate text-[#c0caf5]" title={m.prompt}>
                  {m.title}
                </span>
                <span
                  className="shrink-0 text-[10px] truncate max-w-[150px]"
                  style={{ color: STATE_COLOR[m.state] ?? "#a9b1d6" }}
                  title={m.reason ?? m.state}
                >
                  {memberStateLabel(m)}
                </span>
                {canReleaseMember(m) && (
                  <button
                    type="button"
                    data-ui-bridge-id="terminal.fanout-strip-release"
                    data-member-index={m.index}
                    disabled={busy}
                    onClick={() =>
                      void runOp(
                        () => api.release(run.id, m.index),
                        () => `released #${memberNumber(m)}`,
                      )
                    }
                    className="shrink-0 px-1.5 py-0.5 rounded border border-[#2a2d3d] text-[10px] text-[#7aa2f7] hover:bg-[#7aa2f7]/10 disabled:opacity-40"
                    title="Free this member's slot so the next queued member is admitted. The session itself keeps running."
                  >
                    Release slot
                  </button>
                )}
              </div>
            ))}
          </div>
          {note && (
            <div
              data-ui-bridge-id="terminal.fanout-strip-note"
              className={`px-3 py-1.5 text-[10px] border-t border-[#2a2d3d] ${
                note.error ? "text-[#f7768e]" : "text-[#9ece6a]"
              }`}
            >
              {note.text}
            </div>
          )}
        </div>
      )}
    </span>
  );
}
