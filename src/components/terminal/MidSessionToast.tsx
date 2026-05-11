/**
 * Dismissible mid-session warning toast. Rendered overlay-style over
 * the active terminal when {@link useMidSessionProbe} has a fresh
 * predicted-collisions response for that tab.
 *
 * Visual language mirrors the yellow "Possible conflicts" panel in
 * `LaunchMenu.tsx` so the surfaces feel like one product — same
 * palette (`#e0af68`), same row layout (holder name + basename), but
 * compact since this floats over the terminal rather than inline in
 * the launch sheet.
 */

import type { MidSessionProbeState } from "./useMidSessionProbe";
import type { PredictedCollision } from "./useSessionManager";

// ── Pure helpers (exported for tests) ───────────────────────────────────────

export interface ToastRow {
  /** Full original path — used as a stable React key + tooltip. */
  file_path: string;
  /** Final path component for compact display in the row. */
  basename: string;
  /** First holder's friendly name (used by the "jump to" affordance). */
  holder_name: string;
  /** All holders joined for the secondary text. */
  holders_label: string;
}

export interface ToastSummary {
  header: string;
  rows: ToastRow[];
}

/** Final segment of a /- or \-delimited path. */
function basename(p: string): string {
  const idx = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return idx >= 0 ? p.slice(idx + 1) : p;
}

/**
 * Pure derivation of the toast's displayed data from a probe state.
 * Exported so the file/session pluralization + row aggregation can be
 * unit-tested without rendering JSX.
 */
export function formatToastSummary(state: MidSessionProbeState): ToastSummary {
  const collisions = state.predicted_collisions;
  const n = collisions.length;
  // Pluralize independently — N=1 always means a single holder by
  // construction (one file held by one or more sessions). The
  // "other sessions" half pluralizes on whether ANY collision row has
  // more than one holder OR there's more than one collision row.
  const multipleHolders =
    n > 1 || collisions.some((c) => (c.other_holders?.length ?? 0) > 1);
  const fileWord = n === 1 ? "file" : "files";
  const sessionWord = multipleHolders ? "other sessions" : "another session";
  const header = `${n} ${fileWord} held by ${sessionWord}`;

  const rows: ToastRow[] = collisions.map((c: PredictedCollision) => {
    const holders = c.other_holders ?? [];
    const firstHolder = holders[0]?.holder_name ?? "unknown";
    const label = holders.map((h) => h.holder_name).join(", ") || "unknown";
    return {
      file_path: c.file_path,
      basename: basename(c.file_path),
      holder_name: firstHolder,
      holders_label: label,
    };
  });

  return { header, rows };
}

// ── Component ───────────────────────────────────────────────────────────────

interface MidSessionToastProps {
  state: MidSessionProbeState;
  onDismiss: () => void;
  onJumpToHolder?: (holderName: string) => void;
}

export function MidSessionToast({
  state,
  onDismiss,
  onJumpToHolder,
}: MidSessionToastProps) {
  const { header, rows } = formatToastSummary(state);
  return (
    <div
      data-testid="mid-session-toast"
      className="absolute top-2 right-2 z-30 w-[280px] p-2 rounded border bg-[#e0af68]/15 border-[#e0af68]/40 shadow-lg"
    >
      <div className="flex items-start gap-2">
        <div className="text-[10px] uppercase tracking-wider text-[#e0af68] flex-1">
          {header}
        </div>
        <button
          type="button"
          data-testid="mid-session-toast-dismiss"
          aria-label="Dismiss conflict warning"
          onClick={onDismiss}
          className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
        >
          ×
        </button>
      </div>
      <ul className="mt-1.5 space-y-0.5">
        {rows.map((row) => {
          const clickable = Boolean(onJumpToHolder);
          return (
            <li
              key={row.file_path}
              data-testid="mid-session-toast-row"
              className={`flex items-center gap-2 text-[10px] ${
                clickable ? "cursor-pointer hover:bg-[#e0af68]/10 rounded px-1 -mx-1" : ""
              }`}
              onClick={() => onJumpToHolder?.(row.holder_name)}
              title={row.file_path}
            >
              <span className="font-mono text-[#c0caf5] truncate">{row.basename}</span>
              <span className="text-[#a9b1d6] truncate ml-auto">{row.holders_label}</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
