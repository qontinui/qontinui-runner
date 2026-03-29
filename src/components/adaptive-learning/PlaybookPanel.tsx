/**
 * PlaybookPanel — Browse lessons from the adaptive learning playbook.
 *
 * Shows lessons grouped by severity with helpfulness ratios and DO/DON'T indicators.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

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

export function PlaybookPanel() {
  const [entries, setEntries] = useState<PlaybookEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [domainFilter, setDomainFilter] = useState<string>("");
  const [statusFilter, setStatusFilter] = useState<string>("active");

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

  useEffect(() => {
    loadEntries();
  }, [loadEntries]);

  const criticalEntries = entries.filter((e) => e.severity === "critical");
  const importantEntries = entries.filter((e) => e.severity === "important");
  const minorEntries = entries.filter((e) => e.severity === "minor");

  return (
    <div style={{ padding: "16px" }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px" }}>
        <h3 style={{ margin: 0 }}>Playbook Lessons</h3>
        <div style={{ display: "flex", gap: "8px" }}>
          <select
            value={domainFilter}
            onChange={(e) => setDomainFilter(e.target.value)}
            style={{ padding: "4px 8px", borderRadius: "4px", border: "1px solid #374151", background: "#1f2937", color: "#e5e7eb" }}
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
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value)}
            style={{ padding: "4px 8px", borderRadius: "4px", border: "1px solid #374151", background: "#1f2937", color: "#e5e7eb" }}
          >
            <option value="">All Status</option>
            <option value="active">Active</option>
            <option value="staged">Staged</option>
            <option value="retired">Retired</option>
          </select>
          <button onClick={loadEntries} style={{ padding: "4px 12px", borderRadius: "4px", background: "#3b82f6", color: "white", border: "none", cursor: "pointer" }}>
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
            <LessonGroup title="Critical" color={SEVERITY_COLORS.critical} entries={criticalEntries} />
          )}
          {importantEntries.length > 0 && (
            <LessonGroup title="Important" color={SEVERITY_COLORS.important} entries={importantEntries} />
          )}
          {minorEntries.length > 0 && (
            <LessonGroup title="Minor" color={SEVERITY_COLORS.minor} entries={minorEntries} />
          )}
        </>
      )}
    </div>
  );
}

function LessonGroup({ title, color, entries }: { title: string; color: string; entries: PlaybookEntry[] }) {
  return (
    <div style={{ marginBottom: "16px" }}>
      <h4 style={{ color, margin: "0 0 8px 0", fontSize: "14px", textTransform: "uppercase", letterSpacing: "0.05em" }}>
        {title} ({entries.length})
      </h4>
      <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
        {entries.map((entry) => (
          <LessonRow key={entry.id} entry={entry} />
        ))}
      </div>
    </div>
  );
}

function LessonRow({ entry }: { entry: PlaybookEntry }) {
  const prefix = entry.positive ? "DO" : "DON'T";
  const prefixColor = entry.positive ? "#22c55e" : "#ef4444";
  const helpfulness = entry.times_applied > 0
    ? `${(entry.helpfulness_ratio * 100).toFixed(0)}%`
    : "n/a";

  return (
    <div
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: "8px",
        padding: "8px 12px",
        background: "#111827",
        borderRadius: "4px",
        border: "1px solid #1f2937",
        fontSize: "13px",
      }}
    >
      <span style={{ color: prefixColor, fontWeight: "bold", minWidth: "50px", flexShrink: 0 }}>
        {prefix}
      </span>
      <span style={{ flex: 1, color: "#e5e7eb" }}>{entry.lesson}</span>
      <span style={{ color: "#6b7280", fontSize: "11px", minWidth: "80px", textAlign: "right" }}>
        {CATEGORY_LABELS[entry.category] || entry.category}
      </span>
      <span
        style={{
          color: entry.helpfulness_ratio > 0.5 ? "#22c55e" : "#9ca3af",
          fontSize: "11px",
          minWidth: "60px",
          textAlign: "right",
        }}
        title={`Applied ${entry.times_applied}x, helped ${entry.times_helped}x`}
      >
        {helpfulness}
      </span>
    </div>
  );
}

export default PlaybookPanel;
