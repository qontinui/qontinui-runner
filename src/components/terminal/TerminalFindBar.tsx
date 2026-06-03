/**
 * TerminalFindBar — VS Code-parity find widget for a single terminal.
 *
 * Overlays the top-right of a `TerminalInstance` (the same spot VS Code's
 * terminal find widget occupies). State lives in `TerminalInstance`; this
 * component is purely presentational + keyboard interpretation:
 *
 *   Enter / F3              find next
 *   Shift+Enter / Shift+F3  find previous
 *   Escape                  close (clears highlights, refocuses terminal)
 *
 * Toggles mirror VS Code's three: match case (Aa), whole word (ab), regex (.*).
 * The match counter renders "k of n" like VS Code, or "No results".
 *
 * All keydown events stop propagation so the page-level shortcut handler
 * (`useKeyboardShortcuts`) and the terminal itself never see keystrokes meant
 * for the find input.
 */

import { useEffect, useRef } from "react";
import { ChevronUp, ChevronDown, X } from "lucide-react";

/** Resolved find-bar key action — pure + unit-testable (node env, no JSX). */
export type FindKeyAction = "next" | "prev" | "close" | null;

/** Interpret a keydown inside the find input. */
export function interpretFindKey(e: { key: string; shiftKey: boolean }): FindKeyAction {
  if (e.key === "Escape") return "close";
  if (e.key === "Enter" || e.key === "F3") return e.shiftKey ? "prev" : "next";
  return null;
}

/** Format the VS Code-style match counter. */
export function formatMatchCount(resultIndex: number, resultCount: number, query: string): string {
  if (!query) return "";
  if (resultCount <= 0) return "No results";
  // resultIndex is -1 when the addon exceeded its highlight limit — fall back
  // to a bare total rather than a bogus "0 of n".
  if (resultIndex < 0) return `${resultCount}`;
  return `${resultIndex + 1} of ${resultCount}`;
}

interface TerminalFindBarProps {
  query: string;
  onQueryChange: (q: string) => void;
  resultIndex: number;
  resultCount: number;
  caseSensitive: boolean;
  wholeWord: boolean;
  regex: boolean;
  onToggleCase: () => void;
  onToggleWholeWord: () => void;
  onToggleRegex: () => void;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
}

function ToggleButton({
  active,
  label,
  title,
  onClick,
}: {
  active: boolean;
  label: string;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      onMouseDown={(e) => e.preventDefault()} // keep focus in the find input
      className={`px-1 py-0.5 rounded text-[10px] font-mono transition-colors ${
        active
          ? "bg-[#3d59a1] text-[#c0caf5]"
          : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
      }`}
    >
      {label}
    </button>
  );
}

export function TerminalFindBar({
  query,
  onQueryChange,
  resultIndex,
  resultCount,
  caseSensitive,
  wholeWord,
  regex,
  onToggleCase,
  onToggleWholeWord,
  onToggleRegex,
  onNext,
  onPrev,
  onClose,
}: TerminalFindBarProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  // Focus + select the query on open so typing replaces a stale term —
  // matches VS Code, which seeds the find widget with the previous query.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const count = formatMatchCount(resultIndex, resultCount, query);
  const hasMatches = resultCount > 0;

  return (
    <div
      className="absolute top-1 right-2 z-30 flex items-center gap-1 px-2 py-1 rounded border border-[#2a2d3d] bg-[#16161e] shadow-lg"
      data-testid="terminal-find-bar"
      onMouseDown={(e) => e.stopPropagation()} // don't steal zone focus-click
    >
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => onQueryChange(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          const action = interpretFindKey(e);
          if (!action) return;
          e.preventDefault();
          if (action === "next") onNext();
          else if (action === "prev") onPrev();
          else onClose();
        }}
        placeholder="Find"
        spellCheck={false}
        className="w-40 bg-transparent text-[12px] text-[#c0caf5] placeholder-[#565f89] outline-hidden font-mono"
      />
      <span
        className={`text-[10px] whitespace-nowrap min-w-[52px] text-right ${
          query && !hasMatches ? "text-[#f7768e]" : "text-[#565f89]"
        }`}
      >
        {count}
      </span>
      <ToggleButton active={caseSensitive} label="Aa" title="Match case" onClick={onToggleCase} />
      <ToggleButton
        active={wholeWord}
        label="ab"
        title="Match whole word"
        onClick={onToggleWholeWord}
      />
      <ToggleButton
        active={regex}
        label=".*"
        title="Use regular expression"
        onClick={onToggleRegex}
      />
      <button
        title="Previous match (Shift+Enter)"
        onClick={onPrev}
        onMouseDown={(e) => e.preventDefault()}
        disabled={!hasMatches}
        className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] disabled:opacity-40 disabled:hover:bg-transparent"
      >
        <ChevronUp className="w-3.5 h-3.5" />
      </button>
      <button
        title="Next match (Enter)"
        onClick={onNext}
        onMouseDown={(e) => e.preventDefault()}
        disabled={!hasMatches}
        className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] disabled:opacity-40 disabled:hover:bg-transparent"
      >
        <ChevronDown className="w-3.5 h-3.5" />
      </button>
      <button
        title="Close (Escape)"
        onClick={onClose}
        className="p-0.5 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#2a2d3d]"
      >
        <X className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}
