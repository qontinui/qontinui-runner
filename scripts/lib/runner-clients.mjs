/**
 * Shared HTTP clients for the dev supervisor (`:9875`) and for a runner under
 * test — the two classes `scripts/perf-harness.mjs` defines privately at
 * `SupervisorClient` / `RunnerClient`, lifted here so a second harness
 * (`scripts/termhost-bench/`, plan `2026-10-11-terminal-page-performance-kpis-across-setups`
 * Phase 3) imports them instead of copying them.
 *
 * ## Alignment with perf-harness and runner#1856
 *
 * The plan moves perf-harness's two classes here, but that move edits
 * `perf-harness.mjs`, which runner#1856 (open, held) rewrites, so it is
 * sequenced behind #1856. Until then perf-harness keeps its private copies and
 * this module is their SUPERSET:
 *
 *   - every method perf-harness calls exists here with the same name, the same
 *     arguments, the same HTTP request and the same return value — including
 *     the three #1856 adds to `RunnerClient` (`resetTransportStats`,
 *     `transportStats`, `gridAltScreen`, taken from #1856's head `db0fea935`);
 *   - `httpJson` / `unwrap` / `sleep` behave identically, error messages
 *     included. The one widening: a failed request throws an {@link HttpError}
 *     (a subclass of `Error`, same message) that also carries `status` and the
 *     parsed `body`, so a caller can branch on a refusal without parsing prose;
 *   - both constructors keep their `(baseUrl, log)` positional form; an optional
 *     third `opts` argument only adds `fetch` injection for tests.
 *
 * So the later switch is a deletion in perf-harness (its two classes plus
 * `httpJson` / `unwrap` and the `PRIMARY_RUNNER_PORT` constant) and one import
 * line — no reconciliation. Everything below the "termhost-bench additions"
 * markers is additive and perf-harness does not need it.
 *
 * ## The primary is never driven
 *
 * `RunnerClient` refuses port 9876 at construction: the primary runner is
 * neither driven, restarted nor rebuilt by a harness (served policy
 * `production-and-cost` `runner-lifecycle`). `SupervisorClient` only ever
 * addresses temp runners: `spawn-test` cannot target the primary, and
 * `stopRunner` is given the temp id `spawn-test` returned.
 *
 * Node built-ins only (global `fetch`, Node >= 18).
 */

/** The primary runner's port. Never driven by a harness. */
export const PRIMARY_RUNNER_PORT = 9876;

/** @param {number} ms */
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A sleep that ends early when `signal` aborts (and clears its timer). */
export function abortableSleep(ms, signal = null) {
  if (!signal) return sleep(ms);
  return new Promise((r) => {
    if (signal.aborted) return setImmediate(r);
    const done = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      r();
    };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
  });
}

/** A wait the run's abort signal ended. */
export class WaitAborted extends Error {
  constructor(what) {
    super(`interrupted while waiting for ${what}`);
    this.name = "WaitAborted";
  }
}

/** The supervisor does not list the runner: it is already gone, nothing was stopped. */
export class RunnerNotListed extends Error {
  constructor(id) {
    super(`runner ${id} is not listed by the supervisor; nothing was stopped`);
    this.name = "RunnerNotListed";
    this.id = id;
  }
}

/**
 * A non-2xx answer or a non-JSON body. The message is exactly what
 * perf-harness's private `httpJson` threw; `status` and `body` are added so a
 * caller can tell a 404 from a 409 from a refusal envelope without parsing it.
 */
export class HttpError extends Error {
  /**
   * @param {string} message
   * @param {{status: number, body: unknown, method: string, url: string}} info
   */
  constructor(message, { status, body, method, url }) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.body = body;
    this.method = method;
    this.url = url;
  }
}

/**
 * JSON HTTP call with a timeout and a readable failure.
 *
 * @param {string} url
 * @param {{method?:string, body?:unknown, timeoutMs?:number, fetch?: typeof fetch}} [options]
 * @returns {Promise<unknown>}
 */
export async function httpJson(url, options = {}) {
  const { method = "GET", body, timeoutMs = 30000, fetch: fetchImpl = globalThis.fetch } = options;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetchImpl(url, {
      method,
      signal: controller.signal,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let parsed;
    try {
      parsed = text ? JSON.parse(text) : null;
    } catch {
      throw new HttpError(`${method} ${url} -> ${res.status}, non-JSON body: ${text.slice(0, 400)}`, {
        status: res.status,
        body: text,
        method,
        url,
      });
    }
    if (!res.ok) {
      throw new HttpError(
        `${method} ${url} -> ${res.status} ${JSON.stringify(parsed).slice(0, 600)}`,
        { status: res.status, body: parsed, method, url },
      );
    }
    return parsed;
  } finally {
    clearTimeout(timer);
  }
}

/** Unwrap the runner's `{success, data}` envelope. */
export function unwrap(payload) {
  if (payload && typeof payload === "object" && "data" in payload && "success" in payload) {
    if (payload.success === false) throw new Error(`runner API error: ${JSON.stringify(payload)}`);
    return payload.data;
  }
  return payload;
}

/** The supervisor's terminal build states (`build_submissions.rs` `BuildStatus`). */
export const TERMINAL_BUILD_STATES = Object.freeze(["succeeded", "failed"]);

/** Supervisor client — spawn-test and stop only. Never touches the primary. */
export class SupervisorClient {
  /**
   * @param {string} baseUrl
   * @param {(line: string) => void} log
   * @param {{fetch?: typeof fetch}} [opts]
   */
  constructor(baseUrl, log, opts = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.log = log;
    this.fetch = opts.fetch;
  }

  /** @private */
  http(url, options = {}) {
    return httpJson(url, this.fetch ? { ...options, fetch: this.fetch } : options);
  }

  async health() {
    return this.http(`${this.baseUrl}/health`, { timeoutMs: 20000 });
  }

  /**
   * `POST /runners/spawn-test`. Temp runners land on 9877+; the primary is
   * untouched by construction — this endpoint cannot address it.
   *
   * @param {object} body
   * @param {number} timeoutMs
   */
  async spawnTest(body, timeoutMs) {
    this.log(`supervisor: POST /runners/spawn-test ${JSON.stringify(body)}`);
    return this.http(`${this.baseUrl}/runners/spawn-test`, {
      method: "POST",
      body,
      timeoutMs,
    });
  }

  /**
   * @param {string} id temp-runner id returned by spawn-test
   */
  async stopRunner(id) {
    this.log(`supervisor: POST /runners/${id}/stop`);
    return this.http(`${this.baseUrl}/runners/${encodeURIComponent(id)}/stop`, {
      method: "POST",
      body: {},
      timeoutMs: 60000,
    });
  }

  // -------------------------------------------------------------------------
  // termhost-bench additions (perf-harness does not use these)
  // -------------------------------------------------------------------------

  /** `GET /runners` — the supervisor's runner rows (`id`, `port`, `pid`, `protected`, `requester_id`, …). */
  async runners() {
    const rows = await this.http(`${this.baseUrl}/runners`, { timeoutMs: 20000 });
    return Array.isArray(rows) ? rows : Array.isArray(rows?.runners) ? rows.runners : [];
  }

  /**
   * Stop a temp runner only after reading its row back: it must still be listed,
   * never `protected`, and — when `requesterId` is given — owned by that requester.
   * `stopRunner` trusts the id it is given; this is the defence in depth a harness
   * that runs on an operator's box wants before it stops anything.
   *
   * @param {string} id
   * @param {{requesterId?: string}} [opts]
   */
  async stopRunnerChecked(id, { requesterId } = {}) {
    const row = (await this.runners()).find((r) => r?.id === id);
    if (!row) throw new RunnerNotListed(id);
    if (row.protected === true) throw new Error(`runner ${id} is protected; it was not stopped`);
    if (requesterId && row.requester_id && row.requester_id !== requesterId) {
      throw new Error(`runner ${id} belongs to requester ${row.requester_id}, not ${requesterId}; it was not stopped`);
    }
    return this.stopRunner(id);
  }

  /**
   * `POST /runners/spawn-test` with `async: true`: the supervisor answers `202
   * {submission_id, id, port, poll_url, …}` at once and builds in the
   * background (qontinui-supervisor `src/routes/runners.rs`, the `async` field
   * of `SpawnTestRequest`), so a 10-30 minute cold build never rides one HTTP
   * connection a supervisor restart would cut.
   *
   * @param {object} body the spawn-test body; `async` is forced true
   * @returns {Promise<{submission_id: string, id: string, port: number}>}
   */
  async spawnTestAsync(body) {
    const res = await this.spawnTest({ ...body, async: true }, 60000);
    if (!res || typeof res !== "object" || typeof res.submission_id !== "string") {
      throw new Error(`spawn-test (async) answered without a submission_id: ${JSON.stringify(res)}`);
    }
    return res;
  }

  /**
   * `GET /build/{id}/status` — the build submission, whose `spawn` field holds
   * `{http_status, port, body}` once the spawn reached a terminal state.
   *
   * @param {string} submissionId
   */
  async buildStatus(submissionId) {
    return this.http(`${this.baseUrl}/build/${encodeURIComponent(submissionId)}/status`, {
      timeoutMs: 20000,
    });
  }

  /**
   * `GET /runners/spawn-test/in-flight` — the runner id and port reserved for
   * every build still running; the recovery path for a lost spawn answer.
   *
   * @param {{requesterId?: string}} [opts]
   */
  async spawnInFlight({ requesterId } = {}) {
    const q = requesterId ? `?requester_id=${encodeURIComponent(requesterId)}` : "";
    return this.http(`${this.baseUrl}/runners/spawn-test/in-flight${q}`, { timeoutMs: 20000 });
  }

  /**
   * Poll a spawn submission until its `spawn` outcome is known.
   *
   * Resolves with `{httpStatus, port, body}` — the body the synchronous
   * spawn-test would have answered, success or failure: the caller judges it
   * (a 503 `frontend_stale`, a 500 build failure). Rejects only when no answer
   * could be obtained before `timeoutMs`, or the submission vanished.
   *
   * @param {string} submissionId
   * @param {{timeoutMs?: number, pollMs?: number, sleepFn?: (ms: number) => Promise<void>,
   *   now?: () => number}} [opts]
   */
  async awaitSpawn(
    submissionId,
    { timeoutMs = 45 * 60_000, pollMs = 5000, signal = null, sleepFn = (ms) => abortableSleep(ms, signal), now = Date.now } = {},
  ) {
    const deadline = now() + timeoutMs;
    let lastError = null;
    let lastState = "unknown";
    while (now() < deadline) {
      if (signal?.aborted) throw new WaitAborted(`spawn submission ${submissionId} (last state ${lastState})`);
      let sub = null;
      try {
        sub = await this.buildStatus(submissionId);
      } catch (e) {
        if (e instanceof HttpError && e.status === 404) {
          throw new Error(
            `spawn submission ${submissionId} is unknown to the supervisor (evicted, or the supervisor restarted); ` +
              `recover the reserved runner from GET /runners/spawn-test/in-flight`,
          );
        }
        lastError = e;
      }
      if (sub && typeof sub === "object") {
        lastState = sub.status?.state ?? lastState;
        if (sub.spawn && typeof sub.spawn === "object") {
          return {
            httpStatus: sub.spawn.http_status,
            port: sub.spawn.port,
            body: sub.spawn.body,
            status: sub.status,
          };
        }
        if (lastState === "failed") {
          // A failed build whose spawn outcome is not recorded (yet): the body
          // the synchronous path would answer is the build error.
          return {
            httpStatus: 500,
            port: null,
            body: { error: "build_failed", detail: sub.status?.error ?? "build failed" },
            status: sub.status,
          };
        }
      }
      await sleepFn(pollMs);
    }
    throw new Error(
      `spawn submission ${submissionId} reached no spawn outcome within ${Math.round(timeoutMs / 1000)}s ` +
        `(last state ${lastState}${lastError ? `; last error ${lastError.message}` : ""})`,
    );
  }
}

/** Temp-runner client: terminals API, log-sink resolution and the UI Bridge. */
export class RunnerClient {
  /**
   * @param {string} baseUrl
   * @param {(line: string) => void} log
   * @param {{fetch?: typeof fetch}} [opts]
   */
  constructor(baseUrl, log, opts = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    const port = Number.parseInt(new URL(this.baseUrl).port || "0", 10);
    if (port === PRIMARY_RUNNER_PORT) {
      throw new Error(
        `refusing to drive port ${PRIMARY_RUNNER_PORT}: that is the primary runner. ` +
          `Spawn a temp runner via the supervisor (the harness does this by default).`,
      );
    }
    this.log = log;
    this.fetch = opts.fetch;
  }

  /** @private */
  http(url, options = {}) {
    return httpJson(url, this.fetch ? { ...options, fetch: this.fetch } : options);
  }

  async health() {
    return this.http(`${this.baseUrl}/health`, { timeoutMs: 20000 });
  }

  /** Wait for `/health` to answer, polling until `timeoutMs` elapses. */
  async waitHealthy(timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    let lastError = null;
    while (Date.now() < deadline) {
      try {
        const h = await this.health();
        if (h) return h;
      } catch (e) {
        lastError = e;
      }
      await sleep(1000);
    }
    throw new Error(`runner ${this.baseUrl} never became healthy: ${lastError?.message ?? "?"}`);
  }

  async logSink() {
    return unwrap(await this.http(`${this.baseUrl}/log-sources/runner-log-sink`, { timeoutMs: 20000 }));
  }

  async listTerminals() {
    const data = unwrap(await this.http(`${this.baseUrl}/terminals`, { timeoutMs: 30000 }));
    if (Array.isArray(data)) return data;
    if (Array.isArray(data?.terminals)) return data.terminals;
    if (Array.isArray(data?.sessions)) return data.sessions;
    return [];
  }

  /** @returns {Promise<{info:object, wallMs:number}>} */
  async createTerminal(body, timeoutMs) {
    const t0 = performance.now();
    const data = unwrap(
      await this.http(`${this.baseUrl}/terminals`, { method: "POST", body, timeoutMs }),
    );
    return { info: data, wallMs: performance.now() - t0 };
  }

  async writeTerminal(id, text) {
    return this.http(`${this.baseUrl}/terminals/${encodeURIComponent(id)}/write`, {
      method: "POST",
      body: { data: Buffer.from(text, "utf8").toString("base64") },
      timeoutMs: 20000,
    });
  }

  async closeTerminal(id) {
    return this.http(`${this.baseUrl}/terminals/${encodeURIComponent(id)}`, {
      method: "DELETE",
      timeoutMs: 30000,
    });
  }

  /** UI Bridge page evaluation — the ONLY frontend measurement channel here. */
  async evaluate(expression, timeoutMs = 30000) {
    const payload = await this.http(`${this.baseUrl}/ui-bridge/control/page/evaluate`, {
      method: "POST",
      body: { expression, unwrap: true, timeoutMs: Math.max(1000, timeoutMs - 2000) },
      timeoutMs,
    });
    const data = unwrap(payload);
    // `evaluate` answers `{type, value}` when it can classify the result.
    if (data && typeof data === "object" && "value" in data && "type" in data) return data.value;
    return data;
  }

  /** Zero the runner's transport counters. (runner#1856) */
  async resetTransportStats() {
    return this.http(`${this.baseUrl}/terminals/transport-stats`, {
      method: "POST",
      body: {},
      timeoutMs: 20000,
    });
  }

  /** Raw `GET /terminals/transport-stats` body (envelope or bare). (runner#1856) */
  async transportStats() {
    return this.http(`${this.baseUrl}/terminals/transport-stats`, { timeoutMs: 20000 });
  }

  /** The runner grid's `alt_screen` flag for one terminal. (runner#1856) */
  async gridAltScreen(id) {
    const data = unwrap(
      await this.http(
        `${this.baseUrl}/ui-bridge/sdk/terminal/sessions/${encodeURIComponent(id)}/grid`,
        { timeoutMs: 20000 },
      ),
    );
    return typeof data?.alt_screen === "boolean" ? data.alt_screen : null;
  }

  async activateTab(tabId) {
    return this.http(`${this.baseUrl}/ui-bridge/control/tab/activate`, {
      method: "POST",
      body: { tabId },
      timeoutMs: 20000,
    });
  }

  // -------------------------------------------------------------------------
  // termhost-bench additions (perf-harness does not use these)
  // -------------------------------------------------------------------------

  /**
   * `POST /ui-bridge/control/component/{id}/action/{action}` (runner
   * `mcp/ui_bridge/elements.rs`, `ui_bridge_execute_component_action_handler`).
   * The action's return value, unwrapped. A frontend refusal arrives as an
   * {@link HttpError} (4xx with `{success: false, error}`).
   *
   * @param {string} componentId e.g. `terminal-launch-menu`
   * @param {string} actionId e.g. `create-with-command`
   * @param {object} [params]
   * @param {number} [timeoutMs]
   */
  async componentAction(componentId, actionId, params = {}, timeoutMs = 60000) {
    return unwrap(
      await this.http(
        `${this.baseUrl}/ui-bridge/control/component/${encodeURIComponent(componentId)}/action/${encodeURIComponent(actionId)}`,
        { method: "POST", body: { params }, timeoutMs },
      ),
    );
  }

  /**
   * `POST /ui-bridge/control/element/{id}/action` (`elements.rs`,
   * `ui_bridge_execute_action_handler`) — e.g. `focus` on `terminal-input-<id>`.
   *
   * @param {string} elementId
   * @param {string} action
   * @param {object} [params]
   * @param {number} [timeoutMs]
   */
  async elementAction(elementId, action, params = undefined, timeoutMs = 20000) {
    const body = params === undefined ? { action } : { action, params };
    return unwrap(
      await this.http(
        `${this.baseUrl}/ui-bridge/control/element/${encodeURIComponent(elementId)}/action`,
        { method: "POST", body, timeoutMs },
      ),
    );
  }

  /** `GET /ui-bridge/control/element/{id}` — one registered element, with its bbox. */
  async getElement(elementId) {
    return unwrap(
      await this.http(`${this.baseUrl}/ui-bridge/control/element/${encodeURIComponent(elementId)}`, {
        timeoutMs: 20000,
      }),
    );
  }

  /** `POST /ui-bridge/control/discover` — the registered and auto-discovered elements. */
  async discover(body = { force: true }) {
    return unwrap(
      await this.http(`${this.baseUrl}/ui-bridge/control/discover`, {
        method: "POST",
        body,
        timeoutMs: 30000,
      }),
    );
  }

  /** `GET /ui-bridge/control/elements` — cheap; drives a UI Bridge IPC round trip. */
  async uiBridgeElements() {
    return this.http(`${this.baseUrl}/ui-bridge/control/elements`, { timeoutMs: 10000 });
  }

  /**
   * `GET /ui-bridge/sdk/terminal/sessions/{id}/text` — the server-side grid's
   * text (`mcp/sdk_terminal_buffer.rs` `handle_terminal_text`). Unlike
   * `GET /terminals/{id}/buffer` it does not reset flow control, so it is the
   * post-soak fidelity read.
   */
  async terminalText(id) {
    return unwrap(
      await this.http(
        `${this.baseUrl}/ui-bridge/sdk/terminal/sessions/${encodeURIComponent(id)}/text`,
        { timeoutMs: 20000 },
      ),
    );
  }

  /** `GET /terminals/{id}/coord-session` → `{terminal_id, coord_session_id | null}`. */
  async coordSession(id) {
    return unwrap(
      await this.http(`${this.baseUrl}/terminals/${encodeURIComponent(id)}/coord-session`, {
        timeoutMs: 20000,
      }),
    );
  }

  /** `GET /restart-readiness` — carries coord's device drain as `drain.coordDrain`. */
  async restartReadiness() {
    return unwrap(await this.http(`${this.baseUrl}/restart-readiness`, { timeoutMs: 30000 }));
  }

  /**
   * Wait until a UI Bridge IPC round trip has completed — `/health`
   * `uiBridgeIpcObserved: true` — poking `GET /ui-bridge/control/elements`
   * once the HTTP shell is responsive, because a passive poll never drives the
   * round trip. The readiness rule of `scripts/contract-smoke.ps1`
   * `Wait-DirectRunnerReady`, ported.
   *
   * @param {number} timeoutMs
   * @param {{alive?: () => boolean, sleepFn?: (ms: number) => Promise<void>, now?: () => number}} [opts]
   *   `alive` returns false once the launched process has exited (fail fast)
   * @returns {Promise<object>} the `/health` data that showed readiness
   */
  async waitUiBridgeReady(timeoutMs, { alive = () => true, signal = null, sleepFn = (ms) => abortableSleep(ms, signal), now = Date.now } = {}) {
    const deadline = now() + timeoutMs;
    let last = "no answer yet";
    while (now() < deadline) {
      if (signal?.aborted) throw new WaitAborted(`runner ${this.baseUrl} to become ready (${last})`);
      if (!alive()) throw new Error(`runner ${this.baseUrl} exited before becoming ready (${last})`);
      let responsive = false;
      try {
        const data = unwrap(await this.health());
        if (data && typeof data === "object") {
          if (data.uiBridgeIpcObserved === true) return data;
          responsive = data.responsive === true;
          last = `uiBridgeIpcObserved=${data.uiBridgeIpcObserved}, responsive=${data.responsive}`;
        }
      } catch (e) {
        last = e.message;
      }
      if (responsive) {
        try {
          await this.uiBridgeElements();
        } catch {
          // a non-2xx still drove the IPC round trip, which is all the poke is for
        }
      }
      await sleepFn(1000);
    }
    throw new Error(
      `runner ${this.baseUrl} did not report uiBridgeIpcObserved:true within ${Math.round(timeoutMs / 1000)}s (${last})`,
    );
  }
}
