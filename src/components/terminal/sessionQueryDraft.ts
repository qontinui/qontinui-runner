/**
 * The query box's own text, kept apart from the Session Manager's query state
 * (plan `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 8).
 *
 * ## Why the box does not bind to the manager's query directly
 *
 * The query lives in `useSessionQuery`, at the top of the Terminal page's
 * session context, and a change to it re-renders that whole tree. Bound
 * straight to it (`value={query}`), an edit's re-render did not commit before
 * React restored the controlled input — so React put the OLD text back into
 * the DOM. A keystroke survived that (its later render caught up), but the UI
 * Bridge `type` / `clear` actions call `onChange` a second time after their
 * input event, read the restored old text, and wrote it back: the query never
 * changed (the shape: `onChange "is:idle"`, then `onChange ""`, then renders
 * `"is:idle"`, `""`).
 *
 * So the box renders a local draft — a cheap update in the box's own component,
 * the shape the palette and group-rename inputs already use — and forwards each
 * edit to the manager outside the event's synchronous batch. The draft follows
 * the manager's query only when that query changes from elsewhere (a chip, a
 * picker, a saved view, `set-query`, Clear), never when it is merely catching up
 * with what the box already sent.
 */

export interface QueryDraftState {
  /** What the box shows. */
  draft: string;
  /**
   * Queries the box sent that the manager has not rendered back yet, oldest
   * first. Forwarding is a transition, so an echo can arrive after a later
   * edit; an echo of a value in here is the box's own, never a change from
   * elsewhere, and must not roll the draft back.
   */
  pending: readonly string[];
  /** The manager's query as the box last saw it. */
  seen: string;
}

export type QueryDraftEvent =
  /** The operator (or a driver) edited the box. */
  | { kind: "typed"; value: string }
  /** The manager's query, as the box is rendered with it. */
  | { kind: "prop"; query: string };

export function initialQueryDraft(query: string): QueryDraftState {
  return { draft: query, pending: [], seen: query };
}

/** The next draft state. Returns `state` itself when nothing changes. */
export function queryDraftReducer(state: QueryDraftState, event: QueryDraftEvent): QueryDraftState {
  if (event.kind === "typed") {
    if (event.value === state.draft) return state;
    return { ...state, draft: event.value, pending: [...state.pending, event.value] };
  }
  const q = event.query;
  if (q === state.seen) return state;
  if (q === state.draft) {
    // The manager now shows exactly what the box shows: it has caught up with
    // the box's latest edit, so nothing the box sent is still in flight. Any
    // pending value left behind (typing a → ab → a leaves "ab") would
    // otherwise later mistake an external change to that exact text for an
    // echo. The trade-off this picks: echoes carry no sequence number, so if
    // the manager renders an intermediate edit ("ab") AFTER one equal to the
    // draft, that late echo is adopted as an outside change and briefly rolls
    // the box back. That window is one render; mistaking a real outside change
    // for an echo (the other choice) would hide it until the next edit.
    return { ...state, pending: [], seen: q };
  }
  const echo = state.pending.indexOf(q);
  if (echo >= 0) {
    // The box's own edit coming back: keep the draft, drop it and anything
    // older (a transition may skip intermediate values).
    return { ...state, pending: state.pending.slice(echo + 1), seen: q };
  }
  // A change from elsewhere (chip, picker, saved view, set-query): adopt it.
  return { draft: q, pending: [], seen: q };
}
