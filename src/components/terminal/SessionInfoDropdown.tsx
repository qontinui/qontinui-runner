import { useEffect, useRef, useState } from "react";
import { ChevronDown, Copy, Check, Info } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useUIElement } from "@qontinui/ui-bridge";
import {
  useSessionInfo,
  deriveTrigger,
  sessionInfoElementId,
  isProvisionalIdentity,
  PROVISIONAL_NOTE,
  sessionInfoRows,
  summarizePrs,
  prPanelRows,
  TONE_COLORS,
  UNKNOWN_TEXT,
  type InfoRowSpec,
  type PrChipTone,
  type SessionInfoState,
} from "./useSessionInfo";

/**
 * Per-session identity + PR dropdown for the zone header (plan
 * `2026-08-16-runner-session-info-dropdown-and-restore-verification`, D4).
 *
 * Replaces (and deletes) the PR-only dropdown that used to live in this
 * header, which **self-hid** whenever the backing read was unavailable —
 * making "we could not read this session" render exactly like "this session
 * opened no PRs" (G5). This trigger renders for EVERY tab
 * carrying a Claude session id; when the read is unavailable it renders a
 * muted `?` with the reason in its tooltip, `aria-label` and
 * `data-session-info-summary`, so absence-of-data and absence-of-PRs are
 * visually and machine-readably distinct.
 *
 * Every row is UI-Bridge addressable under a zone-indexed id (D5) and carries
 * `data-session-info-field` + `data-session-info-value` so a client reads the
 * RAW value rather than parsing truncated display text.
 */

const PANEL_BG = "#1a1b26";
const PANEL_BORDER = "#2a2d3d";
const PANEL_TEXT = "#a9b1d6";
const PANEL_MUTED = "#565f89";

const CHIP_COLORS: Record<PrChipTone, string> = {
  landed: TONE_COLORS.landed,
  unknown: TONE_COLORS.partial,
  "not-landed": TONE_COLORS.pending,
  open: PANEL_TEXT,
};

/** One labelled, individually addressable field row. Its own component so the
 * `useUIElement` registration is a plain unconditional hook call per row. */
function InfoRow({ row, zoneIndex }: { row: InfoRowSpec; zoneIndex: number }) {
  const [copied, setCopied] = useState(false);
  const { ref } = useUIElement({
    id: sessionInfoElementId(row.field, zoneIndex),
    type: "generic",
    label: `${row.label}: ${row.display}`,
  });

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1200);
    return () => clearTimeout(timer);
  }, [copied]);

  const known = row.value !== null;

  return (
    <div
      ref={ref}
      data-session-info-field={row.field}
      data-session-info-value={row.value ?? undefined}
      data-session-info-unknown={known ? undefined : "true"}
      className="flex items-center gap-1.5 px-2 py-0.5"
    >
      <span className="text-[9px] text-[#565f89] w-[68px] shrink-0">{row.label}</span>
      <span
        className="text-[10px] font-mono truncate flex-1"
        style={{ color: known ? PANEL_TEXT : PANEL_MUTED, fontStyle: known ? undefined : "italic" }}
        title={row.display}
      >
        {row.display}
      </span>
      {row.copyable && known && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            navigator.clipboard
              .writeText(row.value ?? "")
              .then(() => setCopied(true))
              .catch((err) => console.error("Failed to copy session-info value:", err));
          }}
          className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors shrink-0"
          title={`Copy ${row.label}`}
          aria-label={`Copy ${row.label}`}
        >
          {copied ? <Check className="w-2.5 h-2.5" /> : <Copy className="w-2.5 h-2.5" />}
        </button>
      )}
    </div>
  );
}

/** The panel body for an available projection. */
function SessionInfoPanelBody({
  state,
  zoneIndex,
  onOpened,
}: {
  state: SessionInfoState;
  zoneIndex: number;
  onOpened: () => void;
}) {
  if (state.status === "loading") {
    return (
      <div className="px-2 py-1.5 text-[10px] text-[#565f89] italic">Loading session info…</div>
    );
  }
  if (state.status === "unavailable" || !state.body) {
    return (
      <div className="px-2 py-1.5">
        <div className="text-[10px] text-[#e0af68]">Session info unavailable</div>
        <div className="text-[9px] text-[#565f89] font-mono mt-0.5 break-all">
          reason: {state.reason ?? UNKNOWN_TEXT}
        </div>
      </div>
    );
  }

  const body = state.body;
  const provisional = isProvisionalIdentity(body.lifecycle);
  const rows = sessionInfoRows(body);
  const prs = summarizePrs(body.prs);
  const prRows = prPanelRows(body.prs);

  return (
    <>
      {/* A provisional identity is qualified ONCE, at the top, rather than only
          per-row: it governs how the whole panel should be read. Amber, not red
          — nothing is broken, the runner simply has not observed a provider
          here yet. */}
      {provisional && (
        <div
          className="px-2 py-1 border-b border-[#2a2d3d] text-[9px] text-[#e0af68] leading-snug"
          data-session-info-provisional="true"
        >
          {PROVISIONAL_NOTE}
        </div>
      )}
      <div className="flex items-center gap-1.5 px-2 py-1 border-b border-[#2a2d3d]">
        <span className="text-[9px] text-[#a9b1d6] truncate">
          {body.name.value ?? UNKNOWN_TEXT}
        </span>
        <span className="text-[8px] text-[#565f89] shrink-0">name: {body.name.source}</span>
      </div>

      <div className="max-h-64 overflow-y-auto scrollbar-dark py-0.5">
        {rows.map((row) => (
          <InfoRow key={row.field} row={row} zoneIndex={zoneIndex} />
        ))}
      </div>

      <div className="border-t border-[#2a2d3d]">
        <div className="px-2 py-0.5 text-[9px] text-[#565f89]">{prs.text}</div>
        {body.prs.status === "ok" && prRows.length === 0 && (
          <div className="px-2 pb-1 text-[9px] text-[#565f89] italic">
            no PRs attributed to this session
          </div>
        )}
        {prRows.length > 0 && (
          <div className="max-h-32 overflow-y-auto scrollbar-dark">
            {prRows.map((pr) => (
              <button
                key={pr.key}
                onClick={(e) => {
                  e.stopPropagation();
                  if (!pr.url) return;
                  void openUrl(pr.url).catch((err) => console.error("Failed to open PR url:", err));
                  onOpened();
                }}
                className={`w-full flex items-center gap-1.5 px-2 py-1 text-left transition-colors text-[#a9b1d6] ${
                  pr.url ? "hover:bg-[#2a2d3d]/50 cursor-pointer" : "cursor-default"
                }`}
                title={pr.url ?? `${pr.key} (no GitHub link — bare repo name)`}
              >
                <div
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ backgroundColor: CHIP_COLORS[pr.chip.tone] }}
                />
                <span className="text-[10px] font-mono truncate flex-1">{pr.label}</span>
                <span
                  className="text-[8px] shrink-0 truncate max-w-[96px]"
                  style={{ color: CHIP_COLORS[pr.chip.tone] }}
                >
                  {pr.chip.text}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>
    </>
  );
}

export function SessionInfoDropdown({
  claudeSessionId,
  zoneIndex,
}: {
  claudeSessionId?: string;
  zoneIndex: number;
}) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const state = useSessionInfo(claudeSessionId);
  const trigger = deriveTrigger(state);

  const { ref: triggerRef } = useUIElement({
    id: sessionInfoElementId("trigger", zoneIndex),
    type: "button",
    label: `Session info (zone ${zoneIndex + 1}): ${trigger.summary}`,
  });
  const { ref: panelRef } = useUIElement({
    id: sessionInfoElementId("panel", zoneIndex),
    type: "generic",
    label: `Session info panel (zone ${zoneIndex + 1})`,
  });

  // Outside-click AND Escape both close. The outside-click half copies the
  // established `ZoneLabel` pattern; Escape-to-close is net-new here (the
  // Escape handler elsewhere in ZoneLabel belongs to the zone-label rename
  // input), registered under the same `!open` guard and torn down in the
  // same cleanup so neither listener can outlive the open panel.
  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      setOpen(false);
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open]);

  // A tab with no Claude session has no session info to be unknown ABOUT —
  // that is a different statement from "its session info is unavailable", so
  // no trigger is rendered. Every tab that HAS a session id gets one.
  if (!claudeSessionId) return null;

  return (
    <div className="relative shrink-0" ref={containerRef}>
      <button
        ref={triggerRef}
        onClick={(e) => {
          e.stopPropagation();
          if (!open) state.refresh();
          setOpen(!open);
        }}
        className="flex items-center gap-0.5 p-0.5 rounded hover:bg-[#2a2d3d] transition-colors"
        style={{ color: trigger.color }}
        title={trigger.summary}
        aria-label={trigger.summary}
        aria-expanded={open}
        data-session-info-summary={trigger.dataSummary}
        data-session-info-tone={trigger.tone}
      >
        <Info className="w-2.5 h-2.5" />
        <span className="text-[9px] font-mono leading-none">{trigger.countText}</span>
        <ChevronDown className="w-2 h-2" />
      </button>

      {open && (
        <div
          ref={panelRef}
          className="absolute left-0 top-full mt-0.5 w-72 rounded-lg shadow-xl z-50 overflow-hidden"
          style={{ backgroundColor: PANEL_BG, border: `1px solid ${PANEL_BORDER}` }}
        >
          <SessionInfoPanelBody
            state={state}
            zoneIndex={zoneIndex}
            onOpened={() => setOpen(false)}
          />
        </div>
      )}
    </div>
  );
}
