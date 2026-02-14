/**
 * BuilderToolbar.tsx
 *
 * A modular action toolbar for builder/library pages.
 * Provides consistent UI for common actions: AI Generate, New, Import, Export, Delete.
 */

import { useRef } from "react";
import { Sparkles, Plus, Upload, Download, Trash2, GitBranch, Layers, ExternalLink } from "lucide-react";

export interface BuilderToolbarAction {
  /** Unique key for the action */
  key: string;
  /** Icon component to display */
  icon: React.ElementType;
  /** Label text (displayed next to icon) */
  label: string;
  /** Click handler */
  onClick?: () => void;
  /** External URL — renders as <a> tag instead of button */
  href?: string;
  /** Whether the action is disabled */
  disabled?: boolean;
  /** Whether the action is currently active (e.g., selection mode) */
  active?: boolean;
  /** Custom className for the button */
  className?: string;
  /** Tooltip text */
  title?: string;
  /** For file input actions - accept attribute */
  fileAccept?: string;
  /** For file input actions - change handler */
  onFileChange?: (event: React.ChangeEvent<HTMLInputElement>) => void;
  /** Whether to show the label (default: true) */
  showLabel?: boolean;
}

export interface BuilderToolbarProps {
  /** Actions to display in the toolbar */
  actions: BuilderToolbarAction[];
  /** Additional className for the toolbar container */
  className?: string;
}

/**
 * Individual action button component
 */
function ActionButton({ action }: { action: BuilderToolbarAction }) {
  const fileInputRef = useRef<HTMLInputElement>(null);
  const Icon = action.icon;
  const showLabel = action.showLabel !== false;

  // File input action
  if (action.fileAccept && action.onFileChange) {
    return (
      <>
        <input
          ref={fileInputRef}
          type="file"
          accept={action.fileAccept}
          onChange={action.onFileChange}
          className="hidden"
        />
        <button
          onClick={() => fileInputRef.current?.click()}
          disabled={action.disabled}
          className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg
            hover:bg-neutral-800 transition-colors text-xs
            text-neutral-400 hover:text-neutral-200 disabled:opacity-50
            ${action.className || ""}`}
          title={action.title || action.label}
        >
          <Icon className="w-3.5 h-3.5" />
          {showLabel && <span>{action.label}</span>}
        </button>
      </>
    );
  }

  // External link action — renders as <a> tag (works in Tauri webview)
  if (action.href) {
    return (
      <a
        href={action.href}
        target="_blank"
        rel="noopener noreferrer"
        className={`flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg
          transition-colors text-xs no-underline
          ${action.className || "flex-1 hover:bg-neutral-800 text-neutral-400 hover:text-neutral-200"}
          ${!showLabel ? "p-1.5 flex-none" : "flex-1"}`}
        title={action.title || action.label}
      >
        <Icon className="w-3.5 h-3.5" />
        {showLabel && <span>{action.label}</span>}
      </a>
    );
  }

  // Regular button action
  return (
    <button
      onClick={action.onClick}
      disabled={action.disabled}
      className={`flex items-center justify-center gap-1.5 px-2 py-1.5 rounded-lg
        transition-colors text-xs disabled:opacity-50
        ${
          action.active
            ? "bg-red-500/20 text-red-400"
            : action.className ||
              "flex-1 hover:bg-neutral-800 text-neutral-400 hover:text-neutral-200"
        }
        ${!showLabel ? "p-1.5 flex-none" : "flex-1"}`}
      title={action.title || action.label}
    >
      <Icon className="w-3.5 h-3.5" />
      {showLabel && <span>{action.label}</span>}
    </button>
  );
}

/**
 * BuilderToolbar component
 *
 * Usage:
 * ```tsx
 * <BuilderToolbar
 *   actions={[
 *     { key: "ai", icon: Sparkles, label: "AI", onClick: openAiModal, className: "text-blue-400 hover:text-blue-300" },
 *     { key: "new", icon: Plus, label: "New", onClick: createNew },
 *     { key: "import", icon: Upload, label: "Import", fileAccept: ".json", onFileChange: handleImport },
 *     { key: "export", icon: Download, label: "Export", onClick: handleExport, disabled: !hasSelection },
 *     { key: "delete", icon: Trash2, label: "", onClick: toggleDeleteMode, active: isDeleteMode, showLabel: false },
 *   ]}
 * />
 * ```
 */
export function BuilderToolbar({ actions, className = "" }: BuilderToolbarProps) {
  return (
    <div className={`flex items-center gap-1 ${className}`}>
      {actions.map((action) => (
        <ActionButton key={action.key} action={action} />
      ))}
    </div>
  );
}

/**
 * Pre-configured action creators for common use cases
 */
export const toolbarActions = {
  /** AI Generate action - for builders with AI generation */
  ai: (onClick: () => void, disabled = false): BuilderToolbarAction => ({
    key: "ai",
    icon: Sparkles,
    label: "AI",
    onClick,
    disabled,
    className: "text-blue-400 hover:text-blue-300 hover:bg-neutral-800",
    title: "Generate with AI",
  }),

  /** AI Generate action - disabled/placeholder for builders without AI generation yet */
  aiPlaceholder: (): BuilderToolbarAction => ({
    key: "ai",
    icon: Sparkles,
    label: "AI",
    disabled: true,
    className: "text-neutral-600 cursor-not-allowed",
    title: "AI generation coming soon",
  }),

  /** Generate from State Machine action */
  stateMachine: (onClick: () => void, disabled = false): BuilderToolbarAction => ({
    key: "state-machine",
    icon: GitBranch,
    label: "States",
    onClick,
    disabled,
    className: "text-emerald-400 hover:text-emerald-300 hover:bg-neutral-800",
    title: "Generate from loaded state machine",
  }),

  /** Add steps from a single state action */
  addFromState: (onClick: () => void, disabled = false): BuilderToolbarAction => ({
    key: "add-from-state",
    icon: Layers,
    label: "+State",
    onClick,
    disabled,
    className: "text-purple-400 hover:text-purple-300 hover:bg-neutral-800",
    title: "Add verification and agentic steps from a state",
  }),

  /** Live browser mode action */
  liveBrowser: (onClick: () => void, disabled = false): BuilderToolbarAction => ({
    key: "live-browser",
    icon: Sparkles,
    label: "Live",
    onClick,
    disabled,
    className: "text-cyan-400 hover:text-cyan-300 hover:bg-neutral-800",
    title: "Live Browser Mode - connect to open pages without config",
  }),

  /** New item action */
  new: (onClick: () => void, label = "New"): BuilderToolbarAction => ({
    key: "new",
    icon: Plus,
    label,
    onClick,
    title: `Create new ${label.toLowerCase()}`,
  }),

  /** Import action with file input */
  import: (
    onFileChange: (event: React.ChangeEvent<HTMLInputElement>) => void,
    accept = ".json",
    disabled = false,
  ): BuilderToolbarAction => ({
    key: "import",
    icon: Upload,
    label: "Import",
    fileAccept: accept,
    onFileChange,
    disabled,
    title: "Import from file",
  }),

  /** Export action */
  export: (onClick: () => void, disabled = false): BuilderToolbarAction => ({
    key: "export",
    icon: Download,
    label: "Export",
    onClick,
    disabled,
    title: "Export to file",
  }),

  /** Delete/selection mode toggle action */
  delete: (onClick: () => void, active = false): BuilderToolbarAction => ({
    key: "delete",
    icon: Trash2,
    label: "",
    onClick,
    active,
    showLabel: false,
    className: active ? "" : "hover:bg-neutral-800 text-neutral-400 hover:text-red-400",
    title: active ? "Cancel selection" : "Select items to delete",
  }),

  /** Edit in Web action — opens the corresponding web builder page in system browser */
  editInWeb: (webPath: string): BuilderToolbarAction => ({
    key: "edit-in-web",
    icon: ExternalLink,
    label: "Web",
    href: `http://localhost:3001${webPath}`,
    className: "text-indigo-400 hover:text-indigo-300 hover:bg-neutral-800",
    title: "Edit in Web (opens browser)",
  }),
};

export default BuilderToolbar;
