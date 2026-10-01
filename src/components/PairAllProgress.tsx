/**
 * The in-flight and finished states of "Connect all my workspaces", shared by
 * the binding-gap banner and the Settings card's Workspaces section so both
 * render the one flow identically (plan
 * `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`).
 *
 * While waiting: the connect link — prominently when the runner could not
 * launch a browser, as a quiet fallback otherwise — and a Cancel button, so an
 * abandoned tab never strands the user for the flow's 5-minute timeout.
 * After: one result row per workspace, named from the view.
 */

import { useUIElement } from "@qontinui/ui-bridge";

import {
  displayNameFor,
  pairResultLabel,
  type BindingGapView,
  type TenantPairResult,
} from "./binding-gap-ask-logic";
import { shortTenantId } from "./terminal/SpawnTenantPicker";
import type { PairAllPhase } from "./useBindingGaps";

const linkStyle = {
  background: "none",
  border: "none",
  color: "inherit",
  padding: 0,
  textDecoration: "underline",
  cursor: "pointer",
} as const;

export function PairAllProgress({
  idPrefix,
  phase,
  connectLink,
  results,
  view,
  onCancel,
}: {
  /** UI Bridge id prefix, e.g. `binding-gap` or `settings-workspaces`. */
  idPrefix: string;
  phase: PairAllPhase;
  connectLink: { url: string; launched: boolean } | null;
  results: TenantPairResult[] | null;
  view: BindingGapView | null;
  onCancel: () => void;
}) {
  const { ref: cancelRef } = useUIElement({
    id: `${idPrefix}-cancel`,
    label: "Cancel workspace sign-in",
    type: "button",
  });
  const { ref: linkRef } = useUIElement({
    id: `${idPrefix}-connect-link`,
    label: "Open the workspace sign-in link",
    type: "generic",
  });

  return (
    <>
      {phase === "waiting" ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: "0.75rem" }}>
          {connectLink && !connectLink.launched ? (
            <div>
              The browser could not be opened. Open this link to sign in:{" "}
              <a ref={linkRef} href={connectLink.url} target="_blank" rel="noreferrer">
                {connectLink.url}
              </a>
            </div>
          ) : connectLink ? (
            <div style={{ opacity: 0.7 }}>
              Browser didn&rsquo;t open?{" "}
              <a ref={linkRef} href={connectLink.url} target="_blank" rel="noreferrer">
                Open the sign-in link
              </a>
            </div>
          ) : null}
          <div>
            <button ref={cancelRef} type="button" onClick={onCancel} style={linkStyle}>
              Cancel
            </button>
          </div>
        </div>
      ) : null}
      {results && results.length > 0 ? (
        <ul
          data-ui-id={`${idPrefix}-results`}
          style={{ margin: 0, paddingLeft: 0, listStyle: "none", fontSize: "0.75rem" }}
        >
          {results.map((r) => (
            <li key={r.tenantId}>
              <span title={r.tenantId} style={{ fontFamily: "monospace" }}>
                {displayNameFor(view, r.tenantId) ?? shortTenantId(r.tenantId)}
              </span>
              : {pairResultLabel(r)}
            </li>
          ))}
        </ul>
      ) : null}
    </>
  );
}
