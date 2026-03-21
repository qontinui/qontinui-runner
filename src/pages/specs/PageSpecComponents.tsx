import { useState, useReducer } from "react";
import {
  Shield,
  Eye,
  EyeOff,
  ExternalLink,
  Trash2,
  Plus,
  MousePointer2,
  Type,
  Clock,
  Navigation,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import type { SpecGroup, SpecAssertion, SetupAction } from "./types";
import {
  SeverityBadge,
  CategoryBadge,
  StatCard,
  ASSERTION_TYPE_TOOLTIPS,
  SOURCE_TOOLTIPS,
  SPEC_LOAD_SOURCE_TOOLTIPS,
} from "./spec-badges";

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

interface AssertionFormState {
  description: string;
  severity: "critical" | "warning" | "info";
  assertionType: string;
  category: string;
  targetText: string;
  relatedTargetText: string;
  minGap: number;
}

const ASSERTION_FORM_INIT: AssertionFormState = {
  description: "",
  severity: "warning",
  assertionType: "exists",
  category: "custom",
  targetText: "",
  relatedTargetText: "",
  minGap: 0,
};

function assertionFormReducer(
  state: AssertionFormState,
  action: { type: "SET"; field: keyof AssertionFormState; value: string | number },
): AssertionFormState {
  return { ...state, [action.field]: action.value };
}

function AddAssertionForm({
  onAdd,
  onCancel,
}: {
  onAdd: (assertion: SpecAssertion) => void;
  onCancel: () => void;
}) {
  const [formState, formDispatch] = useReducer(assertionFormReducer, ASSERTION_FORM_INIT);
  const { description, severity, assertionType, category, targetText, relatedTargetText, minGap } =
    formState;
  const setF = (field: keyof AssertionFormState, value: string | number) =>
    formDispatch({ type: "SET", field, value });

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
    setF("description", "");
    setF("targetText", "");
    setF("relatedTargetText", "");
  };

  return (
    <div className="px-3 py-2 rounded border border-dashed border-green-500/30 bg-green-500/5 space-y-2">
      <textarea
        value={description}
        onChange={(e) => setF("description", e.target.value)}
        placeholder="Assertion description..."
        rows={2}
        className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1.5
          text-foreground placeholder:text-muted-foreground/40 resize-none
          focus:outline-hidden focus:border-green-500/50"
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
          onChange={(e) => setF("assertionType", e.target.value)}
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
          onChange={(e) => setF("severity", e.target.value)}
          className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-foreground"
        >
          <option value="critical">critical</option>
          <option value="warning">warning</option>
          <option value="info">info</option>
        </select>
        <select
          value={category}
          onChange={(e) => setF("category", e.target.value)}
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
        onChange={(e) => setF("targetText", e.target.value)}
        placeholder="Target element text..."
        className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
          text-foreground placeholder:text-muted-foreground/40
          focus:outline-hidden focus:border-green-500/50"
      />
      {isSpatial && (
        <input
          type="text"
          value={relatedTargetText}
          onChange={(e) => setF("relatedTargetText", e.target.value)}
          placeholder="Related target element text..."
          className="w-full text-xs bg-transparent border border-cyan-500/20 rounded px-2 py-1
            text-foreground placeholder:text-muted-foreground/40
            focus:outline-hidden focus:border-cyan-500/50"
        />
      )}
      {assertionType === "minSpacing" && (
        <div className="flex items-center gap-2">
          <label htmlFor="assertion-min-gap" className="text-[10px] text-muted-foreground">
            Min gap (px):
          </label>
          <input
            id="assertion-min-gap"
            type="number"
            value={minGap}
            onChange={(e) => setF("minGap", Number(e.target.value))}
            min={0}
            className="w-20 text-xs bg-transparent border border-white/10 rounded px-2 py-0.5
              text-foreground focus:outline-hidden focus:border-green-500/50"
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
  const [edState, edDispatch] = useReducer(
    (s: Record<string, unknown>, a: { k: string; v: unknown }) => ({ ...s, [a.k]: a.v }),
    {
      expanded: actions.length > 0,
      showAddForm: false,
      newType: "click",
      newTargetText: "",
      newValue: "",
      newUrl: "",
      newMs: 1000,
    },
  );
  const expanded = edState.expanded as boolean;
  const showAddForm = edState.showAddForm as boolean;
  const newType = edState.newType as SetupAction["type"];
  const newTargetText = edState.newTargetText as string;
  const newValue = edState.newValue as string;
  const newUrl = edState.newUrl as string;
  const newMs = edState.newMs as number;
  const setEd = (k: string, v: unknown) => edDispatch({ k, v });

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
    setEd("newTargetText", "");
    setEd("newValue", "");
    setEd("newUrl", "");
    setEd("showAddForm", false);
  };

  const handleRemove = (index: number) => {
    onChange?.(actions.filter((_, i) => i !== index));
  };

  const needsTarget = newType === "click" || newType === "type" || newType === "waitForElement";

  return (
    <div className="space-y-1">
      <button
        onClick={() => setEd("expanded", !expanded)}
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
                key={`${action.type}-${detail || "no-detail"}`}
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
              onClick={() => setEd("showAddForm", true)}
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
                onChange={(e) => setEd("newType", e.target.value)}
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
                  onChange={(e) => setEd("newTargetText", e.target.value)}
                  placeholder="Target element text..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-hidden focus:border-cyan-500/50"
                />
              )}

              {newType === "type" && (
                <input
                  type="text"
                  value={newValue}
                  onChange={(e) => setEd("newValue", e.target.value)}
                  placeholder="Text to type..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-hidden focus:border-green-500/50"
                />
              )}

              {newType === "navigate" && (
                <input
                  type="text"
                  value={newUrl}
                  onChange={(e) => setEd("newUrl", e.target.value)}
                  placeholder="http://localhost:3001/..."
                  className="w-full text-xs bg-transparent border border-white/10 rounded px-2 py-1
                    text-foreground placeholder:text-muted-foreground/40
                    focus:outline-hidden focus:border-purple-500/50"
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
                    onChange={(e) => setEd("newMs", Number(e.target.value))}
                    min={0}
                    className="w-20 text-xs bg-transparent border border-white/10 rounded px-2 py-0.5
                      text-foreground focus:outline-hidden focus:border-cyan-500/50"
                  />
                </div>
              )}

              <div className="flex items-center gap-2">
                <div className="flex-1" />
                <button
                  onClick={() => setEd("showAddForm", false)}
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

export function GroupDetail({
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
          focus:outline-hidden focus:border-green-500/50"
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
            focus:outline-hidden focus:border-green-500/50"
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

export function PageSpecOverview({
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
