/**
 * The Session Manager query bar (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D5 / Phase 4): the grammar input with inline token highlighting, the chip
 * row, the account and group pickers, and the saved views.
 *
 * One query string is the whole filter. Every chip, picker and saved view is
 * an editor of it (`sessionQueryChips.ts`), so what the box shows is exactly
 * what is filtered, and an agent, a UI Bridge `sendKeys` and a click all speak
 * the same grammar. A parse error is shown under the box, term by term, and
 * filters nothing — a mistyped field is never silently dropped.
 *
 * Presentational: the query, its parse and the counts come from
 * `useSessionQuery`; saved views are read from and written to the Phase 2
 * organization store by the caller.
 */

import { startTransition, useEffect, useReducer, useRef, useState } from "react";
import { Bookmark, Search, X } from "lucide-react";
import {
  UIBridgeComponentScope,
  useUIComponent,
  useUIElement,
  type StandardAction,
} from "@qontinui/ui-bridge";

import { guardedHandler } from "@/lib/ui-bridge/guardedHandler";

import { textArg } from "./commands";
import type { SavedView, SessionGroup } from "./sessionOrgTypes";
import { highlightQuery, type HighlightKind, type ParsedQuery } from "./sessionQuery";
import { initialQueryDraft, queryDraftReducer } from "./sessionQueryDraft";
import {
  QUERY_CHIPS,
  SESSION_QUERY_CONTROLS,
  UNGROUPED_VALUE,
  applySavedView,
  chipActive,
  groupOptions,
  pickAccount,
  pickGroup,
  saveViewRefusal,
  savedViewActive,
  selectedAccount,
  selectedGroup,
  sessionQueryControlAttrs,
  sessionQueryControlId,
  toggleChip,
  type ChipCount,
  type QueryChip,
} from "./sessionQueryChips";

/** The component id the bar's actions register under. */
export const SESSION_QUERY_COMPONENT_ID = "session-query";

/**
 * Actions the query input advertises — `sendKeys` included, since filtering
 * the list means typing into this box (the same reason
 * `SessionManagerHeader`'s search box advertises it).
 */
export const QUERY_INPUT_ACTIONS: StandardAction[] = [
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

/** Highlight colour per segment kind. */
const HIGHLIGHT_CLASS: Readonly<Record<HighlightKind, string>> = {
  space: "",
  word: "text-[#a9b1d6]",
  phrase: "text-[#bb9af7]",
  "field-name": "text-[#7aa2f7]",
  "field-value": "text-[#9ece6a]",
  operator: "text-[#bb9af7] font-semibold",
  negate: "text-[#7dcfff]",
  paren: "text-[#565f89]",
  error: "text-[#f7768e] underline decoration-wavy",
};

const NAME_SCHEMA = { name: "string" } as const;
const QUERY_SCHEMA = { query: "string" } as const;
const CHIP_SCHEMA = { key: "string" } as const;

export interface SessionQueryBarProps {
  query: string;
  onQueryChange: (q: string) => void;
  /** `useSessionQuery().parsed`. */
  parsed: ParsedQuery;
  /** `useSessionQuery().chipCounts`, keyed by chip key. */
  chipCounts: Readonly<Record<string, ChipCount>>;
  /** Account labels the picker offers. */
  accounts: readonly string[];
  /** The org's groups; `null` while the org is loading or unknown. */
  groups: readonly SessionGroup[] | null;
  /** The org's saved views; `null` while the org is loading or unknown. */
  savedViews: readonly SavedView[] | null;
  /** Why `groups` / `savedViews` are null, for the disabled controls' tooltips. */
  orgUnavailableReason?: string | null;
  onSaveView: (name: string, query: string) => void;
  onDeleteView: (name: string) => void;
  /** The filter's outcome, for the summary line; omit to hide it. */
  summary?: { matched: number; unknown: number; total: number } | null;
}

export function SessionQueryBar(props: SessionQueryBarProps) {
  const { query, onQueryChange, parsed, chipCounts, savedViews, onSaveView, onDeleteView } = props;

  const chipByKey = (key: string): QueryChip => {
    const chip = QUERY_CHIPS.find((c) => c.key === key);
    if (!chip) {
      throw new Error(`No chip "${key}". Available: ${QUERY_CHIPS.map((c) => c.key).join(", ")}`);
    }
    return chip;
  };

  useUIComponent({
    id: SESSION_QUERY_COMPONENT_ID,
    name: "Session query",
    description:
      "The Session Manager's filter: one query string in the D5 grammar (is:, where:, " +
      'repo:, text:"…", idle>30m, OR, -negation). Chips and saved views edit it.',
    actions: [
      {
        id: "get-query",
        label: "Get Query",
        // `read` — returns the query, its parse and every chip's state.
        effect: "read",
        handler: () => ({
          query,
          ok: parsed.ok,
          errors: parsed.ok ? [] : parsed.errors,
          chips: QUERY_CHIPS.map((c) => ({
            key: c.key,
            token: c.token,
            active: chipActive(query, c),
            count: chipCounts[c.key]?.matched ?? null,
            unknown: chipCounts[c.key]?.unknown ?? null,
          })),
        }),
      },
      {
        id: "set-query",
        label: "Set Query",
        paramSchema: QUERY_SCHEMA,
        // `read` — narrows what the list shows; no stored state changes.
        effect: "read",
        handler: guardedHandler("set-query", QUERY_SCHEMA, (args) => {
          onQueryChange(textArg(args, "query"));
        }),
      },
      {
        id: "clear-query",
        label: "Clear Query",
        // `read` — shows every session again.
        effect: "read",
        handler: () => onQueryChange(""),
      },
      {
        id: "toggle-chip",
        label: "Toggle Chip",
        description: `Toggle a chip's token. Keys: ${QUERY_CHIPS.map((c) => c.key).join(", ")}.`,
        paramSchema: CHIP_SCHEMA,
        // `read` — edits the filter only.
        effect: "read",
        handler: guardedHandler("toggle-chip", CHIP_SCHEMA, (args) => {
          onQueryChange(toggleChip(query, chipByKey(textArg(args, "key"))));
        }),
      },
      {
        id: "save-view",
        label: "Save View",
        description: "Save the current query as a named view in the organization store.",
        paramSchema: NAME_SCHEMA,
        // `write` — adds (or replaces) one saved view; deletable.
        effect: "write",
        handler: guardedHandler("save-view", NAME_SCHEMA, (args) => {
          const name = textArg(args, "name");
          const refusal = saveViewRefusal(name, query);
          if (refusal) throw new Error(refusal);
          onSaveView(name.trim(), query.trim());
        }),
      },
      {
        id: "delete-view",
        label: "Delete View",
        paramSchema: NAME_SCHEMA,
        // `write` — removes one saved view; its query can be retyped.
        effect: "write",
        handler: guardedHandler("delete-view", NAME_SCHEMA, (args) => {
          const name = textArg(args, "name");
          if (!savedViews?.some((v) => v.name === name)) {
            throw new Error(`No saved view "${name}"`);
          }
          onDeleteView(name);
        }),
      },
    ],
  });

  return (
    <UIBridgeComponentScope componentId={SESSION_QUERY_COMPONENT_ID}>
      <div
        {...sessionQueryControlAttrs("root")}
        data-query={query}
        data-query-ok={parsed.ok ? "true" : "false"}
        data-matched-count={props.summary?.matched}
        data-unknown-count={props.summary?.unknown}
        className="flex flex-col gap-1 px-3 pb-2"
      >
        <QueryInput query={query} onQueryChange={onQueryChange} />
        {!parsed.ok && <QueryErrors parsed={parsed} />}
        {props.summary && parsed.ok && query.trim() !== "" && (
          <div
            data-testid="session-query-summary"
            className="text-[10px] text-[#565f89]"
            title="Sessions shown of all sessions; unknown = the filter could not be evaluated"
          >
            {props.summary.matched} of {props.summary.total}
            {props.summary.unknown > 0 && ` · ${props.summary.unknown} unknown`}
          </div>
        )}
        <div className="flex flex-wrap items-center gap-1">
          {QUERY_CHIPS.map((chip) => (
            <QueryChipButton
              key={chip.key}
              chip={chip}
              count={chipCounts[chip.key]}
              active={chipActive(query, chip)}
              onToggle={() => onQueryChange(toggleChip(query, chip))}
            />
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-1">
          <AccountPicker {...props} />
          <GroupPicker {...props} />
        </div>
        <SavedViews
          query={query}
          parsed={parsed}
          savedViews={savedViews}
          orgUnavailableReason={props.orgUnavailableReason ?? null}
          onApply={(v) => onQueryChange(applySavedView(query, v))}
          onSaveView={onSaveView}
          onDeleteView={onDeleteView}
        />
      </div>
    </UIBridgeComponentScope>
  );
}

/** The grammar box: a transparent input over its own highlighted text. */
function QueryInput({
  query,
  onQueryChange,
}: {
  query: string;
  onQueryChange: (q: string) => void;
}) {
  const { ref } = useUIElement({
    id: SESSION_QUERY_CONTROLS.input.id,
    type: "input",
    label: "Filter sessions (query)",
    actions: QUERY_INPUT_ACTIONS,
  });
  const { ref: clearRef } = useUIElement({
    id: SESSION_QUERY_CONTROLS.clear.id,
    type: "button",
    label: "Clear the session filter",
  });
  // The box renders its own draft (see `sessionQueryDraft.ts` for why it
  // cannot bind to `query` directly) and follows `query` only when it changes
  // from elsewhere.
  const [{ draft }, dispatch] = useReducer(queryDraftReducer, query, initialQueryDraft);
  useEffect(() => dispatch({ kind: "prop", query }), [query]);
  const edit = (value: string) => {
    dispatch({ kind: "typed", value });
    // Outside the input event's synchronous batch, so the box's own update
    // commits before React restores the controlled value.
    startTransition(() => onQueryChange(value));
  };
  const overlayRef = useRef<HTMLDivElement>(null);
  const segments = highlightQuery(draft);
  return (
    <div className="relative rounded border border-[#2a2d3d] bg-[#1a1b26] focus-within:border-[#7aa2f7]/50">
      <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3 h-3 text-[#565f89]" />
      <div
        ref={overlayRef}
        aria-hidden
        data-testid="session-query-highlight"
        className="absolute inset-y-0 left-7 right-6 py-1 text-xs whitespace-pre overflow-hidden pointer-events-none"
      >
        {segments.map((s) => (
          <span key={s.start} data-kind={s.kind} className={HIGHLIGHT_CLASS[s.kind]}>
            {s.text}
          </span>
        ))}
      </div>
      <input
        ref={ref}
        {...sessionQueryControlAttrs("input")}
        aria-label="Filter sessions"
        type="text"
        spellCheck={false}
        autoComplete="off"
        value={draft}
        onChange={(e) => edit(e.target.value)}
        onScroll={(e) => {
          if (overlayRef.current) overlayRef.current.scrollLeft = e.currentTarget.scrollLeft;
        }}
        placeholder='is:needs-you repo:coord text:"merge train"'
        title="Words match title, topic and preview. Fields: is: where: group: color: tag: account: repo: branch: pr: plan: unit: device: page: title: text: started: — and idle>30m, age<2h, msgs>100, -negation, OR"
        className="relative w-full pl-7 pr-6 py-1 text-xs bg-transparent text-transparent caret-[#c0caf5] placeholder-[#414868] focus:outline-none"
      />
      {draft !== "" && (
        <button
          ref={clearRef}
          {...sessionQueryControlAttrs("clear")}
          onClick={() => edit("")}
          className="absolute right-1 top-1/2 -translate-y-1/2 p-0.5 rounded text-[#565f89] hover:text-[#c0caf5]"
          title="Clear the filter"
          aria-label="Clear the session filter"
        >
          <X className="w-3 h-3" />
        </button>
      )}
    </div>
  );
}

function QueryErrors({ parsed }: { parsed: ParsedQuery }) {
  if (parsed.ok) return null;
  return (
    <ul
      {...sessionQueryControlAttrs("errors")}
      role="alert"
      data-error-count={parsed.errors.length}
      className="text-[10px] text-[#f7768e] space-y-0.5"
    >
      {parsed.errors.map((e) => (
        <li key={`${e.start}-${e.code}`} data-code={e.code}>
          {e.message} — nothing is filtered until this is fixed
        </li>
      ))}
    </ul>
  );
}

/** One chip. Its own component so `useUIElement` is a plain hook per chip. */
function QueryChipButton({
  chip,
  count,
  active,
  onToggle,
}: {
  chip: QueryChip;
  count: ChipCount | undefined;
  active: boolean;
  onToggle: () => void;
}) {
  const id = sessionQueryControlId("chip", chip.key);
  const { ref } = useUIElement({ id, type: "button", label: `Filter: ${chip.label}` });
  const unknown = count?.unknown ?? 0;
  const title =
    `${chip.title} — toggles ${chip.token}` +
    (count ? ` (${count.matched} match${unknown > 0 ? `, ${unknown} unknown` : ""})` : "");
  return (
    <button
      ref={ref}
      {...sessionQueryControlAttrs("chip", chip.key)}
      data-token={chip.token}
      data-count={count?.matched}
      data-unknown-count={count?.unknown}
      aria-pressed={active}
      onClick={onToggle}
      title={title}
      className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] border transition-colors ${
        active
          ? "bg-[#7aa2f7]/15 text-[#7aa2f7] border-[#7aa2f7]/30"
          : "text-[#a9b1d6] border-[#2a2d3d] hover:bg-[#2a2d3d]"
      }`}
    >
      {chip.label}
      <span className="text-[#565f89]">{count ? count.matched : "?"}</span>
    </button>
  );
}

function AccountPicker({ query, onQueryChange, accounts }: SessionQueryBarProps) {
  const { ref } = useUIElement({
    id: SESSION_QUERY_CONTROLS.accountPicker.id,
    type: "select",
    label: "Filter sessions by account",
  });
  const current = selectedAccount(query);
  const options =
    current !== null && !accounts.includes(current) ? [...accounts, current] : accounts;
  return (
    <select
      ref={ref}
      {...sessionQueryControlAttrs("accountPicker")}
      aria-label="Filter sessions by account"
      value={current ?? ""}
      onChange={(e) => onQueryChange(pickAccount(query, e.target.value || null))}
      className="text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50"
    >
      <option value="">Any account</option>
      {options.map((a) => (
        <option key={a} value={a}>
          {a}
        </option>
      ))}
    </select>
  );
}

function GroupPicker({ query, onQueryChange, groups, orgUnavailableReason }: SessionQueryBarProps) {
  const { ref } = useUIElement({
    id: SESSION_QUERY_CONTROLS.groupPicker.id,
    type: "select",
    label: "Filter sessions by group",
  });
  const current = selectedGroup(query);
  const options = groups ? groupOptions(groups) : [];
  const known = new Set(options.map((o) => o.value.toLowerCase()));
  return (
    <select
      ref={ref}
      {...sessionQueryControlAttrs("groupPicker")}
      aria-label="Filter sessions by group"
      disabled={groups === null}
      title={
        groups === null
          ? `Groups unknown: ${orgUnavailableReason ?? "store not read yet"}`
          : undefined
      }
      value={current ?? ""}
      onChange={(e) => onQueryChange(pickGroup(query, e.target.value || null))}
      className="text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded px-1 py-0.5 text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50 disabled:opacity-50"
    >
      <option value="">{groups === null ? "Groups unknown" : "Any group"}</option>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
      {current !== null && !known.has(current.toLowerCase()) && (
        <option value={current}>{current === UNGROUPED_VALUE ? "Ungrouped" : current}</option>
      )}
    </select>
  );
}

function SavedViews({
  query,
  parsed,
  savedViews,
  orgUnavailableReason,
  onApply,
  onSaveView,
  onDeleteView,
}: {
  query: string;
  parsed: ParsedQuery;
  savedViews: readonly SavedView[] | null;
  orgUnavailableReason: string | null;
  onApply: (v: SavedView) => void;
  onSaveView: (name: string, query: string) => void;
  onDeleteView: (name: string) => void;
}) {
  const { ref: saveRef } = useUIElement({
    id: SESSION_QUERY_CONTROLS.saveView.id,
    type: "button",
    label: "Save the filter as a view",
  });
  const { ref: nameRef } = useUIElement({
    id: SESSION_QUERY_CONTROLS.saveViewName.id,
    type: "input",
    label: "Saved view name",
    actions: QUERY_INPUT_ACTIONS,
  });
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState("");

  if (savedViews === null) {
    return (
      <div className="text-[10px] text-[#565f89]" data-testid="session-query-views-unknown">
        Saved views unknown: {orgUnavailableReason ?? "store not read yet"}
      </div>
    );
  }
  const refusal = saveViewRefusal(name, query);
  const canStartSaving = parsed.ok && query.trim() !== "";
  return (
    <div className="flex flex-wrap items-center gap-1">
      {savedViews.map((v) => (
        <SavedViewChip
          key={v.name}
          view={v}
          active={savedViewActive(query, v)}
          onApply={() => onApply(v)}
          onDelete={() => onDeleteView(v.name)}
        />
      ))}
      {naming ? (
        <form
          className="flex items-center gap-1"
          onSubmit={(e) => {
            e.preventDefault();
            if (refusal) return;
            onSaveView(name.trim(), query.trim());
            setName("");
            setNaming(false);
          }}
        >
          <input
            ref={nameRef}
            {...sessionQueryControlAttrs("saveViewName")}
            aria-label="Saved view name"
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onBlur={() => {
              if (name.trim() === "") setNaming(false);
            }}
            placeholder="View name"
            title={refusal ?? "Press Enter to save"}
            className="w-24 px-1 py-0.5 text-[10px] bg-[#1a1b26] border border-[#2a2d3d] rounded text-[#a9b1d6] focus:outline-none focus:border-[#7aa2f7]/50"
          />
        </form>
      ) : (
        <button
          ref={saveRef}
          {...sessionQueryControlAttrs("saveView")}
          disabled={!canStartSaving}
          onClick={() => setNaming(true)}
          title={canStartSaving ? "Save this filter as a view" : "Type a valid query to save it"}
          className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] disabled:opacity-40"
        >
          <Bookmark className="w-3 h-3" />
          Save view
        </button>
      )}
    </div>
  );
}

function SavedViewChip({
  view,
  active,
  onApply,
  onDelete,
}: {
  view: SavedView;
  active: boolean;
  onApply: () => void;
  onDelete: () => void;
}) {
  const id = sessionQueryControlId("savedView", view.name);
  const deleteId = sessionQueryControlId("deleteView", view.name);
  const { ref } = useUIElement({ id, type: "button", label: `Saved view ${view.name}` });
  const { ref: deleteRef } = useUIElement({
    id: deleteId,
    type: "button",
    label: `Delete saved view ${view.name}`,
  });
  return (
    <span
      className={`flex items-center rounded border text-[10px] ${
        active ? "bg-[#7aa2f7]/15 border-[#7aa2f7]/30" : "border-[#2a2d3d]"
      }`}
    >
      <button
        ref={ref}
        {...sessionQueryControlAttrs("savedView", view.name)}
        data-view-query={view.query}
        aria-pressed={active}
        onClick={onApply}
        title={view.query}
        className={`px-1.5 py-0.5 ${active ? "text-[#7aa2f7]" : "text-[#a9b1d6]"}`}
      >
        {view.name}
      </button>
      <button
        ref={deleteRef}
        {...sessionQueryControlAttrs("deleteView", view.name)}
        onClick={onDelete}
        title={`Delete the saved view "${view.name}"`}
        aria-label={`Delete saved view ${view.name}`}
        className="pr-1 text-[#565f89] hover:text-[#c0caf5]"
      >
        <X className="w-2.5 h-2.5" />
      </button>
    </span>
  );
}
