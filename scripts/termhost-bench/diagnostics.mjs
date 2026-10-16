/**
 * termhost-bench runner-only diagnostics that are DERIVED or DECLARED rather than
 * read in one call, and how a report renders any diagnostic. Every diagnostic is
 * `comparable: false` (D1): it describes the runner's internals, has no
 * counterpart in another host, and never enters a comparison or a verdict.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 6. The
 * runner adapter (`adapters/runner.mjs` `diagnostics()`) already reads
 * `/health` (ui thread, thread census, ceilings), UI Bridge reachability, the
 * transport counters (`GET /terminals/transport-stats`, runner#1856's content,
 * on main as e7a57dd3a), the `terminal_spawn.*` span breakdown, backend echo
 * latency, admission and drain. This module adds what those leave out:
 *
 *   - `transport.rates`: perf-harness's two remaining unique measurements
 *     (webview event rate; bytes per second against events), derived from the
 *     transport counters' own window (`since_reset_ms`) instead of a second
 *     harness — the convergence step of Phase 6.
 *   - `renderer.memory`: the WebView2 renderer-memory breakdown. No runner build
 *     serves it over HTTP: runner#1740 (the watchdog) is unlanded and carries the
 *     per-process breakdown on the coord heartbeat only. So it is declared
 *     unknown(not_instrumented) on Windows and not_applicable on Linux
 *     (webkit2gtk has no WebView2 renderer processes), never left out silently.
 */

const diag = (source, fields) => ({ comparable: false, source, ...fields });

const isCount = (x) => typeof x === "number" && Number.isFinite(x) && x >= 0;

/**
 * Rates from one transport-counters snapshot (`TransportSnapshot`,
 * `src-tauri/src/terminal/transport_stats.rs`). The window is `since_reset_ms`:
 * the harness never resets the counters, so it spans the instance's life
 * (spawn and warm-up included), which the diagnostic says.
 *
 * @returns {{since_reset_ms: number, reader_bytes_per_sec: number,
 *   webview_events_per_sec: number, bytes_per_webview_event: number | null} | null}
 *   null when the snapshot lacks a field the rates need
 */
export function transportRates(snapshot) {
  const ms = snapshot?.since_reset_ms;
  const bytes = snapshot?.reader?.bytes;
  const emits = snapshot?.legs?.webview?.emits;
  if (!isCount(ms) || ms === 0 || !isCount(bytes) || !isCount(emits)) return null;
  const sec = ms / 1000;
  return {
    since_reset_ms: ms,
    reader_bytes_per_sec: bytes / sec,
    webview_events_per_sec: emits / sec,
    bytes_per_webview_event: emits > 0 ? bytes / emits : null,
  };
}

/**
 * The derived and declared runner diagnostics for one row, given the ones the
 * adapter already read. Pure: no I/O.
 *
 * @param {Record<string, object>} d   the adapter's diagnostics so far
 * @param {{platform?: string}} [o]
 * @returns {Record<string, object>}   entries to merge into `d`
 */
export function derivedRunnerDiagnostics(d, { platform = process.platform } = {}) {
  const out = {};
  const source = "derived from GET /terminals/transport-stats (reader.bytes, legs.webview.emits over since_reset_ms)";
  const counters = d?.["transport.counters"];
  if (counters?.state === "measured") {
    const rates = transportRates(counters.data);
    out["transport.rates"] = rates
      ? diag(source, {
          state: "measured",
          data: rates,
          notes: ["window = since_reset_ms: the counters are not reset at soak start, so spawn and warm-up are inside it"],
        })
      : diag(source, { state: "unknown", reason: "harness_error", detail: "the transport snapshot lacks since_reset_ms, reader.bytes or legs.webview.emits" });
  } else {
    out["transport.rates"] = diag(source, {
      state: counters?.state === "not_applicable" ? "not_applicable" : "unknown",
      reason: counters?.reason ?? "not_instrumented",
      detail: "no measured transport counters to derive from",
    });
  }
  out["renderer.memory"] =
    platform === "win32"
      ? diag("WebView2 renderer-memory watchdog (runner#1740)", {
          state: "unknown",
          reason: "not_instrumented",
          detail: "no runner build serves the renderer-memory breakdown over HTTP: runner#1740 is unlanded and sends it on the coord heartbeat only",
        })
      : diag("WebView2 renderer-memory watchdog (runner#1740)", {
          state: "not_applicable",
          reason: "not_applicable_to_environment",
          detail: "WebView2 renderer processes exist only on Windows; this runner renders through webkit2gtk",
        });
  return out;
}

const num = (x) => (Math.abs(x) >= 100 ? x.toFixed(0) : Math.abs(x) >= 10 ? x.toFixed(1) : x.toFixed(2));

/**
 * One diagnostic as a short report fragment: its state, and a measured value,
 * statistics or the top-level keys of its data (never the data itself — it can
 * be large, and the report is a table, not a dump).
 */
export function summarizeDiagnostic(v) {
  if (!v || typeof v !== "object") return "missing";
  if (v.state !== "measured") return `${v.state}(${v.reason ?? "?"})${v.detail ? ` — ${v.detail}` : ""}`;
  if (typeof v.value === "number") return `${num(v.value)}${v.unit ? ` ${v.unit}` : ""}`;
  if (v.stats && typeof v.stats === "object") {
    return Object.entries(v.stats)
      .map(([k, x]) => `${k} ${typeof x === "number" ? num(x) : x}`)
      .join(", ") + (v.unit ? ` ${v.unit}` : "");
  }
  if (v.data && typeof v.data === "object" && !Array.isArray(v.data)) {
    const flat = Object.entries(v.data)
      .filter(([, x]) => typeof x === "number" || typeof x === "boolean")
      .map(([k, x]) => `${k} ${typeof x === "number" ? num(x) : x}`);
    const nested = Object.keys(v.data).filter((k) => v.data[k] && typeof v.data[k] === "object");
    return [...flat, ...(nested.length > 0 ? [`{${nested.join(", ")}}`] : [])].join(", ") || "measured";
  }
  if (Array.isArray(v.data)) return `${v.data.length} item(s)`;
  return "measured";
}
