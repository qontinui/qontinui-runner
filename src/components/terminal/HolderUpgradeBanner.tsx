import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, ArrowUpCircle, RefreshCw, X } from "lucide-react";
import { AdvisorySlot } from "./AdvisoryStack";
import {
  attentionLabel,
  bannerHeadline,
  describeOutcome,
  createCensusPoller,
  holderActions,
  invokeErrorText,
  type CensusPoller,
  type HolderAttention,
  type HolderUpgradeOutcome,
} from "./holderUpgrade";

/**
 * PTY holders that need an operator decision (plan
 * `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`, D15 and
 * the Phase 8 runner follow-up): a holder that speaks none of this runner's
 * protocol versions, or one of an older build than this runner ships. Each is
 * listed with honest actions — "Upgrade holder" where an in-place upgrade is
 * supported (Unix), "Leave running" everywhere, and "End and resume" where
 * plan Phase 6's resume path exists (not in this build; the row says so).
 *
 * "Upgrade holder" is the ONLY trigger of `pty_holder_upgrade`: nothing here
 * upgrades on mount, on a timer, or implicitly. The census read
 * (`pty_holder_attention`) is read-only; it is taken on mount, then every
 * `CENSUS_REFRESH_MS` — backed off to `CENSUS_IDLE_REFRESH_MS` while the last
 * one was empty and error-free (`nextCensusDelayMs`) — on "Refresh", and after
 * an action.
 *
 * Lives in the top-right advisory column with the other terminal banners.
 */

const TONE_CLASS = {
  ok: "text-[#9ece6a]",
  warn: "text-[#e0af68]",
  error: "text-[#f7768e]",
} as const;

export function HolderUpgradeBanner() {
  const [attention, setAttention] = useState<HolderAttention | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [outcomes, setOutcomes] = useState<Record<string, HolderUpgradeOutcome | string>>({});
  const [leftRunning, setLeftRunning] = useState<Set<string>>(new Set());

  // One poller per mount: it applies only the newest read's answer and
  // schedules the next read from INSIDE each read, so polling never depends on
  // a state change re-rendering this component (review p8f r2, N1: two equal
  // read errors in a row do not re-render).
  const poller = useRef<CensusPoller | null>(null);
  useEffect(() => {
    const p = createCensusPoller(
      () => invoke<HolderAttention>("pty_holder_attention"),
      (r) => {
        if (r.ok) {
          setAttention(r.attention);
          setReadError(null);
        } else {
          setReadError(r.error);
        }
      },
    );
    poller.current = p;
    void p.refresh();
    return () => {
      p.stop();
      poller.current = null;
    };
  }, []);
  const refresh = useCallback(async () => {
    await poller.current?.refresh();
  }, []);

  const upgrade = useCallback(
    async (paneId: string) => {
      setBusy(paneId);
      try {
        const outcome = await invoke<HolderUpgradeOutcome>("pty_holder_upgrade", { paneId });
        setOutcomes((o) => ({ ...o, [paneId]: outcome }));
      } catch (err) {
        setOutcomes((o) => ({ ...o, [paneId]: invokeErrorText(err) }));
      } finally {
        setBusy(null);
        void refresh();
      }
    },
    [refresh],
  );

  const panes = (attention?.panes ?? []).filter((p) => !leftRunning.has(p.paneId));
  // Outcomes for panes that dropped off the list (an upgrade that took) stay
  // visible until dismissed, so the operator sees what happened.
  const orphanOutcomes = Object.keys(outcomes).filter((id) => !panes.some((p) => p.paneId === id));
  const censusError = attention?.censusError ?? null;
  // A census that could not be read is UNKNOWN, never "nothing needs a
  // decision": it is shown.
  if (panes.length === 0 && orphanOutcomes.length === 0 && !censusError && !readError) return null;

  const actions = holderActions(
    attention ?? {
      upgradeSupported: false,
      upgradeUnsupportedReason: null,
      endAndResumeAvailable: false,
    },
  );

  const renderOutcome = (paneId: string) => {
    const o = outcomes[paneId];
    if (o === undefined) return null;
    const { tone, text } =
      typeof o === "string"
        ? { tone: "error" as const, text: `Request failed: ${o}` }
        : describeOutcome(o);
    return (
      <span
        data-ui-bridge-id={`terminal.holder-upgrade-outcome.${paneId}`}
        data-outcome-kind={typeof o === "string" ? "error" : o.kind}
        className={`text-[10px] break-words ${TONE_CLASS[tone]}`}
      >
        {text}
      </span>
    );
  };

  return (
    <AdvisorySlot>
      <div
        data-ui-bridge-id="terminal.holder-upgrade-banner"
        className="w-[360px] rounded border shadow-lg p-2.5 bg-[#e0af68]/10 border-[#e0af68]/40 text-[11px] text-[#c0caf5]"
      >
        <div className="flex items-center gap-1.5 mb-1">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0 text-[#e0af68]" />
          <span className="font-semibold flex-1">
            {bannerHeadline(panes.length, censusError !== null || readError !== null)}
          </span>
          <button
            type="button"
            data-ui-bridge-id="terminal.holder-upgrade-refresh"
            onClick={() => void refresh()}
            disabled={busy !== null}
            className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d] disabled:opacity-50"
            title="Take the holder census again"
          >
            <RefreshCw className="w-3 h-3" />
          </button>
        </div>
        {censusError && (
          <p className="text-[10px] text-[#f7768e] mb-1 break-words">
            The holder census could not read the pane directory — the list below is UNKNOWN, not
            empty: {censusError}
          </p>
        )}
        {readError && (
          <p className="text-[10px] text-[#f7768e] mb-1 break-words">
            The holder census could not be read: {readError}
          </p>
        )}
        {actions.note && panes.length > 0 && (
          <p
            data-ui-bridge-id="terminal.holder-upgrade-note"
            className="text-[10px] text-[#a9b1d6] mb-1.5 leading-snug"
          >
            {actions.note}
          </p>
        )}
        <ul className="space-y-1.5">
          {panes.map((p) => (
            <li
              key={p.paneId}
              data-ui-bridge-id={`terminal.holder-upgrade.${p.paneId}`}
              data-attention-reason={p.reason}
              className="flex flex-col gap-0.5"
            >
              <div className="flex items-center gap-1.5">
                <span className="truncate flex-1" title={p.detail ?? p.paneId}>
                  <span className="font-medium">{p.paneId}</span>
                  {p.holderPid !== null && (
                    <span className="text-[#565f89]"> · holder {p.holderPid}</span>
                  )}
                </span>
                {actions.upgrade && (
                  <button
                    type="button"
                    data-ui-bridge-id={`terminal.holder-upgrade-upgrade.${p.paneId}`}
                    onClick={() => void upgrade(p.paneId)}
                    disabled={busy !== null}
                    className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-[#7aa2f7]/15 text-[#7aa2f7] hover:bg-[#7aa2f7]/30 disabled:opacity-50 transition-colors text-[10px]"
                    title="Hand this pane to a new holder process in place — same program, same output stream"
                  >
                    <ArrowUpCircle
                      className={`w-2.5 h-2.5 ${busy === p.paneId ? "animate-pulse" : ""}`}
                    />
                    {busy === p.paneId ? "upgrading…" : "Upgrade holder"}
                  </button>
                )}
                {actions.leaveRunning && (
                  <button
                    type="button"
                    data-ui-bridge-id={`terminal.holder-upgrade-leave.${p.paneId}`}
                    onClick={() => setLeftRunning((s) => new Set([...s, p.paneId]))}
                    disabled={busy === p.paneId}
                    className="px-1.5 py-0.5 rounded border border-[#565f89]/50 text-[#a9b1d6] hover:bg-[#2a2d3d] disabled:opacity-50 text-[10px]"
                    title="Keep this holder as it is; its pane's program keeps running"
                  >
                    Leave running
                  </button>
                )}
              </div>
              <span className="text-[10px] text-[#a9b1d6]">
                {attentionLabel(p, attention?.runnerHolderBuild ?? "")}
              </span>
              {renderOutcome(p.paneId)}
            </li>
          ))}
          {orphanOutcomes.map((id) => (
            <li
              key={`done-${id}`}
              data-ui-bridge-id={`terminal.holder-upgrade-done.${id}`}
              className="flex items-start gap-1.5"
            >
              <span className="flex-1 flex flex-col">
                <span className="font-medium">{id}</span>
                {renderOutcome(id)}
              </span>
              <button
                type="button"
                data-ui-bridge-id={`terminal.holder-upgrade-dismiss.${id}`}
                onClick={() =>
                  setOutcomes((o) => {
                    const next = { ...o };
                    delete next[id];
                    return next;
                  })
                }
                className="p-0.5 rounded text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]"
                title="Dismiss"
              >
                <X className="w-3 h-3" />
              </button>
            </li>
          ))}
        </ul>
      </div>
    </AdvisorySlot>
  );
}
