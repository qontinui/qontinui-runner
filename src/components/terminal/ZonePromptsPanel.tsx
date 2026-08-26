import { useEffect, useLayoutEffect, useRef } from "react";
import { MessageSquare, RefreshCw, X } from "lucide-react";
import { formatPromptTime } from "./sessionPrompts";
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
}: {
  claudeSessionId: string;
  configDir?: string;
  projectPath?: string;
  orientation: PromptsPanelOrientation;
  onClose: () => void;
  topOffsetPx?: number;
}) {
  const { status, prompts, reason, refresh } = useSessionPrompts(claudeSessionId, {
    configDir,
    projectPath,
    enabled: true,
  });

  const isTop = orientation === "top";
  const scrollRef = useRef<HTMLDivElement>(null);
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

  // Re-anchor when the panel changes shape: the same content in a column is a
  // different scrollHeight than in a strip, so a stuck-to-bottom view would
  // otherwise land mid-history after a maximize.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickToBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [orientation]);

  const frame = isTop ? "absolute left-0 right-0 border-b" : "absolute right-0 bottom-0 border-l";
  const frameStyle = isTop
    ? { top: `${topOffsetPx}px`, height: `${PROMPTS_PANEL_TOP_HEIGHT_PX}px` }
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
            className="p-0.5 rounded text-[#6b7191] hover:text-[#1f2233] hover:bg-[#d7dae3] transition-colors"
            title="Reload prompts from the session transcript"
          >
            <RefreshCw className="w-2.5 h-2.5" />
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
        className="flex-1 min-h-0 overflow-y-auto overscroll-contain px-2 py-1 space-y-1"
        // Scrolling here must not also scroll the terminal underneath.
        onWheel={(e) => e.stopPropagation()}
      >
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
            <div
              key={p.uuid}
              className="rounded border border-[#d7dae3] bg-white px-1.5 py-1 shadow-[0_1px_1px_rgba(31,34,51,0.04)]"
            >
              {p.timestamp && (
                <div className="text-[8px] font-mono text-[#8a90ad] leading-tight">
                  {formatPromptTime(p.timestamp)}
                </div>
              )}
              <div className="text-[10px] leading-snug whitespace-pre-wrap break-words text-[#1f2233]">
                {p.text}
              </div>
            </div>
          ))}
      </div>
    </div>
  );
}
