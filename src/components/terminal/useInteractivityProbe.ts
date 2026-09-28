import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Run the runner's remote-interactivity probe sweep for each REMOTE device the
 * Fleet view has loaded, once per device per mount (plan
 * `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * A3 scheduling arm (a)).
 *
 * The sweep itself decides which rows need measuring (it skips rows whose
 * facts are fresh, and any row a live tab is measuring by traffic), so this
 * only has to say WHEN: on load. `trigger: "fleet_view"` also stamps the
 * device, which keeps the runner's own scheduler sweeping it for seven days.
 *
 * Sweeps run one device at a time — each mints grants and attaches, and the
 * target side is one relay — and `onSwept` fires after each so the picker
 * re-reads the facts the sweep just filed. `enabled` is false against a coord
 * that serves no interactivity facts: it has no door to record into.
 *
 * Returns the devices being swept right now and the last error per device, so
 * the picker can say "measuring…" instead of showing stale facts as settled.
 */
export function useInteractivityProbe(
  devices: string[],
  enabled: boolean,
  onSwept: () => void,
): { sweeping: string | null; errors: Record<string, string> } {
  const [sweeping, setSweeping] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  /** Devices already swept (or queued) in this mount. */
  const started = useRef<Set<string>>(new Set());
  /** The pending queue, drained by one worker at a time. */
  const queue = useRef<string[]>([]);
  const running = useRef(false);
  const onSweptRef = useRef(onSwept);
  const mounted = useRef(true);

  useEffect(() => {
    onSweptRef.current = onSwept;
  }, [onSwept]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    if (!enabled) return;
    for (const d of devices) {
      if (!started.current.has(d)) {
        started.current.add(d);
        queue.current.push(d);
      }
    }
    if (running.current || queue.current.length === 0) return;
    running.current = true;
    void (async () => {
      // Yield first: every state update below happens after the effect body
      // has returned, never synchronously inside it.
      await Promise.resolve();
      while (queue.current.length > 0 && mounted.current) {
        const device = queue.current.shift() as string;
        setSweeping(device);
        try {
          await invoke("remote_interactivity_probe", { deviceId: device, trigger: "fleet_view" });
          if (!mounted.current) break;
          setErrors((prev) => {
            if (!(device in prev)) return prev;
            const next = { ...prev };
            delete next[device];
            return next;
          });
          onSweptRef.current();
        } catch (err) {
          if (!mounted.current) break;
          // `throttled` = this device was swept moments ago (a re-mounted
          // view): already measured, not a failure. Re-read what it filed.
          if (String(err).startsWith("remote_interactivity_probe:throttled:")) {
            onSweptRef.current();
            continue;
          }
          setErrors((prev) => ({ ...prev, [device]: String(err) }));
        }
      }
      running.current = false;
      if (mounted.current) setSweeping(null);
    })();
    // `devices` should be memoised by the caller; a new array with the same
    // members is harmless — `started` dedupes.
  }, [devices, enabled]);

  return { sweeping, errors };
}
