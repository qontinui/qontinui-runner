/**
 * SpecEditorModal — Modal for editing spec IR documents
 *
 * Allows users to edit spec metadata (name, description, etc.) and view/edit
 * the raw IR JSON structure. Saves via POST /apps/:app_id/spec/author.
 *
 * Features:
 * - Form fields for common spec properties
 * - JSON preview of the full spec
 * - Client-side validation
 * - Server error handling and display
 * - Auto-close on successful save
 */

import { useState, useEffect, useCallback } from "react";
import { X, AlertCircle, CheckCircle2, Loader } from "lucide-react";
import type { LoadedSpec } from "./types";
import { useSpecAuthor } from "./useSpecAuthor";

interface SpecEditorModalProps {
  open: boolean;
  spec: LoadedSpec | null;
  appId: string;
  onClose: () => void;
  onSaveSuccess?: () => void;
}

interface FormData {
  id: string;
  name: string;
  description?: string;
  purpose?: string;
  tags?: string;
  notes?: string;
}

export function SpecEditorModal({
  open,
  spec,
  appId,
  onClose,
  onSaveSuccess,
}: SpecEditorModalProps) {
  const [formData, setFormData] = useState<FormData>({
    id: "",
    name: "",
    description: "",
    purpose: "",
    tags: "",
    notes: "",
  });

  const [errors, setErrors] = useState<Record<string, string>>({});
  const [showJsonEditor, setShowJsonEditor] = useState(false);
  const [jsonText, setJsonText] = useState("");
  const [jsonParseError, setJsonParseError] = useState<string | null>(null);

  const { saveSpec, isLoading, error: saveError } = useSpecAuthor(appId);

  // Initialize form when spec changes
  useEffect(() => {
    if (spec && spec.kind === "page-spec") {
      const config = spec.config as any;
      setFormData({
        id: spec.specId,
        name: config.name || "",
        description: config.description || "",
        purpose: config.metadata?.purpose || "",
        tags: config.metadata?.tags?.join(", ") || "",
        notes: config.metadata?.notes || "",
      });
      // Initialize JSON view with full spec
      setJsonText(JSON.stringify(config, null, 2));
      setJsonParseError(null);
    }
  }, [spec, open]);

  const validateForm = useCallback((): boolean => {
    const newErrors: Record<string, string> = {};

    if (!formData.id.trim()) {
      newErrors.id = "Spec ID is required";
    }
    if (!formData.name.trim()) {
      newErrors.name = "Spec name is required";
    }

    setErrors(newErrors);
    return Object.keys(newErrors).length === 0;
  }, [formData]);

  const validateJson = useCallback((): { valid: boolean; parsed?: any } => {
    try {
      const parsed = JSON.parse(jsonText);
      setJsonParseError(null);
      return { valid: true, parsed };
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Invalid JSON";
      setJsonParseError(`JSON parse error: ${msg}`);
      return { valid: false };
    }
  }, [jsonText]);

  const handleSave = useCallback(async () => {
    // Determine which data to use
    if (showJsonEditor) {
      // Validate JSON first
      const { valid, parsed } = validateJson();
      if (!valid) return;

      // Save the parsed JSON directly
      try {
        await saveSpec(parsed);
        setFormData({
          id: "",
          name: "",
          description: "",
          purpose: "",
          tags: "",
          notes: "",
        });
        setJsonText("");
        onSaveSuccess?.();
        onClose();
      } catch {
        // Error is handled by useSpecAuthor hook
      }
    } else {
      // Validate form
      if (!validateForm()) return;

      // Build spec object from form data
      const specToSave: any = {
        version: "1.0",
        id: formData.id,
        name: formData.name,
        description: formData.description,
      };

      if (spec?.kind === "page-spec") {
        const config = spec.config as any;
        // Preserve existing structure
        specToSave.metadata = {
          ...config.metadata,
          description: formData.description,
          purpose: formData.purpose || undefined,
          tags: formData.tags
            ? formData.tags.split(",").map((t) => t.trim())
            : undefined,
          notes: formData.notes || undefined,
        };
        specToSave.states = config.states;
        specToSave.transitions = config.transitions;
        specToSave.groups = config.groups;
        specToSave.provenance = config.provenance;
      }

      try {
        await saveSpec(specToSave);
        setFormData({
          id: "",
          name: "",
          description: "",
          purpose: "",
          tags: "",
          notes: "",
        });
        setJsonText("");
        onSaveSuccess?.();
        onClose();
      } catch {
        // Error is handled by useSpecAuthor hook
      }
    }
  }, [spec, formData, showJsonEditor, jsonText, saveSpec, validateForm, validateJson, onClose, onSaveSuccess]);

  if (!open || !spec) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-background border border-border rounded-lg shadow-xl max-w-2xl w-full mx-4 max-h-[90vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border shrink-0">
          <h2 className="text-lg font-semibold">Edit Spec</h2>
          <button
            onClick={onClose}
            disabled={isLoading}
            className="p-1 hover:bg-white/10 rounded transition-colors disabled:opacity-50"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex gap-2 px-4 pt-4 border-b border-border shrink-0">
          <button
            onClick={() => setShowJsonEditor(false)}
            className={`px-3 py-2 text-sm font-medium rounded-t transition-colors ${
              !showJsonEditor
                ? "bg-background text-foreground border border-b-0 border-border"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            Metadata
          </button>
          <button
            onClick={() => setShowJsonEditor(true)}
            className={`px-3 py-2 text-sm font-medium rounded-t transition-colors ${
              showJsonEditor
                ? "bg-background text-foreground border border-b-0 border-border"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            JSON
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {!showJsonEditor ? (
            <div className="space-y-4">
              {/* ID Field - Read-only */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Spec ID
                </label>
                <input
                  type="text"
                  value={formData.id}
                  disabled
                  className="w-full px-3 py-2 bg-white/5 border border-border rounded text-muted-foreground text-sm"
                />
                <p className="text-xs text-muted-foreground/60 mt-1">
                  Spec ID cannot be changed
                </p>
              </div>

              {/* Name Field */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Spec Name *
                </label>
                <input
                  type="text"
                  value={formData.name}
                  onChange={(e) => {
                    setFormData({ ...formData, name: e.target.value });
                    if (errors.name) {
                      setErrors({ ...errors, name: "" });
                    }
                  }}
                  placeholder="e.g., Dashboard page spec"
                  className={`w-full px-3 py-2 bg-white/5 border rounded text-sm transition-colors ${
                    errors.name
                      ? "border-red-500/50 focus:border-red-500"
                      : "border-border focus:border-blue-500"
                  }`}
                />
                {errors.name && (
                  <p className="text-xs text-red-500 mt-1">{errors.name}</p>
                )}
              </div>

              {/* Description Field */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Description
                </label>
                <textarea
                  value={formData.description}
                  onChange={(e) =>
                    setFormData({ ...formData, description: e.target.value })
                  }
                  placeholder="Describe what this spec covers"
                  rows={3}
                  className="w-full px-3 py-2 bg-white/5 border border-border rounded text-sm focus:border-blue-500 transition-colors"
                />
              </div>

              {/* Purpose Field */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Purpose
                </label>
                <input
                  type="text"
                  value={formData.purpose}
                  onChange={(e) =>
                    setFormData({ ...formData, purpose: e.target.value })
                  }
                  placeholder="e.g., Verify dashboard layout and content"
                  className="w-full px-3 py-2 bg-white/5 border border-border rounded text-sm focus:border-blue-500 transition-colors"
                />
              </div>

              {/* Tags Field */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Tags (comma-separated)
                </label>
                <input
                  type="text"
                  value={formData.tags}
                  onChange={(e) =>
                    setFormData({ ...formData, tags: e.target.value })
                  }
                  placeholder="e.g., ui, dashboard, layout"
                  className="w-full px-3 py-2 bg-white/5 border border-border rounded text-sm focus:border-blue-500 transition-colors"
                />
              </div>

              {/* Notes Field */}
              <div>
                <label className="block text-sm font-medium text-foreground mb-1">
                  Notes
                </label>
                <textarea
                  value={formData.notes}
                  onChange={(e) =>
                    setFormData({ ...formData, notes: e.target.value })
                  }
                  placeholder="Additional notes about this spec"
                  rows={2}
                  className="w-full px-3 py-2 bg-white/5 border border-border rounded text-sm focus:border-blue-500 transition-colors"
                />
              </div>
            </div>
          ) : (
            <div className="space-y-2">
              {jsonParseError && (
                <div className="flex items-start gap-2 p-3 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-500">
                  <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
                  <p>{jsonParseError}</p>
                </div>
              )}
              <textarea
                value={jsonText}
                onChange={(e) => {
                  setJsonText(e.target.value);
                  setJsonParseError(null);
                }}
                className="w-full h-96 p-3 bg-white/5 border border-border rounded text-sm font-mono focus:border-blue-500 transition-colors"
              />
              <p className="text-xs text-muted-foreground/60">
                Edit the raw IR (state-machine.derived.json) here. Must be valid JSON.
              </p>
            </div>
          )}
        </div>

        {/* Error Message */}
        {saveError && (
          <div className="flex items-start gap-2 p-3 mx-4 mb-4 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-500">
            <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
            <p>{saveError}</p>
          </div>
        )}

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 p-4 border-t border-border shrink-0">
          <button
            onClick={onClose}
            disabled={isLoading}
            className="px-4 py-2 text-sm font-medium rounded border border-border hover:bg-white/5 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={isLoading}
            className="px-4 py-2 text-sm font-medium rounded bg-blue-500/20 text-blue-400 border border-blue-500/30 hover:bg-blue-500/30 transition-colors disabled:opacity-50 flex items-center gap-2"
          >
            {isLoading ? (
              <>
                <Loader className="w-4 h-4 animate-spin" />
                Saving...
              </>
            ) : (
              <>
                <CheckCircle2 className="w-4 h-4" />
                Save Changes
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
