import { RefreshCw, Search, Filter } from "lucide-react";
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
  return (
    <div className="border-b border-[#2a2d3d] bg-[#13141f]">
      {/* Title row */}
      <div className="flex items-center justify-between px-3 py-2">
        <div className="flex items-center gap-2">
          <span className="text-xs font-semibold text-[#c0caf5]">Session Manager</span>
          <span className="text-[10px] text-[#565f89]">{totalCount}</span>
        </div>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="p-1 rounded hover:bg-[#2a2d3d] text-[#565f89] hover:text-[#c0caf5] transition-colors"
          title="Refresh sessions"
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
                className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] bg-[#1a1b26] border border-[#2a2d3d]"
                title={`${label}: ${pct != null ? `${pct}% utilization` : a.status ?? "unknown"}`}
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
            <button
              key={acct}
              onClick={() => onAccountFilterChange(accountFilter === acct ? "all" : acct)}
              className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
                accountFilter === acct
                  ? "bg-[#7aa2f7]/15 text-[#7aa2f7] border border-[#7aa2f7]/30"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] border border-transparent"
              }`}
            >
              {acct}
            </button>
          ))}
        </div>
      )}

      {/* Group by / Sort by row */}
      <div className="flex items-center gap-2 px-3 pb-2">
        <div className="flex items-center gap-1">
          <span className="text-[10px] text-[#414868]">Group:</span>
          <select
            value={groupBy}
            onChange={(e) => onGroupByChange(e.target.value as "status" | "account" | "project" | "date")}
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
            value={sortBy}
            onChange={(e) => onSortByChange(e.target.value as "recent" | "oldest" | "messages" | "staleness")}
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
