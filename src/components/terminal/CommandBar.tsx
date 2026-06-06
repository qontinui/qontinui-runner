/**
 * Phase 2 — CommandBar v1.
 *
 * Bottom-docked slash-command input. Tier-1 resolver only (slash +
 * fuzzy); no AI tier. The 10 actions registered by `useTerminalCommands`
 * drive every match shown here, so adding a future action is a single
 * `useCommandAction` call — no edits in this component.
 *
 * Behavior contracted by the audit (`commands/audit.md` §"Per-action
 * success criterion checks"):
 *
 *   - **Empty input + focus** drops a fuzzy palette under the input,
 *     sorted by recency. Common case = zero keystrokes past focus +
 *     arrow + Enter.
 *   - **`Ctrl+/`** focuses the bar from anywhere on the page. Listener
 *     is attached with `capture: true` so xterm's textarea (which
 *     captures most Ctrl-chords for the PTY) doesn't swallow it.
 *   - **Tab** completes the selected suggestion's slash form into the
 *     input (`/sp` → `/spawn-ai `), leaving the cursor at the arg
 *     position.
 *   - **Enter** parses args from the input and executes the selected
 *     action. Result feedback goes into a 3s status line just above
 *     the bar; recents are updated on success.
 *   - **Escape** clears + blurs.
 *   - **Rotating placeholder** cycles example slash commands every 8s
 *     when the input is empty (passive learning surface per redesign
 *     plan §3 item 3).
 *
 * Mounts via `TerminalPage.tsx` as the last in-flow child of the page's
 * flex column, so it docks as a full-width footer below the zone grid
 * (the grid's `flex-1` row shrinks to make room) rather than floating
 * over a terminal. The transient status line + suggestion dropdown are
 * the only absolutely-positioned parts — they overlay UPWARD from the
 * footer (`bottom-full`) so they never change the docked bar's height.
 */

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { instanceStorage } from "@/lib/instance-storage";

import {
  type CommandAction,
  type CommandResult,
  type InterpretMatch,
  getAll,
  interpretCommand,
  matchPattern,
  parseArgs,
  resolve,
  subscribe,
} from "./commands";

const RECENTS_STORAGE_KEY = "terminal-command-bar-recents";
const MAX_RECENTS = 6;
const STATUS_AUTO_CLEAR_MS = 3000;
const PLACEHOLDER_ROTATE_MS = 8000;

// Tier-3 (claude subprocess) debounce + gating. The subprocess takes
// ~1.5-3s; we don't want to fire it on every keystroke. 600ms is long
// enough to let the operator finish typing a phrase, short enough that
// they don't feel a lag after they stop.
const TIER3_DEBOUNCE_MS = 600;
// Minimum normalized-query length that's "meaningful enough" to spend a
// subprocess call on. Below this, Tier-1 fuzzy carries the response.
const TIER3_MIN_CHARS = 3;

const PLACEHOLDER_EXAMPLES = [
  "/spawn-ai 3 best",
  "/spawn 2",
  "/layout six-pack",
  "/focus needs-input",
  "/swap 1 3",
];

interface StatusLine {
  kind: "ok" | "error";
  text: string;
}

/** Subscribe to the registry via useSyncExternalStore so the suggestion
 *  list rebuilds when `useTerminalCommands` registers / unregisters. */
function useRegistrySnapshot(): readonly CommandAction[] {
  return useSyncExternalStore(subscribe, getAll);
}

function useRotatingPlaceholder(): string {
  const [idx, setIdx] = useState(0);
  useEffect(() => {
    const id = setInterval(
      () => setIdx((i) => (i + 1) % PLACEHOLDER_EXAMPLES.length),
      PLACEHOLDER_ROTATE_MS,
    );
    return () => clearInterval(id);
  }, []);
  return `Press / or try: ${PLACEHOLDER_EXAMPLES[idx]}`;
}

export function CommandBar() {
  const [query, setQuery] = useState("");
  const [focused, setFocused] = useState(false);
  const [selectedIdx, setSelectedIdx] = useState(0);
  const [status, setStatus] = useState<StatusLine | null>(null);
  const [recents, setRecents] = useState<string[]>(() =>
    instanceStorage.getJSON<string[]>(RECENTS_STORAGE_KEY, []),
  );

  const inputRef = useRef<HTMLInputElement>(null);
  const blurTimerRef = useRef<number | null>(null);
  const statusTimerRef = useRef<number | null>(null);

  // Tier-3 state — async AI resolution result + in-flight indicator.
  // Lives outside the synchronous `matches` useMemo because the
  // subprocess call is debounced + async, not a pure function of query.
  const [tier3Match, setTier3Match] = useState<InterpretMatch | null>(null);
  const [interpreting, setInterpreting] = useState(false);

  // Re-render when actions register / unregister.
  useRegistrySnapshot();
  const placeholder = useRotatingPlaceholder();

  // ── Ctrl+/ → focus the input. Capture-phase so xterm's textarea (which
  //    intercepts most Ctrl-letter chords for PTY input) doesn't swallow
  //    it; preventDefault + stopPropagation block the default
  //    "control-_" character from reaching the focused terminal.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && !e.shiftKey && !e.altKey && e.key === "/") {
        e.preventDefault();
        e.stopPropagation();
        inputRef.current?.focus();
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => {
      window.removeEventListener("keydown", handler, { capture: true });
    };
  }, []);

  // ── Auto-clear the status line.
  useEffect(() => {
    if (!status) return;
    if (statusTimerRef.current !== null) {
      window.clearTimeout(statusTimerRef.current);
    }
    statusTimerRef.current = window.setTimeout(() => {
      setStatus(null);
      statusTimerRef.current = null;
    }, STATUS_AUTO_CLEAR_MS);
    return () => {
      if (statusTimerRef.current !== null) {
        window.clearTimeout(statusTimerRef.current);
        statusTimerRef.current = null;
      }
    };
  }, [status]);

  // Match list — Tier 3 (AI), then Tier 2 (regex patterns), then Tier 1
  // (exact slash / fuzzy). Higher tiers WIN because shape-aware routing
  // beats verb-only routing: Tier-1's exact `/spawn` hit would let
  // parseArgs mis-bind `count` to "3 best" via the free-form catch-all,
  // and Tier-3's free-text understanding routes phrasings neither
  // tier-1 nor tier-2 can catch. When a higher tier hits, its action is
  // filtered out of the lower tiers so the dropdown doesn't show the
  // same row twice. Pre-parsed args from the higher tier (regex named
  // groups for Tier-2, model output for Tier-3) ride on the match as
  // `presetArgs` so `execute` can skip `parseArgs` and use them
  // directly.
  const matches = useMemo(() => {
    type LocalMatch = {
      action: CommandAction;
      exact: boolean;
      recent: boolean;
      indices: number[];
      /** Pre-extracted args from Tier-2 / Tier-3. Bypasses parseArgs. */
      presetArgs?: Record<string, unknown>;
      /** Source tier — surfaces in the dropdown so operators can
       *  sanity-check the AI hit before pressing Enter. */
      tier?: "ai";
      /** Model self-confidence for Tier-3 entries. */
      confidence?: number;
    };
    const tier2 = matchPattern(query);
    const tier1 = resolve(query, recents);
    const tier3 = tier3Match;

    const headMatch: LocalMatch | null = tier3
      ? {
          action: tier3.action,
          exact: true,
          recent: recents.includes(tier3.action.id),
          indices: [],
          presetArgs: tier3.args,
          tier: "ai",
          confidence: tier3.confidence,
        }
      : tier2
        ? {
            action: tier2.action,
            exact: true,
            recent: recents.includes(tier2.action.id),
            indices: [],
            presetArgs: tier2.args,
          }
        : null;

    if (!headMatch) return tier1 as LocalMatch[];

    // Filter the lower tier(s) so the dropdown shows the head match
    // once, not twice. Cast the spread to `LocalMatch[]` so the
    // optional `tier` / `confidence` fields are reachable downstream.
    return [
      headMatch,
      ...(tier1.filter((m) => m.action.id !== headMatch.action.id) as LocalMatch[]),
    ];
  }, [query, recents, tier3Match]);

  // ── Tier-3 debounced fire ──────────────────────────────────────────
  // After the operator stops typing for TIER3_DEBOUNCE_MS, if Tier-1/2
  // didn't produce an exact match AND the input is long enough to be
  // meaningful, fire the claude subprocess. Result lands in
  // `tier3Match` and prepends to the dropdown above.
  useEffect(() => {
    const trimmed = query.trim();
    // Clear any previous result whenever the query changes — operator
    // is mid-typing or starting over, the prior Tier-3 hit no longer
    // applies.
    setTier3Match(null);

    if (trimmed.length < TIER3_MIN_CHARS) {
      setInterpreting(false);
      return;
    }
    // Skip if Tier-1 / Tier-2 already nailed it — Tier-3 would burn a
    // subprocess on a query that's already resolved.
    const tier1Exact = resolve(query, recents).find((m) => m.exact);
    if (tier1Exact || matchPattern(query)) {
      setInterpreting(false);
      return;
    }

    const controller = new AbortController();
    const timer = window.setTimeout(async () => {
      setInterpreting(true);
      try {
        const result = await interpretCommand(query, { signal: controller.signal });
        if (!controller.signal.aborted) {
          setTier3Match(result);
        }
      } finally {
        if (!controller.signal.aborted) setInterpreting(false);
      }
    }, TIER3_DEBOUNCE_MS);

    return () => {
      controller.abort();
      window.clearTimeout(timer);
      setInterpreting(false);
    };
  }, [query, recents]);

  // Keep the selection in range when the match list shrinks.
  useEffect(() => {
    if (selectedIdx >= matches.length) {
      setSelectedIdx(0);
    }
  }, [matches.length, selectedIdx]);

  const selectedMatch = matches[selectedIdx];

  const persistRecent = useCallback(
    (id: string) => {
      setRecents((prev) => {
        const next = [id, ...prev.filter((x) => x !== id)].slice(0, MAX_RECENTS);
        instanceStorage.setJSON(RECENTS_STORAGE_KEY, next);
        return next;
      });
    },
    [setRecents],
  );

  const execute = useCallback(
    async (
      action: CommandAction,
      rawInput: string,
      presetArgs?: Record<string, unknown>,
      tier?: "ai",
    ) => {
      // Tier-2 / Tier-3 hits arrive with args already extracted (regex
      // named groups for Tier-2, model output for Tier-3); use them
      // verbatim rather than re-parsing positionally (positional parse
      // on "spawn 3 best" against /spawn's 1-field schema would silently
      // mis-bind).
      const args = presetArgs ?? parseArgs(rawInput, action);
      let result: CommandResult;
      try {
        result = await action.handler(args, {
          source: tier === "ai" ? "ai" : "slash",
        });
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setStatus({ kind: "error", text: `${action.slash}: ${message}` });
        return;
      }
      if (result.ok) {
        persistRecent(action.id);
        // Result.value may be `undefined` (action just ran) — render the
        // slash itself so the operator gets the "yes, that happened"
        // confirmation regardless of return shape.
        setStatus({
          kind: "ok",
          text: `${action.slash} ✓`,
        });
        setQuery("");
        setSelectedIdx(0);
        inputRef.current?.blur();
      } else {
        setStatus({
          kind: "error",
          text: `${action.slash}: ${result.message ?? result.code}`,
        });
      }
    },
    [persistRecent],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIdx((i) => Math.min(i + 1, Math.max(0, matches.length - 1)));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIdx((i) => Math.max(0, i - 1));
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        if (selectedMatch) {
          // If the input already starts with the slash, keep what's
          // past it (user is mid-arg-edit). Otherwise replace the
          // input with the slash + trailing space so the cursor is at
          // the arg position.
          const slash = selectedMatch.action.slash;
          if (query.trim().startsWith(slash)) {
            const argsTail = query.replace(slash, "").trimStart();
            setQuery(`${slash} ${argsTail}`);
          } else {
            setQuery(`${slash} `);
          }
        }
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        if (selectedMatch) {
          void execute(selectedMatch.action, query, selectedMatch.presetArgs, selectedMatch.tier);
        }
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setQuery("");
        setSelectedIdx(0);
        inputRef.current?.blur();
        return;
      }
    },
    [matches.length, query, selectedMatch, execute],
  );

  const handleSuggestionClick = useCallback(
    (action: CommandAction, idx: number, presetArgs?: Record<string, unknown>, tier?: "ai") => {
      // If the user clicks an action that takes args and they haven't typed
      // any AND we didn't pattern-match preset args, populate the input
      // rather than executing — they probably wanted to fill in the args.
      const hasArgs = action.paramSchema && Object.keys(action.paramSchema).length > 0;
      const argsTyped = query.trim().length > action.slash.length;
      if (hasArgs && !argsTyped && !presetArgs) {
        setQuery(`${action.slash} `);
        setSelectedIdx(idx);
        inputRef.current?.focus();
        return;
      }
      void execute(action, query.trim().length > 0 ? query : action.slash, presetArgs, tier);
    },
    [execute, query],
  );

  // Delay the blur-driven dropdown hide so click events on suggestions
  // fire before the dropdown unmounts. Standard fuzzy-finder pattern.
  const handleBlur = useCallback(() => {
    if (blurTimerRef.current !== null) {
      window.clearTimeout(blurTimerRef.current);
    }
    blurTimerRef.current = window.setTimeout(() => {
      setFocused(false);
      blurTimerRef.current = null;
    }, 120);
  }, []);

  const handleFocus = useCallback(() => {
    if (blurTimerRef.current !== null) {
      window.clearTimeout(blurTimerRef.current);
      blurTimerRef.current = null;
    }
    setFocused(true);
  }, []);

  // Surface matches when the input is focused OR when there's query
  // content. Gating solely on `focused` meant an external driver (UI
  // Bridge `type`/`setValue`, which sets the value + fires `input` but
  // not `focus`) could never open the dropdown — and any environment
  // that drops the focus event saw the same dead resolver. Keying off
  // query content makes match-surfacing correct regardless of how input
  // arrives, and removes the `dispatchEvent(FocusEvent('focus'))`
  // workaround from on-page slash tests.
  const dropdownVisible = (focused || query.trim().length > 0) && matches.length >= 0;

  return (
    <div data-page-element="command-bar" className="relative z-40 w-full shrink-0">
      {/* Status line + suggestion dropdown float in an overlay anchored
          ABOVE the docked footer (`bottom-full`) so they never change the
          bar's own height or push the terminal grid while the operator
          types. The wrapper is click-through; only the panels inside it
          capture pointer events. */}
      {((status && !focused) || dropdownVisible) && (
        <div className="absolute bottom-full inset-x-0 z-40 flex justify-center px-3 pb-1 pointer-events-none">
          <div className="w-[520px] max-w-full">
            {/* Status line — visible briefly after execute. */}
            {status && !focused && (
              <div className="mb-1 px-2 py-1 text-[10px] rounded bg-[#1a1b26]/90 border border-[#2a2d3d]/60 backdrop-blur-sm pointer-events-auto">
                <span
                  className={
                    status.kind === "ok" ? "text-[#9ece6a] font-mono" : "text-[#f7768e] font-mono"
                  }
                >
                  {status.text}
                </span>
              </div>
            )}

            {/* Suggestion dropdown — drops UPWARD above the input since the
          input is bottom-pinned. Only renders when focused. */}
            {dropdownVisible && (
              <div className="mb-1 bg-[#1a1b26]/95 border border-[#2a2d3d] rounded-md shadow-xl backdrop-blur-sm overflow-hidden pointer-events-auto">
                {/* Preview row — only shown when there's an exact match (operator
              has past the disambiguation point). For Tier-3 matches the
              row also surfaces the confidence so operators can sanity-
              check the AI's choice before pressing Enter. */}
                {selectedMatch?.exact && (
                  <div className="px-3 py-1.5 border-b border-[#2a2d3d]/50 flex items-baseline gap-2 text-[11px]">
                    <span className="text-[#9ece6a]">⏎</span>
                    <span className="text-[#c0caf5] font-mono truncate">
                      {query.trim().length > 0 ? query.trim() : selectedMatch.action.slash}
                    </span>
                    {selectedMatch.tier === "ai" && selectedMatch.confidence !== undefined && (
                      <span
                        className="text-[9px] font-mono px-1 rounded bg-[#bb9af7]/15 text-[#bb9af7] shrink-0"
                        title={`AI Tier-3 match — model confidence ${Math.round(
                          selectedMatch.confidence * 100,
                        )}%`}
                      >
                        AI {Math.round(selectedMatch.confidence * 100)}%
                      </span>
                    )}
                    <span className="text-[#565f89] ml-auto shrink-0">
                      {selectedMatch.action.label}
                    </span>
                  </div>
                )}

                {/* Tier-3 in-flight indicator. Sits above the match list so
              the dropdown shifts predictably as state changes. */}
                {interpreting && (
                  <div
                    className="px-3 py-1 border-b border-[#2a2d3d]/50 flex items-center gap-2 text-[10px] text-[#bb9af7]"
                    data-page-element="status-indicator"
                    data-indicator="tier3-interpreting"
                  >
                    <span className="w-2 h-2 border-2 border-[#bb9af7] border-t-transparent rounded-full animate-spin" />
                    Interpreting…
                    <span className="ml-auto text-[#565f89]">Esc to cancel</span>
                  </div>
                )}

                {/* Match list */}
                {matches.length === 0 ? (
                  <div className="px-3 py-2 text-[11px] text-[#565f89]">
                    No match — press <span className="font-mono text-[#a9b1d6]">Ctrl+Shift+K</span>{" "}
                    to browse.
                  </div>
                ) : (
                  matches.map((m, idx) => (
                    <button
                      key={m.action.id}
                      type="button"
                      onMouseDown={(e) => e.preventDefault() /* keep input focus */}
                      onClick={() => handleSuggestionClick(m.action, idx, m.presetArgs, m.tier)}
                      onMouseEnter={() => setSelectedIdx(idx)}
                      className={`w-full flex items-center gap-2 px-3 py-1 text-[11px] text-left transition-colors ${
                        idx === selectedIdx
                          ? "bg-[#7aa2f7]/10 text-[#c0caf5]"
                          : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                      }`}
                    >
                      <span className="font-mono text-[#7aa2f7] shrink-0 w-24 truncate">
                        {m.action.slash}
                      </span>
                      <span className="text-[#565f89] truncate flex-1">{m.action.label}</span>
                      {m.tier === "ai" && (
                        <span
                          className="ml-auto text-[8px] font-mono uppercase tracking-wider shrink-0 text-[#bb9af7]"
                          title={
                            m.confidence !== undefined
                              ? `Tier-3 AI match — confidence ${Math.round(m.confidence * 100)}%`
                              : "Tier-3 AI match"
                          }
                        >
                          AI
                          {m.confidence !== undefined ? ` ${Math.round(m.confidence * 100)}%` : ""}
                        </span>
                      )}
                      {m.recent && m.tier !== "ai" && (
                        <span className="ml-auto text-[8px] text-[#bb9af7] uppercase tracking-wider shrink-0">
                          recent
                        </span>
                      )}
                    </button>
                  ))
                )}
              </div>
            )}
          </div>
        </div>
      )}

      {/* The docked footer bar — full width, fixed height, top border so it
          reads as page chrome (a minibuffer-style command line) rather than
          a floating control overlapping the terminal grid. */}
      <div
        className={`h-7 flex items-center gap-2 px-3 border-t backdrop-blur-sm transition-colors ${
          focused ? "bg-[#13141f]/95 border-[#7aa2f7]/40" : "bg-[#13141f]/80 border-[#2a2d3d]"
        }`}
      >
        <span className="text-[10px] text-[#565f89] font-mono select-none">›</span>
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onFocus={handleFocus}
          onBlur={handleBlur}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          className="flex-1 bg-transparent outline-hidden text-[11px] text-[#c0caf5] placeholder-[#565f89]/70"
        />
        <span
          className="text-[9px] text-[#565f89] font-mono select-none"
          title="Press Ctrl+/ to focus this bar from anywhere on the page"
        >
          Ctrl+/
        </span>
      </div>
    </div>
  );
}
