import { useCallback, useRef, useState } from "react";

export interface HistoryEntry {
  time: number;
  type: string;
  session: string;
  zone?: number;
  color: string;
}

export interface Metrics {
  totalApprovals: number;
  totalRejections: number;
  totalBroadcasts: number;
  sessionsCreated: number;
}

export function useEventHistory(): {
  eventHistory: HistoryEntry[];
  addHistoryEvent: (type: string, session: string, zone?: number, color?: string) => void;
  metrics: React.MutableRefObject<Metrics>;
  incrementMetric: (key: keyof Metrics, amount?: number) => void;
} {
  const [eventHistory, setEventHistory] = useState<HistoryEntry[]>([]);

  const metricsRef = useRef<Metrics>({
    totalApprovals: 0,
    totalRejections: 0,
    totalBroadcasts: 0,
    sessionsCreated: 0,
  });

  const addHistoryEvent = useCallback(
    (type: string, session: string, zone?: number, color = "#a9b1d6") => {
      setEventHistory((prev) => [
        ...prev.slice(-99),
        { time: Date.now(), type, session, zone, color },
      ]);
    },
    [],
  );

  const incrementMetric = useCallback((key: keyof Metrics, amount = 1) => {
    metricsRef.current[key] += amount;
  }, []);

  return {
    eventHistory,
    addHistoryEvent,
    metrics: metricsRef,
    incrementMetric,
  };
}
