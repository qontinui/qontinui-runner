/**
 * ScreenshotView Component
 *
 * Displays the current screenshot/screen capture area with overlays.
 * Shows bounding boxes for detected elements and zoom controls.
 */

import { ZoomIn, ZoomOut, Maximize2, Monitor } from "lucide-react";
import { cn } from "../../lib/utils";
import { Button, Badge } from "../ui";
import type { ScreenshotViewProps } from "./types";
import { getStatusColors, getAccentColors } from "@/design-system";

export function ScreenshotView({ status }: ScreenshotViewProps) {
  const greenColors = getAccentColors("green");
  const blueColors = getAccentColors("blue");
  const pausedColors = getStatusColors("paused");

  return (
    <div className="relative h-[60%] border-b border-border bg-card">
      {/* Screenshot placeholder with grid pattern */}
      <div className="h-full w-full bg-gradient-to-br from-background via-background to-muted/30">
        <div
          className="h-full w-full"
          style={{
            backgroundImage: `linear-gradient(rgba(255, 255, 255, 0.03) 1px, transparent 1px),
                           linear-gradient(90deg, rgba(255, 255, 255, 0.03) 1px, transparent 1px)`,
            backgroundSize: "20px 20px",
          }}
        >
          {/* Simulated bounding box */}
          <div
            className={cn(
              "absolute left-[40%] top-[45%] h-12 w-32 border-2",
              greenColors.border,
              greenColors.bg,
            )}
          >
            <div className={cn("absolute -top-6 left-0 text-xs font-mono", greenColors.text)}>
              save-button (92.3%)
            </div>
          </div>

          {/* Simulated crosshair */}
          <div className="absolute left-[46%] top-[50%]">
            <div className="relative h-4 w-4">
              <div
                className={cn(
                  "absolute left-1/2 top-0 h-full w-0.5 -translate-x-1/2",
                  blueColors.bgSolid,
                )}
              />
              <div
                className={cn(
                  "absolute left-0 top-1/2 h-0.5 w-full -translate-y-1/2",
                  blueColors.bgSolid,
                )}
              />
            </div>
          </div>
        </div>
      </div>

      {/* Paused overlay */}
      {status === "paused" && (
        <div className="absolute inset-0 flex items-center justify-center bg-black/60 backdrop-blur-sm">
          <div className={cn("text-4xl font-bold", pausedColors.text)}>PAUSED</div>
        </div>
      )}

      {/* Corner overlays */}
      <div className="absolute left-3 top-3">
        <Badge className="bg-muted/90 text-foreground border-border">
          <Monitor className="mr-1.5 h-3 w-3" />
          Monitor 1
        </Badge>
      </div>

      <div className="absolute right-3 top-3">
        <Badge variant="muted" className="bg-muted/90 border-border font-mono text-xs">
          Updated 2s ago
        </Badge>
      </div>

      <div className="absolute bottom-3 right-3 flex gap-2">
        <Button size="sm" variant="outline" className="h-8 w-8 border-border bg-muted/90 p-0">
          <ZoomOut className="h-3.5 w-3.5" />
        </Button>
        <Button size="sm" variant="outline" className="h-8 w-8 border-border bg-muted/90 p-0">
          <span className="text-xs font-mono">100%</span>
        </Button>
        <Button size="sm" variant="outline" className="h-8 w-8 border-border bg-muted/90 p-0">
          <ZoomIn className="h-3.5 w-3.5" />
        </Button>
        <Button size="sm" variant="outline" className="h-8 w-8 border-border bg-muted/90 p-0">
          <Maximize2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  );
}
