/**
 * ResultMessage.tsx
 *
 * Displays success/error result messages.
 */

import { CheckCircle, XCircle } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function ResultMessage() {
  const { lastResult } = useAiBuilder();

  if (!lastResult) {
    return null;
  }

  return (
    <div
      className={`flex items-center gap-2 p-3 rounded-md ${
        lastResult.success ? "bg-green-500/10 text-green-500" : "bg-red-500/10 text-red-500"
      }`}
    >
      {lastResult.success ? <CheckCircle className="w-4 h-4" /> : <XCircle className="w-4 h-4" />}
      <span className="text-sm">{lastResult.message}</span>
    </div>
  );
}
