/**
 * Non-modal banner that surfaces a `ReconcileReport` after each
 * runner-spawned Claude session reaches session-end.
 *
 * Plan reference:
 * `qontinui-dev-notes/plans/2026-05-22-memories-on-coord-cross-machine.md`
 * Phase 5 — "Surface failures in the runner UI: a single-line banner
 * 'Memory federation: N pushed, M pulled, K failed' with click-through
 * to the failure log."
 *
 * Listens for the runner's `memory-federation:reconcile` Tauri event
 * (emitted from `claude_session::federation::emit_federation_report`).
 * Successes auto-dismiss after 6s so the banner doesn't accumulate
 * across many short sessions. Reports with `report.failed > 0` are
 * persistent and styled with a destructive border so the operator
 * notices.
 */

import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface ReconcileReport {
  pushed: number;
  pulled: number;
  unchanged: number;
  failed: number;
}

interface ReconcileEvent {
  session_id: string;
  account: string;
  report: ReconcileReport;
}

interface ActiveNotice extends ReconcileEvent {
  id: number;
  receivedAt: number;
}

const SUCCESS_TTL_MS = 6000;

export function MemoryFederationBanner() {
  const [notices, setNotices] = useState<ActiveNotice[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let counter = 0;
    (async () => {
      try {
        unlisten = await listen<ReconcileEvent>(
          "memory-federation:reconcile",
          (event) => {
            counter += 1;
            const notice: ActiveNotice = {
              ...event.payload,
              id: counter,
              receivedAt: Date.now(),
            };
            setNotices((prev) => [...prev, notice]);
          },
        );
      } catch (e) {
        console.error("MemoryFederationBanner: listen failed", e);
      }
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    if (notices.length === 0) return;
    const earliestSuccess = notices.find((n) => n.report.failed === 0);
    if (!earliestSuccess) return;
    const elapsed = Date.now() - earliestSuccess.receivedAt;
    const remaining = Math.max(0, SUCCESS_TTL_MS - elapsed);
    const timeout = window.setTimeout(() => {
      setNotices((prev) => prev.filter((n) => n.id !== earliestSuccess.id));
    }, remaining);
    return () => window.clearTimeout(timeout);
  }, [notices]);

  const dismiss = (id: number) => {
    setNotices((prev) => prev.filter((n) => n.id !== id));
  };

  if (notices.length === 0) return null;

  return (
    <div
      role="region"
      aria-label="Memory federation reconcile notifications"
      style={containerStyle}
    >
      {notices.map((n) => {
        const hasFailures = n.report.failed > 0;
        return (
          <div
            key={n.id}
            role="status"
            aria-live="polite"
            style={hasFailures ? bannerStyleFailure : bannerStyleSuccess}
          >
            <div style={{ flex: 1 }}>
              <p
                style={hasFailures ? titleStyleFailure : titleStyleSuccess}
              >
                Memory federation
              </p>
              <p style={messageStyle}>
                <code style={codeStyle}>{n.account}</code>:{" "}
                <strong>{n.report.pushed}</strong> pushed,{" "}
                <strong>{n.report.pulled}</strong> pulled,{" "}
                <strong>{n.report.unchanged}</strong> unchanged
                {hasFailures ? (
                  <>
                    ,{" "}
                    <strong style={failedNumberStyle}>
                      {n.report.failed}
                    </strong>{" "}
                    failed
                  </>
                ) : null}
                {" — session "}
                <code style={codeStyle}>{n.session_id.slice(0, 8)}</code>
              </p>
            </div>
            <div style={actionsColumnStyle}>
              {hasFailures ? (
                <a
                  href="#/admin/memory-federation"
                  style={linkStyle}
                  onClick={(e) => {
                    e.preventDefault();
                    window.location.hash = "#/admin/memory-federation";
                  }}
                >
                  Open log
                </a>
              ) : null}
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
        );
      })}
    </div>
  );
}

const containerStyle: React.CSSProperties = {
  position: "fixed",
  top: 16,
  right: 16,
  zIndex: 9997,
  display: "flex",
  flexDirection: "column",
  gap: 8,
  maxWidth: 480,
};

const bannerBase: React.CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: 12,
  padding: "12px 14px",
  background: "var(--bg-tertiary, #242837)",
  color: "var(--text-primary, #e4e4e7)",
  borderRadius: 8,
  boxShadow: "0 4px 16px rgba(0, 0, 0, 0.35)",
  fontSize: "0.875rem",
};

const bannerStyleSuccess: React.CSSProperties = {
  ...bannerBase,
  border: "1px solid var(--border, #3f3f46)",
};

const bannerStyleFailure: React.CSSProperties = {
  ...bannerBase,
  border: "1px solid var(--destructive, #ef4444)",
};

const titleStyleSuccess: React.CSSProperties = {
  margin: 0,
  marginBottom: 4,
  fontWeight: 600,
  color: "var(--text-primary, #e4e4e7)",
};

const titleStyleFailure: React.CSSProperties = {
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

const failedNumberStyle: React.CSSProperties = {
  color: "var(--destructive, #ef4444)",
};

const actionsColumnStyle: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: 4,
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
