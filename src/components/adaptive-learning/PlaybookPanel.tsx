/**
 * PlaybookPanel — Browse and manage lessons from the adaptive learning playbook.
 *
 * Shows lessons grouped by severity with helpfulness ratios, DO/DON'T indicators,
 * interactive status controls, expandable detail panels, and auto-refresh.
 */

import { useState, useEffect, useCallback, useRef, useReducer } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface PlaybookEntry {
  id: string;
  lesson: string;
  category: string;
  domain: string | null;
  severity: string;
  positive: boolean;
  times_applied: number;
  times_helped: number;
  helpfulness_ratio: number;
  status: string;
  created_at: string;
}

interface SourceRun {
  run_id: string;
  status: string;
  duration_secs: number | null;
  iterations: number | null;
  strategy: string | null;
  architecture: string | null;
  created_at: string;
}

interface PlaybookEntryDetail {
  entry: PlaybookEntry;
  source_run: SourceRun | null;
}

const SEVERITY_COLORS: Record<string, string> = {
  critical: "#ef4444",
  important: "#f59e0b",
  minor: "#6b7280",
};

const CATEGORY_LABELS: Record<string, string> = {
  step_construction: "Step Construction",
  selector_choice: "Selector Choice",
  error_handling: "Error Handling",
  tool_usage: "Tool Usage",
  domain_knowledge: "Domain Knowledge",
  anti_pattern: "Anti-Pattern",
};

const AUTO_REFRESH_INTERVAL_MS = 30_000;

export function PlaybookPanel() {
  const [entries, setEntries] = useState<PlaybookEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [domainFilter, setDomainFilter] = useState<string>("");
  const [statusFilter, setStatusFilter] = useState<string>("active");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const loadEntries = useCallback(async () => {
    setLoading(true);
    try {
      const result = await invoke<PlaybookEntry[]>("get_playbook_entries", {
        domain: domainFilter || null,
        status: statusFilter || null,
        limit: 100,
      });
      setEntries(result);
    } catch (err) {
      console.error("Failed to load playbook entries:", err);
    } finally {
      setLoading(false);
    }
  }, [domainFilter, statusFilter]);

  // Initial + filter-change load. Body wraps loadEntries() in an IIFE so the
  // synchronous setLoading(true) inside it isn't the first statement reached
  // from the effect body (react-hooks/set-state-in-effect).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (cancelled) return;
      await loadEntries();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadEntries]);

  // Auto-refresh on learning-update events
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    (async () => {
      unlisten = await listen("learning-update", () => {
        if (!cancelled) {
          loadEntries();
        }
      });
    })();

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [loadEntries]);

  // Periodic refresh every 30s
  useEffect(() => {
    const interval = setInterval(() => {
      loadEntries();
    }, AUTO_REFRESH_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [loadEntries]);

  const handleStatusChange = useCallback(
    async (id: string, newStatus: string) => {
      try {
        await invoke("update_playbook_entry_status", { id, new_status: newStatus });
        await loadEntries();
      } catch (err) {
        console.error(`Failed to update status to ${newStatus}:`, err);
      }
    },
    [loadEntries],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await invoke("delete_playbook_entry", { id });
        if (expandedId === id) setExpandedId(null);
        await loadEntries();
      } catch (err) {
        console.error("Failed to delete playbook entry:", err);
      }
    },
    [loadEntries, expandedId],
  );

  const handleToggleExpand = useCallback((id: string) => {
    setExpandedId((prev) => (prev === id ? null : id));
  }, []);

  const criticalEntries = entries.filter((e) => e.severity === "critical");
  const importantEntries = entries.filter((e) => e.severity === "important");
  const minorEntries = entries.filter((e) => e.severity === "minor");

  return (
    <div style={{ padding: "16px" }}>
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: "16px",
        }}
      >
        <h3 style={{ margin: 0 }}>Playbook Lessons</h3>
        <div style={{ display: "flex", gap: "8px" }}>
          <select
            aria-label="Filter by domain"
            value={domainFilter}
            onChange={(e) => setDomainFilter(e.target.value)}
            style={{
              padding: "4px 8px",
              borderRadius: "4px",
              border: "1px solid #374151",
              background: "#1f2937",
              color: "#e5e7eb",
            }}
          >
            <option value="">All Domains</option>
            <option value="compilation">Compilation</option>
            <option value="api">API</option>
            <option value="ui">UI</option>
            <option value="database">Database</option>
            <option value="security">Security</option>
            <option value="general">General</option>
          </select>
          <select
            aria-label="Filter by status"
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value)}
            style={{
              padding: "4px 8px",
              borderRadius: "4px",
              border: "1px solid #374151",
              background: "#1f2937",
              color: "#e5e7eb",
            }}
          >
            <option value="">All Status</option>
            <option value="active">Active</option>
            <option value="staged">Staged</option>
            <option value="retired">Retired</option>
          </select>
          <button
            onClick={loadEntries}
            style={{
              padding: "4px 12px",
              borderRadius: "4px",
              background: "#3b82f6",
              color: "white",
              border: "none",
              cursor: "pointer",
            }}
          >
            Refresh
          </button>
        </div>
      </div>

      {loading ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>Loading...</div>
      ) : entries.length === 0 ? (
        <div style={{ textAlign: "center", padding: "32px", color: "#9ca3af" }}>
          No playbook entries found. Lessons are automatically extracted from workflow runs.
        </div>
      ) : (
        <>
          {criticalEntries.length > 0 && (
            <LessonGroup
              title="Critical"
              color={SEVERITY_COLORS.critical}
              entries={criticalEntries}
              expandedId={expandedId}
              onToggleExpand={handleToggleExpand}
              onStatusChange={handleStatusChange}
              onDelete={handleDelete}
            />
          )}
          {importantEntries.length > 0 && (
            <LessonGroup
              title="Important"
              color={SEVERITY_COLORS.important}
              entries={importantEntries}
              expandedId={expandedId}
              onToggleExpand={handleToggleExpand}
              onStatusChange={handleStatusChange}
              onDelete={handleDelete}
            />
          )}
          {minorEntries.length > 0 && (
            <LessonGroup
              title="Minor"
              color={SEVERITY_COLORS.minor}
              entries={minorEntries}
              expandedId={expandedId}
              onToggleExpand={handleToggleExpand}
              onStatusChange={handleStatusChange}
              onDelete={handleDelete}
            />
          )}
        </>
      )}
    </div>
  );
}

interface LessonGroupProps {
  title: string;
  color: string;
  entries: PlaybookEntry[];
  expandedId: string | null;
  onToggleExpand: (id: string) => void;
  onStatusChange: (id: string, newStatus: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
}

function LessonGroup({
  title,
  color,
  entries,
  expandedId,
  onToggleExpand,
  onStatusChange,
  onDelete,
}: LessonGroupProps) {
  return (
    <div style={{ marginBottom: "16px" }}>
      <h4
        style={{
          color,
          margin: "0 0 8px 0",
          fontSize: "14px",
          textTransform: "uppercase",
          letterSpacing: "0.05em",
        }}
      >
        {title} ({entries.length})
      </h4>
      <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
        {entries.map((entry) => (
          <LessonRow
            key={entry.id}
            entry={entry}
            isExpanded={expandedId === entry.id}
            onToggleExpand={onToggleExpand}
            onStatusChange={onStatusChange}
            onDelete={onDelete}
          />
        ))}
      </div>
    </div>
  );
}

interface LessonRowProps {
  entry: PlaybookEntry;
  isExpanded: boolean;
  onToggleExpand: (id: string) => void;
  onStatusChange: (id: string, newStatus: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
}

type DetailState = {
  detail: PlaybookEntryDetail | null;
  loading: boolean;
};

type DetailAction =
  | { type: "reset" }
  | { type: "loading" }
  | { type: "loaded"; detail: PlaybookEntryDetail }
  | { type: "error" };

function detailReducer(_state: DetailState, action: DetailAction): DetailState {
  switch (action.type) {
    case "reset":
      return { detail: null, loading: false };
    case "loading":
      return { detail: null, loading: true };
    case "loaded":
      return { detail: action.detail, loading: false };
    case "error":
      return { detail: null, loading: false };
  }
}

function LessonRow({
  entry,
  isExpanded,
  onToggleExpand,
  onStatusChange,
  onDelete,
}: LessonRowProps) {
  const [detailState, dispatchDetail] = useReducer(detailReducer, { detail: null, loading: false });
  const { detail, loading: detailLoading } = detailState;
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const confirmTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const prefix = entry.positive ? "DO" : "DON'T";
  const prefixColor = entry.positive ? "#22c55e" : "#ef4444";
  const helpfulness =
    entry.times_applied > 0 ? `${(entry.helpfulness_ratio * 100).toFixed(0)}%` : "n/a";

  // Load detail when expanded
  useEffect(() => {
    if (!isExpanded) {
      dispatchDetail({ type: "reset" });
      return;
    }

    let cancelled = false;
    dispatchDetail({ type: "loading" });
    (async () => {
      try {
        const result = await invoke<PlaybookEntryDetail>("get_playbook_entry_detail", {
          id: entry.id,
        });
        if (!cancelled) dispatchDetail({ type: "loaded", detail: result });
      } catch (err) {
        console.error("Failed to load entry detail:", err);
        if (!cancelled) dispatchDetail({ type: "error" });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [isExpanded, entry.id]);

  // Clear delete confirmation timeout on unmount
  useEffect(() => {
    return () => {
      if (confirmTimeoutRef.current) clearTimeout(confirmTimeoutRef.current);
    };
  }, []);

  const handleDeleteClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (confirmingDelete) {
      setConfirmingDelete(false);
      if (confirmTimeoutRef.current) clearTimeout(confirmTimeoutRef.current);
      onDelete(entry.id);
    } else {
      setConfirmingDelete(true);
      confirmTimeoutRef.current = setTimeout(() => setConfirmingDelete(false), 3000);
    }
  };

  const handleStatusClick = (e: React.MouseEvent, newStatus: string) => {
    e.stopPropagation();
    onStatusChange(entry.id, newStatus);
  };

  const actionButtonStyle: React.CSSProperties = {
    padding: "2px 8px",
    borderRadius: "3px",
    border: "1px solid #374151",
    background: "#1f2937",
    color: "#d1d5db",
    cursor: "pointer",
    fontSize: "11px",
    lineHeight: "18px",
    whiteSpace: "nowrap",
  };

  return (
    <div>
      <div
        onClick={() => onToggleExpand(entry.id)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: "8px",
          padding: "8px 12px",
          background: isExpanded ? "#1a2332" : "#111827",
          borderRadius: isExpanded ? "4px 4px 0 0" : "4px",
          border: "1px solid #1f2937",
          borderBottom: isExpanded ? "1px solid #253347" : "1px solid #1f2937",
          fontSize: "13px",
          cursor: "pointer",
          userSelect: "none",
        }}
      >
        <span style={{ color: "#6b7280", fontSize: "10px", width: "12px", flexShrink: 0 }}>
          {isExpanded ? "\u25BC" : "\u25B6"}
        </span>
        <span style={{ color: prefixColor, fontWeight: "bold", minWidth: "50px", flexShrink: 0 }}>
          {prefix}
        </span>
        <span style={{ flex: 1, color: "#e5e7eb" }}>{entry.lesson}</span>
        <span
          style={{
            color: "#6b7280",
            fontSize: "11px",
            minWidth: "80px",
            textAlign: "right",
            flexShrink: 0,
          }}
        >
          {CATEGORY_LABELS[entry.category] || entry.category}
        </span>
        <span
          style={{
            color: entry.helpfulness_ratio > 0.5 ? "#22c55e" : "#9ca3af",
            fontSize: "11px",
            minWidth: "40px",
            textAlign: "right",
            flexShrink: 0,
          }}
          title={`Applied ${entry.times_applied}x, helped ${entry.times_helped}x`}
        >
          {helpfulness}
        </span>

        {/* Status/action buttons */}
        <div style={{ display: "flex", gap: "4px", marginLeft: "8px", flexShrink: 0 }}>
          {entry.status === "staged" && (
            <button
              onClick={(e) => handleStatusClick(e, "active")}
              style={{ ...actionButtonStyle, borderColor: "#22543d", color: "#4ade80" }}
              title="Activate this lesson"
            >
              Activate
            </button>
          )}
          {entry.status === "active" && (
            <button
              onClick={(e) => handleStatusClick(e, "retired")}
              style={{ ...actionButtonStyle, borderColor: "#713f12", color: "#fbbf24" }}
              title="Retire this lesson"
            >
              Retire
            </button>
          )}
          <button
            onClick={handleDeleteClick}
            style={{
              ...actionButtonStyle,
              borderColor: confirmingDelete ? "#991b1b" : "#374151",
              color: confirmingDelete ? "#fca5a5" : "#9ca3af",
              background: confirmingDelete ? "#450a0a" : "#1f2937",
            }}
            title={confirmingDelete ? "Click again to confirm deletion" : "Delete this lesson"}
          >
            {confirmingDelete ? "Confirm?" : "Delete"}
          </button>
        </div>
      </div>

      {/* Expanded detail panel */}
      {isExpanded && (
        <div
          style={{
            padding: "12px 16px",
            background: "#0f1724",
            border: "1px solid #1f2937",
            borderTop: "none",
            borderRadius: "0 0 4px 4px",
            fontSize: "12px",
            color: "#9ca3af",
          }}
        >
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "120px 1fr",
              gap: "4px 12px",
              marginBottom: "8px",
            }}
          >
            <span style={{ color: "#6b7280" }}>Status:</span>
            <span style={{ color: statusColor(entry.status) }}>{entry.status}</span>
            <span style={{ color: "#6b7280" }}>Category:</span>
            <span>{CATEGORY_LABELS[entry.category] || entry.category}</span>
            <span style={{ color: "#6b7280" }}>Domain:</span>
            <span>{entry.domain || "General"}</span>
            <span style={{ color: "#6b7280" }}>Applied / Helped:</span>
            <span>
              {entry.times_applied}x / {entry.times_helped}x
            </span>
            <span style={{ color: "#6b7280" }}>Created:</span>
            <span>{formatDate(entry.created_at)}</span>
          </div>

          {detailLoading && (
            <div style={{ color: "#6b7280", fontStyle: "italic" }}>Loading source run info...</div>
          )}

          {!detailLoading && detail?.source_run && (
            <div
              style={{
                marginTop: "8px",
                padding: "8px 12px",
                background: "#111827",
                borderRadius: "4px",
                border: "1px solid #1f2937",
              }}
            >
              <div
                style={{
                  color: "#d1d5db",
                  fontWeight: "bold",
                  marginBottom: "6px",
                  fontSize: "11px",
                  textTransform: "uppercase",
                  letterSpacing: "0.05em",
                }}
              >
                Source Run
              </div>
              <div style={{ display: "grid", gridTemplateColumns: "120px 1fr", gap: "3px 12px" }}>
                <span style={{ color: "#6b7280" }}>Run ID:</span>
                <span style={{ fontFamily: "monospace", fontSize: "11px" }}>
                  {detail.source_run.run_id}
                </span>
                <span style={{ color: "#6b7280" }}>Status:</span>
                <span style={{ color: statusColor(detail.source_run.status) }}>
                  {detail.source_run.status}
                </span>
                {detail.source_run.duration_secs != null && (
                  <>
                    <span style={{ color: "#6b7280" }}>Duration:</span>
                    <span>{formatDuration(detail.source_run.duration_secs)}</span>
                  </>
                )}
                {detail.source_run.iterations != null && (
                  <>
                    <span style={{ color: "#6b7280" }}>Iterations:</span>
                    <span>{detail.source_run.iterations}</span>
                  </>
                )}
                {detail.source_run.strategy && (
                  <>
                    <span style={{ color: "#6b7280" }}>Strategy:</span>
                    <span>{detail.source_run.strategy}</span>
                  </>
                )}
                {detail.source_run.architecture && (
                  <>
                    <span style={{ color: "#6b7280" }}>Architecture:</span>
                    <span>{detail.source_run.architecture}</span>
                  </>
                )}
                <span style={{ color: "#6b7280" }}>Created:</span>
                <span>{formatDate(detail.source_run.created_at)}</span>
              </div>
            </div>
          )}

          {!detailLoading && detail && !detail.source_run && (
            <div style={{ color: "#6b7280", fontStyle: "italic", marginTop: "4px" }}>
              No source run information available.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function statusColor(status: string): string {
  switch (status) {
    case "active":
    case "completed":
    case "success":
      return "#4ade80";
    case "staged":
    case "running":
      return "#60a5fa";
    case "retired":
      return "#f59e0b";
    case "failed":
    case "error":
      return "#ef4444";
    default:
      return "#9ca3af";
  }
}

function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  const remainingSecs = secs % 60;
  if (mins < 60) return `${mins}m ${remainingSecs}s`;
  const hours = Math.floor(mins / 60);
  const remainingMins = mins % 60;
  return `${hours}h ${remainingMins}m`;
}

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

export default PlaybookPanel;
