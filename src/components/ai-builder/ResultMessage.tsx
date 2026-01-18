/**
 * ResultMessage.tsx
 *
 * Displays success/error result messages.
 */

import { CheckCircle, XCircle } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";
import { getStatusColors } from "@/design-system";

export function ResultMessage() {
  const { lastResult } = useAiBuilder();

  if (!lastResult) {
    return null;
  }

  return (
    <div
      className={`flex items-center gap-2 p-3 rounded-md ${
        lastResult.success
          ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
          : `${getStatusColors("error").bg} ${getStatusColors("error").text}`
      }`}
    >
      {lastResult.success ? <CheckCircle className="w-4 h-4" /> : <XCircle className="w-4 h-4" />}
      <span className="text-sm">{lastResult.message}</span>
    </div>
  );
}
