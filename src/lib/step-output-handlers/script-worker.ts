/**
 * Script Worker
 *
 * Executes a small JS expression in a sandboxed `node:vm` context with a
 * frozen minimal global surface.  This is the "think-in-code" escape valve:
 * rather than pipe many kilobytes of raw step output back to the LLM, we
 * let the LLM emit a short expression (referencing `output`) and ship only
 * its result back.
 *
 * Security posture:
 * - Context is built with `vm.createContext` from a frozen object.
 * - No `process`, `require`, `eval`, `Function`, `global`, `globalThis`
 *   access; they are simply not defined in the sandbox.
 * - `runInContext`'s `timeout` option enforces a hard wall-clock cap
 *   (synchronous only — JS has no way to interrupt a tight async loop,
 *   so expressions are expected to be synchronous).
 * - Return value is forced through `JSON.stringify` → `JSON.parse`; any
 *   value that is not JSON-serializable (cycles, functions, symbols) causes
 *   the worker to throw and the caller to fall back.
 */

import vm from "node:vm";

export const DEFAULT_SCRIPT_TIMEOUT_MS = 2000;

export class ScriptWorkerError extends Error {
  readonly cause?: unknown;
  constructor(message: string, cause?: unknown) {
    super(message);
    this.name = "ScriptWorkerError";
    this.cause = cause;
  }
}

/**
 * Run a JS expression against an input value.
 *
 * The expression is wrapped as `(output) => (<expr>)` so the LLM can write
 * a single-expression form like `output.slice(0, 4000)` or
 * `output.lines.filter(l => l.includes('ERROR')).slice(-10)`.
 *
 * @param expr       JS expression body, referencing `output`.
 * @param inputVar   Value to bind as `output` inside the sandbox.
 * @param timeoutMs  Hard timeout in milliseconds (default 2000).
 * @returns A JSON-cloned result value.
 * @throws {ScriptWorkerError} On compile errors, runtime throws, timeout,
 *                             or non-serializable return values.
 */
export async function runScript(
  expr: string,
  inputVar: unknown,
  timeoutMs: number = DEFAULT_SCRIPT_TIMEOUT_MS,
): Promise<unknown> {
  if (typeof expr !== "string" || expr.trim().length === 0) {
    throw new ScriptWorkerError("Script expression must be a non-empty string");
  }

  // Minimal frozen sandbox. We intentionally omit `process`, `require`,
  // `global`, `globalThis`, `Function`, `eval`, `setTimeout`, `fetch`, etc.
  // The `console.log` stub is provided as a convenience so well-behaved
  // expressions don't throw ReferenceError if they log — logs are dropped.
  const noopConsole = Object.freeze({
    log: () => {},
    info: () => {},
    warn: () => {},
    error: () => {},
    debug: () => {},
  });

  const sandbox = Object.freeze({
    output: inputVar,
    console: noopConsole,
  });

  const context = vm.createContext(sandbox, {
    codeGeneration: {
      strings: false, // disables `eval` / `new Function`
      wasm: false,
    },
  });

  // Wrap so the expression evaluates as a value and its result is returned.
  // Using IIFE form keeps the outer scope clean and yields a return value.
  const wrapped = `(function(){ return (${expr}); })()`;

  let script: vm.Script;
  try {
    script = new vm.Script(wrapped, { filename: "scripted-output.js" });
  } catch (err) {
    throw new ScriptWorkerError(`Failed to compile script: ${stringifyError(err)}`, err);
  }

  let rawResult: unknown;
  try {
    rawResult = script.runInContext(context, {
      timeout: timeoutMs,
      displayErrors: false,
      breakOnSigint: true,
    });
  } catch (err) {
    // `vm` throws a plain Error with message containing "Script execution timed out"
    // when the timeout fires.  Normalize both timeout and runtime errors here.
    throw new ScriptWorkerError(`Script execution failed: ${stringifyError(err)}`, err);
  }

  // Guarantee the value is JSON-clean: no functions, no cycles, no symbols.
  // If this throws, the caller falls back — which is the desired behavior.
  let cloned: unknown;
  try {
    const serialized = JSON.stringify(rawResult);
    if (serialized === undefined) {
      throw new Error("Script returned a non-JSON-serializable value (e.g. undefined or function)");
    }
    cloned = JSON.parse(serialized);
  } catch (err) {
    throw new ScriptWorkerError(
      `Script result is not JSON-serializable: ${stringifyError(err)}`,
      err,
    );
  }

  return cloned;
}

function stringifyError(err: unknown): string {
  if (err instanceof Error) return err.message;
  try {
    return String(err);
  } catch {
    return "<unstringifiable error>";
  }
}
