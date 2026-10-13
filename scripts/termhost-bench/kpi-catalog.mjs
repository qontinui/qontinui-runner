/**
 * termhost-bench KPI catalog (`termhost-bench/v1`) — the measurement contract as data.
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 1, "KPI
 * catalog". Every consumer reads the catalog from here rather than restating it:
 * `build-schema.mjs` generates `result-schema.json` from it, `stats.mjs` turns
 * samples into KPI values with its minimum sample counts, the Phase 3 orchestrator
 * fills one value per catalog id per results row, and the Phase 6 report renders it.
 *
 * Every KPI value in a results file has the shape
 *
 *     {state: "measured" | "unknown" | "not_applicable", unit, n, value | stats, reason}
 *
 * A KPI with fewer samples than its `min_n` is `unknown`, never 0, and an unmeasured
 * KPI carries a `reason` and no number at all.
 *
 * Row fields:
 *   id                  stable identifier (the results file's key)
 *   label               short human name
 *   description         what exactly is measured, including exclusions
 *   unit                the one unit every number of this KPI is in
 *   direction           lower_is_better | higher_is_better | neutral
 *   shape               "stats" (a `stats` object keyed by `statistics`) or "value" (one `value`)
 *   statistics          the statistics reported; for shape "value", what the value is
 *   sample_unit         what `n` counts
 *   min_n               below this `n` the KPI is unknown(insufficient_samples)
 *   instrument          how the number is obtained
 *   needs               capabilities the KPI depends on (see NEEDS)
 *   comparable          true when the instrument is identical for every setup (outside-in)
 *   attribution         host | workload | shared — whose resources the number describes
 *   derived_from        KPI ids this one is computed from
 *   degradation_of      for degradation_* rows: {kpi, statistic} whose N / N=1 ratio this is
 *   exclusion           "valid_intervals" when non-foreground / occluded / operator-input
 *                       intervals are excluded before counting
 *   headline_statistic  the statistic the operator's headline table shows, or null
 *   value_range         bounds every number of this KPI must satisfy
 *   integer             true when the value is a count
 */

/** The results-file schema identifier. */
export const SCHEMA_ID = "termhost-bench/v1";

/** Benchmark environments (D6). Numbers compare only within one environment. */
export const ENVIRONMENTS = Object.freeze(["windows-desktop", "windows-hosted", "linux-xvfb"]);

/** Setups (hosts) a row can describe. */
export const SETUP_IDS = Object.freeze([
  "runner",
  "vscode",
  "windows-terminal",
  "tmux",
  "tmux-wsl",
]);

/** Visibility arms (D4). */
export const ARMS = Object.freeze(["focus", "tiled"]);

/** The N ladder (KPI catalog, "The headline table"). */
export const N_LADDER = Object.freeze([1, 5, 15, 40]);

/** The operator's headline question: windows-desktop, N = 15, both arms. */
export const HEADLINE = Object.freeze({
  environment: "windows-desktop",
  n: 15,
  arms: Object.freeze(["focus", "tiled"]),
});

/** How the host under test was launched (D5, D12). */
export const LAUNCH_MODES = Object.freeze([
  "supervisor_spawn_test",
  "subject_instance_root",
  "portable_pinned",
  "installed_app_path",
  "private_server",
]);

/** The ConPTY host a Windows setup actually runs, or a Unix pty (catalog diagnostics note). */
export const PTY_HOST_IMPLS = Object.freeze(["system_conhost", "bundled_openconsole", "unix_pty"]);

/** Where a profile's numbers come from (D3). */
export const PROFILE_PROVENANCE = Object.freeze(["calibration", "assumption", "synthetic"]);

/** Capability doctor verdicts — the vocabulary of qontinui-claude-config `capability-doctor.sh`. */
export const DOCTOR_STATES = Object.freeze([
  "OPERATIVE",
  "DEGRADED",
  "INOPERATIVE-ON-THIS-MACHINE",
  "UNKNOWN",
]);

/** Capabilities a KPI can depend on. */
export const NEEDS = Object.freeze({
  capture: "an operative screen-capture source for the measured window (D2)",
  safe_injection: "the D9 input-injection safeguard established for this run",
  gpu_counters: "per-process GPU engine counters (Windows with a WDDM 2.0+ GPU)",
  baseline_level: "a measured N = 1 row for the same setup, variant, build role and arm",
  n_ladder: "measured rows at three or more N levels",
  refresh_rate: "a known display refresh rate (intervals are counted in refresh intervals)",
});

/**
 * Why a value is `unknown` or `not_applicable`. A results file may use only these
 * codes; a new reason is added here (the schema is regenerated from this list).
 */
export const REASON_CODES = Object.freeze({
  insufficient_samples: "fewer samples than the KPI's minimum n",
  degenerate_input: "the samples cannot define the statistic (e.g. every x equal for a slope)",
  capture_unavailable: "no operative capture source for this environment",
  capture_disagreement:
    "WGC and DDA disagreed on marker first-appearance by more than one refresh interval (D2)",
  refresh_rate_unknown:
    "the display refresh rate is unknown, so nothing measured in refresh intervals can be counted",
  clock_unaligned: "the generator and probe clocks could not be shown to share a timebase",
  marker_unreadable: "the setup's calibration marker was unreadable or altered",
  sessions_not_rendered: "the probe did not see the expected number of sessions' markers",
  occluded: "too few valid intervals: the window was not foreground, was cloaked or occluded",
  operator_activity: "too few valid intervals: real operator input occurred",
  shared_process: "the host process tree could not be proven exclusive to the run (D5)",
  input_unsafe: "the D9 input safeguard could not be established; nothing was typed",
  zone_unresolved: "the runner focus arm could not resolve the session's zone",
  pane_not_mounted: "the focused pane refused keyboard focus (TERMINAL_NO_MOUNTED_VIEW)",
  pane_stalled:
    "a displayed session's output marker stayed still for >= 2 s seen while its generator kept writing, and did not advance in any later frame of the soak; `detail` says when it was last shown",
  device_drained: "the device is drained, or its drain state is unknown",
  admission_refused: "the runner's spawn-time resource guard refused sessions at this N",
  host_crashed: "a host process exited during the run",
  gpu_counters_unavailable: "no per-process GPU engine counters on this machine",
  no_baseline_level:
    "the ratio needs a measured N = 1 value for the same setup, variant, build role and arm",
  baseline_level: "this is the N = 1 row; a ratio against itself carries no information",
  zero_baseline: "the N = 1 value is 0, so the ratio is undefined",
  too_few_levels: "fewer N levels than the slope needs",
  arm_not_realisable: "the setup cannot realise this arm (D4)",
  not_applicable_to_arm:
    "the KPI has no referent in this arm (showing one hidden session is a focus-arm action; the tiled arm shows them together)",
  single_session: "N = 1: there is no other session to switch to",
  not_applicable_to_setup:
    "the KPI does not describe this setup (e.g. a WSL VM on a non-WSL setup)",
  not_applicable_to_environment: "the KPI does not exist in this environment",
  not_instrumented: "the instrument for this KPI is not built yet",
  harness_error: "the harness failed while measuring; `detail` says how",
});

/** Row-level labels a results row may carry. */
export const ROW_LABELS = Object.freeze({
  dom_renderer_fallback:
    "VS Code's terminal fell back from WebGL to the DOM renderer (software rendering, D6)",
  software_rendered: "the environment renders on the CPU (llvmpipe / no GPU)",
  unknown_build: "the runner build could not be proven; never used as a baseline (D12)",
  assert_only_smoke:
    "a third-party adapter smoke in public CI: asserted in the job, never uploaded (D7)",
  pane_smaller_than_profile: "a pane was smaller than the profile geometry, so frames wrapped",
});

/** Incident kinds. An incident with `stops_run: true` ended the run. */
export const INCIDENT_KINDS = Object.freeze({
  primary_state_changed:
    "a primary-runner state digest (settings.json, paired_user.json, auth_tokens.enc) changed (D12)",
  coord_session_left_open:
    "a terminal the run created was still open in coord after teardown (D12)",
  generator_survived_teardown: "a generator carrying the run id was alive after teardown",
  admission_refused: "the runner's resource guard refused spawns; the setup's ladder ended",
  capture_lost: "the capture source was lost mid-run (e.g. DXGI ACCESS_LOST on a desktop switch)",
  operator_input: "real operator input was detected and the input arm aborted (D9)",
  host_crashed: "a host process exited during the run",
  pane_stalled:
    "a displayed session's output marker stayed still for >= 2 s while its generator kept writing, in a soak or the focus arm's restore check; `detail` says whether the pane recovered (a hitch: the marker KPIs stay measured) or did not advance in any later frame (a wedge), and when it was last shown",
  harness_error: "an internal harness failure",
});

const PCT_RANGE = Object.freeze({ minimum: 0, maximum: 100 });
const NON_NEGATIVE = Object.freeze({ minimum: 0 });
const UNBOUNDED = Object.freeze({});
const CORE_PCT = "% of one logical core";

/**
 * Fill the optional fields so every row carries every field: consumers never
 * default a missing one.
 */
function normalizeRow(row) {
  return {
    derived_from: [],
    degradation_of: null,
    exclusion: null,
    integer: false,
    ...row,
  };
}

/** The catalog. Order is the presentation order of a results table. */
export const KPI_CATALOG = deepFreeze(
  [
    {
      id: "output_latency_ms",
      label: "Output-to-frame latency",
      description:
        "Per visible pane: time from the generator emitting a marker (t_emit, taken just before " +
        "the write) to the first captured frame carrying that marker.",
      unit: "ms",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["p50", "p95", "p99"],
      sample_unit: "marker first-appearance on a visible pane",
      min_n: 100,
      instrument: "generator t_emit -> first captured frame carrying that marker",
      needs: ["capture"],
      comparable: true,
      attribution: "host",
      headline_statistic: "p95",
      value_range: NON_NEGATIVE,
    },
    {
      id: "input_latency_ms",
      label: "Keystroke-to-frame echo latency",
      description:
        "In the focused pane while the others stream: time from the probe injecting a key " +
        "(t_inject) to the first captured frame carrying the generator's echo marker for it.",
      unit: "ms",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["p50", "p95", "p99"],
      sample_unit: "injected keystroke whose echo marker was observed",
      min_n: 100,
      instrument: "probe t_inject -> first frame carrying the echo marker",
      needs: ["capture", "safe_injection"],
      comparable: true,
      attribution: "host",
      headline_statistic: "p95",
      value_range: NON_NEGATIVE,
    },
    {
      id: "frame_interval_ms",
      label: "Presented-frame interval",
      description:
        "Cadence of the host window's presented updates while streaming: the interval between " +
        "consecutive captured frames of the measured window.",
      unit: "ms",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["p50", "p95"],
      sample_unit: "interval between consecutive captured frames",
      min_n: 100,
      instrument: "capture frame times",
      needs: ["capture"],
      comparable: true,
      attribution: "host",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "jank_share",
      label: "Jank share",
      description: "Share of frame intervals longer than twice the display's refresh interval.",
      unit: "%",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["share"],
      sample_unit: "interval between consecutive captured frames",
      min_n: 100,
      instrument: "capture frame times",
      needs: ["capture", "refresh_rate"],
      comparable: true,
      attribution: "host",
      derived_from: ["frame_interval_ms"],
      headline_statistic: null,
      value_range: PCT_RANGE,
    },
    {
      id: "switch_latency_ms",
      label: "Session-switch latency",
      description:
        "Time from the setup's own automation path being told to show session k to k's newest " +
        "marker becoming visible.",
      unit: "ms",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["p50", "p95"],
      sample_unit: "switch",
      min_n: 20,
      instrument: "the setup's own automation path + capture",
      needs: ["capture"],
      comparable: true,
      attribution: "host",
      headline_statistic: "p95",
      value_range: NON_NEGATIVE,
    },
    {
      id: "spawn_latency_ms",
      label: "Session-spawn latency",
      description:
        "Time from 'new session' through the setup's own automation path to that session's first " +
        "marker becoming visible; includes the session shell's start-up (D13).",
      unit: "ms",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["p50", "p95"],
      sample_unit: "spawned session",
      min_n: 20,
      instrument: "the setup's own automation path + capture",
      needs: ["capture"],
      comparable: true,
      attribution: "host",
      headline_statistic: "p95",
      value_range: NON_NEGATIVE,
    },
    {
      id: "cpu_pct",
      label: "Host CPU",
      description:
        "Host process-tree CPU, excluding every generator AND its parent session shell (identified " +
        "from the slot file's PID -> parent PID; reported separately as workload_cpu_pct). The PTY " +
        "host (conhost.exe / OpenConsole.exe) stays in the host tree, because the host chooses it.",
      unit: CORE_PCT,
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["mean", "p95"],
      sample_unit: "500 ms CPU sample",
      min_n: 20,
      instrument: "GetProcessTimes / /proc/<pid>/stat",
      needs: [],
      comparable: true,
      attribution: "host",
      headline_statistic: "mean",
      value_range: NON_NEGATIVE,
    },
    {
      id: "cpu_ms_per_mb",
      label: "Host CPU per MB delivered",
      description:
        "Host CPU time over the soak divided by the MB of PTY output the generators wrote in it.",
      unit: "ms/MB",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["value"],
      sample_unit: "500 ms CPU sample",
      min_n: 20,
      instrument: "host CPU time / MB the generators wrote",
      needs: [],
      comparable: true,
      attribution: "host",
      derived_from: ["cpu_pct"],
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "memory_mb",
      label: "Host memory at end of soak",
      description:
        "Host-tree private bytes (Windows) / PSS (Linux) at the end of the soak, with the same " +
        "generator-and-session-shell exclusion as cpu_pct (the excluded workload is " +
        "workload_memory_mb).",
      unit: "MB",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["value"],
      sample_unit: "memory sample",
      min_n: 1,
      instrument: "GetProcessMemoryInfo PrivateUsage / smaps_rollup",
      needs: [],
      comparable: true,
      attribution: "host",
      headline_statistic: "value",
      value_range: NON_NEGATIVE,
    },
    {
      id: "memory_growth_mb_per_min",
      label: "Host memory growth",
      description: "Least-squares slope of the host-tree memory samples over the soak.",
      unit: "MB/min",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["slope"],
      sample_unit: "memory sample",
      min_n: 20,
      instrument: "GetProcessMemoryInfo PrivateUsage / smaps_rollup",
      needs: [],
      comparable: true,
      attribution: "host",
      derived_from: ["memory_mb"],
      headline_statistic: null,
      value_range: UNBOUNDED,
    },
    {
      id: "memory_mb_per_session",
      label: "Marginal host memory per session",
      description:
        "Least-squares slope of memory_mb over the N ladder (session shells excluded), for one " +
        "setup, variant and arm.",
      unit: "MB/session",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["slope"],
      sample_unit: "N level with a measured memory_mb",
      min_n: 3,
      instrument: "derived",
      needs: ["n_ladder"],
      comparable: true,
      attribution: "host",
      derived_from: ["memory_mb"],
      headline_statistic: null,
      value_range: UNBOUNDED,
    },
    {
      id: "gpu_pct",
      label: "Host GPU",
      description: "GPU engine utilisation summed over the host's PIDs.",
      unit: "%",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["mean", "p95"],
      sample_unit: "500 ms GPU sample",
      min_n: 20,
      instrument:
        "\\GPU Engine(pid_*)\\Utilization Percentage (instances enumerated on the machine)",
      needs: ["gpu_counters"],
      comparable: true,
      attribution: "host",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "producer_stall_pct",
      label: "Producer stall",
      description:
        "Share of generator wall time spent blocked inside write: the mean across sessions, and " +
        "the worst session.",
      unit: "%",
      direction: "lower_is_better",
      shape: "stats",
      statistics: ["mean", "max"],
      sample_unit: "session with progress records",
      min_n: 1,
      instrument: "generator self-timing (fs.writeSync on fd 1)",
      needs: [],
      comparable: true,
      attribution: "host",
      headline_statistic: "mean",
      value_range: PCT_RANGE,
    },
    {
      id: "delivered_ratio",
      label: "Delivered ratio",
      description:
        "Bytes a generator wrote divided by the bytes its profile scheduled over the soak; the " +
        "minimum across sessions. A generator catching up on a backlog inside the window can " +
        "exceed 1.",
      unit: "ratio",
      direction: "higher_is_better",
      shape: "value",
      statistics: ["min"],
      sample_unit: "session with progress records",
      min_n: 1,
      instrument: "generator",
      needs: [],
      comparable: true,
      attribution: "host",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "hangs",
      label: "Hangs",
      description:
        "Intervals of >= 2 s with markers pending and no window update, plus OS 'not responding'. " +
        "Intervals in which the window was not foreground, was cloaked or occluded, or real operator " +
        "input occurred are excluded; a run left with too few valid intervals is unknown(occluded) " +
        "or unknown(operator_activity) instead of a count.",
      unit: "count",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["count"],
      sample_unit: "valid 500 ms observation interval",
      min_n: 60,
      instrument: "capture + IsHungAppWindow",
      needs: ["capture"],
      comparable: true,
      attribution: "host",
      exclusion: "valid_intervals",
      headline_statistic: "count",
      value_range: NON_NEGATIVE,
      integer: true,
    },
    {
      id: "crashes",
      label: "Host crashes",
      description: "Host processes that exited during the run.",
      unit: "count",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["count"],
      sample_unit: "host process monitored",
      min_n: 1,
      instrument: "probe",
      needs: [],
      comparable: true,
      attribution: "host",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
      integer: true,
    },
    {
      id: "marker_loss",
      label: "Marker loss",
      description:
        "Visible-pane marker sequences that were current in the generator's stream for >= 2 refresh " +
        "intervals but were never observed, or were observed out of order. A sequence superseded " +
        "before it was composited is marker_skipped (coalescing), not loss. Same interval exclusion " +
        "as hangs.",
      unit: "%",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["share"],
      sample_unit: "eligible visible-pane marker sequence",
      min_n: 100,
      instrument: "capture",
      needs: ["capture", "refresh_rate"],
      comparable: true,
      attribution: "host",
      exclusion: "valid_intervals",
      headline_statistic: null,
      value_range: PCT_RANGE,
    },
    {
      id: "marker_skipped",
      label: "Marker coalescing",
      description:
        "Visible-pane marker sequences superseded by the next sequence before any composited frame " +
        "carried them (current for < 2 refresh intervals and never observed): the host coalesced " +
        "frames, which is not loss. Same interval exclusion as hangs.",
      unit: "%",
      direction: "neutral",
      shape: "value",
      statistics: ["share"],
      sample_unit: "visible-pane marker sequence emitted",
      min_n: 100,
      instrument: "capture",
      needs: ["capture", "refresh_rate"],
      comparable: true,
      attribution: "host",
      exclusion: "valid_intervals",
      headline_statistic: null,
      value_range: PCT_RANGE,
    },
    {
      id: "degradation_output_latency_ms",
      label: "Output-latency degradation",
      description: "output_latency_ms p95 at this N divided by the same setup's p95 at N = 1.",
      unit: "ratio",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["ratio"],
      sample_unit: "measured operand (the value at N and at N = 1)",
      min_n: 2,
      instrument: "derived",
      needs: ["baseline_level"],
      comparable: true,
      attribution: "host",
      derived_from: ["output_latency_ms"],
      degradation_of: { kpi: "output_latency_ms", statistic: "p95" },
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "degradation_input_latency_ms",
      label: "Input-latency degradation",
      description: "input_latency_ms p95 at this N divided by the same setup's p95 at N = 1.",
      unit: "ratio",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["ratio"],
      sample_unit: "measured operand (the value at N and at N = 1)",
      min_n: 2,
      instrument: "derived",
      needs: ["baseline_level"],
      comparable: true,
      attribution: "host",
      derived_from: ["input_latency_ms"],
      degradation_of: { kpi: "input_latency_ms", statistic: "p95" },
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "degradation_cpu_pct",
      label: "Host-CPU degradation",
      description: "cpu_pct mean at this N divided by the same setup's mean at N = 1.",
      unit: "ratio",
      direction: "lower_is_better",
      shape: "value",
      statistics: ["ratio"],
      sample_unit: "measured operand (the value at N and at N = 1)",
      min_n: 2,
      instrument: "derived",
      needs: ["baseline_level"],
      comparable: true,
      attribution: "host",
      derived_from: ["cpu_pct"],
      degradation_of: { kpi: "cpu_pct", statistic: "mean" },
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "workload_cpu_pct",
      label: "Workload CPU (generators + session shells)",
      description:
        "CPU of every generator and its parent session shell — the workload cpu_pct excludes. " +
        "Identical across setups by construction unless a host throttles its producers.",
      unit: CORE_PCT,
      direction: "neutral",
      shape: "stats",
      statistics: ["mean", "p95"],
      sample_unit: "500 ms CPU sample",
      min_n: 20,
      instrument: "GetProcessTimes / /proc/<pid>/stat over slot-file PIDs and their parents",
      needs: [],
      comparable: true,
      attribution: "workload",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "workload_memory_mb",
      label: "Workload memory (generators + session shells)",
      description:
        "Private bytes (Windows) / PSS (Linux) of every generator and its parent session shell at " +
        "the end of the soak — the workload memory_mb excludes.",
      unit: "MB",
      direction: "neutral",
      shape: "value",
      statistics: ["value"],
      sample_unit: "memory sample",
      min_n: 1,
      instrument:
        "GetProcessMemoryInfo PrivateUsage / smaps_rollup over slot-file PIDs and parents",
      needs: [],
      comparable: true,
      attribution: "workload",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "compositor_cpu_pct",
      label: "Compositor CPU (attributed to no setup)",
      description:
        "CPU of the machine's compositor — dwm.exe on Windows, the X server on linux-xvfb. Shared " +
        "by everything on the machine, so it is recorded separately and attributed to no setup.",
      unit: CORE_PCT,
      direction: "neutral",
      shape: "stats",
      statistics: ["mean", "p95"],
      sample_unit: "500 ms CPU sample",
      min_n: 20,
      instrument: "GetProcessTimes / /proc/<pid>/stat",
      needs: [],
      comparable: false,
      attribution: "shared",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "compositor_memory_mb",
      label: "Compositor memory (attributed to no setup)",
      description: "Private bytes / PSS of the machine's compositor at the end of the soak.",
      unit: "MB",
      direction: "neutral",
      shape: "value",
      statistics: ["value"],
      sample_unit: "memory sample",
      min_n: 1,
      instrument: "GetProcessMemoryInfo PrivateUsage / smaps_rollup",
      needs: [],
      comparable: false,
      attribution: "shared",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "wsl_vm_cpu_pct",
      label: "WSL VM CPU (tmux-wsl; attributed to no setup)",
      description:
        "CPU of the shared WSL VM process for the tmux-wsl setup; not_applicable to every other " +
        "setup. Shared by everything in WSL, so it is attributed to no setup.",
      unit: CORE_PCT,
      direction: "neutral",
      shape: "stats",
      statistics: ["mean", "p95"],
      sample_unit: "500 ms CPU sample",
      min_n: 20,
      instrument: "GetProcessTimes on the WSL VM process",
      needs: [],
      comparable: false,
      attribution: "shared",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
    {
      id: "wsl_vm_memory_mb",
      label: "WSL VM memory (tmux-wsl; attributed to no setup)",
      description: "Memory of the shared WSL VM process at the end of the soak; tmux-wsl only.",
      unit: "MB",
      direction: "neutral",
      shape: "value",
      statistics: ["value"],
      sample_unit: "memory sample",
      min_n: 1,
      instrument: "GetProcessMemoryInfo on the WSL VM process",
      needs: [],
      comparable: false,
      attribution: "shared",
      headline_statistic: null,
      value_range: NON_NEGATIVE,
    },
  ].map(normalizeRow),
);

/** Ids in catalog order. */
export const KPI_IDS = Object.freeze(KPI_CATALOG.map((k) => k.id));

const BY_ID = new Map(KPI_CATALOG.map((k) => [k.id, k]));

/**
 * The catalog row for `id`.
 *
 * @param {string} id
 * @returns {object}
 */
export function getKpi(id) {
  const row = BY_ID.get(id);
  if (!row) throw new RangeError(`unknown KPI id: ${id}`);
  return row;
}

/**
 * The smallest n at which a nearest-rank p-th percentile is not forced to be the
 * maximum sample: ceil(100 / (100 - p)). p50 -> 2, p95 -> 20, p99 -> 100. Below it a
 * "p99" is just the max under another name, so reporting it would mislabel a number.
 *
 * @param {number} p percentile in (0, 100]
 * @returns {number}
 */
export function percentileFloor(p) {
  const { num, den } = percentParts(p);
  const whole = 100 * den;
  if (num === whole) return 1;
  const gap = whole - num;
  return Math.floor((whole + gap - 1) / gap);
}

/**
 * A percentile as an exact fraction num / den (den a power of ten), so rank
 * arithmetic stays in integers: in floating point, 100 / (100 - 99.9) is 1000.00000000006.
 *
 * @param {number} p in (0, 100], at most 6 decimals
 * @returns {{num: number, den: number}}
 */
export function percentParts(p) {
  if (!(typeof p === "number" && p > 0 && p <= 100))
    throw new RangeError(`percentile out of range: ${p}`);
  const text = String(p);
  const dot = text.indexOf(".");
  const decimals = dot < 0 ? 0 : text.length - dot - 1;
  if (/e/i.test(text) || decimals > 6)
    throw new RangeError(`percentile ${p} has more than 6 decimals`);
  const den = 10 ** decimals;
  return { num: Math.round(p * den), den };
}

/**
 * The minimum samples a statistic needs before it means what its name says.
 *
 * @param {string} statistic
 * @returns {number}
 */
export function minSamplesForStatistic(statistic) {
  const m = /^p(\d+(?:\.\d+)?)$/.exec(statistic);
  if (m) return percentileFloor(Number(m[1]));
  switch (statistic) {
    case "slope":
      return 3;
    case "ratio":
      return 2;
    case "mean":
    case "max":
    case "min":
    case "value":
    case "share":
    case "count":
      return 1;
    default:
      throw new RangeError(`unknown statistic: ${statistic}`);
  }
}

function deepFreeze(value) {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const v of Object.values(value)) deepFreeze(v);
  }
  return value;
}
