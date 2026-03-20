/**
 * SpotlightOverlay Component
 *
 * Creates a dark overlay with a spotlight cutout around a target element.
 * Used for contextual tutorials to draw attention to specific UI elements.
 */

import { useEffect, useState, useCallback, useRef } from "react";
import { cn } from "../../lib/utils";

interface SpotlightOverlayProps {
  /** CSS selector or data-tutorial-id to find the target element */
  targetSelector: string | null;

  /** Whether the spotlight is visible */
  isVisible?: boolean;

  /** Padding around the highlighted element (pixels) */
  padding?: number;

  /** Border radius of the spotlight cutout */
  borderRadius?: number;

  /** Opacity of the dark overlay (0-1) */
  overlayOpacity?: number;

  /** Whether clicking the overlay should be allowed */
  allowClickThrough?: boolean;

  /** Callback when clicking outside the spotlight */
  onOverlayClick?: () => void;

  /** Whether to show a pulse animation */
  showPulse?: boolean;
}

interface ElementRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * SpotlightOverlay - SVG-based spotlight with smooth transitions
 */
export function SpotlightOverlay({
  targetSelector,
  isVisible = true,
  padding = 8,
  borderRadius = 8,
  overlayOpacity = 0.75,
  allowClickThrough = false,
  onOverlayClick,
  showPulse = true,
}: SpotlightOverlayProps) {
  const [rect, setRect] = useState<ElementRect | null>(null);
  const [windowSize, setWindowSize] = useState({ width: 0, height: 0 });
  const observerRef = useRef<ResizeObserver | null>(null);
  const elementRef = useRef<HTMLElement | null>(null);

  /**
   * Update the rect based on target element position
   */
  const updateRect = useCallback(() => {
    if (!targetSelector) {
      setRect(null);
      return;
    }

    // Try data-tutorial-id first
    let element = document.querySelector(`[data-tutorial-id="${targetSelector}"]`) as HTMLElement;

    // Fall back to CSS selector
    if (!element) {
      element = document.querySelector(targetSelector) as HTMLElement;
    }

    if (element) {
      const boundingRect = element.getBoundingClientRect();
      setRect({
        x: boundingRect.left - padding,
        y: boundingRect.top - padding,
        width: boundingRect.width + padding * 2,
        height: boundingRect.height + padding * 2,
      });
      elementRef.current = element;
    } else {
      setRect(null);
      elementRef.current = null;
    }
  }, [targetSelector, padding]);

  /**
   * Update window size for SVG viewport
   */
  const updateWindowSize = useCallback(() => {
    setWindowSize({
      width: window.innerWidth,
      height: window.innerHeight,
    });
  }, []);

  // Set up resize observer and event listeners
  useEffect(() => {
    updateWindowSize();
    updateRect();

    window.addEventListener("resize", updateWindowSize);
    window.addEventListener("scroll", updateRect, true);

    // Create resize observer for target element
    observerRef.current = new ResizeObserver(() => {
      updateRect();
    });

    // Observe document body for layout changes
    observerRef.current.observe(document.body);

    return () => {
      window.removeEventListener("resize", updateWindowSize);
      window.removeEventListener("scroll", updateRect, true);
      observerRef.current?.disconnect();
    };
  }, [updateWindowSize, updateRect]);

  // Update rect when selector changes
  useEffect(() => {
    updateRect();
  }, [targetSelector, updateRect]);

  // Don't render if not visible or no target
  if (!isVisible || !rect) {
    return null;
  }

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (!allowClickThrough) {
      e.stopPropagation();
      onOverlayClick?.();
    }
  };

  return (
    <div
      className={cn("fixed inset-0 z-tutorial-spotlight", "pointer-events-auto tutorial-fade-in")}
      onClick={handleOverlayClick}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !allowClickThrough) {
          e.stopPropagation();
          onOverlayClick?.();
        }
      }}
      role="button"
      tabIndex={allowClickThrough ? -1 : 0}
      aria-label="Tutorial overlay"
      style={{ pointerEvents: allowClickThrough ? "none" : "auto" }}
    >
      <svg
        className="w-full h-full"
        viewBox={`0 0 ${windowSize.width} ${windowSize.height}`}
        preserveAspectRatio="none"
      >
        <defs>
          {/* Mask for the spotlight cutout */}
          <mask id="spotlight-mask">
            {/* White background (visible area) */}
            <rect x="0" y="0" width="100%" height="100%" fill="white" />
            {/* Black cutout (transparent area) */}
            <rect
              x={rect.x}
              y={rect.y}
              width={rect.width}
              height={rect.height}
              rx={borderRadius}
              ry={borderRadius}
              fill="black"
              style={{
                transition: "all 300ms ease-in-out",
              }}
            />
          </mask>
        </defs>

        {/* Dark overlay with cutout */}
        <rect
          x="0"
          y="0"
          width="100%"
          height="100%"
          fill={`rgba(0, 0, 0, ${overlayOpacity})`}
          mask="url(#spotlight-mask)"
        />

        {/* Pulse ring around spotlight */}
        {showPulse && (
          <rect
            x={rect.x - 4}
            y={rect.y - 4}
            width={rect.width + 8}
            height={rect.height + 8}
            rx={borderRadius + 4}
            ry={borderRadius + 4}
            fill="none"
            stroke="hsl(var(--primary))"
            strokeWidth="2"
            className="tutorial-spotlight-pulse"
            style={{
              transition: "all 300ms ease-in-out",
            }}
          />
        )}

        {/* Border around spotlight */}
        <rect
          x={rect.x}
          y={rect.y}
          width={rect.width}
          height={rect.height}
          rx={borderRadius}
          ry={borderRadius}
          fill="none"
          stroke="hsl(var(--primary) / 0.5)"
          strokeWidth="2"
          style={{
            transition: "all 300ms ease-in-out",
          }}
        />
      </svg>
    </div>
  );
}
