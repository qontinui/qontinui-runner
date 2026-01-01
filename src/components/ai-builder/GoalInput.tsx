/**
 * GoalInput.tsx
 *
 * Text area for entering the automation goal.
 */

import { Sparkles } from "lucide-react";
import { useAiBuilder } from "./AiBuilderContext";

export function GoalInput() {
  const { goal, setGoal } = useAiBuilder();

  return (
    <div className="card p-4 space-y-3">
      <div className="flex items-center gap-2">
        <Sparkles className="w-4 h-4 text-accent" />
        <span className="font-medium">Goal</span>
      </div>

      <textarea
        value={goal}
        onChange={(e) => setGoal(e.target.value)}
        placeholder="Describe what you want to verify or achieve..."
        className="w-full h-24 px-3 py-2 bg-background border border-border rounded-md text-sm resize-none focus:outline-none focus:ring-2 focus:ring-primary"
      />
    </div>
  );
}
