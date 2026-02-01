/**
 * TimeRangeSelector.tsx
 *
 * Dropdown for selecting time range filter.
 */

import { Clock } from "lucide-react";
import type { TimeRange } from "../../types/performance-metrics";
import { TIME_RANGE_LABELS } from "../../types/performance-metrics";

interface TimeRangeSelectorProps {
  /** Current selected time range */
  value: TimeRange;
  /** Callback when time range changes */
  onChange: (value: TimeRange) => void;
  /** Optional CSS classes */
  className?: string;
}

const TIME_RANGE_OPTIONS: TimeRange[] = ["1h", "24h", "7d", "30d", "all"];

export function TimeRangeSelector({ value, onChange, className = "" }: TimeRangeSelectorProps) {
  return (
    <div className={`flex items-center gap-2 ${className}`}>
      <Clock className="w-4 h-4 text-muted-foreground" />
      <select
        value={value}
        onChange={(e) => onChange(e.target.value as TimeRange)}
        className="bg-muted border border-border rounded-md px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
      >
        {TIME_RANGE_OPTIONS.map((option) => (
          <option key={option} value={option}>
            {TIME_RANGE_LABELS[option]}
          </option>
        ))}
      </select>
    </div>
  );
}
