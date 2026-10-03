/**
 * `StructuredSessionCell` — the grid cell for a structured (stream-json)
 * session: a Conductor worker, or a structured session the operator launched
 * from the Terminal page's launch menu (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Design decision 3 + Phase 9). One cell kind per lane, labelled with what it
 * hosts (`kind`): "Worker" or "Structured".
 *
 * A worker spawned by `/orchestrate` (`dispatch_subtask` in
 * `orchestration_loop/ai_session_executor.rs`) and a structured launch
 * (`create_structured_session`, `commands/structured_session.rs`) are both an
 * in-process stream-json `ClaudeSession` registered in the Rust
 * `SessionManager`. Neither has a terminal process: `TerminalInstance` would
 * attach to nothing and render a silent pane that looks like a terminal with
 * nothing to say. This cell renders what the session actually is —
 *
 * - **Permission card** (Phase 9): a structured launch runs in `Prompt` mode,
 *   so each tool call the CLI wants to make arrives as a typed request
 *   (`session-permission-request`, read at mount through
 *   `session_pending_permissions`). The card shows the tool and its arguments
 *   and answers through `respond_session_permission`: Allow, Deny, or Deny &
 *   interrupt. An unanswered request is denied by the runner at its deadline
 *   (fail closed), and the card says so. A worker runs in bypass mode and never
 *   raises one.
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
 *   settles to UNKNOWN — "the worker ended — delivery unconfirmed" — rather
 *   than going on promising a delivery. It is UNKNOWN and not "not delivered"
 *   because a `Ready` between the backend's `pop_front` and its re-send can be
 *   coalesced into one React batch: the pop is then never observed, the entry
 *   is still `queued` at the end edge, and a confident "not delivered" about a
 *   message that WAS delivered invites the operator to re-send and duplicate
 *   the steering. The cell genuinely cannot separate the two.
 *
 * Honesty (served `ux-priorities`): a read that fails renders UNKNOWN with
 * the failure named — never an empty transcript, never an empty change list,
 * never "closed".
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Send, ShieldAlert, Square } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AiMessage, AiSessionState } from "@qontinui/shared-types";
import { cn } from "@/lib/utils";
import {
  useAiSession,
  type SendMessageOutcome,
  type SessionReadStatus,
} from "@/hooks/useAiSession";
import { StreamingMessageView } from "../shared/StreamingMessageView";
import type { TerminalTab } from "./useTerminalManager";
import { changedCountLabel } from "./workerFileChanges";
import { FileChangesPanel, formatClock } from "./FileChangesView";
import { ReviewNotesList, ReviewSendBar, useHunkReviewBinding } from "./SessionReviewPanel";
import type { ReviewTarget } from "./sessionReviewApi";
import { useSessionReview } from "./useSessionReview";
import { TabTitle } from "./displayTitle";

// The diff list moved to `FileChangesView.tsx` so the PTY review panel can
// reuse it; `shouldFetchChanges` moved with the read it gates. Re-exported so
// existing imports keep resolving.
export { DiffView, FileChangeRow, FileChangesPanel } from "./FileChangesView";
export { shouldFetchChanges } from "./useSessionReview";

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
 * They settle to `undelivered` instead — a state whose LABEL says "delivery
 * unconfirmed", because the same unobserved-`Ready` window (see the FOLLOW-UP
 * on the effect below) means an entry can still be `queued` here after the
 * backend already popped and re-sent it. `undelivered` is this ledger's UNKNOWN
 * for a message whose fate the state stream cannot settle, not an assertion of
 * loss.
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
 *
 * **A still-PENDING arm survives `wentOutImmediately`, and that is the whole
 * point of the arm.** This runs on the awaited continuation — a microtask —
 * while `consumeDirectSendArm` runs from an effect, after a commit. The edge's
 * only route to the UI is the same round-trip that resolves this promise
 * (`send_user_message` returns the new `state`, which `useAiSession.sendMessage`
 * writes), so on the ordinary immediate-send path the reconcile lands FIRST and
 * the edge has NOT been observed yet. Disarming here would hand that unobserved
 * `ready → processing` to `settleQueuedOnTransition` with
 * `causedByDirectSend === false`, crediting a still-queued predecessor with a
 * delivery — the exact false claim the arm exists to prevent. (It is reachable:
 * the backend sends directly whenever `state == Ready` without consulting
 * `pending_messages` — `claude_session/session.rs` `send_user_message` — while
 * `send_next_pending_message` bails once the state is no longer `Ready`
 * — `claude_session/dispatcher.rs` — so a direct send that wins that race
 * leaves the older message queued.)
 *
 * A pending arm is retired by `consumeDirectSendArm` on the next OBSERVED
 * transition, whatever it is. Only a `spent` arm — one whose suppression was
 * already applied — is retired here, because its edge is gone.
 *
 * That bound is on observed transitions, not on elapsed turns, and the gap is
 * real: in the same coalescing window this doc names above, a `processing`
 * that React never renders is never observed, so the effect sees
 * `prev === next` and returns without consuming. A pending arm can therefore
 * outlive its own send and suppress a LATER genuine `pop_front` edge. The
 * consequence is a delivered entry left `queued`, which the end edge then
 * reports as `unconfirmed` — an UNKNOWN, never a false delivery — and the next
 * send re-arms. The asymmetry is accepted in that direction on purpose.
 */
export function reconcileDirectSendArm(
  arm: DirectSendArm,
  wentOutImmediately: boolean,
): { arm: DirectSendArm; resettle: boolean } {
  if (wentOutImmediately) {
    return { arm: arm.spent ? IDLE_DIRECT_SEND_ARM : arm, resettle: false };
  }
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
      // UNKNOWN, deliberately, and not "not delivered": the cell cannot tell
      // "never popped" from "popped during a turn end it never observed" (the
      // coalesced-`Ready` window named in the FOLLOW-UP below). A confident
      // "not delivered" about a message the worker did receive invites the
      // operator to re-send and duplicate the steering, so the label states the
      // uncertainty and the row stays red.
      return "the worker ended — delivery unconfirmed";
    case "failed":
      return `failed: ${entry.error ?? "unknown error"}`;
    default:
      return entry.delivery;
  }
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

export function ConversationView({
  messages,
  streamingContent,
  streamingDroppedChars,
  isProcessing,
  toolActivity,
  readStatus,
  lastReadError,
  kind = "worker",
}: {
  messages: readonly AiMessage[];
  streamingContent: string;
  streamingDroppedChars: number;
  isProcessing: boolean;
  toolActivity: string | null;
  readStatus: SessionReadStatus;
  lastReadError: string | null;
  /** Words the empty and unreadable states for what the cell hosts. */
  kind?: StructuredSessionKind;
}) {
  const copy = kindCopy(kind);
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
          UNKNOWN — this {copy.noun}&apos;s state and transcript could not be read
          {lastReadError ? `: ${lastReadError}` : ""}. Live output still streams in if the{" "}
          {copy.noun} is alive.
        </div>
      )}
      {readStatus === "pending" && nothingYet && (
        <div className="px-1 py-2 text-[11px] text-zinc-500">attaching to the {copy.noun} session…</div>
      )}
      {readStatus === "ok" && nothingYet && !isProcessing && (
        <div className="px-1 py-2 text-[11px] text-zinc-500">{copy.emptyTranscript}</div>
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

// ── Permission requests (Phase 9) ─────────────────────────────────────────────

/** `session-permission-request` payload (Rust `PermissionRequestNotice`). */
export interface PermissionRequest {
  sessionId: string;
  requestId: string;
  toolName: string;
  displayName?: string | null;
  description?: string | null;
  toolUseId?: string | null;
  input: unknown;
  suggestions: unknown[];
  requestedAt: string;
  expiresAt: string;
  timeoutSecs: number;
}

/** `session-permission-resolved` payload (Rust `PermissionResolvedNotice`). */
export interface PermissionResolved {
  sessionId: string;
  requestId: string;
  toolName: string;
  outcome: "allowed" | "denied" | "timed_out" | "session_ended" | "superseded";
  message?: string | null;
  interrupt: boolean;
}

export type PermissionAnswer = "allow" | "deny" | "deny_interrupt";

/** The most of a tool's arguments the card renders. */
export const PERMISSION_INPUT_MAX_CHARS = 4000;

/** Pretty-printed tool arguments, bounded. Pure. */
export function formatToolInput(
  input: unknown,
  maxChars: number = PERMISSION_INPUT_MAX_CHARS,
): { text: string; truncatedChars: number } {
  let text: string;
  try {
    text = JSON.stringify(input, null, 2) ?? String(input);
  } catch {
    text = String(input);
  }
  if (text.length <= maxChars) return { text, truncatedChars: 0 };
  return { text: text.slice(0, maxChars), truncatedChars: text.length - maxChars };
}

/** Add a request (idempotent by id), oldest first. Pure. */
export function addPermissionRequest(
  list: readonly PermissionRequest[],
  req: PermissionRequest,
): readonly PermissionRequest[] {
  if (list.some((r) => r.requestId === req.requestId)) return list;
  return [...list, req].sort((a, b) => a.requestedAt.localeCompare(b.requestedAt));
}

/** Drop a resolved request. Returns the SAME array when it was not listed. Pure. */
export function removePermissionRequest(
  list: readonly PermissionRequest[],
  requestId: string,
): readonly PermissionRequest[] {
  return list.some((r) => r.requestId === requestId)
    ? list.filter((r) => r.requestId !== requestId)
    : list;
}

/** What the card says about how the last request ended. Pure. */
export function resolvedLabel(r: PermissionResolved): string {
  switch (r.outcome) {
    case "allowed":
      return `Allowed ${r.toolName}.`;
    case "denied":
      return r.interrupt
        ? `Denied ${r.toolName} and interrupted the turn.`
        : `Denied ${r.toolName}.`;
    case "timed_out":
      return `No decision in time — the runner denied ${r.toolName} (fail closed).`;
    case "session_ended":
      return `The session ended before ${r.toolName} was answered.`;
    case "superseded":
      return `The CLI replaced the ${r.toolName} request with a new one before it was answered.`;
  }
}

/** What `respond_session_permission` returns (Rust `PermissionResponseOutcome`). */
export interface PermissionResponseOutcome {
  resolved: PermissionResolved;
  /** Set when a deny asked to interrupt and the interrupt could not be sent. */
  interruptError?: string | null;
}

/**
 * The warning to show for an answer whose deny went through but whose
 * interrupt did not — the turn is still running. `null` when there is
 * nothing to warn about. Pure.
 */
export function interruptFailureText(outcome: PermissionResponseOutcome | null): string | null {
  if (!outcome?.interruptError) return null;
  return `Denied ${outcome.resolved.toolName}, but the interrupt could not be sent — the turn is still running: ${outcome.interruptError}`;
}

/** The arguments `respond_session_permission` takes for an answer. Pure. */
export function respondArgs(
  sessionId: string,
  requestId: string,
  answer: PermissionAnswer,
): { sessionId: string; requestId: string; decision: "allow" | "deny"; interrupt: boolean } {
  return {
    sessionId,
    requestId,
    decision: answer === "allow" ? "allow" : "deny",
    interrupt: answer === "deny_interrupt",
  };
}

function formatDeadline(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleTimeString();
}

/**
 * One parked permission request: the tool, its arguments, and the three
 * answers. `busy` disables the buttons while an answer is in flight; `error`
 * is the last answer's failure, shown rather than swallowed.
 */
export function PermissionCard({
  request,
  onAnswer,
  busy,
  error,
}: {
  request: PermissionRequest;
  onAnswer: (answer: PermissionAnswer) => void;
  busy: boolean;
  error: string | null;
}) {
  const { text, truncatedChars } = formatToolInput(request.input);
  const tool = request.displayName || request.toolName;
  return (
    <div
      className="mx-2 my-1.5 rounded border border-amber-500/40 bg-amber-500/5 p-2 text-[11px]"
      data-permission-request={request.requestId}
      data-ui-bridge-id={`structured-session.permission-card.${request.requestId}`}
    >
      <div className="flex items-center gap-1.5 text-amber-200">
        <ShieldAlert className="h-3.5 w-3.5" />
        <span className="font-medium">Permission requested: {tool}</span>
        {request.description && (
          <span className="truncate text-[10px] text-amber-200/70" title={request.description}>
            — {request.description}
          </span>
        )}
      </div>
      <pre
        className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-black/30 p-1.5 font-mono text-[10px] text-[#a9b1d6]"
        data-permission-input
      >
        {text}
      </pre>
      {truncatedChars > 0 && (
        <div className="text-[10px] text-zinc-500" data-permission-input-truncated>
          {truncatedChars} more characters not shown
        </div>
      )}
      <div className="mt-1.5 flex items-center gap-1.5">
        <button
          type="button"
          disabled={busy}
          onClick={() => onAnswer("allow")}
          className="rounded border border-emerald-500/40 px-2 py-px text-[10px] text-emerald-300 hover:bg-emerald-500/10 disabled:opacity-40"
          data-permission-answer="allow"
        >
          Allow
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => onAnswer("deny")}
          className="rounded border border-red-500/40 px-2 py-px text-[10px] text-red-300 hover:bg-red-500/10 disabled:opacity-40"
          data-permission-answer="deny"
        >
          Deny
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => onAnswer("deny_interrupt")}
          className="rounded border border-red-500/40 px-2 py-px text-[10px] text-red-300 hover:bg-red-500/10 disabled:opacity-40"
          data-permission-answer="deny_interrupt"
          title="Deny this call and stop the current turn"
        >
          Deny &amp; interrupt
        </button>
        <span className="ml-auto text-[10px] text-zinc-500" title={request.expiresAt}>
          denied automatically at {formatDeadline(request.expiresAt)}
        </span>
      </div>
      {error && (
        <div className="mt-1 text-[10px] text-red-300" data-permission-error>
          The answer was not delivered: {error}
        </div>
      )}
    </div>
  );
}

/** How the pending set was read at mount. */
export type PermissionReadStatus = "pending" | "ok" | "failed";

/**
 * Session `sessionId`'s parked permission requests: read once at mount (a
 * request raised before this cell subscribed is still on screen), then kept by
 * the request/resolved events. A failed read is reported, never rendered as
 * "nothing pending".
 */
function useSessionPermissions(sessionId: string) {
  const [requests, setRequests] = useState<readonly PermissionRequest[]>([]);
  const [readStatus, setReadStatus] = useState<PermissionReadStatus>("pending");
  const [readError, setReadError] = useState<string | null>(null);
  const [lastResolved, setLastResolved] = useState<PermissionResolved | null>(null);
  const [busyIds, setBusyIds] = useState<ReadonlySet<string>>(() => new Set());
  const [answerError, setAnswerError] = useState<{ id: string; error: string } | null>(null);
  const [interruptWarning, setInterruptWarning] = useState<string | null>(null);
  /**
   * Requests known to be over — resolved by an event or answered here. The
   * mount read can return a snapshot taken before a resolution that was
   * delivered first; filtering through this set keeps that stale snapshot from
   * bringing back a card whose buttons can no longer do anything.
   */
  const resolvedIdsRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const keep = (fn: () => void) => {
      if (disposed) fn();
      else unlisteners.push(fn);
    };
    // A new session: nothing from the previous one carries over. Reset from
    // a microtask, never synchronously in the effect body
    // (react-hooks/set-state-in-effect).
    resolvedIdsRef.current = new Set();
    void Promise.resolve().then(() => {
      if (disposed) return;
      setRequests([]);
      setLastResolved(null);
      setReadStatus("pending");
      setReadError(null);
      setBusyIds(new Set());
      setAnswerError(null);
    });
    const admit = (list: readonly PermissionRequest[], req: PermissionRequest) =>
      resolvedIdsRef.current.has(req.requestId) ? list : addPermissionRequest(list, req);
    void listen<PermissionRequest>("session-permission-request", (event) => {
      if (event.payload.sessionId !== sessionId) return;
      setRequests((list) => admit(list, event.payload));
    }).then(keep);
    void listen<PermissionResolved>("session-permission-resolved", (event) => {
      if (event.payload.sessionId !== sessionId) return;
      resolvedIdsRef.current.add(event.payload.requestId);
      setRequests((list) => removePermissionRequest(list, event.payload.requestId));
      setLastResolved(event.payload);
    }).then(keep);
    invoke<PermissionRequest[]>("session_pending_permissions", { sessionId })
      .then((pending) => {
        if (disposed) return;
        setRequests((list) => pending.reduce(admit, list));
        setReadStatus("ok");
      })
      .catch((e: unknown) => {
        if (disposed) return;
        setReadStatus("failed");
        setReadError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      disposed = true;
      for (const fn of unlisteners) fn();
    };
  }, [sessionId]);

  const answer = useCallback(
    async (requestId: string, choice: PermissionAnswer) => {
      setBusyIds((ids) => new Set(ids).add(requestId));
      setAnswerError(null);
      setInterruptWarning(null);
      try {
        const outcome = await invoke<PermissionResponseOutcome | null>(
          "respond_session_permission",
          respondArgs(sessionId, requestId, choice),
        );
        setInterruptWarning(interruptFailureText(outcome));
        // The resolved event removes the card; remove it here too so a missed
        // event cannot leave live-looking buttons for an answered request.
        resolvedIdsRef.current.add(requestId);
        setRequests((list) => removePermissionRequest(list, requestId));
      } catch (e) {
        setAnswerError({ id: requestId, error: e instanceof Error ? e.message : String(e) });
      } finally {
        setBusyIds((ids) => {
          const next = new Set(ids);
          next.delete(requestId);
          return next;
        });
      }
    },
    [sessionId],
  );

  return {
    requests,
    readStatus,
    readError,
    lastResolved,
    busyIds,
    answerError,
    interruptWarning,
    answer,
  };
}

/** The structured-session kinds the cell labels. */
export type StructuredSessionKind = "worker" | "structured";

/** Header label and steering wording per kind. Pure. */
export function kindCopy(kind: StructuredSessionKind): {
  label: string;
  /** What the cell calls the thing it hosts, in running text. */
  noun: string;
  steerQueued: string;
  steerNow: string;
  /** The empty-transcript line after a successful read. */
  emptyTranscript: string;
} {
  return kind === "worker"
    ? {
        label: "Worker",
        noun: "worker",
        steerQueued: "Steer the worker — queued until its current turn ends",
        steerNow: "Steer the worker — sent immediately",
        emptyTranscript: "No transcript yet — the worker has not produced output.",
      }
    : {
        label: "Structured",
        noun: "session",
        steerQueued: "Message the session — queued until its current turn ends",
        steerNow: "Message the session — sent immediately",
        emptyTranscript: "No transcript yet — send the session a message to start it.",
      };
}

// ── The cell ──────────────────────────────────────────────────────────────────

export interface StructuredSessionCellProps {
  tab: TerminalTab;
  /** The session's task run id (`tab.taskRunId`), passed explicitly so the type says it is present. */
  taskRunId: string;
  visible: boolean;
  /**
   * What the cell hosts — a Conductor worker (bypass, never asks) or an
   * operator's structured launch (asks before each tool). Labels the header
   * and the steering input; the permission card renders for either whenever a
   * request is parked.
   */
  kind: StructuredSessionKind;
}

type CellPane = "conversation" | "changes";

export function StructuredSessionCell({ tab, taskRunId, visible, kind }: StructuredSessionCellProps) {
  const session = useAiSession({ attachTo: taskRunId });
  // Changes + review store, visible-gated: a hidden cell reads nothing.
  const review = useSessionReview(taskRunId, { visible, sessionState: session.sessionState });
  const changesRead = review.changes;
  const refreshChanges = review.refreshChanges;
  const { hunkReview, error: reviewError } = useHunkReviewBinding(review);
  const reviewTarget = useMemo<ReviewTarget>(() => ({ taskRunId }), [taskRunId]);
  const permissions = useSessionPermissions(taskRunId);
  const copy = kindCopy(kind);
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
  // the entry sticks at `queued` until the worker ends. That cuts BOTH ways:
  // the end-edge settle then moves an entry the backend really did deliver, so
  // `undelivered` is falsely ASSERTABLE, not merely late. The cell cannot tell
  // the two apart from this stream, which is why `undelivered` is labelled as
  // unconfirmed rather than as a loss. A `delivered`-with-message-id signal
  // from `send_next_pending_message` would remove the whole class; inferring it
  // from a state stream cannot.
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
    setLedger((entries) => entries.map((e) => (e.id !== id ? e : { ...e, ...settled })));
    setSending(false);
    inputRef.current?.focus();
  }, [draft, sending, session]);

  // The tab's count must agree with the rows the panel renders underneath it
  // — including the stale list it keeps up after a failed read.
  const changedCount = changedCountLabel(changesRead);

  return (
    <div
      className="flex h-full w-full min-h-0 flex-col bg-[#1a1b26] text-[#a9b1d6]"
      data-page-element="structured-session-cell"
      data-task-run-id={taskRunId}
      data-visible={visible ? "true" : "false"}
    >
      {/* Header: identity + state */}
      <div className="flex items-center gap-2 border-b border-[#2a2d3d] bg-[#13141f] px-2 py-1">
        <span
          className="text-[10px] font-medium uppercase tracking-wide text-[#7aa2f7]"
          data-session-kind={kind}
        >
          {copy.label}
        </span>
        <span className="truncate text-[11px] text-[#a9b1d6]" title={taskRunId}>
          <TabTitle tab={tab} />
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

      {/* Permission requests (Phase 9): parked tool calls waiting on the operator. */}
      {permissions.requests.map((req) => (
        <PermissionCard
          key={req.requestId}
          request={req}
          busy={permissions.busyIds.has(req.requestId)}
          error={permissions.answerError?.id === req.requestId ? permissions.answerError.error : null}
          onAnswer={(choice) => void permissions.answer(req.requestId, choice)}
        />
      ))}
      {permissions.requests.length === 0 && permissions.lastResolved && (
        <div
          className="border-b border-[#2a2d3d] px-2 py-0.5 text-[10px] text-zinc-400"
          data-permission-last-resolved={permissions.lastResolved.outcome}
        >
          {resolvedLabel(permissions.lastResolved)}
        </div>
      )}
      {permissions.interruptWarning && (
        <div
          className="border-b border-[#2a2d3d] px-2 py-0.5 text-[10px] text-amber-300"
          data-permission-interrupt-failed
          role="alert"
        >
          {permissions.interruptWarning}
        </div>
      )}
      {kind === "structured" && permissions.readStatus === "failed" && (
        <div
          className="border-b border-[#2a2d3d] px-2 py-0.5 text-[10px] text-fuchsia-300"
          data-permission-read-failed
        >
          UNKNOWN — pending permission requests could not be read
          {permissions.readError ? `: ${permissions.readError}` : ""}. New requests still appear
          here as they arrive.
        </div>
      )}

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
          kind={kind}
        />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          {reviewError && (
            <div className="truncate px-2 py-0.5 text-[10px] text-red-400" title={reviewError}>
              {reviewError}
            </div>
          )}
          <div className="min-h-0 flex-1">
            <FileChangesPanel read={changesRead} onRefresh={refreshChanges} review={hunkReview} />
          </div>
          <ReviewNotesList handle={review} />
          <ReviewSendBar handle={review} target={reviewTarget} />
        </div>
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
            placeholder={isProcessing ? copy.steerQueued : copy.steerNow}
            className="min-h-[26px] flex-1 resize-none rounded border border-[#2a2d3d] bg-[#1a1b26] px-2 py-1 text-xs text-[#a9b1d6] placeholder:text-zinc-600 focus:border-[#7aa2f7] focus:outline-none"
            data-steering-input
          />
          {isProcessing && (
            <button
              type="button"
              onClick={() => void session.interrupt()}
              className="inline-flex h-[26px] items-center gap-1 rounded border border-red-500/30 px-2 text-[10px] text-red-300 hover:bg-red-500/10"
              title="Interrupt the session's current turn"
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

export default StructuredSessionCell;
