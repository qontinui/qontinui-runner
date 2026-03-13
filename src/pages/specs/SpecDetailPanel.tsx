/**
 * SpecDetailPanel — Shows details of the selected spec or group
 */

import { useState } from "react";
import {
  Shield,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  Info,
  Eye,
  EyeOff,
  ExternalLink,
  Tag,
  Server,
  Globe,
  Folder,
  Layers,
  Lock,
  ArrowRight,
  Database,
  Link2,
  GitBranch,
  Gauge,
  Monitor,
  Smartphone,
  Zap,
  Table2,
  KeyRound,
  Trash2,
  Plus,
  MousePointer2,
  Type,
  Clock,
  Navigation,
  ChevronDown,
  ChevronRight,
  Bug,
} from "lucide-react";
import type { LoadedSpec } from "./types";
import { SpecIssuesPanel } from "./SpecIssuesPanel";
import { GlobalIssuesPanel } from "./GlobalIssuesPanel";
import type {
  SpecGroup,
  SpecAssertion,
  SetupAction,
  ArchitectureConfig,
  TechStackEntry,
  ArchitectureConstraint,
  FeatureSpec,
  ApiConfig,
  ApiEndpoint,
  DataModel,
  SchemaField,
  DataConfig,
  DataEntity,
  DataColumn,
  DataRelation,
  DependencyConfig,
  DependencyLink,
  ModuleRef,
  ConstraintConfig,
  PerformanceBudget,
  BundleBudget,
  BrowserTarget,
  ResponsiveBreakpoint,
} from "ui-bridge";

// ============================================================================
// Shared badge components
// ============================================================================

const SEVERITY_TOOLTIPS: Record<string, string> = {
  critical: "Must-pass. Failure means the page is fundamentally broken.",
  warning: "Should-pass. Failure indicates a degraded experience.",
  info: "Nice-to-have. Informational check, low impact if it fails.",
};

function SeverityBadge({ severity }: { severity: string }) {
  const styles: Record<string, string> = {
    critical: "bg-red-500/15 text-red-400 border-red-500/30",
    warning: "bg-amber-500/15 text-amber-400 border-amber-500/30",
    info: "bg-blue-500/15 text-blue-400 border-blue-500/30",
  };
  const icons: Record<string, typeof AlertTriangle> = {
    critical: XCircle,
    warning: AlertTriangle,
    info: Info,
  };
  const Icon = icons[severity] || Info;

  return (
    <span
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 text-[10px] font-medium rounded border ${styles[severity] || styles.info}`}
      title={SEVERITY_TOOLTIPS[severity] || `Severity: ${severity}`}
    >
      <Icon className="w-2.5 h-2.5" />
      {severity}
    </span>
  );
}

const CATEGORY_TOOLTIPS: Record<string, string> = {
  "element-presence": "Core UI elements exist on the page.",
  semantic: "Semantic correctness — roles, labels, and structure.",
  accessibility: "Accessibility (a11y) compliance checks.",
  "form-validation": "Form behavior and validation rules.",
  "state-consistency": "UI state is coherent across interactions.",
  navigation: "Page navigation and routing checks.",
  "cross-page-consistency": "Consistency across multiple pages.",
  "modal-dialog": "Modal and dialog behavior.",
  design: "Visual design compliance.",
  layout: "Layout and spatial relationships between elements.",
  custom: "Custom assertion category.",
};

function CategoryBadge({ category }: { category: string }) {
  return (
    <span
      className="inline-flex items-center gap-1 px-1.5 py-0.5 text-[10px] font-medium rounded bg-white/5 text-muted-foreground border border-white/10"
      title={CATEGORY_TOOLTIPS[category] || `Category: ${category}`}
    >
      <Tag className="w-2.5 h-2.5" />
      {category}
    </span>
  );
}

function StatCard({
  label,
  value,
  color,
}: {
  label: string;
  value: string | number;
  color?: string;
}) {
  return (
    <div className="rounded border border-white/5 bg-white/[0.02] px-3 py-2">
      <div className={`text-sm font-semibold tabular-nums ${color || "text-foreground"}`}>
        {value}
      </div>
      <div className="text-[10px] text-muted-foreground">{label}</div>
    </div>
  );
}

const ASSERTION_TYPE_TOOLTIPS: Record<string, string> = {
  exists: "Element is present in the DOM.",
  notExists: "Element must NOT be in the DOM.",
  visible: "Element is visible on screen.",
  hidden: "Element is not visible.",
  enabled: "Element is interactive (not disabled).",
  disabled: "Element is non-interactive (disabled).",
  focused: "Element has keyboard focus.",
  checked: "Checkbox or radio is checked.",
  unchecked: "Checkbox or radio is unchecked.",
  hasText: "Element has exact text match.",
  containsText: "Element contains partial text match.",
  hasValue: "Form input has a specific value.",
  hasClass: "Element has a specific CSS class.",
  count: "Number of matching elements equals expected.",
  attribute: "Element has a specific HTML attribute value.",
  cssProperty: "Element has a specific CSS property value.",
  cssPropertyInSet: "CSS property value is in an allowed set.",
  cssPropertyRange: "CSS property value is within a numeric range.",
  tokenCompliance: "CSS property matches a design token.",
  noOverlap: "Two elements do not visually overlap (bounding box check).",
  minSpacing: "Minimum pixel gap between two elements.",
};

const SOURCE_TOOLTIPS: Record<string, string> = {
  manual: "Hand-written by a developer.",
  auto: "Auto-generated by scanning the page.",
  "ai-generated": "Created by AI during spec generation.",
};

const SPEC_LOAD_SOURCE_TOOLTIPS: Record<string, string> = {
  bundled: "Loaded from bundled spec files shipped with the app.",
  discovered: "Discovered from a connected app via UI Bridge.",
  file: "Loaded from a local spec file.",
};

// ============================================================================
// Page Spec Components
// ============================================================================

function AssertionRow({
  assertion,
  editMode,
  onToggle,
  onRemove,
}: {
  assertion: SpecAssertion;
  editMode?: boolean;
  onToggle?: () => void;
  onRemove?: () => void;
}) {
  const targetLabel =
    assertion.target?.type === "search"
      ? (assertion.target as { criteria?: { textContent?: string; role?: string } }).criteria
          ?.textContent ||
        (assertion.target as { criteria?: { role?: string } }).criteria?.role ||
        "search"
      : assertion.target?.type === "elementId"
        ? (assertion.target as { elementId?: string }).elementId || "elementId"
        : "";

  return (
    <div
      className={`flex items-start gap-3 px-3 py-2 rounded border transition-colors
        ${assertion.enabled ? "border-white/5 bg-white/[0.02] hover:bg-white/[0.04]" : "border-transparent opacity-40"}`}
    >
      <button
        className="shrink-0 mt-0.5"
        onClick={editMode ? onToggle : undefined}
        title={editMode ? "Toggle enabled" : undefined}
      >
        {assertion.enabled ? (
          <Eye
            className={`w-3.5 h-3.5 text-green-400/60 ${editMode ? "hover:text-green-400 cursor-pointer" : ""}`}
          />
        ) : (
          <EyeOff
            className={`w-3.5 h-3.5 text-muted-foreground/40 ${editMode ? "hover:text-muted-foreground cursor-pointer" : ""}`}
          />
        )}
      </button>

      <div className="flex-1 min-w-0">
        <p className="text-xs text-foreground leading-relaxed">{assertion.description}</p>

        <div className="flex items-center gap-2 mt-1.5 flex-wrap">
          <SeverityBadge severity={assertion.severity} />

          <span
            className="text-[10px] font-mono text-muted-foreground bg-white/5 px-1.5 py-0.5 rounded"
            title={
              ASSERTION_TYPE_TOOLTIPS[assertion.assertionType || "exists"] ||
              `Assertion type: ${assertion.assertionType}`
            }
          >
            {assertion.assertionType || "exists"}
          </span>

          {targetLabel && (
            <span
              className="text-[10px] text-muted-foreground truncate max-w-[200px]"
              title={`Target element: ${targetLabel}`}
            >
              {targetLabel}
            </span>
          )}

          {assertion.relatedTarget && (
            <span className="text-[10px] text-cyan-400/70">
              ↔{" "}
              {assertion.relatedTarget.type === "search"
                ? assertion.relatedTarget.criteria?.textContent ||
                  assertion.relatedTarget.criteria?.role ||
                  "related"
                : assertion.relatedTarget.elementId || "related"}
            </span>
          )}

          {assertion.minGap !== undefined && (
            <span className="text-[10px] text-muted-foreground font-mono">
              gap≥{assertion.minGap}px
            </span>
          )}

          {assertion.precondition && (
            <span
              className="text-[10px] text-muted-foreground italic truncate max-w-[200px]"
              title={`Precondition: This assertion only runs when "${assertion.precondition}"`}
            >
              when: {assertion.precondition}
            </span>
          )}
        </div>
      </div>

      {editMode && onRemove && (
        <button
          onClick={onRemove}
          className="shrink-0 mt-0.5 text-red-400/50 hover:text-red-400 transition-colors"
          title="Remove assertion"
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      )}

      {!editMode && assertion.source && (
        <span
          className="text-[10px] text-muted-foreground/50 shrink-0"
          title={SOURCE_TOOLTIPS[assertion.source] || `Source: ${assertion.source}`}
        >
          {assertion.source}
        </span>
      )}
    </div>
  );
}

// ============================================================================
// Add Assertion Form (inline)
// ============================================================================

function AddAssertionForm({
  onAdd,
  onCancel,
}: {
  onAdd: (assertion: SpecAssertion) => void;
  onCancel: () => void;
}) {
  const [description, setDescription] = useState("");
  const [severity, setSeverity] = useState<"critical" | "warning" | "info">("warning");
  const [assertionType, setAssertionType] = useState("exists");
  const [category, setCategory] = useState("custom");
  const [targetText, setTargetText] = useState("");
  const [relatedTargetText, setRelatedTargetText] = useState("");
  const [minGap, setMinGap] = useState(0);

  const isSpatial = assertionType === "noOverlap" || assertionType === "minSpacing";

  const handleSubmit = () => {
    if (!description.trim()) return;
    const assertion: Record<string, unknown> = {
      id: crypto.randomUUID(),
      description: description.trim(),
      severity,
      assertionType,
      category,
      enabled: true,
      reviewed: false,
      source: "manual",
      target: {
        type: "search",
        criteria: targetText.trim() ? { textContent: targetText.trim() } : {},
      },
    };
    if (isSpatial && relatedTargetText.trim()) {
      assertion.relatedTarget = {
        type: "search",
        criteria: { textContent: relatedTargetText.trim() },
      };
    }
    if (assertionType === "minSpacing") {
      assertion.minGap = minGap;
    }
    onAdd(assertion as unknown as SpecAssertion);
    setDescription("");
    setTargetText("");
    setRelatedTargetText("");
  };

  return (
    <div className="px-3 py-2 rounded border border-dashed border-green-500/30 bg-green-500/5 space-y-2">
      <textarea
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        placeholder="Assertion description..."
        rows={2}
        className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1.5
          text-foreground placeholder:text-muted-foreground/40 resize-none
          focus:outline-none focus:border-green-500/50"
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            handleSubmit();
          }
        }}
      />
      <div className="flex items-center gap-2 flex-wrap">
        <select
          value={assertionType}
          onChange={(e) => setAssertionType(e.target.value)}
          className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground"
        >
          <optgroup label="Existence">
            <option value="exists">exists</option>
            <option value="notExists">notExists</option>
            <option value="visible">visible</option>
            <option value="hidden">hidden</option>
          </optgroup>
          <optgroup label="State">
            <option value="enabled">enabled</option>
            <option value="disabled">disabled</option>
            <option value="focused">focused</option>
            <option value="checked">checked</option>
            <option value="unchecked">unchecked</option>
          </optgroup>
          <optgroup label="Content">
            <option value="hasText">hasText</option>
            <option value="containsText">containsText</option>
            <option value="hasValue">hasValue</option>
            <option value="count">count</option>
          </optgroup>
          <optgroup label="Style">
            <option value="cssProperty">cssProperty</option>
            <option value="cssPropertyInSet">cssPropertyInSet</option>
            <option value="cssPropertyRange">cssPropertyRange</option>
            <option value="tokenCompliance">tokenCompliance</option>
          </optgroup>
          <optgroup label="Layout">
            <option value="noOverlap">noOverlap</option>
            <option value="minSpacing">minSpacing</option>
          </optgroup>
          <optgroup label="Other">
            <option value="attribute">attribute</option>
            <option value="hasClass">hasClass</option>
          </optgroup>
        </select>
        <select
          value={severity}
          onChange={(e) => setSeverity(e.target.value as "critical" | "warning" | "info")}
          className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground"
        >
          <option value="critical">critical</option>
          <option value="warning">warning</option>
          <option value="info">info</option>
        </select>
        <select
          value={category}
          onChange={(e) => setCategory(e.target.value)}
          className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground"
        >
          <option value="element-presence">element-presence</option>
          <option value="layout">layout</option>
          <option value="design">design</option>
          <option value="accessibility">accessibility</option>
          <option value="form-validation">form-validation</option>
          <option value="state-consistency">state-consistency</option>
          <option value="navigation">navigation</option>
          <option value="semantic">semantic</option>
          <option value="custom">custom</option>
        </select>
      </div>
      <input
        type="text"
        value={targetText}
        onChange={(e) => setTargetText(e.target.value)}
        placeholder="Target element text..."
        className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
          text-foreground placeholder:text-muted-foreground/40
          focus:outline-none focus:border-green-500/50"
      />
      {isSpatial && (
        <input
          type="text"
          value={relatedTargetText}
          onChange={(e) => setRelatedTargetText(e.target.value)}
          placeholder="Related target element text..."
          className="w-full text-xs bg-transparent border border-cyan-500/20 rounded px-2 py-1
            text-foreground placeholder:text-muted-foreground/40
            focus:outline-none focus:border-cyan-500/50"
        />
      )}
      {assertionType === "minSpacing" && (
        <div className="flex items-center gap-2">
          <label className="text-[10px] text-muted-foreground">Min gap (px):</label>
          <input
            type="number"
            value={minGap}
            onChange={(e) => setMinGap(Number(e.target.value))}
            min={0}
            className="w-20 text-xs bg-transparent border border-white/10 rounded px-2 py-0.5
              text-foreground focus:outline-none focus:border-green-500/50"
          />
        </div>
      )}
      <div className="flex items-center gap-2">
        <div className="flex-1" />
        <button
          onClick={onCancel}
          className="text-[10px] text-muted-foreground hover:text-foreground transition-colors px-2 py-0.5"
        >
          Cancel
        </button>
        <button
          onClick={handleSubmit}
          disabled={!description.trim()}
          className="text-[10px] px-2 py-0.5 rounded bg-green-500/10 text-green-400 border border-green-500/20
            hover:bg-green-500/20 disabled:opacity-40 transition-colors"
        >
          Add
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// Setup Actions Editor
// ============================================================================

const SETUP_ACTION_LABELS: Record<
  string,
  { icon: typeof MousePointer2; label: string; color: string }
> = {
  click: { icon: MousePointer2, label: "Click", color: "text-blue-400" },
  type: { icon: Type, label: "Type", color: "text-green-400" },
  navigate: { icon: Navigation, label: "Navigate", color: "text-purple-400" },
  waitForElement: { icon: Clock, label: "Wait for element", color: "text-amber-400" },
  wait: { icon: Clock, label: "Wait", color: "text-muted-foreground" },
};

function SetupActionsEditor({
  actions,
  editMode,
  onChange,
}: {
  actions: SetupAction[];
  editMode?: boolean;
  onChange?: (actions: SetupAction[]) => void;
}) {
  const [expanded, setExpanded] = useState(actions.length > 0);
  const [showAddForm, setShowAddForm] = useState(false);
  const [newType, setNewType] = useState<SetupAction["type"]>("click");
  const [newTargetText, setNewTargetText] = useState("");
  const [newValue, setNewValue] = useState("");
  const [newUrl, setNewUrl] = useState("");
  const [newMs, setNewMs] = useState(1000);

  const handleAdd = () => {
    let action: SetupAction;
    switch (newType) {
      case "click":
        action = {
          type: "click",
          target: { type: "search", criteria: { textContent: newTargetText.trim() } },
        };
        break;
      case "type":
        action = {
          type: "type",
          target: { type: "search", criteria: { textContent: newTargetText.trim() } },
          value: newValue,
        };
        break;
      case "navigate":
        action = { type: "navigate", url: newUrl };
        break;
      case "waitForElement":
        action = {
          type: "waitForElement",
          target: { type: "search", criteria: { textContent: newTargetText.trim() } },
          timeout: newMs,
        };
        break;
      case "wait":
        action = { type: "wait", ms: newMs };
        break;
    }
    onChange?.([...actions, action]);
    setNewTargetText("");
    setNewValue("");
    setNewUrl("");
    setShowAddForm(false);
  };

  const handleRemove = (index: number) => {
    onChange?.(actions.filter((_, i) => i !== index));
  };

  const needsTarget = newType === "click" || newType === "type" || newType === "waitForElement";

  return (
    <div className="space-y-1">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground transition-colors"
      >
        {expanded ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        Setup Actions
        {actions.length > 0 && (
          <span className="text-cyan-400/70 font-normal normal-case">({actions.length})</span>
        )}
      </button>

      {expanded && (
        <div className="space-y-1 ml-1">
          {actions.map((action, i) => {
            const config = SETUP_ACTION_LABELS[action.type] || SETUP_ACTION_LABELS.wait;
            const Icon = config.icon;
            const detail =
              action.type === "navigate"
                ? action.url
                : action.type === "wait"
                  ? `${action.ms}ms`
                  : action.type === "type"
                    ? `"${action.value}" → ${action.target.type === "search" ? action.target.criteria?.textContent || "?" : action.target.elementId}`
                    : "target" in action && action.target.type === "search"
                      ? action.target.criteria?.textContent || ""
                      : "";

            return (
              <div
                key={i}
                className="flex items-center gap-2 px-2 py-1 rounded border border-white/5 bg-white/[0.02] text-xs"
              >
                <span className="text-[10px] text-muted-foreground/40 w-4 text-right">{i + 1}</span>
                <Icon className={`w-3 h-3 shrink-0 ${config.color}`} />
                <span className={`font-medium ${config.color}`}>{config.label}</span>
                <span className="text-muted-foreground truncate flex-1">{detail}</span>
                {editMode && (
                  <button
                    onClick={() => handleRemove(i)}
                    className="text-red-400/50 hover:text-red-400 transition-colors shrink-0"
                  >
                    <Trash2 className="w-3 h-3" />
                  </button>
                )}
              </div>
            );
          })}

          {editMode && !showAddForm && (
            <button
              onClick={() => setShowAddForm(true)}
              className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 text-[10px] text-muted-foreground/60
                rounded border border-dashed border-white/10 hover:border-cyan-500/30 hover:text-cyan-400
                transition-colors"
            >
              <Plus className="w-2.5 h-2.5" />
              Add Setup Action
            </button>
          )}

          {editMode && showAddForm && (
            <div className="px-2 py-2 rounded border border-dashed border-cyan-500/30 bg-cyan-500/5 space-y-2">
              <select
                value={newType}
                onChange={(e) => setNewType(e.target.value as SetupAction["type"])}
                className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground w-full"
              >
                <option value="click">Click element</option>
                <option value="type">Type text</option>
                <option value="navigate">Navigate to URL</option>
                <option value="waitForElement">Wait for element</option>
                <option value="wait">Wait (delay)</option>
              </select>

              {needsTarget && (
                <input
                  type="text"
                  value={newTargetText}
                  onChange={(e) => setNewTargetText(e.target.value)}
                  placeholder="Target element text..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-none focus:border-cyan-500/50"
                />
              )}

              {newType === "type" && (
                <input
                  type="text"
                  value={newValue}
                  onChange={(e) => setNewValue(e.target.value)}
                  placeholder="Text to type..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-none focus:border-green-500/50"
                />
              )}

              {newType === "navigate" && (
                <input
                  type="text"
                  value={newUrl}
                  onChange={(e) => setNewUrl(e.target.value)}
                  placeholder="http://localhost:3001/..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-none focus:border-purple-500/50"
                />
              )}

              {(newType === "wait" || newType === "waitForElement") && (
                <div className="flex items-center gap-2">
                  <label className="text-[10px] text-muted-foreground">
                    {newType === "wait" ? "Duration" : "Timeout"} (ms):
                  </label>
                  <input
                    type="number"
                    value={newMs}
                    onChange={(e) => setNewMs(Number(e.target.value))}
                    min={0}
                    className="w-20 text-xs bg-transparent border border-white/10 rounded px-2 py-0.5
                      text-foreground focus:outline-none focus:border-cyan-500/50"
                  />
                </div>
              )}

              <div className="flex items-center gap-2">
                <div className="flex-1" />
                <button
                  onClick={() => setShowAddForm(false)}
                  className="text-[10px] text-muted-foreground hover:text-foreground transition-colors px-2 py-0.5"
                >
                  Cancel
                </button>
                <button
                  onClick={handleAdd}
                  className="text-[10px] px-2 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
                    hover:bg-cyan-500/20 transition-colors"
                >
                  Add
                </button>
              </div>
            </div>
          )}

          {actions.length === 0 && !showAddForm && !editMode && (
            <p className="text-[10px] text-muted-foreground/40 italic px-2">No setup actions</p>
          )}
        </div>
      )}
    </div>
  );
}

function GroupDetail({
  group,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  editMode,
  onToggleAssertion,
  onRemoveAssertion,
  onAddAssertion,
  onRemoveGroup,
  onUpdateSetupActions,
}: {
  group: SpecGroup;
  specId?: string;
  editMode?: boolean;
  onToggleAssertion?: (assertionId: string) => void;
  onRemoveAssertion?: (assertionId: string) => void;
  onAddAssertion?: (assertion: SpecAssertion) => void;
  onRemoveGroup?: () => void;
  onUpdateSetupActions?: (actions: SetupAction[]) => void;
}) {
  const [showAddForm, setShowAddForm] = useState(false);
  const enabled = group.assertions.filter((a) => a.enabled);
  const disabled = group.assertions.filter((a) => !a.enabled);

  return (
    <div className="space-y-4">
      <div>
        <div className="flex items-center gap-2">
          <h2 className="text-sm font-semibold text-foreground">{group.name}</h2>
          {editMode && onRemoveGroup && (
            <button
              onClick={onRemoveGroup}
              className="text-red-400/50 hover:text-red-400 transition-colors"
              title="Remove group"
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
          )}
        </div>
        {group.description && (
          <p className="text-xs text-muted-foreground mt-1 leading-relaxed">{group.description}</p>
        )}
        <div className="flex items-center gap-2 mt-2">
          <CategoryBadge category={group.category} />
          <span className="text-[10px] text-muted-foreground">
            {enabled.length} enabled
            {disabled.length > 0 && `, ${disabled.length} disabled`}
          </span>
          {group.source && (
            <span
              className="text-[10px] text-muted-foreground/50"
              title={SOURCE_TOOLTIPS[group.source] || `Source: ${group.source}`}
            >
              source: {group.source}
            </span>
          )}
        </div>
      </div>

      <SetupActionsEditor
        actions={group.setupActions || []}
        editMode={editMode}
        onChange={onUpdateSetupActions}
      />

      <div className="space-y-1">
        {group.assertions.map((assertion) => (
          <AssertionRow
            key={assertion.id}
            assertion={assertion}
            editMode={editMode}
            onToggle={onToggleAssertion ? () => onToggleAssertion(assertion.id) : undefined}
            onRemove={onRemoveAssertion ? () => onRemoveAssertion(assertion.id) : undefined}
          />
        ))}
      </div>

      {/* Add assertion */}
      {editMode && (
        <>
          {showAddForm ? (
            <AddAssertionForm
              onAdd={(assertion) => {
                onAddAssertion?.(assertion);
                setShowAddForm(false);
              }}
              onCancel={() => setShowAddForm(false)}
            />
          ) : (
            <button
              onClick={() => setShowAddForm(true)}
              className="w-full flex items-center justify-center gap-1.5 px-3 py-2 text-xs text-muted-foreground/60
                rounded border border-dashed border-white/10 hover:border-green-500/30 hover:text-green-400
                transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add Assertion
            </button>
          )}
        </>
      )}
    </div>
  );
}

// ============================================================================
// Add Group Form (inline)
// ============================================================================

function AddGroupForm({
  onAdd,
  onCancel,
}: {
  onAdd: (group: SpecGroup) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [category, setCategory] = useState("custom");
  const [description, setDescription] = useState("");

  const handleSubmit = () => {
    if (!name.trim()) return;
    onAdd({
      id: crypto.randomUUID(),
      name: name.trim(),
      description: description.trim(),
      category: category as SpecGroup["category"],
      source: "manual",
      assertions: [],
    } as SpecGroup);
  };

  return (
    <div className="px-3 py-2 rounded border border-dashed border-green-500/30 bg-green-500/5 space-y-2">
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="Group name..."
        className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1.5
          text-foreground placeholder:text-muted-foreground/40
          focus:outline-none focus:border-green-500/50"
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            handleSubmit();
          }
        }}
      />
      <div className="flex items-center gap-2">
        <select
          value={category}
          onChange={(e) => setCategory(e.target.value)}
          className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground"
        >
          <option value="element-presence">element-presence</option>
          <option value="semantic">semantic</option>
          <option value="accessibility">accessibility</option>
          <option value="form-validation">form-validation</option>
          <option value="state-consistency">state-consistency</option>
          <option value="navigation">navigation</option>
          <option value="design">design</option>
          <option value="custom">custom</option>
        </select>
        <input
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="Description (optional)"
          className="flex-1 text-[10px] bg-transparent border border-white/10 rounded px-1.5 py-0.5
            text-foreground placeholder:text-muted-foreground/40
            focus:outline-none focus:border-green-500/50"
        />
      </div>
      <div className="flex items-center gap-2 justify-end">
        <button
          onClick={onCancel}
          className="text-[10px] text-muted-foreground hover:text-foreground transition-colors px-2 py-0.5"
        >
          Cancel
        </button>
        <button
          onClick={handleSubmit}
          disabled={!name.trim()}
          className="text-[10px] px-2 py-0.5 rounded bg-green-500/10 text-green-400 border border-green-500/20
            hover:bg-green-500/20 disabled:opacity-40 transition-colors"
        >
          Add Group
        </button>
      </div>
    </div>
  );
}

function PageSpecOverview({
  config,
  specId,
  source,
  appName,
  editMode,
  onAddGroup,
  onRemoveGroup,
}: {
  config: import("ui-bridge").SpecConfig;
  specId: string;
  source: string;
  appName?: string;
  editMode?: boolean;
  onAddGroup?: (group: SpecGroup) => void;
  onRemoveGroup?: (groupId: string) => void;
}) {
  const [showAddGroupForm, setShowAddGroupForm] = useState(false);
  const groups = config.groups || [];
  const totalAssertions = groups.reduce((sum, g) => sum + g.assertions.length, 0);
  const enabledAssertions = groups.reduce(
    (sum, g) => sum + g.assertions.filter((a) => a.enabled).length,
    0,
  );

  const bySeverity = { critical: 0, warning: 0, info: 0 };
  for (const group of groups) {
    for (const a of group.assertions) {
      if (!a.enabled) continue;
      const s = a.severity as keyof typeof bySeverity;
      if (s in bySeverity) bySeverity[s]++;
    }
  }

  const byCategory = new Map<string, number>();
  for (const group of groups) {
    const cat = group.category || "unknown";
    byCategory.set(
      cat,
      (byCategory.get(cat) || 0) + group.assertions.filter((a) => a.enabled).length,
    );
  }

  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Shield className="w-4 h-4 text-purple-400" />
          <h2 className="text-sm font-semibold text-foreground">{specId}</h2>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
          {!!config.metadata?.specType && (
            <span
              className={`text-[10px] px-1.5 py-0.5 rounded border ${
                config.metadata.specType === "semantic"
                  ? "bg-blue-500/10 text-blue-400 border-blue-500/20"
                  : config.metadata.specType === "mixed"
                    ? "bg-amber-500/10 text-amber-400 border-amber-500/20"
                    : config.metadata.specType === "comprehensive"
                      ? "bg-emerald-500/10 text-emerald-400 border-emerald-500/20"
                      : "bg-white/5 text-muted-foreground border-white/10"
              }`}
            >
              {config.metadata.specType as string}
            </span>
          )}
        </div>
        {config.description && (
          <p className="text-xs text-muted-foreground mt-1.5 leading-relaxed">
            {config.description}
          </p>
        )}
        {config.metadata?.pageUrl && (
          <div className="flex items-center gap-1 mt-1 text-[10px] text-muted-foreground/60">
            <ExternalLink className="w-2.5 h-2.5" />
            {config.metadata.pageUrl as string}
          </div>
        )}
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Groups" value={groups.length} />
        <StatCard label="Assertions" value={`${enabledAssertions}/${totalAssertions}`} />
        <StatCard label="Critical" value={bySeverity.critical} color="text-red-400" />
        <StatCard label="Warnings" value={bySeverity.warning} color="text-amber-400" />
      </div>

      <div>
        <h3 className="text-xs font-medium text-muted-foreground mb-2">Categories</h3>
        <div className="flex flex-wrap gap-1.5">
          {Array.from(byCategory.entries())
            .sort((a, b) => b[1] - a[1])
            .map(([cat, count]) => (
              <span
                key={cat}
                className="text-[10px] px-2 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
              >
                {cat}: {count}
              </span>
            ))}
        </div>
      </div>

      <div>
        <h3 className="text-xs font-medium text-muted-foreground mb-2">Groups ({groups.length})</h3>
        <div className="space-y-1">
          {groups.map((group) => {
            const enabled = group.assertions.filter((a) => a.enabled).length;
            return (
              <div
                key={group.id}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5
                  hover:bg-white/[0.04] transition-colors"
              >
                <span className="text-xs text-foreground flex-1 truncate">{group.name}</span>
                <CategoryBadge category={group.category} />
                <span className="text-[10px] text-muted-foreground tabular-nums">
                  {enabled} assertions
                </span>
                {editMode && onRemoveGroup && (
                  <button
                    onClick={() => onRemoveGroup(group.id)}
                    className="text-red-400/40 hover:text-red-400 transition-colors shrink-0"
                    title="Remove group"
                  >
                    <Trash2 className="w-3 h-3" />
                  </button>
                )}
              </div>
            );
          })}
        </div>

        {/* Add group */}
        {editMode && onAddGroup && (
          <>
            {showAddGroupForm ? (
              <div className="mt-2">
                <AddGroupForm
                  onAdd={(group) => {
                    onAddGroup(group);
                    setShowAddGroupForm(false);
                  }}
                  onCancel={() => setShowAddGroupForm(false)}
                />
              </div>
            ) : (
              <button
                onClick={() => setShowAddGroupForm(true)}
                className="w-full mt-2 flex items-center justify-center gap-1.5 px-3 py-2 text-xs text-muted-foreground/60
                  rounded border border-dashed border-white/10 hover:border-green-500/30 hover:text-green-400
                  transition-colors"
              >
                <Plus className="w-3 h-3" />
                Add Group
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Architecture Spec Components
// ============================================================================

function TechStackRow({ entry }: { entry: TechStackEntry }) {
  const categoryColors: Record<string, string> = {
    language: "text-blue-400",
    framework: "text-purple-400",
    library: "text-cyan-400",
    database: "text-green-400",
    service: "text-orange-400",
    tool: "text-amber-400",
    other: "text-muted-foreground",
  };

  return (
    <div className="flex items-center gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
      <Layers
        className={`w-3.5 h-3.5 shrink-0 ${categoryColors[entry.category] || "text-muted-foreground"}`}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-foreground">{entry.name}</span>
          {entry.version && (
            <span className="text-[10px] font-mono text-muted-foreground bg-white/5 px-1.5 py-0.5 rounded">
              {entry.version}
            </span>
          )}
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground/70 border border-white/10">
            {entry.category}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground mt-0.5">{entry.purpose}</p>
      </div>
    </div>
  );
}

function ConstraintRow({ constraint }: { constraint: ArchitectureConstraint }) {
  return (
    <div className="flex items-start gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02]">
      <Lock className="w-3.5 h-3.5 shrink-0 mt-0.5 text-amber-400/60" />
      <div className="flex-1 min-w-0">
        <p className="text-xs text-foreground leading-relaxed">{constraint.description}</p>
        <div className="flex items-center gap-2 mt-1.5">
          <SeverityBadge severity={constraint.severity} />
          <CategoryBadge category={constraint.category} />
          {constraint.verificationHint && (
            <span className="text-[10px] text-muted-foreground/60 italic truncate max-w-[300px]">
              verify: {constraint.verificationHint}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function FeatureRow({ feature }: { feature: FeatureSpec }) {
  const statusColors: Record<string, string> = {
    planned: "bg-blue-500/15 text-blue-400 border-blue-500/30",
    "in-progress": "bg-amber-500/15 text-amber-400 border-amber-500/30",
    completed: "bg-green-500/15 text-green-400 border-green-500/30",
    blocked: "bg-red-500/15 text-red-400 border-red-500/30",
  };
  const priorityColors: Record<string, string> = {
    critical: "text-red-400",
    high: "text-orange-400",
    medium: "text-amber-400",
    low: "text-muted-foreground",
  };

  return (
    <div className="flex items-start gap-3 px-3 py-2 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors">
      <CheckCircle2
        className={`w-3.5 h-3.5 shrink-0 mt-0.5 ${priorityColors[feature.priority] || "text-muted-foreground"}`}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-foreground">{feature.name}</span>
          <span
            className={`text-[10px] px-1.5 py-0.5 rounded border font-medium ${statusColors[feature.status] || statusColors.planned}`}
          >
            {feature.status}
          </span>
          <span className={`text-[10px] ${priorityColors[feature.priority]}`}>
            {feature.priority}
          </span>
        </div>
        <p className="text-[10px] text-muted-foreground mt-0.5 leading-relaxed">
          {feature.description}
        </p>
        {feature.pageSpecId && (
          <div className="flex items-center gap-1 mt-1 text-[10px] text-muted-foreground/50">
            <Link2 className="w-2.5 h-2.5" />
            spec: {feature.pageSpecId}
          </div>
        )}
      </div>
    </div>
  );
}

function ArchitectureOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ArchitectureConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      {/* Header */}
      <div>
        <div className="flex items-center gap-2">
          <Server className="w-4 h-4 text-indigo-400" />
          <h2 className="text-sm font-semibold text-foreground">{config.projectName}</h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-indigo-500/10 text-indigo-400 border border-indigo-500/20 font-medium">
            architecture
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
        {config.description && (
          <p className="text-xs text-muted-foreground mt-1.5 leading-relaxed">
            {config.description}
          </p>
        )}
      </div>

      {/* Stats */}
      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Tech Stack" value={config.techStack.length} color="text-purple-400" />
        <StatCard label="Patterns" value={config.patterns.length} color="text-cyan-400" />
        <StatCard label="Constraints" value={config.constraints.length} color="text-amber-400" />
        <StatCard label="Features" value={config.features.length} color="text-green-400" />
      </div>

      {/* Tech Stack */}
      {config.techStack.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Tech Stack ({config.techStack.length})
          </h3>
          <div className="space-y-1">
            {config.techStack.map((entry, i) => (
              <TechStackRow key={`${entry.name}-${i}`} entry={entry} />
            ))}
          </div>
        </div>
      )}

      {/* Directories */}
      {config.directories.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Directory Structure ({config.directories.length})
          </h3>
          <div className="space-y-1">
            {config.directories.map((dir, i) => (
              <div
                key={`${dir.path}-${i}`}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <Folder className="w-3 h-3 shrink-0 text-amber-400/60" />
                <span className="text-xs font-mono text-foreground">{dir.path}</span>
                {dir.required && (
                  <span className="text-[10px] px-1 py-0.5 rounded bg-red-500/10 text-red-400 border border-red-500/20">
                    required
                  </span>
                )}
                <span className="text-[10px] text-muted-foreground flex-1 truncate text-right">
                  {dir.purpose}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Patterns */}
      {config.patterns.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Architectural Patterns ({config.patterns.length})
          </h3>
          <div className="space-y-1">
            {config.patterns.map((pattern) => (
              <div
                key={pattern.id}
                className="px-3 py-2 rounded bg-white/[0.02] border border-white/5"
              >
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-foreground">{pattern.name}</span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20">
                    {pattern.scope}
                  </span>
                </div>
                <p className="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                  {pattern.description}
                </p>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Constraints */}
      {config.constraints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Constraints ({config.constraints.length})
          </h3>
          <div className="space-y-1">
            {config.constraints.map((constraint) => (
              <ConstraintRow key={constraint.id} constraint={constraint} />
            ))}
          </div>
        </div>
      )}

      {/* Features */}
      {config.features.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Features ({config.features.length})
          </h3>
          <div className="space-y-1">
            {config.features.map((feature) => (
              <FeatureRow key={feature.id} feature={feature} />
            ))}
          </div>
        </div>
      )}

      {/* Dependencies */}
      {config.dependencies.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Feature Dependencies ({config.dependencies.length})
          </h3>
          <div className="space-y-1">
            {config.dependencies.map((dep, i) => (
              <div
                key={`${dep.featureId}-${dep.dependsOn}-${i}`}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <span className="text-xs font-mono text-foreground">{dep.featureId}</span>
                <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
                  {dep.type}
                </span>
                <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
                <span className="text-xs font-mono text-foreground">{dep.dependsOn}</span>
                {dep.description && (
                  <span className="text-[10px] text-muted-foreground/50 truncate flex-1 text-right">
                    {dep.description}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// API Spec Components
// ============================================================================

const METHOD_COLORS: Record<string, string> = {
  GET: "bg-green-500/15 text-green-400 border-green-500/30",
  POST: "bg-blue-500/15 text-blue-400 border-blue-500/30",
  PUT: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  PATCH: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  DELETE: "bg-red-500/15 text-red-400 border-red-500/30",
};

function SchemaFieldRow({ field, depth = 0 }: { field: SchemaField; depth?: number }) {
  return (
    <>
      <div
        className="flex items-center gap-2 py-0.5"
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
      >
        <span className="text-xs font-mono text-foreground">{field.name}</span>
        <span className="text-[10px] font-mono text-cyan-400/80">{field.type}</span>
        {field.required && <span className="text-[10px] text-red-400/70">required</span>}
        {field.description && (
          <span className="text-[10px] text-muted-foreground/60 truncate flex-1">
            — {field.description}
          </span>
        )}
      </div>
      {field.fields?.map((nested, i) => (
        <SchemaFieldRow
          key={`${field.name}-${nested.name}-${i}`}
          field={nested}
          depth={depth + 1}
        />
      ))}
      {field.items && (
        <SchemaFieldRow
          key={`${field.name}-items`}
          field={{ ...field.items, name: `[${field.items.name || "item"}]` }}
          depth={depth + 1}
        />
      )}
    </>
  );
}

function EndpointRow({ endpoint }: { endpoint: ApiEndpoint }) {
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors space-y-2">
      <div className="flex items-center gap-2">
        <span
          className={`text-[10px] font-mono font-bold px-2 py-0.5 rounded border ${METHOD_COLORS[endpoint.method] || METHOD_COLORS.GET}`}
        >
          {endpoint.method}
        </span>
        <span className="text-xs font-mono text-foreground">{endpoint.path}</span>
        {endpoint.auth && <Lock className="w-3 h-3 text-amber-400/60" />}
        {endpoint.featureId && (
          <span className="text-[10px] text-muted-foreground/50 ml-auto">
            feature: {endpoint.featureId}
          </span>
        )}
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{endpoint.description}</p>

      {/* Request params */}
      {endpoint.pathParams && endpoint.pathParams.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Path params:</span>
          <div className="mt-0.5 border-l-2 border-white/5">
            {endpoint.pathParams.map((p, i) => (
              <SchemaFieldRow key={`path-${p.name}-${i}`} field={p} />
            ))}
          </div>
        </div>
      )}
      {endpoint.queryParams && endpoint.queryParams.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Query params:</span>
          <div className="mt-0.5 border-l-2 border-white/5">
            {endpoint.queryParams.map((p, i) => (
              <SchemaFieldRow key={`query-${p.name}-${i}`} field={p} />
            ))}
          </div>
        </div>
      )}
      {endpoint.requestBody && endpoint.requestBody.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Request body:</span>
          <div className="mt-0.5 border-l-2 border-cyan-500/20">
            {endpoint.requestBody.map((f, i) => (
              <SchemaFieldRow key={`req-${f.name}-${i}`} field={f} />
            ))}
          </div>
        </div>
      )}
      {endpoint.responseBody && endpoint.responseBody.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Response:</span>
          <div className="mt-0.5 border-l-2 border-green-500/20">
            {endpoint.responseBody.map((f, i) => (
              <SchemaFieldRow key={`res-${f.name}-${i}`} field={f} />
            ))}
          </div>
        </div>
      )}

      {/* Status codes */}
      {endpoint.statusCodes && endpoint.statusCodes.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-[10px] font-medium text-muted-foreground">Status:</span>
          {endpoint.statusCodes.map((sc) => (
            <span
              key={sc.code}
              className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
              title={sc.description}
            >
              {sc.code}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function ModelCard({ model }: { model: DataModel }) {
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] space-y-2">
      <div className="flex items-center gap-2">
        <Database className="w-3.5 h-3.5 text-green-400/60" />
        <span className="text-xs font-medium text-foreground">{model.name}</span>
        <span className="text-[10px] text-muted-foreground">{model.fields.length} fields</span>
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{model.description}</p>

      <div className="border-l-2 border-white/5">
        {model.fields.map((f, i) => (
          <SchemaFieldRow key={`${model.id}-${f.name}-${i}`} field={f} />
        ))}
      </div>

      {model.relations && model.relations.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap pt-1 border-t border-white/5">
          <span className="text-[10px] font-medium text-muted-foreground">Relations:</span>
          {model.relations.map((rel, i) => (
            <span
              key={`${rel.modelId}-${i}`}
              className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            >
              {rel.type} → {rel.modelId}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function ApiOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ApiConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  const methodCounts = new Map<string, number>();
  for (const ep of config.endpoints) {
    methodCounts.set(ep.method, (methodCounts.get(ep.method) || 0) + 1);
  }

  return (
    <div className="space-y-5">
      {/* Header */}
      <div>
        <div className="flex items-center gap-2">
          <Globe className="w-4 h-4 text-cyan-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || config.basePath}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20 font-medium">
            api
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
        <div className="flex items-center gap-2 mt-1.5">
          <span className="text-xs font-mono text-muted-foreground">{config.basePath}</span>
          {config.authType && config.authType !== "none" && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20">
              <Lock className="w-2.5 h-2.5 inline mr-0.5" />
              {config.authType}
            </span>
          )}
        </div>
      </div>

      {/* Stats */}
      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Endpoints" value={config.endpoints.length} color="text-cyan-400" />
        <StatCard label="Models" value={config.models.length} color="text-green-400" />
        <StatCard
          label="Auth Required"
          value={config.endpoints.filter((e) => e.auth).length}
          color="text-amber-400"
        />
        <StatCard
          label="Methods"
          value={Array.from(methodCounts.entries())
            .map(([m, c]) => `${m}:${c}`)
            .join(" ")}
        />
      </div>

      {/* Endpoints */}
      {config.endpoints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Endpoints ({config.endpoints.length})
          </h3>
          <div className="space-y-2">
            {config.endpoints.map((endpoint) => (
              <EndpointRow key={endpoint.id} endpoint={endpoint} />
            ))}
          </div>
        </div>
      )}

      {/* Models */}
      {config.models.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Data Models ({config.models.length})
          </h3>
          <div className="space-y-2">
            {config.models.map((model) => (
              <ModelCard key={model.id} model={model} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Data Spec Components
// ============================================================================

function ColumnRow({ column }: { column: DataColumn }) {
  return (
    <div className="flex items-center gap-2 py-0.5 px-2">
      <span className="text-xs font-mono text-foreground">{column.name}</span>
      <span className="text-[10px] font-mono text-cyan-400/80">{column.type}</span>
      {column.primaryKey && <KeyRound className="w-2.5 h-2.5 text-amber-400" />}
      {column.required && <span className="text-[10px] text-red-400/70">NOT NULL</span>}
      {column.unique && <span className="text-[10px] text-purple-400/70">UNIQUE</span>}
      {column.defaultValue && (
        <span className="text-[10px] text-muted-foreground/50">default: {column.defaultValue}</span>
      )}
      {column.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1">
          — {column.description}
        </span>
      )}
    </div>
  );
}

function EntityCard({ entity }: { entity: DataEntity }) {
  const pkColumns = entity.columns.filter((c) => c.primaryKey);
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] space-y-2">
      <div className="flex items-center gap-2">
        <Table2 className="w-3.5 h-3.5 text-green-400/60" />
        <span className="text-xs font-medium text-foreground">{entity.name}</span>
        <span className="text-[10px] text-muted-foreground">{entity.columns.length} columns</span>
        {pkColumns.length > 0 && (
          <span className="text-[10px] text-amber-400/60">
            PK: {pkColumns.map((c) => c.name).join(", ")}
          </span>
        )}
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{entity.description}</p>

      <div className="border-l-2 border-white/5">
        {entity.columns.map((col, i) => (
          <ColumnRow key={`${entity.id}-${col.name}-${i}`} column={col} />
        ))}
      </div>

      {entity.indexes && entity.indexes.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap pt-1 border-t border-white/5">
          <span className="text-[10px] font-medium text-muted-foreground">Indexes:</span>
          {entity.indexes.map((idx, i) => (
            <span
              key={`${idx.name}-${i}`}
              className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            >
              {idx.unique ? "UNIQUE " : ""}
              {idx.name} ({idx.columns.join(", ")})
            </span>
          ))}
        </div>
      )}

      {entity.timestamps && (
        <div className="flex items-center gap-2 text-[10px] text-muted-foreground/50 pt-1 border-t border-white/5">
          {entity.timestamps.createdAt && <span>created: {entity.timestamps.createdAt}</span>}
          {entity.timestamps.updatedAt && <span>updated: {entity.timestamps.updatedAt}</span>}
          {entity.softDelete && <span>soft delete: {entity.softDelete}</span>}
        </div>
      )}
    </div>
  );
}

function RelationRow({ relation }: { relation: DataRelation }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className="text-xs font-mono text-foreground">{relation.fromEntity}</span>
      <span className="text-[10px] text-muted-foreground/60">
        ({relation.fromColumns.join(", ")})
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
        {relation.type}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className="text-xs font-mono text-foreground">{relation.toEntity}</span>
      <span className="text-[10px] text-muted-foreground/60">
        ({relation.toColumns.join(", ")})
      </span>
      {relation.onDelete && (
        <span className="text-[10px] text-red-400/50 ml-auto">ON DELETE {relation.onDelete}</span>
      )}
    </div>
  );
}

function DataOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: DataConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Database className="w-4 h-4 text-green-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Data Schema"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/10 text-green-400 border border-green-500/20 font-medium">
            data
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
          <span className="text-[10px] font-mono text-muted-foreground">{config.database}</span>
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Entities" value={config.entities.length} color="text-green-400" />
        <StatCard label="Relations" value={config.relations.length} color="text-cyan-400" />
        <StatCard label="Seeds" value={config.seeds?.length || 0} color="text-amber-400" />
        <StatCard
          label="Migrations"
          value={config.migrations?.length || 0}
          color="text-purple-400"
        />
      </div>

      {config.entities.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Entities ({config.entities.length})
          </h3>
          <div className="space-y-2">
            {config.entities.map((entity) => (
              <EntityCard key={entity.id} entity={entity} />
            ))}
          </div>
        </div>
      )}

      {config.relations.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Relations ({config.relations.length})
          </h3>
          <div className="space-y-1">
            {config.relations.map((rel) => (
              <RelationRow key={rel.id} relation={rel} />
            ))}
          </div>
        </div>
      )}

      {config.seeds && config.seeds.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Seed Data ({config.seeds.length})
          </h3>
          <div className="space-y-1">
            {config.seeds.map((seed, i) => (
              <div
                key={`${seed.entityId}-${i}`}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <span className="text-xs font-mono text-foreground">{seed.entityId}</span>
                <span className="text-[10px] text-muted-foreground">
                  {typeof seed.records === "number"
                    ? `${seed.records} records`
                    : `${seed.records.length} records`}
                </span>
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
                  {seed.environment}
                </span>
                {seed.description && (
                  <span className="text-[10px] text-muted-foreground/50 truncate flex-1 text-right">
                    {seed.description}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {config.migrations && config.migrations.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Planned Migrations ({config.migrations.length})
          </h3>
          <div className="space-y-1">
            {config.migrations
              .sort((a, b) => a.order - b.order)
              .map((mig) => (
                <div
                  key={mig.id}
                  className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
                >
                  <span className="text-[10px] font-mono text-muted-foreground/50 tabular-nums w-6">
                    #{mig.order}
                  </span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/10 text-purple-400 border border-purple-500/20">
                    {mig.changeType}
                  </span>
                  <span className="text-xs text-foreground flex-1">{mig.description}</span>
                  <span className="text-[10px] text-muted-foreground/50">
                    {mig.entityIds.join(", ")}
                  </span>
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Dependency Spec Components
// ============================================================================

const ARTIFACT_COLORS: Record<string, string> = {
  page: "text-purple-400",
  "api-endpoint": "text-cyan-400",
  "data-entity": "text-green-400",
  module: "text-blue-400",
  service: "text-orange-400",
  component: "text-pink-400",
};

const LINK_TYPE_COLORS: Record<string, string> = {
  calls: "bg-cyan-500/10 text-cyan-400 border-cyan-500/20",
  reads: "bg-green-500/10 text-green-400 border-green-500/20",
  writes: "bg-amber-500/10 text-amber-400 border-amber-500/20",
  renders: "bg-purple-500/10 text-purple-400 border-purple-500/20",
  imports: "bg-blue-500/10 text-blue-400 border-blue-500/20",
  extends: "bg-pink-500/10 text-pink-400 border-pink-500/20",
  configures: "bg-orange-500/10 text-orange-400 border-orange-500/20",
};

function LinkRow({ link }: { link: DependencyLink }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className={`text-[10px] ${ARTIFACT_COLORS[link.from.kind] || "text-muted-foreground"}`}>
        {link.from.kind}
      </span>
      <span className="text-xs font-mono text-foreground">
        {link.from.label || link.from.artifactId}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span
        className={`text-[10px] px-1.5 py-0.5 rounded border ${LINK_TYPE_COLORS[link.type] || "bg-white/5 text-muted-foreground border-white/10"}`}
      >
        {link.type}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className={`text-[10px] ${ARTIFACT_COLORS[link.to.kind] || "text-muted-foreground"}`}>
        {link.to.kind}
      </span>
      <span className="text-xs font-mono text-foreground">
        {link.to.label || link.to.artifactId}
      </span>
      {!link.required && (
        <span className="text-[10px] text-muted-foreground/40 italic ml-auto">optional</span>
      )}
    </div>
  );
}

function ModuleCard({ module }: { module: ModuleRef }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
        {module.moduleType}
      </span>
      <span className="text-xs font-medium text-foreground">{module.name}</span>
      <span className="text-[10px] font-mono text-muted-foreground/50">{module.path}</span>
      {module.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {module.description}
        </span>
      )}
    </div>
  );
}

function DependencyOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: DependencyConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  // Group links by type
  const byType = new Map<string, number>();
  for (const link of config.links) {
    byType.set(link.type, (byType.get(link.type) || 0) + 1);
  }

  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <GitBranch className="w-4 h-4 text-blue-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Dependencies"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-blue-500/10 text-blue-400 border border-blue-500/20 font-medium">
            dependencies
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Modules" value={config.modules.length} color="text-blue-400" />
        <StatCard label="Links" value={config.links.length} color="text-cyan-400" />
        <StatCard
          label="Required"
          value={config.links.filter((l) => l.required).length}
          color="text-red-400"
        />
        <StatCard label="Clusters" value={config.clusters?.length || 0} color="text-purple-400" />
      </div>

      {/* Link type breakdown */}
      <div className="flex flex-wrap gap-1.5">
        {Array.from(byType.entries())
          .sort((a, b) => b[1] - a[1])
          .map(([type, count]) => (
            <span
              key={type}
              className={`text-[10px] px-2 py-0.5 rounded border ${LINK_TYPE_COLORS[type] || "bg-white/5 text-muted-foreground border-white/10"}`}
            >
              {type}: {count}
            </span>
          ))}
      </div>

      {config.modules.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Modules ({config.modules.length})
          </h3>
          <div className="space-y-1">
            {config.modules.map((mod) => (
              <ModuleCard key={mod.id} module={mod} />
            ))}
          </div>
        </div>
      )}

      {config.links.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Dependency Links ({config.links.length})
          </h3>
          <div className="space-y-1">
            {config.links.map((link) => (
              <LinkRow key={link.id} link={link} />
            ))}
          </div>
        </div>
      )}

      {config.clusters && config.clusters.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Build Clusters ({config.clusters.length})
          </h3>
          <div className="space-y-2">
            {config.clusters
              .sort((a, b) => a.buildOrder - b.buildOrder)
              .map((cluster) => (
                <div
                  key={cluster.id}
                  className="px-3 py-2 rounded border border-white/5 bg-white/[0.02]"
                >
                  <div className="flex items-center gap-2">
                    <span className="text-[10px] font-mono text-muted-foreground/50 tabular-nums w-6">
                      #{cluster.buildOrder}
                    </span>
                    <span className="text-xs font-medium text-foreground">{cluster.name}</span>
                    <span className="text-[10px] text-muted-foreground">
                      {cluster.artifacts.length} artifacts
                    </span>
                  </div>
                  <p className="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                    {cluster.description}
                  </p>
                  <div className="flex flex-wrap gap-1 mt-1.5">
                    {cluster.artifacts.map((ref, i) => (
                      <span
                        key={`${ref.artifactId}-${i}`}
                        className={`text-[10px] px-1.5 py-0.5 rounded bg-white/5 border border-white/10 ${ARTIFACT_COLORS[ref.kind] || "text-muted-foreground"}`}
                      >
                        {ref.label || ref.artifactId}
                      </span>
                    ))}
                  </div>
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Constraint Spec Components
// ============================================================================

function PerfBudgetRow({ budget }: { budget: PerformanceBudget }) {
  const metricLabel =
    budget.metric === "custom" ? budget.customMetric || "custom" : budget.metric.toUpperCase();
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Zap className="w-3 h-3 text-amber-400/60" />
      <span className="text-xs font-mono font-medium text-foreground">{metricLabel}</span>
      <span className="text-[10px] font-mono text-cyan-400">
        ≤ {budget.budget}
        {budget.unit}
      </span>
      {budget.pageIds && budget.pageIds.length > 0 && (
        <span className="text-[10px] text-muted-foreground/50">
          pages: {budget.pageIds.join(", ")}
        </span>
      )}
      {budget.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {budget.description}
        </span>
      )}
    </div>
  );
}

function BundleBudgetRow({ budget }: { budget: BundleBudget }) {
  const sizeLabel =
    budget.maxSizeBytes >= 1024 * 1024
      ? `${(budget.maxSizeBytes / (1024 * 1024)).toFixed(1)} MB`
      : `${(budget.maxSizeBytes / 1024).toFixed(0)} KB`;
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Layers className="w-3 h-3 text-purple-400/60" />
      <span className="text-xs font-mono text-foreground">{budget.target}</span>
      {budget.name && (
        <span className="text-[10px] font-mono text-muted-foreground">{budget.name}</span>
      )}
      <span className="text-[10px] font-mono text-cyan-400">≤ {sizeLabel}</span>
      <span className="text-[10px] text-muted-foreground/50">
        {budget.compressed ? "gzipped" : "raw"}
      </span>
    </div>
  );
}

function BrowserRow({ target }: { target: BrowserTarget }) {
  const supportColors: Record<string, string> = {
    full: "text-green-400",
    partial: "text-amber-400",
    none: "text-red-400",
  };
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Monitor className="w-3 h-3 text-blue-400/60" />
      <span className="text-xs text-foreground">{target.browser}</span>
      <span className="text-[10px] font-mono text-muted-foreground">≥ {target.minVersion}</span>
      <span
        className={`text-[10px] font-medium ${supportColors[target.support] || "text-muted-foreground"}`}
      >
        {target.support}
      </span>
    </div>
  );
}

function BreakpointRow({ bp }: { bp: ResponsiveBreakpoint }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <Smartphone className="w-3 h-3 text-pink-400/60" />
      <span className="text-xs font-medium text-foreground">{bp.name}</span>
      <span className="text-[10px] font-mono text-cyan-400">
        {bp.minWidth}px{bp.maxWidth ? ` – ${bp.maxWidth}px` : "+"}
      </span>
      {bp.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1 text-right">
          {bp.description}
        </span>
      )}
    </div>
  );
}

function ConstraintOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ConstraintConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Gauge className="w-4 h-4 text-amber-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Project Constraints"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20 font-medium">
            constraints
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard
          label="Performance"
          value={config.performance?.length || 0}
          color="text-amber-400"
        />
        <StatCard
          label="Bundles"
          value={config.bundleBudgets?.length || 0}
          color="text-purple-400"
        />
        <StatCard label="Browsers" value={config.browsers?.length || 0} color="text-blue-400" />
        <StatCard
          label="Breakpoints"
          value={config.breakpoints?.length || 0}
          color="text-pink-400"
        />
      </div>

      {config.performance && config.performance.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Performance Budgets ({config.performance.length})
          </h3>
          <div className="space-y-1">
            {config.performance.map((budget) => (
              <PerfBudgetRow key={budget.id} budget={budget} />
            ))}
          </div>
        </div>
      )}

      {config.bundleBudgets && config.bundleBudgets.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Bundle Budgets ({config.bundleBudgets.length})
          </h3>
          <div className="space-y-1">
            {config.bundleBudgets.map((budget) => (
              <BundleBudgetRow key={budget.id} budget={budget} />
            ))}
          </div>
        </div>
      )}

      {config.browsers && config.browsers.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Browser Support ({config.browsers.length})
          </h3>
          <div className="space-y-1">
            {config.browsers.map((target) => (
              <BrowserRow key={target.browser} target={target} />
            ))}
          </div>
        </div>
      )}

      {config.breakpoints && config.breakpoints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Responsive Breakpoints ({config.breakpoints.length})
          </h3>
          <div className="space-y-1">
            {config.breakpoints.map((bp) => (
              <BreakpointRow key={bp.id} bp={bp} />
            ))}
          </div>
        </div>
      )}

      {config.accessibility && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">Accessibility</h3>
          <div className="px-3 py-2 rounded border border-white/5 bg-white/[0.02] space-y-1.5">
            <div className="flex items-center gap-2">
              <span className="text-xs font-medium text-foreground">
                WCAG {config.accessibility.level}
              </span>
              {config.accessibility.minContrastRatio && (
                <span className="text-[10px] text-muted-foreground">
                  min contrast: {config.accessibility.minContrastRatio}:1
                </span>
              )}
              {config.accessibility.minTouchTarget && (
                <span className="text-[10px] text-muted-foreground">
                  min touch: {config.accessibility.minTouchTarget}px
                </span>
              )}
            </div>
            {config.accessibility.description && (
              <p className="text-[10px] text-muted-foreground/60 leading-relaxed">
                {config.accessibility.description}
              </p>
            )}
            {config.accessibility.requiredCriteria &&
              config.accessibility.requiredCriteria.length > 0 && (
                <div className="flex flex-wrap gap-1">
                  {config.accessibility.requiredCriteria.map((c) => (
                    <span
                      key={c}
                      className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
                    >
                      {c}
                    </span>
                  ))}
                </div>
              )}
          </div>
        </div>
      )}

      {config.capacity && config.capacity.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Capacity Constraints ({config.capacity.length})
          </h3>
          <div className="space-y-1">
            {config.capacity.map((cap) => (
              <div
                key={cap.id}
                className="px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <div className="flex items-center gap-2">
                  <span className="text-xs font-medium text-foreground">{cap.target}</span>
                  {cap.maxConcurrent && (
                    <span className="text-[10px] text-cyan-400">
                      max concurrent: {cap.maxConcurrent}
                    </span>
                  )}
                  {cap.rateLimit && (
                    <span className="text-[10px] text-amber-400">
                      {cap.rateLimit.requests}/{cap.rateLimit.windowSeconds}s
                    </span>
                  )}
                  {cap.maxResponseTimeMs && (
                    <span className="text-[10px] text-purple-400">≤ {cap.maxResponseTimeMs}ms</span>
                  )}
                </div>
                {cap.description && (
                  <p className="text-[10px] text-muted-foreground/60 mt-0.5">{cap.description}</p>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main Detail Panel
// ============================================================================

// ============================================================================
// Unspecced page placeholder
// ============================================================================

function UnspeccedPageView({
  label,
  description,
  expectedSpecId,
  onGenerateSpec,
}: {
  label: string;
  description: string;
  expectedSpecId: string;
  onGenerateSpec: () => void;
}) {
  return (
    <div className="h-full flex items-center justify-center p-8">
      <div className="text-center max-w-md space-y-4">
        <div className="w-12 h-12 rounded-full bg-amber-500/10 border border-amber-500/20 flex items-center justify-center mx-auto">
          <Shield className="w-6 h-6 text-amber-500/60" />
        </div>
        <div>
          <h2 className="text-lg font-semibold text-foreground">{label}</h2>
          <p className="text-xs text-muted-foreground mt-1">{description}</p>
        </div>
        <div className="text-xs text-muted-foreground/60 bg-white/[0.02] rounded-lg border border-white/5 px-4 py-3">
          <p>No spec exists yet for this page.</p>
          <p className="mt-1 text-[10px] opacity-70">
            Expected spec ID: <code className="text-amber-400/60">{expectedSpecId}</code>
          </p>
        </div>
        <button
          onClick={onGenerateSpec}
          className="inline-flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg
            bg-purple-500/15 text-purple-400 border border-purple-500/25
            hover:bg-purple-500/25 transition-colors"
        >
          <Plus className="w-4 h-4" />
          Generate Spec
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// SpecDetailPanel
// ============================================================================

interface SpecDetailPanelProps {
  selectedSpec: LoadedSpec | null;
  selectedGroup: SpecGroup | null;
  selectionType: "none" | "spec" | "group" | "assertion" | "unspecced-page";
  editMode?: boolean;
  unspeccedPageInfo?: { label: string; description: string; expectedSpecId: string } | null;
  onToggleAssertion?: (specId: string, groupId: string, assertionId: string) => void;
  onRemoveAssertion?: (specId: string, groupId: string, assertionId: string) => void;
  onAddAssertion?: (specId: string, groupId: string, assertion: SpecAssertion) => void;
  onAddGroup?: (specId: string, group: SpecGroup) => void;
  onRemoveGroup?: (specId: string, groupId: string) => void;
  onUpdateSetupActions?: (specId: string, groupId: string, actions: SetupAction[]) => void;
  onClearSelection?: () => void;
  onGenerateSpec?: () => void;
}

export function SpecDetailPanel({
  selectedSpec,
  selectedGroup,
  selectionType,
  editMode,
  unspeccedPageInfo,
  onToggleAssertion,
  onRemoveAssertion,
  onAddAssertion,
  onAddGroup,
  onRemoveGroup,
  onUpdateSetupActions,
  onClearSelection,
  onGenerateSpec,
}: SpecDetailPanelProps) {
  const [activeTab, setActiveTab] = useState<"assertions" | "issues">("assertions");

  // Unspecced page selected — show generate prompt
  if (selectionType === "unspecced-page" && unspeccedPageInfo) {
    return (
      <UnspeccedPageView
        label={unspeccedPageInfo.label}
        description={unspeccedPageInfo.description}
        expectedSpecId={unspeccedPageInfo.expectedSpecId}
        onGenerateSpec={onGenerateSpec || (() => {})}
      />
    );
  }

  if (!selectedSpec) {
    return <GlobalIssuesPanel />;
  }

  if (selectionType === "group" && selectedGroup && selectedSpec.kind === "page-spec") {
    return (
      <div className="h-full overflow-y-auto p-4">
        <GroupDetail
          group={selectedGroup}
          specId={selectedSpec.specId}
          editMode={editMode}
          onToggleAssertion={
            onToggleAssertion
              ? (aid) => onToggleAssertion(selectedSpec.specId, selectedGroup.id, aid)
              : undefined
          }
          onRemoveAssertion={
            onRemoveAssertion
              ? (aid) => onRemoveAssertion(selectedSpec.specId, selectedGroup.id, aid)
              : undefined
          }
          onAddAssertion={
            onAddAssertion
              ? (a) => onAddAssertion(selectedSpec.specId, selectedGroup.id, a)
              : undefined
          }
          onRemoveGroup={
            onRemoveGroup ? () => onRemoveGroup(selectedSpec.specId, selectedGroup.id) : undefined
          }
          onUpdateSetupActions={
            onUpdateSetupActions
              ? (actions) => onUpdateSetupActions(selectedSpec.specId, selectedGroup.id, actions)
              : undefined
          }
        />
      </div>
    );
  }

  // Page spec view with tabs for Assertions and Issues
  if (selectedSpec.kind === "page-spec") {
    const pageUrl = (selectedSpec.config as { metadata?: { pageUrl?: string } })?.metadata?.pageUrl;

    return (
      <div className="h-full flex flex-col">
        {/* Tab bar */}
        <div className="flex border-b border-border px-4 pt-2 gap-1 shrink-0">
          <button
            onClick={() => setActiveTab("assertions")}
            className={`px-3 py-1.5 text-xs font-medium rounded-t border border-b-0 transition-colors ${
              activeTab === "assertions"
                ? "bg-background text-foreground border-border"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            <Shield className="w-3 h-3 inline-block mr-1 -mt-0.5" />
            Assertions
          </button>
          <button
            onClick={() => setActiveTab("issues")}
            className={`px-3 py-1.5 text-xs font-medium rounded-t border border-b-0 transition-colors ${
              activeTab === "issues"
                ? "bg-background text-foreground border-border"
                : "text-muted-foreground border-transparent hover:text-foreground"
            }`}
          >
            <Bug className="w-3 h-3 inline-block mr-1 -mt-0.5" />
            Issues
          </button>
        </div>

        {/* Tab content */}
        <div className="flex-1 overflow-y-auto p-4">
          {activeTab === "assertions" && (
            <PageSpecOverview
              config={selectedSpec.config}
              specId={selectedSpec.specId}
              source={selectedSpec.source}
              appName={selectedSpec.appName}
              editMode={editMode}
              onAddGroup={onAddGroup ? (g) => onAddGroup(selectedSpec.specId, g) : undefined}
              onRemoveGroup={
                onRemoveGroup ? (gid) => onRemoveGroup(selectedSpec.specId, gid) : undefined
              }
            />
          )}
          {activeTab === "issues" && (
            <SpecIssuesPanel
              specId={selectedSpec.specId}
              pageUrl={pageUrl}
              onViewAllIssues={onClearSelection}
            />
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto p-4">
      {selectedSpec.kind === "architecture" && (
        <ArchitectureOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "api" && (
        <ApiOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "data" && (
        <DataOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "dependency" && (
        <DependencyOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
      {selectedSpec.kind === "constraint" && (
        <ConstraintOverview
          config={selectedSpec.config}
          specId={selectedSpec.specId}
          source={selectedSpec.source}
          appName={selectedSpec.appName}
        />
      )}
    </div>
  );
}
