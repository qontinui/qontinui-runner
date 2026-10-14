/**
 * The session search palette — Chrome's tab search for sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * Opened by `Ctrl+Shift+Space` (`GLOBAL_CHORDS.sessionSearch`) or the
 * `/sessions.find` command; searches live, previous and recently-closed
 * sessions by their identifying text, plus transcript full text through
 * `transcript_search`. ↑/↓ move, Enter focuses a live session or resumes the
 * others, Shift+Enter does the same in a new zone, Escape closes.
 *
 * The transcript arm states its own coverage ("searched N of M") and says so
 * when its scan was cut short, so a session absent from the list is never
 * presented as "does not mention it".
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { Search } from "lucide-react";
import { UIBridgeComponentScope, useUIComponent, useUIElement } from "@qontinui/ui-bridge";

import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";
import { describeThrown } from "@/lib/utils";

import { textArg } from "./commands";
import type { SessionFacts } from "./sessionFacts";
import type { ClosedSession } from "./sessionOrgTypes";
import { QUERY_INPUT_ACTIONS } from "./SessionQueryBar";
import { slugify } from "./sessionQueryChips";
import {
  PALETTE_SOURCE_LABEL,
  buildPaletteEntries,
  paletteTerms,
  searchPalette,
  stepSelection,
  transcriptAnswerFor,
  transcriptCoverageNote,
  type PaletteEntry,
  type PaletteMatch,
} from "./sessionSearchPalette";
import {
  sessionSearchPaletteStore,
  useSessionSearchHotkey,
  useSessionSearchPaletteOpen,
} from "./sessionSearchPaletteStore";
import { searchTranscripts, type TranscriptSearchResponse } from "./transcriptSearch";
import type { PastSession } from "./usePastSessions";

/** The component id the palette's actions register under. */
export const SESSION_SEARCH_PALETTE_COMPONENT_ID = "session-search-palette";

/** Author-controlled UI Bridge ids. */
export const SESSION_SEARCH_PALETTE_IDS = {
  root: "terminal.session-org.search-palette",
  input: "terminal.session-org.search-palette-input",
} as const;

/** One result row's id. */
export function sessionSearchPaletteItemId(entryKey: string): string {
  return `terminal.session-org.search-palette-item-${slugify(entryKey)}`;
}

/** Transcript search waits this long after the last keystroke. */
export const TRANSCRIPT_DEBOUNCE_MS = 300;
/** Shortest query the transcript arm runs for. */
export const TRANSCRIPT_MIN_CHARS = 3;
/** Transcript hits asked for. */
const TRANSCRIPT_LIMIT = 50;

const KEY_SCHEMA = { key: "string" } as const;
const QUERY_SCHEMA = { query: "string" } as const;

export interface SessionSearchPaletteProps {
  /** Live rows, with their facts. */
  live: readonly SessionFacts[];
  /** Previous sessions (`usePastSessions().sessions`). */
  previous: readonly PastSession[];
  /** Recently closed (`org.recentlyClosed`); empty while the org is unknown. */
  closed: readonly ClosedSession[];
  /**
   * Act on a picked entry: focus a live one (`entry.rowId`), resume the
   * others (`entry.claudeSessionId` / `entry.resumeCommand`). `newZone` is
   * Shift+Enter or the row's "new zone" button.
   */
  onActivate: (entry: PaletteEntry, opts: { newZone: boolean }) => void;
}

type TranscriptState =
  | { state: "idle" }
  | { state: "pending" }
  | { state: "failed"; reason: string }
  | { state: "done"; resp: TranscriptSearchResponse };

/**
 * Mount once where the Session Manager lives. Renders nothing until opened;
 * its hidden anchor is what gates the chord to a visible Terminal page.
 */
export function SessionSearchPalette(props: SessionSearchPaletteProps) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  useSessionSearchHotkey(anchorRef);
  useEffect(() => sessionSearchPaletteStore.mount(), []);
  const open = useSessionSearchPaletteOpen();
  return (
    <>
      <span ref={anchorRef} aria-hidden data-testid="session-search-palette-anchor" />
      {open && <PaletteBody {...props} />}
    </>
  );
}

function PaletteBody({ live, previous, closed, onActivate }: SessionSearchPaletteProps) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const [answer, setAnswer] = useState<{ query: string; answer: TranscriptState } | null>(null);
  const close = () => sessionSearchPaletteStore.setOpen(false);

  const { ref: rootRef } = useUIElement({
    id: SESSION_SEARCH_PALETTE_IDS.root,
    type: "generic",
    label: "Session search",
  });
  const { ref: inputRef } = useUIElement({
    id: SESSION_SEARCH_PALETTE_IDS.input,
    type: "input",
    label: "Search sessions",
    actions: QUERY_INPUT_ACTIONS,
  });

  const entries = useMemo(
    () => buildPaletteEntries({ live, previous, closed }),
    [live, previous, closed],
  );

  // The transcript arm: debounced; an answer is kept with the query it
  // answered, so a stale one is never shown for the query on screen.
  const q = query.trim();
  const eligible = q.length >= TRANSCRIPT_MIN_CHARS && paletteTerms(q).length > 0;
  useEffect(() => {
    if (!eligible) return;
    let current = true;
    const t = setTimeout(() => {
      searchTranscripts(q, undefined, TRANSCRIPT_LIMIT)
        .then((resp) => {
          if (current) setAnswer({ query: q, answer: { state: "done", resp } });
        })
        .catch((e: unknown) => {
          if (current)
            setAnswer({
              query: q,
              answer: { state: "failed", reason: describeThrown(e, "transcript_search failed") },
            });
        });
    }, TRANSCRIPT_DEBOUNCE_MS);
    return () => {
      current = false;
      clearTimeout(t);
    };
  }, [q, eligible]);
  const transcript: TranscriptState = !eligible
    ? { state: "idle" }
    : answer?.query === q
      ? answer.answer
      : { state: "pending" };

  const resp = transcript.state === "done" ? transcript.resp : null;
  const matches = useMemo(() => searchPalette(entries, query, resp), [entries, query, resp]);
  const selected = matches.length === 0 ? -1 : Math.min(index, matches.length - 1);
  const note = transcriptCoverageNote(
    resp,
    transcript.state,
    transcript.state === "failed" ? transcript.reason : null,
  );

  const activate = (m: PaletteMatch | undefined, newZone: boolean) => {
    if (!m) return;
    onActivate(m.entry, { newZone });
    close();
  };

  useUIComponent({
    id: SESSION_SEARCH_PALETTE_COMPONENT_ID,
    name: "Session search",
    description:
      "Search live, previous and recently-closed sessions by title, topic, repo or " +
      "transcript text; activate focuses a live session or resumes the others.",
    actions: [
      {
        id: "search",
        label: "Search",
        paramSchema: QUERY_SCHEMA,
        // `read` — sets the palette's text and returns what it lists.
        effect: "read",
        handler: guardedHandler("search", QUERY_SCHEMA, (args) => {
          const q = textArg(args, "query");
          setQuery(q);
          // Only an answer kept FOR this query — `resp` is the query on
          // screen before this call, whose transcript hits are not this one's.
          return searchPalette(entries, q, transcriptAnswerFor(answer, q)).map((m) => ({
            key: m.entry.key,
            source: m.entry.source,
            title: m.entry.title,
            via: m.via,
          }));
        }),
      },
      {
        id: "activate",
        label: "Activate",
        description: "Focus (live) or resume (previous / recently closed) the entry with this key.",
        paramSchema: KEY_SCHEMA,
        // `write` — resuming starts a session process; focusing changes the
        // visible tab. Neither ends anything.
        effect: "write",
        handler: guardedHandler("activate", KEY_SCHEMA, (args) => {
          const key = textArg(args, "key");
          const m = matches.find((x) => x.entry.key === key);
          if (!m) throw new Error(`No listed entry "${key}"`);
          activate(m, false);
        }),
      },
      {
        id: "close",
        label: "Close",
        effect: "read",
        handler: () => close(),
      },
    ],
  });

  return (
    <UIBridgeComponentScope componentId={SESSION_SEARCH_PALETTE_COMPONENT_ID}>
      <div className="fixed inset-0 z-50 flex items-start justify-center pt-24">
        <div
          className="absolute inset-0 bg-black/50"
          onClick={close}
          role="button"
          tabIndex={-1}
          aria-label="Close session search"
        />
        <div
          ref={rootRef}
          data-ui-bridge-id={SESSION_SEARCH_PALETTE_IDS.root}
          data-effect="read"
          data-result-count={matches.length}
          role="dialog"
          aria-label="Search sessions"
          className="relative w-[560px] max-w-[90vw] rounded-lg border border-[#2a2d3d] bg-[#13141f] shadow-xl"
        >
          <div className="relative border-b border-[#2a2d3d]">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-[#565f89]" />
            <input
              ref={inputRef}
              data-ui-bridge-id={SESSION_SEARCH_PALETTE_IDS.input}
              data-effect="read"
              autoFocus
              aria-label="Search sessions"
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setIndex(0);
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                  e.preventDefault();
                  setIndex(stepSelection(selected, e.key === "ArrowDown" ? 1 : -1, matches.length));
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  activate(matches[selected], e.shiftKey);
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  close();
                }
              }}
              placeholder="Search live, previous and recently closed sessions"
              className="w-full bg-transparent pl-9 pr-3 py-2 text-sm text-[#c0caf5] placeholder-[#414868] focus:outline-none"
            />
          </div>
          <ul role="listbox" className="max-h-[50vh] overflow-y-auto py-1">
            {matches.length === 0 && (
              <li className="px-3 py-2 text-xs text-[#565f89]">
                {transcript.state === "pending" ? "Searching…" : "No session matches"}
              </li>
            )}
            {matches.map((m, i) => (
              <PaletteRow
                key={m.entry.key}
                match={m}
                selected={i === selected}
                onHover={() => setIndex(i)}
                onActivate={(newZone) => activate(m, newZone)}
              />
            ))}
          </ul>
          <div className="flex items-center justify-between gap-2 border-t border-[#2a2d3d] px-3 py-1 text-[10px] text-[#565f89]">
            <span
              data-testid="session-search-transcript-note"
              className="truncate"
              title={note ?? ""}
            >
              {note ?? `${entries.length} sessions`}
            </span>
            <span className="shrink-0">
              ↑↓ move · Enter open · Shift+Enter new zone · Esc close
            </span>
          </div>
        </div>
      </div>
    </UIBridgeComponentScope>
  );
}

function PaletteRow({
  match,
  selected,
  onHover,
  onActivate,
}: {
  match: PaletteMatch;
  selected: boolean;
  onHover: () => void;
  onActivate: (newZone: boolean) => void;
}) {
  const { entry } = match;
  const id = sessionSearchPaletteItemId(entry.key);
  const { ref } = useUIElement({ id, type: "button", label: `Open ${entry.title}` });
  const { ref: newZoneRef } = useUIElement({
    id: `${id}-new-zone`,
    type: "button",
    label: `Open ${entry.title} in a new zone`,
  });
  return (
    <li role="option" aria-selected={selected} onMouseEnter={onHover}>
      <div className={`flex items-start gap-2 px-3 py-1.5 ${selected ? "bg-[#7aa2f7]/15" : ""}`}>
        <button
          ref={ref}
          data-ui-bridge-id={id}
          data-effect="write"
          data-source={entry.source}
          data-via={match.via}
          onClick={() => onActivate(false)}
          className="flex-1 min-w-0 text-left"
        >
          <div className="flex items-center gap-2">
            <span className="shrink-0 rounded px-1 text-[9px] uppercase tracking-wide text-[#565f89] border border-[#2a2d3d]">
              {PALETTE_SOURCE_LABEL[entry.source]}
            </span>
            <span className="truncate text-xs text-[#c0caf5]" title={entry.title}>
              {entry.title}
            </span>
          </div>
          {entry.detail && (
            <div className="truncate text-[10px] text-[#565f89]" title={entry.detail}>
              {entry.detail}
            </div>
          )}
          {match.snippet && (
            <div className="truncate text-[10px] text-[#a9b1d6]" title={match.snippet}>
              {match.snippet}
            </div>
          )}
        </button>
        <button
          ref={newZoneRef}
          data-ui-bridge-id={`${id}-new-zone`}
          data-effect="write"
          onClick={() => onActivate(true)}
          className="shrink-0 rounded px-1 text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
          title={entry.source === "live" ? "Show in a new zone" : "Resume in a new zone"}
        >
          New zone
        </button>
      </div>
    </li>
  );
}
