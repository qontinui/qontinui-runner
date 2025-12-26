/**
 * ScriptletsSubTab.tsx
 *
 * Wrapper component for the Scriptlets tab in AI Workflows.
 * Scriptlets are reusable text snippets generated from AI debugging sessions
 * that can be inserted into Playwright script descriptions.
 */

import type { AiOutputLine } from "../AiOutputTab";
import { ScriptletsTab } from "../ScriptletsTab";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ScriptletsSubTabProps {
  onLog: (level: LogLevel, message: string) => void;
  aiOutputLines: AiOutputLine[];
}

export function ScriptletsSubTab({ onLog, aiOutputLines }: ScriptletsSubTabProps) {
  return <ScriptletsTab onLog={onLog} aiOutputLines={aiOutputLines} />;
}
