/**
 * ScriptsPage.tsx
 *
 * Container component that hosts both Scripts and Scriptlets in sub-tabs.
 * Scriptlets exist to be inserted into script descriptions, so they belong
 * together on the same page.
 */

import { useState, useEffect } from "react";
import { TestTube, Puzzle } from "lucide-react";
import { ScriptBuilderTab } from "./ScriptBuilderTab";
import { ScriptletBuilderTab } from "./ScriptletBuilderTab";
import { getAccentColors } from "@/design-system";

type ScriptsSubTab = "scripts" | "scriptlets";
type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ScriptsPageProps {
  onLog: (level: LogLevel, message: string) => void;
  editScriptId?: string | null;
  editScriptletId?: string | null;
  onNavigateToLibrary?: () => void;
}

export function ScriptsPage({
  onLog,
  editScriptId,
  editScriptletId,
  onNavigateToLibrary,
}: ScriptsPageProps) {
  const [activeSubTab, setActiveSubTab] = useState<ScriptsSubTab>("scripts");

  // Auto-switch to scriptlets tab when editScriptletId is provided
  useEffect(() => {
    if (editScriptletId) {
      setActiveSubTab("scriptlets");
    }
  }, [editScriptletId]);

  // Auto-switch to scripts tab when editScriptId is provided
  useEffect(() => {
    if (editScriptId) {
      setActiveSubTab("scripts");
    }
  }, [editScriptId]);

  const scriptsAccent = getAccentColors("purple");
  const scriptletsAccent = getAccentColors("cyan");

  return (
    <div className="h-full flex flex-col">
      {/* Sub-tab navigation bar */}
      <div className="flex-shrink-0 border-b border-neutral-700 bg-neutral-900/50">
        <div className="flex items-center gap-1 px-4 py-2">
          <button
            onClick={() => setActiveSubTab("scripts")}
            className={`flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              activeSubTab === "scripts"
                ? "bg-neutral-700 text-white"
                : "text-neutral-400 hover:text-neutral-200 hover:bg-neutral-800"
            }`}
          >
            <TestTube
              className="w-4 h-4"
              style={{ color: activeSubTab === "scripts" ? scriptsAccent.bgSolid : undefined }}
            />
            Scripts
          </button>
          <button
            onClick={() => setActiveSubTab("scriptlets")}
            className={`flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
              activeSubTab === "scriptlets"
                ? "bg-neutral-700 text-white"
                : "text-neutral-400 hover:text-neutral-200 hover:bg-neutral-800"
            }`}
          >
            <Puzzle
              className="w-4 h-4"
              style={{
                color: activeSubTab === "scriptlets" ? scriptletsAccent.bgSolid : undefined,
              }}
            />
            Scriptlets
          </button>
        </div>
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeSubTab === "scripts" ? (
          <ScriptBuilderTab
            onLog={onLog}
            editScriptId={editScriptId}
            onNavigateToLibrary={onNavigateToLibrary}
          />
        ) : (
          <ScriptletBuilderTab
            onLog={onLog}
            editScriptletId={editScriptletId}
            onNavigateToLibrary={onNavigateToLibrary}
          />
        )}
      </div>
    </div>
  );
}
