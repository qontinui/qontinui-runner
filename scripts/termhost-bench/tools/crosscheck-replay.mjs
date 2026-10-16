#!/usr/bin/env node
/**
 * Replay the probe's capture cross-check (plan D2) over a probe watch file, so a
 * reviewer can re-derive every figure the rule was tuned on from a CI artifact.
 *
 *   node scripts/termhost-bench/tools/crosscheck-replay.mjs <probe-*.jsonl>... [--json]
 *
 * It reads the watch's `frame`, `first_seen` and `summary` records and reports,
 * per file:
 * - each source's frame-spacing p50 and its spacing histogram in refresh
 *   intervals (how often the compositor presented);
 * - the CURRENT rule (`crates/termhost-probe/src/model/analysis.rs` `crosscheck`,
 *   ported below): F, the median gap and its band and margin, the per-marker
 *   band and the share outside it, the cadence ratio, the verdict;
 * - the RETIRED rule ("any one marker more than one refresh interval apart"):
 *   how many markers it counted, how many of those had WGC later, and of those
 *   how many had no primary frame between the two times (sampling phase, not a
 *   defect).
 *
 * The port must follow the Rust rule; `__tests__/crosscheck-replay.test.mjs`
 * pins it on a synthetic watch. Node built-ins only. Input files are data: they
 * are parsed as JSON lines and nothing in them is executed.
 */

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

/** Mirrors analysis.rs. */
export const RULE = Object.freeze({
  markerBandFrames: 1.0,
  medianBandFrames: 0.25,
  maxOutsideShare: 0.05,
  tailMinCompared: 20,
  medianMinCompared: 3,
  frameMaxRefreshes: 2.0,
  frameMinDeltas: 20,
  maxCadenceRatio: 1.5,
});

/** `selftest::render_offset_ms`: the stage each source stamps, relative to DDA. */
const RENDER_OFFSET_MS = { wgc: 10, dda: 0 };

const rank = (p, n) => Math.min(Math.max(Math.ceil((p * n) / 100), 1), n);
const cmpBig = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

/** Parse a watch file's text into the records the replay needs. */
export function parseWatch(text) {
  const frames = new Map();
  const first = new Map();
  let summary = null;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let r;
    try {
      r = JSON.parse(line);
    } catch {
      continue;
    }
    if (r?.type === "frame" && typeof r.source === "string") {
      if (!frames.has(r.source)) frames.set(r.source, []);
      frames.get(r.source).push({ index: r.index, t: BigInt(r.t_ns), missed: r.missed ?? null });
    } else if (r?.type === "first_seen" && typeof r.source === "string") {
      if (!first.has(r.source)) first.set(r.source, new Map());
      first.get(r.source).set(`${r.kind}:${r.session}:${r.seq}`, { kind: r.kind, t: BigInt(r.t_ns), frame: r.frame, missed: r.missed ?? null });
    } else if (r?.type === "summary") {
      summary = r;
    }
  }
  for (const list of frames.values()) list.sort((a, b) => cmpBig(a.t, b.t));
  return { frames, first, summary };
}

/** A source's frame-spacing p50 (ms), or null below `RULE.frameMinDeltas` deltas. */
export function spacingP50(frames) {
  const d = [];
  for (let i = 1; i < frames.length; i++) d.push(Number(frames[i].t - frames[i - 1].t) / 1e6);
  if (d.length < RULE.frameMinDeltas) return null;
  d.sort((a, b) => a - b);
  return d[rank(50, d.length) - 1];
}

/** How a source's spacings fall in whole refresh intervals: {"1": n, "2": n, ...}. */
export function spacingHistogram(frames, refreshMs) {
  const h = {};
  for (let i = 1; i < frames.length; i++) {
    const b = Math.round(Number(frames[i].t - frames[i - 1].t) / 1e6 / refreshMs);
    h[b] = (h[b] ?? 0) + 1;
  }
  return h;
}

/**
 * The rule over one parsed watch.
 *
 * @param {{refreshMs?: number, primary?: string, secondary?: string}} [o]
 */
export function replay(watch, o = {}) {
  const primary = o.primary ?? watch.summary?.primary ?? "wgc";
  const secondary = o.secondary ?? watch.summary?.secondary ?? "dda";
  const refreshMs = o.refreshMs ?? watch.summary?.refresh_ms;
  if (!(refreshMs > 0)) throw new Error("no refresh interval: pass --refresh-ms or a watch with a summary record");
  const pFrames = watch.frames.get(primary) ?? [];
  const sFrames = watch.frames.get(secondary) ?? [];
  const sByIndex = new Map(sFrames.map((f) => [f.index, f]));
  const pSp = spacingP50(pFrames);
  const sSp = spacingP50(sFrames);
  const F = Math.min(Math.max(pSp !== null && sSp !== null ? sSp : refreshMs, refreshMs), RULE.frameMaxRefreshes * refreshMs);
  const cadenceRatio = pSp !== null && sSp !== null ? pSp / sSp : null;
  const offset = (RENDER_OFFSET_MS[primary] ?? 0) - (RENDER_OFFSET_MS[secondary] ?? 0);
  const lo = Math.min(offset, 0);
  const hi = Math.max(offset, 0);
  const markerBand = [lo - RULE.markerBandFrames * F, hi + RULE.markerBandFrames * F];
  const medianBand = [lo - RULE.medianBandFrames * F, hi + RULE.medianBandFrames * F];

  const gaps = [];
  let coalesced = 0;
  let retiredOver = 0;
  let retiredPrimaryLater = 0;
  let retiredNoPrimaryFrameBetween = 0;
  const pFirst = watch.first.get(primary) ?? new Map();
  const sFirst = watch.first.get(secondary) ?? new Map();
  const pTimes = pFrames.map((f) => f.t);
  for (const [key, p] of pFirst) {
    if (p.kind === "calibration") continue;
    const q = sFirst.get(key);
    if (!q) continue;
    const missed = q.missed ?? sByIndex.get(q.frame)?.missed ?? 0;
    if (missed > 0 && q.t > p.t) {
      coalesced++;
      continue;
    }
    const gap = Number(p.t - q.t) / 1e6;
    gaps.push(gap);
    if (Math.abs(gap) > refreshMs) {
      retiredOver++;
      if (gap > 0) {
        retiredPrimaryLater++;
        // The primary frame before the one that first showed the marker: did it
        // come after the secondary already showed it?
        const i = pTimes.findIndex((t) => t === p.t);
        if (!(i > 0 && pTimes[i - 1] >= q.t)) retiredNoPrimaryFrameBetween++;
      }
    }
  }
  const sorted = [...gaps].sort((a, b) => a - b);
  const n = sorted.length;
  const median = n > 0 ? sorted[rank(50, n) - 1] : null;
  const outside = sorted.filter((g) => g < markerBand[0] || g > markerBand[1]).length;
  const share = n > 0 ? outside / n : null;
  let verdict = "not_performed";
  const why = [];
  if (n >= RULE.medianMinCompared) {
    if (median < medianBand[0] || median > medianBand[1]) why.push("systematic");
    if (cadenceRatio !== null && (cadenceRatio > RULE.maxCadenceRatio || cadenceRatio * RULE.maxCadenceRatio < 1)) why.push("cadence");
    if (n >= RULE.tailMinCompared && share > RULE.maxOutsideShare) why.push("tail");
    verdict = why.length ? "disagree" : "agree";
  }
  const r3 = (x) => (x === null ? null : Math.round(x * 1000) / 1000);
  return {
    primary,
    secondary,
    refreshMs,
    spacingP50: { [primary]: r3(pSp), [secondary]: r3(sSp) },
    spacingHistogram: { [primary]: spacingHistogram(pFrames, refreshMs), [secondary]: spacingHistogram(sFrames, refreshMs) },
    frameMs: r3(F),
    cadenceRatio: r3(cadenceRatio),
    compared: n,
    coalesced,
    medianGapMs: r3(median),
    medianBandMs: medianBand.map(r3),
    medianMarginMs: median === null ? null : r3(Math.min(median - medianBand[0], medianBand[1] - median)),
    markerBandMs: markerBand.map(r3),
    outside,
    outsideShare: r3(share),
    verdict,
    why,
    retired: { overOneRefresh: retiredOver, primaryLater: retiredPrimaryLater, noPrimaryFrameBetween: retiredNoPrimaryFrameBetween },
  };
}

function main(argv) {
  const json = argv.includes("--json");
  const ri = argv.indexOf("--refresh-ms");
  const refreshMs = ri >= 0 ? Number(argv[ri + 1]) : undefined;
  const files = argv.filter((a, i) => !a.startsWith("--") && !(ri >= 0 && i === ri + 1));
  if (files.length === 0) {
    process.stderr.write("usage: crosscheck-replay.mjs <probe-*.jsonl>... [--json] [--refresh-ms N]\n");
    return 2;
  }
  for (const f of files) {
    const r = replay(parseWatch(readFileSync(f, "utf8")), { refreshMs });
    if (json) {
      process.stdout.write(`${JSON.stringify({ file: f, ...r })}\n`);
      continue;
    }
    const name = f.split(/[\\/]/).pop();
    process.stdout.write(
      `${name}: F ${r.frameMs} ms (spacing p50 ${r.primary} ${r.spacingP50[r.primary]}, ${r.secondary} ${r.spacingP50[r.secondary]}; ratio ${r.cadenceRatio}) ` +
        `median ${r.medianGapMs} ms in [${r.medianBandMs.join(", ")}] (margin ${r.medianMarginMs} ms) ` +
        `outside [${r.markerBandMs.join(", ")}]: ${r.outside}/${r.compared} (${r.outsideShare === null ? "-" : (100 * r.outsideShare).toFixed(1)}%) ` +
        `=> ${r.verdict}${r.why.length ? ` (${r.why.join(", ")})` : ""}; retired rule: ${r.retired.overOneRefresh} over one refresh, ` +
        `${r.retired.primaryLater} with ${r.primary} later, ${r.retired.noPrimaryFrameBetween} of those with no ${r.primary} frame between\n` +
        `  spacing (refresh intervals) ${r.primary} ${JSON.stringify(r.spacingHistogram[r.primary])} ${r.secondary} ${JSON.stringify(r.spacingHistogram[r.secondary])}\n`,
    );
  }
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) process.exitCode = main(process.argv.slice(2));
