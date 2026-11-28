import {
  findByTextPrimitive,
  clickByTextPrimitive,
  clickBySelectorPrimitive,
} from "@qontinui/ui-bridge/control";
import { REDACTED_VALUE, isContentRedacted } from "@qontinui/ui-bridge";
import type { FindByTextHit, ClickEcho } from "@qontinui/ui-bridge/control";

/**
 * The relay's `find_by_text` / `click_by_text` / `click_by_selector` requests,
 * answered by the SDK's §4.6-gated primitives.
 *
 * The runner used to carry private copies that matched on raw `textContent`
 * and echoed it back, so text inside a `data-bridge-redact="true"` boundary was
 * served in cleartext. The primitives drop content-redacted hits and scrub the
 * click echo, so one choke point owns the verdict.
 */
export type TextActionOutcome<T> =
  | { success: true; data: T }
  | { success: false; error: string; code?: string };

function outcome<T>(
  result: { ok: true; data: T } | { ok: false; error: string; code: string },
): TextActionOutcome<T> {
  return result.ok
    ? { success: true, data: result.data }
    : { success: false, error: result.error, code: result.code };
}

/** Scroll into view, then click: what the direct HTTP handler's JS twin did. */
function scrollAndClick(el: HTMLElement): void {
  el.scrollIntoView({ block: "center" });
  el.click();
}

export function runFindByTextRequest(params: {
  text?: string;
  tag?: string;
  exact?: boolean;
}): TextActionOutcome<FindByTextHit[]> {
  return outcome(
    findByTextPrimitive(params.text, {
      tag: params.tag || undefined,
      exact: params.exact ?? undefined,
    }),
  );
}

export function runClickByTextRequest(params: {
  text?: string;
  tag?: string;
  exact?: boolean;
}): TextActionOutcome<{ clicked: true; element: ClickEcho }> {
  return outcome(
    clickByTextPrimitive(
      params.text,
      { tag: params.tag || undefined, exact: params.exact ?? undefined },
      scrollAndClick,
    ),
  );
}

export function runClickBySelectorRequest(params: {
  selector?: string;
  index?: number | null;
}): TextActionOutcome<{ clicked: true; element: ClickEcho }> {
  return outcome(
    clickBySelectorPrimitive(params.selector, params.index ?? undefined, scrollAndClick),
  );
}

/**
 * An element's `textContent`, or the SDK's `REDACTED_VALUE` when the element
 * sits inside a `data-bridge-redact` boundary. For call sites that echo or
 * compare caller-selected text and are not a find/click primitive.
 */
export function scrubbedTextContent(el: HTMLElement | null | undefined): string {
  if (!el) return "";
  return isContentRedacted(el) ? REDACTED_VALUE : (el.textContent ?? "");
}

/** An attribute read that yields `REDACTED_VALUE` inside a redaction boundary. */
export function scrubbedAttribute(el: HTMLElement | null | undefined, name: string): string | undefined {
  const raw = el?.getAttribute(name) ?? undefined;
  if (raw === undefined || !el) return undefined;
  return isContentRedacted(el) ? REDACTED_VALUE : raw;
}
