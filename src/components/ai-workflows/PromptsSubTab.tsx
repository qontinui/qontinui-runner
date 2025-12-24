/**
 * PromptsSubTab.tsx
 *
 * Prompts sub-tab - wraps PromptLibraryTab functionality
 * for managing saved prompts.
 */

import { PromptLibraryTab } from "../PromptLibraryTab";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

interface PromptsSubTabProps {
  onLog: (level: LogLevel, message: string) => void;
}

export function PromptsSubTab({ onLog }: PromptsSubTabProps) {
  return <PromptLibraryTab onLog={onLog} />;
}
