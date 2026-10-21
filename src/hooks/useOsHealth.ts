/**
 * useOsHealth — this machine's kernel and process-creation health, as the
 * runner grades it (plan
 * `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`,
 * Phase 5, D5).
 *
 * The Rust `os_health` module measures Token objects, kernel pool and OS
 * spawn latency on its own thread, grades them (S7, S8, S9) and serves the
 * result as `/health` `osHealth`, from the `os_health_get` command, and as the
 * `os-health-changed` event whenever the verdict changes.
 *
 * Module singleton (the `useCoordDrainState` pattern): one listener, one
 * getter read at first subscribe, and a 30 s poll — the reasons can change
 * without the verdict changing, and a poll is also what moves the banner's
 * dwell clock forward.
 *
 * The singleton also owns the banner's dwell and dismissal state, so a
 * remount of the banner does not restart the 10-minute dwell or forget a
 * dismissal.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors `OS_HEALTH_EVENT` in `os_health/mod.rs`. */
export const OS_HEALTH_EVENT = "os-health-changed";

/** Mirrors the Rust getter command name. */
export const OS_HEALTH_GET_COMMAND = "os_health_get";

/** Plan D5: the banner shows once `breach` has held this long. */
export const DEFAULT_DWELL_SECS = 600;

export const POLL_MS = 30_000;

export type OsHealthVerdict = "ok" | "warn" | "breach" | "unknown";

/** One rule that did not grade `ok`. Mirrors `Reason::to_json`. */
export interface OsHealthReason {
  axis: string;
  rule: string;
  band: "warn" | "breach" | "unknown";
  value: number | null;
  threshold: number | null;
  message: string;
}

export interface OsHealthReadings {
  kernelPagedBytes: number | null;
  kernelNonpagedBytes: number | null;
  tokenObjects: number | null;
  spawnProbe: {
    p50Ms: number | null;
    maxMs: number | null;
    count: number;
    timeouts: number;
    errors: number;
    errorCode: number | null;
  } | null;
}

/** The subset of `/health` `osHealth` the banner reads. */
export interface OsHealthPayload {
  verdict: OsHealthVerdict;
  reasons: OsHealthReason[];
  readings: OsHealthReadings | null;
  platform: string;
  /** Absent on a runner predating the field: the plan's 600 s applies. */
  bannerDwellSecs?: number;
  injected?: boolean;
}

const VERDICTS: readonly string[] = ["ok", "warn", "breach", "unknown"];

/** Runtime guard for a payload crossing the Tauri boundary. */
export function isOsHealthPayload(value: unknown): value is OsHealthPayload {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.verdict === "string" &&
    VERDICTS.includes(v.verdict) &&
    Array.isArray(v.reasons) &&
    typeof v.platform === "string"
  );
}

/** The banner's dwell and dismissal state. */
export interface BannerTracker {
  /** When the current unbroken run of `breach` began (ms), or null. */
  breachSince: number | null;
  /** The operator dismissed the banner; holds until the verdict next clears. */
  dismissed: boolean;
}

export const INITIAL_TRACKER: BannerTracker = { breachSince: null, dismissed: false };

/**
 * PURE: fold one verdict observed at `nowMs` into the tracker.
 *
 * - `breach` starts the dwell clock if it is not running.
 * - `ok` CLEARS: the dwell stops and a dismissal is forgotten (D5: "a
 *   dismissal lasts until the verdict next clears").
 * - `warn` stops the dwell (breach did not hold) but keeps a dismissal — the
 *   condition has not cleared.
 * - `unknown` changes nothing: it is not evidence the breach ended, and it is
 *   not evidence it continued either, so the clock neither resets nor is
 *   allowed to show the banner on its own (see `bannerVisible`).
 */
export function advanceTracker(
  t: BannerTracker,
  verdict: OsHealthVerdict,
  nowMs: number,
): BannerTracker {
  switch (verdict) {
    case "breach":
      return t.breachSince === null ? { ...t, breachSince: nowMs } : t;
    case "ok":
      return { breachSince: null, dismissed: false };
    case "warn":
      return t.breachSince === null ? t : { ...t, breachSince: null };
    case "unknown":
      return t;
  }
}

/** PURE: whether the banner shows. */
export function bannerVisible(
  t: BannerTracker,
  verdict: OsHealthVerdict | null,
  nowMs: number,
  dwellSecs: number,
): boolean {
  return (
    verdict === "breach" &&
    !t.dismissed &&
    t.breachSince !== null &&
    nowMs - t.breachSince >= dwellSecs * 1000
  );
}

export interface OsHealthState {
  payload: OsHealthPayload | null;
  /** Whether the App-root banner shows now. */
  bannerVisible: boolean;
}

let cached: OsHealthPayload | null = null;
let tracker: BannerTracker = INITIAL_TRACKER;
const subscribers = new Set<(s: OsHealthState) => void>();
let started = false;
let pollId: ReturnType<typeof setInterval> | null = null;
let unlisten: (() => void) | null = null;

function state(): OsHealthState {
  const dwell = cached?.bannerDwellSecs ?? DEFAULT_DWELL_SECS;
  return {
    payload: cached,
    bannerVisible: bannerVisible(tracker, cached?.verdict ?? null, Date.now(), dwell),
  };
}

function notify() {
  const s = state();
  for (const fn of subscribers) fn(s);
}

function publish(payload: unknown) {
  if (!isOsHealthPayload(payload)) return;
  cached = payload;
  tracker = advanceTracker(tracker, payload.verdict, Date.now());
  notify();
}

async function readNow() {
  try {
    publish(await invoke<unknown>(OS_HEALTH_GET_COMMAND));
  } catch (err) {
    console.warn("[os-health] os_health_get failed:", err);
  }
}

function ensureStarted() {
  if (started) return;
  started = true;
  void listen<unknown>(OS_HEALTH_EVENT, (event) => publish(event.payload))
    .then((fn) => {
      unlisten = fn;
    })
    .catch((err) => {
      // Outside Tauri, or with no event plugin: the 30 s poll still runs.
      console.warn("[os-health] listen failed:", err);
    });
  void readNow();
  pollId = setInterval(() => void readNow(), POLL_MS);
}

/** Dismiss the banner until the verdict next clears. */
export function dismissOsHealthBanner() {
  tracker = { ...tracker, dismissed: true };
  notify();
}

/** Subscribe outside React. Returns the unsubscribe function. */
export function subscribeOsHealth(fn: (s: OsHealthState) => void): () => void {
  ensureStarted();
  subscribers.add(fn);
  fn(state());
  return () => {
    subscribers.delete(fn);
  };
}

/** Tests only: forget every module-level value. */
export function resetOsHealthForTests() {
  if (pollId !== null) clearInterval(pollId);
  unlisten?.();
  pollId = null;
  unlisten = null;
  started = false;
  cached = null;
  tracker = INITIAL_TRACKER;
  subscribers.clear();
}

/** The latest payload and whether the banner shows. */
export function useOsHealth(): OsHealthState {
  const [s, setS] = useState<OsHealthState>(state);
  useEffect(() => subscribeOsHealth(setS), []);
  return s;
}
