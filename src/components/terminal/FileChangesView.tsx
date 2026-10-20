/**
 * The per-file diff list — `DiffView`, `FileChangeRow`, `ChangeList`,
 * `FileChangesPanel` — shared by the Conductor worker cell (its Changes pane)
 * and the PTY tab's `SessionReviewPanel`. One component, two hosts: the review
 * panel reuses it rather than forking it (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 4).
 *
 * Review affordances are an OPTIONAL `review` prop threaded down to each hunk:
 * an unread dot that is also the explicit, reversible read toggle, an "add
 * note" button on the hunk header, a note count, and — the automatic trigger —
 * a hunk scrolled PAST inside an expanded file's diff is marked read (an
 * `IntersectionObserver` rooted on that diff's scroll box; see
 * `scrolledPast`). A hunk the operator explicitly marked unread is never
 * re-marked by a scroll. Without the prop the list renders exactly as before.
 */

import { useEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { CircleDot, MessageSquarePlus, RefreshCw } from "lucide-react";
import { cn } from "@/lib/utils";
import { hunkKeysForFile, excerptForHunk } from "./sessionReview";
import type { ReviewNoteRow } from "./sessionReviewApi";
import { notesForHunk, scrolledPast } from "./sessionReviewView";
import {
  baseLabel,
  countChanged,
  diffHunks,
  diffStat,
  noDiffReason,
  orderChanges,
  shortPath,
  type DiffHunk,
  type FileChangesRead,
  type SessionFileChange,
  type SessionFileChangesResponse,
} from "./workerFileChanges";

export function formatClock(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(
    d.getSeconds(),
  ).padStart(2, "0")}`;
}

/** What a note is being drafted against. */
export interface NoteDraftTarget {
  filePath: string;
  hunkKey: string;
  hunkHeader: string;
  excerpt: string;
}

/** The review affordances a host passes down. Absent → a plain diff list. */
export interface HunkReview {
  /** Hunk keys marked read; `null` when the read state is UNKNOWN (no read succeeded). */
  readKeys: ReadonlySet<string> | null;
  /** Visible (non-discarded) notes, for the per-hunk count. */
  notes: readonly ReviewNoteRow[];
  onToggleRead: (filePath: string, hunkKey: string, read: boolean) => void;
  /** A hunk scrolled past in an expanded diff. The host decides whether it marks. */
  onScrolledPast: (filePath: string, hunkKey: string) => void;
  onAddNote: (target: NoteDraftTarget) => void;
  /** The hunk whose note draft is open, and the editor to render under its header. */
  draftKey: string | null;
  draftSlot: ReactNode;
}

/** Short, stable id fragment for a hunk's UI Bridge ids. */
export function hunkIdFragment(hunkKey: string): string {
  return hunkKey.slice(0, 12);
}

type HunkReadState = "read" | "unread" | "unknown";

function readStateOf(readKeys: ReadonlySet<string> | null, key: string): HunkReadState {
  if (!readKeys) return "unknown";
  return readKeys.has(key) ? "read" : "unread";
}

export function DiffView({
  hunks,
  filePath,
  review,
  scrollRootRef,
}: {
  hunks: DiffHunk[];
  /** Required for review keys; the plain worker view may omit it. */
  filePath?: string;
  review?: HunkReview;
  /** The scroll box the diff lives in — the root the scroll-past observer watches. */
  scrollRootRef?: RefObject<HTMLElement | null>;
}) {
  // Keyed on whether there IS a review, not on the review object: the keys
  // depend only on the hunks, and the review changes on every mark.
  const hasReview = review !== undefined;
  const keys = useMemo(
    () => (hasReview && filePath !== undefined ? hunkKeysForFile(filePath, hunks) : null),
    [hasReview, filePath, hunks],
  );
  const hunkEls = useRef(new Map<string, HTMLElement>());
  // The observer calls the LATEST handler through a ref, so a mark (which
  // changes the handler's read set) does not tear the observer down and
  // rebuild it over every hunk.
  const onScrolledPast = review?.onScrolledPast;
  const onScrolledPastRef = useRef(onScrolledPast);
  useEffect(() => {
    onScrolledPastRef.current = onScrolledPast;
  }, [onScrolledPast]);
  const observes = onScrolledPast !== undefined;

  useEffect(() => {
    const root = scrollRootRef?.current;
    if (!keys || !observes || filePath === undefined || !root) return;
    if (typeof IntersectionObserver === "undefined") return;
    const byEl = new Map<Element, string>();
    for (const [key, el] of hunkEls.current) byEl.set(el, key);
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const key = byEl.get(entry.target);
          if (!key) continue;
          if (
            scrolledPast({
              isIntersecting: entry.isIntersecting,
              bottom: entry.boundingClientRect.bottom,
              rootTop: entry.rootBounds?.top ?? null,
            })
          ) {
            onScrolledPastRef.current?.(filePath, key);
          }
        }
      },
      { root, threshold: 0 },
    );
    for (const el of byEl.keys()) observer.observe(el);
    return () => observer.disconnect();
  }, [keys, observes, filePath, scrollRootRef]);

  return (
    <div className="m-0 overflow-x-auto font-mono text-[10px] leading-4">
      {hunks.map((hunk, hi) => {
        const key = keys?.[hi];
        const readState = key && review ? readStateOf(review.readKeys, key) : null;
        const noteCount = key && review ? notesForHunk(review.notes, key).length : 0;
        return (
          <div
            key={key ?? hi}
            ref={(el) => {
              if (!key) return;
              if (el) hunkEls.current.set(key, el);
              else hunkEls.current.delete(key);
            }}
            data-ui-bridge-id={key ? `terminal.review-hunk.${hunkIdFragment(key)}` : undefined}
            data-hunk-key={key}
            data-hunk-read={readState ?? undefined}
          >
            <div className="flex items-center gap-1 text-[#7aa2f7]/80">
              {key && review && filePath !== undefined && readState && (
                <button
                  type="button"
                  onClick={() => review.onToggleRead(filePath, key, readState !== "read")}
                  disabled={readState === "unknown"}
                  className={cn(
                    "inline-flex h-3 w-3 shrink-0 items-center justify-center rounded-full",
                    readState === "unread" && "text-sky-400 hover:bg-sky-400/20",
                    readState === "read" && "text-zinc-600 hover:bg-white/10",
                    readState === "unknown" && "cursor-default text-fuchsia-300",
                  )}
                  title={
                    readState === "unknown"
                      ? "UNKNOWN — the read state could not be read"
                      : readState === "unread"
                        ? "Unread — click to mark read"
                        : "Read — click to mark unread"
                  }
                  aria-pressed={readState === "read"}
                  data-ui-bridge-id={`terminal.review-hunk-read.${hunkIdFragment(key)}`}
                >
                  {readState === "unknown" ? (
                    "?"
                  ) : (
                    <CircleDot
                      className={cn("h-2.5 w-2.5", readState === "read" && "opacity-40")}
                    />
                  )}
                </button>
              )}
              <span className="whitespace-pre">{hunk.header}</span>
              {noteCount > 0 && (
                <span className="text-amber-300" title={`${noteCount} note(s) on this hunk`}>
                  {noteCount} note{noteCount === 1 ? "" : "s"}
                </span>
              )}
              {key && review && filePath !== undefined && (
                <button
                  type="button"
                  onClick={() =>
                    review.onAddNote({
                      filePath,
                      hunkKey: key,
                      hunkHeader: hunk.header,
                      excerpt: excerptForHunk(hunk),
                    })
                  }
                  className="ml-auto inline-flex shrink-0 items-center gap-0.5 rounded px-1 text-zinc-500 hover:bg-white/5 hover:text-amber-300"
                  title="Add a note on this hunk"
                  data-ui-bridge-id={`terminal.review-add-note.${hunkIdFragment(key)}`}
                >
                  <MessageSquarePlus className="h-3 w-3" />
                  note
                </button>
              )}
            </div>
            {key && review?.draftKey === key && review.draftSlot}
            {hunk.lines.map((line, li) => (
              <div
                key={li}
                className={cn(
                  "whitespace-pre",
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
        );
      })}
    </div>
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

export function FileChangeRow({
  change,
  review,
}: {
  change: SessionFileChange;
  review?: HunkReview;
}) {
  const hunks = useMemo(() => diffHunks(change), [change]);
  const stat = useMemo(() => diffStat(hunks), [hunks]);
  const hasReview = review !== undefined;
  const keys = useMemo(
    () => (hasReview && hunks ? hunkKeysForFile(change.filePath, hunks) : null),
    [hasReview, hunks, change.filePath],
  );
  const reason = noDiffReason(change);
  const [open, setOpen] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const expandable = hunks !== null && hunks.length > 0;
  const unread =
    keys && review?.readKeys ? keys.filter((k) => !review.readKeys?.has(k)).length : null;
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
        {unread !== null && unread > 0 && (
          <span className="shrink-0 text-[10px] text-sky-400" data-file-unread={unread}>
            {unread} unread
          </span>
        )}
        {expandable && (
          <span className="ml-auto shrink-0 font-mono text-[10px]">
            <span className="text-emerald-400">+{stat.additions}</span>{" "}
            <span className="text-red-400">-{stat.deletions}</span>
          </span>
        )}
        {reason && <span className="ml-auto shrink-0 text-[10px] text-zinc-500">{reason}</span>}
      </button>
      {open && hunks && (
        <div
          ref={scrollRef}
          className="max-h-64 overflow-y-auto border-t border-[#2a2d3d] bg-black/20 px-2 py-1"
        >
          <DiffView
            hunks={hunks}
            filePath={change.filePath}
            review={review}
            scrollRootRef={scrollRef}
          />
        </div>
      )}
    </li>
  );
}

function ChangeList({
  response,
  review,
  emptyText,
}: {
  response: SessionFileChangesResponse;
  review?: HunkReview;
  emptyText: string;
}) {
  if (response.files.length === 0) {
    return <div className="px-2 py-3 text-[11px] text-zinc-500">{emptyText}</div>;
  }
  return (
    <>
      <ul className="m-0 list-none p-0">
        {orderChanges(response.files).map((change) => (
          <FileChangeRow key={change.filePath} change={change} review={review} />
        ))}
      </ul>
      {response.filesTruncated && (
        <div
          className="border-t border-[#2a2d3d] px-2 py-1.5 text-[10px] text-fuchsia-300"
          data-file-changes-cut="true"
        >
          list cut at {response.files.length} files — {response.omittedFiles} more path
          {response.omittedFiles === 1 ? "" : "s"} this session touched are NOT shown
        </div>
      )}
    </>
  );
}

const WORKER_EMPTY_TEXT =
  "No snapshotted edits yet — the worker has not written to any file the runner saw.";

export function FileChangesPanel({
  read,
  onRefresh,
  review,
  emptyText = WORKER_EMPTY_TEXT,
}: {
  read: FileChangesRead;
  onRefresh: () => void;
  review?: HunkReview;
  /** What an empty (successfully read) list says. */
  emptyText?: string;
}) {
  // During an ordinary refresh the previous list stays up unlabelled (the
  // header already says "reading…"); only a FAILED read marks it stale.
  const shown =
    read.status === "ok" ? read.response : read.status === "loading" ? read.previous : null;
  const stale = read.status === "error" ? read.previous : null;
  const base = shown ?? stale;
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
        {base?.baseKind && (
          <span className="truncate text-zinc-400" data-review-base={base.baseKind}>
            vs {baseLabel(base.baseKind, base.baseSha)}
          </span>
        )}
        <button
          type="button"
          onClick={onRefresh}
          className="ml-auto inline-flex items-center gap-1 rounded px-1 hover:bg-white/5 hover:text-zinc-300"
          title="Re-read the session's file changes"
        >
          <RefreshCw className="h-3 w-3" />
          refresh
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {shown && <ChangeList response={shown} review={review} emptyText={emptyText} />}
        {stale && (
          <>
            <div className="px-2 pt-1 text-[10px] text-zinc-600">
              last successful read {formatClock(stale.readAtMs)} — may be stale
            </div>
            <ChangeList response={stale} review={review} emptyText={emptyText} />
          </>
        )}
        {read.status === "error" && !stale && (
          <div className="px-2 py-3 text-[11px] text-fuchsia-300">
            UNKNOWN — nothing has been read successfully for this session yet.
          </div>
        )}
      </div>
    </div>
  );
}
