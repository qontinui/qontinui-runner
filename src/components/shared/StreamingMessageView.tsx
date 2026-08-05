/**
 * `StreamingMessageView` — how an IN-FLIGHT AI response is rendered.
 *
 * Every consumer of `useAiSession().streamingContent` used to push the whole
 * accumulated buffer through a markdown parser on every streamed line, which is
 * O(n²) in the length of the response (plan
 * `2026-07-28-runner-many-sessions-performance.md` §A5a/A5b). While the message
 * is still streaming there is nothing to gain from a full markdown parse — the
 * document is syntactically incomplete anyway — so this component renders a
 * bounded `<pre>` TAIL of the buffer and leaves full markdown rendering to the
 * completed message.
 *
 * Earlier text is not lost: "Show earlier" widens the window in slabs and
 * "Show all" reveals the entire retained buffer.
 */

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { cn } from "../../lib/utils";
import {
  computeStreamWindow,
  shouldResetStreamWindow,
  STREAM_EARLIER_STEP_CHARS,
  STREAM_TAIL_CHARS,
} from "../../lib/ai-streaming/streamingBuffer";

export interface StreamingMessageViewProps {
  /** Full retained streaming buffer. */
  content: string;
  /** Characters permanently dropped by the buffer's retention cap. */
  droppedChars?: number;
  /** Extra classes for the `<pre>` tail. */
  className?: string;
  /** Extra classes for the wrapper. */
  wrapperClassName?: string;
  /** Render the blinking "still typing" caret after the text. */
  showCaret?: boolean;
  /** Caret colour class (defaults to the amber used by the process-manager panel). */
  caretClassName?: string;
}

function formatChars(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)} MB`;
  if (count >= 1024) return `${Math.round(count / 1024)} KB`;
  return `${count} chars`;
}

function StreamingMessageViewImpl({
  content,
  droppedChars = 0,
  className,
  wrapperClassName,
  showCaret = false,
  caretClassName = "bg-amber-400",
}: StreamingMessageViewProps) {
  const [windowChars, setWindowChars] = useState(STREAM_TAIL_CHARS);
  const previousProgressRef = useRef({ length: content.length, droppedChars });

  // A shrinking buffer means a new turn started (the hook resets it to ""), so
  // collapse back to the default tail — but the retention cap ALSO shrinks it
  // mid-stream, and that must not snap a reader who clicked "Show all" back to
  // the tail. `shouldResetStreamWindow` separates the two.
  useEffect(() => {
    const next = { length: content.length, droppedChars };
    if (shouldResetStreamWindow(previousProgressRef.current, next)) {
      setWindowChars(STREAM_TAIL_CHARS);
    }
    previousProgressRef.current = next;
  }, [content.length, droppedChars]);

  const showEarlier = useCallback(() => {
    setWindowChars((chars) => chars + STREAM_EARLIER_STEP_CHARS);
  }, []);

  const showAll = useCallback(() => {
    setWindowChars(Number.MAX_SAFE_INTEGER);
  }, []);

  const { visible, hiddenChars } = computeStreamWindow(content, windowChars, droppedChars);

  return (
    <div className={cn("flex flex-col gap-1", wrapperClassName)}>
      {(hiddenChars > 0 || droppedChars > 0) && (
        <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
          {hiddenChars > 0 && (
            <>
              <span>{formatChars(hiddenChars)} earlier</span>
              <button
                type="button"
                onClick={showEarlier}
                className="underline underline-offset-2 hover:text-foreground transition-colors"
              >
                Show earlier
              </button>
              <button
                type="button"
                onClick={showAll}
                className="underline underline-offset-2 hover:text-foreground transition-colors"
              >
                Show all
              </button>
            </>
          )}
          {droppedChars > 0 && (
            <span title="This response exceeded the retention cap; the oldest text was discarded.">
              ({formatChars(droppedChars)} trimmed)
            </span>
          )}
        </div>
      )}
      <pre
        className={cn(
          "whitespace-pre-wrap break-words font-mono text-xs leading-relaxed m-0",
          className,
        )}
      >
        {visible}
        {showCaret && (
          <span className={cn("inline-block w-1.5 h-3 animate-pulse ml-0.5", caretClassName)} />
        )}
      </pre>
    </div>
  );
}

/**
 * Memoised: the streaming buffer changes on a batched cadence, and every other
 * prop is stable, so an unrelated parent re-render must not re-slice + re-paint
 * the tail.
 */
export const StreamingMessageView = memo(StreamingMessageViewImpl);

export default StreamingMessageView;
