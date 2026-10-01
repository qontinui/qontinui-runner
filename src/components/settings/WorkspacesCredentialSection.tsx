/**
 * WorkspacesCredentialSection.tsx
 *
 * The per-tenant half of the Settings card's backend connection (plan
 * `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`,
 * D3). "This device is connected" answers a DEVICE question — registered with
 * qontinui-web, heartbeat up — and was true on a box where two of its three
 * workspaces could not run autonomous sessions. This section answers the
 * per-workspace question beside it: one row per tenant in
 * `coord_bound_tenants ∪ slots`, each `connected` / `no credential` /
 * `unknown`, from `get_binding_gaps` — the same view the binding-gap banner
 * renders. An unknown view is said as unknown, never as connected.
 */

import { useUIElement } from "@qontinui/ui-bridge";
import { AlertTriangle, CheckCircle2, HelpCircle, Loader2, XCircle } from "lucide-react";

import {
  credentialStateLabel,
  rowsNeedingConnect,
  type BindingGapRow,
  type TenantCredentialState,
} from "../binding-gap-ask-logic";
import { PairAllProgress } from "../PairAllProgress";
import { shortTenantId } from "../terminal/SpawnTenantPicker";
import { useBindingGapView, usePairAllTenants } from "../useBindingGaps";

function StateIcon({ state }: { state: TenantCredentialState }) {
  switch (state) {
    case "connected":
      return <CheckCircle2 className="w-3.5 h-3.5 text-green-500 shrink-0" />;
    case "no_credential":
      return <XCircle className="w-3.5 h-3.5 text-destructive shrink-0" />;
    case "unknown":
      return <HelpCircle className="w-3.5 h-3.5 text-muted-foreground shrink-0" />;
  }
}

function WorkspaceRow({ row }: { row: BindingGapRow }) {
  const { ref } = useUIElement({
    id: `settings-workspace-row-${row.tenantId}`,
    label: `Workspace ${row.displayName ?? shortTenantId(row.tenantId)}: ${credentialStateLabel(row.state)}`,
    type: "generic",
  });
  return (
    <li ref={ref} className="flex items-center gap-2 text-xs" data-state={row.state}>
      <StateIcon state={row.state} />
      <span className="font-mono" title={row.tenantId}>
        {row.displayName ?? shortTenantId(row.tenantId)}
      </span>
      <span className="text-muted-foreground">{credentialStateLabel(row.state)}</span>
    </li>
  );
}

export function WorkspacesCredentialSection() {
  const { view, refresh } = useBindingGapView();
  const { phase, results, error, connectLink, connect, cancel } = usePairAllTenants(refresh);
  const { ref: sectionRef } = useUIElement({
    id: "settings-workspaces",
    label: "Workspaces credential state",
    type: "generic",
  });
  const { ref: connectRef } = useUIElement({
    id: "settings-workspaces-connect-all",
    label: "Connect all my workspaces",
    type: "button",
  });

  const needing = rowsNeedingConnect(view);
  const busy = phase === "waiting";

  return (
    <div ref={sectionRef} className="space-y-2 rounded-lg bg-card/50 p-4">
      <div className="text-sm font-medium">Workspaces</div>
      {view === null ? (
        <p className="text-xs text-muted-foreground">
          Unknown — this runner did not report which workspaces hold a credential.
        </p>
      ) : (
        <>
          {view.status === "unknown" ? (
            <p className="text-[11px] text-muted-foreground flex items-start gap-1">
              <AlertTriangle className="w-3 h-3 shrink-0 mt-0.5" />
              <span>Unknown: {view.reason}. Connecting is offered once this is known.</span>
            </p>
          ) : null}
          {view.rows.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              {view.status === "measured"
                ? "This device is bound to no workspace."
                : "No workspace could be listed."}
            </p>
          ) : (
            <ul className="space-y-1">
              {view.rows.map((row) => (
                <WorkspaceRow key={row.tenantId} row={row} />
              ))}
            </ul>
          )}
        </>
      )}
      {needing.length > 0 ? (
        <div className="space-y-1.5">
          <button
            ref={connectRef}
            type="button"
            disabled={busy}
            onClick={() => void connect(needing.map((r) => r.tenantId))}
            className="inline-flex items-center gap-2 rounded-md bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            {busy ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : null}
            {busy ? "Waiting for browser…" : "Connect all my workspaces"}
          </button>
          <p className="text-[11px] text-muted-foreground">
            One browser sign-in connects every workspace above that has no credential. Your home
            workspace does not change.
          </p>
        </div>
      ) : null}
      <PairAllProgress
        idPrefix="settings-workspaces"
        phase={phase}
        connectLink={connectLink}
        results={results}
        view={view}
        onCancel={() => void cancel()}
      />
      {error ? (
        <p className="text-[11px] text-destructive flex items-center gap-1">
          <AlertTriangle className="w-3 h-3 shrink-0" /> Could not connect: {error}
        </p>
      ) : null}
    </div>
  );
}
