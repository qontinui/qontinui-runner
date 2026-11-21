/**
 * The runner-webview half of `POST /ui-bridge/control/page/read-value`: the
 * `read_value` message `ui_bridge_read_value_handler` (and the relay fallback
 * `/ui-bridge/sdk/page/read-value`) sends.
 *
 * It delegates to the SDK's own `readValuePrimitive`, the single choke point
 * that applies the redaction verdict (§4.6): a `type="password"` input, and any
 * value inside a `data-bridge-redact="true"` boundary, reads back `[REDACTED]`
 * with length 0. The runner used to carry two further copies of this read
 * that never consulted that verdict and served the stored auth token and the
 * sign-in password in cleartext (plan
 * 2026-11-13-the-runners-read-value-serves-password-values-raw). Do not
 * re-grow an inline `.value` read here.
 */

import { readValuePrimitive } from "@qontinui/ui-bridge/control";

/** The request params as they arrive over the wire (`null` for "absent"). */
export interface ReadValueParams {
  selector?: string | null;
  index?: number | null;
  all?: unknown;
}

/** The reply fields the hook sends back (it adds `requestId`, `type`, `timestamp`). */
export type ReadValueReply =
  | { success: true; data: unknown }
  | { success: false; error: string; code?: string };

export function runReadValueRequest(params: ReadValueParams): ReadValueReply {
  // A JSON caller's `null` means "not given". The primitive's validation reads
  // `index !== undefined` / `typeof all !== "boolean"`, so a literal null would
  // be rejected (or make `{all:true,index:null}` look mutually exclusive).
  const index = params.index ?? undefined;
  const all = params.all ?? undefined;
  const result = readValuePrimitive(params.selector ?? undefined, index, {
    all: all as boolean | undefined,
  });
  if (!result.ok) {
    return { success: false, error: result.error, code: result.code };
  }
  return { success: true, data: result.data };
}
