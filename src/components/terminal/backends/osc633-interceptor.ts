/**
 * OSC 633 escape sequence interceptor.
 *
 * Scans a stream of terminal output bytes for OSC 633 shell integration sequences
 * (\x1b]633;...\x07 or \x1b]633;...\x1b\\) and emits the payload when found.
 *
 * This is needed for terminal backends (like ghostty-web) that parse OSC sequences
 * internally via WASM but don't expose a JS-side hook to intercept them.
 *
 * The interceptor does NOT modify the data stream — all bytes pass through unchanged.
 */

const ESC = 0x1b; // \x1b
const RBRACKET = 0x5d; // ]
const BEL = 0x07; // \x07
const BACKSLASH = 0x5c; // \\

/** Prefix bytes: \x1b]633; */
const OSC_633_PREFIX = [ESC, RBRACKET, 0x36, 0x33, 0x33, 0x3b]; // \x1b]633;

const enum State {
  /** Scanning for the start of an OSC 633 sequence. */
  Normal,
  /** Matched some prefix bytes, checking if the rest follows. */
  MatchingPrefix,
  /** Inside the OSC 633 payload, accumulating until terminator. */
  InPayload,
  /** Saw \x1b inside payload — next byte decides if it's ST (\x1b\\) or not. */
  InPayloadEsc,
}

/** Maximum payload size before we give up (protects against malformed streams). */
const MAX_PAYLOAD_BYTES = 8192;

/** Timeout (ms) for partial sequences — if no terminator arrives, reset. */
const PARTIAL_TIMEOUT_MS = 5000;

export class Osc633Interceptor {
  private state = State.Normal;
  private prefixIndex = 0;
  private payloadBytes: number[] = [];
  private partialTimer: ReturnType<typeof setTimeout> | null = null;
  private listener: ((payload: string) => void) | null = null;

  /**
   * Register a callback for OSC 633 payloads.
   * Returns a disposable to unsubscribe.
   */
  onPayload(cb: (payload: string) => void): { dispose(): void } {
    if (this.listener) {
      throw new Error(
        "Osc633Interceptor: only one listener supported. Dispose the existing one first.",
      );
    }
    this.listener = cb;
    return {
      dispose: () => {
        if (this.listener === cb) this.listener = null;
      },
    };
  }

  /**
   * Feed terminal output data through the interceptor.
   * Call this BEFORE writing to the terminal backend.
   * The data is never modified — this is read-only scanning.
   */
  scan(data: Uint8Array | string): void {
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;

    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];

      switch (this.state) {
        case State.Normal:
          if (b === OSC_633_PREFIX[0]) {
            this.prefixIndex = 1;
            this.state = State.MatchingPrefix;
          }
          break;

        case State.MatchingPrefix:
          if (b === OSC_633_PREFIX[this.prefixIndex]) {
            this.prefixIndex++;
            if (this.prefixIndex === OSC_633_PREFIX.length) {
              // Full prefix matched — now accumulate payload
              this.state = State.InPayload;
              this.payloadBytes = [];
              this.startPartialTimer();
            }
          } else {
            // Mismatch — check if this byte starts a new sequence
            this.reset();
            if (b === OSC_633_PREFIX[0]) {
              this.prefixIndex = 1;
              this.state = State.MatchingPrefix;
            }
          }
          break;

        case State.InPayload:
          if (b === BEL) {
            // BEL terminator — emit payload
            this.emit();
          } else if (b === ESC) {
            // Possible ST terminator (\x1b\\)
            this.state = State.InPayloadEsc;
          } else {
            this.payloadBytes.push(b);
            if (this.payloadBytes.length > MAX_PAYLOAD_BYTES) {
              // Safety valve — discard malformed sequence
              this.reset();
            }
          }
          break;

        case State.InPayloadEsc:
          if (b === BACKSLASH) {
            // ST terminator (\x1b\\) — emit payload
            this.emit();
          } else {
            // False alarm — the ESC was part of the payload
            this.payloadBytes.push(ESC);
            this.payloadBytes.push(b);
            this.state = State.InPayload;
            if (this.payloadBytes.length > MAX_PAYLOAD_BYTES) {
              this.reset();
            }
          }
          break;
      }
    }
  }

  /** Clean up timers. Call when the terminal is disposed. */
  dispose(): void {
    this.clearPartialTimer();
    this.listener = null;
  }

  private emit(): void {
    this.clearPartialTimer();
    if (this.listener) {
      const payload = new TextDecoder().decode(new Uint8Array(this.payloadBytes));
      this.listener(payload);
    }
    this.state = State.Normal;
    this.prefixIndex = 0;
    this.payloadBytes = [];
  }

  private reset(): void {
    this.clearPartialTimer();
    this.state = State.Normal;
    this.prefixIndex = 0;
    this.payloadBytes = [];
  }

  private startPartialTimer(): void {
    this.clearPartialTimer();
    this.partialTimer = setTimeout(() => {
      // Partial sequence timed out — discard and reset
      this.reset();
    }, PARTIAL_TIMEOUT_MS);
  }

  private clearPartialTimer(): void {
    if (this.partialTimer !== null) {
      clearTimeout(this.partialTimer);
      this.partialTimer = null;
    }
  }
}
