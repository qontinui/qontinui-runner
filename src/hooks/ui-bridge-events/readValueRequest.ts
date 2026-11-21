import { readValuePrimitive } from "@qontinui/ui-bridge/control";
import type { ReadValueResult } from "@qontinui/ui-bridge/control";

/**
 * The relay's `read_value` request, answered by the SDK's gated primitive.
 *
 * The runner used to carry two private copies of `readValue` that read `.value`
 * raw, so a password input or anything inside a `data-bridge-redact="true"`
 * boundary was served in cleartext. One choke point now owns the §4.6 verdict:
 * `readValuePrimitive` reads `[REDACTED]` for withheld values, never `""`.
 *
 * `null` for `index` / `all` (how the Rust handler and older callers spell
 * "absent") is normalised to `undefined` because the primitive treats any
 * non-undefined `index` beside `all: true` as a conflict.
 */
export type ReadValueOutcome =
  | { success: true; data: ReadValueResult }
  | { success: false; error: string; code?: string };

export function runReadValueRequest(params: {
  selector?: string;
  index?: number | null;
  all?: unknown;
}): ReadValueOutcome {
  const index = params.index ?? undefined;
  if (index !== undefined && (!Number.isInteger(index) || index < 0)) {
    return {
      success: false,
      error: `'index' must be a non-negative integer, got ${JSON.stringify(params.index)}`,
      code: "INVALID_PARAMS",
    };
  }
  const result = readValuePrimitive(params.selector || undefined, index, {
    all: params.all ?? undefined,
  });
  return result.ok
    ? { success: true, data: result.data }
    : { success: false, error: result.error, code: result.code };
}
