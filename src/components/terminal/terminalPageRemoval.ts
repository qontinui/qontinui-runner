/**
 * "A terminal page was removed": the signal `useTerminalPages.removePage` gives
 * the rest of this window once the page has left the persisted page list.
 *
 * The session restore (`useTerminalInitialization`) owns state keyed by page
 * that must not outlive the page: a guard retry, its published wait and its
 * auto-save hold, and the orphan-adoption claim. It cannot learn of a removal
 * from its own runs, because a page restores only once and the pages left
 * behind may have finished restoring long before. So the removal says so, at
 * the moment it happens.
 *
 * A module-level set of listeners rather than React context, for the reason
 * `lib/resourceGuard.ts` gives for its store: the page list lives at the App
 * root and the restore deep inside `TerminalPage`, and the only payload is one
 * page id. A page removed in ANOTHER window does not reach this window's
 * listeners, and need not: windows do not sync page lists, so that page still
 * exists here, and what the restore holds for it stays with it.
 */

type PageRemovedListener = (pageId: string) => void;

const listeners = new Set<PageRemovedListener>();

/** Be told each time a page is removed. Returns the unsubscribe. */
export function subscribeTerminalPageRemoved(listener: PageRemovedListener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** `pageId` was removed from the page list. Called by `removePage` only. */
export function notifyTerminalPageRemoved(pageId: string): void {
  for (const listener of [...listeners]) listener(pageId);
}
