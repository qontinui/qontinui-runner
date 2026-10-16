#!/usr/bin/env node
/**
 * Replay the probe's capture cross-check (plan D2) over a probe watch file, so a
 * reviewer can re-derive every figure the rule was tuned on from a CI artifact.
 *
 *   node scripts/termhost-bench/tools/crosscheck-replay.mjs <probe-*.jsonl>... [--json] [--profile strict|software-compositor]
 *
 * The profile (plan D2: `strict`, or `software-compositor` for windows-hosted,
 * which drops the DDA-coalescing fallback) is the one the watch's summary
 * records it was told, `--profile` to judge it under the other, else `strict`.
 *
 * It reads the watch's `frame`, `first_seen` and `summary` records and reports,
 * per file:
 * - each source's frame-spacing p50 and its spacing histogram in refresh
 *   intervals (how often the compositor presented);
 * - the CURRENT rule (`crates/termhost-probe/src/model/analysis.rs` `crosscheck`,
 *   ported below): F and where it came from (DDA's spacing p50, or R when DDA
 *   had too few deltas or coalesced presentations on more than 5% of its
 *   frames), the median gap and its band and margin, the late share and its
 *   margin, the share outside the one-frame band, the verdict;
 * - the RETIRED rule ("any one marker more than one refresh interval apart"):
 *   how many markers it counted, how many of those had WGC later, and of those
 *   how many had no primary frame between the two times (sampling phase, not a
 *   defect).
 *
 * The port must follow the Rust rule; `__tests__/crosscheck-replay.test.mjs`
 * pins it on synthetic watches. Node built-ins only. Input files are data: they
 * are parsed as JSON lines and nothing in them is executed.
 *
 * A watch must have been recorded with `--frames all` (the probe's default):
 * with `changes` or `none` its frame records are not every frame, so every
 * spacing — and so F — would be wrong. The tool refuses such a watch, and one
 * with no start record to say which it was.
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
  lateBandFrames: 0.5,
  maxLateShare: 0.1,
  maxSecondaryMissedShare: 0.05,
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
  let start = null;
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
    } else if (r?.type === "start" && start === null) {
      start = r;
    }
  }
  for (const list of frames.values()) list.sort((a, b) => cmpBig(a.t, b.t));
  return { frames, first, summary, start };
}

/**
 * Which `--frames` mode the watch was recorded in, from its start record's
 * argv: `all` (also the probe's default when the flag is absent), `changes`,
 * `none`, or null when there is no start record to read.
 */
export function framesMode(start) {
  if (!start || !Array.isArray(start.args)) return null;
  const args = start.args.map(String);
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--frames") return args[i + 1] ?? null;
    if (args[i].startsWith("--frames=")) return args[i].slice("--frames=".length);
  }
  return "all";
}

/** A source's frame-spacing p50 (ms), or null below `RULE.frameMinDeltas` deltas. */
export function spacingP50(frames) {
  const d = [];
  for (let i = 1; i < frames.length; i++) d.push(Number(frames[i].t - frames[i - 1].t) / 1e6);
  if (d.length < RULE.frameMinDeltas) return null;
  d.sort((a, b) => a - b);
  return d[rank(50, d.length) - 1];
}

/** The share of a source's frames that coalesced presentations (`missed > 0`), or null. */
export function secondaryMissedShare(frames) {
  const known = frames.filter((f) => f.missed !== null && f.missed !== undefined);
  if (known.length === 0) return null;
  return known.filter((f) => f.missed > 0).length / known.length;
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
  if (!o.skipFramesCheck) {
    const mode = framesMode(watch.start);
    if (mode !== "all") {
      throw new Error(
        mode === null
          ? "the watch has no start record, so whether it recorded every frame (--frames all) is unknown: refusing to size F from it"
          : `the watch was recorded with --frames ${mode}, so its frame records are not every frame: refusing to size F from them`,
      );
    }
  }
  const primary = o.primary ?? watch.summary?.primary ?? "wgc";
  const secondary = o.secondary ?? watch.summary?.secondary ?? "dda";
  const refreshMs = o.refreshMs ?? watch.summary?.refresh_ms;
  if (!(refreshMs > 0)) throw new Error("no refresh interval: pass --refresh-ms or a watch with a summary record");
  const pFrames = watch.frames.get(primary) ?? [];
  const sFrames = watch.frames.get(secondary) ?? [];
  const sByIndex = new Map(sFrames.map((f) => [f.index, f]));
  const rule = { ...RULE, ...(o.rule ?? {}) };
  const profile = o.profile ?? watch.summary?.crosscheck?.profile ?? "strict";
  if (!["strict", "software-compositor"].includes(profile)) throw new Error(`unknown cross-check profile "${profile}"`);
  const pSp = spacingP50(pFrames);
  const sSp = spacingP50(sFrames);
  const sMissedShare = secondaryMissedShare(sFrames);
  const sMissedTooMany = profile === "strict" && sMissedShare !== null && sMissedShare > rule.maxSecondaryMissedShare;
  const F = Math.min(Math.max(sSp !== null && !sMissedTooMany ? sSp : refreshMs, refreshMs), rule.frameMaxRefreshes * refreshMs);
  const offset = (RENDER_OFFSET_MS[primary] ?? 0) - (RENDER_OFFSET_MS[secondary] ?? 0);
  const lo = Math.min(offset, 0);
  const hi = Math.max(offset, 0);
  const markerBand = [lo - rule.markerBandFrames * F, hi + rule.markerBandFrames * F];
  const medianBand = [lo - rule.medianBandFrames * F, hi + rule.medianBandFrames * F];
  const lateBound = hi + rule.lateBandFrames * F;

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
  const late = sorted.filter((g) => g > lateBound).length;
  const lateShare = n > 0 ? late / n : null;
  let verdict = "not_performed";
  const why = [];
  if (n >= rule.medianMinCompared) {
    if (median < medianBand[0] || median > medianBand[1]) why.push("systematic");
    if (n >= rule.tailMinCompared && lateShare > rule.maxLateShare) why.push("late");
    if (n >= rule.tailMinCompared && share > rule.maxOutsideShare) why.push("tail");
    verdict = why.length ? "disagree" : "agree";
  }
  const r3 = (x) => (x === null ? null : Math.round(x * 1000) / 1000);
  return {
    profile,
    primary,
    secondary,
    refreshMs,
    spacingP50: { [primary]: r3(pSp), [secondary]: r3(sSp) },
    spacingHistogram: { [primary]: spacingHistogram(pFrames, refreshMs), [secondary]: spacingHistogram(sFrames, refreshMs) },
    frameMs: r3(F),
    secondaryMissedShare: r3(sMissedShare),
    frameFellBackToRefresh: sSp === null || sMissedTooMany,
    compared: n,
    coalesced,
    medianGapMs: r3(median),
    medianBandMs: medianBand.map(r3),
    medianMarginMs: median === null ? null : r3(Math.min(median - medianBand[0], medianBand[1] - median)),
    markerBandMs: markerBand.map(r3),
    outside,
    outsideShare: r3(share),
    lateBoundMs: r3(lateBound),
    late,
    lateShare: r3(lateShare),
    lateMarginPoints: lateShare === null ? null : r3(100 * (rule.maxLateShare - lateShare)),
    verdict,
    why,
    retired: { overOneRefresh: retiredOver, primaryLater: retiredPrimaryLater, noPrimaryFrameBetween: retiredNoPrimaryFrameBetween },
  };
}

function main(argv) {
  const json = argv.includes("--json");
  const ri = argv.indexOf("--refresh-ms");
  const refreshMs = ri >= 0 ? Number(argv[ri + 1]) : undefined;
  const pi = argv.indexOf("--profile");
  const profile = pi >= 0 ? argv[pi + 1] : undefined;
  const files = argv.filter((a, i) => !a.startsWith("--") && !(ri >= 0 && i === ri + 1) && !(pi >= 0 && i === pi + 1));
  if (files.length === 0) {
    process.stderr.write("usage: crosscheck-replay.mjs <probe-*.jsonl>... [--json] [--refresh-ms N]\n");
    return 2;
  }
  for (const f of files) {
    let r;
    try {
      r = replay(parseWatch(readFileSync(f, "utf8")), { refreshMs, profile });
    } catch (e) {
      process.stderr.write(`${f}: ${e.message}\n`);
      return 2;
    }
    if (json) {
      process.stdout.write(`${JSON.stringify({ file: f, ...r })}\n`);
      continue;
    }
    const name = f.split(/[\\/]/).pop();
    process.stdout.write(
      `${name} [${r.profile}]: F ${r.frameMs} ms${r.frameFellBackToRefresh ? " (R: fallback)" : ""} (spacing p50 ${r.primary} ${r.spacingP50[r.primary]}, ${r.secondary} ${r.spacingP50[r.secondary]}; ${r.secondary} coalesced on ${r.secondaryMissedShare === null ? "-" : (100 * r.secondaryMissedShare).toFixed(1)}% of frames) ` +
        `median ${r.medianGapMs} ms in [${r.medianBandMs.join(", ")}] (margin ${r.medianMarginMs} ms) ` +
        `late > ${r.lateBoundMs}: ${r.late}/${r.compared} (${r.lateShare === null ? "-" : (100 * r.lateShare).toFixed(1)}%, margin ${r.lateMarginPoints} points) ` +
        `outside [${r.markerBandMs.join(", ")}]: ${r.outside}/${r.compared} (${r.outsideShare === null ? "-" : (100 * r.outsideShare).toFixed(1)}%) ` +
        `=> ${r.verdict}${r.why.length ? ` (${r.why.join(", ")})` : ""}; retired rule: ${r.retired.overOneRefresh} over one refresh, ` +
        `${r.retired.primaryLater} with ${r.primary} later, ${r.retired.noPrimaryFrameBetween} of those with no ${r.primary} frame between\n` +
        `  spacing (refresh intervals) ${r.primary} ${JSON.stringify(r.spacingHistogram[r.primary])} ${r.secondary} ${JSON.stringify(r.spacingHistogram[r.secondary])}\n`,
    );
  }
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) process.exitCode = main(process.argv.slice(2));
