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
 *     action. Result feedback goes into a status line just above the bar
 *     which HOLDS until the next execute; recents are updated on success.
 *   - **ArrowUp on an empty input** walks the executed-input history
 *     (ArrowDown walks back out of it); with content in the input the
 *     arrows navigate the suggestion list as before.
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
import { useUIElement, type StandardAction } from "@qontinui/ui-bridge";

import { instanceStorage } from "@/lib/instance-storage";
import { GLOBAL_CHORDS, matchesChord } from "@/lib/globalChords";
import { isSurfaceVisible } from "@/lib/surfaceVisible";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { SessionManagerToggle } from "./SessionManagerToggle";
import { SpawnTenantPicker } from "./SpawnTenantPicker";

import {
  type CommandAction,
  type CommandResult,
  type InterpretMatch,
  type StatusKind,
  renderCommandStatus,
  getAll,
  interpretCommand,
  applyDeclaredFlags,
  chooseTier,
  didYouMean,
  matchPattern,
  parseArgs,
  unboundTokens,
  resolve,
  subscribe,
} from "./commands";

const RECENTS_STORAGE_KEY = "terminal-command-bar-recents";
const MAX_RECENTS = 6;
const PLACEHOLDER_ROTATE_MS = 8000;

/** Raw inputs the operator actually executed, newest first — the
 *  ArrowUp-on-empty recall ring. Distinct from RECENTS, which stores
 *  action *ids* for ranking, not the text that was typed. */
const HISTORY_STORAGE_KEY = "terminal-command-bar-history";
const MAX_HISTORY = 50;

/** DOM id of the suggestion listbox, referenced by the input's
 *  `aria-controls` / `aria-activedescendant`. */
const LISTBOX_ID = "command-bar-listbox";

/** Stable per-option DOM id so `aria-activedescendant` can name the row
 *  the keyboard selection is on. */
function optionId(actionId: string): string {
  return `command-bar-option-${actionId}`;
}

/** `data-page-element` for a suggestion row. Keyed by the slash body
 *  (leading `/` dropped) so the id is a clean selector token. */
function suggestionElementId(slash: string): string {
  return `command-bar-suggestion-${slash.replace(/^\//, "")}`;
}


/**
 * Author-controlled control id for the command bar's text input. Exported so
 * tests and callers building UI-Bridge action URLs reference one constant
 * instead of re-typing the string. Same convention as
 * `SESSION_MANAGER_TOGGLE_ID` and `ZoneHoverActions`' `terminal.close-session-<id>`.
 */
export const COMMAND_BAR_INPUT_ID = "terminal.command-bar-input";

/**
 * Actions this input advertises.
 *
 * The registry's `inferActions('input')` yields
 * `focus, blur, hover, scroll, scrollIntoView, click, hoverClick, type, clear`
 * — no `sendKeys`. Both GLOBAL whitelists already carry `sendKeys` (SDK
 * `action-executor.ts`, runner `mcp/ui_bridge/elements.rs`
 * `SUPPORTED_ACTION_NAMES`), so the rejection a driver actually hit —
 * `ACTION_NOT_SUPPORTED`, "Action 'sendKeys' is not supported by element …" —
 * came from the PER-ELEMENT advertised list at the pre-IPC gate
 * (`is_action_advertised`, permissive only when an element advertises nothing
 * at all). Driving the bar (type `/resume`, press Enter) is exactly a
 * `sendKeys` job, so the inferred set is simply wrong here; this restates it
 * with `sendKeys` added.
 *
 * Deliberately a plain ADVERTISEMENT and not a `customActions` entry: the
 * SDK's built-in `performSendKeys` already does the right thing for a text
 * input, and a registered custom action WINS over a same-named built-in on
 * that element (ui-bridge `bc591af`). `TerminalInstance.tsx` already registers
 * a custom `sendKeys` on the terminal PANE whose `keys` param is a plain string
 * written straight to the PTY — a second, incompatible meaning for the name.
 * A third would make `sendKeys` unpredictable per element, so the command bar
 * keeps the SDK's descriptor-array contract (`keys: [{key, modifiers?}]`).
 */
export const COMMAND_BAR_INPUT_ACTIONS: StandardAction[] = [
  "focus",
  "blur",
  "hover",
  "scroll",
  "scrollIntoView",
  "click",
  "hoverClick",
  "type",
  "clear",
  "sendKeys",
];

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

/**
 * The three verdicts this bar can render.
 *
 * The CLASSIFIER is `verdict.ts::statusKindOf` and the composition is
 * `verdict.ts::renderCommandStatus`; what stays here is the product decision
 * and the three colours. Moving the decision out is not tidying — while it
 * lived in this component it was unreachable from the headless harness, and
 * the ok/noop split went uncharacterised across a 91,784-input corpus.
 *
 * `noop` is a PRODUCT DECISION, made here and stated here.
 *
 * A handler that reports `{affected: 0}` did nothing. Rendering that as `✓`
 * is the defect this phase exists to close — it is exactly what let
 * `/approve-all` claim three approvals having delivered zero keystrokes. But
 * rendering it as an ERROR is wrong too, and not marginally so:
 *
 *  - `/tag-clear` with no filter active, `/mute` when already muted,
 *    `/layout quad` when the grid is already quad — these are LEGITIMATE
 *    outcomes of correct commands. `/mute` is idempotent BY DESIGN; it was
 *    split out of `/sound` precisely so that running it twice leaves the
 *    sound off. Calling the second run an error would make the command's
 *    reason for existing look like a fault.
 *  - Errors on this surface carry a "did you mean" hint and are the operator's
 *    signal to retype. Retyping a no-op produces the same no-op. Training the
 *    operator to ignore red is a strictly worse outcome than one grey line.
 *  - Mechanically: verdicts are pinned by `__golden__/pipeline-golden.txt` as
 *    `ok` / `error:<code>`. Turning no-ops into errors would flip thousands of
 *    corpus rows and change what every downstream consumer of
 *    `CommandResult.ok` sees, for a rendering concern.
 *
 * So a no-op stays `ok` at the `CommandResult` level and gets its OWN verdict
 * at the render level: distinct text ("no tag filters cleared"), distinct
 * colour (neutral, not green), and a distinct `data-status-kind="noop"` that
 * a UI Bridge assertion can read. The invariant that matters is the one the
 * bare `✓` broke: **a no-op must never render identically to an effect.**
 *
 * What that argument does NOT cover, and what `statusKindOf` now separates: a
 * zero that came of a POSITIVE request. `/approve-all` with three panes in
 * needs-input and all three writes refused on the wire is not `/mute` being
 * idempotent — every single thing the operator asked for failed. It rendered
 * grey, with the same head sentence as "nothing was waiting", and told a UI
 * Bridge assertion `data-status-kind="noop"`. That arm is red. The
 * `CommandResult` stays `ok` either way, so the golden rows above do not move.
 */
interface StatusLine {
  kind: StatusKind;
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
  /** Previous `query`, so the selection can be reset the moment the
   *  query changes — see the derived-state block below `matches`. */
  const [prevQuery, setPrevQuery] = useState("");
  const [status, setStatus] = useState<StatusLine | null>(null);
  const [recents, setRecents] = useState<string[]>(() =>
    instanceStorage.getJSON<string[]>(RECENTS_STORAGE_KEY, []),
  );
  // Executed raw inputs, newest first, plus the cursor into them.
  // `historyIdx === -1` means "not browsing history".
  const [history, setHistory] = useState<string[]>(() =>
    instanceStorage.getJSON<string[]>(HISTORY_STORAGE_KEY, []),
  );
  const [historyIdx, setHistoryIdx] = useState(-1);

  // Working dir of the focused tab — the repo the spawn-tenant inference
  // reads. Undefined on an empty page (no tabs yet), which the picker treats
  // as "no inference, use the active pin".
  const { tabs, activeId } = useTerminalSession();
  const activeTabCwd = useMemo(
    () => tabs.find((t) => t.id === activeId)?.workingDir,
    [tabs, activeId],
  );

  const inputRef = useRef<HTMLInputElement>(null);
  // Register the input as an addressable bridge element so a driver can
  // `sendKeys` at it — see `COMMAND_BAR_INPUT_ACTIONS` for why the inferred
  // action set is not enough.
  const { ref: bridgeInputRef } = useUIElement({
    id: COMMAND_BAR_INPUT_ID,
    type: "input",
    label: "Command bar",
    actions: COMMAND_BAR_INPUT_ACTIONS,
  });
  const blurTimerRef = useRef<number | null>(null);

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
      // `matchesChord` covers Ctrl and the exact Shift state; Alt stays an
      // explicit extra term because the shared table deliberately does not
      // inspect it (see `globalChords`), and this handler has always
      // required Alt to be absent.
      if (matchesChord(e, GLOBAL_CHORDS.commandBar) && !e.altKey) {
        // The input is the surface this chord acts on, so it is also the
        // honest visibility test. `App.tsx` keeps the terminal page mounted
        // behind a `hidden` div on every other tab: without this guard the
        // chord `preventDefault()`ed and `stopPropagation()`ed app-wide —
        // capture phase, so it beat every other listener — and then focused
        // nothing, because `.focus()` on an element in a `display:none`
        // subtree is a no-op. Claiming a key you cannot act on is the same
        // defect as the `Ctrl+1..8` double-fire, one surface over.
        if (!isSurfaceVisible(inputRef.current)) return;
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

  // The status line has NO auto-expiry: it holds the last verdict until
  // the next execute replaces it. A 3s timer (which the focus gate cut to
  // under 2s of visible time) meant a slow command's verdict — the
  // /orchestrate case — landed and vanished while the operator was still
  // watching the grid.

  // Match list. WHICH TIER OWNS THE INPUT is decided by
  // `commands/rank.ts::chooseTier`, and "a literal slash beats everything"
  // has NOT been the rule since iteration 8: a literal slash yields to a
  // Tier-2 pattern naming the SAME action (only the pattern knows what its
  // own trailing token means), reroutes to a DIFFERENT action only when that
  // action declares itself neither `costly` nor `destructive`, and otherwise
  // runs while the alternative is named in a "did you mean" hint. Among the
  // rest the shape-aware tiers beat the verb-only one. The rule and the bugs
  // that produced it live there, in one place, rather than in a comment here
  // that the resolver cannot be tested against.
  //
  // When a higher tier wins, its action is filtered out of Tier 1's list so
  // the dropdown doesn't show the same row twice. Pre-parsed args from that
  // tier (regex named groups for Tier 2, model output for Tier 3) ride on
  // the match as `presetArgs`.
  const matches = useMemo(() => {
    type LocalMatch = {
      action: CommandAction;
      exact: boolean;
      /** See `commands/rank.ts` — the leading `/` the operator typed. */
      literal: boolean;
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
    const tier1 = resolve(query, recents);
    const { head } = chooseTier(tier1, matchPattern(query), tier3Match);
    const headMatch: LocalMatch | null = head
      ? {
          action: head.action,
          exact: true,
          literal: false,
          recent: recents.includes(head.action.id),
          indices: [],
          presetArgs: head.presetArgs,
          tier: head.tier === "ai" ? "ai" : undefined,
          confidence: head.confidence,
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

  // ── The selection belongs to ONE query ─────────────────────────────
  // `matches` is recomputed from scratch on every query change, so index
  // N in the old list names a different command (or none) in the new
  // one. Index 0 — the top-ranked match for the query now in the input —
  // is the only index that survives the change meaningfully.
  //
  // Clamping against `matches.length`, which is all this used to do, is
  // NOT the same guarantee: a stale index stays *in range* whenever the
  // new list is at least as long, and the bar then executes a command
  // the operator never selected. Measured on-page: type `sp`, ArrowDown
  // twice (selection on `/select-by-state`), then type `lyt` — the
  // dropdown re-rendered as [/layout, /analyze, /select-by-state],
  // `aria-activedescendant` still pointed at `select-by-state`, and
  // Enter ran it.
  //
  // Written as derived state (React's "adjust state during render",
  // already the idiom in `CommandPalette.tsx`) rather than a reset
  // inside `handleChange`, because `query` is mutated from six places —
  // typing, ArrowUp/ArrowDown history recall, Tab completion, Escape, a
  // suggestion click that pre-fills args, and the post-execute clear.
  // Keying off the VALUE covers every one of them, including a path
  // added later that nobody remembers to patch.
  if (query !== prevQuery) {
    setPrevQuery(query);
    setSelectedIdx(0);
  }

  // Keep the selection in range when the match list shrinks WITHOUT the
  // query changing — a registry unregister, or the async Tier-3 match
  // being cleared.
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

  /** Record the raw input the operator ran, for ArrowUp recall. Failed
   *  runs are recorded too — a typo is exactly what you want back. */
  const persistHistory = useCallback((rawInput: string) => {
    const entry = rawInput.trim();
    if (!entry) return;
    setHistory((prev) => {
      const next = [entry, ...prev.filter((x) => x !== entry)].slice(0, MAX_HISTORY);
      instanceStorage.setJSON(HISTORY_STORAGE_KEY, next);
      return next;
    });
  }, []);

  const execute = useCallback(
    async (
      action: CommandAction,
      rawInput: string,
      presetArgs?: Record<string, unknown>,
      tier?: "ai",
    ) => {
      // The previous verdict is retired the moment a new command runs —
      // that, not a timer, is what bounds the status line's lifetime.
      setStatus(null);
      persistHistory(rawInput);
      setHistoryIdx(-1);
      // Tier-2 / Tier-3 hits arrive with args already extracted (regex
      // named groups for Tier-2, model output for Tier-3); use them
      // verbatim rather than re-parsing positionally (positional parse
      // on "spawn 3 best" against /spawn's 1-field schema would silently
      // mis-bind).
      //
      // `applyDeclaredFlags` then runs on EVERY route, not just the slash
      // one. Flag extraction used to be a property of `parseArgs`, which
      // made it a property of the route that calls `parseArgs` — so a
      // Tier-2 pattern ending in `.+` swallowed `/spawn-ai`'s declared
      // `--tenant` and the handler's tenant guard never ran. Any future
      // declared flag would have inherited that silently; it is a
      // route-independent step now so it cannot.
      // `origin` is load-bearing, not bookkeeping: the parsed bag was built
      // from the raw input with its quoting intact, so re-scanning its string
      // fields can only destroy information (it deleted words out of a quoted
      // prompt — `parse.ts::applyDeclaredFlags`). A preset bag never saw the
      // schema and still carries raw text, so it gets the full treatment.
      const preset = presetArgs !== undefined;
      const args = applyDeclaredFlags(
        preset ? presetArgs : parseArgs(rawInput, action),
        rawInput,
        action,
        preset ? "preset" : "parsed",
      );
      // A Tier-2 phrasing the literal slash outranked because it names a
      // COSTLY (or destructive) neighbour. The protection stays — `/spawn`
      // must not launch paid sessions — but it stops being a dead end: every
      // failure verdict below names the command that would have run.
      const hint = didYouMean(rawInput, action, matchPattern(rawInput));
      const withHint = (text: string): string => (hint ? `${text} — ${hint}` : text);
      // Trailing junk on a no-argument command. Only the SLASH route can
      // carry it — Tier-2/Tier-3 arrive with `presetArgs` already bound —
      // and only for an empty schema, where `parseArgs`'s catch-all is
      // guarded off and quietly discards every token. `/mute please stop`
      // used to render `/mute ✓`.
      if (!presetArgs) {
        const extra = unboundTokens(rawInput, action);
        if (extra.length > 0) {
          setStatus({
            kind: "error",
            text: withHint(`${action.slash}: takes no arguments (got "${extra.join(" ")}")`),
          });
          return;
        }
      }
      let result: CommandResult;
      try {
        result = await action.handler(args, {
          source: tier === "ai" ? "ai" : "slash",
        });
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setStatus({ kind: "error", text: withHint(`${action.slash}: ${message}`) });
        return;
      }
      if (result.ok) {
        persistRecent(action.id);
        // CONSULT `result.value`. The renderer used to compose
        // `${action.slash} ✓` and throw the value away, which made the
        // status line structurally incapable of telling a no-op from an
        // effect — every handler on the page reported the same sentence.
        //
        // A handler that reports an `EffectReport` gets its numbers rendered;
        // one that does not still gets the bare `✓`, so this is an
        // improvement for the converted handlers and a no-change for the
        // rest, which is what lets a handler be converted independently.
        //
        // The composition itself lives in `verdict.ts::renderCommandStatus`,
        // NOT here. It used to be this component's own two-line ternary, and
        // that put the ok/noop split — the entire subject of this phase — out
        // of reach of the headless corpus harness, which can only see a
        // `CommandResult` and for which a no-op is `ok`. The harness now calls
        // the same function this line does.
        setStatus(renderCommandStatus(action.slash, result.value));
        setQuery("");
        // Explicit even though the derived reset covers a query CHANGE:
        // running off an already-empty input (the recents palette) leaves
        // `query` at "" and the reset would not fire.
        setSelectedIdx(0);
        inputRef.current?.blur();
      } else {
        setStatus({
          kind: "error",
          text: withHint(`${action.slash}: ${result.message ?? result.code}`),
        });
      }
    },
    [persistRecent, persistHistory],
  );

  // History recall is armed while the input is EMPTY (nothing to navigate
  // past) and stays armed once browsing has started — otherwise the recalled
  // text itself would disarm it on the second ArrowUp.
  const historyMode = historyIdx >= 0 || query.trim().length === 0;

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        if (historyMode && historyIdx >= 0) {
          const next = historyIdx - 1;
          setHistoryIdx(next);
          setQuery(next < 0 ? "" : (history[next] ?? ""));
          return;
        }
        setSelectedIdx((i) => Math.min(i + 1, Math.max(0, matches.length - 1)));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        if (historyMode && history.length > 0) {
          const next = Math.min(historyIdx + 1, history.length - 1);
          setHistoryIdx(next);
          setQuery(history[next] ?? "");
          return;
        }
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
        // Same reason as in `execute`: Escape on an already-empty input
        // is not a query change, so the derived reset does not fire.
        setSelectedIdx(0);
        setHistoryIdx(-1);
        inputRef.current?.blur();
        return;
      }
    },
    [matches.length, query, selectedMatch, execute, history, historyIdx, historyMode],
  );

  // Typing anything by hand leaves history-browsing mode — the recalled
  // entry has become a fresh edit, not a cursor position. The suggestion
  // selection is NOT reset here: it is reset for every query mutation at
  // once, in the derived block above.
  const handleChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    setQuery(e.target.value);
    setHistoryIdx(-1);
  }, []);

  const handleSuggestionClick = useCallback(
    (action: CommandAction, presetArgs?: Record<string, unknown>, tier?: "ai") => {
      // If the user clicks an action that takes args and they haven't typed
      // any AND we didn't pattern-match preset args, populate the input
      // rather than executing — they probably wanted to fill in the args.
      //
      // "Pattern-matched preset args" means args the pattern actually BOUND,
      // not merely that a pattern matched. Since a literal slash now yields
      // to its own Tier-2 pattern (`rank.ts::chooseTier`), a bare `/close`
      // arrives here with `presetArgs: {}` — the zone group is optional and
      // matched nothing. Reading that as "the pattern filled the args in"
      // turned a CLICK on the `/close` row from "prefill `/close ` so I can
      // name a zone" into "close the focused zone now".
      const hasArgs = action.paramSchema && Object.keys(action.paramSchema).length > 0;
      const argsTyped = query.trim().length > action.slash.length;
      const presetBound = presetArgs !== undefined && Object.keys(presetArgs).length > 0;
      if (hasArgs && !argsTyped && !presetBound) {
        // No `setSelectedIdx(idx)` here: `idx` indexes the list built for
        // the OLD query, and the new query (`/slash `) is an exact hit
        // resolving to a single row. The derived reset above puts the
        // selection on it.
        setQuery(`${action.slash} `);
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

  // ── Focus tracked from the INPUT's own events, not React's synthetic
  //    delegation. React listens for `focusin`/`focusout` at the root, so
  //    a non-bubbling `FocusEvent('focus')` dispatched straight at the
  //    element — which is what an external driver like the UI Bridge
  //    produces — never reached `onFocus`, leaving the empty-input recents
  //    dropdown undrivable. A native listener on the element sees both the
  //    real and the dispatched event.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.addEventListener("focus", handleFocus);
    el.addEventListener("blur", handleBlur);
    return () => {
      el.removeEventListener("focus", handleFocus);
      el.removeEventListener("blur", handleBlur);
    };
  }, [handleFocus, handleBlur]);

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
      {(status || dropdownVisible) && (
        <div className="absolute bottom-full inset-x-0 z-40 flex justify-center px-3 pb-1 pointer-events-none">
          <div className="w-[520px] max-w-full">
            {/* Status line — the last command's verdict, held until the next
                execute. It is NOT gated on blur any more: an error leaves
                the input focused, so the `!focused` gate hid exactly the
                verdicts worth reading. */}
            {status && (
              <div
                data-page-element="command-bar-status"
                data-status-kind={status.kind}
                role="status"
                aria-live="polite"
                className="mb-1 px-2 py-1 text-[10px] rounded bg-[#1a1b26]/90 border border-[#2a2d3d]/60 backdrop-blur-sm pointer-events-auto"
              >
                <span
                  className={
                    // Three verdicts, three colours — green for an effect,
                    // muted grey for a no-op, red for a failure. The grey is
                    // load-bearing: an operator scanning this line has to be
                    // able to see "nothing happened" without reading it.
                    status.kind === "ok"
                      ? "text-[#9ece6a] font-mono"
                      : status.kind === "noop"
                        ? "text-[#a9b1d6] font-mono"
                        : "text-[#f7768e] font-mono"
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
                  <div
                    data-page-element="command-bar-preview"
                    className="px-3 py-1.5 border-b border-[#2a2d3d]/50 flex items-baseline gap-2 text-[11px]"
                  >
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

                {/* Match list. `role="listbox"` + one `role="option"` per row
              is what makes the keyboard selection READABLE from outside —
              the highlight used to live only in a Tailwind class, so no
              external driver could tell which row Enter would run. The
              option's `value` carries the slash so `read-value` on the
              selected row returns the command itself. */}
                {matches.length === 0 ? (
                  <div className="px-3 py-2 text-[11px] text-[#565f89]">
                    No match — press <span className="font-mono text-[#a9b1d6]">Ctrl+Shift+K</span>{" "}
                    to browse.
                  </div>
                ) : (
                  <div id={LISTBOX_ID} role="listbox" aria-label="Command suggestions">
                    {matches.map((m, idx) => (
                      <button
                        key={m.action.id}
                        id={optionId(m.action.id)}
                        data-page-element={suggestionElementId(m.action.slash)}
                        role="option"
                        aria-selected={idx === selectedIdx}
                        value={m.action.slash}
                        type="button"
                        onMouseDown={(e) => e.preventDefault() /* keep input focus */}
                        onClick={() => handleSuggestionClick(m.action, m.presetArgs, m.tier)}
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
                            {m.confidence !== undefined
                              ? ` ${Math.round(m.confidence * 100)}%`
                              : ""}
                          </span>
                        )}
                        {m.recent && m.tier !== "ai" && (
                          <span className="ml-auto text-[8px] text-[#bb9af7] uppercase tracking-wider shrink-0">
                            recent
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
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
        {/* Session Manager sidebar toggle — the only always-visible strip of
            terminal-page chrome, so this is where the panel's visible (and
            UI-Bridge addressable) affordance lives. Ctrl+Shift+B and
            `/sessions` remain; this is additive. */}
        <SessionManagerToggle />
        {/* F2 — tenant for the next spawn, immediately left of the console
            the operator types `/spawn-ai` into. Self-hides on single-tenant
            devices. */}
        <SpawnTenantPicker cwd={activeTabCwd} />
        <span className="text-[10px] text-[#565f89] font-mono select-none">›</span>
        <input
          // Two consumers of one node: the component's own imperative
          // focus/blur ref, and the bridge registration's ref callback.
          ref={(node) => {
            inputRef.current = node;
            bridgeInputRef(node);
          }}
          // The stamp pins the CONTROL id, and the author stamp wins over the
          // SDK's auto-derived one (`useAutoRegister.ts`'s `!existingStamp`
          // guard) — which here would be minted from the ROTATING placeholder
          // and so would not be stable across the 8s cycle.
          data-ui-bridge-id={COMMAND_BAR_INPUT_ID}
          value={query}
          onChange={handleChange}
          // A bare click must open the dropdown too: a synthetic click
          // from an external driver doesn't focus the element, so without
          // this the recents palette stayed shut. Focus/blur themselves
          // are bound natively in the effect above.
          onClick={handleFocus}
          onKeyDown={handleKeyDown}
          // Stable accessible name. The placeholder rotates every 8s, so
          // it can't be the only name — a lookup by name would resolve
          // the input or not depending on when it ran. `ai/find` and the
          // snapshot's `accessibleName` resolve on this too.
          aria-label="Terminal command bar"
          role="combobox"
          aria-expanded={dropdownVisible}
          aria-autocomplete="list"
          aria-controls={dropdownVisible && matches.length > 0 ? LISTBOX_ID : undefined}
          aria-activedescendant={
            dropdownVisible && selectedMatch ? optionId(selectedMatch.action.id) : undefined
          }
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
