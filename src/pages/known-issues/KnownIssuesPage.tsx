/**
 * KnownIssuesPage -- Full-page issue management view.
 *
 * Provides listing, filtering, creation, resolution, and deletion of
 * known issues across all scopes.  Also includes a collapsible pattern
 * template browser at the bottom.
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
  FileCode,
  Layers,
  BarChart2,
  Download,
  Upload,
  Sparkles,
} from "lucide-react";
import type {
  KnownIssue,
  CreateKnownIssueRequest,
  CreatePatternTemplateRequest,
  IssuePatternTemplate,
  IssueCategory,
  KnownIssueSeverity,
  DetectionMethod,
  ListKnownIssuesQuery,
} from "@qontinui/shared-types";
import { ISSUE_CATEGORIES, ISSUE_SEVERITIES, DETECTION_METHODS } from "@qontinui/shared-types";

// ============================================================================
// Style constants
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

const STATUS_STYLES: Record<string, string> = {
  active: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  resolved: "bg-green-500/15 text-green-400 border-green-500/30",
  monitoring: "bg-blue-500/15 text-blue-400 border-blue-500/30",
  wont_fix: "bg-gray-500/15 text-gray-400 border-gray-500/30",
};

type StatusFilter = "all" | "active" | "resolved" | "monitoring";

// ============================================================================
// Badge
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

// ============================================================================
// Confidence bar
// ============================================================================

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
        <Badge
          text={issue.detection_method.replace(/_/g, " ")}
          style="bg-gray-500/15 text-gray-400 border-gray-500/30"
        />
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
                  <li key={i} className="text-foreground">
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
  initialDetectionMethod,
  initialDetectionConfig,
  initialCategory,
}: {
  onSubmit: (req: CreateKnownIssueRequest) => Promise<void>;
  onCancel: () => void;
  initialDetectionMethod?: DetectionMethod;
  initialDetectionConfig?: Record<string, unknown>;
  initialCategory?: IssueCategory;
}) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState<IssueCategory>(initialCategory || "other");
  const [severity, setSeverity] = useState<KnownIssueSeverity>("medium");
  const [detectionMethod, setDetectionMethod] = useState<DetectionMethod>(
    initialDetectionMethod || "ai_judgment",
  );
  const [scopeType, setScopeType] = useState<"global" | "spec" | "url" | "component" | "feature">(
    "global",
  );
  const [scopeValue, setScopeValue] = useState("");
  const [verificationHint, setVerificationHint] = useState("");
  const [reproductionContext, setReproductionContext] = useState("");
  const [detectionConfigStr, setDetectionConfigStr] = useState(
    initialDetectionConfig ? JSON.stringify(initialDetectionConfig, null, 2) : "",
  );
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!title.trim() || !description.trim()) return;
    setIsSubmitting(true);
    try {
      let detectionConfig: Record<string, unknown> | undefined;
      if (detectionConfigStr.trim()) {
        try {
          detectionConfig = JSON.parse(detectionConfigStr.trim());
        } catch {
          // ignore parse error, submit without config
        }
      }

      await onSubmit({
        title: title.trim(),
        description: description.trim(),
        category,
        severity,
        detection_method: detectionMethod,
        scope_type: scopeType,
        scope_value: scopeType !== "global" ? scopeValue.trim() || undefined : undefined,
        verification_hint: verificationHint.trim() || undefined,
        reproduction_context: reproductionContext.trim() || undefined,
        detection_config: detectionConfig,
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

      {/* Title */}
      <input
        type="text"
        placeholder="Issue title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
        autoFocus
      />

      {/* Description */}
      <textarea
        placeholder="Describe the issue -- what you observed, what is wrong"
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50 min-h-[80px] resize-y"
        rows={3}
      />

      {/* Row: Category + Severity + Scope */}
      <div className="grid grid-cols-3 gap-3">
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Category
          </label>
          <select
            value={category}
            onChange={(e) => setCategory(e.target.value as IssueCategory)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
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
            value={severity}
            onChange={(e) => setSeverity(e.target.value as KnownIssueSeverity)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
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
            value={scopeType}
            onChange={(e) =>
              setScopeType(e.target.value as "global" | "spec" | "url" | "component" | "feature")
            }
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          >
            <option value="global">Global</option>
            <option value="spec">Spec</option>
            <option value="url">URL</option>
            <option value="component">Component</option>
            <option value="feature">Feature</option>
          </select>
        </div>
      </div>

      {/* Scope value (only when not global) */}
      {scopeType !== "global" && (
        <input
          type="text"
          placeholder={`${scopeType} identifier`}
          value={scopeValue}
          onChange={(e) => setScopeValue(e.target.value)}
          className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
        />
      )}

      {/* Detection method */}
      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Detection Method
        </label>
        <select
          value={detectionMethod}
          onChange={(e) => setDetectionMethod(e.target.value as DetectionMethod)}
          className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
        >
          {DETECTION_METHODS.map((d) => (
            <option key={d.value} value={d.value}>
              {d.label}
            </option>
          ))}
        </select>
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
          className="w-full mt-0.5 px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
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
          className="w-full mt-0.5 px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
        />
      </div>

      {/* Detection config JSON */}
      {detectionConfigStr && (
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Detection Config (JSON)
          </label>
          <textarea
            value={detectionConfigStr}
            onChange={(e) => setDetectionConfigStr(e.target.value)}
            className="w-full mt-0.5 px-3 py-2 text-xs font-mono bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50 min-h-[60px] resize-y"
            rows={3}
          />
        </div>
      )}

      {/* Actions */}
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
// Template card
// ============================================================================

function TemplateCard({
  template,
  onUse,
}: {
  template: IssuePatternTemplate;
  onUse: (t: IssuePatternTemplate) => void;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="border border-border rounded-lg p-3 space-y-2 hover:border-border/80 transition-colors">
      <div className="flex items-start justify-between gap-2">
        <div className="flex-1 min-w-0">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex items-center gap-1.5 text-left group"
          >
            {expanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
            )}
            <FileCode className="w-3.5 h-3.5 text-blue-400 shrink-0" />
            <span className="text-sm font-medium group-hover:text-primary transition-colors">
              {template.name}
            </span>
          </button>
          <p className="text-xs text-muted-foreground line-clamp-2 ml-[52px] mt-0.5">
            {template.description}
          </p>
        </div>
        <button
          onClick={() => onUse(template)}
          className="px-2 py-1 text-xs rounded-md border border-border hover:bg-primary/10 hover:text-primary hover:border-primary/30 transition-colors shrink-0"
        >
          Use Template
        </button>
      </div>

      <div className="flex items-center gap-1.5 ml-[52px]">
        <Badge
          text={template.category.replace(/_/g, " ")}
          style={
            CATEGORY_STYLES[template.category] || "bg-gray-500/15 text-gray-400 border-gray-500/30"
          }
        />
        <Badge
          text={template.detection_type.replace(/_/g, " ")}
          style="bg-gray-500/15 text-gray-400 border-gray-500/30"
        />
        {template.built_in && (
          <Badge text="built-in" style="bg-blue-500/15 text-blue-400 border-blue-500/30" />
        )}
      </div>

      {expanded && (
        <div className="ml-[52px] mt-1 space-y-2 border-t border-border pt-2 text-xs">
          {template.parameters.length > 0 && (
            <div>
              <span className="text-muted-foreground font-medium">Parameters:</span>
              <div className="mt-1 space-y-1">
                {template.parameters.map((p) => (
                  <div key={p.name} className="flex items-baseline gap-2">
                    <code className="text-[11px] bg-white/5 px-1 rounded">{p.name}</code>
                    <span className="text-muted-foreground">({p.type})</span>
                    <span className="text-foreground">{p.description}</span>
                  </div>
                ))}
              </div>
            </div>
          )}
          {template.ai_prompt_template && (
            <div>
              <span className="text-muted-foreground font-medium">AI Prompt Template:</span>
              <pre className="mt-0.5 bg-white/5 p-2 rounded text-[11px] overflow-x-auto whitespace-pre-wrap">
                {template.ai_prompt_template}
              </pre>
            </div>
          )}
          {template.step_template && Object.keys(template.step_template).length > 0 && (
            <div>
              <span className="text-muted-foreground font-medium">Step Template:</span>
              <pre className="mt-0.5 bg-white/5 p-2 rounded text-[11px] overflow-x-auto">
                {JSON.stringify(template.step_template, null, 2)}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Create Template Form
// ============================================================================

const TEMPLATE_CATEGORIES = [
  { value: "duplication", label: "Duplication" },
  { value: "rendering", label: "Rendering" },
  { value: "data_integrity", label: "Data Integrity" },
  { value: "timing", label: "Timing" },
  { value: "layout", label: "Layout" },
  { value: "state", label: "State" },
  { value: "performance", label: "Performance" },
  { value: "encoding", label: "Encoding" },
  { value: "navigation", label: "Navigation" },
  { value: "authentication", label: "Authentication" },
  { value: "other", label: "Other" },
];

const TEMPLATE_DETECTION_TYPES = [
  { value: "algorithmic", label: "Algorithmic" },
  { value: "ai_judgment", label: "AI Judgment" },
  { value: "visual", label: "Visual" },
  { value: "command", label: "Command" },
  { value: "ui_bridge", label: "UI Bridge" },
];

function CreateTemplateForm({
  onSubmit,
  onCancel,
}: {
  onSubmit: (req: CreatePatternTemplateRequest) => Promise<void>;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState("other");
  const [detectionType, setDetectionType] = useState("ai_judgment");
  const [aiPromptTemplate, setAiPromptTemplate] = useState("");
  const [parametersStr, setParametersStr] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleSubmit = async () => {
    if (!name.trim() || !description.trim()) return;
    setIsSubmitting(true);
    try {
      // Validate parameters JSON if provided
      if (parametersStr.trim()) {
        try {
          JSON.parse(parametersStr.trim());
        } catch {
          // ignore parse error, submit without parameters
        }
      }

      await onSubmit({
        name: name.trim(),
        description: description.trim(),
        category,
        detection_type: detectionType,
        ai_prompt_template: aiPromptTemplate.trim() || undefined,
        parameters: parametersStr.trim() || undefined,
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
          New Pattern Template
        </span>
        <button onClick={onCancel} className="p-1 rounded hover:bg-muted text-muted-foreground">
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Name */}
      <input
        type="text"
        placeholder="Template name"
        value={name}
        onChange={(e) => setName(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
        autoFocus
      />

      {/* Description */}
      <textarea
        placeholder="Describe what this template detects"
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        className="w-full px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50 min-h-[80px] resize-y"
        rows={3}
      />

      {/* Row: Category + Detection Type */}
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Category
          </label>
          <select
            value={category}
            onChange={(e) => setCategory(e.target.value)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          >
            {TEMPLATE_CATEGORIES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
            Detection Type
          </label>
          <select
            value={detectionType}
            onChange={(e) => setDetectionType(e.target.value)}
            className="w-full mt-0.5 px-2 py-1.5 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          >
            {TEMPLATE_DETECTION_TYPES.map((d) => (
              <option key={d.value} value={d.value}>
                {d.label}
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* AI Prompt Template */}
      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          AI Prompt Template (optional)
        </label>
        <textarea
          placeholder="The prompt the AI uses to detect this pattern..."
          value={aiPromptTemplate}
          onChange={(e) => setAiPromptTemplate(e.target.value)}
          className="w-full mt-0.5 px-3 py-2 text-sm bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50 min-h-[80px] resize-y"
          rows={3}
        />
      </div>

      {/* Parameters JSON */}
      <div>
        <label className="text-[10px] text-muted-foreground uppercase tracking-wider">
          Parameters (optional, JSON array)
        </label>
        <textarea
          placeholder={
            '[\n  { "name": "threshold", "type": "number", "description": "Detection threshold" }\n]'
          }
          value={parametersStr}
          onChange={(e) => setParametersStr(e.target.value)}
          className="w-full mt-0.5 px-3 py-2 text-xs font-mono bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50 min-h-[60px] resize-y"
          rows={3}
        />
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-2 pt-1">
        <button
          onClick={onCancel}
          className="px-3 py-1.5 text-xs rounded-md border border-border hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={handleSubmit}
          disabled={!name.trim() || !description.trim() || isSubmitting}
          className="px-3 py-1.5 text-xs rounded-md bg-primary hover:bg-primary/90 text-primary-foreground disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          {isSubmitting ? "Creating..." : "Create Template"}
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// Horizontal bar helper
// ============================================================================

function HorizontalBar({
  label,
  count,
  total,
  color,
}: {
  label: string;
  count: number;
  total: number;
  color: string;
}) {
  const pct = total > 0 ? (count / total) * 100 : 0;
  return (
    <div className="flex items-center gap-2">
      <span className="text-[11px] text-muted-foreground w-20 shrink-0 text-right">{label}</span>
      <div className="flex-1 h-4 rounded bg-white/5 overflow-hidden">
        {pct > 0 && (
          <div
            className={`h-full rounded ${color} transition-all duration-300`}
            style={{ width: `${Math.max(pct, 2)}%` }}
          />
        )}
      </div>
      <span className="text-[11px] text-muted-foreground w-8 shrink-0">{count}</span>
    </div>
  );
}

// ============================================================================
// Analytics section
// ============================================================================

function AnalyticsSection({ issues }: { issues: KnownIssue[] }) {
  const [expanded, setExpanded] = useState(false);

  const analytics = useMemo(() => {
    const total = issues.length;
    const now = Date.now();
    const ONE_DAY = 24 * 60 * 60 * 1000;

    // Detection frequency buckets
    const detectionBuckets = { once: 0, few: 0, several: 0, many: 0 };
    for (const issue of issues) {
      const td = issue.times_detected;
      if (td <= 1) detectionBuckets.once++;
      else if (td <= 3) detectionBuckets.few++;
      else if (td <= 9) detectionBuckets.several++;
      else detectionBuckets.many++;
    }

    // Confidence distribution
    const confidenceBuckets = { low: 0, moderate: 0, high: 0, veryHigh: 0 };
    for (const issue of issues) {
      const c = issue.confidence;
      if (c < 0.25) confidenceBuckets.low++;
      else if (c < 0.5) confidenceBuckets.moderate++;
      else if (c < 0.75) confidenceBuckets.high++;
      else confidenceBuckets.veryHigh++;
    }

    // Age breakdown
    const ageBuckets = { day: 0, week: 0, month: 0, older: 0 };
    for (const issue of issues) {
      const age = now - new Date(issue.created_at).getTime();
      if (age <= ONE_DAY) ageBuckets.day++;
      else if (age <= 7 * ONE_DAY) ageBuckets.week++;
      else if (age <= 30 * ONE_DAY) ageBuckets.month++;
      else ageBuckets.older++;
    }

    // Provenance breakdown
    const provenanceCounts: Record<string, number> = {};
    for (const issue of issues) {
      const p = issue.provenance || "unknown";
      provenanceCounts[p] = (provenanceCounts[p] || 0) + 1;
    }

    return { total, detectionBuckets, confidenceBuckets, ageBuckets, provenanceCounts };
  }, [issues]);

  if (issues.length === 0) return null;

  return (
    <div className="border-b border-border px-6 py-3">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors"
      >
        {expanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        <BarChart2 className="w-4 h-4" />
        Analytics
      </button>

      {expanded && (
        <div className="mt-3 grid grid-cols-2 gap-6">
          {/* Detection trends */}
          <div className="space-y-1.5">
            <h3 className="text-[11px] uppercase tracking-wider text-muted-foreground font-medium mb-2">
              Detection Frequency
            </h3>
            <HorizontalBar
              label="1 time"
              count={analytics.detectionBuckets.once}
              total={analytics.total}
              color="bg-blue-500"
            />
            <HorizontalBar
              label="2-3 times"
              count={analytics.detectionBuckets.few}
              total={analytics.total}
              color="bg-amber-500"
            />
            <HorizontalBar
              label="4-9 times"
              count={analytics.detectionBuckets.several}
              total={analytics.total}
              color="bg-orange-500"
            />
            <HorizontalBar
              label="10+ times"
              count={analytics.detectionBuckets.many}
              total={analytics.total}
              color="bg-red-500"
            />
          </div>

          {/* Confidence distribution */}
          <div className="space-y-1.5">
            <h3 className="text-[11px] uppercase tracking-wider text-muted-foreground font-medium mb-2">
              Confidence Distribution
            </h3>
            <HorizontalBar
              label="Low"
              count={analytics.confidenceBuckets.low}
              total={analytics.total}
              color="bg-red-500"
            />
            <HorizontalBar
              label="Moderate"
              count={analytics.confidenceBuckets.moderate}
              total={analytics.total}
              color="bg-amber-500"
            />
            <HorizontalBar
              label="High"
              count={analytics.confidenceBuckets.high}
              total={analytics.total}
              color="bg-blue-500"
            />
            <HorizontalBar
              label="Very High"
              count={analytics.confidenceBuckets.veryHigh}
              total={analytics.total}
              color="bg-green-500"
            />
          </div>

          {/* Age breakdown */}
          <div className="space-y-1.5">
            <h3 className="text-[11px] uppercase tracking-wider text-muted-foreground font-medium mb-2">
              Age Breakdown
            </h3>
            <HorizontalBar
              label="Last 24h"
              count={analytics.ageBuckets.day}
              total={analytics.total}
              color="bg-green-500"
            />
            <HorizontalBar
              label="Last 7d"
              count={analytics.ageBuckets.week}
              total={analytics.total}
              color="bg-blue-500"
            />
            <HorizontalBar
              label="Last 30d"
              count={analytics.ageBuckets.month}
              total={analytics.total}
              color="bg-amber-500"
            />
            <HorizontalBar
              label="Older"
              count={analytics.ageBuckets.older}
              total={analytics.total}
              color="bg-gray-500"
            />
          </div>

          {/* Provenance breakdown */}
          <div className="space-y-1.5">
            <h3 className="text-[11px] uppercase tracking-wider text-muted-foreground font-medium mb-2">
              Provenance
            </h3>
            {Object.entries(analytics.provenanceCounts)
              .sort(([, a], [, b]) => b - a)
              .map(([provenance, count]) => {
                const colorMap: Record<string, string> = {
                  manual: "bg-blue-500",
                  auto_detected: "bg-amber-500",
                  reflection: "bg-purple-500",
                  imported: "bg-cyan-500",
                };
                return (
                  <HorizontalBar
                    key={provenance}
                    label={provenance.replace(/_/g, " ")}
                    count={count}
                    total={analytics.total}
                    color={colorMap[provenance] || "bg-gray-500"}
                  />
                );
              })}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main page
// ============================================================================

export function KnownIssuesPage() {
  // Data
  const [issues, setIssues] = useState<KnownIssue[]>([]);
  const [templates, setTemplates] = useState<IssuePatternTemplate[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Filters
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("active");
  const [categoryFilter, setCategoryFilter] = useState<string>("all");
  const [severityFilter, setSeverityFilter] = useState<string>("all");
  const [searchQuery, setSearchQuery] = useState("");

  // UI state
  const [showForm, setShowForm] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [showTemplates, setShowTemplates] = useState(false);
  const [formInitialDetectionMethod, setFormInitialDetectionMethod] = useState<
    DetectionMethod | undefined
  >();
  const [formInitialDetectionConfig, setFormInitialDetectionConfig] = useState<
    Record<string, unknown> | undefined
  >();
  const [formInitialCategory, setFormInitialCategory] = useState<IssueCategory | undefined>();
  const [showTemplateForm, setShowTemplateForm] = useState(false);
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

  const loadTemplates = useCallback(async () => {
    try {
      const result = await invoke<IssuePatternTemplate[]>("list_pattern_templates");
      setTemplates(result);
    } catch {
      // non-critical
    }
  }, []);

  useEffect(() => {
    loadIssues();
  }, [loadIssues]);

  useEffect(() => {
    if (showTemplates && templates.length === 0) {
      loadTemplates();
    }
  }, [showTemplates, templates.length, loadTemplates]);

  // Listen for auto-detected known issues from the loop controller
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
        // Auto-dismiss after 8 seconds
        setTimeout(() => setAutoDetectedToast(null), 8000);
        // Refresh the issues list
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
    const all = issues;
    return {
      total: all.length,
      critical: all.filter((i) => i.severity === "critical" && i.status === "active").length,
      high: all.filter((i) => i.severity === "high" && i.status === "active").length,
      medium: all.filter((i) => i.severity === "medium" && i.status === "active").length,
      low: all.filter((i) => i.severity === "low" && i.status === "active").length,
    };
  }, [issues]);

  // ------ Actions ------

  const handleCreate = useCallback(
    async (req: CreateKnownIssueRequest) => {
      try {
        await invoke("create_known_issue", { request: req });
        setShowForm(false);
        setFormInitialDetectionMethod(undefined);
        setFormInitialDetectionConfig(undefined);
        setFormInitialCategory(undefined);
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
        await invoke("resolve_known_issue", {
          id,
          resolution: "Resolved from Known Issues page",
        });
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
        await invoke("resolve_known_issue", {
          id,
          resolution: "Bulk resolved from Known Issues page",
        });
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

  const handleUseTemplate = useCallback((template: IssuePatternTemplate) => {
    setFormInitialDetectionMethod(template.detection_type as DetectionMethod);
    setFormInitialDetectionConfig(template.step_template || undefined);
    setFormInitialCategory(
      (ISSUE_CATEGORIES.find((c) => c.value === template.category)
        ? template.category
        : "other") as IssueCategory,
    );
    setShowForm(true);
    // Scroll to top
    window.scrollTo({ top: 0, behavior: "smooth" });
  }, []);

  const handleCreateTemplate = useCallback(
    async (req: CreatePatternTemplateRequest) => {
      try {
        await invoke("create_pattern_template", { request: req });
        setShowTemplateForm(false);
        await loadTemplates();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [loadTemplates],
  );

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
        // Reset file input so the same file can be re-selected
        if (fileInputRef.current) {
          fileInputRef.current.value = "";
        }
      }
    },
    [loadIssues],
  );

  // ------ Pill button helper ------

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
      <div className="shrink-0 border-b border-border px-6 py-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Bug className="w-5 h-5 text-amber-400" />
            <h1 className="text-lg font-semibold">Known Issues</h1>
            <div className="flex items-center gap-2 ml-2">
              <span className="text-xs px-2 py-0.5 rounded-full bg-white/10 text-muted-foreground">
                {stats.total} total
              </span>
              {stats.critical > 0 && (
                <span className="text-xs px-2 py-0.5 rounded-full bg-red-500/15 text-red-400">
                  {stats.critical} critical
                </span>
              )}
              {stats.high > 0 && (
                <span className="text-xs px-2 py-0.5 rounded-full bg-orange-500/15 text-orange-400">
                  {stats.high} high
                </span>
              )}
              {stats.medium > 0 && (
                <span className="text-xs px-2 py-0.5 rounded-full bg-amber-500/15 text-amber-400">
                  {stats.medium} medium
                </span>
              )}
              {stats.low > 0 && (
                <span className="text-xs px-2 py-0.5 rounded-full bg-blue-500/15 text-blue-400">
                  {stats.low} low
                </span>
              )}
            </div>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleExport}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md border border-border hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Export issues as JSON"
            >
              <Download className="w-4 h-4" />
              Export
            </button>
            <button
              onClick={() => fileInputRef.current?.click()}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md border border-border hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Import issues from JSON"
            >
              <Upload className="w-4 h-4" />
              Import
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
                onClick={() => {
                  setFormInitialDetectionMethod(undefined);
                  setFormInitialDetectionConfig(undefined);
                  setFormInitialCategory(undefined);
                  setShowForm(true);
                }}
                className="flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
              >
                <Plus className="w-4 h-4" />
                New Issue
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Analytics */}
      <AnalyticsSection issues={issues} />

      {/* Filters bar */}
      <div className="shrink-0 border-b border-border px-6 py-3">
        <div className="flex items-center gap-4 flex-wrap">
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

          {/* Category select */}
          <select
            value={categoryFilter}
            onChange={(e) => setCategoryFilter(e.target.value)}
            className="px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          >
            <option value="all">All Categories</option>
            {ISSUE_CATEGORIES.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>

          {/* Severity select */}
          <select
            value={severityFilter}
            onChange={(e) => setSeverityFilter(e.target.value)}
            className="px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          >
            <option value="all">All Severities</option>
            {ISSUE_SEVERITIES.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>

          {/* Search */}
          <div className="flex items-center gap-1 flex-1 min-w-[180px] max-w-[320px]">
            <Search className="w-3.5 h-3.5 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search issues..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="flex-1 px-2 py-1 text-xs bg-background border border-border rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
            />
          </div>

          {/* Bulk actions */}
          {selectedIds.size > 0 && (
            <div className="flex items-center gap-2 ml-auto">
              <span className="text-xs text-muted-foreground">{selectedIds.size} selected</span>
              <button
                onClick={handleBulkResolve}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-md border border-green-500/30 text-green-400 hover:bg-green-500/10 transition-colors"
              >
                <CheckCircle2 className="w-3 h-3" />
                Resolve Selected
              </button>
              <button
                onClick={handleBulkDelete}
                className="flex items-center gap-1 px-2 py-1 text-xs rounded-md border border-red-500/30 text-red-400 hover:bg-red-500/10 transition-colors"
              >
                <Trash2 className="w-3 h-3" />
                Delete Selected
              </button>
            </div>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-6 py-4 space-y-4">
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

        {/* Auto-detected known issues toast */}
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
          <CreateIssueForm
            onSubmit={handleCreate}
            onCancel={() => {
              setShowForm(false);
              setFormInitialDetectionMethod(undefined);
              setFormInitialDetectionConfig(undefined);
              setFormInitialCategory(undefined);
            }}
            initialDetectionMethod={formInitialDetectionMethod}
            initialDetectionConfig={formInitialDetectionConfig}
            initialCategory={formInitialCategory}
          />
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

        {/* Pattern Templates section */}
        <div className="border-t border-border pt-4 mt-6">
          <div className="flex items-center justify-between">
            <button
              onClick={() => setShowTemplates(!showTemplates)}
              className="flex items-center gap-2 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors"
            >
              {showTemplates ? (
                <ChevronDown className="w-4 h-4" />
              ) : (
                <ChevronRight className="w-4 h-4" />
              )}
              <Layers className="w-4 h-4" />
              Pattern Templates
              {templates.length > 0 && (
                <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-white/10">
                  {templates.length}
                </span>
              )}
            </button>
            {showTemplates && !showTemplateForm && (
              <button
                onClick={() => setShowTemplateForm(true)}
                className="flex items-center gap-1.5 px-2.5 py-1 text-xs rounded-md border border-border hover:bg-primary/10 hover:text-primary hover:border-primary/30 transition-colors"
              >
                <Plus className="w-3.5 h-3.5" />
                Create Template
              </button>
            )}
          </div>

          {showTemplates && (
            <div className="mt-3 space-y-2">
              {showTemplateForm && (
                <CreateTemplateForm
                  onSubmit={handleCreateTemplate}
                  onCancel={() => setShowTemplateForm(false)}
                />
              )}
              {templates.length === 0 && !showTemplateForm && (
                <p className="text-xs text-muted-foreground py-4 text-center">
                  No pattern templates available.
                </p>
              )}
              {templates.map((t) => (
                <TemplateCard key={t.id} template={t} onUse={handleUseTemplate} />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
