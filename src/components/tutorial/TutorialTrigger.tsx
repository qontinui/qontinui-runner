/**
 * TutorialTrigger Component
 *
 * A small help/tutorial icon that appears on relevant pages.
 * Clicking it starts the contextual tutorial for that page.
 *
 * Inspired by Flowise's lightbulb icons for contextual hints.
 */

import { HelpCircle, Lightbulb } from "lucide-react";
import { useTutorial } from "../../contexts/TutorialContext";
import { tutorials } from "./data";
import { cn } from "../../lib/utils";

interface TutorialTriggerProps {
  /** ID of the tutorial to start */
  tutorialId: string;

  /** Optional custom tooltip text */
  tooltip?: string;

  /** Visual variant */
  variant?: "icon" | "button" | "inline";

  /** Icon to use */
  icon?: "help" | "lightbulb";

  /** Size of the trigger */
  size?: "sm" | "md" | "lg";

  /** Additional CSS classes */
  className?: string;

  /** Whether to show the trigger (can be controlled externally) */
  show?: boolean;
}

/**
 * TutorialTrigger - Clickable icon/button to start a tutorial
 */
export function TutorialTrigger({
  tutorialId,
  tooltip = "Start tutorial",
  variant = "icon",
  icon = "help",
  size = "md",
  className,
  show = true,
}: TutorialTriggerProps) {
  const { openTutorial, isOpen } = useTutorial();

  // Don't show if tutorial is already open or if show is false
  if (!show || isOpen) {
    return null;
  }

  // Find the tutorial by ID
  const tutorial = tutorials.find((t) => t.id === tutorialId);
  if (!tutorial) {
    console.warn(`[TutorialTrigger] Tutorial not found: ${tutorialId}`);
    return null;
  }

  const handleClick = () => {
    openTutorial(tutorial);
  };

  const IconComponent = icon === "lightbulb" ? Lightbulb : HelpCircle;

  const sizeClasses = {
    sm: "w-4 h-4",
    md: "w-5 h-5",
    lg: "w-6 h-6",
  };

  const buttonSizeClasses = {
    sm: "px-2 py-1 text-xs",
    md: "px-3 py-1.5 text-sm",
    lg: "px-4 py-2 text-base",
  };

  if (variant === "button") {
    return (
      <button
        onClick={handleClick}
        className={cn(
          "flex items-center gap-2 rounded-md font-medium transition-all",
          "border border-border hover:bg-muted",
          buttonSizeClasses[size],
          className,
        )}
        title={tooltip}
        aria-label={tooltip}
      >
        <IconComponent className={sizeClasses[size]} />
        <span>Tutorial</span>
      </button>
    );
  }

  if (variant === "inline") {
    return (
      <button
        onClick={handleClick}
        className={cn(
          "inline-flex items-center gap-1.5 text-muted-foreground hover:text-foreground transition-colors",
          className,
        )}
        title={tooltip}
        aria-label={tooltip}
      >
        <IconComponent className={sizeClasses[size]} />
        <span className="text-sm underline underline-offset-2">Need help?</span>
      </button>
    );
  }

  // Default: icon variant
  return (
    <button
      onClick={handleClick}
      className={cn(
        "p-1.5 rounded-full transition-all",
        "text-muted-foreground hover:text-foreground",
        "hover:bg-cyan-500/10",
        className,
      )}
      title={tooltip}
      aria-label={tooltip}
    >
      <IconComponent className={sizeClasses[size]} />
    </button>
  );
}

/**
 * TutorialTriggerFloating - Floating tutorial trigger positioned in corner
 */
export function TutorialTriggerFloating({
  tutorialId,
  position = "bottom-right",
  ...props
}: TutorialTriggerProps & {
  position?: "top-left" | "top-right" | "bottom-left" | "bottom-right";
}) {
  const positionClasses = {
    "top-left": "top-4 left-4",
    "top-right": "top-4 right-4",
    "bottom-left": "bottom-4 left-4",
    "bottom-right": "bottom-4 right-4",
  };

  return (
    <div className={cn("fixed z-40", positionClasses[position])}>
      <TutorialTrigger
        tutorialId={tutorialId}
        variant="icon"
        icon="lightbulb"
        size="lg"
        {...props}
        className={cn("shadow-lg bg-card border border-border", "hover:scale-110", props.className)}
      />
    </div>
  );
}
