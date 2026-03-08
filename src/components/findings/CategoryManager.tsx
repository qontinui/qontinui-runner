/**
 * CategoryManager.tsx
 *
 * Component for managing finding categories. Allows users to:
 * - View all categories (built-in + custom)
 * - Create, edit, and delete custom categories
 * - Hide/show categories
 * - Reorder categories using up/down buttons
 * - Reset to default configuration
 */

import { useState, useEffect, useCallback } from "react";
import {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
  Plus,
  Trash2,
  Edit3,
  Save,
  X,
  ChevronUp,
  ChevronDown,
  Eye,
  EyeOff,
  RotateCcw,
  Lock,
} from "lucide-react";
import { getAccentColors } from "@/design-system";
import type { FindingCategory, ActionType } from "../../types/findings";
import {
  getAllCategories,
  addCustomCategory,
  updateCustomCategory,
  deleteCustomCategory,
  setCategoryVisibility,
  setCategoryOrder,
  resetCategories,
  getCategoryColorClasses,
} from "../../services/FindingCategories";

// Map icon names to Lucide components
const iconMap: Record<string, React.ComponentType<{ className?: string }>> = {
  Bug,
  CheckSquare,
  Shield,
  Settings,
  CheckCircle,
  Info,
  Database,
  Activity,
  TestTube,
  Sparkles,
  FileText,
  Zap,
  AlertTriangle,
};

// Available icon names for selection
const availableIcons = Object.keys(iconMap);

// Available colors for selection
const availableColors = [
  { name: "red", label: "Red" },
  { name: "amber", label: "Amber" },
  { name: "orange", label: "Orange" },
  { name: "yellow", label: "Yellow" },
  { name: "green", label: "Green" },
  { name: "blue", label: "Blue" },
  { name: "purple", label: "Purple" },
  { name: "cyan", label: "Cyan" },
  { name: "slate", label: "Slate" },
];

// Action types with labels
const actionTypes: { value: ActionType; label: string; description: string }[] = [
  { value: "auto_fix", label: "Auto Fix", description: "Can be fixed by AI automatically" },
  { value: "needs_user_input", label: "Needs Input", description: "Requires user decision" },
  { value: "manual", label: "Manual", description: "User must handle manually" },
  { value: "informational", label: "Informational", description: "No action needed" },
];

/** Resolve an icon name to its Lucide component, defaulting to Info. */
function getIconComponent(iconName: string): React.ComponentType<{ className?: string }> {
  return iconMap[iconName] || Info;
}

interface CategoryManagerProps {
  onLog?: (level: "info" | "warning" | "error" | "success", message: string) => void;
}

export function CategoryManager({ onLog }: CategoryManagerProps) {
  // Category list state
  const [categories, setCategories] = useState<FindingCategory[]>([]);
  const [hiddenCategories, setHiddenCategories] = useState<Set<string>>(new Set());

  // Editing state
  const [editingId, setEditingId] = useState<string | null>(null);
  const [isCreating, setIsCreating] = useState(false);

  // Form state
  const [formId, setFormId] = useState("");
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [formIcon, setFormIcon] = useState("Bug");
  const [formColor, setFormColor] = useState("blue");
  const [formActionType, setFormActionType] = useState<ActionType>("auto_fix");

  const loadCategories = useCallback(() => {
    const allCategories = getAllCategories();
    setCategories(allCategories);

    // Check which categories are hidden by comparing with stored data
    const store = localStorage.getItem("qontinui_finding_categories");
    if (store) {
      try {
        const parsed = JSON.parse(store);
        setHiddenCategories(new Set(parsed.hiddenCategories || []));
      } catch {
        setHiddenCategories(new Set());
      }
    }
  }, []);

  // Load categories on mount
  useEffect(() => {
    loadCategories();
  }, [loadCategories]);

  const log = useCallback(
    (level: "info" | "warning" | "error" | "success", message: string) => {
      if (onLog) {
        onLog(level, message);
      } else {
        console.log(`[${level}] ${message}`);
      }
    },
    [onLog],
  );

  const resetForm = useCallback(() => {
    setFormId("");
    setFormName("");
    setFormDescription("");
    setFormIcon("Bug");
    setFormColor("blue");
    setFormActionType("auto_fix");
  }, []);

  const handleCreate = useCallback(() => {
    if (!formId.trim()) {
      log("warning", "Category ID is required");
      return;
    }
    if (!formName.trim()) {
      log("warning", "Category name is required");
      return;
    }

    try {
      addCustomCategory({
        id: formId.trim().toLowerCase().replace(/\s+/g, "_"),
        name: formName.trim(),
        description: formDescription.trim(),
        icon: formIcon,
        color: formColor,
        defaultActionType: formActionType,
      });
      log("success", `Created category: ${formName}`);
      resetForm();
      setIsCreating(false);
      loadCategories();
    } catch (error) {
      log("error", `Failed to create category: ${error}`);
    }
  }, [
    formId,
    formName,
    formDescription,
    formIcon,
    formColor,
    formActionType,
    log,
    resetForm,
    loadCategories,
  ]);

  const handleUpdate = useCallback(
    (id: string) => {
      try {
        updateCustomCategory(id, {
          name: formName.trim(),
          description: formDescription.trim(),
          icon: formIcon,
          color: formColor,
          defaultActionType: formActionType,
        });
        log("success", `Updated category: ${formName}`);
        setEditingId(null);
        loadCategories();
      } catch (error) {
        log("error", `Failed to update category: ${error}`);
      }
    },
    [formName, formDescription, formIcon, formColor, formActionType, log, loadCategories],
  );

  const handleDelete = useCallback(
    (id: string, name: string) => {
      if (
        !confirm(
          `Delete category "${name}"? Findings using this category will become uncategorized.`,
        )
      ) {
        return;
      }

      try {
        deleteCustomCategory(id);
        log("success", `Deleted category: ${name}`);
        loadCategories();
      } catch (error) {
        log("error", `Failed to delete category: ${error}`);
      }
    },
    [log, loadCategories],
  );

  const handleToggleVisibility = useCallback(
    (id: string, currentlyHidden: boolean) => {
      try {
        setCategoryVisibility(id, currentlyHidden); // If hidden, make visible; if visible, hide
        loadCategories();
      } catch (error) {
        log("error", `Failed to toggle visibility: ${error}`);
      }
    },
    [log, loadCategories],
  );

  const handleMoveUp = useCallback(
    (index: number) => {
      if (index <= 0) return;
      const newOrder = [...categories];
      [newOrder[index - 1], newOrder[index]] = [newOrder[index], newOrder[index - 1]];
      setCategoryOrder(newOrder.map((c) => c.id));
      loadCategories();
    },
    [categories, loadCategories],
  );

  const handleMoveDown = useCallback(
    (index: number) => {
      if (index >= categories.length - 1) return;
      const newOrder = [...categories];
      [newOrder[index], newOrder[index + 1]] = [newOrder[index + 1], newOrder[index]];
      setCategoryOrder(newOrder.map((c) => c.id));
      loadCategories();
    },
    [categories, loadCategories],
  );

  const handleResetToDefaults = useCallback(() => {
    if (
      !confirm(
        "Reset all categories to defaults? This will remove custom categories and restore default order.",
      )
    ) {
      return;
    }
    resetCategories();
    log("success", "Categories reset to defaults");
    loadCategories();
  }, [log, loadCategories]);

  const startEditing = useCallback((category: FindingCategory) => {
    setFormName(category.name);
    setFormDescription(category.description);
    setFormIcon(category.icon);
    setFormColor(category.color);
    setFormActionType(category.defaultActionType);
    setEditingId(category.id);
    setIsCreating(false);
  }, []);

  return (
    <div className="h-full flex flex-col p-4">
      {/* Header */}
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-xl font-semibold">Finding Categories</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Configure how findings are categorized and displayed
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={handleResetToDefaults}
            className="flex items-center gap-2 px-3 py-2 text-sm border border-border rounded-lg hover:bg-muted transition-colors"
            title="Reset to defaults"
          >
            <RotateCcw className="w-4 h-4" />
            Reset
          </button>
          <button
            onClick={() => {
              resetForm();
              setIsCreating(true);
              setEditingId(null);
            }}
            className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
          >
            <Plus className="w-4 h-4" />
            New Category
          </button>
        </div>
      </div>

      {/* Create Form */}
      {isCreating && (
        <div className="card p-4 mb-4 border-2 border-primary/30">
          <h3 className="text-lg font-medium mb-4">Create New Category</h3>
          <CategoryForm
            formId={formId}
            setFormId={setFormId}
            formName={formName}
            setFormName={setFormName}
            formDescription={formDescription}
            setFormDescription={setFormDescription}
            formIcon={formIcon}
            setFormIcon={setFormIcon}
            formColor={formColor}
            setFormColor={setFormColor}
            formActionType={formActionType}
            setFormActionType={setFormActionType}
            isNew={true}
            onSave={handleCreate}
            onCancel={() => {
              resetForm();
              setIsCreating(false);
            }}
          />
        </div>
      )}

      {/* Category List */}
      <div className="flex-1 overflow-y-auto space-y-2">
        {categories.length === 0 ? (
          <div className="text-center text-muted-foreground py-8">
            No categories found. Create one to get started!
          </div>
        ) : (
          categories.map((category, index) => {
            const isHidden = hiddenCategories.has(category.id);
            const isBuiltIn = category.isBuiltIn;
            const isEditing = editingId === category.id;
            const colorClasses = getCategoryColorClasses(category.color);
            const Icon = getIconComponent(category.icon);

            return (
              <div
                key={category.id}
                className={`rounded-lg border ${isHidden ? "opacity-50" : ""} ${
                  isEditing ? "border-primary/50" : "border-border"
                } bg-card overflow-hidden transition-all`}
              >
                {/* Category Row */}
                <div className="p-3">
                  <div className="flex items-center gap-3">
                    {/* Reorder Buttons */}
                    <div className="flex flex-col gap-0.5">
                      <button
                        onClick={() => handleMoveUp(index)}
                        disabled={index === 0}
                        className="p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                        title="Move up"
                      >
                        <ChevronUp className="w-4 h-4" />
                      </button>
                      <button
                        onClick={() => handleMoveDown(index)}
                        disabled={index === categories.length - 1}
                        className="p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                        title="Move down"
                      >
                        <ChevronDown className="w-4 h-4" />
                      </button>
                    </div>

                    {/* Icon */}
                    <div className={`flex-shrink-0 p-2 rounded-lg ${colorClasses.bg}`}>
                      <Icon className={`w-4 h-4 ${colorClasses.text}`} />
                    </div>

                    {/* Content */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <h4 className="font-medium text-sm">{category.name}</h4>
                        {isBuiltIn && (
                          <span className="flex items-center gap-1 px-1.5 py-0.5 text-[10px] bg-muted text-muted-foreground rounded">
                            <Lock className="w-2.5 h-2.5" />
                            Built-in
                          </span>
                        )}
                        <span
                          className={`px-1.5 py-0.5 text-[10px] rounded ${colorClasses.bg} ${colorClasses.text}`}
                        >
                          {category.color}
                        </span>
                        <span className="px-1.5 py-0.5 text-[10px] bg-muted text-muted-foreground rounded">
                          {actionTypes.find((a) => a.value === category.defaultActionType)?.label}
                        </span>
                      </div>
                      <p className="text-xs text-muted-foreground mt-0.5 truncate">
                        {category.description}
                      </p>
                      <p className="text-[10px] text-muted-foreground/70 mt-0.5">
                        ID: {category.id}
                      </p>
                    </div>

                    {/* Actions */}
                    <div className="flex items-center gap-1 flex-shrink-0">
                      {/* Visibility Toggle */}
                      <button
                        onClick={() => handleToggleVisibility(category.id, isHidden)}
                        className={`p-2 rounded-lg transition-colors ${
                          isHidden
                            ? "text-muted-foreground hover:text-foreground hover:bg-muted"
                            : "text-foreground hover:bg-muted"
                        }`}
                        title={isHidden ? "Show category" : "Hide category"}
                      >
                        {isHidden ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                      </button>

                      {/* Edit Button (custom only) */}
                      {!isBuiltIn && (
                        <button
                          onClick={() => {
                            if (isEditing) {
                              setEditingId(null);
                            } else {
                              startEditing(category);
                            }
                          }}
                          className="p-2 hover:bg-muted rounded-lg transition-colors"
                          title={isEditing ? "Cancel editing" : "Edit category"}
                        >
                          {isEditing ? (
                            <X className="w-4 h-4" />
                          ) : (
                            <Edit3 className="w-4 h-4 text-muted-foreground" />
                          )}
                        </button>
                      )}

                      {/* Delete Button (custom only) */}
                      {!isBuiltIn && (
                        <button
                          onClick={() => handleDelete(category.id, category.name)}
                          className={`p-2 hover:${getAccentColors("red").bg} rounded-lg transition-colors`}
                          title="Delete category"
                        >
                          <Trash2 className={`w-4 h-4 ${getAccentColors("red").text}`} />
                        </button>
                      )}
                    </div>
                  </div>
                </div>

                {/* Edit Form */}
                {isEditing && (
                  <div className="px-3 pb-3 border-t border-border pt-3">
                    <CategoryForm
                      formId={category.id}
                      setFormId={() => {}}
                      formName={formName}
                      setFormName={setFormName}
                      formDescription={formDescription}
                      setFormDescription={setFormDescription}
                      formIcon={formIcon}
                      setFormIcon={setFormIcon}
                      formColor={formColor}
                      setFormColor={setFormColor}
                      formActionType={formActionType}
                      setFormActionType={setFormActionType}
                      isNew={false}
                      onSave={() => handleUpdate(category.id)}
                      onCancel={() => setEditingId(null)}
                    />
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>

      {/* Legend */}
      <div className="mt-4 pt-4 border-t border-border">
        <div className="flex flex-wrap gap-4 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <Lock className="w-3 h-3" />
            Built-in categories cannot be edited or deleted
          </span>
          <span className="flex items-center gap-1">
            <EyeOff className="w-3 h-3" />
            Hidden categories are not shown in reports
          </span>
        </div>
      </div>
    </div>
  );
}

// Category form component
interface CategoryFormProps {
  formId: string;
  setFormId: (value: string) => void;
  formName: string;
  setFormName: (value: string) => void;
  formDescription: string;
  setFormDescription: (value: string) => void;
  formIcon: string;
  setFormIcon: (value: string) => void;
  formColor: string;
  setFormColor: (value: string) => void;
  formActionType: ActionType;
  setFormActionType: (value: ActionType) => void;
  isNew: boolean;
  onSave: () => void;
  onCancel: () => void;
}

function CategoryForm({
  formId,
  setFormId,
  formName,
  setFormName,
  formDescription,
  setFormDescription,
  formIcon,
  setFormIcon,
  formColor,
  setFormColor,
  formActionType,
  setFormActionType,
  isNew,
  onSave,
  onCancel,
}: CategoryFormProps) {
  const colorClasses = getCategoryColorClasses(formColor);
  const Icon = iconMap[formIcon] || Info;

  // Auto-generate ID from name for new categories
  const handleNameChange = (name: string) => {
    setFormName(name);
    if (isNew) {
      setFormId(
        name
          .toLowerCase()
          .replace(/\s+/g, "_")
          .replace(/[^a-z0-9_]/g, ""),
      );
    }
  };

  return (
    <div className="space-y-4">
      {/* Preview */}
      <div className="flex items-center gap-3 p-3 rounded-lg border border-dashed border-border bg-muted/30">
        <div className={`flex-shrink-0 p-2 rounded-lg ${colorClasses.bg}`}>
          <Icon className={`w-4 h-4 ${colorClasses.text}`} />
        </div>
        <div>
          <span className="text-sm font-medium">{formName || "Category Name"}</span>
          <p className="text-xs text-muted-foreground">
            {formDescription || "Category description"}
          </p>
        </div>
      </div>

      {/* ID and Name */}
      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium mb-1">
            ID {isNew && <span className="text-muted-foreground">(auto-generated)</span>}
          </label>
          <input
            type="text"
            value={formId}
            onChange={(e) => setFormId(e.target.value)}
            disabled={!isNew}
            placeholder="category_id"
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50 disabled:opacity-50 disabled:cursor-not-allowed font-mono text-sm"
          />
        </div>
        <div>
          <label className="block text-sm font-medium mb-1">Name</label>
          <input
            type="text"
            value={formName}
            onChange={(e) => handleNameChange(e.target.value)}
            placeholder="Category Name"
            className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
          />
        </div>
      </div>

      {/* Description */}
      <div>
        <label className="block text-sm font-medium mb-1">Description</label>
        <input
          type="text"
          value={formDescription}
          onChange={(e) => setFormDescription(e.target.value)}
          placeholder="Brief description of what this category is for"
          className="w-full px-3 py-2 bg-background border border-border rounded-lg focus:outline-none focus:ring-2 focus:ring-primary/50"
        />
      </div>

      {/* Icon and Color */}
      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium mb-1">Icon</label>
          <div className="flex flex-wrap gap-1 p-2 border border-border rounded-lg bg-background max-h-24 overflow-y-auto">
            {availableIcons.map((iconName) => {
              const IconOption = iconMap[iconName];
              const isSelected = formIcon === iconName;
              return (
                <button
                  key={iconName}
                  onClick={() => setFormIcon(iconName)}
                  className={`p-2 rounded transition-colors ${
                    isSelected
                      ? `${colorClasses.bg} ${colorClasses.text}`
                      : "hover:bg-muted text-muted-foreground"
                  }`}
                  title={iconName}
                >
                  <IconOption className="w-4 h-4" />
                </button>
              );
            })}
          </div>
        </div>
        <div>
          <label className="block text-sm font-medium mb-1">Color</label>
          <div className="flex flex-wrap gap-1 p-2 border border-border rounded-lg bg-background">
            {availableColors.map((color) => {
              const colorOption = getCategoryColorClasses(color.name);
              const isSelected = formColor === color.name;
              return (
                <button
                  key={color.name}
                  onClick={() => setFormColor(color.name)}
                  className={`px-2 py-1 rounded text-xs font-medium transition-colors ${
                    isSelected
                      ? `${colorOption.badgeBg} text-white ring-2 ring-offset-2 ring-offset-background ring-${color.name}-500`
                      : `${colorOption.bg} ${colorOption.text} hover:opacity-80`
                  }`}
                  title={color.label}
                >
                  {color.label}
                </button>
              );
            })}
          </div>
        </div>
      </div>

      {/* Action Type */}
      <div>
        <label className="block text-sm font-medium mb-1">Default Action Type</label>
        <div className="grid grid-cols-2 gap-2">
          {actionTypes.map((action) => (
            <button
              key={action.value}
              onClick={() => setFormActionType(action.value)}
              className={`p-2 text-left rounded-lg border transition-colors ${
                formActionType === action.value
                  ? "border-primary bg-primary/10"
                  : "border-border hover:bg-muted"
              }`}
            >
              <div className="text-sm font-medium">{action.label}</div>
              <div className="text-xs text-muted-foreground">{action.description}</div>
            </button>
          ))}
        </div>
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-3 pt-2">
        <button
          onClick={onCancel}
          className="px-4 py-2 border border-border rounded-lg hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={onSave}
          disabled={!formName.trim() || (isNew && !formId.trim())}
          className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <Save className="w-4 h-4" />
          {isNew ? "Create Category" : "Save Changes"}
        </button>
      </div>
    </div>
  );
}
