import { useMemo } from "react";
import { instanceStorage } from "@/lib/instance-storage";

const HISTORY_KEY = "prompt-home-history";
const MAX_HISTORY = 10;

const EXAMPLE_PROMPTS = [
  "Show me the error monitor",
  "Run the last workflow",
  "Schedule a daily workflow at 3am",
  "Open the workflow builder",
];

export function getPromptHistory(): string[] {
  const stored = instanceStorage.getItem(HISTORY_KEY);
  if (!stored) return [];
  try {
    return JSON.parse(stored) as string[];
  } catch {
    return [];
  }
}

export function savePromptToHistory(prompt: string) {
  const history = getPromptHistory().filter((h) => h !== prompt);
  history.unshift(prompt);
  instanceStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(0, MAX_HISTORY)));
}

interface PromptSuggestionsProps {
  onSelect: (prompt: string) => void;
}

export function PromptSuggestions({ onSelect }: PromptSuggestionsProps) {
  const suggestions = useMemo(() => {
    const history = getPromptHistory();
    return history.length > 0 ? history.slice(0, 3) : EXAMPLE_PROMPTS.slice(0, 4);
  }, []);

  return (
    <div className="flex flex-wrap justify-center gap-2 mt-4 max-w-xl">
      {suggestions.map((suggestion) => (
        <button
          key={suggestion}
          onClick={() => onSelect(suggestion)}
          className="px-3 py-1.5 text-sm rounded-full border border-border/50 text-muted-foreground
            hover:border-primary/50 hover:text-foreground hover:bg-accent/50 transition-colors"
        >
          {suggestion}
        </button>
      ))}
    </div>
  );
}
