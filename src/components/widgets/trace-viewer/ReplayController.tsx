import { useState, useEffect, useCallback } from "react";
import { Play, Pause, SkipBack, SkipForward, ChevronLeft, ChevronRight } from "lucide-react";

interface ReplayControllerProps {
  currentStep: number;
  totalSteps: number;
  onStepChange: (step: number) => void;
  onClose: () => void;
}

export function ReplayController({
  currentStep,
  totalSteps,
  onStepChange,
  onClose,
}: ReplayControllerProps) {
  const [isPlaying, setIsPlaying] = useState(false);
  const [speed, setSpeed] = useState(1000); // ms between steps

  const canGoBack = currentStep > 0;
  const canGoForward = currentStep < totalSteps - 1;

  const goToStart = useCallback(() => onStepChange(0), [onStepChange]);
  const goBack = useCallback(() => {
    if (canGoBack) onStepChange(currentStep - 1);
  }, [canGoBack, currentStep, onStepChange]);
  const goForward = useCallback(() => {
    if (canGoForward) onStepChange(currentStep + 1);
  }, [canGoForward, currentStep, onStepChange]);
  const goToEnd = useCallback(
    () => onStepChange(Math.max(0, totalSteps - 1)),
    [totalSteps, onStepChange],
  );

  // Auto-play
  useEffect(() => {
    if (!isPlaying || !canGoForward) {
      if (!canGoForward) setIsPlaying(false);
      return;
    }
    const timer = setTimeout(() => {
      onStepChange(currentStep + 1);
    }, speed);
    return () => clearTimeout(timer);
  }, [isPlaying, canGoForward, currentStep, speed, onStepChange]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT") return;
      if (e.key === "ArrowLeft") goBack();
      else if (e.key === "ArrowRight") goForward();
      else if (e.key === " ") {
        e.preventDefault();
        setIsPlaying((p) => !p);
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [goBack, goForward]);

  if (totalSteps === 0) return null;

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 bg-zinc-800 border-b border-zinc-700 text-xs">
      <div className="flex items-center gap-1">
        <button
          onClick={goToStart}
          disabled={!canGoBack}
          className="p-0.5 hover:bg-zinc-700 rounded disabled:opacity-30"
          title="Start"
        >
          <SkipBack className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={goBack}
          disabled={!canGoBack}
          className="p-0.5 hover:bg-zinc-700 rounded disabled:opacity-30"
          title="Previous (←)"
        >
          <ChevronLeft className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={() => setIsPlaying((p) => !p)}
          className="p-0.5 hover:bg-zinc-700 rounded"
          title="Play/Pause (Space)"
        >
          {isPlaying ? (
            <Pause className="w-3.5 h-3.5" />
          ) : (
            <Play className="w-3.5 h-3.5" />
          )}
        </button>
        <button
          onClick={goForward}
          disabled={!canGoForward}
          className="p-0.5 hover:bg-zinc-700 rounded disabled:opacity-30"
          title="Next (→)"
        >
          <ChevronRight className="w-3.5 h-3.5" />
        </button>
        <button
          onClick={goToEnd}
          disabled={!canGoForward}
          className="p-0.5 hover:bg-zinc-700 rounded disabled:opacity-30"
          title="End"
        >
          <SkipForward className="w-3.5 h-3.5" />
        </button>
      </div>

      <div className="flex items-center gap-1.5 text-muted-foreground">
        <span>Step</span>
        <span className="font-mono text-foreground">{currentStep + 1}</span>
        <span>of</span>
        <span className="font-mono text-foreground">{totalSteps}</span>
      </div>

      {/* Speed control */}
      <select
        value={speed}
        onChange={(e) => setSpeed(Number(e.target.value))}
        className="text-xs bg-zinc-700 border border-zinc-600 rounded px-1 py-0.5"
      >
        <option value={2000}>0.5x</option>
        <option value={1000}>1x</option>
        <option value={500}>2x</option>
        <option value={250}>4x</option>
      </select>

      {/* Timeline scrubber */}
      <input
        type="range"
        min={0}
        max={totalSteps - 1}
        value={currentStep}
        onChange={(e) => onStepChange(Number(e.target.value))}
        className="flex-1 h-1 accent-blue-500"
      />

      <button
        onClick={onClose}
        className="text-muted-foreground hover:text-foreground ml-auto"
        title="Exit replay"
      >
        ✕
      </button>
    </div>
  );
}
