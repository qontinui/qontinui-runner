/**
 * The review surface on the Terminal page — plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 4.
 *
 * - `SessionReviewPanel` — a PTY zone's overlay, toggled from the zone title
 *   bar beside the prompts toggle and positioned by the same `top` / `right`
 *   orientation contract as `ZonePromptsPanel`. It lists the session's file
 *   changes with `FileChangesPanel` (the worker cell's list, not a fork), each
 *   hunk carrying the review affordances, and its footer is the send bar.
 * - `ReviewSendBar` — attached notes as removable chips, a free-text line,
 *   "Preview exact text" (the `composeReviewPrompt` output, byte for byte),
 *   **Send**, and — for a PTY tab only — **Insert without sending**, which
 *   types the text with no Enter: the notes stay attached and delivery is the
 *   operator's. The worker cell mounts the same bar with a `{taskRunId}` target.
 * - `SessionReviewBadge` — `n unread` / `?` / nothing beside the tab title
 *   (`reviewBadgeState`).
 *
 * The decisions all live in `sessionReviewView.ts` (pure, tested); this file
 * is the glue. Every mutation's failure is shown where it happened — a click
 * that silently did nothing is the failure mode this surface must not have.
 */

import { useCallback, useMemo, useReducer, useState, type ReactNode } from "react";
import {
  ClipboardCheck,
  Eye,
  EyeOff,
  FileDiff,
  RefreshCw,
  Send,
  TextCursorInput,
  X,
} from "lucide-react";
import { cn, describeThrown } from "@/lib/utils";
import { FileChangesPanel, type HunkReview, type NoteDraftTarget } from "./FileChangesView";
import { REVIEW_PANEL_GEOMETRY } from "./promptsPanelLayout";
import { ORPHAN_LABEL } from "./sessionReview";
import { ReviewApiError, type ReviewNoteRow, type ReviewTarget } from "./sessionReviewApi";
import {
  attachedNotes,
  canInsert,
  composeForSend,
  initialSendBar,
  isOrphaned,
  liveKeysOf,
  newSalt,
  noteStateLabel,
  pendingNotes,
  readKeySet,
  sendBarReducer,
  sendResultLabel,
  sentNotes,
  shouldAutoMarkRead,
  shownChanges,
  shownReview,
  visibleNotes,
  type ReviewBadgeState,
  type SendMode,
} from "./sessionReviewView";
import type { SessionReviewHandle } from "./useSessionReview";
import type { PromptsPanelOrientation } from "./ZonePromptsPanel";

/** The bare cause of a rejection; every caller prefixes its own context. */
function errorText(err: unknown): string {
  return describeThrown(err, "unknown error");
}

// ---------------------------------------------------------------------------
// The hunk binding: read toggles, scroll-past, note drafts
// ---------------------------------------------------------------------------

function NoteDraftEditor({
  target,
  onSave,
  onCancel,
}: {
  target: NoteDraftTarget;
  onSave: (body: string, attach: boolean) => Promise<void>;
  onCancel: () => void;
}) {
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async (attach: boolean) => {
    if (!body.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSave(body.trim(), attach);
    } catch (err) {
      setError(errorText(err));
      setBusy(false);
    }
  };
  return (
    <div
      className="my-1 flex flex-col gap-1 rounded border border-amber-500/30 bg-amber-500/5 p-1.5 font-sans"
      data-ui-bridge-id="terminal.review-note-draft"
      data-hunk-key={target.hunkKey}
    >
      <textarea
        autoFocus
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") onCancel();
        }}
        rows={2}
        placeholder={`Note on ${target.hunkHeader}`}
        className="w-full resize-y rounded border border-[#2a2d3d] bg-[#1a1b26] px-1.5 py-1 text-[11px] text-[#c0caf5] placeholder:text-zinc-600 focus:border-amber-400/60 focus:outline-none"
        data-ui-bridge-id="terminal.review-note-draft-input"
      />
      <div className="flex items-center gap-1 text-[10px]">
        <button
          type="button"
          disabled={busy || !body.trim()}
          onClick={() => void save(false)}
          className="rounded border border-[#2a2d3d] px-1.5 py-px text-zinc-300 hover:bg-white/5 disabled:opacity-40"
          data-ui-bridge-id="terminal.review-note-save"
          title="Save the note as a draft (not attached to the next send)"
        >
          Save
        </button>
        <button
          type="button"
          disabled={busy || !body.trim()}
          onClick={() => void save(true)}
          className="rounded border border-amber-500/40 px-1.5 py-px text-amber-300 hover:bg-amber-500/10 disabled:opacity-40"
          data-ui-bridge-id="terminal.review-note-save-attach"
          title="Save the note and attach it to the next send"
        >
          Save &amp; attach
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="rounded px-1.5 py-px text-zinc-500 hover:text-zinc-300"
          data-ui-bridge-id="terminal.review-note-cancel"
        >
          Cancel
        </button>
        {error && (
          <span className="truncate text-red-400" title={error}>
            not saved: {error}
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * Builds the `HunkReview` a `FileChangesPanel` takes from a review handle.
 *
 * Mark-read triggers (stated here because the plan asked which one shipped):
 * the explicit per-hunk toggle, and a hunk scrolled PAST inside an expanded
 * file's diff. Expanding a file alone marks nothing. A hunk the operator
 * explicitly marks unread is never re-marked by a scroll in this view.
 */
export function useHunkReviewBinding(handle: SessionReviewHandle): {
  hunkReview: HunkReview;
  error: string | null;
} {
  const review = shownReview(handle.review);
  const readKeys = useMemo(() => (review ? readKeySet(review) : null), [review]);
  const notes = useMemo(() => visibleNotes(review), [review]);
  const [manuallyUnread, setManuallyUnread] = useState<ReadonlySet<string>>(() => new Set());
  const [inFlight, setInFlight] = useState<ReadonlySet<string>>(() => new Set());
  const [draft, setDraft] = useState<NoteDraftTarget | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { markRead, addNote, patchNote } = handle;

  const mark = useCallback(
    (filePath: string, hunkKey: string, read: boolean) => {
      setInFlight((prev) => new Set(prev).add(hunkKey));
      setError(null);
      markRead([{ filePath, hunkKey }], read)
        .catch((err: unknown) =>
          setError(`mark ${read ? "read" : "unread"} failed: ${errorText(err)}`),
        )
        .finally(() =>
          setInFlight((prev) => {
            const next = new Set(prev);
            next.delete(hunkKey);
            return next;
          }),
        );
    },
    [markRead],
  );

  const onToggleRead = useCallback(
    (filePath: string, hunkKey: string, read: boolean) => {
      setManuallyUnread((prev) => {
        const next = new Set(prev);
        if (read) next.delete(hunkKey);
        else next.add(hunkKey);
        return next;
      });
      mark(filePath, hunkKey, read);
    },
    [mark],
  );

  const onScrolledPast = useCallback(
    (filePath: string, hunkKey: string) => {
      if (!readKeys) return; // read state UNKNOWN: never write on a guess
      if (shouldAutoMarkRead(hunkKey, { readKeys, manuallyUnread, inFlight })) {
        mark(filePath, hunkKey, true);
      }
    },
    [readKeys, manuallyUnread, inFlight, mark],
  );

  const saveDraft = useCallback(
    async (body: string, attach: boolean) => {
      if (!draft) return;
      const note = await addNote({
        filePath: draft.filePath,
        hunkKey: draft.hunkKey,
        hunkHeader: draft.hunkHeader,
        excerpt: draft.excerpt,
        body,
      });
      setDraft(null);
      if (attach) {
        await patchNote(note.id, { type: "attach" }).catch((err: unknown) =>
          setError(`the note was saved but not attached: ${errorText(err)}`),
        );
      }
    },
    [draft, addNote, patchNote],
  );

  const cancelDraft = useCallback(() => setDraft(null), []);
  const draftSlot: ReactNode = useMemo(
    () =>
      draft ? (
        <NoteDraftEditor
          key={draft.hunkKey}
          target={draft}
          onSave={saveDraft}
          onCancel={cancelDraft}
        />
      ) : null,
    [draft, saveDraft, cancelDraft],
  );

  // One object per real change: a fresh `hunkReview` every render invalidated
  // every `keys` memo below it and rebuilt each diff's IntersectionObserver.
  const hunkReview = useMemo<HunkReview>(
    () => ({
      readKeys,
      notes,
      onToggleRead,
      onScrolledPast,
      onAddNote: setDraft,
      draftKey: draft?.hunkKey ?? null,
      draftSlot,
    }),
    [readKeys, notes, onToggleRead, onScrolledPast, draft, draftSlot],
  );

  return { hunkReview, error };
}

// ---------------------------------------------------------------------------
// Notes list
// ---------------------------------------------------------------------------

function NoteLine({
  note,
  liveKeys,
  actions,
}: {
  note: ReviewNoteRow;
  liveKeys: ReadonlySet<string> | null;
  actions?: ReactNode;
}) {
  const orphaned = isOrphaned(note, liveKeys);
  return (
    <li
      className="flex flex-col gap-0.5 border-b border-[#2a2d3d] px-2 py-1 last:border-b-0"
      data-note-id={note.id}
      data-note-state={note.state}
      data-note-orphaned={orphaned ? "true" : undefined}
    >
      <div className="flex items-center gap-1.5 text-[10px]">
        <span className="truncate font-mono text-zinc-400" title={note.filePath}>
          {note.filePath.split(/[/\\]/).pop()} {note.hunkHeader}
        </span>
        <span
          className={cn(
            "shrink-0",
            note.state === "confirmed" && "text-emerald-400",
            note.state === "submitted" && "text-amber-300",
            note.state === "unknown" && "text-fuchsia-300",
            note.state === "pending" && "text-zinc-500",
          )}
        >
          {noteStateLabel(note)}
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-1">{actions}</span>
      </div>
      {orphaned && <div className="text-[9px] text-fuchsia-300">{ORPHAN_LABEL}</div>}
      <div className="whitespace-pre-wrap break-words text-[11px] text-[#c0caf5]">{note.body}</div>
    </li>
  );
}

export function ReviewNotesList({ handle }: { handle: SessionReviewHandle }) {
  const review = shownReview(handle.review);
  const liveKeys = useMemo(() => liveKeysOf(shownChanges(handle.changes)), [handle.changes]);
  const [error, setError] = useState<string | null>(null);
  const pending = pendingNotes(review);
  const sent = sentNotes(review);
  const { patchNote } = handle;
  const act = (noteId: string, type: "attach" | "discard") => {
    setError(null);
    patchNote(noteId, { type }).catch((err: unknown) =>
      setError(`${type} failed: ${errorText(err)}`),
    );
  };
  if (!review) {
    return handle.review.status === "error" ? (
      <div className="px-2 py-1 text-[10px] text-fuchsia-300">
        UNKNOWN — notes could not be read: {handle.review.error}
      </div>
    ) : null;
  }
  if (pending.length === 0 && sent.length === 0 && !error) return null;
  return (
    <div className="max-h-40 shrink-0 overflow-y-auto border-t border-[#2a2d3d]" data-review-notes>
      {error && <div className="px-2 py-0.5 text-[10px] text-red-400">{error}</div>}
      {pending.length > 0 && (
        <>
          <div className="px-2 pt-1 text-[9px] uppercase tracking-wide text-zinc-500">Drafts</div>
          <ul className="m-0 list-none p-0">
            {pending.map((note) => (
              <NoteLine
                key={note.id}
                note={note}
                liveKeys={liveKeys}
                actions={
                  <>
                    <button
                      type="button"
                      onClick={() => act(note.id, "attach")}
                      className="rounded px-1 text-amber-300 hover:bg-amber-500/10"
                      data-ui-bridge-id={`terminal.review-note-attach.${note.id}`}
                    >
                      attach
                    </button>
                    <button
                      type="button"
                      onClick={() => act(note.id, "discard")}
                      className="rounded px-1 text-zinc-500 hover:text-red-300"
                      data-ui-bridge-id={`terminal.review-note-discard.${note.id}`}
                    >
                      discard
                    </button>
                  </>
                }
              />
            ))}
          </ul>
        </>
      )}
      {sent.length > 0 && (
        <>
          <div className="px-2 pt-1 text-[9px] uppercase tracking-wide text-zinc-500">Sent</div>
          <ul className="m-0 list-none p-0">
            {sent.map((note) => (
              <NoteLine key={note.id} note={note} liveKeys={liveKeys} />
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Send bar
// ---------------------------------------------------------------------------

export function ReviewSendBar({
  handle,
  target,
}: {
  handle: SessionReviewHandle;
  target: ReviewTarget;
}) {
  const [bar, dispatch] = useReducer(sendBarReducer, undefined, () => initialSendBar(newSalt()));
  const [chipError, setChipError] = useState<string | null>(null);
  const review = shownReview(handle.review);
  const attached = useMemo(() => attachedNotes(review), [review]);
  const liveKeys = useMemo(() => liveKeysOf(shownChanges(handle.changes)), [handle.changes]);
  const composed = useMemo(
    () =>
      handle.sessionId
        ? composeForSend({
            sessionId: handle.sessionId,
            attached,
            freeText: bar.freeText,
            salt: bar.salt,
            liveKeys,
          })
        : null,
    [handle.sessionId, attached, bar.freeText, bar.salt, liveKeys],
  );
  const insertable = canInsert(target);
  const { deliver, patchNote } = handle;

  const go = async (mode: SendMode) => {
    if (!composed || bar.busy) return;
    dispatch({ type: "start", mode });
    try {
      const outcome = await deliver(mode, {
        target,
        noteIds: attached.map((n) => n.id),
        text: composed.text,
      });
      dispatch({
        type: "succeeded",
        mode,
        marker: outcome.marker,
        sanitized: outcome.sanitized,
        atMs: Date.now(),
        nextSalt: newSalt(),
      });
    } catch (err) {
      const typed = err instanceof ReviewApiError ? err : null;
      dispatch({
        type: "failed",
        mode,
        message: errorText(err),
        atMs: Date.now(),
        code: typed?.code ?? null,
        marker: composed.marker,
        noteIds: typed?.noteIds ?? [],
        nextSalt: newSalt(),
      });
    }
  };

  const detach = (noteId: string) => {
    setChipError(null);
    patchNote(noteId, { type: "detach" }).catch((err: unknown) =>
      setChipError(`detach failed: ${errorText(err)}`),
    );
  };

  const reviewUnknown = !review;
  return (
    <div
      className="shrink-0 border-t border-[#2a2d3d] bg-[#13141f] px-2 py-1.5"
      data-ui-bridge-id="terminal.review-send-bar"
      onKeyDown={(e) => e.stopPropagation()}
    >
      <div className="flex flex-wrap items-center gap-1">
        {reviewUnknown && (
          <span className="text-[10px] text-fuchsia-300">
            UNKNOWN — attached notes could not be read
          </span>
        )}
        {!reviewUnknown && attached.length === 0 && (
          <span className="text-[10px] text-zinc-500">
            No notes attached — add a note on a hunk, then attach it.
          </span>
        )}
        {attached.map((note) => {
          const orphaned = isOrphaned(note, liveKeys);
          return (
            <span
              key={note.id}
              className={cn(
                "inline-flex max-w-[220px] items-center gap-1 rounded border px-1.5 py-px text-[10px]",
                orphaned
                  ? "border-fuchsia-500/40 bg-fuchsia-500/10 text-fuchsia-200"
                  : "border-amber-500/40 bg-amber-500/10 text-amber-200",
              )}
              title={`${note.filePath} ${note.hunkHeader}\n${note.body}${orphaned ? `\n${ORPHAN_LABEL}` : ""}`}
              data-ui-bridge-id={`terminal.review-chip.${note.id}`}
              data-note-orphaned={orphaned ? "true" : undefined}
            >
              <span className="truncate">
                {orphaned ? "orphaned · " : ""}
                {note.filePath.split(/[/\\]/).pop()}: {note.body}
              </span>
              <button
                type="button"
                onClick={() => detach(note.id)}
                className="shrink-0 rounded hover:bg-white/10"
                title="Detach — the note stays as a draft"
                data-ui-bridge-id={`terminal.review-chip-detach.${note.id}`}
              >
                <X className="h-2.5 w-2.5" />
              </button>
            </span>
          );
        })}
      </div>
      {chipError && <div className="text-[10px] text-red-400">{chipError}</div>}
      <div className="mt-1 flex items-center gap-1">
        <input
          value={bar.freeText}
          onChange={(e) => dispatch({ type: "setText", text: e.target.value })}
          placeholder="Anything else to say with these notes (optional)"
          className="min-w-0 flex-1 rounded border border-[#2a2d3d] bg-[#1a1b26] px-1.5 py-0.5 text-[11px] text-[#c0caf5] placeholder:text-zinc-600 focus:border-[#7aa2f7] focus:outline-none"
          data-ui-bridge-id="terminal.review-free-text"
        />
        <button
          type="button"
          onClick={() => dispatch({ type: "togglePreview" })}
          disabled={!composed}
          className="inline-flex shrink-0 items-center gap-1 rounded border border-[#2a2d3d] px-1.5 py-0.5 text-[10px] text-zinc-300 hover:bg-white/5 disabled:opacity-40"
          aria-pressed={bar.previewOpen}
          data-ui-bridge-id="terminal.review-preview-toggle"
          title="Show the exact text that will be sent"
        >
          {bar.previewOpen ? <EyeOff className="h-3 w-3" /> : <Eye className="h-3 w-3" />}
          Preview exact text
        </button>
        <button
          type="button"
          onClick={() => void go("send")}
          disabled={!composed || bar.busy !== null}
          className="inline-flex shrink-0 items-center gap-1 rounded border border-[#7aa2f7]/50 px-1.5 py-0.5 text-[10px] text-[#7aa2f7] hover:bg-[#7aa2f7]/10 disabled:opacity-40"
          data-ui-bridge-id="terminal.review-send"
          title="Send the notes as the session's next prompt"
        >
          <Send className="h-3 w-3" />
          {bar.busy === "send" ? "Sending…" : "Send"}
        </button>
        {insertable && (
          <button
            type="button"
            onClick={() => void go("insert")}
            disabled={!composed || bar.busy !== null}
            className="inline-flex shrink-0 items-center gap-1 rounded border border-[#2a2d3d] px-1.5 py-0.5 text-[10px] text-zinc-300 hover:bg-white/5 disabled:opacity-40"
            data-ui-bridge-id="terminal.review-insert"
            title="Type the text into the terminal WITHOUT pressing Enter. Sending it is yours; the notes stay attached."
          >
            <TextCursorInput className="h-3 w-3" />
            {bar.busy === "insert" ? "Inserting…" : "Insert without sending"}
          </button>
        )}
      </div>
      {insertable && (
        <div className="mt-0.5 text-[9px] text-zinc-600">
          Insert types the text without Enter — delivery is yours, and the notes stay attached.
        </div>
      )}
      {bar.previewOpen && composed && (
        <pre
          className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded border border-[#2a2d3d] bg-black/30 p-1.5 font-mono text-[10px] text-zinc-300"
          data-ui-bridge-id="terminal.review-send-preview"
          data-review-marker={composed.marker}
          data-orphaned-notes={composed.orphanedNoteIds.length || undefined}
        >
          {composed.text}
        </pre>
      )}
      {bar.result && (
        <div
          className={cn(
            "mt-0.5 flex items-center gap-1 text-[10px]",
            bar.result.kind === "error" && !bar.result.partial && "text-red-400",
            bar.result.kind === "error" && bar.result.partial && "text-amber-300",
            bar.result.kind === "deliveredUnrecorded" && "text-amber-300",
            (bar.result.kind === "sent" || bar.result.kind === "inserted") && "text-zinc-400",
          )}
          data-review-send-result={bar.result.kind}
          data-review-send-partial={
            bar.result.kind === "error" && bar.result.partial ? "true" : undefined
          }
        >
          {(bar.result.kind === "sent" ||
            bar.result.kind === "inserted" ||
            bar.result.kind === "deliveredUnrecorded") && (
            <ClipboardCheck className="h-3 w-3 shrink-0" />
          )}
          <span
            // A warning the operator must act on is never cut off.
            className={cn(
              bar.result.kind === "deliveredUnrecorded" ||
                (bar.result.kind === "error" && bar.result.partial)
                ? "break-words"
                : "truncate",
            )}
            title={sendResultLabel(bar.result)}
          >
            {sendResultLabel(bar.result)}
          </span>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Badge
// ---------------------------------------------------------------------------

/** The tab's review badge: `n unread`, `?`, or nothing. */
export function SessionReviewBadge({
  state,
  onClick,
}: {
  state: ReviewBadgeState;
  onClick?: () => void;
}) {
  if (state.kind === "none") return null;
  const unknown = state.kind === "unknown";
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick?.();
      }}
      onMouseDown={(e) => e.stopPropagation()}
      className={cn(
        "shrink-0 rounded px-1 text-[9px] leading-[14px]",
        unknown
          ? "bg-fuchsia-500/15 text-fuchsia-300"
          : state.unread > 0
            ? "bg-sky-500/15 text-sky-300"
            : "bg-zinc-500/15 text-zinc-400",
        !unknown && state.stale && "italic",
      )}
      title={state.title}
      data-ui-bridge-id="terminal.review-badge"
      data-review-badge={state.kind}
      data-review-unread={unknown ? undefined : state.unread}
    >
      {unknown ? "?" : state.text}
    </button>
  );
}

/**
 * The zone title-bar control: the review toggle (beside the prompts toggle)
 * followed by the tab's unread badge. Rendered at every title-bar mount site.
 */
export function SessionReviewToggle({
  open,
  onToggle,
  badge,
  iconClassName = "w-2.5 h-2.5",
}: {
  open: boolean;
  onToggle: () => void;
  badge: ReviewBadgeState;
  iconClassName?: string;
}) {
  return (
    <>
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggle();
        }}
        onMouseDown={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
        className={`p-0.5 rounded transition-colors shrink-0 ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/15"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title={open ? "Hide review" : "Review this session's changes"}
        aria-pressed={open}
        data-ui-bridge-id="terminal.review-toggle"
      >
        <FileDiff className={iconClassName} />
      </button>
      <SessionReviewBadge state={badge} onClick={open ? undefined : onToggle} />
    </>
  );
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

export function SessionReviewPanel({
  handle,
  terminalId,
  orientation,
  onClose,
  topOffsetPx = 0,
  heightPx = REVIEW_PANEL_GEOMETRY.topHeightPx,
}: {
  handle: SessionReviewHandle;
  /** The PTY tab the send bar writes into. */
  terminalId: string;
  orientation: PromptsPanelOrientation;
  onClose: () => void;
  /** Distance from the top of the positioned parent (title bar + filter bar). */
  topOffsetPx?: number;
  /** Rendered height of the `"top"` strip; the caller reserves the same body padding. */
  heightPx?: number;
}) {
  const { hunkReview, error } = useHunkReviewBinding(handle);
  const target = useMemo<ReviewTarget>(() => ({ terminalId }), [terminalId]);
  const isTop = orientation === "top";
  const frame = isTop ? "absolute left-0 right-0 border-b" : "absolute right-0 bottom-0 border-l";
  const frameStyle = isTop
    ? { top: `${topOffsetPx}px`, height: `${heightPx}px` }
    : { top: `${topOffsetPx}px`, width: `${REVIEW_PANEL_GEOMETRY.rightWidthPx}px` };
  return (
    <div
      data-ui-bridge-id="terminal.review-panel"
      data-testid="zone-review-panel"
      data-orientation={orientation}
      data-session-id={handle.sessionId ?? undefined}
      // z-6, like the prompts panel: above the terminal body, below the zone
      // title bar (z-10) whose dropdowns must stay reachable.
      className={`${frame} z-[6] flex flex-col border-[#2a2d3d] bg-[#1a1b26] text-[#a9b1d6]`}
      style={frameStyle}
      // A review surface layered over a terminal: without these the zone's
      // mousedown handler steals focus and keystrokes go back to the PTY.
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
      onWheel={(e) => e.stopPropagation()}
    >
      <div className="flex shrink-0 items-center gap-1.5 border-b border-[#2a2d3d] bg-[#13141f] px-2 py-0.5">
        <span className="text-[10px] font-medium text-[#7aa2f7]">Review</span>
        {error && (
          <span className="truncate text-[10px] text-red-400" title={error}>
            {error}
          </span>
        )}
        {/* Left-anchored in the column, right-anchored in the strip — the
            zone's hover-action cluster owns the top-right corner (see
            ZonePromptsPanel). */}
        <div className={`flex items-center gap-1 ${isTop ? "ml-auto" : ""}`}>
          <button
            type="button"
            onClick={handle.refresh}
            className="rounded p-0.5 text-zinc-500 hover:bg-white/5 hover:text-zinc-300"
            title="Re-read changes and notes"
          >
            <RefreshCw className="h-2.5 w-2.5" />
          </button>
          <button
            type="button"
            onClick={onClose}
            className="rounded p-0.5 text-zinc-500 hover:bg-white/5 hover:text-zinc-300"
            title="Hide review"
            data-ui-bridge-id="terminal.review-panel-close"
          >
            <X className="h-2.5 w-2.5" />
          </button>
        </div>
      </div>
      <div className="min-h-0 flex-1">
        <FileChangesPanel
          read={handle.changes}
          onRefresh={handle.refreshChanges}
          review={hunkReview}
          emptyText="No changed files yet — the session has not touched a file the runner saw."
        />
      </div>
      <ReviewNotesList handle={handle} />
      <ReviewSendBar handle={handle} target={target} />
    </div>
  );
}
