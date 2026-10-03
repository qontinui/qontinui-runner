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

import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
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

function ConnectLink({
  idPrefix,
  url,
  launched,
}: {
  idPrefix: string;
  url: string;
  launched: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const { ref: openRef } = useUIElement({
    id: `${idPrefix}-connect-link`,
    label: "Open the workspace sign-in link",
    type: "button",
  });
  const { ref: copyRef } = useUIElement({
    id: `${idPrefix}-copy-link`,
    label: "Copy the workspace sign-in link",
    type: "button",
  });
  return (
    <div style={{ opacity: launched ? 0.7 : 1 }}>
      {launched ? "Browser didn\u2019t open? " : "The browser could not be opened. "}
      <button
        ref={openRef}
        type="button"
        onClick={() => void openUrl(url).catch(() => undefined)}
        style={linkStyle}
      >
        Open the sign-in link
      </button>{" "}
      ·{" "}
      <button
        ref={copyRef}
        type="button"
        onClick={() =>
          navigator.clipboard
            .writeText(url)
            .then(() => setCopied(true))
            .catch(() => setCopied(false))
        }
        style={linkStyle}
      >
        {copied ? "Copied" : "Copy link"}
      </button>
      {!launched ? (
        <div style={{ wordBreak: "break-all", userSelect: "all", opacity: 0.8 }}>{url}</div>
      ) : null}
    </div>
  );
}

export function PairAllProgress({
  idPrefix,
  phase,
  connectLink,
  cancellable,
  results,
  view,
  onCancel,
}: {
  /** UI Bridge id prefix, e.g. `binding-gap` or `settings-workspaces`. */
  idPrefix: string;
  phase: PairAllPhase;
  connectLink: { url: string; launched: boolean } | null;
  /** False once the browser has called back: the flow can no longer be cancelled. */
  cancellable: boolean;
  results: TenantPairResult[] | null;
  view: BindingGapView | null;
  onCancel: () => void;
}) {
  const { ref: cancelRef } = useUIElement({
    id: `${idPrefix}-cancel`,
    label: "Cancel workspace sign-in",
    type: "button",
  });

  return (
    <>
      {phase === "waiting" ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: "0.75rem" }}>
          {connectLink ? (
            <ConnectLink
              idPrefix={idPrefix}
              url={connectLink.url}
              launched={connectLink.launched}
            />
          ) : null}
          {!cancellable ? (
            <div style={{ opacity: 0.7 }}>Saving your workspace credentials…</div>
          ) : null}
          {cancellable ? (
            <div>
              <button ref={cancelRef} type="button" onClick={onCancel} style={linkStyle}>
                Cancel
              </button>
            </div>
          ) : null}
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
