import {
  useEffect,
  useRef,
  useState,
  useCallback,
  forwardRef,
  memo,
  useImperativeHandle,
} from "react";
import { FilePathLinkProvider } from "./FilePathLinkProvider";
import { invoke } from "@tauri-apps/api/core";
import { useUIBridgeOptional } from "@qontinui/ui-bridge";
import { createTerminalBackend } from "./backends";
import type { BackendType, ITerminalBackend, TerminalSearchResults } from "./backends";
import { TerminalFindBar } from "./TerminalFindBar";
import { paintGrid, type GridSnapshot } from "./paintGrid";
import { getTerminalDebug, recordPaintGrid } from "./terminalDebug";
import { instanceStorage } from "@/lib/instance-storage";
import { writeClipboard } from "@/lib/clipboard";
import { consumeInputChunk } from "./consumeInputChunk";
import { preparePasteData } from "./preparePaste";
import { wheelToLineDelta, DEFAULT_CELL_HEIGHT_PX } from "./wheelScroll";
import { matchScrollShortcut } from "./scrollKeys";
import {
  trimReplayedChunk,
  isEmissionGap,
  resyncSliceStart,
  drainHeldChunks,
  lostWindowBytes,
  formatLostOutputMarker,
  RESYNC_INCOMPLETE_MARKER,
  type OffsetChunk,
} from "./scrollbackReplay";
import { RenderAckAccumulator, ACK_FLOOR_INTERVAL_MS } from "./flowControl";
import { useWindowAssignments } from "./contexts/WindowAssignmentsContext";
import {
  registerTerminalOutputHandler,
  registerTerminalExitHandler,
  type TerminalOutputPayload,
} from "./terminalEventDemux";
import { declarePaneTier } from "./terminalVisibilityTiers";
import { PaneVisibilityService, routeOutputChunk } from "./paneVisibilityService";
import { setWebglSlotVisible } from "./backends/webglContextLru";

export interface TerminalInstanceHandle {
  getSelection: () => string;
  hasSelection: () => boolean;
  writeToTerminal: (text: string) => void;
  /**
   * Write raw output data directly to the terminal display without sending to the PTY.
   * Used for replaying saved scrollback buffers on session restore.
   */
  writeToDisplay: (data: string) => void;
  /** Read up to `maxLines` lines from the terminal scrollback buffer. */
  getScrollback: (maxLines?: number) => string;
  /** Scroll the terminal viewport to the very bottom. */
  scrollToBottom: () => void;
}

export type ShellIntegrationEvent =
  | { type: "prompt_start" }
  | { type: "command_ready" }
  | { type: "command_execute" }
  | { type: "command_done"; exitCode: number }
  | { type: "command_line"; command: string }
  | { type: "cwd"; path: string };

interface TerminalInstanceProps {
  terminalId: string;
  visible: boolean;
  /**
   * The terminal page this instance belongs to. Feeds the stdin ownership
   * gate: a page-bound pop-out window claims stdin for ALL of its page's tabs,
   * including the ones that carry no `session_owner` entry (e.g. every tab
   * after a restart, where the owner map is deliberately cleared).
   *
   * REQUIRED on purpose. A mount site that forgets it renders a terminal that
   * is permanently stdin-dead in a page-bound pop-out — and nothing goes red,
   * which is exactly the bug this prop exists to fix. Keeping it required puts
   * that guarantee in `tsc` instead of in a test that scans source text.
   */
  pageId: string;
  /** Which terminal backend to use. Defaults to "xterm". */
  backendType?: BackendType;
  /** True when this instance is reconnecting to an existing Rust PTY session. */
  isReconnecting?: boolean;
  /** Called after scrollback buffer has been replayed and live events are flowing. */
  onReconnected?: () => void;
  onExit?: (exitCode: number | null) => void;
  onSelectionChange?: (hasSelection: boolean) => void;
  onFirstInput?: (input: string) => void;
  /**
   * Called for EVERY non-empty newline-terminated input line typed into
   * the terminal. Unlike `onFirstInput`, this is ungated and fires on
   * every subsequent line — the consumer (e.g. `useMidSessionProbe`)
   * applies its own debouncing / rate-limiting / gating.
   */
  onUserInputLine?: (input: string) => void;
  /** Called when the shell emits an OSC 633 shell integration event. */
  onShellIntegration?: (event: ShellIntegrationEvent) => void;
  /**
   * Called with the latest OSC 0 / OSC 2 title observed by the Rust grid.
   * Fires from the bootstrap paint and from the ~1s ack-timer title poll
   * whenever the grid's `title` differs from the previous value. (The poll
   * replaced the retired periodic idle paint as the title source — Phase 2 of
   * `plans/2026-06-13-terminal-rendering-robustness.md`.)
   */
  onTitleChange?: (title: string) => void;
}

// `TerminalOutputEvent` and `TerminalExitEvent` are imported from
// `@qontinui/shared-types/tauri-events` — generated from the canonical Rust
// structs in `qontinui-schemas/rust/src/terminal.rs`. Future serde renames
// will break this file at compile time instead of silently dropping events.

/** Encode a Uint8Array to base64 without stack overflow on large buffers. */
function uint8ToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 8192;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    const slice = bytes.subarray(i, Math.min(i + CHUNK, bytes.length));
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

const encoder = new TextEncoder();

/** Default terminal options shared by all backends. */
const TERMINAL_OPTIONS = {
  cursorBlink: true,
  cursorStyle: "block" as const,
  fontSize: 14,
  fontFamily:
    "'Cascadia Code', 'Fira Code', 'JetBrains Mono', Menlo, Monaco, 'Courier New', monospace",
  lineHeight: 1.2,
  scrollback: 10000,
  // VS Code parity: Alt+click moves the shell cursor to the clicked column
  // (synthesized arrow keys; `terminal.integrated.altClickMovesCursor` is ON
  // by default in VS Code), and double-click word selection uses VS Code's
  // separator set (adds backtick, box-drawing dash, smart quotes, pipe to
  // xterm's default).
  altClickMovesCursor: true,
  wordSeparator: " ()[]{}',\"`─‘’“”|",
  theme: {
    background: "#1a1b26",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    selectionBackground: "#33467c",
    selectionForeground: "#c0caf5",
    black: "#15161e",
    red: "#f7768e",
    green: "#9ece6a",
    yellow: "#e0af68",
    blue: "#7aa2f7",
    magenta: "#bb9af7",
    cyan: "#7dcfff",
    white: "#a9b1d6",
    brightBlack: "#414868",
    brightRed: "#f7768e",
    brightGreen: "#9ece6a",
    brightYellow: "#e0af68",
    brightBlue: "#7aa2f7",
    brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff",
    brightWhite: "#c0caf5",
  },
};

/**
 * Scrollback cap applied once a terminal's process has exited.
 *
 * A dead pane never produces more output, but it stays mounted (often
 * indefinitely — see `useTerminalManager.closeTerminal`, the only path that
 * disposes the backend) so the operator can still read its result. At the live
 * `scrollback: 10000` cap each xterm buffer retains ~20MB of cell storage, and
 * under a heavy multi-session workload these finished panes accumulate until
 * the WebView2 renderer hits its memory ceiling and crashes with
 * "Error code: out of memory". Trimming the buffer to its tail on exit releases
 * the bulk of that storage while keeping the recent output readable (the full
 * scrollback is persisted Rust-side for session restore regardless).
 */
const DEAD_TERMINAL_SCROLLBACK = 2000;

/**
 * Title-poll cadence for a pane in the hidden ("quiet") visibility tier.
 *
 * Half the rate of the ~1 Hz poll a visible pane rides on its ack timer: the
 * operator cannot read a hidden pane's output, but its tab title still labels
 * it in the tab strip / unzoned chips / compact zone cards, so the title must
 * stay fresh. Two seconds matches the freshness bar the plan sets for the
 * other mount-independent signals (state chips, sparklines).
 */
const QUIET_TITLE_POLL_INTERVAL_MS = 2000;

/**
 * Ring-refetch attempts one `resyncFromRing` pass makes before giving up and
 * marking the pane spliced. Each attempt costs one `terminal_get_scrollback`
 * IPC; the loop only re-runs when the previous window fell short (a hole the
 * fetch did not cover, or bytes dropped while it was in flight), so the common
 * case is exactly one.
 */
const RESYNC_MAX_ATTEMPTS = 5;

/**
 * `memo(forwardRef(...))` (plan `2026-07-28-runner-many-sessions-performance`
 * Phase 1). The xterm host is the single most expensive node in the zone tree
 * and it re-rendered on every parent render — a state-duration tick, a
 * sparkline update, any context churn. Its props are stabilized at the ZoneGrid
 * call site (`instanceHandlers`), so the memo genuinely holds and the only
 * re-renders left are real prop changes (visibility, reconnect state).
 */
const TerminalInstanceInner = forwardRef<TerminalInstanceHandle, TerminalInstanceProps>(
  function TerminalInstanceRender(
    {
      terminalId,
      visible,
      pageId,
      backendType: backendTypeProp,
      isReconnecting,
      onReconnected,
      onExit,
      onSelectionChange,
      onFirstInput,
      onUserInputLine,
      onShellIntegration,
      onTitleChange,
    },
    ref,
  ) {
    const uiBridge = useUIBridgeOptional();
    // Resolve backend type: explicit prop > instanceStorage > "xterm"
    const backendType: BackendType =
      backendTypeProp ??
      ((instanceStorage.getItem("terminal-backend") as BackendType | null) || "xterm");
    // Resolve GPU acceleration policy (xterm only): instanceStorage > "auto".
    // "dom" forces the pure-DOM renderer (never ghosts) — user escape hatch
    // and the force-DOM diagnostic lever for the ghosting investigation.
    const gpuAcceleration: "auto" | "dom" =
      instanceStorage.getItem("terminal-gpu-acceleration") === "dom" ? "dom" : "auto";
    const containerRef = useRef<HTMLDivElement>(null);
    const backendRef = useRef<ITerminalBackend | null>(null);
    const bytesReceivedRef = useRef(0);
    // Stable refs for callbacks to avoid effect re-runs
    const onExitRef = useRef(onExit);
    onExitRef.current = onExit;
    const onSelectionChangeRef = useRef(onSelectionChange);
    onSelectionChangeRef.current = onSelectionChange;
    const onFirstInputRef = useRef(onFirstInput);
    onFirstInputRef.current = onFirstInput;
    const onUserInputLineRef = useRef(onUserInputLine);
    onUserInputLineRef.current = onUserInputLine;
    const onReconnectedRef = useRef(onReconnected);
    onReconnectedRef.current = onReconnected;
    const onShellIntegrationRef = useRef(onShellIntegration);
    onShellIntegrationRef.current = onShellIntegration;
    const onTitleChangeRef = useRef(onTitleChange);
    onTitleChangeRef.current = onTitleChange;
    // Phase 1 (pop-out windows): owner-gated stdin. Only the window that owns
    // this session forwards user input to the PTY; during a transient
    // double-mount (a tab moving between windows) this prevents double-SEND
    // (output still double-renders identically — benign). Kept in a ref so the
    // long-lived onData/onBinary closures read the freshest value. Defaults to
    // owned (true) in the single-window / no-provider case.
    //
    // `pageId` is passed so the gate asks the SAME question the renderer asks:
    // a page-bound pop-out owns every tab of its page, including tabs with no
    // `session_owner` entry. Without it those tabs render but never type.
    const { isOwned } = useWindowAssignments();
    const isOwnedRef = useRef(true);
    isOwnedRef.current = isOwned(terminalId, pageId);
    /**
     * Most-recently reported title — guards `onTitleChange` against firing on
     * every title poll (~1s ack-timer cadence) when the title is unchanged.
     */
    const lastTitleRef = useRef<string | null>(null);
    const firstInputReportedRef = useRef(false);
    const inputAccumulatorRef = useRef("");
    // Fractional carry for the Shift+wheel local-scroll override — sub-line
    // pixel deltas (trackpads) accumulate here so slow scrolling advances
    // smoothly instead of snapping a whole line per tick. See `wheelScroll.ts`.
    const wheelScrollAccumRef = useRef(0);
    /**
     * Visibility tier for this pane (Phase 2 / A4). Owned by the init effect
     * (it drives that effect's timers, observer and output routing) and read
     * by the `visible`-prop effect below, which must NOT re-run the init
     * effect — remounting a terminal on every maximize/compact toggle would
     * cost far more than the throttling saves.
     */
    const paneServiceRef = useRef<PaneVisibilityService | null>(null);
    const visibleRef = useRef(visible);
    visibleRef.current = visible;

    // ── Find-in-terminal (VS Code Ctrl+F parity) ─────────────────────────
    // State drives the TerminalFindBar overlay; the refs mirror it so the
    // backend key handler (created once in the init effect) and runFind can
    // read fresh values without re-running the effect.
    const [findOpen, setFindOpen] = useState(false);
    // Bumped on every Ctrl+F so a re-press while already open remounts the
    // bar (key prop), refocusing + selecting the query like VS Code.
    const [findFocusSeq, setFindFocusSeq] = useState(0);
    const [findQuery, setFindQuery] = useState("");
    const [findOpts, setFindOpts] = useState({
      caseSensitive: false,
      wholeWord: false,
      regex: false,
    });
    const [findResults, setFindResults] = useState<TerminalSearchResults>({
      resultIndex: -1,
      resultCount: 0,
    });
    const findQueryRef = useRef(findQuery);
    findQueryRef.current = findQuery;
    const findOptsRef = useRef(findOpts);
    findOptsRef.current = findOpts;

    /**
     * Run a search pass against the backend. `incremental` keeps the active
     * match anchored while the operator types (VS Code behavior); plain
     * next/prev navigation passes false to advance. Invalid regexes are
     * swallowed — the addon throws on bad patterns mid-typing (e.g. "[").
     */
    const runFind = useCallback((direction: "next" | "prev", incremental = false) => {
      const b = backendRef.current;
      if (!b) return;
      const q = findQueryRef.current;
      if (!q) {
        b.clearSearch();
        setFindResults({ resultIndex: -1, resultCount: 0 });
        return;
      }
      const opts = { ...findOptsRef.current, incremental };
      try {
        if (direction === "next") b.findNext(q, opts);
        else b.findPrevious(q, opts);
      } catch {
        // Invalid regex while typing — leave previous results in place.
      }
    }, []);

    const handleFindQueryChange = useCallback(
      (q: string) => {
        setFindQuery(q);
        findQueryRef.current = q; // searchable immediately, before re-render
        runFind("next", true);
      },
      [runFind],
    );

    const toggleFindOpt = useCallback(
      (key: "caseSensitive" | "wholeWord" | "regex") => {
        const next = { ...findOptsRef.current, [key]: !findOptsRef.current[key] };
        findOptsRef.current = next;
        setFindOpts(next);
        runFind("next", true);
      },
      [runFind],
    );

    const closeFind = useCallback(() => {
      setFindOpen(false);
      setFindResults({ resultIndex: -1, resultCount: 0 });
      backendRef.current?.clearSearch();
      backendRef.current?.focus();
    }, []);

    // Expose selection, write, and scrollback API to parent components
    useImperativeHandle(ref, () => ({
      getSelection: () => backendRef.current?.getSelection() ?? "",
      hasSelection: () => backendRef.current?.hasSelection() ?? false,
      writeToTerminal: (text: string) => {
        const bytes = encoder.encode(text);
        invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(() => {});
      },
      writeToDisplay: (data: string) => {
        backendRef.current?.write(data);
      },
      getScrollback: (maxLines = 500) => {
        const backend = backendRef.current;
        if (!backend) return "";
        const totalLines = backend.getBufferLength();
        const startLine = Math.max(0, totalLines - maxLines);
        const lines: string[] = [];
        for (let i = startLine; i < totalLines; i++) {
          const line = backend.getBufferLine(i);
          if (line) {
            lines.push(line);
          }
        }
        return lines.join("\n");
      },
      scrollToBottom: () => {
        backendRef.current?.scrollToBottom();
      },
    }));

    // Debounced fit — coalesce rapid resize events
    const fitTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const fitTerminal = useCallback(() => {
      if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
      fitTimerRef.current = setTimeout(() => {
        const backend = backendRef.current;
        if (!backend || !containerRef.current) return;
        // Skip resize when container is hidden (e.g., compact mode) to avoid
        // resizing the PTY to zero columns which garbles output wrapping.
        const { clientWidth, clientHeight } = containerRef.current;
        if (clientWidth === 0 || clientHeight === 0) return;
        try {
          backend.fit();
          invoke("terminal_resize", {
            terminalId,
            cols: backend.cols,
            rows: backend.rows,
          }).catch(() => {});
        } catch {
          // Container may not be visible yet
        }
      }, 50);
    }, [terminalId]);

    useEffect(() => {
      if (!containerRef.current) return;
      const container = containerRef.current;

      let disposed = false;
      const disposables: Array<{ dispose(): void }> = [];
      let outputUnsub: (() => void) | null = null;
      let exitUnsub: (() => void) | null = null;
      let ackTimer: ReturnType<typeof setInterval> | null = null;
      let quietTitleTimer: ReturnType<typeof setInterval> | null = null;
      let observer: ResizeObserver | null = null;
      let paneTierDeclaration: ReturnType<typeof declarePaneTier> | null = null;
      let blockNativePaste: ((e: Event) => void) | null = null;
      let wheelScrollOverride: ((e: WheelEvent) => void) | null = null;
      let contextMenuCopyPaste: ((e: MouseEvent) => void) | null = null;
      // Ensure the dev instrumentation hook (window.__qontinuiTerminal) exists
      // so it's discoverable from the console even before any paint fires.
      getTerminalDebug();
      // Bytes received before the backend finishes async init. Tauri does
      // NOT queue events before listen() resolves, and createTerminalBackend
      // (WASM load + open + key handlers + UI Bridge registration) takes
      // ~200ms; without this buffer we'd silently drop everything Claude
      // emits in that window and xterm would freeze on whatever the grid
      // contained at bootstrap snapshot time. Chunks keep their absolute
      // stream offset so the drain can dedup against the scrollback-ring
      // replay (see scrollbackReplay.ts).
      let pendingBytes: OffsetChunk[] = [];
      let backendReady = false;
      // End offset (exclusive) of the scrollback-ring replay written into
      // this xterm instance during init. Live/pending chunks below this
      // boundary are already in the buffer via the ring and must not be
      // written again.
      let replayedThrough = 0;
      // Emission-gap resync state. The Rust reader gates WEBVIEW EMISSION
      // (never the PTY read) on flow-control backpressure, so when this
      // renderer falls behind — or the pane mounts onto a terminal whose
      // emission was paused — the live stream carries an offset jump. We
      // detect it via `nextExpectedOffset` (exclusive end of the last
      // observed chunk), hold post-gap chunks in `resyncPending`, refetch
      // the scrollback ring (which also resets the backend's flow-control
      // counters, resuming emission), write the missed slice, then drain the
      // held chunks trimmed against the advanced replay boundary.
      // `writtenThrough` tracks the exclusive end of bytes actually written
      // (ring replays + live chunks) — the resync slice boundary.
      let nextExpectedOffset: number | null = null;
      let writtenThrough = 0;
      let resyncInFlight = false;
      /** A resync was requested while one was already running (see `resyncFromRing`). */
      let resyncAgainRequested = false;
      let resyncPending: OffsetChunk[] = [];
      // Phase 3 — render-based flow control. Tracks bytes the backend has
      // actually RENDERED (its `write` completion callback fired), not bytes
      // merely received off the wire, and decides when to ack the Rust reader
      // (every ~AckSize rendered bytes; the floor timer below drains a trailing
      // remainder). This replaces the old 250ms byte-count ack so a burst can't
      // outrun xterm's ~50MB input buffer (which silently drops overflow)
      // before backpressure engages.
      const renderAck = new RenderAckAccumulator();
      const sendAck = (bytes: number) => {
        if (bytes <= 0) return;
        invoke("terminal_ack", { terminalId, bytesAcked: bytes }).catch(() => {});
      };
      // Phase 4 — write coalescing. Per `terminal-output` event we trim the
      // chunk against the ring-replay boundary and stage the unreplayed suffix
      // here; a microtask flush concatenates everything staged into a SINGLE
      // `backend.write()`. xterm's own RenderDebouncer then batches one reflow
      // per frame — we add no rAF loop around xterm. The render-ack callback
      // fires once for the whole coalesced write and accounts its full length.
      let coalesceQueue: Uint8Array[] = [];
      let coalesceLen = 0;
      let flushScheduled = false;
      const flushCoalesced = () => {
        flushScheduled = false;
        if (coalesceLen === 0) return;
        const b = backendRef.current;
        if (!b) {
          coalesceQueue = [];
          coalesceLen = 0;
          return;
        }
        let merged: Uint8Array;
        if (coalesceQueue.length === 1) {
          merged = coalesceQueue[0];
        } else {
          merged = new Uint8Array(coalesceLen);
          let at = 0;
          for (const c of coalesceQueue) {
            merged.set(c, at);
            at += c.length;
          }
        }
        const writtenLen = merged.length;
        coalesceQueue = [];
        coalesceLen = 0;
        renderAck.onWriteIssued(writtenLen);
        try {
          b.write(merged, () => {
            sendAck(renderAck.onRendered(writtenLen));
          });
        } catch (e) {
          console.error(`[Terminal ${terminalId}] write error:`, e);
          // The render callback won't fire for a failed write — settle the
          // in-flight bytes so the accumulator and reader backpressure don't
          // wedge on a permanently-unrendered chunk.
          sendAck(renderAck.onRendered(writtenLen));
        }
      };
      const scheduleFlush = () => {
        if (flushScheduled) return;
        flushScheduled = true;
        queueMicrotask(flushCoalesced);
      };
      /**
       * Apply a one-shot bootstrap paint of the Rust `GridSnapshot` into the
       * backend, with the Phase-2 alt-screen guard.
       *
       * The periodic idle reconciliation paint was RETIRED in Phase 2: the
       * Rust grid is single-buffer and can't model the alt screen that
       * Claude's TUI lives on, so a periodic full-screen overlay could never
       * be made safe. The live `terminal-output` stream is authoritative for
       * forward motion; offset-dedup + remount ring-replay are the recovery
       * path for missed chunks. paintGrid now runs ONLY for the two one-shot
       * bootstraps (mount-gap + reconnect), and even those hard-skip when the
       * snapshot reports the alt screen is active — overlaying a single-buffer
       * snapshot that holds alt content onto a freshly-mounted xterm can
       * mis-place rows. In that case we let the live stream + ring-replay
       * populate instead.
       *
       * Title reporting is decoupled from the paint (see the ack timer) so
       * titles still flow up even when the paint is skipped.
       */
      const bootstrapPaint = (
        snapshot: GridSnapshot,
        reason: "mount-bootstrap" | "reconnect-bootstrap",
      ) => {
        const b = backendRef.current;
        if (!b) return;
        const altScreen = snapshot.alt_screen === true;
        const applied = !altScreen;
        if (applied) {
          paintGrid(b, snapshot);
        }
        reportTitleFromSnapshot(snapshot.title);
        recordPaintGrid({
          terminalId,
          reason,
          altScreen,
          applied,
          bytesWritten: bytesReceivedRef.current,
          ts: Date.now(),
        });
      };

      /**
       * Emit `onTitleChange` if the snapshot's OSC 0/2 title is non-empty AND
       * differs from the last reported value. Called from the bootstrap paint
       * and from the ack-timer title poll so the latest title from the Rust
       * grid flows up to the tab title without spamming the callback.
       */
      const reportTitleFromSnapshot = (title: string | undefined | null) => {
        if (!title) return;
        if (title === lastTitleRef.current) return;
        lastTitleRef.current = title;
        try {
          onTitleChangeRef.current?.(title);
        } catch (e) {
          console.warn(`[Terminal ${terminalId}] onTitleChange handler error:`, e);
        }
      };

      /**
       * Mid-session recovery for a webview-emission gap (flow-control drop).
       * Each pass fetches the scrollback ring — which also resets the
       * backend's flow-control counters, resuming emission — writes the
       * slice the renderer missed, then drains held chunks only up to the
       * first REMAINING hole (`drainHeldChunks`): post-hole chunks stay held
       * so the next pass's (advanced) ring window can cover the hole first —
       * writing them early would splice the hole out permanently. Bounded
       * retries; on exhaustion or fetch failure the leftovers are written
       * spliced rather than stranded (full-frame TUI redraws self-heal, and
       * the next remount's ring replay reconciles scrollback).
       *
       * Also the catch-up path when a hidden pane is revealed (Phase 2 / A4),
       * which is why a request that arrives while a pass is in flight is
       * QUEUED rather than dropped: the running pass is working from a ring
       * snapshot taken before the newly-missed bytes existed, so silently
       * declining would leave a hole that no later gap detection can see
       * (`nextExpectedOffset` keeps advancing across drops, so the stream
       * looks contiguous afterwards).
       */
      const resyncFromRing = async () => {
        if (resyncInFlight) {
          resyncAgainRequested = true;
          return;
        }
        resyncInFlight = true;
        // Set only on the one exit that PROVES the pane is caught up: a replay
        // that covered every dropped byte AND left no held chunk behind a
        // hole. Every other exit — a rejected fetch, an empty ring, a replay
        // that stopped short of bytes dropped while it was in flight, the
        // retry budget running out — falls through to the `finally`, which
        // re-arms the pane and tells the operator the pane is spliced.
        let caughtUp = false;
        try {
          for (let attempt = 0; attempt < RESYNC_MAX_ATTEMPTS; attempt++) {
            let ring: {
              success: boolean;
              data: { data: string; startOffset: number; endOffset: number } | null;
            };
            try {
              ring = await invoke("terminal_get_scrollback", { terminalId });
            } catch (e) {
              console.warn(
                `[Terminal ${terminalId}] emission-gap resync fetch failed (attempt ${attempt + 1}):`,
                e,
              );
              continue;
            }
            const b = backendRef.current;
            if (disposed || !b) return;
            if (!ring.success || !ring.data) {
              console.warn(
                `[Terminal ${terminalId}] scrollback ring unavailable for resync (attempt ${attempt + 1})`,
              );
              continue;
            }
            const rawRing = atob(ring.data.data);
            const ringBytes = new Uint8Array(rawRing.length);
            for (let i = 0; i < rawRing.length; i++) {
              ringBytes[i] = rawRing.charCodeAt(i);
            }
            const ringWindow = {
              startOffset: ring.data.startOffset,
              endOffset: ring.data.endOffset,
            };
            // Ring overrun: the hole starts before the oldest byte the backend
            // still holds, so those bytes are gone from every source and no
            // retry can produce them. `resyncSliceStart` replays the whole
            // ring, which would join the pane's last written byte straight to
            // a later one — indistinguishable from continuous output. Say so
            // in-band instead, and warn for the log.
            const lost = lostWindowBytes(ringWindow, writtenThrough);
            if (lost > 0) {
              console.warn(
                `[Terminal ${terminalId}] scrollback ring overran the missed window; ${lost} bytes of output are unrecoverable`,
              );
              b.write(formatLostOutputMarker(lost));
            }
            const from = resyncSliceStart(ringWindow, writtenThrough);
            const slice = ringBytes.subarray(from);
            if (slice.length > 0) {
              // Direct write (not the coalesce queue): any pre-gap staged
              // chunks flushed in an earlier microtask, and the held
              // post-gap chunks drain AFTER this, so stream order holds.
              b.write(slice);
            }
            replayedThrough = Math.max(replayedThrough, ringWindow.endOffset);
            writtenThrough = Math.max(writtenThrough, ringWindow.endOffset);
            nextExpectedOffset = Math.max(nextExpectedOffset ?? 0, ringWindow.endOffset);
            // Clears the pane's "behind" flag only if this ring window reached
            // every byte that was dropped. A chunk dropped while the fetch was
            // in flight sits beyond this (older) snapshot — the next attempt's
            // ring covers it.
            const covered = paneService.noteResynced(replayedThrough);

            const drained = drainHeldChunks(resyncPending, writtenThrough, replayedThrough);
            resyncPending = drained.reheld;
            writtenThrough = drained.writtenThrough;
            for (const w of drained.writable) {
              coalesceQueue.push(w);
              coalesceLen += w.length;
            }
            if (drained.writable.length > 0) scheduleFlush();
            if (covered && drained.reheld.length === 0) {
              caughtUp = true;
              return;
            }
            console.warn(
              `[Terminal ${terminalId}] emission gap persisted across resync (attempt ${attempt + 1}); retrying`,
            );
          }
        } catch (e) {
          console.warn(`[Terminal ${terminalId}] emission-gap resync failed:`, e);
        } finally {
          // Never strand held chunks: on success the loop drained them all;
          // on retry exhaustion or fetch failure, write what remains even
          // though a hole may precede it (spliced beats frozen).
          const rest = resyncPending;
          resyncPending = [];
          for (const c of rest) {
            const slice = trimReplayedChunk(c, replayedThrough);
            if (slice && slice.length > 0) {
              coalesceQueue.push(slice);
              coalesceLen += slice.length;
              scheduleFlush();
            }
            if (c.offset !== undefined) {
              writtenThrough = Math.max(writtenThrough, c.offset + c.bytes.length);
            }
          }
          // A request that arrived mid-pass ran against a stale ring snapshot;
          // run exactly one more pass to cover what it could not see. That
          // pass is the retry, so don't declare the gap unrecovered yet.
          const retryQueued = resyncAgainRequested && !disposed;
          if (!caughtUp && !disposed && !retryQueued) {
            // Stay armed so the next reveal tries again — and surface it, because
            // what the operator is looking at right now has a hole in it.
            paneService.noteResyncFailed();
            console.warn(
              `[Terminal ${terminalId}] emission-gap resync did not complete; output is spliced`,
            );
            backendRef.current?.write(RESYNC_INCOMPLETE_MARKER);
          }
          resyncInFlight = false;
          if (retryQueued) {
            resyncAgainRequested = false;
            void resyncFromRing();
          }
        }
      };

      // ── Visibility tier (Phase 2 / A4) ──────────────────────────────────
      // Everything a pane costs while the operator cannot see it lives behind
      // this service: the 250ms ack-floor timer (which also carries the
      // every-4th-tick `terminal_grid_text` title poll), the ResizeObserver,
      // and the xterm parse/write of every chunk. Hidden ⇒ all three stop and
      // the page tap takes over acking; visible again ⇒ they restart and the
      // ring replays whatever was missed.
      // The lightweight title poll (Phase 2 of the rendering-robustness plan):
      // title reporting used to ride on the retired periodic idle paint. Only
      // when there is an `onTitleChange` consumer do we fetch the cheap text
      // snapshot (`terminal_grid_text` does NOT clone the cell buffer, unlike
      // `terminal_get_grid`) and report any new OSC 0/2 title.
      // `reportTitleFromSnapshot` already dedups against the last value, so
      // unchanged titles are free.
      const pollTitle = () => {
        if (!onTitleChangeRef.current) return;
        invoke<{ success: boolean; data: { title?: string | null } | null }>("terminal_grid_text", {
          terminalId,
        })
          .then((r) => {
            if (!disposed && r.success && r.data) {
              reportTitleFromSnapshot(r.data.title);
            }
          })
          .catch(() => {
            /* title poll best-effort; ignore failures */
          });
      };
      let titlePollTick = 0;
      const startAckTimer = () => {
        if (ackTimer) return;
        // Flow control floor + title poll. The primary ack is RENDER-based
        // (Phase 3): the coalesced write's completion callback acks every
        // ~AckSize rendered bytes. This timer is the floor — it drains a
        // trailing sub-AckSize remainder so the last bytes of a burst still
        // ack and the Rust reader resumes even when no further output arrives
        // to trip the AckSize threshold. It carries the title poll every 4th
        // tick (~1 Hz).
        ackTimer = setInterval(() => {
          // Drain any rendered-but-sub-AckSize remainder.
          sendAck(renderAck.flush());

          titlePollTick = (titlePollTick + 1) % 4;
          if (titlePollTick === 0) pollTitle();
        }, ACK_FLOOR_INTERVAL_MS);
      };
      const stopAckTimer = () => {
        if (!ackTimer) return;
        clearInterval(ackTimer);
        ackTimer = null;
      };
      const startObserver = () => {
        if (observer) return;
        observer = new ResizeObserver(() => fitTerminal());
        observer.observe(container);
      };
      const stopObserver = () => {
        observer?.disconnect();
        observer = null;
      };
      // A hidden pane keeps a HALF-RATE title poll and nothing else. The tab
      // title is the one operator-visible signal a parked or compacted pane
      // still owns — state chips and sparklines are fed by the page tap, but
      // `onTitleChange` has no other source, so killing this outright would
      // freeze the titles of every unassigned tab and every compact zone card
      // at whatever they were when the pane mounted.
      const startQuietTitleTimer = () => {
        if (quietTitleTimer) return;
        quietTitleTimer = setInterval(pollTitle, QUIET_TITLE_POLL_INTERVAL_MS);
      };
      const stopQuietTitleTimer = () => {
        if (!quietTitleTimer) return;
        clearInterval(quietTitleTimer);
        quietTitleTimer = null;
      };

      const paneService = new PaneVisibilityService(
        {
          enterActiveTier: () => {
            startObserver();
            startAckTimer();
          },
          leaveActiveTier: () => {
            stopAckTimer();
            stopObserver();
          },
          enterQuietTier: startQuietTitleTimer,
          leaveQuietTier: stopQuietTitleTimer,
          flushAck: () => sendAck(renderAck.flush()),
          refit: () => {
            // Schedule after layout so the container has actual dimensions.
            setTimeout(() => {
              if (disposed) return;
              fitTerminal();
              backendRef.current?.focus();
            }, 16);
          },
          resync: () => {
            void resyncFromRing();
          },
          // Phase 5 — tell the runner what this pane is worth. `null` on
          // unmount drops the declaration; with no pane anywhere declaring it,
          // the reconciler pushes `unwatched` and the runner stops emitting
          // `terminal-output` for the terminal altogether.
          setVisibilityTier: (tier) => {
            if (tier === null) {
              paneTierDeclaration?.release();
              paneTierDeclaration = null;
            } else if (paneTierDeclaration) {
              paneTierDeclaration.update(tier);
            } else {
              paneTierDeclaration = declarePaneTier(terminalId, tier);
            }
          },
        },
        visibleRef.current,
      );
      paneServiceRef.current = paneService;

      /**
       * The per-terminal `terminal-output` handler. Registered with the
       * window's single demuxed listener (Phase 2 / A3), so it is called only
       * for THIS terminal's chunks — the id filter no longer runs after an
       * IPC deserialize + dispatch that every mounted pane paid.
       */
      const handleOutputPayload = (payload: TerminalOutputPayload) => {
        const raw = atob(payload.data);
        const bytes = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) {
          bytes[i] = raw.charCodeAt(i);
        }
        const offset = payload.offset;

        // Emission-gap detection: the backend gates webview emission (not
        // the PTY read) under backpressure, so a dropped chunk surfaces as
        // the next delivered chunk starting beyond the end of the last one.
        // Only meaningful once the backend is ready — pre-ready holes are
        // covered wholesale by the mount ring replay.
        const gapDetected = backendReady && isEmissionGap(nextExpectedOffset, offset);
        // Keep the "last observed chunk end" advancing in EVERY tier,
        // including while hidden: it is what stops the first chunk after a
        // reveal from being misread as a fresh emission gap.
        if (offset !== undefined) {
          nextExpectedOffset = Math.max(nextExpectedOffset ?? 0, offset + bytes.length);
        }

        const route = routeOutputChunk({
          paused: paneService.paused,
          backendReady,
          gapDetected,
          resyncInFlight,
        });

        if (route === "drop") {
          // Hidden pane: no parse, no write, no ack. `writtenThrough` stays
          // put, so the reveal resync replays exactly this window from the
          // ring. State chips/sparklines keep updating — they are fed by the
          // page-level output tap, which is mount- and visibility-independent.
          // The chunk's end offset is what makes the reveal resync provable:
          // a replay only clears the flag once it has reached this far.
          paneService.noteMissedOutput(offset === undefined ? undefined : offset + bytes.length);
          bytesReceivedRef.current += bytes.length;
          return;
        }

        if (route === "buffer") {
          // Backend not yet created — buffer the bytes; they'll be drained
          // (and the idle timer primed) once the backend init completes.
          pendingBytes.push({ bytes, offset });
          return;
        }

        if (!backendRef.current) return;

        if (route === "hold") {
          // Hold post-gap chunks until the ring slice covering the hole is
          // written so bytes land in stream order; the resync drain trims
          // each held chunk against the advanced replay boundary.
          resyncPending.push({ bytes, offset });
          bytesReceivedRef.current += bytes.length;
          if (gapDetected) void resyncFromRing();
          return;
        }
        // A chunk emitted before the ring snapshot can still be DELIVERED
        // after the replay (event delivery and invoke responses are not
        // mutually ordered) — trim it to its unreplayed suffix so the
        // boundary bytes aren't written twice. ack bookkeeping below stays on
        // the full chunk: the content reached the terminal either way (via
        // the ring), only the duplicate write is skipped.
        //
        // Session-state tracking is NOT fed from here anymore — the global
        // `terminal-output` tap in `TerminalSessionContext` decodes + feeds
        // `handleOutput` independently of this instance's mount state (Phase 2
        // of the flow-grid virtualization plan). This handler is now purely
        // the xterm write/render path.
        const unreplayed = trimReplayedChunk({ bytes, offset }, replayedThrough);
        if (unreplayed && unreplayed.length > 0) {
          // Phase 4: stage the unreplayed suffix and flush once per microtask
          // into a single coalesced backend.write() (offset-dedup already
          // applied above, before coalescing). The render-ack accounts the
          // coalesced length when its completion callback fires. Length
          // guard: zero-length resume-marker events carry only an offset.
          coalesceQueue.push(unreplayed);
          coalesceLen += unreplayed.length;
          scheduleFlush();
        }
        if (offset !== undefined) {
          writtenThrough = Math.max(writtenThrough, offset + bytes.length);
        }
        bytesReceivedRef.current += bytes.length;
      };

      // Cold-mount path: register the output handler IMMEDIATELY — before any
      // awaits — so we don't lose bytes during the backend's async init.
      // Registration is synchronous (the window's listener is already live, or
      // is installed by this call), so unlike the old per-instance
      // `await listen(...)` there is no attach window at all. Bytes arriving
      // before backendReady are buffered into pendingBytes and drained once
      // the backend is ready.
      //
      // Reconnect path (Layer 4 polish): the PTY survived the page reload and
      // the Rust grid already holds the full state. Live bytes during the
      // catch-up window would race with paintGrid, so we DEFER the handler
      // registration until after the bootstrap paintGrid resolves. See
      // `plans/terminal-grid-bootstrap-redesign.md` Layer 4.
      if (!isReconnecting) {
        outputUnsub = registerTerminalOutputHandler(terminalId, handleOutputPayload);
      }

      // Async init because backend creation may require WASM loading
      (async () => {
        const backend = await createTerminalBackend(backendType, {
          ...TERMINAL_OPTIONS,
          gpuAcceleration,
          // Pane identity for the WebGL context LRU (A9) — at most 8 panes
          // hold a GL context, the rest render on Canvas.
          instanceKey: terminalId,
        });
        if (disposed) {
          backend.dispose();
          return;
        }

        backendRef.current = backend;

        // Register file path link provider (backend-agnostic)
        disposables.push(
          backend.registerLinkProvider(
            new FilePathLinkProvider((line) => backend.getBufferLine(line)),
          ),
        );

        // Open terminal in container
        backend.open(container);

        // Style viewport scrollbar to match the runner's dark theme
        const viewport = backend.getViewportElement();
        if (viewport) {
          viewport.classList.add("scrollbar-dark");
        }

        // Shift+wheel → always scroll the local scrollback, even when the
        // foreground program has captured the wheel via a mouse-tracking mode
        // (DECSET 1000/1002/1003). xterm.js sets `handleMouseWheel: false` in
        // that state and forwards the wheel to the app, so a plain wheel can't
        // scroll the buffer; its `attachCustomWheelEventHandler` is also
        // bypassed once mouse mode is on. We intercept on the container in the
        // CAPTURE phase (before xterm's own bubble-phase listeners on the
        // viewport/screen) and, only when Shift is held, take the event over.
        // Un-modified wheel events are left untouched so the app keeps its
        // mouse interaction — matching the Konsole / GNOME Terminal / Windows
        // Terminal "Shift bypasses application mouse reporting" convention.
        wheelScrollOverride = (e: WheelEvent) => {
          if (!e.shiftKey) return;
          const b = backendRef.current;
          if (!b) return;
          // Take over: stop xterm (and the app) from also seeing this wheel.
          e.preventDefault();
          e.stopImmediatePropagation();
          wheelScrollAccumRef.current += wheelToLineDelta(e, b.rows, DEFAULT_CELL_HEIGHT_PX);
          const whole = Math.trunc(wheelScrollAccumRef.current);
          if (whole !== 0) {
            wheelScrollAccumRef.current -= whole;
            b.scrollLines(whole);
          }
        };
        container.addEventListener("wheel", wheelScrollOverride, {
          capture: true,
          passive: false,
        });

        // Initial fit after layout settles
        requestAnimationFrame(() => fitTerminal());

        // Track selection changes for parent components
        disposables.push(
          backend.onSelectionChange(() => {
            onSelectionChangeRef.current?.(backend.hasSelection());
          }),
        );

        // Find-in-terminal match counts → TerminalFindBar ("k of n").
        disposables.push(
          backend.onSearchResults((results) => {
            setFindResults(results);
          }),
        );

        // Handle Ctrl+C copy (when text is selected) and Ctrl+V paste.
        // Tauri's webview doesn't fire the browser clipboard events that xterm.js
        // relies on, so we intercept the keys and use the clipboard API manually.
        backend.attachCustomKeyEventHandler((event) => {
          // Ctrl+C: copy selected text, or pass through as SIGINT when nothing selected
          if (event.type === "keydown" && event.key === "c" && event.ctrlKey && !event.shiftKey) {
            if (backend.hasSelection()) {
              // Native clipboard write (see writeClipboard) — navigator.clipboard
              // silently fails in the webview when unfocused, which is why copy
              // "often didn't work".
              void writeClipboard(backend.getSelection());
              return false; // prevent terminal from sending SIGINT
            }
            // No selection → let Ctrl+C pass through as SIGINT
          }

          // Ctrl+Shift+C: conventional terminal "copy" shortcut. Previously
          // unhandled, so users relying on it got nothing on the clipboard.
          // Copy the selection (if any) and swallow the key either way — it has
          // no other meaning inside a terminal.
          if (
            event.type === "keydown" &&
            event.key.toLowerCase() === "c" &&
            event.ctrlKey &&
            event.shiftKey
          ) {
            if (backend.hasSelection()) {
              void writeClipboard(backend.getSelection());
            }
            return false;
          }

          if (
            event.type === "keydown" &&
            event.key === "v" &&
            (event.ctrlKey || event.metaKey) &&
            !event.shiftKey
          ) {
            navigator.clipboard
              .readText()
              .then((text) => {
                if (text) {
                  // Write directly to PTY instead of paste to avoid double
                  // paste when WebView2 also fires a native paste event. Run
                  // the clipboard text through the same bracketed-paste +
                  // newline normalization xterm's own paste would apply — a raw
                  // write breaks multi-line paste into TUIs that enable
                  // bracketed paste mode (Claude Code, vim, fzf). See
                  // `preparePaste.ts`.
                  const prepared = preparePasteData(text, backend.bracketedPasteMode);
                  const bytes = encoder.encode(prepared);
                  invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(
                    () => {},
                  );
                }
              })
              .catch(() => {});
            return false; // prevent terminal default handling
          }

          // VS Code-parity find: Ctrl+F opens the find bar; F3 / Shift+F3
          // jump to the next/previous match (opening the bar if needed).
          // Swallowed so the keys never reach the PTY — Ctrl+F would
          // otherwise be forward-char in readline/PSReadLine.
          if (
            event.type === "keydown" &&
            (event.ctrlKey || event.metaKey) &&
            !event.altKey &&
            !event.shiftKey &&
            event.key.toLowerCase() === "f"
          ) {
            event.preventDefault();
            setFindOpen(true);
            setFindFocusSeq((s) => s + 1);
            return false;
          }
          if (
            event.type === "keydown" &&
            event.key === "F3" &&
            !event.ctrlKey &&
            !event.altKey &&
            !event.metaKey
          ) {
            event.preventDefault();
            setFindOpen(true);
            if (findQueryRef.current) {
              runFind(event.shiftKey ? "prev" : "next");
            } else {
              setFindFocusSeq((s) => s + 1);
            }
            return false;
          }

          // VS Code-parity scrollback navigation: Shift+PageUp/PageDown,
          // Ctrl+Alt+PageUp/PageDown, Ctrl+Home/End, Ctrl+Up/Down (prev/next
          // command via OSC 633;A marks). Scroll the focused terminal locally
          // and swallow the key so it isn't forwarded to the PTY (xterm
          // doesn't preventDefault on a `false` return, so we do it here to
          // also stop any browser-default scroll/caret motion).
          if (event.type === "keydown") {
            const action = matchScrollShortcut(event);
            if (action) {
              event.preventDefault();
              switch (action.kind) {
                case "lines":
                  backend.scrollLines(action.amount);
                  break;
                case "pages":
                  backend.scrollPages(action.amount);
                  break;
                case "top":
                  backend.scrollToTop();
                  break;
                case "bottom":
                  backend.scrollToBottom();
                  break;
                case "prevCommand":
                  backend.scrollToPreviousCommand();
                  break;
                case "nextCommand":
                  backend.scrollToNextCommand();
                  break;
              }
              return false;
            }
          }
          return true;
        });

        // Block native paste events from reaching the terminal — we handle paste
        // manually in the custom key handler above.  Without this, WebView2
        // fires a native "paste" event that the terminal processes via onData(),
        // causing the pasted text to be written to the PTY a second time.
        const inputEl = backend.getInputElement();
        blockNativePaste = (e: Event) => {
          e.preventDefault();
          e.stopPropagation();
        };
        inputEl?.addEventListener("paste", blockNativePaste, true);

        // Right-click copy/paste (VS Code `terminal.integrated.rightClickBehavior:
        // "copyPaste"` parity): with a selection, right-click copies it (and
        // clears the highlight as feedback); with no selection, it pastes the
        // clipboard. We preventDefault + stopPropagation so neither the WebView2
        // context menu nor the enclosing zone's `onContextMenu` (which opens the
        // zone menu) fires — inside the terminal body, right-click belongs to
        // copy/paste.
        contextMenuCopyPaste = (e: MouseEvent) => {
          const b = backendRef.current;
          if (!b) return;
          e.preventDefault();
          e.stopPropagation();
          if (b.hasSelection()) {
            const selection = b.getSelection();
            if (selection) {
              // Native clipboard write (see writeClipboard). Only clear the
              // highlight once the copy actually succeeds, so a failed write
              // doesn't also lose the user's selection.
              void writeClipboard(selection).then((copied) => {
                if (copied) b.clearSelection();
              });
            }
            return;
          }
          // No selection → paste, mirroring the Ctrl+V path (bracketed-paste +
          // newline normalization; see `preparePaste.ts`).
          navigator.clipboard
            .readText()
            .then((text) => {
              if (!text) return;
              const prepared = preparePasteData(text, b.bracketedPasteMode);
              const bytes = encoder.encode(prepared);
              invoke("terminal_write", { terminalId, data: uint8ToBase64(bytes) }).catch(() => {});
            })
            .catch(() => {});
        };
        container.addEventListener("contextmenu", contextMenuCopyPaste);

        // Register terminal input with UI Bridge for external automation.
        const bridgeRegistry = uiBridge?.registry;
        if (bridgeRegistry && inputEl) {
          const termId = terminalId; // capture for closures
          bridgeRegistry.registerElement(`terminal-input-${termId}`, inputEl, {
            type: "textarea",
            label: `Terminal input (${termId.slice(0, 8)})`,
            actions: ["focus", "blur"],
            customActions: {
              sendKeys: {
                id: "sendKeys",
                description: "Send key sequences to the terminal",
                handler: (params?: unknown) => {
                  const { keys } = (params || {}) as { keys?: string };
                  if (!keys) return;
                  const bytes = encoder.encode(keys);
                  invoke("terminal_write", {
                    terminalId: termId,
                    data: uint8ToBase64(bytes),
                  }).catch(() => {});
                },
              },
              writeToTerminal: {
                id: "writeToTerminal",
                description: "Write text directly to the PTY (no keyboard events)",
                handler: (params?: unknown) => {
                  const { text } = (params || {}) as { text?: string };
                  if (!text) return;
                  const bytes = encoder.encode(text);
                  invoke("terminal_write", {
                    terminalId: termId,
                    data: uint8ToBase64(bytes),
                  }).catch(() => {});
                },
              },
              paste: {
                id: "paste",
                description: "Read clipboard and write to PTY (same as Ctrl+V)",
                handler: async () => {
                  const text = await navigator.clipboard.readText().catch(() => "");
                  if (text) {
                    // Same bracketed-paste + newline normalization as the
                    // Ctrl+V path (see `preparePaste.ts`).
                    const prepared = preparePasteData(text, backend.bracketedPasteMode);
                    const bytes = encoder.encode(prepared);
                    invoke("terminal_write", {
                      terminalId: termId,
                      data: uint8ToBase64(bytes),
                    }).catch(() => {});
                  }
                },
              },
              pasteText: {
                id: "pasteText",
                description:
                  "Paste literal text through the Ctrl+V path (bracketed-paste aware); no clipboard/keyboard. For automated tests.",
                handler: (params?: unknown) => {
                  const { text } = (params || {}) as { text?: string };
                  if (!text) return;
                  const b = backendRef.current;
                  const prepared = preparePasteData(text, b?.bracketedPasteMode ?? false);
                  const bytes = encoder.encode(prepared);
                  invoke("terminal_write", {
                    terminalId: termId,
                    data: uint8ToBase64(bytes),
                  }).catch(() => {});
                },
              },
              getScrollback: {
                id: "getScrollback",
                description: "Read the terminal scrollback buffer as plain text",
                handler: (params?: unknown) => {
                  const { maxLines = 500 } = (params || {}) as { maxLines?: number };
                  const b = backendRef.current;
                  if (!b) return "";
                  const totalLines = b.getBufferLength();
                  const startLine = Math.max(0, totalLines - maxLines);
                  const lines: string[] = [];
                  for (let i = startLine; i < totalLines; i++) {
                    const line = b.getBufferLine(i);
                    if (line) lines.push(line);
                  }
                  return lines.join("\n");
                },
              },
            },
          });
        }

        // Forward user input to PTY + track first input line for auto-naming
        // + emit every non-empty line to `onUserInputLine` for mid-session
        // probes. The accumulator logic lives in the pure `consumeInputChunk`
        // helper in `./consumeInputChunk.ts` (a leaf module — kept separate
        // so vitest tests don't pull in xterm.js's canvas addon).
        disposables.push(
          backend.onData((data) => {
            const result = consumeInputChunk(
              data,
              inputAccumulatorRef.current,
              firstInputReportedRef.current,
            );
            inputAccumulatorRef.current = result.accum;
            // First-input gate: fires at most once per session.
            if (result.firstInputLineIfAny !== undefined) {
              firstInputReportedRef.current = true;
              onFirstInputRef.current?.(result.firstInputLineIfAny);
            }
            // Per-line callback: fires for EVERY non-empty newline-terminated
            // line, ungated. Consumer (e.g. useMidSessionProbe) applies its
            // own gates.
            const perLine = onUserInputLineRef.current;
            if (perLine) {
              for (const line of result.lines) {
                perLine(line);
              }
            }

            if (!isOwnedRef.current) return; // owner-gated stdin (Phase 1)
            const bytes = encoder.encode(data);
            invoke("terminal_write", {
              terminalId,
              data: uint8ToBase64(bytes),
            }).catch(() => {});
          }),
        );

        // Forward binary data (e.g. paste with special chars)
        disposables.push(
          backend.onBinary((data) => {
            if (!isOwnedRef.current) return; // owner-gated stdin (Phase 1)
            const bytes = new Uint8Array(data.length);
            for (let i = 0; i < data.length; i++) {
              bytes[i] = data.charCodeAt(i);
            }
            invoke("terminal_write", {
              terminalId,
              data: uint8ToBase64(bytes),
            }).catch(() => {});
          }),
        );

        // Layer 2 bootstrap (live-first):
        //   1. The live `terminal-output` listener was attached at the very
        //      top of this effect, BEFORE createTerminalBackend. Bytes that
        //      arrived during async init were buffered into pendingBytes —
        //      drain them here now that the backend exists.
        //   2. Forward motion is owned ENTIRELY by the live `terminal-output`
        //      stream (with offset-dedup + remount ring-replay as the recovery
        //      path). The periodic idle reconciliation paint was retired in
        //      Phase 2 — see `bootstrapPaint` above for why.
        //   3. After waiting for the container to have non-zero dims, fit
        //      + terminal_resize + brief 50ms wait for SIGWINCH, then fetch
        //      the GridSnapshot for the one-shot mount-gap bootstrap paint
        //      (alt-screen-guarded via bootstrapPaint).
        // Replay the Rust-side scrollback ring BEFORE any live/pending bytes
        // land. TerminalInstance remounts whenever the zone layout reshapes
        // (maximize / single-view toggle, layout switch, hidden↔assigned
        // reclassification) and on page reload — each remount discards the
        // previous xterm buffer, and the grid bootstrap below restores only
        // the visible rows×cols screen. Without this replay a remounted pane
        // has an EMPTY scrollback (wheel / Shift+PgUp scroll nothing) until
        // fresh output accumulates. The ring is written at the backend's
        // default size; the bootstrap fit below reflows it to the real
        // viewport, same as the disk-based session-restore path.
        try {
          const ring = await invoke<{
            success: boolean;
            data: { data: string; startOffset: number; endOffset: number } | null;
          }>("terminal_get_scrollback", { terminalId });
          if (disposed) return;
          if (ring.success && ring.data && ring.data.data) {
            const rawRing = atob(ring.data.data);
            const ringBytes = new Uint8Array(rawRing.length);
            for (let i = 0; i < rawRing.length; i++) {
              ringBytes[i] = rawRing.charCodeAt(i);
            }
            backend.write(ringBytes);
            replayedThrough = ring.data.endOffset;
            writtenThrough = Math.max(writtenThrough, ring.data.endOffset);
          }
        } catch (e) {
          // Best-effort: a failed replay degrades to the pre-fix behavior
          // (screen-only bootstrap), never blocks the live stream.
          console.warn(`[Terminal ${terminalId}] scrollback replay failed:`, e);
        }

        for (const buffered of pendingBytes) {
          // Drop/trim the part of each buffered chunk the ring replay above
          // already wrote (ack bookkeeping stays on the full chunk).
          const slice = trimReplayedChunk(buffered, replayedThrough);
          if (slice) {
            try {
              backend.write(slice);
            } catch (e) {
              console.error(`[Terminal ${terminalId}] drain write error:`, e);
            }
          }
          if (buffered.offset !== undefined) {
            writtenThrough = Math.max(writtenThrough, buffered.offset + buffered.bytes.length);
          }
          bytesReceivedRef.current += buffered.bytes.length;
        }
        pendingBytes = [];
        backendReady = true;

        // Wait for the container to have non-zero dimensions before
        // fitting. On fast initial mount the layout is already done; on
        // slow mount (off-screen tab, second pane in a not-yet-laid-out
        // grid) we need to wait. Without this guard, fitAddon computes
        // ~10x5 from a 0x0 box and the subsequent terminal_resize
        // SIGWINCHes Claude into a 10x5 redraw — the Rust grid is then
        // destructively resized and stays at 10x5 forever.
        const waitForLayout = async (): Promise<boolean> => {
          const c = containerRef.current;
          if (!c) return false;
          if (c.clientWidth > 0 && c.clientHeight > 0) return true;
          return new Promise<boolean>((resolve) => {
            const ro = new ResizeObserver(() => {
              if (disposed) {
                ro.disconnect();
                resolve(false);
                return;
              }
              const cc = containerRef.current;
              if (cc && cc.clientWidth > 0 && cc.clientHeight > 0) {
                ro.disconnect();
                resolve(true);
              }
            });
            ro.observe(c);
            // Safety timeout — give up after 1s and try anyway.
            setTimeout(() => {
              ro.disconnect();
              resolve(true);
            }, 1000);
          });
        };

        // Wait one paint frame so backend.cols/rows are valid after open().
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        if (disposed) return;
        const laidOut = await waitForLayout();
        if (disposed) return;

        try {
          if (laidOut) {
            backend.fit();
            // Sanity check: refuse to ship a tiny resize that would wipe
            // the grid. Claude's TUI requires a non-trivial viewport;
            // anything below this is almost certainly a measurement error.
            if (backend.cols >= 20 && backend.rows >= 5) {
              await invoke("terminal_resize", {
                terminalId,
                cols: backend.cols,
                rows: backend.rows,
              });
              // Brief wait for SIGWINCH to propagate to the child process.
              await new Promise<void>((resolve) => setTimeout(resolve, 50));
            } else {
              console.warn(
                `[Terminal ${terminalId}] skipping bootstrap resize: too small (${backend.cols}x${backend.rows})`,
              );
            }
          }
          if (disposed) return;
          const result = await invoke<{ success: boolean; data: GridSnapshot | null }>(
            "terminal_get_grid",
            { terminalId },
          );
          if (result.success && result.data && !disposed) {
            bootstrapPaint(result.data, isReconnecting ? "reconnect-bootstrap" : "mount-bootstrap");
          }
        } catch (err) {
          console.warn(`[Terminal ${terminalId}] grid bootstrap failed:`, err);
        }
        if (disposed) return;

        // Layer 4 polish: on the reconnect path the live `terminal-output`
        // listener was deferred so it wouldn't race with the bootstrap
        // paintGrid above. Attach it NOW that the grid has landed. The
        // pendingBytes drain below is a no-op since nothing buffered while
        // the listener wasn't attached, but the structure stays uniform.
        if (isReconnecting && !outputUnsub) {
          outputUnsub = registerTerminalOutputHandler(terminalId, handleOutputPayload);
        }
        if (disposed) return;

        // (Phase 2) The periodic idle re-paint heartbeat that used to be
        // primed here was retired — forward motion is owned by the live
        // `terminal-output` stream. The one-shot bootstrap paint above is the
        // only paintGrid that runs now.

        // Reconnect callback fires only on actual reconnects (not first
        // mount), preserving the prior semantic.
        if (isReconnecting) {
          onReconnectedRef.current?.();
        }

        // Process-exit handler — demuxed alongside `terminal-output`, so a
        // window installs one `terminal-exit` listener no matter how many
        // panes are mounted. Runs in every visibility tier: a hidden pane
        // must still record the exit and release its scrollback.
        exitUnsub = registerTerminalExitHandler(terminalId, (payload) => {
          backend.write(
            `\r\n\x1b[90m[Process exited with code ${payload.exitCode ?? "unknown"}]\x1b[0m\r\n`,
          );
          // Release the memory held by this now-dead pane. It will never emit
          // more output, so trim its scrollback to the tail — this is the
          // accumulator behind the WebView2 "out of memory" crash when many
          // finished sessions stay mounted. See DEAD_TERMINAL_SCROLLBACK.
          backend.setScrollback(DEAD_TERMINAL_SCROLLBACK);
          onExitRef.current?.(payload.exitCode ?? null);
        });
        if (disposed) return;

        // Backend is live: the visibility tier may now run its services (the
        // ResizeObserver + the ack-floor/title-poll timer). A pane that
        // mounted hidden starts neither, and resyncs from the ring when it is
        // first revealed.
        paneService.markReady();
        // Seed the WebGL LRU with this pane's real visibility — the backend
        // claims its slot as visible (it has no visibility concept), and the
        // `visible` effect below ran before the backend existed, so a pane
        // that mounted hidden would otherwise outrank a watched one.
        setWebglSlotVisible(terminalId, visibleRef.current);

        // OSC 633 shell integration handler
        disposables.push(
          backend.onOsc633((data) => {
            const cb = onShellIntegrationRef.current;
            if (!cb) return;
            if (data === "A") cb({ type: "prompt_start" });
            else if (data === "B") cb({ type: "command_ready" });
            else if (data === "C") cb({ type: "command_execute" });
            else if (data.startsWith("D")) {
              const code = parseInt(data.slice(2), 10);
              cb({ type: "command_done", exitCode: isNaN(code) ? 0 : code });
            } else if (data.startsWith("E;")) {
              cb({ type: "command_line", command: data.slice(2).replace(/\\x3b/g, ";") });
            } else if (data.startsWith("P;Cwd=")) {
              cb({ type: "cwd", path: data.slice(6) });
            }
          }),
        );
      })();

      // Cleanup
      return () => {
        disposed = true;
        // Flush any microtask-staged coalesced write synchronously so trailing
        // bytes reach the (about-to-be-disposed) backend rather than being
        // dropped. Harmless if nothing is staged.
        if (flushScheduled) flushCoalesced();
        // Stops the ack timer + observer and hands acking back to the page tap.
        paneService.dispose();
        if (paneServiceRef.current === paneService) paneServiceRef.current = null;
        if (fitTimerRef.current) clearTimeout(fitTimerRef.current);
        if (wheelScrollOverride) {
          container.removeEventListener("wheel", wheelScrollOverride, { capture: true });
        }
        if (contextMenuCopyPaste) {
          container.removeEventListener("contextmenu", contextMenuCopyPaste);
        }
        for (const d of disposables) d.dispose();
        const inputEl = backendRef.current?.getInputElement();
        if (inputEl && blockNativePaste) {
          inputEl.removeEventListener("paste", blockNativePaste, true);
        }
        uiBridge?.registry?.unregisterElement(`terminal-input-${terminalId}`);
        outputUnsub?.();
        exitUnsub?.();
        // Hand consumption over to the per-page tap: this pane's render-acks
        // strand on dispose (the final write's completion callback never
        // fires), and if the emission gate is paused right now the tab would
        // enter a permanent emission blackout — the tap only proxy-acks
        // bytes it RECEIVES, so it could never reopen the gate on its own.
        // Resetting is safe: the unrendered backlog is being discarded with
        // the xterm buffer anyway.
        invoke("terminal_flow_reset", { terminalId }).catch(() => {});
        try {
          backendRef.current?.dispose();
        } catch {
          // xterm.js WebGL renderer may throw during disposal when its internal
          // options are already cleaned up (known issue with onShowLinkUnderline).
        }
        backendRef.current = null;
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps -- uiBridge is stable context
    }, [terminalId, backendType, fitTerminal]);

    /**
     * Drive the visibility tier (Phase 2 / A4). Hidden ⇒ the pane stops its
     * timers, its ResizeObserver and its xterm writes, and the page tap takes
     * over flow-control acks; visible ⇒ they restart, the pane re-fits +
     * focuses, and the ring replays whatever it missed.
     *
     * Deliberately keyed on `visible` alone, NOT on the flow-mode
     * classification: `classifyTabs` marks every tab of a preset layout
     * "assigned" (all mounted) and never consults viewport proximity, so a
     * flow-mode-only tier would leave preset layouts — maximized single view,
     * compact zone cards, `HiddenTerminal`'s off-grid parking — paying full
     * cost for panes nobody can see.
     *
     * The WebGL LRU is told too: a hidden pane is the first candidate to be
     * downgraded to Canvas when another pane needs a context, and a revealed
     * one is promoted to most-recently-used. No-op on backends that hold no
     * WebGL context (ghostty is Canvas 2D).
     */
    useEffect(() => {
      paneServiceRef.current?.setVisible(visible);
      setWebglSlotVisible(terminalId, visible);
    }, [visible, terminalId]);

    return (
      // Wrapper exists so the find bar can overlay as a React-managed
      // sibling: xterm imperatively appends its own DOM into the inner
      // container div, and mixing React children with foreign nodes in the
      // same parent invites reconciliation surprises.
      <div className={`relative h-full w-full ${visible ? "" : "hidden"}`}>
        <div ref={containerRef} className="h-full w-full" style={{ padding: "4px 0 0 4px" }} />
        {findOpen && (
          <TerminalFindBar
            key={findFocusSeq}
            query={findQuery}
            onQueryChange={handleFindQueryChange}
            resultIndex={findResults.resultIndex}
            resultCount={findResults.resultCount}
            caseSensitive={findOpts.caseSensitive}
            wholeWord={findOpts.wholeWord}
            regex={findOpts.regex}
            onToggleCase={() => toggleFindOpt("caseSensitive")}
            onToggleWholeWord={() => toggleFindOpt("wholeWord")}
            onToggleRegex={() => toggleFindOpt("regex")}
            onNext={() => runFind("next")}
            onPrev={() => runFind("prev")}
            onClose={closeFind}
          />
        )}
      </div>
    );
  },
);

export const TerminalInstance = memo(TerminalInstanceInner);
