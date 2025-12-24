/**
 * ScriptsSubTab.tsx
 *
 * Scripts sub-tab - wraps ScriptBuilderTab functionality
 * for managing Playwright scripts.
 */

import { ScriptBuilderTab } from "../ScriptBuilderTab";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface ScriptsSubTabProps {
  onLog: (level: LogLevel, message: string) => void;
}

export function ScriptsSubTab({ onLog }: ScriptsSubTabProps) {
  return <ScriptBuilderTab onLog={onLog} />;
}
