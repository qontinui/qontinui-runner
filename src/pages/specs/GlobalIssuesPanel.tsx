/**
 * GlobalIssuesPanel — Global issue management embedded in the Specs page.
 *
 * Shows when no spec is selected in the center panel. Provides filtering,
 * search, creation, resolution, deletion, bulk operations, and export/import
 * of known issues across all scopes.
 */

import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Bug,
  Plus,
  Search,
  CheckCircle2,
  Trash2,
  ChevronDown,
  ChevronRight,
  Clock,
  AlertTriangle,
  X,
  Download,
  Upload,
  Sparkles,
} from "lucide-react";
import type {
  KnownIssue,
  CreateKnownIssueRequest,
  IssueCategory,
  KnownIssueSeverity,
  ListKnownIssuesQuery,
} from "@qontinui/shared-types";
import { ISSUE_CATEGORIES, ISSUE_SEVERITIES } from "@qontinui/shared-types";
import { getAllSpecs } from "@/lib/spec-registry";
import { SEVERITY_STYLES, CATEGORY_STYLES, STATUS_STYLES } from "./issue-styles";

type StatusFilter = "all" | "active" | "resolved" | "monitoring";

// ============================================================================
// Badge & ConfidenceBar
// ============================================================================

function Badge({ text, style }: { text: string; style?: string }) {
  return (
    <span
      className={`text-[10px] px-1.5 py-0.5 rounded border font-medium ${style || "bg-gray-500/15 text-gray-400 border-gray-500/30"}`}
    >
      {text}
    </span>
  );
}

function ConfidenceBar({ value }: { value: number }) {
  const pct = Math.round(value * 100);
  const color = pct >= 80 ? "bg-green-500" : pct >= 50 ? "bg-amber-500" : "bg-red-500";

  return (
    <div className="flex items-center gap-1.5">
      <div className="w-16 h-1.5 rounded-full bg-white/10 overflow-hidden">
        <div className={`h-full rounded-full ${color}`} style={{ width: `${pct}%` }} />
      </div>
      <span className="text-[10px] text-muted-foreground">{pct}%</span>
    </div>
  );
}

// ============================================================================
// Issue card
// ============================================================================

function IssueCard({
  issue,
  selected,
  onToggleSelect,
  onResolve,
  onDelete,
}: {
  issue: KnownIssue;
  selected: boolean;
  onToggleSelect: () => void;
  onResolve: () => void;
  onDelete: () => void;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div
      className={`border rounded-lg p-3 space-y-2 transition-colors ${
        selected ? "border-primary/50 bg-primary/5" : "border-border hover:border-border/80"
      }`}
    >
      {/* Top row */}
      <div className="flex items-start gap-2">
        <input
          type="checkbox"
          aria-label="Select issue"
          checked={selected}
          onChange={onToggleSelect}
          className="mt-1 accent-primary"
        />
        <div className="flex-1 min-w-0">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex items-center gap-1.5 text-left w-full group"
          >
            {expanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            )}
            <Bug className="w-3.5 h-3.5 text-amber-400 shrink-0" />
            <span className="text-sm font-medium truncate group-hover:text-primary transition-colors">
              {issue.title}
            </span>
          </button>
          {!expanded && (
            <p className="text-xs text-muted-foreground line-clamp-1 ml-[52px] mt-0.5">
              {issue.description}
            </p>
          )}
        </div>
        <div className="flex items-center gap-1 shrink-0">
          {issue.status === "active" && (
            <button
              onClick={onResolve}
              className="p-1 rounded hover:bg-green-500/20 text-muted-foreground hover:text-green-400 transition-colors"
              title="Resolve issue"
            >
              <CheckCircle2 className="w-3.5 h-3.5" />
            </button>
          )}
          <button
            onClick={onDelete}
            className="p-1 rounded hover:bg-red-500/20 text-muted-foreground hover:text-red-400 transition-colors"
            title="Delete issue"
          >
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        </div>
      </div>

      {/* Badges row */}
      <div className="flex items-center gap-1.5 flex-wrap ml-6">
        <Badge text={issue.severity} style={SEVERITY_STYLES[issue.severity]} />
        <Badge text={issue.category.replace(/_/g, " ")} style={CATEGORY_STYLES[issue.category]} />
        <Badge text={issue.status} style={STATUS_STYLES[issue.status]} />
        {issue.scope_type !== "global" && (
          <Badge
            text={`${issue.scope_type}: ${issue.scope_value || "?"}`}
            style="bg-gray-500/15 text-gray-400 border-gray-500/30"
          />
        )}
        {issue.scope_type === "global" && (
          <Badge text="global" style="bg-gray-500/15 text-gray-400 border-gray-500/30" />
        )}
        {issue.times_detected > 1 && (
          <span className="text-[10px] text-muted-foreground flex items-center gap-0.5">
            <AlertTriangle className="w-3 h-3" />
            {issue.times_detected}x detected
          </span>
        )}
        {issue.last_detected_at && (
          <span className="text-[10px] text-muted-foreground flex items-center gap-0.5">
            <Clock className="w-3 h-3" />
            {new Date(issue.last_detected_at).toLocaleDateString()}
          </span>
        )}
        <ConfidenceBar value={issue.confidence} />
      </div>

      {/* Expanded details */}
      {expanded && (
        <div className="ml-6 mt-2 space-y-2 text-xs border-t border-border pt-2">
          <div>
            <span className="text-muted-foreground font-medium">Description:</span>
            <p className="text-foreground mt-0.5">{issue.description}</p>
          </div>
          {issue.verification_hint && (
            <div>
              <span className="text-muted-foreground font-medium">Verification Hint:</span>
              <p className="text-foreground mt-0.5 italic">{issue.verification_hint}</p>
            </div>
          )}
          {issue.reproduction_context && (
            <div>
              <span className="text-muted-foreground font-medium">Reproduction Context:</span>
              <p className="text-foreground mt-0.5">{issue.reproduction_context}</p>
            </div>
          )}
          {Object.keys(issue.detection_config).length > 0 && (
            <div>
              <span className="text-muted-foreground font-medium">Detection Config:</span>
              <pre className="text-foreground mt-0.5 bg-white/5 p-2 rounded overflow-x-auto text-[11px]">
                {JSON.stringify(issue.detection_config, null, 2)}
              </pre>
            </div>
          )}
          {issue.trigger_conditions.length > 0 && (
            <div>
              <span className="text-muted-foreground font-medium">Trigger Conditions:</span>
              <ul className="list-disc list-inside mt-0.5">
                {issue.trigger_conditions.map((tc, i) => (
                  <li key={`${tc}-${i}`} className="text-foreground">
                    {tc}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="flex items-center gap-4 text-muted-foreground">
            <span>Provenance: {issue.provenance}</span>
            <span>Checked: {issue.times_checked}x</span>
            {issue.resolved_at && (
              <span>Resolved: {new Date(issue.resolved_at).toLocaleDateString()}</span>
            )}
            <span>Created: {new Date(issue.created_at).toLocaleDateString()}</span>
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Create Issue Form
// ============================================================================

function CreateIssueForm({
  onSubmit,
  onCancel,
}: {
  onSubmit: (req: CreateKnownIssueRequest) => Promise<void>;
  onCancel: () => void;
}) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState<IssueCategory>("other");
  const [severity, setSeverity] = useState<KnownIssueSeverity>("medium");
  const [scopeType, setScopeType] = useState<"global" | "spec" | "url" | "component" | "feature">(
    "spec",
  );
  const [scopeValue, setScopeValue] = useState("");
  const [verificationHint, setVerificationHint] = useState("");
  const [reproductionContext, setReproductionContext] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!title.trim() || !description.trim()) return;
    setIsSubmitting(true);
    try {
      await onSubmit({
        title: title.trim(),
        description: description.trim(),
        category,
        severity,
        detection_method: "ai_judgment",
        scope_type: scopeType,
        scope_value: scopeType !== "global" ? scopeValue.trim() || undefined : undefined,
        verification_hint: verificationHint.trim() || undefined,
        reproduction_context: reproductionContext.trim() || undefined,
      });
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="border border-primary/30 rounded-lg p-4 space-y-3 bg-card">
      <div className="flex items-center justify-between">
        <span className="text-sm font-medium flex items-center gap-2">
          <Plus className="w-4 h-4 text-primary" />
          New Issue
        </span>
        <button onClick={onCancel} className="p-1 rounded hover:bg-muted text-muted-foreground">
          <X className="w-4 h-4" />
        </button>
      </div>

      <input
        type="text"
        aria-label="Issue title"
        placeholder="Issue title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
        autoFocus
      />

      <textarea
        aria-label="Issue description"
        placeholder="Describe the issue -- what you observed, what is wrong"
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50 min-h-[80px] resize-y"
        rows={3}
      />

      <div className="grid grid-cols-3 gap-3">
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Category
          </label>
          <select
            aria-label="Category"
            value={category}
            onChange={(e) => setCategory(e.target.value as IssueCategory)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            {ISSUE_CATEGORIES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Severity
          </label>
          <select
            aria-label="Severity"
            value={severity}
            onChange={(e) => setSeverity(e.target.value as KnownIssueSeverity)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            {ISSUE_SEVERITIES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Scope
          </label>
          <select
            aria-label="Scope"
            value={scopeType}
            onChange={(e) =>
              setScopeType(e.target.value as "global" | "spec" | "url" | "component" | "feature")
            }
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            <option value="global">Global</option>
            <option value="spec">Spec</option>
            <option value="url">URL</option>
            <option value="component">Component</option>
            <option value="feature">Feature</option>
          </select>
        </div>
      </div>

      {scopeType !== "global" && scopeType === "spec" ? (
        <select
          aria-label="Spec scope value"
          value={scopeValue}
          onChange={(e) => setScopeValue(e.target.value)}
          className="w-full px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
        >
          <option value="">Select a page spec...</option>
          {getAllSpecs().map((s) => {
            const displayName = s.specId.replace(/^runner:/, "");
            return (
              <option key={s.specId} value={s.specId}>
                {displayName} ({s.appName})
              </option>
            );
          })}
        </select>
      ) : scopeType !== "global" ? (
        <input
          type="text"
          aria-label="Scope identifier"
          placeholder={`${scopeType} identifier`}
          value={scopeValue}
          onChange={(e) => setScopeValue(e.target.value)}
          className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
        />
      ) : null}

      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Verification Hint (optional)
        </label>
        <input
          type="text"
          aria-label="Verification Hint (optional)"
          placeholder="How to check for this issue"
          value={verificationHint}
          onChange={(e) => setVerificationHint(e.target.value)}
          className="w-full mt-0.5 px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
        />
      </div>

      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Reproduction Context (optional)
        </label>
        <input
          type="text"
          aria-label="Reproduction Context (optional)"
          placeholder="When does this happen?"
          value={reproductionContext}
          onChange={(e) => setReproductionContext(e.target.value)}
          className="w-full mt-0.5 px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
        />
      </div>

      <div className="flex justify-end gap-2 pt-1">
        <button
          onClick={onCancel}
          className="px-3 py-1.5 text-xs rounded-md border border-border hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={handleSubmit}
          disabled={!title.trim() || !description.trim() || isSubmitting}
          className="px-3 py-1.5 text-xs rounded-md bg-primary hover:bg-primary/90 text-primary-foreground disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          {isSubmitting ? "Creating..." : "Create Issue"}
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// Main panel
// ============================================================================

export function GlobalIssuesPanel() {
  const [issues, setIssues] = useState<KnownIssue[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [statusFilter, setStatusFilter] = useState<StatusFilter>("active");
  const [categoryFilter, setCategoryFilter] = useState<string>("all");
  const [severityFilter, setSeverityFilter] = useState<string>("all");
  const [searchQuery, setSearchQuery] = useState("");

  const [showForm, setShowForm] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [autoDetectedToast, setAutoDetectedToast] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // ------ Data loading ------

  const loadIssues = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const query: ListKnownIssuesQuery = {};
      if (statusFilter !== "all") query.status = statusFilter;
      if (categoryFilter !== "all") query.category = categoryFilter;
      if (severityFilter !== "all") query.severity = severityFilter;

      const result = await invoke<KnownIssue[]>("list_known_issues", { query });
      setIssues(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsLoading(false);
    }
  }, [statusFilter, categoryFilter, severityFilter]);

  useEffect(() => {
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void loadIssues();
    });
    return () => {
      cancelled = true;
    };
  }, [loadIssues]);

  // Listen for auto-detected known issues
  useEffect(() => {
    const unlisten = listen<{ count: number; issue_ids: string[] }>(
      "known-issues-auto-detected",
      (event) => {
        const { count } = event.payload;
        const msg =
          count === 1
            ? "1 new known issue auto-detected from recurring findings"
            : `${count} new known issues auto-detected from recurring findings`;
        setAutoDetectedToast(msg);
        setTimeout(() => setAutoDetectedToast(null), 8000);
        loadIssues();
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [loadIssues]);

  // ------ Filtered issues ------

  const filteredIssues = useMemo(() => {
    if (!searchQuery.trim()) return issues;
    const q = searchQuery.toLowerCase();
    return issues.filter(
      (i) =>
        i.title.toLowerCase().includes(q) ||
        i.description.toLowerCase().includes(q) ||
        i.category.toLowerCase().includes(q),
    );
  }, [issues, searchQuery]);

  // ------ Stats ------

  const stats = useMemo(() => {
    return {
      total: issues.length,
      critical: issues.filter((i) => i.severity === "critical" && i.status === "active").length,
      high: issues.filter((i) => i.severity === "high" && i.status === "active").length,
      medium: issues.filter((i) => i.severity === "medium" && i.status === "active").length,
      low: issues.filter((i) => i.severity === "low" && i.status === "active").length,
    };
  }, [issues]);

  // ------ Actions ------

  const handleCreate = useCallback(
    async (req: CreateKnownIssueRequest) => {
      try {
        await invoke("create_known_issue", { request: req });
        setShowForm(false);
        await loadIssues();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [loadIssues],
  );

  const handleResolve = useCallback(
    async (id: string) => {
      try {
        await invoke("resolve_known_issue", { id, resolution: "Resolved from Specs page" });
        await loadIssues();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [loadIssues],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await invoke<boolean>("delete_known_issue", { id });
        setSelectedIds((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
        await loadIssues();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [loadIssues],
  );

  const handleBulkResolve = useCallback(async () => {
    for (const id of selectedIds) {
      try {
        await invoke("resolve_known_issue", { id, resolution: "Bulk resolved from Specs page" });
      } catch {
        // continue with others
      }
    }
    setSelectedIds(new Set());
    await loadIssues();
  }, [selectedIds, loadIssues]);

  const handleBulkDelete = useCallback(async () => {
    for (const id of selectedIds) {
      try {
        await invoke<boolean>("delete_known_issue", { id });
      } catch {
        // continue with others
      }
    }
    setSelectedIds(new Set());
    await loadIssues();
  }, [selectedIds, loadIssues]);

  const toggleSelect = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // ------ Export / Import ------

  const handleExport = useCallback(async () => {
    try {
      const jsonStr = await invoke<string>("export_known_issues", {
        status: statusFilter !== "all" ? statusFilter : null,
        category: categoryFilter !== "all" ? categoryFilter : null,
      });
      const blob = new Blob([jsonStr], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      const date = new Date().toISOString().slice(0, 10);
      a.href = url;
      a.download = `known-issues-export-${date}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [statusFilter, categoryFilter]);

  const handleImport = useCallback(
    async (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.target.files?.[0];
      if (!file) return;

      try {
        const fileContent = await file.text();
        const result = await invoke<{ imported: number; skipped: number }>("import_known_issues", {
          jsonData: fileContent,
        });
        setSuccessMessage(
          `Import complete: ${result.imported} imported, ${result.skipped} skipped`,
        );
        setTimeout(() => setSuccessMessage(null), 5000);
        await loadIssues();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (fileInputRef.current) {
          fileInputRef.current.value = "";
        }
      }
    },
    [loadIssues],
  );

  // ------ Pill helper ------

  const pillClass = (active: boolean) =>
    `px-3 py-1 text-xs rounded-full font-medium transition-colors ${
      active
        ? "bg-primary text-primary-foreground"
        : "bg-white/5 text-muted-foreground hover:text-foreground hover:bg-white/10"
    }`;

  // ============================================================================
  // Render
  // ============================================================================

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="shrink-0 border-b border-border px-4 py-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Bug className="w-4 h-4 text-amber-400" />
            <span className="text-sm font-semibold">All Issues</span>
            <div className="flex items-center gap-1.5 ml-1">
              <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-white/10 text-muted-foreground">
                {stats.total}
              </span>
              {stats.critical > 0 && (
                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-red-500/15 text-red-400">
                  {stats.critical} crit
                </span>
              )}
              {stats.high > 0 && (
                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-orange-500/15 text-orange-400">
                  {stats.high} high
                </span>
              )}
            </div>
          </div>
          <div className="flex items-center gap-1.5">
            <button
              onClick={handleExport}
              className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Export issues as JSON"
            >
              <Download className="w-3.5 h-3.5" />
            </button>
            <button
              onClick={() => fileInputRef.current?.click()}
              className="p-1.5 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Import issues from JSON"
            >
              <Upload className="w-3.5 h-3.5" />
            </button>
            <input
              ref={fileInputRef}
              type="file"
              accept=".json"
              onChange={handleImport}
              className="hidden"
            />
            {!showForm && (
              <button
                onClick={() => setShowForm(true)}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
              >
                <Plus className="w-3 h-3" />
                New
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Filters bar */}
      <div className="shrink-0 border-b border-border px-4 py-2">
        <div className="flex items-center gap-3 flex-wrap">
          {/* Status pills */}
          <div className="flex items-center gap-1">
            {(["all", "active", "resolved", "monitoring"] as StatusFilter[]).map((s) => (
              <button
                key={s}
                onClick={() => setStatusFilter(s)}
                className={pillClass(statusFilter === s)}
              >
                {s === "all" ? "All" : s.charAt(0).toUpperCase() + s.slice(1)}
              </button>
            ))}
          </div>

          <select
            aria-label="Filter by category"
            value={categoryFilter}
            onChange={(e) => setCategoryFilter(e.target.value)}
            className="px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            <option value="all">All Categories</option>
            {ISSUE_CATEGORIES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>

          <select
            aria-label="Filter by severity"
            value={severityFilter}
            onChange={(e) => setSeverityFilter(e.target.value)}
            className="px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
          >
            <option value="all">All Severities</option>
            {ISSUE_SEVERITIES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>

          <div className="flex items-center gap-1 flex-1 min-w-[140px] max-w-[240px]">
            <Search className="w-3.5 h-3.5 text-muted-foreground" />
            <input
              type="text"
              aria-label="Search issues"
              placeholder="Search..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="flex-1 px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-hidden focus:ring-1 focus:ring-primary/50"
            />
          </div>

          {/* Bulk actions */}
          {selectedIds.size > 0 && (
            <div className="flex items-center gap-1.5 ml-auto">
              <span className="text-[10px] text-muted-foreground">{selectedIds.size} sel.</span>
              <button
                onClick={handleBulkResolve}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-md border border-green-500/30 text-green-400 hover:bg-green-500/10 transition-colors"
              >
                <CheckCircle2 className="w-3 h-3" />
                Resolve
              </button>
              <button
                onClick={handleBulkDelete}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-md border border-red-500/30 text-red-400 hover:bg-red-500/10 transition-colors"
              >
                <Trash2 className="w-3 h-3" />
                Delete
              </button>
            </div>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
        {/* Error */}
        {error && (
          <div className="flex items-center gap-2 px-3 py-2 rounded-md bg-red-500/10 border border-red-500/30 text-red-400 text-sm">
            <AlertTriangle className="w-4 h-4 shrink-0" />
            {error}
            <button
              onClick={() => setError(null)}
              className="ml-auto p-0.5 hover:bg-red-500/20 rounded"
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        )}

        {/* Success */}
        {successMessage && (
          <div className="flex items-center gap-2 px-3 py-2 rounded-md bg-green-500/10 border border-green-500/30 text-green-400 text-sm">
            <CheckCircle2 className="w-4 h-4 shrink-0" />
            {successMessage}
            <button
              onClick={() => setSuccessMessage(null)}
              className="ml-auto p-0.5 hover:bg-green-500/20 rounded"
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        )}

        {/* Auto-detected toast */}
        {autoDetectedToast && (
          <div className="flex items-center gap-2 px-3 py-2 rounded-md bg-amber-500/10 border border-amber-500/30 text-amber-400 text-sm">
            <Sparkles className="w-4 h-4 shrink-0" />
            {autoDetectedToast}
            <button
              onClick={() => setAutoDetectedToast(null)}
              className="ml-auto p-0.5 hover:bg-amber-500/20 rounded"
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        )}

        {/* Create form */}
        {showForm && (
          <CreateIssueForm onSubmit={handleCreate} onCancel={() => setShowForm(false)} />
        )}

        {/* Loading */}
        {isLoading && issues.length === 0 && (
          <div className="text-center py-12 text-muted-foreground">
            <Bug className="w-8 h-8 mx-auto mb-2 opacity-30 animate-pulse" />
            <p className="text-sm">Loading issues...</p>
          </div>
        )}

        {/* Empty state */}
        {!isLoading && filteredIssues.length === 0 && (
          <div className="text-center py-12 text-muted-foreground">
            <Bug className="w-10 h-10 mx-auto mb-3 opacity-20" />
            <p className="text-sm font-medium">No issues found</p>
            <p className="text-xs mt-1">
              {searchQuery
                ? "Try adjusting your search or filters"
                : statusFilter !== "all"
                  ? `No ${statusFilter} issues. Try changing the status filter.`
                  : "Create a new issue to start tracking known problems."}
            </p>
          </div>
        )}

        {/* Issue list */}
        <div className="space-y-2">
          {filteredIssues.map((issue) => (
            <IssueCard
              key={issue.id}
              issue={issue}
              selected={selectedIds.has(issue.id)}
              onToggleSelect={() => toggleSelect(issue.id)}
              onResolve={() => handleResolve(issue.id)}
              onDelete={() => handleDelete(issue.id)}
            />
          ))}
        </div>
      </div>
    </div>
  );
}
