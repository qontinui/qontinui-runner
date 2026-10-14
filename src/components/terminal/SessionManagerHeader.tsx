/**
 * Session Manager header — title row, account-usage chips, the query bar
 * (`SessionQueryBar`: the D5 grammar input, its chips with counts, the account
 * and group pickers and the saved views) and the group/sort selects.
 *
 * Addressability contract (plan
 * `2026-07-19-session-titles-ansi-and-ui-bridge-friction`, Task C). Every
 * control here was previously reachable only by a `page/evaluate` DOM scrape:
 * the file carried no `data-ui-bridge-id`, no `data-page-element` and no
 * `data-testid`. Each interactive control now registers through the SDK's
 * `useUIElement` under an author-stamped id following the convention
 * `SessionManagerToggle` established (`terminal.session-manager-toggle`), and
 * the header root carries the panel's COUNTS and current QUERY as `data-*`
 * attributes — `ElementState.dataset` projects them, so
 * `GET /control/element/terminal.session-manager-header` answers "how many
 * sessions need you, and what is filtered right now" in one read. The filter
 * itself is the query bar's (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4): one query string, which every chip and picker edits.
 *
 * The account-usage chips are the one non-interactive surface, so they use
 * `data-ui-bridge-content` (the SDK's opt-in semantic-content selector) rather
 * than a hook — there is nothing to drive on them, only text to assert.
 */
import { RefreshCw, PanelLeftClose, Sparkles } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import type { SessionGroupBy, SessionSortBy, AccountUsageInfo } from "./useSessionManager";
import type { SessionActivity } from "./sessionStatusModel";
import { SessionQueryBar, type SessionQueryBarProps } from "./SessionQueryBar";
import { parseGroupBy } from "./sessionGrouping";
import { SESSION_ORG_CONTROLS, sessionOrgControlAttrs } from "./sessionOrgControls";

interface SessionManagerHeaderProps {
  loading: boolean;
  /** The query bar — the whole filter (D5). */
  queryBar: SessionQueryBarProps;
  groupBy: SessionGroupBy;
  onGroupByChange: (g: SessionGroupBy) => void;
  sortBy: SessionSortBy;
  onSortByChange: (s: SessionSortBy) => void;
  accountUsage: AccountUsageInfo[];
  onRefresh: () => void;
  /** Rows per activity, over every Live row — the root's dataset counts. */
  activityCounts: Readonly<Record<SessionActivity, number>>;
  /** Rows the filter shows. */
  totalCount: number;
  /** Collapse the panel to its rail (icons only). */
  onToggleRail?: () => void;
  /** Open "Suggest groups"; offered only in the "My groups" lens. */
  onSuggestGroups?: () => void;
  /**
   * The org store's in-flight or refused state ("saving 1…", "refused: …"),
   * so an organization click acknowledges itself; `null` when settled.
   */
  orgStatus?: string | null;
}

const ACCOUNT_COLORS: Record<string, string> = {
  gmail: "#7aa2f7",
  hotmail: "#bb9af7",
  default: "#565f89",
};

function getAccountColor(label: string): string {
  return ACCOUNT_COLORS[label] ?? ACCOUNT_COLORS.default;
}

/** Author-controlled UI-Bridge control ids for this header. */
export const SESSION_MANAGER_HEADER_IDS = {
  root: "terminal.session-manager-header",
  refresh: "terminal.session-manager-refresh",
  groupBy: "terminal.session-manager-group-by",
  sortBy: "terminal.session-manager-sort-by",
} as const;

export function SessionManagerHeader({
  loading,
  queryBar,
  groupBy,
  onGroupByChange,
  sortBy,
  onSortByChange,
  accountUsage,
  onRefresh,
  activityCounts,
  totalCount,
  onToggleRail,
  onSuggestGroups,
  orgStatus = null,
}: SessionManagerHeaderProps) {
  const { ref: railRef } = useUIElement({
    id: SESSION_ORG_CONTROLS.railToggle.id,
    type: "button",
    label: "Collapse the Session Manager to a rail",
  });
  const { ref: suggestRef } = useUIElement({
    id: SESSION_ORG_CONTROLS.suggestGroups.id,
    type: "button",
    label: "Suggest groups",
  });
  const { ref: rootRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.root,
    type: "generic",
    label: "Session Manager header",
  });
  const { ref: refreshRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.refresh,
    type: "button",
    label: "Refresh sessions",
  });
  const { ref: groupByRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.groupBy,
    type: "select",
    label: "Group sessions by",
  });
  const { ref: sortByRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.sortBy,
    type: "select",
    label: "Sort sessions by",
  });

  return (
    <div
      ref={rootRef}
      data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.root}
      // Counts + the live query, projected into `ElementState.dataset` so a
      // driver reads them in one call instead of scraping the chip labels.
      data-total-count={totalCount}
      data-needs-you-count={activityCounts["needs-you"]}
      data-no-output-count={activityCounts["no-output-mid-tool"]}
      data-working-count={activityCounts.working}
      data-unknown-count={activityCounts.unknown}
      data-query={queryBar.query}
      data-group-by={groupBy}
      data-sort-by={sortBy}
      data-loading={loading ? "true" : "false"}
      className="border-b border-[#2a2d3d] bg-[#13141f]"
    >
      {/* Title row */}
      <div className="flex items-center justify-between px-3 py-2">
        <div className="flex items-center gap-2">
          <span className="text-xs font-semibold text-[#c0caf5]">Session Manager</span>
          <span
            data-ui-bridge-content="terminal.session-manager-total-count"
            className="text-[10px] text-[#565f89]"
          >
            {totalCount}
          </span>
        </div>
        <div className="flex items-center gap-0.5">
          {onSuggestGroups && (
            <button
              ref={suggestRef}
              {...sessionOrgControlAttrs("suggestGroups")}
              onClick={onSuggestGroups}
              className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
              title="Suggest groups for ungrouped sessions by plan, pull request, branch or repo"
            >
              <Sparkles className="w-3 h-3" />
              Suggest groups
            </button>
          )}
          <button
            ref={refreshRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.refresh}
            onClick={onRefresh}
            disabled={loading}
            className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
            title="Refresh sessions"
            aria-label="Refresh sessions"
          >
            <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          </button>
          {onToggleRail && (
            <button
              ref={railRef}
              {...sessionOrgControlAttrs("railToggle")}
              onClick={onToggleRail}
              aria-pressed={false}
              className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
              title="Collapse to a rail (status dots only)"
              aria-label="Collapse the Session Manager to a rail"
            >
              <PanelLeftClose className="w-3 h-3" />
            </button>
          )}
        </div>
      </div>
      {orgStatus && (
        <div
          data-testid="session-org-status"
          className="px-3 pb-1 text-[10px] text-[#565f89] truncate"
          title={orgStatus}
        >
          Groups: {orgStatus}
        </div>
      )}

      {/* Account usage chips */}
      {accountUsage.length > 0 && (
        <div className="flex items-center gap-1.5 px-3 pb-1.5">
          {accountUsage.map((a) => {
            const label = (() => {
              const normalized = a.config_dir.replace(/\\/g, "/").replace(/\/$/, "");
              const last = normalized.split("/").pop() ?? "";
              const match = last.match(/^\.claude-(.+)$/);
              return match ? match[1] : "default";
            })();
            const pct = a.utilization != null ? Math.round(a.utilization * 100) : null;
            const color = getAccountColor(label);
            return (
              <div
                key={a.config_dir}
                // Non-interactive: nothing to drive, only text to assert, so
                // this is the SDK's semantic-content opt-in rather than a hook.
                // The attribute value becomes the snapshot id verbatim.
                data-ui-bridge-content={`terminal.session-manager-account-usage-${label}`}
                data-account={label}
                data-utilization={pct != null ? String(pct) : undefined}
                data-status={a.status ?? undefined}
                className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-[#1a1b26] border border-[#2a2d3d]"
                title={`${label}: ${pct != null ? `${pct}% utilization` : (a.status ?? "unknown")}`}
              >
                <span
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ backgroundColor: color }}
                />
                <span className="text-[#a9b1d6]">{label}</span>
                {pct != null && (
                  <span className={pct > 80 ? "text-[#f7768e]" : "text-[#565f89]"}>{pct}%</span>
                )}
              </div>
            );
          })}
        </div>
      )}

      <SessionQueryBar {...queryBar} />

      {/* Group by / Sort by row */}
      <div className="flex items-center gap-2 px-3 pb-2">
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-[#414868]">Group:</span>
          <select
            ref={groupByRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.groupBy}
            aria-label="Group sessions by"
            value={groupBy}
            onChange={(e) => onGroupByChange(parseGroupBy(e.target.value))}
            className="text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50"
          >
            <option value="groups">My groups</option>
            <option value="status">Status</option>
            <option value="account">Account</option>
            <option value="project">Project</option>
            <option value="date">Date</option>
          </select>
        </div>
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-[#414868]">Sort:</span>
          <select
            ref={sortByRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.sortBy}
            aria-label="Sort sessions by"
            value={sortBy}
            onChange={(e) =>
              onSortByChange(e.target.value as "recent" | "oldest" | "messages" | "staleness")
            }
            className="text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50"
          >
            <option value="recent">Recent</option>
            <option value="oldest">Oldest</option>
            <option value="messages">Messages</option>
            <option value="staleness">Urgency</option>
          </select>
        </div>
      </div>
    </div>
  );
}
