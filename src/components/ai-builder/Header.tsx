/**
 * Header.tsx
 *
 * Header section for the AI Automation Builder.
 */

import { Sparkles } from "lucide-react";

export function Header() {
  return (
    <div className="flex items-center gap-3">
      <div className="p-2 bg-primary/10 rounded-lg">
        <Sparkles className="w-6 h-6 text-primary" />
      </div>
      <div>
        <h2 className="text-xl font-semibold">AI Automation Builder</h2>
        <p className="text-sm text-muted-foreground">
          Run automation sequences, capture screenshots, and let AI analyze/fix issues in a loop
        </p>
      </div>
    </div>
  );
}
