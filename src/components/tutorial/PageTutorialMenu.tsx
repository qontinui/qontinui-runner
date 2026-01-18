/**
 * PageTutorialMenu Component
 *
 * Shows available tutorials for a specific page in a dropdown/popover.
 * Users can see and start tutorials directly from the page they teach.
 */

import { useState, useRef, useEffect } from "react";
import { BookOpen, ChevronDown, Clock, GraduationCap, CheckCircle } from "lucide-react";
import { useTutorial } from "../../contexts/TutorialContext";
import { getTutorialsByFocusPage } from "./data";
import { cn } from "../../lib/utils";
import type { TutorialFocusPage, Tutorial, DifficultyLevel } from "../../types/tutorial";

interface PageTutorialMenuProps {
  /** The page/tab to show tutorials for */
  page: TutorialFocusPage;

  /** Optional label text (defaults to "Tutorials") */
  label?: string;

  /** Visual variant */
  variant?: "button" | "compact" | "inline";

  /** Additional CSS classes */
  className?: string;
}

const difficultyColors: Record<DifficultyLevel, string> = {
  beginner: "bg-green-500/20 text-green-400 border-green-500/30",
  intermediate: "bg-yellow-500/20 text-yellow-400 border-yellow-500/30",
  advanced: "bg-red-500/20 text-red-400 border-red-500/30",
};

/**
 * PageTutorialMenu - Dropdown showing tutorials for a specific page
 */
export function PageTutorialMenu({
  page,
  label = "Tutorials",
  variant = "button",
  className,
}: PageTutorialMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const {
    openTutorial,
    isOpen: tutorialIsOpen,
    completedTutorials,
    inProgressTutorials,
  } = useTutorial();

  // Get tutorials for this page
  const pageTutorials = getTutorialsByFocusPage(page);

  // Close when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    }

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [isOpen]);

  // Don't render if no tutorials for this page or tutorial is open
  if (pageTutorials.length === 0 || tutorialIsOpen) {
    return null;
  }

  const handleStartTutorial = (tutorial: Tutorial) => {
    setIsOpen(false);
    openTutorial(tutorial);
  };

  const isCompleted = (tutorialId: string) => completedTutorials.includes(tutorialId);

  const isInProgress = (tutorialId: string) => inProgressTutorials.includes(tutorialId);

  // Compact variant - just an icon
  if (variant === "compact") {
    return (
      <div ref={menuRef} className={cn("relative", className)}>
        <button
          onClick={() => setIsOpen(!isOpen)}
          className={cn(
            "p-1.5 rounded-md transition-all",
            "text-muted-foreground hover:text-foreground",
            "hover:bg-muted",
            isOpen && "bg-muted text-foreground",
          )}
          title={`${pageTutorials.length} tutorial${pageTutorials.length > 1 ? "s" : ""} available`}
        >
          <BookOpen className="w-4 h-4" />
        </button>
        {isOpen && (
          <TutorialDropdown
            tutorials={pageTutorials}
            onSelect={handleStartTutorial}
            isCompleted={isCompleted}
            isInProgress={isInProgress}
          />
        )}
      </div>
    );
  }

  // Inline variant - text link style
  if (variant === "inline") {
    return (
      <div ref={menuRef} className={cn("relative inline-block", className)}>
        <button
          onClick={() => setIsOpen(!isOpen)}
          className={cn(
            "inline-flex items-center gap-1.5 text-sm",
            "text-muted-foreground hover:text-foreground transition-colors",
          )}
        >
          <BookOpen className="w-4 h-4" />
          <span className="underline underline-offset-2">{label}</span>
          <ChevronDown className={cn("w-3 h-3 transition-transform", isOpen && "rotate-180")} />
        </button>
        {isOpen && (
          <TutorialDropdown
            tutorials={pageTutorials}
            onSelect={handleStartTutorial}
            isCompleted={isCompleted}
            isInProgress={isInProgress}
          />
        )}
      </div>
    );
  }

  // Default: button variant
  return (
    <div ref={menuRef} className={cn("relative", className)}>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className={cn(
          "flex items-center gap-2 px-3 py-1.5 rounded-md text-sm font-medium transition-all",
          "border border-border bg-card hover:bg-muted",
          isOpen && "bg-muted",
        )}
      >
        <BookOpen className="w-4 h-4" />
        <span>{label}</span>
        <span className="ml-1 px-1.5 py-0.5 text-xs rounded-full bg-cyan-500/20 text-cyan-400">
          {pageTutorials.length}
        </span>
        <ChevronDown className={cn("w-3 h-3 transition-transform", isOpen && "rotate-180")} />
      </button>
      {isOpen && (
        <TutorialDropdown
          tutorials={pageTutorials}
          onSelect={handleStartTutorial}
          isCompleted={isCompleted}
          isInProgress={isInProgress}
        />
      )}
    </div>
  );
}

/**
 * Dropdown panel showing tutorial list
 */
function TutorialDropdown({
  tutorials,
  onSelect,
  isCompleted,
  isInProgress,
}: {
  tutorials: Tutorial[];
  onSelect: (tutorial: Tutorial) => void;
  isCompleted: (id: string) => boolean;
  isInProgress: (id: string) => boolean;
}) {
  return (
    <div
      className={cn(
        "absolute top-full left-0 mt-1 z-50",
        "w-80 max-h-96 overflow-y-auto",
        "bg-card border border-border rounded-lg shadow-lg",
        "animate-in fade-in-0 zoom-in-95 duration-150",
      )}
    >
      <div className="p-2">
        <div className="px-2 py-1 text-xs font-medium text-muted-foreground uppercase tracking-wider">
          Available Tutorials
        </div>
        <div className="mt-1 space-y-1">
          {tutorials.map((tutorial) => (
            <TutorialMenuItem
              key={tutorial.id}
              tutorial={tutorial}
              onSelect={onSelect}
              completed={isCompleted(tutorial.id)}
              inProgress={isInProgress(tutorial.id)}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

/**
 * Individual tutorial item in the dropdown
 */
function TutorialMenuItem({
  tutorial,
  onSelect,
  completed,
  inProgress,
}: {
  tutorial: Tutorial;
  onSelect: (tutorial: Tutorial) => void;
  completed: boolean;
  inProgress: boolean;
}) {
  return (
    <button
      onClick={() => onSelect(tutorial)}
      className={cn(
        "w-full text-left p-3 rounded-md transition-all",
        "hover:bg-muted group",
        completed && "opacity-75",
      )}
    >
      <div className="flex items-start gap-3">
        {/* Status icon */}
        <div className="mt-0.5">
          {completed ? (
            <CheckCircle className="w-4 h-4 text-green-400" />
          ) : (
            <GraduationCap className="w-4 h-4 text-muted-foreground group-hover:text-foreground" />
          )}
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm truncate">{tutorial.title}</span>
            {inProgress && !completed && (
              <span className="px-1.5 py-0.5 text-xs rounded bg-cyan-500/20 text-cyan-400">
                In Progress
              </span>
            )}
          </div>

          <p className="mt-0.5 text-xs text-muted-foreground line-clamp-2">
            {tutorial.description}
          </p>

          {/* Metadata */}
          <div className="mt-2 flex items-center gap-2">
            <span
              className={cn(
                "px-1.5 py-0.5 text-xs rounded border",
                difficultyColors[tutorial.difficulty],
              )}
            >
              {tutorial.difficulty}
            </span>
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Clock className="w-3 h-3" />
              {tutorial.duration}
            </span>
          </div>
        </div>
      </div>
    </button>
  );
}

export default PageTutorialMenu;
