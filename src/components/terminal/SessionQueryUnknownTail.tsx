/**
 * "Unknown for this filter (N)" — the rows the query could not evaluate (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * A predicate over an UNKNOWN fact neither matches nor excludes, so those rows
 * are listed here, each with the reason it could not be decided — a filter
 * never hides a session it could not evaluate (served policy
 * `verification-and-evidence` `silent-empty-is-unknown`). Rendered after the
 * matched rows; collapsed by default so it does not crowd them, but its count
 * is always visible.
 */

import { useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";

import type { SessionFacts } from "./sessionFacts";
import type { UnknownRow } from "./sessionQuery";
import { SESSION_QUERY_CONTROLS, sessionQueryControlAttrs } from "./sessionQueryChips";

export interface SessionQueryUnknownTailProps<F extends SessionFacts> {
  /** `useSessionQuery().result.unknown` (when `result.ok`). */
  unknown: readonly UnknownRow<F>[];
  /** Render one row — the same card the matched list uses. */
  renderRow: (facts: F) => ReactNode;
  defaultOpen?: boolean;
}

export function SessionQueryUnknownTail<F extends SessionFacts>({
  unknown,
  renderRow,
  defaultOpen = false,
}: SessionQueryUnknownTailProps<F>) {
  const [open, setOpen] = useState(defaultOpen);
  const { ref } = useUIElement({
    id: SESSION_QUERY_CONTROLS.unknownTail.id,
    type: "button",
    label: "Sessions the filter could not evaluate",
  });
  if (unknown.length === 0) return null;
  return (
    <div data-testid="session-query-unknown-tail" data-unknown-count={unknown.length}>
      <button
        ref={ref}
        {...sessionQueryControlAttrs("unknownTail")}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1 px-3 py-1 text-[10px] text-[#565f89] hover:text-[#c0caf5] border-t border-[#2a2d3d]"
        title="These sessions are neither shown nor hidden by the filter: a fact it needs is unknown"
      >
        {open ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        Unknown for this filter ({unknown.length})
      </button>
      {open &&
        unknown.map(({ facts, reason }) => (
          <div key={facts.rowId} data-row-id={facts.rowId} data-unknown-reason={reason}>
            <div className="px-3 pt-1 text-[10px] text-[#565f89] truncate" title={reason}>
              Unknown: {reason}
            </div>
            {renderRow(facts)}
          </div>
        ))}
    </div>
  );
}
