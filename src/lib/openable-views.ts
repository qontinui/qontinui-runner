/**
 * Openable-view registry (P1/P4 of plan
 * 2026-07-08-ui-bridge-reach-and-verify-gated-flows).
 *
 * The runner has no router: `App.tsx` renders views by conditional state, not
 * routes. So a view that only mounts under some app state (the setup wizard,
 * and the GitHub clone picker inside it) is unreachable by the UI Bridge — an
 * agent cannot `page/navigate` to it, and `snapshot`/`discover` only see what is
 * currently mounted. The only way to learn a view exists was to grep the TSX.
 *
 * This registry closes that gap. A component registers an **opener** — a
 * callback that drives the app into the named state — and the bridge can then
 * enumerate the registrations (`GET /ui-bridge/views`, P4) and invoke one
 * (`POST /ui-bridge/control/view/open`, P1).
 *
 * # Openers MUST be non-destructive
 *
 * An opener flips **in-memory view state only**. It must never clear persisted
 * data to force a view to mount. Opening the setup wizard sets the in-memory
 * `setupCompleted` flag to `false`; it must NOT call a command that clears the
 * persisted `setup_completed` setting, or "let me look at the wizard" would
 * silently reset the operator's install — the exact footgun the plan calls out.
 *
 * The HTTP surface that reaches these openers is additionally gated by a
 * capability flag (`QONTINUI_UI_BRIDGE_VIEW_CONTROL=1`, off by default), so a
 * released operator build ships with view control inert. See
 * `src-tauri/src/mcp/ui_bridge/gated_flow.rs`.
 */

/** A registered openable view. */
export interface OpenableView {
  /** Stable name the bridge addresses, e.g. `"setup-wizard"`. */
  name: string;
  /** Human-readable description of what mounts when this is opened. */
  description: string;
  /**
   * What must already be true for the opener to work, in prose — e.g. "none"
   * for a view that can always be opened, or "setup-wizard must be open" for a
   * sub-step. Surfaced by `GET /ui-bridge/views` so an agent can order its
   * calls without reading source.
   */
  preconditions: string;
  /**
   * Drives the app into this view. MUST be non-destructive: flip in-memory view
   * state, never wipe persisted data. Registrants are responsible for honouring
   * this — see the module docs.
   */
  open: () => void | Promise<void>;
}

/** The live registry. Module-scoped so it survives re-renders. */
const registry = new Map<string, OpenableView>();

/**
 * Register (or replace) an openable view. Returns an unregister function, so a
 * component can register on mount and clean up on unmount:
 *
 * ```ts
 * useEffect(
 *   () => registerOpenableView({
 *     name: "setup-wizard",
 *     description: "First-launch setup wizard",
 *     preconditions: "none",
 *     open: () => setSetupCompleted(false), // in-memory only
 *   }),
 *   [],
 * );
 * ```
 *
 * Replacing on re-register (rather than throwing) keeps React StrictMode's
 * double-invoked effects and hot reload from wedging the registry.
 */
export function registerOpenableView(view: OpenableView): () => void {
  registry.set(view.name, view);
  return () => {
    // Only remove if this exact registration is still the live one — a
    // remount may have already replaced it.
    if (registry.get(view.name) === view) {
      registry.delete(view.name);
    }
  };
}

/**
 * Every registered view, minus the opener callbacks (which aren't serializable).
 * This is what `GET /ui-bridge/views` returns.
 */
export function listOpenableViews(): Omit<OpenableView, "open">[] {
  return Array.from(registry.values())
    .map(({ name, description, preconditions }) => ({
      name,
      description,
      preconditions,
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * Invoke a registered opener. Resolves `false` when no view is registered under
 * `name` (the caller turns that into a 404-ish error naming the known views),
 * `true` once the opener has run.
 */
export async function openView(name: string): Promise<boolean> {
  const view = registry.get(name);
  if (!view) return false;
  await view.open();
  return true;
}

/** Test-only: drop all registrations. */
export function __resetOpenableViewsForTest(): void {
  registry.clear();
}
