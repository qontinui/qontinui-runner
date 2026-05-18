/**
 * Non-modal banner that surfaces when this runner's claim was stolen
 * by another machine.
 *
 * Plan reference:
 * `D:/qontinui-root/plans/2026-05-18-agent-spawn-coordination.md` Phase 3.
 *
 * Listens for the runner's `agent-claim-stolen` Tauri event (emitted by
 * the `agent_claims` heartbeat task when `HeartbeatResult::Stolen` fires).
 * Displays a dismissible banner with the resource_key + stolen-by holder
 * and a link to the audit-log page (placeholder route; Phase 5 builds
 * the actual dashboard).
 */

import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface StolenEvent {
  kind: string;
  resourceKey: string;
  currentHolder?: string | null;
  agentId: string;
}

interface ActiveStolenNotice extends StolenEvent {
  id: number;
}

export function StolenBanner() {
  const [notices, setNotices] = useState<ActiveStolenNotice[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let counter = 0;
    (async () => {
      try {
        unlisten = await listen<StolenEvent>("agent-claim-stolen", (event) => {
          counter += 1;
          const notice: ActiveStolenNotice = {
            ...event.payload,
            id: counter,
          };
          setNotices((prev) => [...prev, notice]);
        });
      } catch (e) {
        console.error("StolenBanner: listen failed", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const dismiss = (id: number) => {
    setNotices((prev) => prev.filter((n) => n.id !== id));
  };

  if (notices.length === 0) return null;

  return (
    <div
      role="region"
      aria-label="Stolen claim notifications"
      style={containerStyle}
    >
      {notices.map((n) => (
        <div key={n.id} role="status" aria-live="polite" style={bannerStyle}>
          <div style={{ flex: 1 }}>
            <p style={titleStyle}>Claim stolen</p>
            <p style={messageStyle}>
              Your claim{" "}
              <code style={codeStyle}>
                {n.kind}:{n.resourceKey}
              </code>{" "}
              was revoked
              {n.currentHolder ? (
                <>
                  {" "}by machine{" "}
                  <code style={codeStyle}>{n.currentHolder.slice(0, 8)}</code>
                </>
              ) : null}
              . Agent{" "}
              <code style={codeStyle}>{n.agentId.slice(0, 8)}</code>
              {" "}has been notified.
            </p>
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <a
              href="#/admin/agent-claims"
              style={linkStyle}
              onClick={(e) => {
                // Phase 5 wires this to the actual dashboard route. For
                // now: navigate via window.location.hash so the link
                // doesn't 404 in a standard router setup. If the user's
                // router doesn't have the route yet, this is harmless.
                e.preventDefault();
                window.location.hash = "#/admin/agent-claims";
              }}
            >
              Open audit log
            </a>
            <button
              type="button"
              onClick={() => dismiss(n.id)}
              style={dismissBtn}
              aria-label="Dismiss"
            >
              Dismiss
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────────
// Inline styles — same vocabulary as the canary-rollback banner already in
// App.tsx so the visual rhythm stays consistent.
// ─────────────────────────────────────────────────────────────────────────

const containerStyle: React.CSSProperties = {
  position: "fixed",
  top: 16,
  right: 16,
  zIndex: 9998,
  display: "flex",
  flexDirection: "column",
  gap: 8,
  maxWidth: 480,
};

const bannerStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: 12,
  padding: "12px 14px",
  background: "var(--bg-tertiary, #242837)",
  color: "var(--text-primary, #e4e4e7)",
  border: "1px solid var(--destructive, #ef4444)",
  borderRadius: 8,
  boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
  fontSize: "0.875rem",
};

const titleStyle: React.CSSProperties = {
  margin: 0,
  marginBottom: 4,
  fontWeight: 600,
  color: "var(--destructive, #ef4444)",
};

const messageStyle: React.CSSProperties = {
  margin: 0,
  color: "var(--text-secondary, #a1a1aa)",
  fontSize: "0.8125rem",
  lineHeight: 1.4,
};

const codeStyle: React.CSSProperties = {
  background: "rgba(255, 255, 255, 0.08)",
  padding: "1px 4px",
  borderRadius: 3,
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
  fontSize: "0.75rem",
};

const linkStyle: React.CSSProperties = {
  color: "var(--accent, #6366f1)",
  fontSize: "0.75rem",
  textDecoration: "underline",
  cursor: "pointer",
};

const dismissBtn: React.CSSProperties = {
  background: "transparent",
  color: "var(--text-secondary, #a1a1aa)",
  border: "1px solid var(--border, #3f3f46)",
  borderRadius: 4,
  padding: "2px 8px",
  fontSize: "0.75rem",
  cursor: "pointer",
};
