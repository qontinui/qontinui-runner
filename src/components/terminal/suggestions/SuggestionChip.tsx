/**
 * Per-zone suggestion chip — mounts inside `ZoneCell` and renders the
 * chip (if any) that the engine has assigned to this zone.
 *
 * Single-instance per zone: the engine's reconciler deduplicates by
 * `zoneIdx` so this component never sees more than one candidate at a
 * time. Click the body to apply the chip's registry action; click the
 * `×` to dismiss + suppress the `(zoneIdx, ruleId)` pair for 5min.
 *
 * Renders nothing when no chip is active — keeping zone-cell chrome at
 * zero when the rules are quiet (plan §"Invisible chrome").
 */

import { useCallback, useState } from "react";
import { X } from "lucide-react";

import { callRegistry } from "../commands";
import { useSuggestionForZone } from "./useSuggestions";

interface Props {
  zoneIdx: number;
}

export function SuggestionChip({ zoneIdx }: Props) {
  const { chip, dismiss } = useSuggestionForZone(zoneIdx);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const apply = useCallback(async () => {
    if (!chip) return;
    setApplying(true);
    setError(null);
    try {
      await callRegistry(chip.actionId, chip.args);
      // On success, dismiss locally so the chip clears immediately
      // even if the underlying state hasn't yet flipped.
      dismiss();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      // Auto-clear the error after 3s so the chip returns to its
      // normal copy.
      setTimeout(() => setError(null), 3000);
    } finally {
      setApplying(false);
    }
  }, [chip, dismiss]);

  if (!chip) return null;

  return (
    <div
      className="absolute top-2 right-2 z-30 pointer-events-auto suggestion-chip-fade-in"
      data-page-element="suggestion-chip"
      data-rule-id={chip.ruleId}
    >
      <div
        className={`flex items-center gap-1.5 px-2 py-1 rounded-md text-[10px] border backdrop-blur-sm shadow-md max-w-[280px] ${
          error
            ? "bg-[#f7768e]/10 border-[#f7768e]/40 text-[#f7768e]"
            : "bg-[#1a1b26]/95 border-[#7aa2f7]/30 text-[#c0caf5]"
        }`}
      >
        <button
          type="button"
          onClick={apply}
          disabled={applying}
          className={`flex items-center gap-1.5 text-left ${
            applying ? "opacity-60 cursor-wait" : "hover:text-[#7aa2f7]"
          }`}
          title={`Apply: ${chip.slash}`}
        >
          <span className="truncate">{error ?? chip.headline}</span>
          <span className="font-mono text-[9px] text-[#7aa2f7] opacity-80 shrink-0">
            {chip.slash}
          </span>
        </button>
        <button
          type="button"
          onClick={dismiss}
          className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/60 transition-colors shrink-0"
          title="Dismiss for 5 minutes"
          aria-label="Dismiss suggestion"
        >
          <X className="w-2.5 h-2.5" />
        </button>
      </div>
    </div>
  );
}
