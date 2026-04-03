/**
 * Core hook for the Session Manager panel.
 *
 * Merges three data sources into a unified session view:
 * 1. Transcript sessions on disk (all accounts)
 * 2. Active PTY tabs in the current Runner window
 * 3. Session digests (frozen detection + work summary hints)
 *
 * Provides search, filter, sort, grouping, and actions.
 */

import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState } from "./useZoneLayout";
import type { CommandResponse } from "./types";

// ── Types ────────────────────────────────────────────────────────────────────

export type SessionLiveStatus =
  | "active-in-zone"
  | "active-external"
  | "frozen"
  | "needs-input"
  | "completed"
  | "error"
  | "dormant";

export interface SessionDigest {
  session_id: string;
  config_dir: string;
  project_path: string;
  last_message_type: string;
  last_message_timestamp: string;
  last_message_preview: string;
  last_assistant_had_tool_use: boolean;
  likely_frozen: boolean;
  work_summary_hint: string;
}

export interface ExternalClaudeProcess {
  pid: number;
  working_directory: string | null;
}

export interface AccountUsageInfo {
  config_dir: string;
  label: string;
  utilization: number;
  rate_limit_type: string | null;
  resets_at: number | null;
  status: string | null;
  error: string | null;
}

export interface UnifiedSession {
  sessionId: string;
  accountLabel: string;
  configDir: string;
  projectPath: string;
  projectLabel: string;

  // Transcript metadata
  displayName: string;
  firstMessagePreview: string | null;
  messageCount: number;
  lastModified: string;
  hasPlans: boolean;

  // Live status
  liveStatus: SessionLiveStatus;
  zoneTabId: string | null;
  zoneIndex: number | null;

  // Timing
  startedAt: string | null;
  durationMs: number | null; // total duration from start to last modified

  // Digest data
  lastMessageType: string;
  lastMessageTimestamp: string;
  lastMessagePreview: string;
  workSummaryHint: string;
  likelyFrozen: boolean;

  // Original transcript session (for resume)
  _transcript: TranscriptSession;
}

export type SessionGroupBy = "status" | "account" | "project" | "date";
export type SessionSortBy = "recent" | "oldest" | "messages" | "staleness";

export interface UseSessionManagerParams {
  tabs: TerminalTab[];
  sessionStates: Record<string, SessionState>;
  staleTabs: Set<string>;
  transcriptSessions: TranscriptSession[];
  sessionsLoading: boolean;
  desktopNotify: boolean;
  onRefreshSessions: () => void;
  onResumeSession: (session: TranscriptSession) => void;
  onSelectSession: (sessionId: string) => void;
}

export interface UseSessionManagerReturn {
  sessions: UnifiedSession[];
  loading: boolean;
  accountUsage: AccountUsageInfo[];

  // Filters
  searchQuery: string;
  setSearchQuery: (q: string) => void;
  statusFilter: SessionLiveStatus | "all";
  setStatusFilter: (f: SessionLiveStatus | "all") => void;
  accountFilter: string | "all";
  setAccountFilter: (f: string | "all") => void;
  groupBy: SessionGroupBy;
  setGroupBy: (g: SessionGroupBy) => void;
  sortBy: SessionSortBy;
  setSortBy: (s: SessionSortBy) => void;

  // Actions
  refresh: () => void;
  resumeSession: (session: UnifiedSession) => void;
  viewTranscript: (session: UnifiedSession) => void;
  copySessionId: (session: UnifiedSession) => void;

  // Multi-select & bulk actions
  selectedIds: Set<string>;
  toggleSelect: (sessionId: string) => void;
  selectAllFrozen: () => void;
  clearSelection: () => void;
  bulkResume: () => void;

  // Pinning & labeling
  pinnedIds: Set<string>;
  togglePin: (sessionId: string) => void;
  sessionLabels: Record<string, string>;
  setSessionLabel: (sessionId: string, label: string | null) => void;

  // Derived
  allAccounts: string[];

  // Stats
  frozenCount: number;
  needsInputCount: number;
  activeCount: number;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function extractAccountLabel(configDir: string): string {
  // Extract label from config dir path like "C:\claude\.claude-gmail" → "gmail"
  const normalized = configDir.replace(/\\/g, "/").replace(/\/$/, "");
  const last = normalized.split("/").pop() ?? "";
  const match = last.match(/^\.claude-(.+)$/);
  return match ? match[1] : last === ".claude" ? "default" : last;
}

function extractProjectLabel(projectPath: string): string {
  if (!projectPath) return "";
  const normalized = projectPath.replace(/\\/g, "/").replace(/\/$/, "");
  return normalized.split("/").pop() ?? "";
}

function computeDuration(
  startedAt: string | null | undefined,
  lastModified: string,
): number | null {
  if (!startedAt) return null;
  try {
    const start = new Date(startedAt).getTime();
    const end = new Date(lastModified).getTime();
    const diff = end - start;
    return diff > 0 ? diff : null;
  } catch {
    return null;
  }
}

/** Status priority for sorting (lower = more urgent). */
const STATUS_PRIORITY: Record<SessionLiveStatus, number> = {
  frozen: 0,
  "needs-input": 1,
  "active-in-zone": 2,
  "active-external": 3,
  error: 4,
  completed: 5,
  dormant: 6,
};

// ── Hook ─────────────────────────────────────────────────────────────────────

export function useSessionManager(params: UseSessionManagerParams): UseSessionManagerReturn {
  const {
    tabs,
    sessionStates,
    staleTabs,
    transcriptSessions,
    sessionsLoading,
    desktopNotify,
    onRefreshSessions,
    onResumeSession,
    onSelectSession,
  } = params;

  // Digest state
  const [digests, setDigests] = useState<Record<string, SessionDigest>>({});
  const [digestsLoading, setDigestsLoading] = useState(false);
  const digestsLoadedRef = useRef(false);

  // Account usage state
  const [accountUsage, setAccountUsage] = useState<AccountUsageInfo[]>([]);

  // External process state
  const [externalProcessCount, setExternalProcessCount] = useState(0);

  // Filter/sort state (persisted across restarts)
  const STORAGE_PREFIX = "session-manager-";
  const [searchQuery, setSearchQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<SessionLiveStatus | "all">(
    () =>
      (instanceStorage.getItem(`${STORAGE_PREFIX}status-filter`) as SessionLiveStatus | "all") ||
      "all",
  );
  const [accountFilter, setAccountFilter] = useState<string | "all">(
    () => instanceStorage.getItem(`${STORAGE_PREFIX}account-filter`) || "all",
  );
  const [groupBy, setGroupBy] = useState<SessionGroupBy>(
    () => (instanceStorage.getItem(`${STORAGE_PREFIX}group-by`) as SessionGroupBy) || "status",
  );
  const [sortBy, setSortBy] = useState<SessionSortBy>(
    () => (instanceStorage.getItem(`${STORAGE_PREFIX}sort-by`) as SessionSortBy) || "recent",
  );

  // Persist filter/sort changes
  useEffect(() => {
    instanceStorage.setItem(`${STORAGE_PREFIX}status-filter`, statusFilter);
  }, [statusFilter]);
  useEffect(() => {
    instanceStorage.setItem(`${STORAGE_PREFIX}account-filter`, accountFilter);
  }, [accountFilter]);
  useEffect(() => {
    instanceStorage.setItem(`${STORAGE_PREFIX}group-by`, groupBy);
  }, [groupBy]);
  useEffect(() => {
    instanceStorage.setItem(`${STORAGE_PREFIX}sort-by`, sortBy);
  }, [sortBy]);

  // ── Pinning (persisted) ───────────────────────────────────────────────────

  const [pinnedIds, setPinnedIds] = useState<Set<string>>(() => {
    try {
      const stored = instanceStorage.getItem(`${STORAGE_PREFIX}pinned`);
      return stored ? new Set(JSON.parse(stored) as string[]) : new Set();
    } catch {
      return new Set();
    }
  });

  const togglePin = useCallback((sessionId: string) => {
    setPinnedIds((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) {
        next.delete(sessionId);
      } else {
        next.add(sessionId);
      }
      instanceStorage.setItem(`${STORAGE_PREFIX}pinned`, JSON.stringify([...next]));
      return next;
    });
  }, []);

  // ── Labeling (persisted) ──────────────────────────────────────────────────

  const [sessionLabels, setSessionLabels] = useState<Record<string, string>>(() => {
    try {
      const stored = instanceStorage.getItem(`${STORAGE_PREFIX}labels`);
      return stored ? (JSON.parse(stored) as Record<string, string>) : {};
    } catch {
      return {};
    }
  });

  const setSessionLabel = useCallback((sessionId: string, label: string | null) => {
    setSessionLabels((prev) => {
      const next = { ...prev };
      if (label) {
        next[sessionId] = label;
      } else {
        delete next[sessionId];
      }
      instanceStorage.setItem(`${STORAGE_PREFIX}labels`, JSON.stringify(next));
      return next;
    });
  }, []);

  // ── Fetch digests ──────────────────────────────────────────────────────────

  const fetchDigests = useCallback(async () => {
    setDigestsLoading(true);
    try {
      const result = await invoke<CommandResponse>("transcript_session_digests", {
        maxSessions: 50,
      });
      if (result.success && result.data) {
        const digestList = result.data as SessionDigest[];
        const map: Record<string, SessionDigest> = {};
        for (const d of digestList) {
          map[d.session_id] = d;
        }
        setDigests(map);
      }
    } catch {
      // Silently fail — digests are optional enrichment
    } finally {
      setDigestsLoading(false);
    }
  }, []);

  // Load digests on mount
  useEffect(() => {
    if (!digestsLoadedRef.current) {
      digestsLoadedRef.current = true;
      fetchDigests();
    }
  }, [fetchDigests]);

  // ── Fetch account usage (every 5 minutes) ─────────────────────────────────

  // Merge config dirs from transcript sessions AND saved settings so the
  // launch menu shows accounts even when no sessions exist yet.
  const [savedConfigDirs, setSavedConfigDirs] = useState<string[]>([]);
  useEffect(() => {
    (async () => {
      try {
        const result = await invoke<CommandResponse>("get_claude_config_dirs");
        if (result.success && result.data) {
          const data = result.data as { dirs: string[] };
          if (data.dirs?.length) setSavedConfigDirs(data.dirs);
        }
      } catch {
        // Silently fail
      }
    })();
  }, []);

  // Stable key for config dirs so the interval doesn't reset on every session list change
  const configDirsKey = useMemo(() => {
    const fromSessions = transcriptSessions.map((s) => s.config_dir);
    const dirs = [...new Set([...fromSessions, ...savedConfigDirs])].sort();
    return dirs.join("|");
  }, [transcriptSessions, savedConfigDirs]);

  const configDirsRef = useRef<string[]>([]);
  useEffect(() => {
    configDirsRef.current = configDirsKey ? configDirsKey.split("|") : [];
  }, [configDirsKey]);

  const fetchAccountUsage = useCallback(async () => {
    const configDirs = configDirsRef.current;
    if (configDirs.length === 0) return;

    try {
      const result = await invoke<CommandResponse>("check_accounts_usage", {
        configDirs,
      });
      if (result.success && result.data) {
        setAccountUsage(result.data as AccountUsageInfo[]);
      }
    } catch {
      // Silently fail
    }
  }, []);

  useEffect(() => {
    if (configDirsKey) {
      fetchAccountUsage();
      const interval = setInterval(fetchAccountUsage, 5 * 60 * 1000);
      return () => clearInterval(interval);
    }
  }, [fetchAccountUsage, configDirsKey]);

  // ── Detect external Claude processes (every 30s) ────────────────────────────

  const fetchExternalProcesses = useCallback(async () => {
    try {
      const result = await invoke<CommandResponse>("transcript_find_external_processes");
      if (result.success && result.data) {
        const processes = result.data as ExternalClaudeProcess[];
        setExternalProcessCount(processes.length);
      }
    } catch {
      // Silently fail
    }
  }, []);

  useEffect(() => {
    fetchExternalProcesses();
    const interval = setInterval(fetchExternalProcesses, 30 * 1000);
    return () => clearInterval(interval);
  }, [fetchExternalProcesses]);

  // ── Build tab lookup for live status correlation ───────────────────────────

  const tabSessionMap = useMemo(() => {
    const map = new Map<string, TerminalTab>();
    for (const tab of tabs) {
      if (tab.claudeSessionId) {
        map.set(tab.claudeSessionId, tab);
      }
    }
    return map;
  }, [tabs]);

  // ── Merge into UnifiedSession[] ────────────────────────────────────────────

  const allSessions = useMemo((): UnifiedSession[] => {
    return transcriptSessions
      .filter((s) => s.message_count > 0)
      .map((s): UnifiedSession => {
        const tab = tabSessionMap.get(s.session_id);
        const digest = digests[s.session_id];

        // Compute live status
        let liveStatus: SessionLiveStatus = "dormant";
        let zoneTabId: string | null = null;

        if (tab) {
          zoneTabId = tab.id;
          const tabState = sessionStates[tab.id];
          if (tabState === "needs-input") {
            liveStatus = "needs-input";
          } else if (tabState === "error") {
            liveStatus = "error";
          } else if (tabState === "completed") {
            liveStatus = "completed";
          } else if (staleTabs.has(tab.id)) {
            liveStatus = "frozen";
          } else {
            liveStatus = "active-in-zone";
          }
        } else if (digest?.likely_frozen) {
          // If external processes are running and session was modified very recently,
          // it's likely active externally rather than frozen
          if (externalProcessCount > 0) {
            try {
              const age = Date.now() - new Date(s.last_modified).getTime();
              if (age < 10 * 60 * 1000) {
                // Modified within last 10 minutes — likely active externally
                liveStatus = "active-external";
              } else {
                liveStatus = "frozen";
              }
            } catch {
              liveStatus = "frozen";
            }
          } else {
            liveStatus = "frozen";
          }
        } else if (externalProcessCount > 0 && !tab) {
          // Check if this non-frozen session might be running externally
          try {
            const age = Date.now() - new Date(s.last_modified).getTime();
            if (age < 2 * 60 * 1000) {
              // Very recently modified (within 2 min) and not in a zone — likely external
              liveStatus = "active-external";
            }
          } catch {
            // Keep dormant
          }
        }

        return {
          sessionId: s.session_id,
          accountLabel: extractAccountLabel(s.config_dir),
          configDir: s.config_dir,
          projectPath: s.project_path,
          projectLabel: extractProjectLabel(s.project_path),
          displayName: s.display_name,
          firstMessagePreview: s.first_message_preview,
          messageCount: s.message_count,
          lastModified: s.last_modified,
          hasPlans: s.has_plans,
          liveStatus,
          zoneTabId,
          zoneIndex: null, // Could be enriched from zone assignments
          startedAt: s.started_at ?? null,
          durationMs: computeDuration(s.started_at, s.last_modified),
          lastMessageType: digest?.last_message_type ?? "",
          lastMessageTimestamp: digest?.last_message_timestamp ?? s.last_modified,
          lastMessagePreview: digest?.last_message_preview ?? "",
          workSummaryHint: digest?.work_summary_hint ?? "",
          likelyFrozen: digest?.likely_frozen ?? false,
          _transcript: s,
        };
      });
  }, [transcriptSessions, tabSessionMap, sessionStates, staleTabs, digests, externalProcessCount]);

  // ── Filter and sort ────────────────────────────────────────────────────────

  const sessions = useMemo(() => {
    let filtered = allSessions;

    // Status filter
    if (statusFilter !== "all") {
      filtered = filtered.filter((s) => s.liveStatus === statusFilter);
    }

    // Account filter
    if (accountFilter !== "all") {
      filtered = filtered.filter((s) => s.accountLabel === accountFilter);
    }

    // Search
    if (searchQuery.trim()) {
      const q = searchQuery.toLowerCase();
      filtered = filtered.filter(
        (s) =>
          s.displayName.toLowerCase().includes(q) ||
          s.projectLabel.toLowerCase().includes(q) ||
          s.sessionId.toLowerCase().includes(q) ||
          (s.firstMessagePreview?.toLowerCase().includes(q) ?? false) ||
          s.workSummaryHint.toLowerCase().includes(q),
      );
    }

    // Sort
    const sorted = [...filtered];
    switch (sortBy) {
      case "recent":
        sorted.sort((a, b) => b.lastModified.localeCompare(a.lastModified));
        break;
      case "oldest":
        sorted.sort((a, b) => a.lastModified.localeCompare(b.lastModified));
        break;
      case "messages":
        sorted.sort((a, b) => b.messageCount - a.messageCount);
        break;
      case "staleness":
        // Sort by status priority (frozen first), then by recency
        sorted.sort((a, b) => {
          const pa = STATUS_PRIORITY[a.liveStatus] ?? 5;
          const pb = STATUS_PRIORITY[b.liveStatus] ?? 5;
          if (pa !== pb) return pa - pb;
          return b.lastModified.localeCompare(a.lastModified);
        });
        break;
    }

    // When grouping by status, always apply status priority as primary sort
    if (groupBy === "status") {
      sorted.sort((a, b) => {
        const pa = STATUS_PRIORITY[a.liveStatus] ?? 5;
        const pb = STATUS_PRIORITY[b.liveStatus] ?? 5;
        if (pa !== pb) return pa - pb;
        return b.lastModified.localeCompare(a.lastModified);
      });
    }

    // Pinned sessions always float to the top
    if (pinnedIds.size > 0) {
      sorted.sort((a, b) => {
        const aPinned = pinnedIds.has(a.sessionId) ? 0 : 1;
        const bPinned = pinnedIds.has(b.sessionId) ? 0 : 1;
        return aPinned - bPinned;
      });
    }

    return sorted;
  }, [allSessions, statusFilter, accountFilter, searchQuery, sortBy, groupBy, pinnedIds]);

  // ── Derived ─────────────────────────────────────────────────────────────────

  const allAccounts = useMemo(() => {
    const set = new Set<string>();
    for (const s of allSessions) {
      set.add(s.accountLabel);
    }
    return Array.from(set).sort();
  }, [allSessions]);

  // ── Stats ──────────────────────────────────────────────────────────────────

  const frozenCount = useMemo(
    () => allSessions.filter((s) => s.liveStatus === "frozen").length,
    [allSessions],
  );
  const needsInputCount = useMemo(
    () => allSessions.filter((s) => s.liveStatus === "needs-input").length,
    [allSessions],
  );
  const activeCount = useMemo(
    () => allSessions.filter((s) => s.liveStatus === "active-in-zone").length,
    [allSessions],
  );

  // ── Desktop notifications for frozen sessions ──────────────────────────────

  const prevFrozenCountRef = useRef(0);
  useEffect(() => {
    if (
      desktopNotify &&
      frozenCount > prevFrozenCountRef.current &&
      prevFrozenCountRef.current >= 0 &&
      document.hidden &&
      "Notification" in window &&
      Notification.permission === "granted"
    ) {
      const newFrozen = frozenCount - prevFrozenCountRef.current;
      new Notification("Session Manager", {
        body: `${newFrozen} session${newFrozen > 1 ? "s" : ""} appear${newFrozen === 1 ? "s" : ""} frozen and may need resume`,
        tag: "session-manager-frozen",
      });
    }
    prevFrozenCountRef.current = frozenCount;
  }, [frozenCount, desktopNotify]);

  // ── Actions ────────────────────────────────────────────────────────────────

  const refresh = useCallback(() => {
    onRefreshSessions();
    fetchDigests();
  }, [onRefreshSessions, fetchDigests]);

  const resumeSession = useCallback(
    (session: UnifiedSession) => {
      onResumeSession(session._transcript);
    },
    [onResumeSession],
  );

  const viewTranscript = useCallback(
    (session: UnifiedSession) => {
      onSelectSession(session.sessionId);
    },
    [onSelectSession],
  );

  const copySessionId = useCallback((session: UnifiedSession) => {
    navigator.clipboard.writeText(session.sessionId).catch(() => {});
  }, []);

  // ── Multi-select & bulk actions ──────────────────────────────────────────

  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  const toggleSelect = useCallback((sessionId: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) {
        next.delete(sessionId);
      } else {
        next.add(sessionId);
      }
      return next;
    });
  }, []);

  const selectAllFrozen = useCallback(() => {
    const frozenIds = allSessions.filter((s) => s.liveStatus === "frozen").map((s) => s.sessionId);
    setSelectedIds(new Set(frozenIds));
  }, [allSessions]);

  const clearSelection = useCallback(() => {
    setSelectedIds(new Set());
  }, []);

  const bulkResume = useCallback(async () => {
    const toResume = allSessions.filter((s) => selectedIds.has(s.sessionId));
    setSelectedIds(new Set());
    // Resume sequentially with a small delay to let each tab initialize
    for (const session of toResume) {
      onResumeSession(session._transcript);
      // Small delay between resumes to avoid race conditions in terminal creation
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  }, [allSessions, selectedIds, onResumeSession]);

  return {
    sessions,
    loading: sessionsLoading || digestsLoading,
    accountUsage,
    searchQuery,
    setSearchQuery,
    statusFilter,
    setStatusFilter,
    accountFilter,
    setAccountFilter,
    groupBy,
    setGroupBy,
    sortBy,
    setSortBy,
    refresh,
    resumeSession,
    viewTranscript,
    copySessionId,
    selectedIds,
    toggleSelect,
    selectAllFrozen,
    clearSelection,
    bulkResume,
    pinnedIds,
    togglePin,
    sessionLabels,
    setSessionLabel,
    allAccounts,
    frozenCount,
    needsInputCount,
    activeCount,
  };
}
