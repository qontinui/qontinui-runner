/**
 * "Before the last restart" — the top section of Previous Sessions (plan
 * `2026-10-04-runner-session-roster-restore-picker`, Phases 4-5).
 *
 * Reviewing a restart: one row per session on that boot's roster — name,
 * account, worktree / plan, WIP, what became of it (`back` / `missing:
 * <reason>` / `needs-account` / `finished` / `waiting: …`) and when it was
 * last seen — with a checkbox (pre-checked for unfinished, missing, resumable
 * rows), "Resume selected (n)" (sequential, per-row progress, Retry), an
 * account chooser for a session whose account is unknown, and Finish/Unfinish.
 *
 * In the normal running state (no restart left to review) it is the
 * pre-rebuild preview instead: what a restart NOW would bring back, when the
 * roster was last captured, "Capture now", and Finish per session — the
 * "select which ones come back" step, done before the rebuild.
 *
 * State lives in `SinceRestartContext`; decisions in `sinceRestart.ts`.
 */

import { useEffect, useRef, useState } from "react";
import { Camera, History, RefreshCw, RotateCcw, TerminalSquare } from "lucide-react";
import { useUIComponent } from "@qontinui/ui-bridge";
import type { LedgerEntry, LedgerGeneration, ResumeAccount } from "@/lib/session-ledger";
import { displayNameOf } from "@/lib/session-ledger";
import { CURRENT_ROSTER, useSinceRestart, type SinceRestartValue } from "./SinceRestartContext";
import {
  SINCE_RESTART_CAPTURE_ID,
  SINCE_RESTART_DONE_ID,
  SINCE_RESTART_GENERATION_ID,
  SINCE_RESTART_PREVIEW_TOGGLE_ID,
  SINCE_RESTART_REFRESH_ID,
  SINCE_RESTART_RESUME_SELECTED_ID,
  SINCE_RESTART_SECTION_ID,
  describeProgress,
  finishButtonModel,
  formatAgo,
  previewModel,
  sinceRestartAccountId,
  sinceRestartCheckId,
  sinceRestartFinishId,
  sinceRestartRetryId,
  sinceRestartRowId,
  type ResumeProgress,
  type RowState,
  type SinceRestartRow,
} from "./sinceRestart";

/** How often the "Xs ago" labels re-read the clock. */
const CLOCK_TICK_MS = 15_000;

/** Account chooser value meaning the default home (`~/.claude`, no `CLAUDE_CONFIG_DIR`). */
const DEFAULT_ACCOUNT = "__default__";

const CHIP: Record<RowState, string> = {
  back: "bg-[#9ece6a]/15 text-[#9ece6a]",
  missing: "bg-[#e0af68]/15 text-[#e0af68]",
  "needs-account": "bg-[#f7768e]/15 text-[#f7768e]",
  finished: "bg-[#565f89]/15 text-[#565f89]",
  "closed-by-user": "bg-[#565f89]/15 text-[#a9b1d6]",
  "waiting-page": "bg-[#7aa2f7]/15 text-[#7aa2f7]",
  "waiting-drain": "bg-[#7aa2f7]/15 text-[#7aa2f7]",
};

function progressChip(p: ResumeProgress): string {
  switch (p.state) {
    case "queued":
      return "bg-[#565f89]/15 text-[#a9b1d6]";
    case "resuming":
      return "bg-[#7aa2f7]/15 text-[#7aa2f7]";
    case "back":
      return "bg-[#9ece6a]/15 text-[#9ece6a]";
    case "failed":
      return "bg-[#f7768e]/15 text-[#f7768e]";
  }
}

/** The last path segment — `C:/claude/.claude-gmail` → `.claude-gmail`. */
function basename(path: string): string {
  return (
    path
      .replace(/[\\/]+$/, "")
      .split(/[\\/]/)
      .pop() ?? path
  );
}

/** What the account badge says. */
function accountText(label: string | null, account: ResumeAccount): string {
  if (label) return label;
  if (!account.known) return "account unknown";
  return account.configDir ? basename(account.configDir) : "default";
}

/**
 * "2 back · 3 did not come back · 1 waiting · 1 finished" — waiting rows (a
 * page not opened yet, or a drain) are not counted as misses.
 */
function rowSummary(rows: readonly SinceRestartRow[]): string {
  const count = (pred: (r: SinceRestartRow) => boolean) => rows.filter(pred).length;
  const parts = [
    `${count((r) => r.state === "back")} back`,
    `${count((r) => r.state === "missing" || r.state === "needs-account")} did not come back`,
  ];
  const waiting = count((r) => r.state === "waiting-page" || r.state === "waiting-drain");
  if (waiting > 0) parts.push(`${waiting} waiting`);
  const closedByUser = count((r) => r.state === "closed-by-user");
  if (closedByUser > 0) parts.push(`${closedByUser} closed by you`);
  parts.push(`${count((r) => r.state === "finished")} finished`);
  return parts.join(" · ");
}

function generationLabel(g: LedgerGeneration): string {
  const when = new Date(g.rotatedAtMs).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
  return `Restart ${when} · ${g.missing.length} missing`;
}

/** Shared Finish/Unfinish button. */
function FinishButton({
  id,
  finished,
  roster,
  controlId,
}: {
  id: string;
  finished: boolean;
  roster: SinceRestartValue;
  controlId: string;
}) {
  const model = finishButtonModel(finished, roster.finishOps.get(id));
  return (
    <button
      type="button"
      data-ui-bridge-id={controlId}
      onClick={() => roster.setFinished(id, model.target)}
      disabled={model.disabled}
      title={model.title}
      className="px-1.5 py-0.5 rounded text-[10px] font-medium transition-colors bg-[#414868]/30 text-[#a9b1d6] hover:bg-[#414868]/50 disabled:opacity-50"
    >
      {model.label}
    </button>
  );
}

function ReviewRow({
  row,
  roster,
  now,
}: {
  row: SinceRestartRow;
  roster: SinceRestartValue;
  now: number;
}) {
  const { outcome } = row;
  const id = outcome.claudeSessionId;
  const progress = roster.progress.get(id);
  const inFlight =
    progress?.state === "queued" || progress?.state === "resuming" || progress?.state === "back";
  const checked = roster.selected.has(id) && row.selectable;
  const where = outcome.planSlug ?? (outcome.worktreePath ? basename(outcome.worktreePath) : null);

  return (
    <div
      data-ui-bridge-id={sinceRestartRowId(id)}
      data-session-id={id}
      data-row-state={row.state}
      className="px-3 py-1.5 hover:bg-[#1a1b26]"
    >
      <div className="flex items-center gap-1.5">
        <input
          type="checkbox"
          data-ui-bridge-id={sinceRestartCheckId(id)}
          checked={checked}
          disabled={!row.selectable || inFlight}
          onChange={() => roster.toggleSelected(id)}
          title={row.blockedReason ?? "Include in Resume selected"}
          className="w-3 h-3 shrink-0 accent-[#9ece6a]"
        />
        <span
          className="text-xs text-[#c0caf5] font-medium truncate flex-1"
          title={`${outcome.displayName}\n${id}`}
        >
          {outcome.displayName}
        </span>
        <span
          className={`px-1 rounded text-[9px] font-medium shrink-0 ${
            row.account.known ? "bg-[#7aa2f7]/10 text-[#7aa2f7]" : "bg-[#f7768e]/10 text-[#f7768e]"
          }`}
          title={
            row.account.known
              ? `${row.accountChosen ? "chosen account" : "account"}: ${row.account.configDir ?? "default (~/.claude)"}`
              : "account unknown — resuming under the default would fail as 'No conversation found'"
          }
        >
          {accountText(outcome.accountLabel, row.account)}
        </span>
      </div>

      <div className="flex items-center gap-1 text-[10px] text-[#414868] ml-[18px] flex-wrap mt-0.5">
        <span
          className={`px-1 rounded text-[9px] font-medium ${CHIP[row.state]}`}
          title={row.detail ?? row.label}
        >
          {row.label}
        </span>
        {progress && (
          <span
            className={`px-1 rounded text-[9px] font-medium ${progressChip(progress)}`}
            title={describeProgress(progress)}
          >
            {describeProgress(progress)}
          </span>
        )}
        {outcome.wipState === "captured" && (
          <span
            className="px-1 rounded text-[9px] font-medium bg-[#bb9af7]/15 text-[#bb9af7]"
            title={`uncommitted work snapshotted${outcome.wipRef ? ` to ${outcome.wipRef}` : ""}`}
          >
            captured
          </span>
        )}
        {where && (
          <span className="truncate max-w-[140px]" title={outcome.worktreePath ?? where}>
            {where}
          </span>
        )}
        {outcome.lastSeenAt !== null && (
          <span title={new Date(outcome.lastSeenAt).toLocaleString()}>
            · {formatAgo(outcome.lastSeenAt, now)}
          </span>
        )}
      </div>

      <div className="flex items-center gap-1.5 ml-[18px] mt-1">
        {row.state === "needs-account" && (
          <select
            data-ui-bridge-id={sinceRestartAccountId(id)}
            value=""
            onChange={(e) => {
              const v = e.target.value;
              if (!v) return;
              roster.chooseAccount(
                id,
                v === DEFAULT_ACCOUNT
                  ? { known: true, configDir: null }
                  : { known: true, configDir: v },
              );
            }}
            className="bg-[#1a1b26] border border-[#2a2d3d] rounded text-[10px] text-[#c0caf5] px-1 py-0.5 max-w-[200px]"
            title={row.detail ?? "Choose the account this session ran under"}
          >
            <option value="">
              {roster.accountRoster === null ? "Reading accounts…" : "Choose account…"}
            </option>
            {(roster.accountRoster ?? []).map((dir) => (
              <option key={dir} value={dir}>
                {basename(dir)}
              </option>
            ))}
            <option value={DEFAULT_ACCOUNT}>default (~/.claude)</option>
          </select>
        )}
        {progress?.state === "failed" && row.selectable && (
          <button
            type="button"
            data-ui-bridge-id={sinceRestartRetryId(id)}
            onClick={() => roster.retry(id)}
            className="flex items-center gap-1 px-1.5 py-0.5 rounded border border-[#f7768e]/40 text-[#f7768e] hover:bg-[#f7768e]/15 text-[10px]"
            title="Resume this session again"
          >
            <RotateCcw className="w-2.5 h-2.5" />
            Retry
          </button>
        )}
        <div className="flex-1" />
        <FinishButton
          id={id}
          finished={outcome.finished}
          roster={roster}
          controlId={sinceRestartFinishId(id)}
        />
      </div>
    </div>
  );
}

function PreviewRow({ entry, roster }: { entry: LedgerEntry; roster: SinceRestartValue }) {
  const id = entry.claudeSessionId;
  return (
    <div
      data-ui-bridge-id={sinceRestartRowId(id)}
      data-session-id={id}
      data-row-state={entry.finished ? "finished" : "will-return"}
      className="flex items-center gap-1.5 px-3 py-1 hover:bg-[#1a1b26]"
    >
      <span
        className={`text-xs truncate flex-1 ${entry.finished ? "text-[#565f89] line-through" : "text-[#c0caf5]"}`}
        title={`${displayNameOf(entry)}\n${id}`}
      >
        {displayNameOf(entry)}
      </span>
      {entry.accountLabel && (
        <span className="px-1 rounded text-[9px] font-medium shrink-0 bg-[#7aa2f7]/10 text-[#7aa2f7]">
          {entry.accountLabel}
        </span>
      )}
      <FinishButton
        id={id}
        finished={entry.finished}
        roster={roster}
        controlId={sinceRestartFinishId(id)}
      />
    </div>
  );
}

export function SinceRestartSection() {
  const roster = useSinceRestart();
  const [now, setNow] = useState(() => Date.now());
  const [showPreviewRows, setShowPreviewRows] = useState(false);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), CLOCK_TICK_MS);
    return () => clearInterval(t);
  }, []);

  // The strip's Review lands HERE: bring the section to the top of the list
  // however far the operator had scrolled it.
  const sectionRef = useRef<HTMLDivElement>(null);
  const reviewSeq = roster?.reviewSeq ?? 0;
  useEffect(() => {
    if (reviewSeq > 0) sectionRef.current?.scrollIntoView({ block: "start" });
  }, [reviewSeq]);

  useUIComponent({
    id: "since-restart",
    name: "Before the last restart",
    description:
      "The session roster from before the last runner restart, and what a restart now would bring back.",
    actions: [
      {
        id: "resume-selected",
        label: "Resume selected",
        description:
          "Resume the checked sessions one at a time, each in a new tab with a verified --resume under its account.",
        // `destructive` — types `claude --resume` into new terminals, starting sessions.
        effect: "destructive",
        handler: () => {
          roster?.resumeSelected();
        },
      },
      {
        id: "capture-now",
        label: "Capture now",
        description: "Save the current session roster to disk now.",
        // `write` — persists the roster file; it changes nothing else.
        effect: "write",
        handler: () => {
          roster?.capture();
        },
      },
      {
        id: "refresh",
        label: "Refresh",
        description: "Re-read the session roster.",
        // `read` — a query.
        effect: "read",
        handler: () => {
          roster?.refresh();
        },
      },
    ],
  });

  if (!roster) return null;
  const { report } = roster;
  const reviewing = roster.view !== CURRENT_ROSTER && roster.generation !== null;
  const generations = report?.generations ?? [];

  return (
    <div
      ref={sectionRef}
      data-ui-bridge-id={SINCE_RESTART_SECTION_ID}
      className="border-b border-[#2a2d3d] pb-1"
    >
      {/* Header: title · generation selector · refresh */}
      <div className="flex items-center gap-2 px-3 pt-2 pb-1">
        <History className="w-3 h-3 text-[#565f89]" />
        <span className="text-[10px] font-medium text-[#a9b1d6]">
          {reviewing ? "Before the last restart" : "Session roster"}
        </span>
        <div className="flex-1" />
        {generations.length > 0 && (
          <select
            data-ui-bridge-id={SINCE_RESTART_GENERATION_ID}
            value={roster.view}
            onChange={(e) => roster.setView(e.target.value)}
            className="bg-[#1a1b26] border border-[#2a2d3d] rounded text-[10px] text-[#c0caf5] px-1 py-0.5 max-w-[170px]"
            title="Review an earlier restart, or the roster right now"
          >
            <option value={CURRENT_ROSTER}>Now — if it restarts</option>
            {generations.map((g) => (
              <option key={g.file} value={g.file}>
                {generationLabel(g)}
              </option>
            ))}
          </select>
        )}
        <button
          type="button"
          data-ui-bridge-id={SINCE_RESTART_REFRESH_ID}
          onClick={roster.refresh}
          disabled={roster.loading}
          className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors disabled:opacity-50"
          title="Re-read the session roster"
        >
          <RefreshCw className={`w-3 h-3 ${roster.loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {roster.error && <div className="px-3 py-1 text-[10px] text-[#f7768e]">{roster.error}</div>}
      {!report && !roster.error && (
        <div className="px-3 py-1 text-[10px] text-[#565f89]">Reading the session roster…</div>
      )}

      {report && reviewing && roster.generation && (
        <>
          <div className="px-3 pb-1 text-[10px] text-[#565f89]">
            {rowSummary(roster.rows)}
            {roster.generation.verdict === "unknown" && (
              <span
                className="ml-1 text-[#e0af68]"
                title="This boot latched no restore census, so a session that came back and was then closed counts as missing"
              >
                (misses are an upper bound)
              </span>
            )}
          </div>
          <div className="flex items-center gap-1.5 px-3 pb-1.5">
            <button
              type="button"
              data-ui-bridge-id={SINCE_RESTART_RESUME_SELECTED_ID}
              onClick={roster.resumeSelected}
              disabled={roster.resumable.length === 0}
              className={`flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium transition-colors ${
                roster.resumable.length > 0
                  ? "bg-[#9ece6a]/15 text-[#9ece6a] hover:bg-[#9ece6a]/25"
                  : "bg-[#414868]/20 text-[#414868] cursor-not-allowed"
              }`}
              title="Resume the checked sessions one at a time, each in a new tab under its own account"
            >
              <TerminalSquare className="w-3 h-3" />
              Resume selected ({roster.resumable.length})
            </button>
            {roster.resuming && <span className="text-[10px] text-[#7aa2f7]">resuming…</span>}
            <div className="flex-1" />
            <button
              type="button"
              data-ui-bridge-id={SINCE_RESTART_DONE_ID}
              onClick={roster.markReviewed}
              className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
              title="Done with this restart — show what a restart now would bring back"
            >
              Done reviewing
            </button>
          </div>
          {roster.rows.map((row) => (
            <ReviewRow key={row.outcome.claudeSessionId} row={row} roster={roster} now={now} />
          ))}
        </>
      )}

      {report && !reviewing && (
        <PreviewBody
          roster={roster}
          now={now}
          showRows={showPreviewRows}
          onToggleRows={setShowPreviewRows}
        />
      )}
    </div>
  );
}

function PreviewBody({
  roster,
  now,
  showRows,
  onToggleRows,
}: {
  roster: SinceRestartValue;
  now: number;
  showRows: boolean;
  onToggleRows: (show: boolean) => void;
}) {
  const report = roster.report;
  if (!report) return null;
  const preview = previewModel(report, now);
  const captured = roster.lastCapture;
  const entries = [...preview.unfinished, ...preview.finished];
  return (
    <>
      <div className="px-3 pb-1 text-[11px] text-[#c0caf5] leading-snug">
        {preview.text}
        <span className="text-[#565f89]"> · {preview.savedText}</span>
      </div>
      {preview.savedStale && (
        <div className="px-3 pb-1 text-[10px] text-[#e0af68]">
          The saved roster is behind the current one — it catches up within a minute, or capture it
          now.
        </div>
      )}
      <div className="flex items-center gap-1.5 px-3 pb-1.5">
        <button
          type="button"
          data-ui-bridge-id={SINCE_RESTART_CAPTURE_ID}
          onClick={roster.capture}
          disabled={roster.capturing}
          className="flex items-center gap-1 px-2 py-0.5 rounded text-[10px] font-medium transition-colors bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/25 disabled:opacity-50"
          title="Save the current roster to disk now — it is also saved on its own whenever it changes"
        >
          <Camera className="w-3 h-3" />
          {roster.capturing ? "Capturing…" : "Capture now"}
        </button>
        {captured && !roster.captureError && (
          <span className="text-[10px] text-[#565f89]">
            {captured.persisted
              ? `captured ${captured.ledger.sessions.length} sessions`
              : "unchanged — the saved roster is already current"}
          </span>
        )}
        {roster.captureError && (
          <span className="text-[10px] text-[#f7768e]" title={roster.captureError}>
            capture failed
          </span>
        )}
        <div className="flex-1" />
        {entries.length > 0 && (
          <button
            type="button"
            data-ui-bridge-id={SINCE_RESTART_PREVIEW_TOGGLE_ID}
            onClick={() => onToggleRows(!showRows)}
            className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
            title="Finish a session here to keep the next restart from bringing it back"
          >
            {showRows ? "Hide sessions" : `Choose (${entries.length})`}
          </button>
        )}
      </div>
      {showRows &&
        entries.map((entry) => (
          <PreviewRow key={entry.claudeSessionId} entry={entry} roster={roster} />
        ))}
    </>
  );
}
