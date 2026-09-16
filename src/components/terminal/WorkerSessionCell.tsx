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
 *   when a turn ends. The read costs a whole-file sweep on the runner, so it
 *   is gated on `visible`: a cell the grid has hidden reads nothing and
 *   remembers that a read is owed (`shouldFetchChanges`).
 * - **Steering**: an input that sends through `send_user_message`, the same
 *   command the Process Manager uses. `ClaudeSession::send_user_message` sends
 *   immediately when the worker is `Ready` and queues otherwise; the ledger
 *   under the input says which happened, and flips ONE queued entry —
 *   the oldest — to "delivered" per observed `ready → processing` transition,
 *   matching the single `pop_front` the backend does per turn end. A worker
 *   that ENDS pops nothing ever again, so on that edge everything still queued
 *   settles to "not delivered" rather than going on promising a delivery.
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
import {
  useAiSession,
  type SendMessageOutcome,
  type SessionReadStatus,
} from "@/hooks/useAiSession";
import { StreamingMessageView } from "../shared/StreamingMessageView";
import type { TerminalTab } from "./useTerminalManager";
import {
  changedCountLabel,
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

export type SteeringDelivery =
  | "sending"
  | "sent"
  | "queued"
  | "delivered"
  | "undelivered"
  | "failed";

export interface SteeringEntry {
  id: string;
  text: string;
  atMs: number;
  delivery: SteeringDelivery;
  error?: string;
}

/**
 * A worker state from which no queued message can ever be drained. The backend
 * pops the queue at a turn END; a worker that finishes or dies has no next turn
 * end, so anything still queued is lost.
 */
export function isWorkerEndState(state: AiSessionState): boolean {
  return state === "closed" || state === "error" || state === "not_found";
}

/**
 * The backend drains a queued message the moment the current turn ends:
 * `Ready` is reached, the queued text is written, and the session goes
 * straight back to `Processing`. That `ready → processing` edge is the only
 * evidence the frontend has of delivery, so a queued entry flips to
 * `delivered` on that edge and on nothing else. Pure.
 *
 * **Exactly ONE entry per edge, oldest first.** `send_next_pending_message`
 * (`claude_session/dispatcher.rs`) does a single `pop_front` per turn end, and
 * `MAX_PENDING_MESSAGES` is greater than 1 (`claude_session/session.rs`), so
 * two queued messages drain over two turns. Flipping the whole queue on one
 * edge told the operator a message had landed while it was still queued — or,
 * if the worker finished first, while it was lost. The ledger is the only
 * account of steering this cell offers; it says what happened or it says
 * nothing.
 *
 * **The other terminal edge is the worker ENDING.** `processing → closed` (or
 * `error`, or `not_found`) is the turn that never ends: whatever is still
 * queued when the worker stops will never be popped. Leaving those entries at
 * `queued` left the ledger asserting "delivered when the current turn ends"
 * about a message that is gone, which is the one thing this ledger must not do.
 * They settle to `undelivered` instead.
 *
 * `causedByDirectSend` suppresses the `ready → processing` settle for the edge
 * an immediate send CAUSED: a message sent at the `Ready` instant goes straight
 * out (`queued: false`) and drives the session back to `Processing` without the
 * backend popping anything, so settling on it would credit a still-queued
 * predecessor with a delivery that did not happen.
 */
export function settleQueuedOnTransition(
  entries: readonly SteeringEntry[],
  prev: AiSessionState,
  next: AiSessionState,
  causedByDirectSend = false,
): readonly SteeringEntry[] {
  const firstQueued = entries.findIndex((e) => e.delivery === "queued");
  if (firstQueued === -1) return entries;
  if (isWorkerEndState(next) && !isWorkerEndState(prev)) {
    // No further turn end, so no further pop: everything still queued is lost.
    return entries.map((e) => (e.delivery === "queued" ? { ...e, delivery: "undelivered" } : e));
  }
  if (!(prev === "ready" && next === "processing")) return entries;
  if (causedByDirectSend) return entries;
  // Entries are appended in send order, so the first `queued` one is the head
  // of the backend's own FIFO — the message its `pop_front` just took.
  return entries.map((e, i) => (i === firstQueued ? { ...e, delivery: "delivered" } : e));
}

/**
 * How a send's outcome settles its own ledger row.
 *
 * `workerStateNow` is the worker's state at the moment the outcome arrives, not
 * at the moment the send was issued. A row is `sending` for the whole duration
 * of the `send_user_message` invoke, and `settleQueuedOnTransition` only moves
 * rows that are already `queued` — so a worker that ENDS while a send is in
 * flight goes past the end edge with nothing to settle, and writing `queued`
 * afterwards would strand a permanent "delivered when the current turn ends" on
 * a dead worker. That window is exactly when a steering message is most likely
 * to be lost (queued into a worker's last turn), so it records `undelivered`.
 */
export function deliveryForSendOutcome(
  outcome: SendMessageOutcome,
  workerStateNow: AiSessionState,
): { delivery: SteeringDelivery; error?: string } {
  if (!outcome.ok) return { delivery: "failed", error: outcome.error };
  if (!outcome.queued) return { delivery: "sent" };
  return { delivery: isWorkerEndState(workerStateNow) ? "undelivered" : "queued" };
}

/**
 * The one-shot suppression that keeps an immediate send from being mistaken for
 * the backend draining its queue.
 *
 * `pending` means a send was issued while the worker looked idle, so the next
 * `ready → processing` edge is expected to be that send's own and not a
 * `pop_front`. `spent` means the suppression was actually applied — which
 * matters because the client's "idle" is a guess: only the send's OUTCOME says
 * whether it went out immediately, and it arrives after the edge may already
 * have gone past.
 */
export interface DirectSendArm {
  pending: boolean;
  spent: boolean;
}

export const IDLE_DIRECT_SEND_ARM: DirectSendArm = { pending: false, spent: false };

/** Arm iff the worker looked idle when the send was issued. */
export function armDirectSend(stateAtIssue: AiSessionState): DirectSendArm {
  return { pending: stateAtIssue === "ready", spent: false };
}

/**
 * Apply the arm to an observed transition.
 *
 * ANY observed transition consumes it, not only the expected edge: an arm that
 * is never consumed goes on to suppress a genuine `pop_front` an arbitrary
 * number of turns later, which would report a delivered message as lost.
 */
export function consumeDirectSendArm(
  arm: DirectSendArm,
  prev: AiSessionState,
  next: AiSessionState,
): { arm: DirectSendArm; causedByDirectSend: boolean } {
  if (!arm.pending) return { arm, causedByDirectSend: false };
  const causedByDirectSend = prev === "ready" && next === "processing";
  return { arm: { pending: false, spent: causedByDirectSend }, causedByDirectSend };
}

/**
 * Reconcile the arm with the outcome that finally arrived.
 *
 * `resettle` is the repair: the client thought the worker was idle, suppressed
 * an edge on that basis, and the outcome then said `queued` — so that edge WAS
 * a real turn end and the settle it swallowed has to be put back, or an older
 * queued message is reported lost when it was delivered.
 */
export function reconcileDirectSendArm(
  arm: DirectSendArm,
  wentOutImmediately: boolean,
): { arm: DirectSendArm; resettle: boolean } {
  if (wentOutImmediately) return { arm: { pending: false, spent: false }, resettle: false };
  return { arm: IDLE_DIRECT_SEND_ARM, resettle: arm.spent };
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
    case "undelivered":
      return "not delivered — the worker ended before the queue drained";
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
              // A message that was LOST reads as a failure, because it is one.
              // Falling through to the neutral default would have coloured it
              // the same as a send still in flight.
              (e.delivery === "failed" || e.delivery === "undelivered") && "text-red-400",
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
    <>
      <ul className="m-0 list-none p-0">
        {orderChanges(response.files).map((change) => (
          <FileChangeRow key={change.filePath} change={change} />
        ))}
      </ul>
      {response.filesTruncated && (
        <div
          className="border-t border-[#2a2d3d] px-2 py-1.5 text-[10px] text-fuchsia-300"
          data-file-changes-cut="true"
        >
          list cut at {response.files.length} files — {response.omittedFiles} more path
          {response.omittedFiles === 1 ? "" : "s"} this worker touched are NOT shown
        </div>
      )}
    </>
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

/**
 * Whether the changes hook should issue a read right now. Pure, exported for
 * the test.
 *
 * A cell whose body the grid has hidden (behind a compact card, or an
 * off-screen zone — `ZoneGrid` passes `visible={!showCompactCard}`) reads
 * NOTHING: the route reads every file the worker touched off disk, and with N
 * workers on a page an unconditional read meant N whole-file sweeps per edit
 * burst for the page's lifetime, for a list no one could see. The read is
 * deferred to the moment the cell first becomes visible, and a refresh
 * triggered while hidden is remembered as `stale` rather than performed.
 */
export function shouldFetchChanges(args: {
  visible: boolean;
  /** `taskRunId` the last read was issued for, or `null` if none ever was. */
  fetchedFor: string | null;
  taskRunId: string;
  /** A refresh was wanted while hidden. */
  stale: boolean;
}): boolean {
  if (!args.visible) return false;
  if (args.fetchedFor !== args.taskRunId) return true;
  return args.stale;
}

function useWorkerFileChanges(
  taskRunId: string,
  sessionState: AiSessionState,
  visible: boolean,
) {
  const [read, setRead] = useState<FileChangesRead>({ status: "loading", previous: null });
  const latestRef = useRef<SessionFileChangesResponse | null>(null);
  const inFlightRef = useRef<AbortController | null>(null);
  /** The id the last read was issued for — `null` until one has been. */
  const fetchedForRef = useRef<string | null>(null);
  /** A refresh was wanted while the cell was hidden; owed on next visible. */
  const staleRef = useRef(false);
  /** `visible` readable from the event listener without re-subscribing it. */
  const visibleRef = useRef(visible);
  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);

  const refresh = useCallback(() => {
    inFlightRef.current?.abort();
    const ctrl = new AbortController();
    inFlightRef.current = ctrl;
    fetchedForRef.current = taskRunId;
    staleRef.current = false;
    setRead({ status: "loading", previous: latestRef.current });
    fetchSessionFileChanges(taskRunId, ctrl.signal)
      .then((response) => {
        if (ctrl.signal.aborted) return;
        latestRef.current = response;
        setRead({ status: "ok", response });
      })
      .catch((err: unknown) => {
        if (ctrl.signal.aborted) return;
        // A read is still OWED. `fetchedForRef` and `staleRef` were both
        // settled at issue time, so without this a first FAILED read left
        // `shouldFetchChanges` answering false forever — visible, same id, not
        // stale — and the only ways back were a `commit-state-changed`, a turn
        // end, or a manual refresh. For a worker that failed and went quiet
        // that is never, so the pane sat on its error until the page reloaded.
        staleRef.current = true;
        setRead({
          status: "error",
          error: err instanceof Error ? err.message : String(err),
          atMs: Date.now(),
          previous: latestRef.current,
        });
      });
  }, [taskRunId]);

  /** Refresh, or remember that one is owed, depending on visibility. */
  const refreshIfVisible = useCallback(() => {
    if (!visibleRef.current) {
      staleRef.current = true;
      return;
    }
    refresh();
  }, [refresh]);

  // First read once the cell is actually visible, and again on id change or
  // when a refresh fell due while it was hidden.
  useEffect(() => {
    if (
      shouldFetchChanges({
        visible,
        fetchedFor: fetchedForRef.current,
        taskRunId,
        stale: staleRef.current,
      })
    ) {
      refresh();
    }
  }, [visible, taskRunId, refresh]);

  // Abort whatever is in flight when the cell goes away.
  useEffect(() => () => inFlightRef.current?.abort(), []);

  // Edit-time refresh: the dispatcher emits `commit-state-changed` for the
  // session after every Edit/Write hook (`dispatcher.rs`), debounced here so
  // a burst of edits costs one read.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<{ task_run_id?: string }>("commit-state-changed", (event) => {
      if (event.payload?.task_run_id !== taskRunId) return;
      // Hidden: mark the list stale and do no work. The read happens when the
      // operator can see it.
      if (!visibleRef.current) {
        staleRef.current = true;
        return;
      }
      if (timer) clearTimeout(timer);
      timer = setTimeout(refreshIfVisible, CHANGES_REFRESH_DEBOUNCE_MS);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      unlisten?.();
    };
  }, [taskRunId, refreshIfVisible]);

  // Turn-end refresh: catches edits made by a tool the hook did not see.
  const prevStateRef = useRef<AiSessionState>(sessionState);
  useEffect(() => {
    const prev = prevStateRef.current;
    prevStateRef.current = sessionState;
    if (prev === "processing" && (sessionState === "ready" || sessionState === "closed")) {
      refreshIfVisible();
    }
  }, [sessionState, refreshIfVisible]);

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
    visible,
  );
  const [pane, setPane] = useState<CellPane>("conversation");
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [ledger, setLedger] = useState<readonly SteeringEntry[]>([]);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  /**
   * A send issued while the worker was `Ready` is expected to cause the next
   * `ready → processing` edge itself, so that edge is NOT a queue drain (see
   * `settleQueuedOnTransition`).
   *
   * The transitions are `armDirectSend` / `consumeDirectSendArm` /
   * `reconcileDirectSendArm` — pure, and tested there.
   */
  const directSendEdgeRef = useRef<DirectSendArm>(IDLE_DIRECT_SEND_ARM);

  /**
   * The worker's state as of the last committed render, readable from `send`'s
   * post-await code. `session.sessionState` inside that closure is the value
   * from the render the send was issued in, which by then may be two
   * transitions old.
   */
  const sessionStateRef = useRef<AiSessionState>(session.sessionState);

  // Delivery of a QUEUED steering message is inferred from the two edges the
  // backend exposes for it (see `settleQueuedOnTransition`): a turn end that
  // pops one, and the worker ending, which pops nothing ever again.
  //
  // FOLLOW-UP (backend): both are inferences from a state stream, and a narrow
  // gap survives every mitigation here — the `Ready` between a `pop_front` and
  // the re-send can be microseconds, so if two `claude-session-state` events
  // land in the same React batch the intermediate `ready` is never observed and
  // the entry sticks at `queued` until the worker ends. A
  // `delivered`-with-message-id signal from `send_next_pending_message` would
  // remove the whole class; inferring it from a state stream cannot.
  const prevStateRef = useRef<AiSessionState>(session.sessionState);
  useEffect(() => {
    const prev = prevStateRef.current;
    const next = session.sessionState;
    sessionStateRef.current = next;
    if (prev === next) return;
    prevStateRef.current = next;
    const consumed = consumeDirectSendArm(directSendEdgeRef.current, prev, next);
    directSendEdgeRef.current = consumed.arm;
    // Which edges mean what is decided in ONE place — the pure function, which
    // returns the SAME array when a transition changes nothing, so React bails
    // out of the re-render without a second copy of that decision here.
    setLedger((entries) =>
      settleQueuedOnTransition(entries, prev, next, consumed.causedByDirectSend),
    );
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
    // Issued while the worker is idle: this send goes out immediately and is
    // itself what drives `ready → processing`, so that edge must not be read as
    // the backend popping an older queued message. Armed BEFORE the await,
    // because the state event can arrive before the command resolves.
    directSendEdgeRef.current = armDirectSend(sessionStateRef.current);
    const outcome = await session.sendMessage(text);
    const reconciled = reconcileDirectSendArm(
      directSendEdgeRef.current,
      outcome.ok && !outcome.queued,
    );
    directSendEdgeRef.current = reconciled.arm;
    if (reconciled.resettle) {
      // A suppression was applied to an edge that the outcome proved was a real
      // turn end. Put the settle back BEFORE this entry is marked, so it lands
      // on the older queued message rather than on this one — which is still
      // `sending`, and therefore invisible to `settleQueuedOnTransition`.
      setLedger((entries) => settleQueuedOnTransition(entries, "ready", "processing"));
    }
    // `sessionStateRef` and not the render-time state: the worker may have
    // ENDED while this send was in flight, and the effect cannot fix that row
    // afterwards because it was still `sending` when the end edge went past.
    const settled = deliveryForSendOutcome(outcome, sessionStateRef.current);
    setLedger((entries) =>
      entries.map((e) => (e.id !== id ? e : { ...e, ...settled })),
    );
    setSending(false);
    inputRef.current?.focus();
  }, [draft, sending, session]);

  // The tab's count must agree with the rows the panel renders underneath it
  // — including the stale list it keeps up after a failed read.
  const changedCount = changedCountLabel(changesRead);

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
              title={p === "changes" ? changedCount.title : undefined}
              data-changes-stale={p === "changes" ? String(changedCount.stale) : undefined}
            >
              {p === "conversation" ? "Conversation" : `Changes (${changedCount.text})`}
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
