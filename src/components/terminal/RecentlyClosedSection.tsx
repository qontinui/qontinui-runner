/**
 * Recently closed — Chrome's recently-closed tabs for sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). Rendered at the top of the Previous view: every close the store
 * recorded, newest first, each with Resume (which also puts it back in its
 * group). The list is bounded, so it states `shown of total`; an unknown store
 * says so rather than reading as "nothing was closed".
 */

import { useState } from "react";
import { RotateCcw } from "lucide-react";

import { sessionOrgControlAttrs } from "./sessionOrgControls";
import type { ClosedSession } from "./sessionOrgTypes";
import type { RecentlyClosedView, ReopenResult } from "./useRecentlyClosed";

export interface RecentlyClosedSectionProps {
  view: RecentlyClosedView;
  onReopen: (entry: ClosedSession) => Promise<ReopenResult>;
}

const RESULT_COLOR: Record<ReopenResult["state"], string> = {
  done: "#9ece6a",
  "not-done": "#e0af68",
  unknown: "#565f89",
};

function ago(at: number, now: number): string {
  const m = Math.max(0, Math.round((now - at) / 60_000));
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h}h ago` : `${Math.round(h / 24)}d ago`;
}

export function RecentlyClosedSection({ view, onReopen }: RecentlyClosedSectionProps) {
  const [results, setResults] = useState<Record<string, ReopenResult | "pending">>({});
  const { entries, total } = view;
  if (entries !== null && entries.length === 0) return null;
  const now = Date.now();

  return (
    <div
      {...sessionOrgControlAttrs("recentlyClosed")}
      data-shown-count={entries?.length ?? "unknown"}
      data-total-count={total ?? "unknown"}
      className="border-b border-[#2a2d3d]"
    >
      <div className="flex items-center gap-2 px-3 pt-2 pb-1">
        <span className="text-[10px] font-medium text-[#565f89]">
          Recently closed
          {entries !== null && (
            <span className="ml-1 text-[#414868]">
              ({entries.length === total ? entries.length : `${entries.length} of ${total}`})
            </span>
          )}
        </span>
        <div className="flex-1 h-px bg-[#2a2d3d]" />
      </div>
      {entries === null ? (
        <p className="px-3 pb-2 text-[10px] text-[#565f89]" title={view.unknownReason ?? undefined}>
          Recently closed: unknown — {view.unknownReason}
        </p>
      ) : (
        <ul className="max-h-48 overflow-y-auto scrollbar-dark">
          {entries.map((entry) => {
            const r = results[entry.sessionKey];
            return (
              <li
                key={`${entry.sessionKey}:${entry.closedAt}`}
                data-session-key={entry.sessionKey}
                className="px-3 py-1 text-[11px]"
              >
                <div className="flex items-center gap-2 min-w-0">
                  <span className="truncate flex-1 text-[#a9b1d6]" title={entry.title}>
                    {entry.title || entry.sessionKey}
                  </span>
                  <span className="shrink-0 text-[10px] text-[#565f89]">
                    {ago(entry.closedAt, now)}
                  </span>
                  <button
                    {...sessionOrgControlAttrs("recentlyClosedResume", entry.sessionKey)}
                    disabled={entry.resumeCmd === null || r === "pending"}
                    title={
                      entry.resumeCmd === null
                        ? "Closed with no resumable transcript"
                        : "Resume it in a new tab, back in its group"
                    }
                    onClick={() => {
                      setResults((prev) => ({ ...prev, [entry.sessionKey]: "pending" }));
                      void onReopen(entry).then((res) =>
                        setResults((prev) => ({ ...prev, [entry.sessionKey]: res })),
                      );
                    }}
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#9ece6a] hover:bg-[#9ece6a]/15 disabled:opacity-40"
                  >
                    <RotateCcw className="w-3 h-3" />
                    {r === "pending" ? "Resuming…" : "Resume"}
                  </button>
                </div>
                {r && r !== "pending" && (
                  <span
                    className="block text-[10px]"
                    data-reopen-state={r.state}
                    style={{ color: RESULT_COLOR[r.state] }}
                  >
                    {r.detail}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
