/**
 * ContextCard.tsx
 *
 * Individual context card component for displaying context information
 * in the list view. Shows name, category, tags, scope, usage stats,
 * and quick actions.
 */

import { useState } from "react";
import {
  BookOpen,
  ChevronDown,
  ChevronUp,
  Copy,
  Edit2,
  MoreVertical,
  Tag,
  Trash2,
  ToggleLeft,
  ToggleRight,
  Clock,
  TrendingUp,
  Globe,
  User,
  Package,
  Zap,
  FileText,
  AlertTriangle,
  FolderOpen,
  Cloud,
  CloudOff,
  Check,
  X,
} from "lucide-react";
import type { ContextWithMetadata, ContextScope } from "../../types/context";

interface ContextCardProps {
  context: ContextWithMetadata;
  onEdit: (context: ContextWithMetadata) => void;
  onDelete: (context: ContextWithMetadata) => void;
  onDuplicate: (context: ContextWithMetadata) => void;
  onToggleEnabled: (context: ContextWithMetadata) => void;
  onApproveSync?: (context: ContextWithMetadata) => void;
  onDismissSync?: (context: ContextWithMetadata) => void;
  isProcessing?: boolean;
}

const scopeConfig: Record<
  ContextScope,
  { icon: React.ComponentType<{ className?: string }>; label: string; color: string }
> = {
  project: {
    icon: FolderOpen,
    label: "Project",
    color: "bg-blue-500/20 text-blue-400 border-blue-500/30",
  },
  user: {
    icon: User,
    label: "User",
    color: "bg-purple-500/20 text-purple-400 border-purple-500/30",
  },
  builtin: {
    icon: Package,
    label: "Built-in",
    color: "bg-gray-500/20 text-gray-400 border-gray-500/30",
  },
};

export function ContextCard({
  context,
  onEdit,
  onDelete,
  onDuplicate,
  onToggleEnabled,
  onApproveSync,
  onDismissSync,
  isProcessing = false,
}: ContextCardProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const [showMenu, setShowMenu] = useState(false);

  const scopeInfo = scopeConfig[context.scope];
  const ScopeIcon = scopeInfo.icon;

  // Check if this is a project context with pending web sync
  const isPendingSync = context.scope === "project" && context.webSyncStatus === "pending";

  const formatDate = (dateStr: string | null | undefined) => {
    if (!dateStr) return "Never";
    try {
      return new Date(dateStr).toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
        year: "numeric",
      });
    } catch {
      return dateStr;
    }
  };

  const formatRelativeTime = (dateStr: string | null | undefined) => {
    if (!dateStr) return "Never";
    try {
      const date = new Date(dateStr);
      const now = new Date();
      const diffMs = now.getTime() - date.getTime();
      const diffMins = Math.floor(diffMs / 60000);
      const diffHours = Math.floor(diffMins / 60);
      const diffDays = Math.floor(diffHours / 24);

      if (diffMins < 1) return "Just now";
      if (diffMins < 60) return `${diffMins}m ago`;
      if (diffHours < 24) return `${diffHours}h ago`;
      if (diffDays < 7) return `${diffDays}d ago`;
      return formatDate(dateStr);
    } catch {
      return dateStr;
    }
  };

  const truncateContent = (content: string, maxLength = 150) => {
    if (content.length <= maxLength) return content;
    return content.substring(0, maxLength).trim() + "...";
  };

  const hasAutoInclude =
    context.autoInclude &&
    (context.autoInclude.taskMentions?.length ||
      context.autoInclude.actionTypes?.length ||
      context.autoInclude.errorPatterns?.length ||
      context.autoInclude.filePatterns?.length);

  const canEdit = context.scope !== "builtin";
  const canDelete = context.scope !== "builtin";

  return (
    <div
      className={`rounded-lg border transition-all ${
        isPendingSync
          ? "border-blue-500/50 bg-card"
          : context.enabled
            ? "border-border bg-card hover:border-primary/30"
            : "border-border/50 bg-card/50 opacity-60"
      }`}
    >
      {/* Pending Sync Banner */}
      {isPendingSync && (
        <div className="flex items-center justify-between gap-3 px-3 py-2 bg-blue-500/10 border-b border-blue-500/30">
          <div className="flex items-center gap-2 text-sm text-blue-400">
            <Cloud className="w-4 h-4" />
            <span>Sync this context to qontinui-web?</span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => onApproveSync?.(context)}
              disabled={isProcessing}
              className="flex items-center gap-1 px-2 py-1 text-xs font-medium text-white bg-blue-500 hover:bg-blue-600 rounded transition-colors disabled:opacity-50"
              title="Approve sync to qontinui-web"
            >
              <Check className="w-3 h-3" />
              Sync
            </button>
            <button
              onClick={() => onDismissSync?.(context)}
              disabled={isProcessing}
              className="flex items-center gap-1 px-2 py-1 text-xs font-medium text-muted-foreground hover:text-foreground bg-muted hover:bg-muted/80 rounded transition-colors disabled:opacity-50"
              title="Keep local only"
            >
              <X className="w-3 h-3" />
              Dismiss
            </button>
          </div>
        </div>
      )}

      {/* Header */}
      <div className="p-3">
        <div className="flex items-start gap-3">
          {/* Context Icon */}
          <div
            className={`flex-shrink-0 p-2 rounded-lg ${
              context.enabled ? "bg-primary/10" : "bg-muted"
            }`}
          >
            <BookOpen
              className={`w-4 h-4 ${context.enabled ? "text-primary" : "text-muted-foreground"}`}
            />
          </div>

          {/* Content */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap">
              {/* Name */}
              <h4 className="font-medium text-sm text-foreground truncate">{context.name}</h4>

              {/* Scope Badge */}
              <span
                className={`px-1.5 py-0.5 rounded text-[10px] font-medium border ${scopeInfo.color}`}
              >
                <ScopeIcon className="w-3 h-3 inline-block mr-0.5" />
                {scopeInfo.label}
              </span>

              {/* Enabled/Disabled Badge */}
              <span
                className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${
                  context.enabled
                    ? "bg-green-500/20 text-green-400 border border-green-500/30"
                    : "bg-gray-500/20 text-gray-400 border border-gray-500/30"
                }`}
              >
                {context.enabled ? "Enabled" : "Disabled"}
              </span>

              {/* Auto-Include Indicator */}
              {hasAutoInclude && (
                <span
                  className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/30"
                  title="Has auto-include rules"
                >
                  <Zap className="w-3 h-3 inline-block" />
                </span>
              )}

              {/* Web Sync Status for Project Contexts */}
              {context.scope === "project" && context.webSyncStatus === "synced" && (
                <span
                  className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-green-500/20 text-green-400 border border-green-500/30"
                  title="Synced to qontinui-web"
                >
                  <Cloud className="w-3 h-3 inline-block mr-0.5" />
                  Synced
                </span>
              )}
              {context.scope === "project" && context.webSyncStatus === "dismissed" && (
                <span
                  className="px-1.5 py-0.5 rounded text-[10px] font-medium bg-gray-500/20 text-gray-400 border border-gray-500/30"
                  title="Local only (not synced to web)"
                >
                  <CloudOff className="w-3 h-3 inline-block mr-0.5" />
                  Local
                </span>
              )}
            </div>

            {/* Category */}
            {context.category && (
              <p className="text-xs text-muted-foreground mt-0.5 flex items-center gap-1">
                <Globe className="w-3 h-3" />
                {context.category}
              </p>
            )}

            {/* Preview Content */}
            <p className="text-xs text-muted-foreground mt-1 line-clamp-2">
              {truncateContent(context.content)}
            </p>

            {/* Tags */}
            {context.tags && context.tags.length > 0 && (
              <div className="flex items-center gap-1 mt-2 flex-wrap">
                <Tag className="w-3 h-3 text-muted-foreground" />
                {context.tags.slice(0, 5).map((tag) => (
                  <span
                    key={tag}
                    className="px-1.5 py-0.5 text-[10px] bg-muted border border-border rounded"
                  >
                    {tag}
                  </span>
                ))}
                {context.tags.length > 5 && (
                  <span className="text-[10px] text-muted-foreground">
                    +{context.tags.length - 5} more
                  </span>
                )}
              </div>
            )}

            {/* Stats */}
            <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
              <span className="flex items-center gap-1" title="Usage count">
                <TrendingUp className="w-3 h-3" />
                {context.useCount} uses
              </span>
              <span className="flex items-center gap-1" title="Last used">
                <Clock className="w-3 h-3" />
                {formatRelativeTime(context.lastUsedAt)}
              </span>
            </div>
          </div>

          {/* Actions */}
          <div className="flex items-center gap-1 flex-shrink-0">
            {/* Expand/Collapse */}
            <button
              onClick={() => setIsExpanded(!isExpanded)}
              className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
              title={isExpanded ? "Collapse" : "Expand"}
            >
              {isExpanded ? <ChevronUp className="w-4 h-4" /> : <ChevronDown className="w-4 h-4" />}
            </button>

            {/* Toggle Enabled */}
            <button
              onClick={() => onToggleEnabled(context)}
              disabled={isProcessing}
              className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors disabled:opacity-50"
              title={context.enabled ? "Disable" : "Enable"}
            >
              {context.enabled ? (
                <ToggleRight className="w-4 h-4 text-green-500" />
              ) : (
                <ToggleLeft className="w-4 h-4" />
              )}
            </button>

            {/* More Actions Menu */}
            <div className="relative">
              <button
                onClick={() => setShowMenu(!showMenu)}
                className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
              >
                <MoreVertical className="w-4 h-4" />
              </button>

              {showMenu && (
                <>
                  {/* Backdrop to close menu */}
                  <div className="fixed inset-0 z-10" onClick={() => setShowMenu(false)} />
                  {/* Menu */}
                  <div className="absolute right-0 top-full mt-1 z-20 w-40 bg-card border border-border rounded-lg shadow-lg py-1">
                    {canEdit && (
                      <button
                        onClick={() => {
                          setShowMenu(false);
                          onEdit(context);
                        }}
                        className="w-full flex items-center gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted/50 transition-colors"
                      >
                        <Edit2 className="w-4 h-4" />
                        Edit
                      </button>
                    )}
                    <button
                      onClick={() => {
                        setShowMenu(false);
                        onDuplicate(context);
                      }}
                      className="w-full flex items-center gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted/50 transition-colors"
                    >
                      <Copy className="w-4 h-4" />
                      Duplicate
                    </button>
                    {canDelete && (
                      <button
                        onClick={() => {
                          setShowMenu(false);
                          onDelete(context);
                        }}
                        className="w-full flex items-center gap-2 px-3 py-2 text-sm text-red-400 hover:bg-red-500/10 transition-colors"
                      >
                        <Trash2 className="w-4 h-4" />
                        Delete
                      </button>
                    )}
                  </div>
                </>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Expanded Content */}
      {isExpanded && (
        <div className="px-3 pb-3 border-t border-border/50 pt-3 space-y-3">
          {/* Full Content */}
          <div>
            <h5 className="text-xs font-medium text-muted-foreground mb-1 flex items-center gap-1">
              <FileText className="w-3 h-3" />
              Content
            </h5>
            <pre className="text-xs bg-muted/30 p-3 rounded-lg overflow-x-auto whitespace-pre-wrap font-mono max-h-60">
              {context.content}
            </pre>
          </div>

          {/* Auto-Include Rules */}
          {hasAutoInclude && (
            <div>
              <h5 className="text-xs font-medium text-muted-foreground mb-1 flex items-center gap-1">
                <Zap className="w-3 h-3" />
                Auto-Include Rules
              </h5>
              <div className="bg-muted/30 p-3 rounded-lg space-y-2 text-xs">
                {context.autoInclude?.taskMentions &&
                  context.autoInclude.taskMentions.length > 0 && (
                    <div>
                      <span className="text-muted-foreground">Task mentions: </span>
                      <span className="text-foreground">
                        {context.autoInclude.taskMentions.join(", ")}
                      </span>
                    </div>
                  )}
                {context.autoInclude?.actionTypes && context.autoInclude.actionTypes.length > 0 && (
                  <div>
                    <span className="text-muted-foreground">Action types: </span>
                    <span className="text-foreground">
                      {context.autoInclude.actionTypes.join(", ")}
                    </span>
                  </div>
                )}
                {context.autoInclude?.errorPatterns &&
                  context.autoInclude.errorPatterns.length > 0 && (
                    <div>
                      <span className="text-muted-foreground">Error patterns: </span>
                      <span className="text-foreground font-mono">
                        {context.autoInclude.errorPatterns.join(", ")}
                      </span>
                    </div>
                  )}
                {context.autoInclude?.filePatterns &&
                  context.autoInclude.filePatterns.length > 0 && (
                    <div>
                      <span className="text-muted-foreground">File patterns: </span>
                      <span className="text-foreground font-mono">
                        {context.autoInclude.filePatterns.join(", ")}
                      </span>
                    </div>
                  )}
              </div>
            </div>
          )}

          {/* Metadata */}
          <div className="flex items-center gap-4 text-xs text-muted-foreground">
            <span>Created: {formatDate(context.createdAt)}</span>
            <span>Modified: {formatDate(context.modifiedAt)}</span>
            <span>ID: {context.id.slice(0, 8)}...</span>
          </div>

          {/* Built-in Warning */}
          {context.scope === "builtin" && (
            <div className="flex items-center gap-2 text-xs text-amber-400 bg-amber-500/10 p-2 rounded">
              <AlertTriangle className="w-4 h-4" />
              Built-in contexts cannot be edited or deleted. Use duplicate to create an editable
              copy.
            </div>
          )}
        </div>
      )}
    </div>
  );
}
