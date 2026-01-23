/**
 * Test Properties Panel
 *
 * Right panel for editing test metadata and configuration.
 */

import { useState, useEffect } from "react";
import { Settings, Save, Clock, AlertTriangle, Tag, FileText, Power, Sparkles, Loader2, Wand2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useTestBuilder } from "./TestBuilderContext";
import type { TestCategory, CreateTestInput } from "./types";
import { getStatusColors } from "@/design-system";

// Category options
const categoryOptions: { value: TestCategory; label: string }[] = [
  { value: "visual", label: "Visual" },
  { value: "dom", label: "DOM" },
  { value: "network", label: "Network" },
  { value: "data", label: "Data" },
  { value: "log", label: "Log" },
  { value: "layout", label: "Layout" },
  { value: "unit", label: "Unit" },
  { value: "integration", label: "Integration" },
  { value: "custom", label: "Custom" },
];

interface TestPropertiesPanelProps {
  code: string;
  onSave: () => void;
}

export function TestPropertiesPanel({ code, onSave }: TestPropertiesPanelProps) {
  const { selectedTest, state, updateTest, setDirty, isDraftSelected, saveDraft, updateDraft } =
    useTestBuilder();

  // Local form state
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState<TestCategory>("custom");
  const [timeoutSeconds, setTimeoutSeconds] = useState(60);
  const [isCritical, setIsCritical] = useState(false);
  const [enabled, setEnabled] = useState(true);
  const [tags, setTags] = useState<string[]>([]);
  const [isFillingWithAI, setIsFillingWithAI] = useState(false);
  const [tagInput, setTagInput] = useState("");

  // Sync local state with selected test
  // Only re-sync when the test ID changes (not on every render)
  // selectedTest is recreated as a new object each render, so we use the ID
  useEffect(() => {
    if (selectedTest) {
      setName(selectedTest.name);
      setDescription(selectedTest.description || "");
      setCategory(selectedTest.category || "custom");
      setTimeoutSeconds(selectedTest.timeout_seconds);
      setIsCritical(selectedTest.is_critical);
      setEnabled(selectedTest.enabled);
      setTags(selectedTest.tags || []);
    }
  }, [selectedTest?.id]);

  // Handle name change
  const handleNameChange = (value: string) => {
    setName(value);
    setDirty(true);
  };

  // Handle description change
  const handleDescriptionChange = (value: string) => {
    setDescription(value);
    setDirty(true);
  };

  // Handle tag add
  const handleAddTag = () => {
    if (tagInput.trim() && !tags.includes(tagInput.trim())) {
      setTags([...tags, tagInput.trim()]);
      setTagInput("");
      setDirty(true);
    }
  };

  // Handle tag remove
  const handleRemoveTag = (tag: string) => {
    setTags(tags.filter((t) => t !== tag));
    setDirty(true);
  };

  // Handle fill with AI - analyze test code to generate metadata
  const handleFillWithAI = async () => {
    if (!code.trim()) return;

    setIsFillingWithAI(true);
    try {
      const response = await invoke<{
        success: boolean;
        data?: {
          name?: string;
          description?: string;
          category?: TestCategory;
          tags?: string[];
        };
        message?: string;
      }>("generate_test_metadata", {
        testCode: code,
        testType: selectedTest?.test_type || "python_script",
      });

      if (response.success && response.data) {
        if (response.data.name) {
          setName(response.data.name);
        }
        if (response.data.description) {
          setDescription(response.data.description);
        }
        if (response.data.category) {
          setCategory(response.data.category);
        }
        if (response.data.tags && response.data.tags.length > 0) {
          // Merge with existing tags, avoiding duplicates
          const newTags = [...new Set([...tags, ...response.data.tags])];
          setTags(newTags);
        }
        setDirty(true);
      }
    } catch (err) {
      console.error("Failed to generate metadata with AI:", err);
    } finally {
      setIsFillingWithAI(false);
    }
  };

  // Handle save
  const handleSave = async () => {
    if (!selectedTest) return;

    // Build the update input based on test type
    const input: CreateTestInput = {
      name,
      description: description || undefined,
      test_type: selectedTest.test_type,
      category,
      timeout_seconds: timeoutSeconds,
      is_critical: isCritical,
      enabled,
      tags,
    };

    // Add the code based on test type
    switch (selectedTest.test_type) {
      case "playwright_cdp":
        input.playwright_code = code;
        break;
      case "qontinui_vision":
        try {
          input.vision_config = JSON.parse(code);
        } catch {
          // Keep existing if JSON is invalid
          input.vision_config = selectedTest.vision_config;
        }
        break;
      case "python_script":
        input.python_code = code;
        break;
      case "repository_test":
        try {
          input.repo_test_config = JSON.parse(code);
        } catch {
          // Keep existing if JSON is invalid
          input.repo_test_config = selectedTest.repo_test_config;
        }
        break;
    }

    // If this is a draft, save it as a new test; otherwise update existing
    if (isDraftSelected) {
      // Pass input directly to saveDraft to avoid race condition with state updates
      await saveDraft(input);
    } else {
      await updateTest(selectedTest.id, input);
    }
    onSave();
  };

  // Show placeholder when no test selected
  if (!selectedTest) {
    return (
      <div className="h-full flex flex-col items-center justify-center text-muted-foreground bg-card border-l border-border/50 w-72">
        <Settings className="w-12 h-12 mb-4 opacity-20" />
        <p className="text-sm">Select a test to edit properties</p>
      </div>
    );
  }

  return (
    <div className="h-full flex flex-col bg-card border-l border-border/50 w-72 flex-shrink-0">
      {/* Header */}
      <div className="p-4 border-b border-border/50 flex items-center justify-between">
        <h3 className="text-sm font-semibold flex items-center gap-2">
          <Settings className="w-4 h-4" />
          Properties
          {isDraftSelected && (
            <span className="px-1.5 py-0.5 text-xs bg-amber-500/20 text-amber-400 rounded">
              Draft
            </span>
          )}
        </h3>
        <button
          className={`
            px-3 py-1.5 text-sm font-medium rounded-md flex items-center gap-1.5
            transition-colors
            ${
              state.isDirty && !state.isSaving
                ? "bg-primary text-primary-foreground hover:bg-primary/90"
                : "bg-muted text-muted-foreground"
            }
          `}
          onClick={handleSave}
          disabled={!state.isDirty || state.isSaving}
          data-ui-id="test-builder-save-btn"
        >
          <Save className="w-3.5 h-3.5" />
          {state.isSaving ? "Saving..." : isDraftSelected ? "Create" : "Save"}
        </button>
      </div>

      {/* Form */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {/* Name */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Name</label>
          <input
            type="text"
            value={name}
            onChange={(e) => handleNameChange(e.target.value)}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
            data-ui-id="test-builder-name-input"
          />
        </div>

        {/* Description */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <FileText className="w-3 h-3" />
            Description
          </label>
          <textarea
            value={description}
            onChange={(e) => handleDescriptionChange(e.target.value)}
            rows={3}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50 resize-none"
            placeholder="What does this test verify?"
            data-ui-id="test-builder-description-input"
          />
        </div>

        {/* Category */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground">Category</label>
          <select
            value={category}
            onChange={(e) => {
              setCategory(e.target.value as TestCategory);
              setDirty(true);
            }}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
            data-ui-id="test-builder-category-select"
          >
            {categoryOptions.map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
        </div>

        {/* Timeout */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Clock className="w-3 h-3" />
            Timeout (seconds)
          </label>
          <input
            type="number"
            value={timeoutSeconds}
            onChange={(e) => {
              setTimeoutSeconds(parseInt(e.target.value) || 60);
              setDirty(true);
            }}
            min={1}
            max={600}
            className="w-full px-3 py-2 text-sm bg-muted/30 border border-border/50 rounded-md
                     focus:outline-none focus:ring-2 focus:ring-primary/50"
            data-ui-id="test-builder-timeout-input"
          />
        </div>

        {/* Toggles */}
        <div className="space-y-3">
          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-sm flex items-center gap-2">
              <AlertTriangle className={`w-4 h-4 ${getStatusColors("warning").icon}`} />
              Critical Test
            </span>
            <input
              type="checkbox"
              checked={isCritical}
              onChange={(e) => {
                setIsCritical(e.target.checked);
                setDirty(true);
              }}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-primary/50"
              data-ui-id="test-builder-critical-checkbox"
            />
          </label>
          <p className="text-xs text-muted-foreground ml-6">
            Critical test failure will fail the entire task run
          </p>

          <label className="flex items-center justify-between cursor-pointer">
            <span className="text-sm flex items-center gap-2">
              <Power className="w-4 h-4" />
              Enabled
            </span>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => {
                setEnabled(e.target.checked);
                setDirty(true);
              }}
              className="w-4 h-4 rounded border-border focus:ring-2 focus:ring-primary/50"
              data-ui-id="test-builder-enabled-checkbox"
            />
          </label>
        </div>

        {/* Fill with AI Button - only show when there's code */}
        {code.trim() && (
          <button
            onClick={handleFillWithAI}
            disabled={isFillingWithAI}
            className={`
              w-full px-3 py-2 text-sm font-medium rounded-md flex items-center justify-center gap-2
              transition-colors border border-border/50
              ${
                isFillingWithAI
                  ? "bg-muted/30 text-muted-foreground cursor-not-allowed"
                  : "bg-primary/10 text-primary hover:bg-primary/20"
              }
            `}
            data-ui-id="test-builder-fill-ai-btn"
          >
            {isFillingWithAI ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Analyzing...
              </>
            ) : (
              <>
                <Wand2 className="w-4 h-4" />
                Fill with AI
              </>
            )}
          </button>
        )}

        {/* Tags */}
        <div className="space-y-1.5">
          <label className="text-xs font-medium text-muted-foreground flex items-center gap-1">
            <Tag className="w-3 h-3" />
            Tags
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  handleAddTag();
                }
              }}
              className="flex-1 px-3 py-1.5 text-sm bg-muted/30 border border-border/50 rounded-md
                       focus:outline-none focus:ring-2 focus:ring-primary/50"
              placeholder="Add tag..."
              data-ui-id="test-builder-tag-input"
            />
            <button
              onClick={handleAddTag}
              className="px-3 py-1.5 text-sm bg-muted hover:bg-muted/80 rounded-md"
              data-ui-id="test-builder-add-tag-btn"
            >
              Add
            </button>
          </div>
          {tags.length > 0 && (
            <div className="flex flex-wrap gap-1 mt-2">
              {tags.map((tag) => (
                <span
                  key={tag}
                  className="inline-flex items-center gap-1 px-2 py-0.5 text-xs bg-muted rounded-full"
                >
                  {tag}
                  <button onClick={() => handleRemoveTag(tag)} className="hover:text-destructive">
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
        </div>

        {/* Metadata */}
        {selectedTest && (
          <div className="pt-4 border-t border-border/50 space-y-2 text-xs text-muted-foreground">
            <div>
              <span className="font-medium">Created:</span>{" "}
              {new Date(selectedTest.created_at).toLocaleString()}
            </div>
            <div>
              <span className="font-medium">Updated:</span>{" "}
              {new Date(selectedTest.updated_at).toLocaleString()}
            </div>
            {selectedTest.ai_generated && (
              <div className="flex items-center gap-1 text-primary">
                <Sparkles className="w-3 h-3" />
                AI Generated
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
