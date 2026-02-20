/**
 * PlaywrightTestsPage.tsx
 *
 * Container component that hosts both Playwright Tests and Prompt Snippets in sub-tabs.
 * Prompt Snippets exist to be inserted into test descriptions, so they belong
 * together on the same page.
 */

import { useState, useEffect } from "react";
import { TestTube, Puzzle } from "lucide-react";
import { PlaywrightTestBuilderTab } from "./PlaywrightTestBuilderTab";
import { PromptSnippetBuilderTab } from "./PromptSnippetBuilderTab";
import { getAccentColors } from "@/design-system";

type PlaywrightTestsSubTab = "playwright-tests" | "prompt-snippets";
type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface PlaywrightTestsPageProps {
  onLog: (level: LogLevel, message: string) => void;
  editScriptId?: string | null;
  editPromptSnippetId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function PlaywrightTestsPage({
  onLog,
  editScriptId,
  editPromptSnippetId,
  onNavigateToLibrary,
}: PlaywrightTestsPageProps) {
  const [activeSubTab, setActiveSubTab] = useState<PlaywrightTestsSubTab>("playwright-tests");

  // Auto-switch to prompt snippets tab when editPromptSnippetId is provided
  useEffect(() => {
    if (editPromptSnippetId) {
      setActiveSubTab("prompt-snippets");
    }
  }, [editPromptSnippetId]);

  // Auto-switch to playwright tests tab when editScriptId is provided
  useEffect(() => {
    if (editScriptId) {
      setActiveSubTab("playwright-tests");
    }
  }, [editScriptId]);

  const testsAccent = getAccentColors("purple");
  const snippetsAccent = getAccentColors("cyan");

  return (
    <div className="h-full flex flex-col">
      {/* Sub-tab navigation bar */}
      <div className="flex-shrink-0 border-b border-neutral-700 bg-neutral-900/50">
        <div className="flex items-center gap-1 px-4 py-2">
          <button
            onClick={() => setActiveSubTab("playwright-tests")}
            className={`flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              activeSubTab === "playwright-tests"
                ? "bg-neutral-700 text-white"
                : "text-neutral-400 hover:text-neutral-200 hover:bg-neutral-800"
            }`}
          >
            <TestTube
              className="w-4 h-4"
              style={{
                color: activeSubTab === "playwright-tests" ? testsAccent.bgSolid : undefined,
              }}
            />
            Playwright Tests
          </button>
          <button
            onClick={() => setActiveSubTab("prompt-snippets")}
            className={`flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              activeSubTab === "prompt-snippets"
                ? "bg-neutral-700 text-white"
                : "text-neutral-400 hover:text-neutral-200 hover:bg-neutral-800"
            }`}
          >
            <Puzzle
              className="w-4 h-4"
              style={{
                color: activeSubTab === "prompt-snippets" ? snippetsAccent.bgSolid : undefined,
              }}
            />
            Prompt Snippets
          </button>
        </div>
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeSubTab === "playwright-tests" ? (
          <PlaywrightTestBuilderTab
            onLog={onLog}
            editScriptId={editScriptId}
            onNavigateToLibrary={onNavigateToLibrary}
          />
        ) : (
          <PromptSnippetBuilderTab
            onLog={onLog}
            editPromptSnippetId={editPromptSnippetId}
            onNavigateToLibrary={onNavigateToLibrary}
          />
        )}
      </div>
    </div>
  );
}
