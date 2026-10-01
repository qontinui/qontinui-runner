/**
 * BindingGapAskBanner.tsx
 *
 * ONE banner for every workspace (tenant) this device is bound to but holds no
 * credential for, with a button that fixes it: "Connect all my workspaces"
 * opens one browser sign-in and pairs every listed workspace (plan
 * `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`,
 * D2). A per-workspace "Connect just this one" uses the same flow with one id.
 * The terminal command is still offered, behind "Use a terminal instead", for
 * boxes with no browser.
 *
 * Sources, read not received:
 * - the GAP LIST is `get_binding_gaps` — the same view the Settings card's
 *   Workspaces rows render — so the two cannot disagree;
 * - the ask records (`get_binding_gap_asks`) supply only the per-account,
 *   per-lapse dismissal key and the terminal command. A signed-out box (no
 *   ask account) shows no banner, as before.
 * Both are re-read on mount and on every `autonomy-binding-gap` nudge.
 *
 * States: idle → "Waiting for browser…" → per-workspace result rows →
 * auto-dismissed once a re-read of `get_binding_gaps` shows no gap.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useUIElement } from "@qontinui/ui-bridge";
import { KeyRound, Loader2, X } from "lucide-react";

import { useRunnerTier } from "@/hooks/useRunnerTier";

import {
  bannerGapEntries,
  BINDING_GAP_NUDGE_EVENT,
  gapsCleared,
  GET_BINDING_GAP_ASKS_CMD,
  normalizeBindingGapAsks,
  type BindingGapAsk,
  type BindingGapBannerEntry,
} from "./binding-gap-ask-logic";
import { shortTenantId } from "./terminal/SpawnTenantPicker";
import { PairAllProgress } from "./PairAllProgress";
import { useBindingGapView, usePairAllTenants } from "./useBindingGaps";

/** How long the result rows stay up once every gap has cleared. */
const AUTO_DISMISS_MS = 4000;

const buttonStyle = {
  background: "var(--accent, #6366f1)",
  color: "#fff",
  border: "none",
  borderRadius: 4,
  padding: "3px 10px",
  fontSize: "0.75rem",
  cursor: "pointer",
} as const;

const linkButtonStyle = {
  background: "none",
  border: "none",
  color: "inherit",
  padding: 0,
  fontSize: "0.75rem",
  textDecoration: "underline",
  cursor: "pointer",
} as const;

/** Display name when the runner knows one, else the short id. */
function workspaceLabel(e: { tenantId: string; displayName: string | null }): string {
  return e.displayName ?? shortTenantId(e.tenantId);
}

function GapRow({
  entry,
  busy,
  onConnect,
}: {
  entry: BindingGapBannerEntry;
  busy: boolean;
  onConnect: (tenantId: string) => void;
}) {
  const { ref } = useUIElement({
    id: `binding-gap-connect-one-${entry.tenantId}`,
    label: `Connect workspace ${workspaceLabel(entry)}`,
    type: "button",
  });
  return (
    <li style={{ display: "flex", alignItems: "center", gap: 8 }}>
      <span title={entry.tenantId} style={{ fontFamily: "monospace" }}>
        {workspaceLabel(entry)}
      </span>
      <button
        ref={ref}
        type="button"
        disabled={busy}
        onClick={() => onConnect(entry.tenantId)}
        style={linkButtonStyle}
      >
        Connect just this one
      </button>
    </li>
  );
}

export function BindingGapAskBanner() {
  const { tier } = useRunnerTier();
  const { view, refresh } = useBindingGapView();
  const { phase, results, error, connectLink, connect, cancel, reset } = usePairAllTenants(refresh);
  const [asks, setAsks] = useState<BindingGapAsk[] | null>(null);
  const [dismissed, setDismissed] = useState<Set<string>>(() => new Set());
  const [showTerminal, setShowTerminal] = useState(false);
  const [copied, setCopied] = useState(false);

  const { ref: rootRef } = useUIElement({
    id: "binding-gap-ask-banner",
    label: "Workspaces without a credential banner",
    type: "generic",
  });
  const { ref: connectAllRef } = useUIElement({
    id: "binding-gap-connect-all",
    label: "Connect all my workspaces",
    type: "button",
  });
  const { ref: terminalToggleRef } = useUIElement({
    id: "binding-gap-terminal-toggle",
    label: "Use a terminal instead",
    type: "button",
  });
  const { ref: dismissRef } = useUIElement({
    id: "binding-gap-dismiss",
    label: "Dismiss workspaces banner for this session",
    type: "button",
  });

  // The ask records: per-account dismissal keys + the terminal fallback.
  useEffect(() => {
    let cancelled = false;
    const fetchAsks = () => {
      invoke<unknown>(GET_BINDING_GAP_ASKS_CMD)
        .then((raw) => {
          if (!cancelled) setAsks(normalizeBindingGapAsks(raw));
        })
        .catch(() => {
          // Older runner build without the command, or a failed read: UNKNOWN,
          // so show nothing rather than a stale ask.
          if (!cancelled) setAsks(null);
        });
    };
    fetchAsks();
    const unlisten = listen(BINDING_GAP_NUDGE_EVENT, fetchAsks);
    return () => {
      cancelled = true;
      unlisten
        .then((fn) => fn())
        .catch(() => {
          /* listener cleanup is best-effort */
        });
    };
  }, []);

  // Auto-dismiss: once a pairing finished and the re-read shows no gap, leave
  // the result rows up briefly, then return to idle (which renders nothing).
  const cleared = gapsCleared(view);
  useEffect(() => {
    if (phase !== "done" || !cleared) return;
    const t = setTimeout(reset, AUTO_DISMISS_MS);
    return () => clearTimeout(t);
  }, [phase, cleared, reset]);

  if (tier !== "qontinui_account") return null;
  const entries = bannerGapEntries(view, asks, dismissed);
  const busy = phase === "waiting";
  if (entries.length === 0 && phase === "idle") return null;

  const allIds = entries.map((e) => e.tenantId);
  const terminalText = entries.map((e) => e.command).join("\n");
  const caveat = entries.find((e) => e.caveat)?.caveat ?? null;

  return (
    <div
      ref={rootRef}
      role="status"
      aria-live="polite"
      data-ui-id="binding-gap-ask-banner"
      style={{
        position: "fixed",
        bottom: 16,
        right: 16,
        zIndex: 9998,
        display: "flex",
        gap: 10,
        padding: "10px 12px",
        maxWidth: "min(520px, calc(100vw - 32px))",
        background: "var(--bg-tertiary, #242837)",
        color: "var(--text-primary, #e4e4e7)",
        border: "1px solid var(--accent, #6366f1)",
        borderRadius: 8,
        boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
        fontSize: "0.8125rem",
      }}
    >
      <KeyRound className="w-4 h-4 shrink-0" aria-hidden="true" />
      <div style={{ display: "flex", flexDirection: "column", gap: 6, minWidth: 0, flex: 1 }}>
        {entries.length > 0 ? (
          <>
            <div style={{ fontWeight: 600 }}>
              {entries.length === 1
                ? "One of your workspaces can't run autonomous sessions on this device."
                : `${entries.length} of your workspaces can't run autonomous sessions on this device.`}
            </div>
            <div style={{ opacity: 0.8 }}>
              This device is bound to them but holds no credential. Connect once to restore them.
            </div>
            <ul style={{ margin: 0, paddingLeft: 0, listStyle: "none" }}>
              {entries.map((e) => (
                <GapRow key={e.key} entry={e} busy={busy} onConnect={(id) => void connect([id])} />
              ))}
            </ul>
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <button
                ref={connectAllRef}
                type="button"
                disabled={busy}
                onClick={() => void connect(allIds)}
                style={{ ...buttonStyle, opacity: busy ? 0.6 : 1 }}
              >
                {busy ? (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
                    <Loader2 className="w-3 h-3 animate-spin" /> Waiting for browser…
                  </span>
                ) : entries.length === 1 ? (
                  "Connect my workspace"
                ) : (
                  "Connect all my workspaces"
                )}
              </button>
              <button
                ref={terminalToggleRef}
                type="button"
                aria-expanded={showTerminal}
                onClick={() => setShowTerminal((v) => !v)}
                style={linkButtonStyle}
              >
                Use a terminal instead
              </button>
            </div>
            {busy ? (
              <div style={{ opacity: 0.7 }}>
                Complete the sign-in in your browser. Your home workspace does not change.
              </div>
            ) : null}
            {showTerminal ? (
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <code
                  style={{ fontSize: "0.75rem", wordBreak: "break-all", whiteSpace: "pre-wrap" }}
                >
                  {terminalText}
                </code>
                {caveat ? <div style={{ opacity: 0.7 }}>Note: {caveat}.</div> : null}
                <div>
                  <button
                    type="button"
                    onClick={() =>
                      navigator.clipboard
                        .writeText(terminalText)
                        .then(() => setCopied(true))
                        .catch(() => setCopied(false))
                    }
                    style={buttonStyle}
                  >
                    {copied ? "Copied" : "Copy command"}
                  </button>
                </div>
              </div>
            ) : null}
          </>
        ) : (
          <div style={{ fontWeight: 600 }}>
            {phase === "waiting" ? "Waiting for browser…" : "Workspace connection"}
          </div>
        )}
        <PairAllProgress
          idPrefix="binding-gap"
          phase={phase}
          connectLink={connectLink}
          results={results}
          view={view}
          onCancel={() => void cancel()}
        />
        {phase === "done" && cleared ? <div>All workspaces are connected.</div> : null}
        {error ? (
          <div style={{ color: "var(--error, #f87171)" }}>Could not connect: {error}</div>
        ) : null}
      </div>
      <button
        ref={dismissRef}
        type="button"
        aria-label="Dismiss for this session"
        onClick={() => {
          setDismissed((prev) => {
            const next = new Set(prev);
            for (const e of entries) next.add(e.key);
            return next;
          });
          reset();
        }}
        style={{ background: "none", border: "none", color: "inherit", cursor: "pointer" }}
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}
