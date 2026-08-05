/**
 * Module-level store for the expand/collapse state of AI response sections
 * (`CollapsibleAiSection` in `AiMessageDisplay`).
 *
 * It has to live outside React state: the sections are rendered inside a list
 * that remounts on a session switch, on a parent re-render that changes the
 * element identity, and (once windowing lands) whenever a row scrolls out of
 * view. Without a store, every one of those resets the operator's choice — and
 * re-expanding a response also re-parses its markdown, so the reset is not just
 * visually wrong, it is the expensive direction.
 *
 * A `Map<string, boolean>`, NOT a `Set` of expanded keys. A set can only record
 * "expanded": a section the operator COLLAPSED whose `defaultExpanded` is true
 * looked exactly like one never touched, so it silently re-opened on the next
 * mount. Absent from the map means "never toggled" — only then does the
 * caller's default apply.
 *
 * Leaf module (zero imports) so vitest can exercise it under the runner's
 * `environment: "node"` config, which cannot render the component itself.
 */

const expandedSections = new Map<string, boolean>();

/**
 * The section's current expand state: the operator's last choice if there is
 * one, otherwise `defaultExpanded`.
 */
export function readSectionExpanded(sectionKey: string, defaultExpanded: boolean): boolean {
  return expandedSections.get(sectionKey) ?? defaultExpanded;
}

/** Record an explicit operator toggle, in either direction. */
export function setSectionExpanded(sectionKey: string, expanded: boolean): void {
  expandedSections.set(sectionKey, expanded);
}

/** Test seam: drop every recorded choice. Not used by the component. */
export function resetSectionExpandedStore(): void {
  expandedSections.clear();
}
