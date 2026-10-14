/**
 * The Session Manager's bulk bar and its per-item outcome (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). Shown while rows are selected; every action runs through
 * `sessionBulkActions.ts`, and its outcome lists each session — done, not
 * done with the reason, or unknown — under a summary that never reads "done"
 * for a partial batch.
 */

import { useState } from "react";
import { CheckCircle2, Loader2, TerminalSquare, X } from "lucide-react";

import { summarizeBulkResults, type BulkItemResult, type BulkOutcome } from "./sessionBulkActions";
import { sessionOrgControlAttrs } from "./sessionOrgControls";
import { snoozePresets, type MenuPage } from "./SessionOrgMenus";
import type { SessionGroup } from "./sessionOrgTypes";

export interface SessionBulkBarProps {
  count: number;
  /** "Pinning 4…" while a batch is in flight. */
  running: string | null;
  /** The org's groups; `null` = the store is not known (group actions off). */
  groups: readonly SessionGroup[] | null;
  pages: readonly MenuPage[];
  onClose: () => void;
  onMarkFinished: () => void;
  onAddToGroup: (group: SessionGroup) => void;
  onNewGroup: (title: string) => void;
  onPin: (pinned: boolean) => void;
  onMute: (muted: boolean) => void;
  onSnooze: (until: number | null) => void;
  onMoveToPage: (pageId: string) => void;
  onResume: () => void;
  onClear: () => void;
}

/** The snooze presets by label; the deadline is computed when one is picked. */
const SNOOZE_LABELS = snoozePresets(0).map((p) => p.label);

const BUTTON =
  "px-1.5 py-0.5 rounded text-[10px] font-medium text-[#a9b1d6] hover:bg-[#2a2d3d] disabled:opacity-40 transition-colors";
const SELECT =
  "max-w-[7rem] px-1 py-0.5 rounded text-[10px] bg-[#1a1b26] text-[#a9b1d6] border border-[#2a2d3d] disabled:opacity-40";

export function SessionBulkBar(props: SessionBulkBarProps) {
  const { count, running, groups, pages } = props;
  const busy = running !== null;
  const [naming, setNaming] = useState(false);
  const [title, setTitle] = useState("");

  return (
    <div
      {...sessionOrgControlAttrs("bulkBar")}
      data-selected-count={count}
      data-busy={busy ? "true" : "false"}
      className="flex flex-col gap-1 px-3 py-1.5 bg-[#7aa2f7]/5 border-b border-[#2a2d3d]"
    >
      <div className="flex items-center gap-2">
        <span className="text-[10px] text-[#7aa2f7] font-medium">{count} selected</span>
        {running && (
          <span className="flex items-center gap-1 text-[10px] text-[#a9b1d6]" role="status">
            <Loader2 className="w-3 h-3 animate-spin" />
            {running}
          </span>
        )}
        <div className="flex-1" />
        <button
          {...sessionOrgControlAttrs("bulkClear")}
          onClick={props.onClear}
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          title="Clear selection (Esc)"
          aria-label="Clear selection"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
      <div className="flex flex-wrap items-center gap-1">
        <button
          {...sessionOrgControlAttrs("bulkResume")}
          onClick={props.onResume}
          disabled={busy}
          className={`${BUTTON} flex items-center gap-1 text-[#9ece6a]`}
          title="Resume each selected session that is not running, in a new tab"
        >
          <TerminalSquare className="w-3 h-3" />
          Resume
        </button>
        <button
          {...sessionOrgControlAttrs("bulkFinish")}
          onClick={props.onMarkFinished}
          disabled={busy}
          className={`${BUTTON} flex items-center gap-1`}
          title="Mark each selected session's work finished (/finish-session)"
        >
          <CheckCircle2 className="w-3 h-3" />
          Finished
        </button>
        <button
          {...sessionOrgControlAttrs("bulkPin")}
          onClick={() => props.onPin(true)}
          disabled={busy || !groups}
          className={BUTTON}
        >
          Pin
        </button>
        <button
          {...sessionOrgControlAttrs("bulkUnpin")}
          onClick={() => props.onPin(false)}
          disabled={busy || !groups}
          className={BUTTON}
        >
          Unpin
        </button>
        <button
          {...sessionOrgControlAttrs("bulkMute")}
          onClick={() => props.onMute(true)}
          disabled={busy || !groups}
          className={BUTTON}
          title="Mute the needs-input toast and sound for each selected session"
        >
          Mute
        </button>
        <select
          {...sessionOrgControlAttrs("bulkSnooze")}
          aria-label="Snooze the selected sessions"
          value=""
          disabled={busy || !groups}
          onChange={(e) => {
            const v = e.target.value;
            if (v === "") return;
            if (v === "off") {
              props.onSnooze(null);
              return;
            }
            // The deadline is taken when the operator picks, not when the bar
            // rendered — a bar left open for hours must not snooze into the past.
            const preset = snoozePresets(Date.now()).find((p) => p.label === v);
            if (preset) props.onSnooze(preset.until);
          }}
          className={SELECT}
        >
          <option value="">Snooze…</option>
          {SNOOZE_LABELS.map((label) => (
            <option key={label} value={label}>
              {label}
            </option>
          ))}
          <option value="off">Unsnooze</option>
        </select>
        <select
          {...sessionOrgControlAttrs("bulkAddToGroup")}
          aria-label="Add the selected sessions to a group"
          value=""
          disabled={busy || !groups || groups.length === 0}
          onChange={(e) => {
            const group = groups?.find((g) => g.id === e.target.value);
            if (group) props.onAddToGroup(group);
          }}
          className={SELECT}
        >
          <option value="">Add to group…</option>
          {groups?.map((g) => (
            <option key={g.id} value={g.id}>
              {g.title}
            </option>
          ))}
        </select>
        {naming ? (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              const t = title.trim();
              if (!t) return;
              props.onNewGroup(t);
              setNaming(false);
              setTitle("");
            }}
            className="flex items-center gap-1"
          >
            <input
              {...sessionOrgControlAttrs("bulkNewGroup")}
              autoFocus
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onBlur={() => !title.trim() && setNaming(false)}
              placeholder="Group name"
              aria-label="New group name"
              className="w-24 px-1 py-0.5 rounded text-[10px] bg-[#1a1b26] text-[#c0caf5] border border-[#2a2d3d]"
            />
          </form>
        ) : (
          <button
            {...sessionOrgControlAttrs("bulkNewGroup")}
            onClick={() => setNaming(true)}
            disabled={busy || !groups}
            className={BUTTON}
          >
            New group…
          </button>
        )}
        {pages.length > 0 && (
          <select
            {...sessionOrgControlAttrs("bulkMoveToPage")}
            aria-label="Move the selected sessions to a page"
            value=""
            disabled={busy}
            onChange={(e) => e.target.value && props.onMoveToPage(e.target.value)}
            className={SELECT}
          >
            <option value="">Move to page…</option>
            {pages.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        )}
        <button
          {...sessionOrgControlAttrs("bulkClose")}
          onClick={props.onClose}
          disabled={busy}
          className={`${BUTTON} text-[#f7768e]`}
          title="Close the selected sessions — asks first, and names each one"
        >
          Close…
        </button>
      </div>
    </div>
  );
}

const STATE_COLOR: Record<BulkItemResult["state"], string> = {
  done: "#9ece6a",
  "not-done": "#e0af68",
  unknown: "#565f89",
};

/** A batch's outcome: the summary, then every session that was not plainly done. */
export function SessionBulkOutcome({
  outcome,
  onDismiss,
}: {
  outcome: BulkOutcome;
  onDismiss: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const shortfall = outcome.results.filter((r) => r.state !== "done");
  const listed = expanded ? outcome.results : shortfall;
  return (
    <div
      {...sessionOrgControlAttrs("bulkResults")}
      data-done-count={outcome.results.length - shortfall.length}
      data-total-count={outcome.results.length}
      role="status"
      className="px-3 py-1 text-[10px] text-[#a9b1d6] border-b border-[#2a2d3d]"
    >
      <div className="flex items-center gap-2">
        <span className="flex-1 truncate" title={summarizeBulkResults(outcome)}>
          {summarizeBulkResults(outcome)}
        </span>
        <button
          onClick={() => setExpanded((v) => !v)}
          className="text-[#565f89] hover:text-[#c0caf5]"
        >
          {expanded ? "Less" : "Each"}
        </button>
        <button
          onClick={onDismiss}
          aria-label="Dismiss"
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5]"
        >
          <X className="w-3 h-3" />
        </button>
      </div>
      {listed.length > 0 && (
        <ul className="mt-0.5 space-y-0.5 max-h-32 overflow-y-auto">
          {listed.map((r) => (
            <li key={r.sessionId} data-session-id={r.sessionId} data-result-state={r.state}>
              <span className="text-[#c0caf5]">{r.title}</span>{" "}
              <span style={{ color: STATE_COLOR[r.state] }}>{r.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Chrome's Declutter: `N sessions are finished`, and Review opens the close confirmation. */
export function ReadyToCloseBanner({
  text,
  count,
  onReview,
}: {
  text: string;
  count: number;
  onReview: () => void;
}) {
  return (
    <div
      {...sessionOrgControlAttrs("readyToClose")}
      data-ready-count={count}
      className="flex items-center gap-2 px-3 py-1 text-[10px] text-[#9ece6a] bg-[#9ece6a]/5 border-b border-[#2a2d3d]"
    >
      <CheckCircle2 className="w-3 h-3 shrink-0" />
      <span className="flex-1 truncate" title={text}>
        {text}
      </span>
      <button
        {...sessionOrgControlAttrs("readyToCloseReview")}
        onClick={onReview}
        className="px-1.5 py-0.5 rounded font-medium hover:bg-[#9ece6a]/15"
        title="List them with why each is finished, and close the ones you keep ticked"
      >
        Review…
      </button>
    </div>
  );
}
