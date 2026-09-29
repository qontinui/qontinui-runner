/**
 * BindingGapAskBanner.tsx
 *
 * Shows the device-JWT refresher's once-per-lapse ask: this device is bound to
 * a tenant (per coord) but holds no credential for it, and no headless path
 * can seed one safely, so a human has to pair it once.
 *
 * The ask is READ, not received: {@link GET_BINDING_GAP_ASKS_CMD} on mount and
 * again on every {@link BINDING_GAP_NUDGE_EVENT}. The runner records the ask
 * whether or not this component is listening, which is the point — an ask
 * delivered only as an event is lost whenever the UI was not mounted yet.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { KeyRound, X } from "lucide-react";

import { useRunnerTier } from "@/hooks/useRunnerTier";

import {
  BINDING_GAP_NUDGE_EVENT,
  dismissKey,
  GET_BINDING_GAP_ASKS_CMD,
  normalizeBindingGapAsks,
  visibleBindingGapAsks,
  type BindingGapAsk,
} from "./binding-gap-ask-logic";

export function BindingGapAskBanner() {
  const { tier } = useRunnerTier();
  const [asks, setAsks] = useState<BindingGapAsk[] | null>(null);
  const [dismissed, setDismissed] = useState<Set<string>>(() => new Set());
  const [copied, setCopied] = useState<string | null>(null);

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

  const copy = useCallback((ask: BindingGapAsk) => {
    navigator.clipboard
      .writeText(ask.command)
      .then(() => setCopied(dismissKey(ask)))
      .catch(() => setCopied(null));
  }, []);

  if (tier !== "qontinui_account") return null;
  const visible = visibleBindingGapAsks(asks, dismissed);
  if (visible.length === 0) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      data-ui-id="binding-gap-ask-banner"
      style={{
        position: "fixed",
        bottom: 16,
        right: 16,
        zIndex: 9998,
        display: "flex",
        flexDirection: "column",
        gap: 8,
        maxWidth: "min(520px, calc(100vw - 32px))",
      }}
    >
      {visible.map((ask) => (
        <div
          key={dismissKey(ask)}
          style={{
            display: "flex",
            gap: 10,
            padding: "10px 12px",
            background: "var(--bg-tertiary, #242837)",
            color: "var(--text-primary, #e4e4e7)",
            border: "1px solid var(--accent, #6366f1)",
            borderRadius: 8,
            boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
            fontSize: "0.8125rem",
          }}
        >
          <KeyRound className="w-4 h-4 shrink-0" aria-hidden="true" />
          <div style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}>
            <div style={{ fontWeight: 600 }}>{ask.message}</div>
            {ask.reason ? <div style={{ opacity: 0.8 }}>Why: {ask.reason}.</div> : null}
            <code style={{ fontSize: "0.75rem", wordBreak: "break-all" }}>{ask.command}</code>
            {ask.caveat ? <div style={{ opacity: 0.7 }}>Note: {ask.caveat}.</div> : null}
            <div>
              <button
                type="button"
                onClick={() => copy(ask)}
                style={{
                  background: "var(--accent, #6366f1)",
                  color: "#fff",
                  border: "none",
                  borderRadius: 4,
                  padding: "2px 8px",
                  fontSize: "0.75rem",
                  cursor: "pointer",
                }}
              >
                {copied === dismissKey(ask) ? "Copied" : "Copy command"}
              </button>
            </div>
          </div>
          <button
            type="button"
            aria-label="Dismiss for this session"
            onClick={() => setDismissed((prev) => new Set(prev).add(dismissKey(ask)))}
            style={{ background: "none", border: "none", color: "inherit", cursor: "pointer" }}
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      ))}
    </div>
  );
}
