/**
 * `WorkerSessionCell` — the grid cell for a Conductor worker.
 *
 * A worker spawned by `/orchestrate` (`dispatch_subtask` in
 * `orchestration_loop/ai_session_executor.rs`) is an in-process stream-json
 * `ClaudeSession` registered in the Rust `SessionManager`. It has NO terminal
 * process: `TerminalInstance` would attach to nothing and render a silent
 * pane that looks like a terminal with nothing to say. This cell renders what
 * the worker actually is —
 *
 * - **Conversation**: the transcript so far (`get_ai_output`) plus the
 *   in-flight tail, subscribed through `useAiSession` (`ai-output` /
 *   `claude-session-state`) and bounded by `StreamingMessageView`.
 * - **Changes**: a per-file diff of the worker's PRE-EDIT snapshot
 *   (`capture_pre_edit_snapshot`, taken the first time it touched the path,
 *   whatever tool made the edit) against the file as it is now, refreshed on
 *   the `commit-state-changed` event the edit hook already emits and again
 *   when a turn ends.
 * - **Steering**: an input that sends through `send_user_message`, the same
 *   command the Process Manager uses. `ClaudeSession::send_user_message` sends
 *   immediately when the worker is `Ready` and queues otherwise; the ledger
 *   under the input says which happened, and flips a queued entry to
 *   "delivered" only when the `ready → processing` transition that drains the
 *   queue is actually observed.
 *
 * Honesty (served `ux-priorities`): a read that fails renders UNKNOWN with
 * the failure named — never an empty transcript, never an empty change list,
 * never "closed".
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { RefreshCw, Send, Square } from "lucide-react";
import type { AiMessage, AiSessionState } from "@qontinui/shared-types";
import { cn } from "@/lib/utils";
import { useAiSession, type SessionReadStatus } from "@/hooks/useAiSession";
import { StreamingMessageView } from "../shared/StreamingMessageView";
import type { TerminalTab } from "./useTerminalManager";
import {
  countChanged,
  diffHunks,
  diffStat,
  fetchSessionFileChanges,
  noDiffReason,
  orderChanges,
  shortPath,
  type DiffHunk,
  type FileChangesRead,
  type SessionFileChange,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

// ── Pure presentation logic (exported for tests) ──────────────────────────────

export type WorkerTone = "starting" | "idle" | "busy" | "done" | "error" | "unknown";

export interface WorkerStateDescription {
  label: string;
  tone: WorkerTone;
}

/**
 * What the header badge says. A read that has not settled, or that failed,
 * is UNKNOWN regardless of the state value the hook is holding.
 */
export function describeWorkerState(
  state: AiSessionState,
  readStatus: SessionReadStatus,
): WorkerStateDescription {
  if (readStatus === "pending") return { label: "attaching…", tone: "unknown" };
  if (readStatus === "failed") return { label: "UNKNOWN", tone: "unknown" };
  switch (state) {
    case "connecting":
    case "initializing":
      return { label: "starting", tone: "starting" };
    case "ready":
      return { label: "idle — waiting for input", tone: "idle" };
    case "processing":
      return { label: "working", tone: "busy" };
    case "interrupting":
      return { label: "interrupting", tone: "busy" };
    case "restoring":
      return { label: "restoring", tone: "starting" };
    case "closed":
      return { label: "finished", tone: "done" };
    case "not_found":
      return { label: "not registered", tone: "done" };
    case "error":
      return { label: "error", tone: "error" };
    case "disconnected":
    default:
      return { label: "disconnected", tone: "unknown" };
  }
}

export type SteeringDelivery = "sending" | "sent" | "queued" | "delivered" | "failed";

export interface SteeringEntry {
  id: string;
  text: string;
  atMs: number;
  delivery: SteeringDelivery;
  error?: string;
}

/**
 * The backend drains a queued message the moment the current turn ends:
 * `Ready` is reached, the queued text is written, and the session goes
 * straight back to `Processing`. That `ready → processing` edge is the only
 * evidence the frontend has of delivery, so a queued entry flips to
 * `delivered` on that edge and on nothing else. Pure.
 */
export function settleQueuedOnTransition(
  entries: readonly SteeringEntry[],
  prev: AiSessionState,
  next: AiSessionState,
): SteeringEntry[] {
  if (!(prev === "ready" && next === "processing")) return [...entries];
  if (!entries.some((e) => e.delivery === "queued")) return [...entries];
  return entries.map((e) => (e.delivery === "queued" ? { ...e, delivery: "delivered" } : e));
}

export const STEERING_LEDGER_ROWS = 3;

export function deliveryLabel(entry: SteeringEntry): string {
  switch (entry.delivery) {
    case "sending":
      return "sending…";
    case "sent":
      return "sent";
    case "queued":
      return "queued — delivered when the current turn ends";
    case "delivered":
      return "delivered";
    case "failed":
      return `failed: ${entry.error ?? "unknown error"}`;
    default:
      return entry.delivery;
  }
}

function formatClock(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(
    d.getSeconds(),
  ).padStart(2, "0")}`;
}

// ── Pure components ───────────────────────────────────────────────────────────

const TONE_CLASSES: Record<WorkerTone, string> = {
  starting: "bg-sky-500/15 text-sky-300 border-sky-500/30",
  idle: "bg-emerald-500/15 text-emerald-300 border-emerald-500/30",
  busy: "bg-amber-500/15 text-amber-300 border-amber-500/30",
  done: "bg-zinc-500/15 text-zinc-300 border-zinc-500/30",
  error: "bg-red-500/15 text-red-300 border-red-500/30",
  unknown: "bg-fuchsia-500/15 text-fuchsia-300 border-fuchsia-500/30",
};

export function WorkerStateBadge({
  state,
  readStatus,
  lastReadError,
}: {
  state: AiSessionState;
  readStatus: SessionReadStatus;
  lastReadError: string | null;
}) {
  const { label, tone } = describeWorkerState(state, readStatus);
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded border px-1.5 py-px text-[10px] font-medium",
        TONE_CLASSES[tone],
      )}
      title={readStatus === "failed" && lastReadError ? lastReadError : undefined}
      data-worker-state={readStatus === "failed" ? "unknown" : state}
    >
      {tone === "busy" && <span className="h-1.5 w-1.5 rounded-full bg-current animate-pulse" />}
      {label}
    </span>
  );
}

export function SteeringLedger({ entries }: { entries: readonly SteeringEntry[] }) {
  if (entries.length === 0) return null;
  const shown = entries.slice(-STEERING_LEDGER_ROWS);
  return (
    <ul className="flex flex-col gap-0.5 px-2 pb-1 text-[10px] text-zinc-500" data-steering-ledger>
      {shown.map((e) => (
        <li key={e.id} className="flex items-baseline gap-1.5 truncate" data-delivery={e.delivery}>
          <span className="shrink-0 text-zinc-600">{formatClock(e.atMs)}</span>
          <span className="truncate text-zinc-400">{e.text}</span>
          <span
            className={cn(
              "shrink-0",
              e.delivery === "failed" && "text-red-400",
              e.delivery === "queued" && "text-amber-400",
              (e.delivery === "sent" || e.delivery === "delivered") && "text-emerald-400",
            )}
          >
            {deliveryLabel(e)}
          </span>
        </li>
      ))}
    </ul>
  );
}

export function DiffView({ hunks }: { hunks: DiffHunk[] }) {
  return (
    <pre className="m-0 overflow-x-auto whitespace-pre font-mono text-[10px] leading-4">
      {hunks.map((hunk, hi) => (
        <div key={hi}>
          <div className="text-[#7aa2f7]/80">{hunk.header}</div>
          {hunk.lines.map((line, li) => (
            <div
              key={li}
              className={cn(
                line.kind === "add" && "bg-emerald-500/10 text-emerald-300",
                line.kind === "del" && "bg-red-500/10 text-red-300",
                line.kind === "ctx" && "text-zinc-500",
              )}
            >
              {line.kind === "add" ? "+" : line.kind === "del" ? "-" : " "}
              {line.text}
            </div>
          ))}
        </div>
      ))}
    </pre>
  );
}

const STATUS_LABEL: Record<SessionFileChange["status"], string> = {
  modified: "modified",
  created: "created",
  deleted: "deleted",
  unchanged: "unchanged",
  binary: "binary",
  unreadable: "UNKNOWN",
};

export function FileChangeRow({ change }: { change: SessionFileChange }) {
  const hunks = useMemo(() => diffHunks(change), [change]);
  const stat = useMemo(() => diffStat(hunks), [hunks]);
  const reason = noDiffReason(change);
  const [open, setOpen] = useState(false);
  const expandable = hunks !== null && hunks.length > 0;
  return (
    <li
      className="border-b border-[#2a2d3d] last:border-b-0"
      data-file-change={change.status}
      data-file-path={change.filePath}
    >
      <button
        type="button"
        onClick={() => expandable && setOpen((v) => !v)}
        className={cn(
          "flex w-full items-center gap-2 px-2 py-1 text-left text-[11px]",
          expandable ? "hover:bg-white/5 cursor-pointer" : "cursor-default",
        )}
        title={change.filePath}
        aria-expanded={expandable ? open : undefined}
      >
        <span
          className={cn(
            "shrink-0 rounded px-1 text-[9px] uppercase tracking-wide",
            change.status === "modified" && "bg-amber-500/15 text-amber-300",
            change.status === "created" && "bg-emerald-500/15 text-emerald-300",
            change.status === "deleted" && "bg-red-500/15 text-red-300",
            change.status === "unchanged" && "bg-zinc-500/15 text-zinc-400",
            change.status === "binary" && "bg-zinc-500/15 text-zinc-400",
            change.status === "unreadable" && "bg-fuchsia-500/15 text-fuchsia-300",
          )}
        >
          {STATUS_LABEL[change.status]}
        </span>
        <span className="truncate font-mono text-[#a9b1d6]">{shortPath(change.filePath)}</span>
        {expandable && (
          <span className="ml-auto shrink-0 font-mono text-[10px]">
            <span className="text-emerald-400">+{stat.additions}</span>{" "}
            <span className="text-red-400">-{stat.deletions}</span>
          </span>
        )}
        {reason && <span className="ml-auto shrink-0 text-[10px] text-zinc-500">{reason}</span>}
      </button>
      {open && hunks && (
        <div className="max-h-64 overflow-y-auto border-t border-[#2a2d3d] bg-black/20 px-2 py-1">
          <DiffView hunks={hunks} />
        </div>
      )}
    </li>
  );
}

function ChangeList({ response }: { response: SessionFileChangesResponse }) {
  if (response.files.length === 0) {
    return (
      <div className="px-2 py-3 text-[11px] text-zinc-500">
        No snapshotted edits yet — the worker has not written to any file the runner saw.
      </div>
    );
  }
  return (
    <ul className="m-0 list-none p-0">
      {orderChanges(response.files).map((change) => (
        <FileChangeRow key={change.filePath} change={change} />
      ))}
    </ul>
  );
}

export function FileChangesPanel({
  read,
  onRefresh,
}: {
  read: FileChangesRead;
  onRefresh: () => void;
}) {
  // During an ordinary refresh the previous list stays up unlabelled (the
  // header already says "reading…"); only a FAILED read marks it stale.
  const shown = read.status === "ok" ? read.response : read.status === "loading" ? read.previous : null;
  const stale = read.status === "error" ? read.previous : null;
  return (
    <div className="flex h-full min-h-0 flex-col" data-file-changes-status={read.status}>
      <div className="flex items-center gap-2 border-b border-[#2a2d3d] px-2 py-1 text-[10px] text-zinc-500">
        {read.status === "ok" && (
          <span>
            {countChanged(read.response.files)} changed · read {formatClock(read.response.readAtMs)}
          </span>
        )}
        {read.status === "loading" && <span>reading…</span>}
        {read.status === "error" && (
          <span className="text-fuchsia-300" title={read.error}>
            UNKNOWN — change list could not be read at {formatClock(read.atMs)}: {read.error}
          </span>
        )}
        <button
          type="button"
          onClick={onRefresh}
          className="ml-auto inline-flex items-center gap-1 rounded px-1 hover:bg-white/5 hover:text-zinc-300"
          title="Re-read the worker's file changes"
        >
          <RefreshCw className="h-3 w-3" />
          refresh
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {shown && <ChangeList response={shown} />}
        {stale && (
          <>
            <div className="px-2 pt-1 text-[10px] text-zinc-600">
              last successful read {formatClock(stale.readAtMs)} — may be stale
            </div>
            <ChangeList response={stale} />
          </>
        )}
        {read.status === "error" && !stale && (
          <div className="px-2 py-3 text-[11px] text-fuchsia-300">
            UNKNOWN — nothing has been read successfully for this worker yet.
          </div>
        )}
      </div>
    </div>
  );
}

export function ConversationView({
  messages,
  streamingContent,
  streamingDroppedChars,
  isProcessing,
  toolActivity,
  readStatus,
  lastReadError,
}: {
  messages: readonly AiMessage[];
  streamingContent: string;
  streamingDroppedChars: number;
  isProcessing: boolean;
  toolActivity: string | null;
  readStatus: SessionReadStatus;
  lastReadError: string | null;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const stickRef = useRef(true);
  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  }, []);
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, [messages.length, streamingContent.length, toolActivity]);

  const nothingYet = messages.length === 0 && !streamingContent;
  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      className="min-h-0 flex-1 space-y-2 overflow-y-auto px-2 py-2"
      data-conversation-read={readStatus}
    >
      {readStatus === "failed" && (
        <div className="rounded border border-fuchsia-500/30 bg-fuchsia-500/10 px-2 py-1 text-[11px] text-fuchsia-300">
          UNKNOWN — this worker&apos;s state and transcript could not be read
          {lastReadError ? `: ${lastReadError}` : ""}. Live output still streams in if the worker is
          alive.
        </div>
      )}
      {readStatus === "pending" && nothingYet && (
        <div className="px-1 py-2 text-[11px] text-zinc-500">attaching to the worker session…</div>
      )}
      {readStatus === "ok" && nothingYet && !isProcessing && (
        <div className="px-1 py-2 text-[11px] text-zinc-500">
          No transcript yet — the worker has not produced output.
        </div>
      )}
      {messages.map((msg, i) =>
        msg.role === "ai" ? (
          <div
            key={`ai-${i}`}
            className="prose prose-invert prose-sm max-w-none text-xs [&_pre]:bg-black/30 [&_pre]:rounded-md [&_pre]:p-2 [&_pre]:overflow-x-auto [&_code]:text-amber-300 [&_code]:text-xs [&_a]:text-cyan-400 [&_h1]:text-sm [&_h2]:text-sm [&_h3]:text-xs [&_p]:text-xs [&_li]:text-xs [&_p]:my-1 [&_ul]:my-1 [&_ol]:my-1"
          >
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
          </div>
        ) : msg.role === "system" ? (
          <div key={`sys-${i}`} className="text-[10px] italic text-zinc-500">
            {msg.content}
          </div>
        ) : (
          <div key={`user-${i}`} className="flex justify-end">
            <div className="max-w-[85%] whitespace-pre-wrap rounded-md border border-cyan-700/30 bg-cyan-900/30 px-2 py-1 text-xs text-zinc-300">
              {msg.content}
            </div>
          </div>
        ),
      )}
      {isProcessing && streamingContent && (
        <StreamingMessageView
          content={streamingContent}
          droppedChars={streamingDroppedChars}
          className="text-zinc-300"
          showCaret
        />
      )}
      {isProcessing && toolActivity && (
        <div className="flex items-center gap-1.5 text-[10px] text-amber-300/90">
          <span className="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse" />
          <span className="truncate">{toolActivity}</span>
        </div>
      )}
      {isProcessing && !streamingContent && !toolActivity && (
        <div className="flex items-center gap-1.5 py-1">
          <span className="h-1.5 w-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:0ms]" />
          <span className="h-1.5 w-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:150ms]" />
          <span className="h-1.5 w-1.5 rounded-full bg-amber-400 animate-bounce [animation-delay:300ms]" />
        </div>
      )}
    </div>
  );
}

// ── Data hooks ────────────────────────────────────────────────────────────────

/** Debounce for `commit-state-changed` bursts (one per Edit/Write hook). */
const CHANGES_REFRESH_DEBOUNCE_MS = 750;

function useWorkerFileChanges(taskRunId: string, sessionState: AiSessionState) {
  const [read, setRead] = useState<FileChangesRead>({ status: "loading", previous: null });
  const latestRef = useRef<SessionFileChangesResponse | null>(null);
  const inFlightRef = useRef<AbortController | null>(null);

  const refresh = useCallback(() => {
    inFlightRef.current?.abort();
    const ctrl = new AbortController();
    inFlightRef.current = ctrl;
    setRead({ status: "loading", previous: latestRef.current });
    fetchSessionFileChanges(taskRunId, ctrl.signal)
      .then((response) => {
        if (ctrl.signal.aborted) return;
        latestRef.current = response;
        setRead({ status: "ok", response });
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return;
        setRead({
          status: "error",
          error: err instanceof Error ? err.message : String(err),
          atMs: Date.now(),
          previous: latestRef.current,
        });
      });
  }, [taskRunId]);

  // First read on mount / id change.
  useEffect(() => {
    refresh();
    return () => inFlightRef.current?.abort();
  }, [refresh]);

  // Edit-time refresh: the dispatcher emits `commit-state-changed` for the
  // session after every Edit/Write hook (`dispatcher.rs`), debounced here so
  // a burst of edits costs one read.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<{ task_run_id?: string }>("commit-state-changed", (event) => {
      if (event.payload?.task_run_id !== taskRunId) return;
      if (timer) clearTimeout(timer);
      timer = setTimeout(refresh, CHANGES_REFRESH_DEBOUNCE_MS);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      unlisten?.();
    };
  }, [taskRunId, refresh]);

  // Turn-end refresh: catches edits made by a tool the hook did not see.
  const prevStateRef = useRef<AiSessionState>(sessionState);
  useEffect(() => {
    const prev = prevStateRef.current;
    prevStateRef.current = sessionState;
    if (prev === "processing" && (sessionState === "ready" || sessionState === "closed")) {
      refresh();
    }
  }, [sessionState, refresh]);

  return { read, refresh };
}

// ── The cell ──────────────────────────────────────────────────────────────────

export interface WorkerSessionCellProps {
  tab: TerminalTab;
  /** The worker's task run id (`tab.taskRunId`), passed explicitly so the type says it is present. */
  taskRunId: string;
  visible: boolean;
}

type CellPane = "conversation" | "changes";

export function WorkerSessionCell({ tab, taskRunId, visible }: WorkerSessionCellProps) {
  const session = useAiSession({ attachTo: taskRunId });
  const { read: changesRead, refresh: refreshChanges } = useWorkerFileChanges(
    taskRunId,
    session.sessionState,
  );
  const [pane, setPane] = useState<CellPane>("conversation");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [ledger, setLedger] = useState<SteeringEntry[]>([]);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // Delivery of a QUEUED steering message is inferred from the one edge the
  // backend exposes for it (see `settleQueuedOnTransition`).
  const prevStateRef = useRef<AiSessionState>(session.sessionState);
  useEffect(() => {
    const prev = prevStateRef.current;
    const next = session.sessionState;
    prevStateRef.current = next;
    if (prev === "ready" && next === "processing") {
      setLedger((entries) => settleQueuedOnTransition(entries, prev, next));
    }
  }, [session.sessionState]);

  const isProcessing =
    session.sessionState === "processing" || session.sessionState === "interrupting";

  const send = useCallback(async () => {
    const text = draft.trim();
    if (!text || sending) return;
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    setSending(true);
    setDraft("");
    setLedger((entries) => [...entries, { id, text, atMs: Date.now(), delivery: "sending" }]);
    const outcome = await session.sendMessage(text);
    setLedger((entries) =>
      entries.map((e) =>
        e.id !== id
          ? e
          : outcome.ok
            ? { ...e, delivery: outcome.queued ? "queued" : "sent" }
            : { ...e, delivery: "failed", error: outcome.error },
      ),
    );
    setSending(false);
    inputRef.current?.focus();
  }, [draft, sending, session]);

  const changedCount =
    changesRead.status === "ok"
      ? String(countChanged(changesRead.response.files))
      : changesRead.status === "loading" && changesRead.previous
        ? String(countChanged(changesRead.previous.files))
        : "?";

  return (
    <div
      className="flex h-full w-full min-h-0 flex-col bg-[#1a1b26] text-[#a9b1d6]"
      data-page-element="worker-session-cell"
      data-task-run-id={taskRunId}
      data-visible={visible ? "true" : "false"}
    >
      {/* Header: identity + state */}
      <div className="flex items-center gap-2 border-b border-[#2a2d3d] bg-[#13141f] px-2 py-1">
        <span className="text-[10px] font-medium uppercase tracking-wide text-[#7aa2f7]">
          Worker
        </span>
        <span className="truncate text-[11px] text-[#a9b1d6]" title={taskRunId}>
          {tab.title}
        </span>
        <WorkerStateBadge
          state={session.sessionState}
          readStatus={session.readStatus}
          lastReadError={session.lastReadError}
        />
        <div className="ml-auto flex items-center gap-1 text-[10px]">
          {(["conversation", "changes"] as const).map((p) => (
            <button
              key={p}
              type="button"
              onClick={() => setPane(p)}
              className={cn(
                "rounded px-1.5 py-px",
                pane === p
                  ? "bg-[#7aa2f7]/20 text-[#7aa2f7]"
                  : "text-zinc-500 hover:bg-white/5 hover:text-zinc-300",
              )}
              data-pane={p}
            >
              {p === "conversation" ? "Conversation" : `Changes (${changedCount})`}
            </button>
          ))}
        </div>
      </div>

      {/* Body */}
      {pane === "conversation" ? (
        <ConversationView
          messages={session.messages}
          streamingContent={session.streamingContent}
          streamingDroppedChars={session.streamingDroppedChars}
          isProcessing={isProcessing}
          toolActivity={session.toolActivity}
          readStatus={session.readStatus}
          lastReadError={session.lastReadError}
        />
      ) : (
        <FileChangesPanel read={changesRead} onRefresh={refreshChanges} />
      )}

      {/* Steering */}
      <div className="border-t border-[#2a2d3d] bg-[#13141f]">
        <SteeringLedger entries={ledger} />
        <div className="flex items-end gap-1.5 px-2 py-1.5">
          <textarea
            ref={inputRef}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void send();
              }
            }}
            rows={1}
            placeholder={
              isProcessing
                ? "Steer the worker — queued until its current turn ends"
                : "Steer the worker — sent immediately"
            }
            className="min-h-[26px] flex-1 resize-none rounded border border-[#2a2d3d] bg-[#1a1b26] px-2 py-1 text-xs text-[#a9b1d6] placeholder:text-zinc-600 focus:border-[#7aa2f7] focus:outline-none"
            data-steering-input
          />
          {isProcessing && (
            <button
              type="button"
              onClick={() => void session.interrupt()}
              className="inline-flex h-[26px] items-center gap-1 rounded border border-red-500/30 px-2 text-[10px] text-red-300 hover:bg-red-500/10"
              title="Interrupt the worker's current turn"
            >
              <Square className="h-3 w-3" />
              stop
            </button>
          )}
          <button
            type="button"
            onClick={() => void send()}
            disabled={sending || !draft.trim()}
            className="inline-flex h-[26px] items-center gap-1 rounded border border-[#7aa2f7]/40 px-2 text-[10px] text-[#7aa2f7] hover:bg-[#7aa2f7]/10 disabled:cursor-not-allowed disabled:opacity-40"
            title={isProcessing ? "Queue for delivery after the current turn" : "Send now"}
          >
            <Send className="h-3 w-3" />
            {isProcessing ? "queue" : "send"}
          </button>
        </div>
      </div>
    </div>
  );
}

export default WorkerSessionCell;
