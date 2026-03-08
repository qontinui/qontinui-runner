/**
 * SpecIssuesPanel — Issues tab for the spec detail panel.
 *
 * Shows known issues scoped to the current spec, with ability to
 * create new issues and manage existing ones.
 */

import { useEffect, useState, useCallback } from "react";
import {
  AlertTriangle,
  Plus,
  Bug,
  Clock,
  CheckCircle2,
  Trash2,
  X,
  ChevronDown,
} from "lucide-react";
import { useKnownIssues } from "@/hooks/useKnownIssues";
import type {
  KnownIssue,
  CreateKnownIssueRequest,
  IssueCategory,
  KnownIssueSeverity,
} from "@qontinui/shared-types";
import { ISSUE_CATEGORIES, ISSUE_SEVERITIES } from "@qontinui/shared-types";

// ============================================================================
// Sub-components
// ============================================================================

const SEVERITY_STYLES: Record<string, string> = {
  critical: "bg-red-500/15 text-red-400 border-red-500/30",
  high: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  medium: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  low: "bg-blue-500/15 text-blue-400 border-blue-500/30",
};

const CATEGORY_STYLES: Record<string, string> = {
  duplication: "bg-purple-500/15 text-purple-400 border-purple-500/30",
  rendering: "bg-cyan-500/15 text-cyan-400 border-cyan-500/30",
  data_integrity: "bg-green-500/15 text-green-400 border-green-500/30",
  timing: "bg-yellow-500/15 text-yellow-400 border-yellow-500/30",
  state: "bg-indigo-500/15 text-indigo-400 border-indigo-500/30",
  layout: "bg-pink-500/15 text-pink-400 border-pink-500/30",
  performance: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  encoding: "bg-teal-500/15 text-teal-400 border-teal-500/30",
  navigation: "bg-sky-500/15 text-sky-400 border-sky-500/30",
  authentication: "bg-red-500/15 text-red-400 border-red-500/30",
  other: "bg-gray-500/15 text-gray-400 border-gray-500/30",
};

function Badge({ text, style }: { text: string; style?: string }) {
  return (
    <span
      className={`text-[10px] px-1.5 py-0.5 rounded border font-medium ${style || "bg-gray-500/15 text-gray-400 border-gray-500/30"}`}
    >
      {text}
    </span>
  );
}

function IssueCard({
  issue,
  onResolve,
  onDelete,
}: {
  issue: KnownIssue;
  onResolve: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  const isGlobal = issue.scope_type === "global";

  return (
    <div className="border border-border rounded-lg p-3 space-y-2 hover:border-border/80 transition-colors">
      <div className="flex items-start justify-between gap-2">
        <div className="flex items-center gap-1.5 min-w-0">
          <Bug className="w-3.5 h-3.5 text-amber-400 shrink-0" />
          <span className="text-sm font-medium truncate">{issue.title}</span>
        </div>
        <div className="flex items-center gap-1 shrink-0">
          <button
            onClick={() => onResolve(issue.id)}
            className="p-1 rounded hover:bg-green-500/20 text-muted-foreground hover:text-green-400 transition-colors"
            title="Resolve issue"
          >
            <CheckCircle2 className="w-3.5 h-3.5" />
          </button>
          <button
            onClick={() => onDelete(issue.id)}
            className="p-1 rounded hover:bg-red-500/20 text-muted-foreground hover:text-red-400 transition-colors"
            title="Delete issue"
          >
            <Trash2 className="w-3.5 h-3.5" />
          </button>
        </div>
      </div>

      <p className="text-xs text-muted-foreground line-clamp-2">{issue.description}</p>

      <div className="flex items-center gap-1.5 flex-wrap">
        <Badge text={issue.severity} style={SEVERITY_STYLES[issue.severity]} />
        <Badge text={issue.category.replace("_", " ")} style={CATEGORY_STYLES[issue.category]} />
        {isGlobal && (
          <Badge text="global" style="bg-gray-500/15 text-gray-400 border-gray-500/30" />
        )}
        {issue.times_detected > 1 && (
          <span className="text-[10px] text-muted-foreground flex items-center gap-0.5">
            <AlertTriangle className="w-3 h-3" />
            {issue.times_detected}x
          </span>
        )}
        {issue.last_detected_at && (
          <span className="text-[10px] text-muted-foreground flex items-center gap-0.5">
            <Clock className="w-3 h-3" />
            {new Date(issue.last_detected_at).toLocaleDateString()}
          </span>
        )}
      </div>

      {issue.verification_hint && (
        <p className="text-[11px] text-muted-foreground/70 italic">
          Hint: {issue.verification_hint}
        </p>
      )}
    </div>
  );
}

// ============================================================================
// Create Issue Form
// ============================================================================

interface CreateIssueFormProps {
  specId: string;
  onSubmit: (req: CreateKnownIssueRequest) => Promise<void>;
  onCancel: () => void;
}

function CreateIssueForm({ specId, onSubmit, onCancel }: CreateIssueFormProps) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState<IssueCategory>("other");
  const [severity, setSeverity] = useState<KnownIssueSeverity>("medium");
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
        scope_type: "spec",
        scope_value: specId,
        verification_hint: verificationHint.trim() || undefined,
        reproduction_context: reproductionContext.trim() || undefined,
      });
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="border border-border rounded-lg p-3 space-y-3 bg-card">
      <div className="flex items-center justify-between">
        <span className="text-sm font-medium">Report Issue</span>
        <button onClick={onCancel} className="p-1 rounded hover:bg-muted text-muted-foreground">
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Title */}
      <input
        type="text"
        placeholder="Issue title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className="w-full px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50"
        autoFocus
      />

      {/* Description */}
      <textarea
        placeholder="Describe the issue — what you observed, what's wrong"
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        className="w-full px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50 min-h-[60px] resize-y"
        rows={3}
      />

      {/* Category + Severity row */}
      <div className="flex gap-2">
        <div className="flex-1">
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Category
          </label>
          <select
            value={category}
            onChange={(e) => setCategory(e.target.value as IssueCategory)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50"
          >
            {ISSUE_CATEGORIES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <div className="flex-1">
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Severity
          </label>
          <select
            value={severity}
            onChange={(e) => setSeverity(e.target.value as KnownIssueSeverity)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50"
          >
            {ISSUE_SEVERITIES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Verification hint */}
      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Verification Hint (optional)
        </label>
        <input
          type="text"
          placeholder="How to check for this issue"
          value={verificationHint}
          onChange={(e) => setVerificationHint(e.target.value)}
          className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50"
        />
      </div>

      {/* Reproduction context */}
      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Reproduction Context (optional)
        </label>
        <input
          type="text"
          placeholder="When does this happen?"
          value={reproductionContext}
          onChange={(e) => setReproductionContext(e.target.value)}
          className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded focus:outline-none focus:ring-1 focus:ring-purple-500/50"
        />
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-2 pt-1">
        <button
          onClick={onCancel}
          className="px-3 py-1.5 text-xs rounded border border-border hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={handleSubmit}
          disabled={!title.trim() || !description.trim() || isSubmitting}
          className="px-3 py-1.5 text-xs rounded bg-purple-600 hover:bg-purple-700 text-white disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          {isSubmitting ? "Creating..." : "Create Issue"}
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// Main Panel
// ============================================================================

interface SpecIssuesPanelProps {
  specId: string;
  pageUrl?: string;
}

export function SpecIssuesPanel({ specId, pageUrl }: SpecIssuesPanelProps) {
  const { issues, isLoading, error, loadIssuesForSpec, createIssue, resolveIssue, deleteIssue } =
    useKnownIssues();
  const [showForm, setShowForm] = useState(false);

  useEffect(() => {
    loadIssuesForSpec(specId, pageUrl);
  }, [specId, pageUrl, loadIssuesForSpec]);

  const handleCreate = useCallback(
    async (req: CreateKnownIssueRequest) => {
      await createIssue(req);
      setShowForm(false);
    },
    [createIssue],
  );

  const handleResolve = useCallback(
    async (id: string) => {
      await resolveIssue(id);
      // Reload to update the list
      loadIssuesForSpec(specId, pageUrl);
    },
    [resolveIssue, loadIssuesForSpec, specId, pageUrl],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      await deleteIssue(id);
    },
    [deleteIssue],
  );

  const activeIssues = issues.filter((i) => i.status === "active");
  const resolvedIssues = issues.filter((i) => i.status !== "active");

  return (
    <div className="space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Bug className="w-4 h-4 text-amber-400" />
          <span className="text-sm font-medium">Known Issues</span>
          {activeIssues.length > 0 && (
            <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-amber-500/20 text-amber-400 font-medium">
              {activeIssues.length}
            </span>
          )}
        </div>
        {!showForm && (
          <button
            onClick={() => setShowForm(true)}
            className="flex items-center gap-1 px-2 py-1 text-xs rounded border border-border hover:bg-muted transition-colors"
          >
            <Plus className="w-3 h-3" />
            Report Issue
          </button>
        )}
      </div>

      {error && <p className="text-xs text-red-400">{error}</p>}

      {/* Create form */}
      {showForm && (
        <CreateIssueForm
          specId={specId}
          onSubmit={handleCreate}
          onCancel={() => setShowForm(false)}
        />
      )}

      {/* Loading */}
      {isLoading && issues.length === 0 && (
        <p className="text-xs text-muted-foreground">Loading issues...</p>
      )}

      {/* Active issues */}
      {activeIssues.length === 0 && !isLoading && !showForm && (
        <div className="text-center py-6 text-sm text-muted-foreground">
          <Bug className="w-8 h-8 mx-auto mb-2 opacity-30" />
          <p>No known issues for this spec</p>
          <p className="text-xs mt-1">
            Report issues you notice so they&apos;re checked in generated workflows
          </p>
        </div>
      )}

      {activeIssues.map((issue) => (
        <IssueCard key={issue.id} issue={issue} onResolve={handleResolve} onDelete={handleDelete} />
      ))}

      {/* Resolved issues (collapsed) */}
      {resolvedIssues.length > 0 && <ResolvedSection issues={resolvedIssues} />}
    </div>
  );
}

function ResolvedSection({ issues }: { issues: KnownIssue[] }) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="border-t border-border pt-2 mt-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
      >
        <ChevronDown className={`w-3 h-3 transition-transform ${expanded ? "" : "-rotate-90"}`} />
        {issues.length} resolved
      </button>
      {expanded && (
        <div className="mt-2 space-y-2 opacity-60">
          {issues.map((issue) => (
            <div key={issue.id} className="border border-border rounded p-2 text-xs">
              <span className="line-through">{issue.title}</span>
              <span className="ml-2 text-muted-foreground">
                ({issue.category.replace("_", " ")})
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
