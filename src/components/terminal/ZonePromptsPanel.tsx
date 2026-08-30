import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { MessageSquare, RefreshCw, X } from "lucide-react";
import { formatPromptTime, type UserPrompt } from "./sessionPrompts";
import { useSessionPrompts } from "./useSessionPrompts";

/**
 * Where the panel sits relative to its terminal.
 *
 * `"top"` — a short horizontal strip directly under the zone's title bar, for
 * a zone tiled alongside others. `"right"` — a full-height vertical column on
 * the right, for a zone that has the whole page (the `single` layout, or a
 * maximized zone), where vertical space is the plentiful axis.
 */
export type PromptsPanelOrientation = "top" | "right";

/** Height of the `"top"` strip. Callers add this to the terminal body's padding. */
export const PROMPTS_PANEL_TOP_HEIGHT_PX = 104;
/** Width of the `"right"` column. */
export const PROMPTS_PANEL_RIGHT_WIDTH_PX = 300;

/**
 * The operator's own prompts for one session, deliberately styled as a LIGHT
 * surface: everything else in a zone is dark terminal output, so the contrast
 * is what makes "my side of the conversation" findable at a glance rather than
 * something to be read for.
 *
 * Newest sits at the bottom, matching terminal output — the panel auto-scrolls
 * there on new prompts unless the operator has scrolled up to read history, in
 * which case it holds position.
 */
export function ZonePromptsPanel({
  claudeSessionId,
  configDir,
  projectPath,
  orientation,
  onClose,
  /** Distance from the top of the positioned parent, for `"top"`/`"right"` overlays. */
  topOffsetPx = 0,
  /**
   * Rendered height of the `"top"` strip. Callers clamp this to what the zone
   * can spare (see `promptsStripHeight`) and MUST reserve the same number of
   * pixels of body padding. Ignored for `"right"`.
   */
  heightPx = PROMPTS_PANEL_TOP_HEIGHT_PX,
}: {
  claudeSessionId: string;
  configDir?: string;
  projectPath?: string;
  orientation: PromptsPanelOrientation;
  onClose: () => void;
  topOffsetPx?: number;
  heightPx?: number;
}) {
  const { status, prompts, reason, refreshing, refresh } = useSessionPrompts(claudeSessionId, {
    configDir,
    projectPath,
    enabled: true,
  });

  const isTop = orientation === "top";
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  // Prompts the operator has expanded past the clamp, by uuid. Most prompts
  // are a line or two (median 61 chars across this fleet's transcripts), but a
  // pasted log can run to 150 KB — one of those unclamped would fill the strip
  // and bury everything around it, which is the opposite of "at a glance".
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const toggleExpanded = useCallback((uuid: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(uuid)) next.delete(uuid);
      else next.add(uuid);
      return next;
    });
  }, []);
  // Whether the operator is parked at the bottom. Starts true so the first
  // load lands on the latest prompt.
  const stickToBottomRef = useRef(true);
  const lastCountRef = useRef(0);

  const handleScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    // 24px of slack: a near-bottom position still counts as "following".
    stickToBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  };

  // Layout effect so the jump to the bottom happens in the same frame the new
  // prompt paints — an effect would show one frame of the old scroll position.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (prompts.length === lastCountRef.current) return;
    lastCountRef.current = prompts.length;
    if (!stickToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [prompts.length]);

  // Re-pin whenever the CONTENT resizes, not just when the prompt count does.
  //
  // The layout effect above runs before each card has measured its own clamp,
  // and a card that turns out to be clamped then adds a "Show more" row — so
  // the content grows AFTER the scroll was set and the view lands short of the
  // bottom by exactly that much (measured: scrollTop 65 against a 79 maximum).
  // Expanding a card has the same shape, only larger. Observing the content is
  // what makes "latest at the bottom" true at rest rather than at first paint.
  useEffect(() => {
    const el = scrollRef.current;
    const content = contentRef.current;
    if (!el || !content) return;
    const observer = new ResizeObserver(() => {
      if (stickToBottomRef.current) el.scrollTop = el.scrollHeight;
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, []);

  // Re-anchor when the panel changes shape: the same content in a column is a
  // different scrollHeight than in a strip, so a stuck-to-bottom view would
  // otherwise land mid-history after a maximize.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickToBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [orientation, heightPx]);

  // A panel can outlive the session it was opened for (a zone reassigned
  // underneath it). Reset the follow state and the count baseline, or a new
  // session with a coincidentally equal prompt count would leave the layout
  // effect early-returning and the view stuck mid-history.
  useEffect(() => {
    stickToBottomRef.current = true;
    lastCountRef.current = 0;
  }, [claudeSessionId]);

  const frame = isTop ? "absolute left-0 right-0 border-b" : "absolute right-0 bottom-0 border-l";
  const frameStyle = isTop
    ? { top: `${topOffsetPx}px`, height: `${heightPx}px` }
    : { top: `${topOffsetPx}px`, width: `${PROMPTS_PANEL_RIGHT_WIDTH_PX}px` };

  return (
    <div
      data-testid="zone-prompts-panel"
      data-orientation={orientation}
      // z-6: above the terminal body and ZoneQuickActions' transparent hover
      // sentinels (z-5), but BELOW the zone title bar and filter bar (z-10).
      // Those bars open dropdowns at z-50, and a z-10 bar is a stacking
      // context — so a panel above it would swallow its own title bar's menus.
      className={`${frame} z-[6] flex flex-col bg-[#f4f4f8] border-[#c8ccd8] text-[#1f2233]`}
      style={frameStyle}
      // The panel is a reading surface layered over a terminal; without this
      // the zone's mousedown handler would steal focus and route keystrokes
      // back into the PTY while the operator is scrolling here.
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      <div className="flex items-center gap-1.5 px-2 py-0.5 border-b border-[#d7dae3] bg-[#e7e9f0] shrink-0">
        <MessageSquare className="w-2.5 h-2.5 text-[#5a6180]" />
        <span className="text-[10px] font-medium text-[#3a405c]">My prompts</span>
        {status === "ready" && <span className="text-[9px] text-[#6b7191]">{prompts.length}</span>}
        {/* Right-anchored in the strip, but LEFT-anchored in the column: the
            zone's hover-action cluster owns the top-right ~150px at a higher
            z-index, and a 300px-wide column puts these two buttons squarely
            underneath it — reachable only by first moving the mouse away. */}
        <div className={`flex items-center gap-1 ${isTop ? "ml-auto" : ""}`}>
          <button
            onClick={refresh}
            disabled={refreshing}
            className="p-0.5 rounded text-[#6b7191] hover:text-[#1f2233] hover:bg-[#d7dae3] transition-colors disabled:opacity-50"
            title="Reload prompts from the session transcript"
          >
            <RefreshCw className={`w-2.5 h-2.5 ${refreshing ? "animate-spin" : ""}`} />
          </button>
          <button
            onClick={onClose}
            className="p-0.5 rounded text-[#6b7191] hover:text-[#1f2233] hover:bg-[#d7dae3] transition-colors"
            title="Hide prompts"
          >
            <X className="w-2.5 h-2.5" />
          </button>
        </div>
      </div>

      <div
        ref={scrollRef}
        onScroll={handleScroll}
        className="flex-1 min-h-0 overflow-y-auto overscroll-contain px-2 py-1"
        // Scrolling here must not also scroll the terminal underneath.
        onWheel={(e) => e.stopPropagation()}
      >
        <div ref={contentRef} className="space-y-1">
          {status === "loading" && (
            <div className="text-[10px] text-[#6b7191] py-1">Reading transcript…</div>
          )}
          {status === "unavailable" && (
            <div className="text-[10px] text-[#a1442f] py-1">
              Prompts unavailable{reason ? ` — ${reason}` : ""}
            </div>
          )}
          {status === "ready" && prompts.length === 0 && (
            <div className="text-[10px] text-[#6b7191] py-1">No prompts in this session yet.</div>
          )}
          {status === "ready" &&
            prompts.map((p) => (
              <PromptCard
                key={p.uuid}
                prompt={p}
                expanded={expanded.has(p.uuid)}
                onToggle={() => toggleExpanded(p.uuid)}
              />
            ))}
        </div>
      </div>
    </div>
  );
}

/** Lines a prompt shows before it needs expanding. */
const CLAMP_LINES = 6;

function PromptCard({
  prompt,
  expanded,
  onToggle,
}: {
  prompt: UserPrompt;
  expanded: boolean;
  onToggle: () => void;
}) {
  const bodyRef = useRef<HTMLDivElement>(null);
  const [clamped, setClamped] = useState(false);

  // Whether the clamp actually bit — only then is the card worth making
  // clickable, and only then does the hint earn its row.
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    setClamped(el.scrollHeight > el.clientHeight + 1);
  }, [prompt.text, expanded]);

  const canExpand = clamped || expanded;
  return (
    <div
      className={`rounded border border-[#d7dae3] bg-white px-1.5 py-1 shadow-[0_1px_1px_rgba(31,34,51,0.04)] ${
        canExpand ? "cursor-pointer hover:border-[#b6bccd]" : ""
      }`}
      onClick={canExpand ? onToggle : undefined}
      title={canExpand ? (expanded ? "Collapse" : "Show the whole prompt") : undefined}
    >
      {prompt.timestamp && (
        <div className="text-[8px] font-mono text-[#8a90ad] leading-tight">
          {formatPromptTime(prompt.timestamp)}
        </div>
      )}
      <div
        ref={bodyRef}
        className="text-[10px] leading-snug whitespace-pre-wrap break-words text-[#1f2233]"
        style={
          expanded
            ? undefined
            : {
                display: "-webkit-box",
                WebkitLineClamp: CLAMP_LINES,
                WebkitBoxOrient: "vertical",
                overflow: "hidden",
              }
        }
      >
        {prompt.text}
      </div>
      {canExpand && (
        <div className="text-[8px] text-[#8a90ad] mt-0.5">
          {expanded ? "Show less" : "Show more"}
        </div>
      )}
    </div>
  );
}
