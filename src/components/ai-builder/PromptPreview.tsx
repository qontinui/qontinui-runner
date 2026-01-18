/**
 * PromptPreview.tsx
 *
 * Collapsible panel showing the generated prompt with template editing.
 */

import { Sparkles } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import CollapsiblePanel from "../CollapsiblePanel";
import { DEFAULT_DEVELOPER_PROMPT_TEMPLATE, getDeveloperPromptTemplate } from "./constants";
import { getAccentColors } from "@/design-system";

export function PromptPreview() {
  const {
    currentSessionId,
    generatePrompt,
    isEditingPromptTemplate,
    setIsEditingPromptTemplate,
    editedPromptTemplate,
    setEditedPromptTemplate,
    usingCustomTemplate,
    handleSavePromptTemplate,
    handleResetPromptTemplate,
  } = useAiBuilder();

  return (
    <CollapsiblePanel
      title="Prompt Preview"
      icon={<Sparkles className="w-4 h-4" />}
      defaultCollapsed={currentSessionId !== null}
      storageKey="ai-builder-preview"
      headerExtra={
        <div className="flex items-center gap-2">
          {usingCustomTemplate && (
            <span
              className={`text-xs ${getAccentColors("blue").bg} ${getAccentColors("blue").text} px-2 py-0.5 rounded`}
            >
              Custom
            </span>
          )}
          {!isEditingPromptTemplate ? (
            <button
              onClick={(e) => {
                e.stopPropagation();
                setEditedPromptTemplate(getDeveloperPromptTemplate());
                setIsEditingPromptTemplate(true);
              }}
              className="text-xs text-muted-foreground hover:text-foreground px-2 py-1 rounded hover:bg-muted/50"
              title="Edit prompt template"
            >
              Edit Template
            </button>
          ) : null}
        </div>
      }
    >
      {isEditingPromptTemplate ? (
        <div className="space-y-3">
          <div className="text-xs text-muted-foreground mb-2">
            Edit the developer prompt template. Use placeholders like{" "}
            <code className="bg-muted px-1 rounded">{"{{GOAL}}"}</code>,{" "}
            <code className="bg-muted px-1 rounded">{"{{EXECUTION_STEPS}}"}</code>,{" "}
            <code className="bg-muted px-1 rounded">{"{{DEV_LOGS_ESCAPED}}"}</code>, etc.
          </div>
          <textarea
            value={editedPromptTemplate}
            onChange={(e) => setEditedPromptTemplate(e.target.value)}
            className="w-full h-96 text-xs font-mono bg-background border border-border rounded-md p-3 resize-y"
            placeholder="Enter custom prompt template..."
          />
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <button
                onClick={handleSavePromptTemplate}
                className="px-3 py-1.5 text-xs bg-primary text-primary-foreground rounded hover:bg-primary/90"
              >
                Save Template
              </button>
              <button
                onClick={() => {
                  setIsEditingPromptTemplate(false);
                  setEditedPromptTemplate("");
                }}
                className="px-3 py-1.5 text-xs bg-muted text-muted-foreground rounded hover:bg-muted/80"
              >
                Cancel
              </button>
            </div>
            <button
              onClick={() => {
                handleResetPromptTemplate();
                setEditedPromptTemplate(DEFAULT_DEVELOPER_PROMPT_TEMPLATE);
              }}
              className={`px-3 py-1.5 text-xs ${getAccentColors("orange").text} hover:opacity-80 hover:${getAccentColors("orange").bg} rounded`}
              title="Reset to the hardcoded default template"
            >
              Reset to Default
            </button>
          </div>
        </div>
      ) : (
        <pre className="text-xs bg-background p-3 rounded-md overflow-auto max-h-64 whitespace-pre-wrap">
          {generatePrompt("preview-session")}
        </pre>
      )}
    </CollapsiblePanel>
  );
}
