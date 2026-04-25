/**
 * LogsTab — wrapper subprocess log viewer.
 *
 * Polls `GET /wrappers/:id/logs` every 3s while mounted, keeps a 200-line
 * ring buffer in state, auto-scrolls to bottom unless the user has scrolled
 * up (sticky-bottom heuristic). If the endpoint isn't wired yet (Phase 1
 * stub: see `api.ts::fetchLogs`) the tab renders a friendly placeholder.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, Copy, Trash2, ScrollText, CheckCircle2 } from "lucide-react";
import { fetchLogs } from "@/lib/wrappers/api";

export interface LogsTabProps {
  wrapperId: string;
}

const MAX_LINES = 200;
const POLL_MS = 3000;

export function LogsTab({ wrapperId }: LogsTabProps) {
  const [available, setAvailable] = useState<boolean | null>(null);
  const [lines, setLines] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [copied, setCopied] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const stickyRef = useRef(true);

  const trimmed = useCallback((next: string[]) => {
    if (next.length <= MAX_LINES) return next;
    return next.slice(next.length - MAX_LINES);
  }, []);

  useEffect(() => {
    let cancelled = false;

    const pull = async () => {
      try {
        const res = await fetchLogs(wrapperId, { limit: MAX_LINES });
        if (cancelled) return;
        setAvailable(res.available);
        if (res.available) {
          setLines(trimmed(res.lines));
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    pull();
    const id = window.setInterval(pull, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [wrapperId, trimmed]);

  // Auto-scroll to bottom unless the user manually scrolled up.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    if (stickyRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [lines]);

  const handleScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickyRef.current = distanceFromBottom < 40;
  };

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard failure non-fatal */
    }
  };

  const handleClear = () => {
    setLines([]);
  };

  if (loading && available === null) {
    return (
      <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
        <Loader2 className="w-5 h-5 animate-spin" />
        <span className="text-sm">Loading logs…</span>
      </div>
    );
  }

  if (available === false) {
    return (
      <div className="rounded-xl border border-dashed border-border bg-card/40 p-10 text-center flex flex-col items-center gap-3">
        <div className="flex h-12 w-12 items-center justify-center rounded-xl border border-cyan-500/30 bg-cyan-500/10">
          <ScrollText className="w-5 h-5 text-cyan-400" />
        </div>
        <div>
          <h3 className="text-sm font-semibold text-foreground">Logs are coming soon</h3>
          <p className="mt-1 text-xs text-muted-foreground max-w-sm">
            Live wrapper subprocess logs will stream here once the runner exposes the
            <code className="mx-1 px-1 py-0.5 rounded bg-muted/40 font-mono">
              /wrappers/:id/logs
            </code>
            endpoint.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-border bg-card overflow-hidden flex flex-col">
      <div className="px-5 py-3 border-b border-border flex items-center justify-between">
        <h3 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground">
          Recent log lines
        </h3>
        <div className="flex items-center gap-2">
          <button
            onClick={handleCopy}
            className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md border border-border bg-card text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
          >
            {copied ? (
              <>
                <CheckCircle2 className="w-3 h-3 text-emerald-400" />
                Copied
              </>
            ) : (
              <>
                <Copy className="w-3 h-3" />
                Copy
              </>
            )}
          </button>
          <button
            onClick={handleClear}
            className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md border border-border bg-card text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
          >
            <Trash2 className="w-3 h-3" />
            Clear view
          </button>
        </div>
      </div>
      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="bg-background/50 px-4 py-3 max-h-[420px] min-h-[200px] overflow-auto font-mono text-[11px] leading-relaxed text-foreground"
      >
        {lines.length === 0 ? (
          <p className="text-muted-foreground italic">No log lines yet.</p>
        ) : (
          lines.map((line, idx) => (
            <div key={idx} className="whitespace-pre-wrap break-words">
              {line}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
