/**
 * ResourceGuardSettings — the machine's two resource lanes, in one panel.
 *
 * Plan `2026-08-07-runner-resource-guard-and-session-protection.md`, Part B
 * item 2. Wraps two runner settings groups:
 *
 *   - `session_guard` (`settings::SessionGuardSettings`) — the floors that
 *     protect LIVE interactive sessions. Overnight 2026-08-06→07 Windows'
 *     Resource-Exhaustion-Detector fired 12 times on this fleet and several
 *     Claude Code sessions died inside runner-spawned terminals while the
 *     windows stayed open. Nothing local was watching; these floors are what
 *     watches.
 *
 *     **This panel edits BOTH halves of that group.** Since
 *     `2026-08-30-load-aware-spawn-admission-control` the same struct also
 *     carries two OS-thread CEILINGS (`warn_thread_count` /
 *     `critical_thread_count`, defaults 256 / 400 against tokio's 512-slot
 *     blocking pool), enforced by the same gate and by the same master switch.
 *     They are edited here because every CRITICAL refusal the gate issues —
 *     either lane — ends with "The limits live in Settings > Resource Guard",
 *     and the thread-lane toast even carries an "Open Resource Guard settings"
 *     action. While this panel showed the GiB fields alone, an operator refused
 *     at 412 threads arrived here and could respond only by turning the whole
 *     guard off (killing the memory lane too) or by hand-editing
 *     `settings.json` — the second hand-edited surface the `ci_node` bullet
 *     below rejects, reintroduced by the lane that shipped later.
 *
 *     A ceiling is not a floor and nothing about it may be copied from the
 *     memory pair: the ordering rule inverts (`critical >= warn`), the
 *     three-term fold is a `min`, and the clamp pushes UP. The panel spells that
 *     out beside the inputs, and `resourceGuardHelpers` does it in code.
 *   - `ci_node` (`settings::CiNodeSettings`) — the floors that decide whether
 *     coord may send this box CI work. It had NO settings UI before this panel
 *     and `settings.rs` documented it as hand-edited JSON; shipping the first
 *     resource-lane panel while leaving a second hand-edited surface behind is
 *     the trade the plan explicitly rejects.
 *
 * Both lanes read the same underlying quantity on Windows — free COMMIT, via
 * `fleet::resource_sample::available_commit_bytes` — and differ by VERDICT, not
 * by number. That ladder (warn 3 GiB → block 1.5 GiB here; `ci_node` hard-
 * rejects at 4 GiB; `cargo-guard.sh` and the supervisor defer at 5 GiB) is why
 * the two groups belong on one page: a floor is only meaningful relative to its
 * neighbours.
 *
 * Follows `LockYieldPolicySettings.tsx`: `invoke<TauriResult<T>>("get_…")` on
 * mount, `invoke("save_…")` on save. Pure helpers (unit conversion, clamping,
 * the ordering predicate) live in `resourceGuardHelpers.ts`.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, X, ShieldAlert, Info, TriangleAlert } from "lucide-react";
import { SectionHeader } from "./SectionHeader";
import { getAccentColors } from "@/design-system";
import {
  DISK_FLOOR_MAX_GIB,
  DISK_FLOOR_MIN_GIB,
  MAX_CONCURRENT_BUILDS_MAX,
  MAX_CONCURRENT_BUILDS_MIN,
  SESSION_FLOOR_CAP_GIB,
  SESSION_FLOOR_DEFAULT_CRITICAL_GIB,
  SESSION_FLOOR_DEFAULT_WARN_GIB,
  SESSION_FLOOR_MAX_GIB,
  SESSION_FLOOR_MIN_GIB,
  THREAD_CEILING_DEFAULT_CRITICAL,
  THREAD_CEILING_DEFAULT_WARN,
  THREAD_CEILING_FLOOR,
  THREAD_CEILING_INPUT_MAX,
  THREAD_CEILING_INPUT_MIN,
  bytesToGib,
  clampGib,
  clampInt,
  effectiveSessionFloorsGib,
  effectiveThreadCeilings,
  gibToBytes,
  parseRepoAllowlist,
  sessionFloorsAreInverted,
  threadCeilingsAreInverted,
} from "./resourceGuardHelpers";
import type { LogFunction } from "./types";

interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

/**
 * Wire shape of `settings::SessionGuardSettings` — two lanes, snake_case. The
 * memory pair is in BYTES (the 1.5 GiB critical default has no integer-GiB
 * spelling); the thread pair is a plain count of OS threads.
 */
export interface SessionGuardSettingsValue {
  warn_free_commit_bytes: number;
  critical_free_commit_bytes: number;
  warn_thread_count: number;
  critical_thread_count: number;
  enabled: boolean;
}

/** Wire shape of `settings::CiNodeSettings`. */
export interface CiNodeSettingsValue {
  enabled: boolean;
  max_concurrent_builds: number;
  repo_allowlist: string[];
  min_free_disk_gb: number;
}

interface ResourceGuardSettingsProps {
  onLog: LogFunction;
}

/**
 * Mirrors the Rust `Default` impls exactly. Used both as the pre-load state and
 * as the per-field fallback when an input is typed into nonsense — so the panel
 * can never show a floor the runner would not have used.
 */
const DEFAULT_SESSION_GUARD: SessionGuardSettingsValue = {
  warn_free_commit_bytes: 3 * 1024 * 1024 * 1024,
  critical_free_commit_bytes: (3 * 1024 * 1024 * 1024) / 2,
  warn_thread_count: THREAD_CEILING_DEFAULT_WARN,
  critical_thread_count: THREAD_CEILING_DEFAULT_CRITICAL,
  enabled: true,
};

const DEFAULT_CI_NODE: CiNodeSettingsValue = {
  enabled: false,
  max_concurrent_builds: 1,
  repo_allowlist: [],
  min_free_disk_gb: 20,
};

/**
 * `ci_node`'s hard-reject floor (`ci_node::admission::MIN_FREE_COMMIT_GB`).
 * Shown, not editable: it is a compiled constant, and the point of surfacing it
 * is that the operator can see where their own warn floor sits in the ladder.
 */
const CI_NODE_REJECT_FLOOR_GIB = 4;

export function ResourceGuardSettings({ onLog }: ResourceGuardSettingsProps) {
  const [warnGib, setWarnGib] = useState(bytesToGib(DEFAULT_SESSION_GUARD.warn_free_commit_bytes));
  const [criticalGib, setCriticalGib] = useState(
    bytesToGib(DEFAULT_SESSION_GUARD.critical_free_commit_bytes),
  );
  const [warnThreads, setWarnThreads] = useState(DEFAULT_SESSION_GUARD.warn_thread_count);
  const [criticalThreads, setCriticalThreads] = useState(
    DEFAULT_SESSION_GUARD.critical_thread_count,
  );
  const [guardEnabled, setGuardEnabled] = useState(DEFAULT_SESSION_GUARD.enabled);

  const [ciNode, setCiNode] = useState<CiNodeSettingsValue>(DEFAULT_CI_NODE);
  const [allowlistText, setAllowlistText] = useState("");

  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        // Both groups in one pass: the panel is only coherent when it shows the
        // whole ladder, so a partial load would render a half-truth.
        const [guardResult, ciResult] = await Promise.all([
          invoke<TauriResult<SessionGuardSettingsValue>>("get_session_guard_settings"),
          invoke<TauriResult<CiNodeSettingsValue>>("get_ci_node_settings"),
        ]);
        if (cancelled) return;

        if (guardResult?.success && guardResult.data) {
          const g = { ...DEFAULT_SESSION_GUARD, ...guardResult.data };
          setWarnGib(bytesToGib(g.warn_free_commit_bytes));
          setCriticalGib(bytesToGib(g.critical_free_commit_bytes));
          setWarnThreads(g.warn_thread_count);
          setCriticalThreads(g.critical_thread_count);
          setGuardEnabled(g.enabled);
          onLog("debug", "Session-guard settings loaded");
        } else {
          onLog("warning", "Using default session-guard settings");
        }

        if (ciResult?.success && ciResult.data) {
          const c = { ...DEFAULT_CI_NODE, ...ciResult.data };
          setCiNode(c);
          setAllowlistText(c.repo_allowlist.join("\n"));
          onLog("debug", "CI-node settings loaded");
        } else {
          onLog("warning", "Using default CI-node settings");
        }
      } catch (err) {
        if (cancelled) return;
        console.error("Failed to load resource-guard settings:", err);
        setError(`Failed to load settings: ${String(err)}`);
        onLog("error", `Failed to load resource-guard settings: ${String(err)}`);
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
    // onLog is stable in practice (parent useCallback), but we deliberately
    // skip it as a dep to avoid reload thrash if a parent re-creates it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // The Rust writer refuses an inverted ladder. Saying so inline is strictly
  // better than letting the operator hit Save to be told about something the
  // input boxes already show.
  const invertedFloors = sessionFloorsAreInverted(warnGib, criticalGib);
  // The SAME rule on the other lane, with the comparison inverted — a ceiling's
  // lighter verdict is the lower number. Both are refused by the Rust writer;
  // either one disables Save.
  const invertedCeilings = threadCeilingsAreInverted(warnThreads, criticalThreads);
  const warnAboveCiNodeReject = warnGib > CI_NODE_REJECT_FLOOR_GIB;

  // What the runner will actually enforce, beside what is configured. The
  // enforced floor is `max(configured, built-in default)`, capped, with the
  // ladder coerced — so the bottom of each input's range is inert and a value
  // above the cap is discarded. Rendering only the configured number made the
  // page state something the runner does not do.
  const effective = effectiveSessionFloorsGib(warnGib, criticalGib);
  const warnIsClamped = effective.warnGib !== warnGib;
  const criticalIsClamped = effective.criticalGib !== criticalGib;

  const effectiveThreads = effectiveThreadCeilings(warnThreads, criticalThreads);
  const warnThreadsClamped = effectiveThreads.warnThreads !== warnThreads;
  const criticalThreadsClamped = effectiveThreads.criticalThreads !== criticalThreads;

  /**
   * Save both groups INDEPENDENTLY and report per group.
   *
   * The page writes two settings groups through two commands, and either can be
   * refused on its own: the session floors on an inverted ladder, the CI-node
   * group on a `"*"` allowlist or a zero disk floor (both refused by
   * `commands::resource_guard_settings`, deliberately, to match what coord's
   * remote door refuses). A single try/catch reported "Failed to save settings"
   * for that — hiding both WHICH group failed and, worse, that the other group
   * HAD been persisted. An operator who then reloads the page sees floors they
   * were told did not save.
   *
   * So each group gets its own attempt and its own verdict, and a failure in one
   * no longer skips the other: they are independent settings with independent
   * validation, and there is no ordering between them to preserve.
   */
  const saveSettings = async () => {
    setSaving(true);
    setError(null);
    setSaveSuccess(false);

    const allowlist = parseRepoAllowlist(allowlistText);
    const saved: string[] = [];
    const failures: string[] = [];

    try {
      const guardResult = await invoke<TauriResult<null>>("save_session_guard_settings", {
        enabled: guardEnabled,
        warnFreeCommitBytes: gibToBytes(warnGib),
        criticalFreeCommitBytes: gibToBytes(criticalGib),
        warnThreadCount: warnThreads,
        criticalThreadCount: criticalThreads,
      });
      if (!guardResult?.success) {
        throw new Error(guardResult?.message || "Unknown error saving session-guard settings");
      }
      saved.push("session limits");
    } catch (err) {
      console.error("Failed to save session-guard settings:", err);
      failures.push(`session limits — ${String(err)}`);
    }

    try {
      const ciResult = await invoke<TauriResult<null>>("save_ci_node_settings", {
        enabled: ciNode.enabled,
        maxConcurrentBuilds: ciNode.max_concurrent_builds,
        repoAllowlist: allowlist,
        minFreeDiskGb: ciNode.min_free_disk_gb,
      });
      if (!ciResult?.success) {
        throw new Error(ciResult?.message || "Unknown error saving CI-node settings");
      }
      // Normalize the textarea to what was actually persisted, so a trailing
      // comma the parser dropped does not linger as unsaved-looking text. Only
      // on success: rewriting the box after a REFUSED save would make the page
      // agree with a value the runner never stored.
      setCiNode((c) => ({ ...c, repo_allowlist: allowlist }));
      setAllowlistText(allowlist.join("\n"));
      saved.push("CI-node settings");
    } catch (err) {
      console.error("Failed to save CI-node settings:", err);
      failures.push(`CI-node settings — ${String(err)}`);
    }

    setSaving(false);

    if (failures.length === 0) {
      setSaveSuccess(true);
      onLog("success", "Resource guard settings saved");
      setTimeout(() => setSaveSuccess(false), 3000);
      return;
    }

    // Name what did NOT save first (it is what the operator has to act on), and
    // then what DID — a partial write the operator does not know about is the
    // failure this reporting exists to prevent.
    const savedNote = saved.length > 0 ? ` Saved: ${saved.join(", ")}.` : "";
    const message = `Not saved: ${failures.join("; ")}.${savedNote}`;
    setError(message);
    onLog("error", `Resource guard settings partially saved — ${message}`);
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          data-content-role="status"
          data-content-label="loading resource guard settings"
          className="text-muted-foreground"
        >
          Loading resource guard settings...
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <SectionHeader
        title="Resource Guard"
        description="Limits that protect this machine. The session limits warn (and can block) before a new terminal is spawned onto a box that is out of commit or out of OS threads; the CI-node floors decide whether coord may send this machine build work."
        icon={<ShieldAlert className="w-6 h-6" />}
      />

      {error && (
        <div className={`p-3 ${getAccentColors("red").bg} rounded-lg flex items-start gap-2`}>
          <X className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("red").text} text-xs`}>{error}</span>
        </div>
      )}

      {saveSuccess && (
        <div className={`p-3 ${getAccentColors("green").bg} rounded-lg flex items-start gap-2`}>
          <Check className={`w-4 h-4 ${getAccentColors("green").text} shrink-0 mt-0.5`} />
          <span className={`${getAccentColors("green").text} text-xs`}>
            Resource guard settings saved.
          </span>
        </div>
      )}

      {/* ── Session protection ─────────────────────────────────────────── */}
      <div className="rounded-lg bg-card/50 p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h4 className="font-medium text-sm">Protect live sessions</h4>
            <p className="text-xs text-muted-foreground">
              Checked before a new terminal or runner instance is spawned — two sensors, free memory
              and OS threads, behind this one switch. Never touches a session that is already
              running.
            </p>
          </div>
          <ToggleSwitch
            data-ui-bridge-id="settings.resource-guard-session-guard-enabled"
            checked={guardEnabled}
            onChange={setGuardEnabled}
          />
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-warn-floor">
            Warn below (GiB free commit)
          </label>
          <input
            id="resource-guard-warn-floor"
            data-ui-bridge-id="settings.resource-guard-warn-floor"
            type="number"
            min={SESSION_FLOOR_MIN_GIB}
            max={SESSION_FLOOR_MAX_GIB}
            step={0.25}
            value={warnGib}
            onChange={(e) =>
              setWarnGib(
                clampGib(
                  parseFloat(e.target.value),
                  SESSION_FLOOR_MIN_GIB,
                  SESSION_FLOOR_MAX_GIB,
                  bytesToGib(DEFAULT_SESSION_GUARD.warn_free_commit_bytes),
                ),
              )
            }
            disabled={!guardEnabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            The spawn still proceeds; you get a notification naming the current headroom and this
            floor. Range {SESSION_FLOOR_MIN_GIB}-{SESSION_FLOOR_MAX_GIB} GiB.
          </p>
          <EffectiveLimit
            uiBridgeId="settings.resource-guard-warn-floor-effective"
            configured={`${warnGib} GiB`}
            effective={`${effective.warnGib} GiB`}
            clamped={warnIsClamped}
            reason={
              warnGib < SESSION_FLOOR_DEFAULT_WARN_GIB
                ? `below the runner's built-in ${SESSION_FLOOR_DEFAULT_WARN_GIB} GiB warn floor, which nothing can lower`
                : `above the ${SESSION_FLOOR_CAP_GIB} GiB ceiling, past which a floor is unreachable and every unattended spawn would refuse forever`
            }
          />
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-critical-floor">
            Block below (GiB free commit)
          </label>
          <input
            id="resource-guard-critical-floor"
            data-ui-bridge-id="settings.resource-guard-critical-floor"
            type="number"
            min={SESSION_FLOOR_MIN_GIB}
            max={SESSION_FLOOR_MAX_GIB}
            step={0.25}
            value={criticalGib}
            onChange={(e) =>
              setCriticalGib(
                clampGib(
                  parseFloat(e.target.value),
                  SESSION_FLOOR_MIN_GIB,
                  SESSION_FLOOR_MAX_GIB,
                  bytesToGib(DEFAULT_SESSION_GUARD.critical_free_commit_bytes),
                ),
              )
            }
            disabled={!guardEnabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            The heaviest verdict in the ladder — a spawn is refused by default, always with an
            explicit override. Keep it below the warn floor.
          </p>
          <EffectiveLimit
            uiBridgeId="settings.resource-guard-critical-floor-effective"
            configured={`${criticalGib} GiB`}
            effective={`${effective.criticalGib} GiB`}
            clamped={criticalIsClamped}
            reason={
              criticalGib < SESSION_FLOOR_DEFAULT_CRITICAL_GIB
                ? `below the runner's built-in ${SESSION_FLOOR_DEFAULT_CRITICAL_GIB} GiB block floor, which nothing can lower`
                : criticalGib > SESSION_FLOOR_CAP_GIB
                  ? `above the ${SESSION_FLOOR_CAP_GIB} GiB ceiling, past which a floor is unreachable and every unattended spawn would refuse forever`
                  : "above the effective warn floor, and the lighter verdict has to fire first — so it is clamped down to it rather than blocking spawns that were never warned about"
            }
          />
        </div>

        {/* ── The thread lane ──────────────────────────────────────────
            Same guard, same master switch, second sensor. Kept in this card
            rather than a card of its own because one `enabled` toggle governs
            both — `SessionGuardSettings::enabled` is documented as a statement
            about the QUESTION ("should this machine start another session right
            now?"), not about a sensor — and a separate card with no toggle of
            its own would read as independently switchable. */}
        <div className="pt-2 border-t border-border/50 space-y-1.5">
          <h5 className="font-medium text-xs">OS thread ceilings</h5>
          <p className="text-[10px] text-muted-foreground">
            The other thing this machine runs out of. On 2026-08-29 the primary runner wedged
            carrying <strong>540</strong> OS threads against tokio&apos;s 512-slot blocking pool
            while the box still had memory to spare — the free-commit floors above structurally
            could not see it. Higher is worse here, so these are <strong>ceilings</strong>: every
            rule below is the mirror of the one above it.
          </p>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-warn-threads">
            Warn above (OS threads)
          </label>
          <input
            id="resource-guard-warn-threads"
            data-ui-bridge-id="settings.resource-guard-warn-threads"
            type="number"
            min={THREAD_CEILING_INPUT_MIN}
            max={THREAD_CEILING_INPUT_MAX}
            step={1}
            value={warnThreads}
            onChange={(e) =>
              setWarnThreads(
                clampInt(
                  parseInt(e.target.value, 10),
                  THREAD_CEILING_INPUT_MIN,
                  THREAD_CEILING_INPUT_MAX,
                  DEFAULT_SESSION_GUARD.warn_thread_count,
                ),
              )
            }
            disabled={!guardEnabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            Your own spawn still proceeds with a notification. A queued gate continuation defers
            here and is re-delivered when threads free up — back-pressure that arrives early is the
            point of a queue. Range {THREAD_CEILING_INPUT_MIN}-{THREAD_CEILING_INPUT_MAX}.
          </p>
          <EffectiveLimit
            uiBridgeId="settings.resource-guard-warn-threads-effective"
            configured={`${warnThreads} threads`}
            effective={`${effectiveThreads.warnThreads} threads`}
            clamped={warnThreadsClamped}
            reason={
              warnThreads > THREAD_CEILING_DEFAULT_WARN
                ? `above the runner's built-in ${THREAD_CEILING_DEFAULT_WARN}-thread warn ceiling, which nothing can raise`
                : `below the ${THREAD_CEILING_FLOOR}-thread clamp, under which a machine measured at 150-151 threads at rest could never start a session again`
            }
          />
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-critical-threads">
            Block above (OS threads)
          </label>
          <input
            id="resource-guard-critical-threads"
            data-ui-bridge-id="settings.resource-guard-critical-threads"
            type="number"
            min={THREAD_CEILING_INPUT_MIN}
            max={THREAD_CEILING_INPUT_MAX}
            step={1}
            value={criticalThreads}
            onChange={(e) =>
              setCriticalThreads(
                clampInt(
                  parseInt(e.target.value, 10),
                  THREAD_CEILING_INPUT_MIN,
                  THREAD_CEILING_INPUT_MAX,
                  DEFAULT_SESSION_GUARD.critical_thread_count,
                ),
              )
            }
            disabled={!guardEnabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            A spawn is refused by default, always with an explicit override. Keep it{" "}
            <strong>above</strong> the warn ceiling — the inverse of the memory pair, because on a
            ceiling the lighter verdict is the lower number.
          </p>
          <EffectiveLimit
            uiBridgeId="settings.resource-guard-critical-threads-effective"
            configured={`${criticalThreads} threads`}
            effective={`${effectiveThreads.criticalThreads} threads`}
            clamped={criticalThreadsClamped}
            reason={
              criticalThreads > THREAD_CEILING_DEFAULT_CRITICAL
                ? `above the runner's built-in ${THREAD_CEILING_DEFAULT_CRITICAL}-thread block ceiling, which nothing can raise`
                : criticalThreads < THREAD_CEILING_FLOOR
                  ? `below the ${THREAD_CEILING_FLOOR}-thread clamp, under which a machine measured at 150-151 threads at rest could never start a session again`
                  : "below the effective warn ceiling, and the lighter verdict has to fire first — so it is raised up to it rather than blocking spawns that were never warned about"
            }
          />
        </div>

        {invertedCeilings && (
          <div
            data-ui-bridge-id="settings.resource-guard-inverted-ceilings"
            className={`p-3 ${getAccentColors("red").bg} rounded-lg flex gap-2`}
          >
            <TriangleAlert className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
            <p className={`text-xs ${getAccentColors("red").text}`}>
              The block ceiling ({criticalThreads} threads) is below the warn ceiling ({warnThreads}{" "}
              threads), so spawns would be blocked before they were ever warned about. The runner
              refuses this combination — raise the block ceiling or lower the warn ceiling. (Note
              the direction: on a ceiling the block limit is the HIGHER number, the opposite of the
              GiB floors above.)
            </p>
          </div>
        )}

        {invertedFloors && (
          <div
            data-ui-bridge-id="settings.resource-guard-inverted-floors"
            className={`p-3 ${getAccentColors("red").bg} rounded-lg flex gap-2`}
          >
            <TriangleAlert className={`w-4 h-4 ${getAccentColors("red").text} shrink-0 mt-0.5`} />
            <p className={`text-xs ${getAccentColors("red").text}`}>
              The block floor ({criticalGib} GiB) is above the warn floor ({warnGib} GiB), so spawns
              would be blocked before they were ever warned about. The runner refuses this
              combination — lower the block floor or raise the warn floor.
            </p>
          </div>
        )}

        {warnAboveCiNodeReject && !invertedFloors && (
          <div
            data-ui-bridge-id="settings.resource-guard-warn-above-ci-reject"
            className={`p-3 ${getAccentColors("amber").bg} rounded-lg flex gap-2`}
          >
            <TriangleAlert className={`w-4 h-4 ${getAccentColors("amber").text} shrink-0 mt-0.5`} />
            <p className={`text-xs ${getAccentColors("amber").text}`}>
              A warn floor above the CI-node reject floor ({CI_NODE_REJECT_FLOOR_GIB} GiB) inverts
              the ladder: you would be warned about your own spawns before this machine stops
              accepting build work. Allowed, but rarely what you want.
            </p>
          </div>
        )}

        <div className={`p-3 ${getAccentColors("blue").bg} rounded-lg flex gap-2`}>
          <Info className={`w-4 h-4 ${getAccentColors("blue").text} shrink-0 mt-0.5`} />
          <p className={`text-xs ${getAccentColors("blue").text}`}>
            All floors on this machine read the same quantity — Windows free <strong>commit</strong>
            , not free physical RAM — and differ by verdict: warn here, block here,{" "}
            <code>ci_node</code> rejects at {CI_NODE_REJECT_FLOOR_GIB} GiB, the supervisor and{" "}
            <code>cargo-guard.sh</code> defer at 5 GiB. The host reading already nets out WSL usage,
            because WSL hands idle pages back to Windows — so <code>vmmemWSL</code> pressure is
            visible here.
            <br />
            <br />
            What gets enforced is{" "}
            <code>
              max(this machine, your tenant&apos;s fleet floor, the built-in{" "}
              {SESSION_FLOOR_DEFAULT_WARN_GIB}/{SESSION_FLOOR_DEFAULT_CRITICAL_GIB} GiB defaults)
            </code>
            , capped at {SESSION_FLOOR_CAP_GIB} GiB and with the block floor never above the warn
            floor. You can only ever tighten. The &quot;Enforced&quot; lines above leave the fleet
            term out — it is cached in the background and can only raise these further, and showing
            an unknown as a zero is the one thing this guard must never do.
            <br />
            <br />
            The thread ceilings compose the same way with every step mirrored, because higher is
            worse there:{" "}
            <code>
              min(this machine, your tenant&apos;s fleet ceiling, the built-in{" "}
              {THREAD_CEILING_DEFAULT_WARN}/{THREAD_CEILING_DEFAULT_CRITICAL} defaults)
            </code>
            , clamped <strong>up</strong> at {THREAD_CEILING_FLOOR} threads and with the block
            ceiling never below the warn ceiling. Coord publishes no thread column yet, so that
            fleet term is dormant on every machine today and the fold is{" "}
            <code>min(this machine, the built-in defaults)</code>. The heavier of the two lanes
            decides; on a tie the memory lane is the one quoted in the message, and the other is
            still written to the log.
          </p>
        </div>
      </div>

      {/* ── CI-node admission ──────────────────────────────────────────── */}
      <div className="rounded-lg bg-card/50 p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h4 className="font-medium text-sm">Accept CI work on this machine</h4>
            <p className="text-xs text-muted-foreground">
              Advertises the <code>ci_node</code> capability and admits dispatched builds. Off by
              default — opting in is a deliberate act.
            </p>
          </div>
          <ToggleSwitch
            data-ui-bridge-id="settings.resource-guard-ci-node-enabled"
            checked={ciNode.enabled}
            onChange={(enabled) => setCiNode((c) => ({ ...c, enabled }))}
          />
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-max-builds">
            Max concurrent builds
          </label>
          <input
            id="resource-guard-max-builds"
            data-ui-bridge-id="settings.resource-guard-max-builds"
            type="number"
            min={MAX_CONCURRENT_BUILDS_MIN}
            max={MAX_CONCURRENT_BUILDS_MAX}
            step={1}
            value={ciNode.max_concurrent_builds}
            onChange={(e) =>
              setCiNode((c) => ({
                ...c,
                max_concurrent_builds: clampInt(
                  parseInt(e.target.value, 10),
                  MAX_CONCURRENT_BUILDS_MIN,
                  MAX_CONCURRENT_BUILDS_MAX,
                  DEFAULT_CI_NODE.max_concurrent_builds,
                ),
              }))
            }
            disabled={!ciNode.enabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            Also the build capacity this device advertises to coord. Range{" "}
            {MAX_CONCURRENT_BUILDS_MIN}-{MAX_CONCURRENT_BUILDS_MAX}.
          </p>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-min-free-disk">
            Minimum free disk (GiB)
          </label>
          <input
            id="resource-guard-min-free-disk"
            data-ui-bridge-id="settings.resource-guard-min-free-disk"
            type="number"
            min={DISK_FLOOR_MIN_GIB}
            max={DISK_FLOOR_MAX_GIB}
            step={1}
            value={ciNode.min_free_disk_gb}
            onChange={(e) =>
              setCiNode((c) => ({
                ...c,
                min_free_disk_gb: clampInt(
                  parseInt(e.target.value, 10),
                  DISK_FLOOR_MIN_GIB,
                  DISK_FLOOR_MAX_GIB,
                  DEFAULT_CI_NODE.min_free_disk_gb,
                ),
              }))
            }
            disabled={!ciNode.enabled}
            className="w-full px-2.5 py-1.5 text-sm bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            Free space required on the QONTINUI_ROOT volume to START a build. Cannot be 0 — a zero
            floor disables the guard rather than relaxing it.
          </p>
        </div>

        <div className="space-y-1.5">
          <label className="text-xs font-medium" htmlFor="resource-guard-repo-allowlist">
            Repo allowlist
          </label>
          <textarea
            id="resource-guard-repo-allowlist"
            data-ui-bridge-id="settings.resource-guard-repo-allowlist"
            rows={4}
            value={allowlistText}
            onChange={(e) => setAllowlistText(e.target.value)}
            disabled={!ciNode.enabled}
            placeholder={"qontinui/qontinui-runner\nqontinui/qontinui-web"}
            className="w-full px-2.5 py-1.5 text-sm font-mono bg-muted/50 rounded-md outline-hidden focus:ring-1 focus:ring-primary/50 disabled:opacity-50"
          />
          <p className="text-[10px] text-muted-foreground">
            One repo per line (or comma-separated). Either the coord <code>owner/name</code> slug or
            the bare repo basename. <strong>Empty means nothing is runnable</strong>, even when
            enabled, and <code>*</code> is refused — the same rule coord&apos;s remote configuration
            door enforces.
          </p>
        </div>
      </div>

      <div className="flex justify-end">
        <button
          type="button"
          data-ui-bridge-id="settings.resource-guard-save"
          onClick={saveSettings}
          disabled={saving || invertedFloors || invertedCeilings}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 text-sm"
        >
          {saving ? (
            <>
              <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
              Saving...
            </>
          ) : saveSuccess ? (
            <>
              <Check className="w-4 h-4" />
              Saved!
            </>
          ) : (
            <>
              <ShieldAlert className="w-4 h-4" />
              Save Resource Guard
            </>
          )}
        </button>
      </div>
    </div>
  );
}

// ── Configured vs. enforced ─────────────────────────────────────────────────
interface EffectiveLimitProps {
  uiBridgeId: string;
  /** The typed value, already formatted in its own unit ("2 GiB", "150 threads"). */
  configured: string;
  /** The value the runner will enforce, in the same unit. */
  effective: string;
  /** `true` when the runner will enforce something other than the typed value. */
  clamped: boolean;
  /** Why it was clamped — rendered only when it was. */
  reason: string;
}

/**
 * The limit the runner will ACTUALLY enforce, beside the one that was typed.
 *
 * The panel used to display only the configured value while enforcement was
 * `max(configured, hardcoded default)` — so the entire bottom of the input's
 * range was inert and nothing on the page said so. Showing the enforced number
 * always (not only when it differs) also makes the clamps visible: the
 * `SESSION_FLOOR_CAP_GIB` ceiling, the `THREAD_CEILING_FLOOR` clamp, and both
 * ladder coercions.
 *
 * Takes pre-formatted STRINGS rather than numbers and a unit flag, because the
 * two lanes disagree about direction as well as unit and a component that
 * appended " GiB" would have to grow a branch to stop calling 256 threads a
 * quantity of memory. The caller owns the phrasing; this owns the layout and the
 * clamped-vs-not styling.
 */
function EffectiveLimit({
  uiBridgeId,
  configured,
  effective,
  clamped,
  reason,
}: EffectiveLimitProps) {
  return (
    <p
      data-ui-bridge-id={uiBridgeId}
      className={`text-[10px] ${clamped ? getAccentColors("amber").text : "text-muted-foreground"}`}
    >
      Enforced: <strong>{effective}</strong>
      {clamped ? ` — your ${configured} is ${reason}.` : "."}
    </p>
  );
}

// ── Toggle (mirrors LockYieldPolicySettings.tsx pattern) ────────────────────
interface ToggleSwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  "data-ui-bridge-id"?: string;
}

function ToggleSwitch({
  checked,
  onChange,
  disabled,
  "data-ui-bridge-id": dataUiBridgeId,
}: ToggleSwitchProps) {
  return (
    <button
      type="button"
      data-ui-bridge-id={dataUiBridgeId}
      onClick={() => {
        if (!disabled) onChange(!checked);
      }}
      disabled={disabled}
      aria-pressed={checked}
      className={`relative inline-flex h-5 w-9 items-center rounded-full transition-colors ${
        checked ? "bg-primary" : "bg-muted"
      } ${disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
    >
      <span
        className={`inline-block h-3.5 w-3.5 transform rounded-full bg-white transition-transform ${
          checked ? "translate-x-4" : "translate-x-1"
        }`}
      />
    </button>
  );
}
