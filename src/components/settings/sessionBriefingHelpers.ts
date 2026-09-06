/**
 * Pure helpers for `SessionBriefingPanel`.
 *
 * Lives outside the JSX module for the reason the sibling
 * `lockYieldPolicyHelpers.ts` records: the runner's vitest config runs
 * `environment: "node"` with no jsdom, so a settings panel's logic is only
 * testable once it is factored out of the component.
 *
 * What is worth factoring out here is the HONESTY rules. The panel reports
 * where the text in a session's system prompt came from, and its two metadata
 * fields — the document version and the last-confirmed stamp — each have an
 * absent form that the Rust side treats as UNKNOWN and the panel used to render
 * as though it were a fact.
 */

/** `coord` | `cached` | `builtin`, as `GET /session-briefing` reports it. */
export type BriefingProvenanceKind = string;

/** The provenance token the route reports for the compiled-in fallback. */
export const PROVENANCE_BUILTIN = "builtin";

/**
 * Colour a provenance token.
 *
 * `builtin` is deliberately NOT an error colour: it is the correct, expected
 * state on every runner until coord serves the documents, and painting it red
 * would train operators to ignore it.
 */
export function provenanceClasses(provenance: BriefingProvenanceKind): string {
  switch (provenance) {
    case "coord":
      return "bg-emerald-500/10 text-emerald-500";
    case "cached":
      return "bg-amber-500/10 text-amber-500";
    default:
      return "bg-muted text-muted-foreground";
  }
}

/**
 * The `version:` field, given the version the route reported and the
 * provenance of the same block.
 *
 * Two distinct absences, and they are NOT the same statement:
 *
 * - `null` with `builtin` provenance — nothing was rendered from a document,
 *   because the compiled-in fallback was used. That is a complete answer.
 * - anything else absent — the block DID come from a document whose version
 *   the runner cannot state. `0` lands here as well as `null`: version `0` is
 *   what a coord list row with no `current_version` and a store written by an
 *   older build both decode to, and `fleet_policy_poller` treats it as UNKNOWN
 *   on both sides for exactly that reason. Printing it as `v0` would present a
 *   missing value as a real version.
 */
export function formatDocumentVersion(
  version: number | null | undefined,
  provenance: BriefingProvenanceKind,
): string {
  if (version === null || version === undefined || version === 0) {
    return provenance === PROVENANCE_BUILTIN ? "— (compiled-in fallback)" : "— (unknown)";
  }
  return `v${version}`;
}

/**
 * The `last confirmed:` field.
 *
 * Same split as [`formatDocumentVersion`]. An empty string is an absence, not a
 * stamp: it is the serde default for a cache entry written by a build that
 * predates the field, which `fleet_policy_poller`'s `BriefingDial` already
 * reports as UNKNOWN rather than as an empty timestamp. Rendering it verbatim
 * left the panel printing `last confirmed:` followed by nothing at all.
 */
export function formatLastConfirmed(
  fetchedAt: string | null | undefined,
  provenance: BriefingProvenanceKind,
): string {
  if (fetchedAt === null || fetchedAt === undefined || fetchedAt.trim() === "") {
    return provenance === PROVENANCE_BUILTIN ? "never (compiled-in fallback)" : "unknown";
  }
  return fetchedAt;
}

/**
 * The one-sentence status of the fleet-gated plan-capture clause.
 *
 * The clause has TWO independent facts behind it and the panel used to show one
 * of them: the fleet dial is the AUTHORIZATION, and the coord document is only
 * the CONTENT. `GET /session-briefing` reports the document's provenance,
 * version and stamp whether or not the clause is in force, precisely so an
 * operator asking "why is my edited clause not in the prompt?" can see which
 * half is the answer. So the omitted arm gets a sentence that names the dial
 * and still hands the reader the document's state, rather than ending the
 * conversation at "omitted".
 *
 * Both facts, which is why `provenance` is a parameter rather than something
 * the reader is left to cross-check against the badge beside it. A sentence
 * that says an edit is "cached and ready" while the badge next to it reads
 * `builtin-fallback` is the same defect as dropping the document state
 * altogether — it answers the operator's question wrongly instead of not at
 * all. On the `builtin` arm there is no coord document behind this clause: what
 * a session gets, or would get, is the compiled-in text.
 */
export function describePlanCaptureClause(
  included: boolean,
  provenance: BriefingProvenanceKind,
): string {
  const fromDocument = provenance !== PROVENANCE_BUILTIN;
  if (included) {
    return fromDocument
      ? "Included — the fleet plan-capture dial is at `record`, so this document's text is appended to the briefing above."
      : "Included — the fleet plan-capture dial is at `record`, so a clause is appended to the briefing above. It is the compiled-in text, not a coord document: none was rendered.";
  }
  return fromDocument
    ? "Omitted — the fleet plan-capture dial is off for this tenant. The dial is the authorization, not the document: the state below is the document itself, which is cached and ready but not injected."
    : "Omitted — the fleet plan-capture dial is off for this tenant, AND no coord document was rendered for this clause. Turning the dial on would append the compiled-in text; the state below says why the document is not the one in play.";
}
