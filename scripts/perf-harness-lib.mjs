/**
 * Pure helpers for the many-sessions performance harness (`perf-harness.mjs`).
 *
 * Everything in this file is I/O-free and deterministic so it can be unit
 * tested without a live runner — see `scripts/__tests__/perf-harness-lib.test.mjs`.
 * The harness's live half (HTTP to the supervisor / temp runner) lives in
 * `perf-harness.mjs` and does nothing but feed these functions.
 *
 * Plan: `qontinui-dev-notes/plans/2026-07-28-runner-many-sessions-performance.md`
 * Phase 0.
 */

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/**
 * Nearest-rank percentile over an unsorted numeric array.
 *
 * Nearest-rank (rather than linear interpolation) is deliberate: with the
 * small samples this harness takes (10-40 spawns) an interpolated p95 invents
 * a value that no spawn actually exhibited, which is exactly the kind of
 * number a before/after table must not carry.
 *
 * @param {number[]} values
 * @param {number} p percentile in [0, 100]
 * @returns {number|null} null for an empty input
 */
export function percentile(values, p) {
  if (!Array.isArray(values) || values.length === 0) return null;
  if (!(p >= 0 && p <= 100)) throw new RangeError(`percentile out of range: ${p}`);
  const sorted = values.filter((v) => Number.isFinite(v)).sort((a, b) => a - b);
  if (sorted.length === 0) return null;
  if (p <= 0) return sorted[0];
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[Math.min(rank, sorted.length) - 1];
}

/**
 * Summary statistics used everywhere a distribution is reported.
 *
 * @param {number[]} values
 * @returns {{count:number,min:number|null,p50:number|null,p90:number|null,p95:number|null,p99:number|null,max:number|null,mean:number|null,total:number}}
 */
export function stats(values) {
  const finite = (Array.isArray(values) ? values : []).filter((v) => Number.isFinite(v));
  const total = finite.reduce((a, b) => a + b, 0);
  return {
    count: finite.length,
    min: finite.length ? Math.min(...finite) : null,
    p50: percentile(finite, 50),
    p90: percentile(finite, 90),
    p95: percentile(finite, 95),
    p99: percentile(finite, 99),
    max: finite.length ? Math.max(...finite) : null,
    mean: finite.length ? total / finite.length : null,
    total,
  };
}

// ---------------------------------------------------------------------------
// tracing span parsing
// ---------------------------------------------------------------------------

/** Multipliers converting a `tracing` duration suffix to milliseconds. */
const DURATION_UNITS_MS = {
  ns: 1e-6,
  // `std::time::Duration`'s Debug impl emits the micro SIGN (U+00B5). Some
  // terminals/log shippers normalise it to the GREEK SMALL LETTER MU (U+03BC),
  // and ASCII-only tooling writes `us` — accept all three rather than
  // silently dropping a whole segment's samples.
  "µs": 1e-3,
  "μs": 1e-3,
  us: 1e-3,
  ms: 1,
  s: 1000,
};

/**
 * Parse a `tracing` duration literal (`time.busy=12.3ms`) into milliseconds.
 *
 * @param {string} text e.g. `"912µs"`, `"1.23ms"`, `"1.5s"`, `"84ns"`
 * @returns {number|null} null when the text is not a duration
 */
export function parseTracingDuration(text) {
  if (typeof text !== "string") return null;
  const m = /^([0-9]*\.?[0-9]+)(ns|µs|μs|us|ms|s)$/.exec(text.trim());
  if (!m) return null;
  const mult = DURATION_UNITS_MS[m[2]];
  if (mult === undefined) return null;
  return Number.parseFloat(m[1]) * mult;
}

/**
 * Split on `sep` at brace/quote depth 0, treating a *doubled* separator as a
 * literal.
 *
 * Span-chain prefixes look like `a{k=v}:b{k="x:y"}: qontinui_runner::terminal::session: `.
 * A naive `split(":")` shreds three things at once: the field values, the
 * span/target boundary, and — worst — the Rust module path, which would leave
 * every span attributed to the last path segment (`session`) instead of its
 * own name. Doubling therefore escapes: `::` is content, `:` is a boundary.
 *
 * @param {string} text
 * @param {string} sep single character
 * @returns {string[]}
 */
export function splitTopLevel(text, sep) {
  const out = [];
  let depth = 0;
  let inQuote = false;
  let cur = "";
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (inQuote) {
      cur += ch;
      if (ch === "\\" && i + 1 < text.length) {
        cur += text[i + 1];
        i += 1;
      } else if (ch === '"') {
        inQuote = false;
      }
      continue;
    }
    if (ch === '"') {
      inQuote = true;
      cur += ch;
      continue;
    }
    if (ch === "{" || ch === "[") depth += 1;
    else if (ch === "}" || ch === "]") depth = Math.max(0, depth - 1);
    if (ch === sep && depth === 0) {
      if (text[i + 1] === sep) {
        // A doubled separator is content (`::` in a Rust path), not a boundary.
        cur += sep + sep;
        i += 1;
        continue;
      }
      out.push(cur);
      cur = "";
      continue;
    }
    cur += ch;
  }
  out.push(cur);
  return out;
}

/**
 * Parse the `k=v k2="v 2"` field list inside a span's braces.
 *
 * @param {string} text contents between `{` and `}`
 * @returns {Record<string,string>}
 */
export function parseSpanFields(text) {
  const fields = {};
  if (!text) return fields;
  for (const part of splitTopLevel(text, " ")) {
    const chunk = part.trim();
    if (!chunk) continue;
    const eq = chunk.indexOf("=");
    if (eq <= 0) continue;
    const key = chunk.slice(0, eq);
    let value = chunk.slice(eq + 1);
    if (value.startsWith('"') && value.endsWith('"') && value.length >= 2) {
      value = value.slice(1, -1).replace(/\\"/g, '"').replace(/\\\\/g, "\\");
    }
    fields[key] = value;
  }
  return fields;
}

/**
 * Parse one `FmtSpan::CLOSE` line from `qontinui-runner.log.<date>`.
 *
 * The runner's file layer is configured at `src-tauri/src/logging.rs` with
 * `.with_ansi(false)`, `.with_span_events(FmtSpan::CLOSE)` and a
 * `%Y-%m-%d %H:%M:%S%.3f` timer, so a closed span reads:
 *
 * ```text
 * 2026-08-05 00:31:53.554 DEBUG parent{a=1}:child{terminal_id=x}: some::target: close time.busy=12.3ms time.idle=1.20ms
 * ```
 *
 * The *closed* span is the innermost (last) element of the chain.
 *
 * @param {string} line
 * @returns {{timestamp:string,level:string,span:string,fields:Record<string,string>,parents:string[],target:string,busyMs:number,idleMs:number,totalMs:number}|null}
 */
export function parseSpanCloseLine(line) {
  if (typeof line !== "string" || !line.includes("close time.busy=")) return null;
  const head =
    /^(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}\.\d{3})\s+([A-Z]+)\s+(.*)$/.exec(line.trimEnd());
  if (!head) return null;
  const [, timestamp, level, rest] = head;

  const closeAt = rest.indexOf("close time.busy=");
  if (closeAt < 0) return null;
  const prefix = rest.slice(0, closeAt);
  const tail = rest.slice(closeAt);

  const durations = /^close time\.busy=(\S+)\s+time\.idle=(\S+)\s*$/.exec(tail.trim());
  if (!durations) return null;
  const busyMs = parseTracingDuration(durations[1]);
  const idleMs = parseTracingDuration(durations[2]);
  if (busyMs === null || idleMs === null) return null;

  // `prefix` is `span{..}:span{..}: target: ` — split at depth 0 and drop the
  // trailing empty segment produced by the final `: `.
  const parts = splitTopLevel(prefix, ":")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  if (parts.length < 2) return null;
  const target = parts[parts.length - 1];
  const chain = parts.slice(0, -1);
  const closed = chain[chain.length - 1];

  const brace = closed.indexOf("{");
  const span = brace >= 0 ? closed.slice(0, brace) : closed;
  const fields =
    brace >= 0 && closed.endsWith("}")
      ? parseSpanFields(closed.slice(brace + 1, -1))
      : {};

  return {
    timestamp,
    level,
    span,
    fields,
    parents: chain.slice(0, -1).map((c) => {
      const b = c.indexOf("{");
      return b >= 0 ? c.slice(0, b) : c;
    }),
    target,
    busyMs,
    idleMs,
    totalMs: busyMs + idleMs,
  };
}

/**
 * Extract every `terminal_spawn.*` span close from a slab of log text.
 *
 * @param {string} text
 * @param {{prefix?:string}} [options] span-name prefix to keep (default `terminal_spawn.`)
 * @returns {ReturnType<typeof parseSpanCloseLine>[]}
 */
export function parseSpawnSpans(text, options = {}) {
  const prefix = options.prefix ?? "terminal_spawn.";
  if (typeof text !== "string" || text.length === 0) return [];
  const out = [];
  for (const line of text.split(/\r?\n/)) {
    const parsed = parseSpanCloseLine(line);
    if (parsed && parsed.span.startsWith(prefix)) out.push(parsed);
  }
  return out;
}

/**
 * Aggregate parsed spans into per-segment statistics, keyed by span name.
 *
 * @param {ReturnType<typeof parseSpanCloseLine>[]} records
 * @returns {Record<string, ReturnType<typeof stats>>}
 */
export function aggregateSpans(records) {
  const bySpan = new Map();
  for (const rec of records ?? []) {
    if (!rec) continue;
    if (!bySpan.has(rec.span)) bySpan.set(rec.span, []);
    bySpan.get(rec.span).push(rec.totalMs);
  }
  const out = {};
  for (const name of [...bySpan.keys()].sort()) {
    out[name] = stats(bySpan.get(name));
  }
  return out;
}

// ---------------------------------------------------------------------------
// Run flattening + before/after diffing
// ---------------------------------------------------------------------------

/**
 * Metric descriptors. `lowerIsBetter: false` flips the improvement arrow so a
 * table never claims "more frames dropped" is a win.
 */
const METRIC_UNITS = {
  "terminal_create_ms.p50": { unit: "ms", lowerIsBetter: true },
  "terminal_create_ms.p95": { unit: "ms", lowerIsBetter: true },
  "terminal_create_ms.max": { unit: "ms", lowerIsBetter: true },
  "frame_time_ms.p50": { unit: "ms", lowerIsBetter: true },
  "frame_time_ms.p95": { unit: "ms", lowerIsBetter: true },
  "long_tasks.count": { unit: "", lowerIsBetter: true },
  "long_tasks.per_sec": { unit: "/s", lowerIsBetter: true },
  "long_tasks.total_ms": { unit: "ms", lowerIsBetter: true },
  "long_tasks.blocking_pct": { unit: "%", lowerIsBetter: true },
  "fps.mean": { unit: "fps", lowerIsBetter: false },
  "chunks.bytes_per_sec": { unit: "B/s", lowerIsBetter: false },
  "chunks.webview_events_per_sec": { unit: "/s", lowerIsBetter: true },
  "chunks.paints_per_webview_event": { unit: "", lowerIsBetter: false },
};

/**
 * Look up the unit / direction for a flattened metric key.
 *
 * Keys are `S<n>/<group>.<stat>` (per-level) or `spawn_spans/<span>.<stat>`.
 *
 * @param {string} key
 * @returns {{unit:string,lowerIsBetter:boolean}}
 */
export function metricMeta(key) {
  const tail = key.includes("/") ? key.slice(key.indexOf("/") + 1) : key;
  // Span keys are `S<n>/spawn_spans/<span>.<stat>` — every segment is a duration.
  if (key.startsWith("spawn_spans/") || key.includes("/spawn_spans/")) {
    return { unit: "ms", lowerIsBetter: true };
  }
  return METRIC_UNITS[tail] ?? { unit: "", lowerIsBetter: true };
}

/**
 * Flatten a run document into `metricKey -> number` so two runs taken by
 * different harness versions still diff on whatever keys they share.
 *
 * @param {object} run a document produced by `perf-harness.mjs`
 * @returns {Record<string, number>}
 */
export function flattenRun(run) {
  const flat = {};
  if (!run || typeof run !== "object") return flat;

  for (const level of run.levels ?? []) {
    const s = `S${level.sessions}`;
    const put = (key, value) => {
      if (Number.isFinite(value)) flat[`${s}/${key}`] = value;
    };
    const create = level.terminalCreateMs ?? {};
    put("terminal_create_ms.p50", create.p50);
    put("terminal_create_ms.p95", create.p95);
    put("terminal_create_ms.max", create.max);

    const fe = level.frontend ?? {};
    put("frame_time_ms.p50", fe.frameTimeMs?.p50);
    put("frame_time_ms.p95", fe.frameTimeMs?.p95);
    put("long_tasks.count", fe.longTasks?.count);
    put("long_tasks.per_sec", fe.longTasks?.perSec);
    put("long_tasks.total_ms", fe.longTasks?.totalMs);
    put("long_tasks.blocking_pct", fe.longTasks?.blockingPct);
    put("fps.mean", fe.fps);

    const chunks = level.chunks ?? {};
    put("chunks.bytes_per_sec", chunks.bytesPerSec);
    put("chunks.webview_events_per_sec", chunks.webviewEventsPerSec);
    put("chunks.paints_per_webview_event", chunks.paintsPerWebviewEvent);

    for (const [span, st] of Object.entries(level.spawnSpans ?? {})) {
      if (Number.isFinite(st?.p50)) flat[`${s}/spawn_spans/${span}.p50`] = st.p50;
      if (Number.isFinite(st?.p95)) flat[`${s}/spawn_spans/${span}.p95`] = st.p95;
    }
  }
  return flat;
}

/**
 * Diff two runs into table rows.
 *
 * Metrics present in only one run are still emitted (with a null on the
 * missing side) — silently dropping them is how a regression hides.
 *
 * @param {object} before
 * @param {object} after
 * @param {{minPctChange?:number}} [options]
 * @returns {{key:string,unit:string,before:number|null,after:number|null,delta:number|null,pct:number|null,verdict:string}[]}
 */
export function compareRuns(before, after, options = {}) {
  const minPct = options.minPctChange ?? 5;
  const a = flattenRun(before);
  const b = flattenRun(after);
  const keys = [...new Set([...Object.keys(a), ...Object.keys(b)])].sort(compareMetricKeys);

  return keys.map((key) => {
    const meta = metricMeta(key);
    const beforeValue = Number.isFinite(a[key]) ? a[key] : null;
    const afterValue = Number.isFinite(b[key]) ? b[key] : null;
    let delta = null;
    let pct = null;
    let verdict = "n/a";
    if (beforeValue !== null && afterValue !== null) {
      delta = afterValue - beforeValue;
      pct = beforeValue === 0 ? (delta === 0 ? 0 : null) : (delta / beforeValue) * 100;
      if (pct === null) verdict = "changed";
      else if (Math.abs(pct) < minPct) verdict = "same";
      else {
        const improved = meta.lowerIsBetter ? delta < 0 : delta > 0;
        verdict = improved ? "better" : "WORSE";
      }
    } else if (afterValue !== null) {
      verdict = "new";
    } else {
      verdict = "dropped";
    }
    return { key, unit: meta.unit, before: beforeValue, after: afterValue, delta, pct, verdict };
  });
}

/**
 * Sort metric keys by session level numerically (S10 before S40 before S100),
 * then lexically — so the table reads in load order rather than string order.
 *
 * @param {string} x
 * @param {string} y
 * @returns {number}
 */
export function compareMetricKeys(x, y) {
  const lx = /^S(\d+)\//.exec(x);
  const ly = /^S(\d+)\//.exec(y);
  if (lx && ly && lx[1] !== ly[1]) return Number(lx[1]) - Number(ly[1]);
  if (lx && !ly) return -1;
  if (!lx && ly) return 1;
  return x < y ? -1 : x > y ? 1 : 0;
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/**
 * Format a number for a table cell with a stable number of significant digits.
 *
 * @param {number|null|undefined} value
 * @param {number} [digits]
 * @returns {string}
 */
export function fmtNumber(value, digits = 1) {
  if (value === null || value === undefined || !Number.isFinite(value)) return "—";
  const abs = Math.abs(value);
  if (abs >= 100000) return value.toExponential(2);
  if (abs >= 100) return value.toFixed(0);
  if (abs >= 10) return value.toFixed(Math.min(1, digits));
  return value.toFixed(digits);
}

/**
 * Render a markdown table from a header list and pre-stringified rows.
 *
 * @param {string[]} headers
 * @param {string[][]} rows
 * @returns {string}
 */
export function renderMarkdownTable(headers, rows) {
  const widths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => (r[i] ?? "").length), 3),
  );
  const line = (cells) => `| ${cells.map((c, i) => (c ?? "").padEnd(widths[i])).join(" | ")} |`;
  const sep = `|${widths.map((w) => "-".repeat(w + 2)).join("|")}|`;
  return [line(headers), sep, ...rows.map((r) => line(r))].join("\n");
}

/**
 * Render a single run as markdown (the "before" half of the table, and what
 * you get on a first ever run when no baseline exists yet).
 *
 * @param {object} run
 * @returns {string}
 */
export function renderSingleRun(run) {
  const out = [];
  out.push(`### Run \`${run.label ?? "unlabeled"}\` — ${run.startedAt ?? "?"}`);
  out.push("");
  const meta = run.runner ?? {};
  out.push(
    `- runner: \`${meta.id ?? "?"}\` on ${meta.apiUrl ?? "?"} (buildId \`${meta.buildId ?? "?"}\`, gitSha \`${meta.gitSha ?? "?"}\`)`,
  );
  out.push(`- generator: \`${run.generator ?? "?"}\`  soak: ${run.soakMs ?? "?"} ms`);
  if (run.notes?.length) for (const n of run.notes) out.push(`- NOTE: ${n}`);
  out.push("");

  const rows = [];
  for (const level of run.levels ?? []) {
    const fe = level.frontend ?? {};
    const ch = level.chunks ?? {};
    rows.push([
      String(level.sessions),
      String(level.created ?? "—"),
      fmtNumber(level.terminalCreateMs?.p50),
      fmtNumber(level.terminalCreateMs?.p95),
      fmtNumber(level.terminalCreateMs?.max),
      fmtNumber(fe.frameTimeMs?.p50),
      fmtNumber(fe.frameTimeMs?.p95),
      fmtNumber(fe.longTasks?.count, 0),
      fmtNumber(fe.longTasks?.perSec, 2),
      fmtNumber(ch.bytesPerSec, 0),
      fmtNumber(ch.webviewEventsPerSec, 2),
    ]);
  }
  out.push(
    renderMarkdownTable(
      [
        "S",
        "new",
        "create p50 ms",
        "create p95 ms",
        "create max ms",
        "frame p50 ms",
        "frame p95 ms",
        "longtasks",
        "longtask/s",
        "chunk B/s",
        "webview ev/s",
      ],
      rows,
    ),
  );

  const spanRows = [];
  for (const level of run.levels ?? []) {
    for (const [span, st] of Object.entries(level.spawnSpans ?? {})) {
      spanRows.push([
        `S${level.sessions}`,
        span,
        String(st.count ?? 0),
        fmtNumber(st.p50),
        fmtNumber(st.p95),
        fmtNumber(st.max),
      ]);
    }
  }
  out.push("");
  if (spanRows.length) {
    out.push("#### Spawn-segment breakdown (`terminal_spawn.*` spans)");
    out.push("");
    out.push(renderMarkdownTable(["S", "segment", "n", "p50 ms", "p95 ms", "max ms"], spanRows));
  } else {
    out.push(
      "#### Spawn-segment breakdown: **no `terminal_spawn.*` spans found in the runner log**",
    );
    out.push("");
    out.push(
      "The spans are emitted at DEBUG. Confirm the runner binary contains them and that it " +
        "was launched with `RUST_LOG` enabling debug for `qontinui_runner::terminal::session` " +
        "and `qontinui_runner::commands::terminal` (the harness passes this via `extra_env` " +
        "when it spawns the runner itself).",
    );
  }
  return out.join("\n");
}

/**
 * Render the before/after comparison table — the Phase 0 deliverable.
 *
 * @param {object} before
 * @param {object} after
 * @param {{minPctChange?:number}} [options]
 * @returns {string}
 */
export function renderComparison(before, after, options = {}) {
  const rows = compareRuns(before, after, options);
  const out = [];
  out.push(`## Before / after — \`${before?.label ?? "before"}\` → \`${after?.label ?? "after"}\``);
  out.push("");
  out.push(`- before: ${before?.startedAt ?? "?"} (buildId \`${before?.runner?.buildId ?? "?"}\`)`);
  out.push(`- after:  ${after?.startedAt ?? "?"} (buildId \`${after?.runner?.buildId ?? "?"}\`)`);
  out.push("");
  out.push(
    renderMarkdownTable(
      ["metric", "unit", "before", "after", "delta", "%", "verdict"],
      rows.map((r) => [
        r.key,
        r.unit,
        fmtNumber(r.before),
        fmtNumber(r.after),
        r.delta === null ? "—" : (r.delta > 0 ? "+" : "") + fmtNumber(r.delta),
        r.pct === null ? "—" : (r.pct > 0 ? "+" : "") + fmtNumber(r.pct),
        r.verdict,
      ]),
    ),
  );
  const worse = rows.filter((r) => r.verdict === "WORSE");
  out.push("");
  out.push(
    worse.length
      ? `**${worse.length} metric(s) regressed:** ${worse.map((r) => r.key).join(", ")}`
      : "**No regressions** past the reporting threshold.",
  );
  return out.join("\n");
}

// ---------------------------------------------------------------------------
// Frontend probe payload folding
// ---------------------------------------------------------------------------

/**
 * Fold the raw arrays the in-page probe returns into the harness's frontend
 * metric block. Kept here (rather than in the injected JS) so the percentile
 * maths is the same tested code everywhere.
 *
 * @param {{elapsedMs:number,frameGapsMs:number[],longTasks:{duration:number}[],webviewOutputEvents:number|null,webviewHooked:boolean,errors?:string[]}|null} sample
 * @returns {object|null}
 */
export function foldFrontendSample(sample) {
  if (!sample || typeof sample !== "object") return null;
  const elapsedMs = Number.isFinite(sample.elapsedMs) ? sample.elapsedMs : null;
  const gaps = Array.isArray(sample.frameGapsMs) ? sample.frameGapsMs : [];
  const longTaskDurations = (Array.isArray(sample.longTasks) ? sample.longTasks : [])
    .map((t) => t?.duration)
    .filter((d) => Number.isFinite(d));
  const longTotal = longTaskDurations.reduce((a, b) => a + b, 0);
  const elapsedSec = elapsedMs && elapsedMs > 0 ? elapsedMs / 1000 : null;

  return {
    elapsedMs,
    frameCount: gaps.length,
    frameTimeMs: stats(gaps),
    fps: elapsedSec && gaps.length ? gaps.length / elapsedSec : null,
    longTasks: {
      count: longTaskDurations.length,
      totalMs: longTotal,
      maxMs: longTaskDurations.length ? Math.max(...longTaskDurations) : null,
      perSec: elapsedSec ? longTaskDurations.length / elapsedSec : null,
      blockingPct: elapsedMs && elapsedMs > 0 ? (longTotal / elapsedMs) * 100 : null,
    },
    webviewOutputEvents: Number.isFinite(sample.webviewOutputEvents)
      ? sample.webviewOutputEvents
      : null,
    webviewHooked: Boolean(sample.webviewHooked),
    probeErrors: Array.isArray(sample.errors) ? sample.errors : [],
  };
}

/**
 * Derive the chunk→paint block from a backend byte-counter delta plus the
 * folded frontend sample.
 *
 * @param {{bytes:number,elapsedMs:number}} backendDelta
 * @param {ReturnType<typeof foldFrontendSample>} frontend
 * @returns {object}
 */
export function foldChunkRates(backendDelta, frontend) {
  const elapsedSec =
    backendDelta && backendDelta.elapsedMs > 0 ? backendDelta.elapsedMs / 1000 : null;
  const bytesPerSec = elapsedSec ? (backendDelta.bytes ?? 0) / elapsedSec : null;
  const feElapsedSec =
    frontend && Number.isFinite(frontend.elapsedMs) && frontend.elapsedMs > 0
      ? frontend.elapsedMs / 1000
      : null;
  const events = frontend?.webviewOutputEvents;
  const webviewEventsPerSec =
    feElapsedSec && Number.isFinite(events) ? events / feElapsedSec : null;
  const paintsPerWebviewEvent =
    Number.isFinite(events) && events > 0 && Number.isFinite(frontend?.frameCount)
      ? frontend.frameCount / events
      : null;
  return {
    totalBytes: backendDelta?.bytes ?? null,
    elapsedMs: backendDelta?.elapsedMs ?? null,
    bytesPerSec,
    webviewEventsPerSec,
    paintsPerWebviewEvent,
  };
}
