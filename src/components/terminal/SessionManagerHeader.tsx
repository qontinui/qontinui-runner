/**
 * Session Manager header — title row, account-usage chips, status quick
 * filters, search, account filters and the group/sort selects.
 *
 * Addressability contract (plan
 * `2026-07-19-session-titles-ansi-and-ui-bridge-friction`, Task C). Every
 * control here was previously reachable only by a `page/evaluate` DOM scrape:
 * the file carried no `data-ui-bridge-id`, no `data-page-element` and no
 * `data-testid`. Each interactive control now registers through the SDK's
 * `useUIElement` under an author-stamped id following the convention
 * `SessionManagerToggle` established (`terminal.session-manager-toggle`), and
 * the header root carries the panel's COUNTS and current FILTER state as
 * `data-*` attributes — `ElementState.dataset` projects them, so
 * `GET /control/element/terminal.session-manager-header` answers "how many
 * frozen sessions, and what is filtered right now" in one read.
 *
 * The account-usage chips are the one non-interactive surface, so they use
 * `data-ui-bridge-content` (the SDK's opt-in semantic-content selector) rather
 * than a hook — there is nothing to drive on them, only text to assert.
 */
import { RefreshCw, Search, Filter } from "lucide-react";
import { useUIElement, type StandardAction } from "@qontinui/ui-bridge";

import type {
  SessionLiveStatus,
  SessionGroupBy,
  SessionSortBy,
  AccountUsageInfo,
} from "./useSessionManager";

interface SessionManagerHeaderProps {
  loading: boolean;
  searchQuery: string;
  onSearchChange: (q: string) => void;
  statusFilter: SessionLiveStatus | "all";
  onStatusFilterChange: (f: SessionLiveStatus | "all") => void;
  accountFilter: string | "all";
  onAccountFilterChange: (f: string | "all") => void;
  groupBy: SessionGroupBy;
  onGroupByChange: (g: SessionGroupBy) => void;
  sortBy: SessionSortBy;
  onSortByChange: (s: SessionSortBy) => void;
  accountUsage: AccountUsageInfo[];
  accounts: string[];
  onRefresh: () => void;
  frozenCount: number;
  needsInputCount: number;
  activeCount: number;
  totalCount: number;
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
  search: "terminal.session-manager-search",
  filterFrozen: "terminal.session-manager-filter-frozen",
  filterNeedsInput: "terminal.session-manager-filter-needs-input",
  filterActive: "terminal.session-manager-filter-active",
  filterClear: "terminal.session-manager-filter-clear",
  groupBy: "terminal.session-manager-group-by",
  sortBy: "terminal.session-manager-sort-by",
} as const;

/** Per-account filter button id — one per rendered account. */
export function sessionManagerAccountFilterId(account: string): string {
  return `terminal.session-manager-account-${account}`;
}

/**
 * Actions the search box advertises. `inferActions('input')` omits `sendKeys`,
 * which is the same per-element under-advertisement that made the command bar
 * reject a driver's keystrokes (`CommandBar.tsx`
 * `COMMAND_BAR_INPUT_ACTIONS`) — filtering the session list means typing into
 * this box, so it is advertised here for the same reason and with the same
 * SDK descriptor-array contract.
 */
export const SEARCH_INPUT_ACTIONS: StandardAction[] = [
  "focus",
  "blur",
  "hover",
  "scroll",
  "scrollIntoView",
  "click",
  "hoverClick",
  "type",
  "clear",
  "sendKeys",
];

/**
 * One account filter button. Its own component so the `useUIElement` call is a
 * plain unconditional hook per rendered account — same shape as
 * `SessionInfoDropdown`'s `InfoRow`.
 */
function AccountFilterButton({
  account,
  active,
  onToggle,
}: {
  account: string;
  active: boolean;
  onToggle: () => void;
}) {
  const { ref } = useUIElement({
    id: sessionManagerAccountFilterId(account),
    type: "button",
    label: `Filter sessions by account ${account}`,
  });
  return (
    <button
      ref={ref}
      data-ui-bridge-id={sessionManagerAccountFilterId(account)}
      data-account={account}
      data-active={active ? "true" : "false"}
      aria-pressed={active}
      onClick={onToggle}
      className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
        active
          ? "bg-[#7aa2f7]/15 text-[#7aa2f7] border border-[#7aa2f7]/30"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] border border-transparent"
      }`}
    >
      {account}
    </button>
  );
}

export function SessionManagerHeader({
  loading,
  searchQuery,
  onSearchChange,
  statusFilter,
  onStatusFilterChange,
  accountFilter,
  onAccountFilterChange,
  groupBy,
  onGroupByChange,
  sortBy,
  onSortByChange,
  accountUsage,
  accounts,
  onRefresh,
  frozenCount,
  needsInputCount,
  activeCount,
  totalCount,
}: SessionManagerHeaderProps) {
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
  const { ref: searchRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.search,
    type: "input",
    label: "Search sessions",
    actions: SEARCH_INPUT_ACTIONS,
  });
  const { ref: filterFrozenRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.filterFrozen,
    type: "button",
    label: "Filter frozen sessions",
  });
  const { ref: filterNeedsInputRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.filterNeedsInput,
    type: "button",
    label: "Filter sessions needing input",
  });
  const { ref: filterActiveRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.filterActive,
    type: "button",
    label: "Filter active sessions",
  });
  const { ref: filterClearRef } = useUIElement({
    id: SESSION_MANAGER_HEADER_IDS.filterClear,
    type: "button",
    label: "Clear session status filter",
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
      // Counts + live filter state, projected into `ElementState.dataset` so a
      // driver reads them in one call instead of scraping the chip labels
      // (which self-hide at zero — a scrape cannot distinguish "no frozen
      // sessions" from "chip not rendered").
      data-total-count={totalCount}
      data-frozen-count={frozenCount}
      data-needs-input-count={needsInputCount}
      data-active-count={activeCount}
      data-status-filter={statusFilter}
      data-account-filter={accountFilter}
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
      </div>

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

      {/* Status quick-filter chips */}
      <div className="flex items-center gap-1 px-3 pb-1.5">
        {frozenCount > 0 && (
          <button
            ref={filterFrozenRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.filterFrozen}
            aria-pressed={statusFilter === "frozen"}
            onClick={() => onStatusFilterChange(statusFilter === "frozen" ? "all" : "frozen")}
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              statusFilter === "frozen"
                ? "bg-[#f7768e]/20 text-[#f7768e] border border-[#f7768e]/30"
                : "bg-[#1a1b26] text-[#f7768e]/70 border border-[#2a2d3d] hover:bg-[#f7768e]/10"
            }`}
          >
            <span className="w-1.5 h-1.5 rounded-full bg-[#f7768e]" />
            {frozenCount} frozen
          </button>
        )}
        {needsInputCount > 0 && (
          <button
            ref={filterNeedsInputRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.filterNeedsInput}
            aria-pressed={statusFilter === "needs-input"}
            onClick={() =>
              onStatusFilterChange(statusFilter === "needs-input" ? "all" : "needs-input")
            }
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              statusFilter === "needs-input"
                ? "bg-[#e0af68]/20 text-[#e0af68] border border-[#e0af68]/30"
                : "bg-[#1a1b26] text-[#e0af68]/70 border border-[#2a2d3d] hover:bg-[#e0af68]/10"
            }`}
          >
            <span className="w-1.5 h-1.5 rounded-full bg-[#e0af68]" />
            {needsInputCount} input
          </button>
        )}
        {activeCount > 0 && (
          <button
            ref={filterActiveRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.filterActive}
            aria-pressed={statusFilter === "active-in-zone"}
            onClick={() =>
              onStatusFilterChange(statusFilter === "active-in-zone" ? "all" : "active-in-zone")
            }
            className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
              statusFilter === "active-in-zone"
                ? "bg-[#9ece6a]/20 text-[#9ece6a] border border-[#9ece6a]/30"
                : "bg-[#1a1b26] text-[#9ece6a]/70 border border-[#2a2d3d] hover:bg-[#9ece6a]/10"
            }`}
          >
            <span className="w-1.5 h-1.5 rounded-full bg-[#9ece6a]" />
            {activeCount} active
          </button>
        )}
        {statusFilter !== "all" && (
          <button
            ref={filterClearRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.filterClear}
            onClick={() => onStatusFilterChange("all")}
            className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] transition-colors"
          >
            clear
          </button>
        )}
      </div>

      {/* Search bar */}
      <div className="px-3 pb-2">
        <div className="relative">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3 h-3 text-[#565f89]" />
          <input
            ref={searchRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.search}
            aria-label="Search sessions"
            type="text"
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            placeholder="Search sessions..."
            className="w-full pl-7 pr-2 py-1 text-xs bg-[#1a1b26] border border-[#2a2d3d] rounded text-[#a9b1d6] placeholder-[#414868] focus:outline-none focus:border-[#7aa2f7]/50"
          />
        </div>
      </div>

      {/* Account filter row */}
      {accounts.length > 1 && (
        <div className="flex items-center gap-1.5 px-3 pb-1.5">
          <Filter className="w-3 h-3 text-[#565f89] shrink-0" />
          {accounts.map((acct) => (
            <AccountFilterButton
              key={acct}
              account={acct}
              active={accountFilter === acct}
              onToggle={() => onAccountFilterChange(accountFilter === acct ? "all" : acct)}
            />
          ))}
        </div>
      )}

      {/* Group by / Sort by row */}
      <div className="flex items-center gap-2 px-3 pb-2">
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-[#414868]">Group:</span>
          <select
            ref={groupByRef}
            data-ui-bridge-id={SESSION_MANAGER_HEADER_IDS.groupBy}
            aria-label="Group sessions by"
            value={groupBy}
            onChange={(e) =>
              onGroupByChange(e.target.value as "status" | "account" | "project" | "date")
            }
            className="text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50"
          >
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
