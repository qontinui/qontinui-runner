/**
 * LibraryBuilderLayout
 *
 * Split-panel layout component for library builder pages.
 * Left panel: searchable item list. Right panel: editor form.
 * Designed to work with the useLibraryBuilder hook.
 */

import { useState } from "react";
import type { LucideIcon } from "lucide-react";
import { Plus, Search, Trash2, Play, Save, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button, ScrollArea, ConfirmDialog, EmptyState } from "@/components/ui";
import type { UseLibraryBuilderReturn } from "@/hooks/useLibraryBuilder";

export interface LibraryBuilderLayoutProps<T extends { id: string; name: string }> {
  title: string;
  icon: LucideIcon;
  builder: UseLibraryBuilderReturn<T>;
  renderListItem: (item: T, isSelected: boolean) => React.ReactNode;
  renderEditor: () => React.ReactNode;
  renderEmptyState?: () => React.ReactNode;
  actions?: React.ReactNode;
}

export function LibraryBuilderLayout<T extends { id: string; name: string }>({
  title,
  icon: Icon,
  builder,
  renderListItem,
  renderEditor,
  renderEmptyState,
  actions,
}: LibraryBuilderLayoutProps<T>) {
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  const hasSelection = builder.selectedItem !== null || builder.isCreating;

  const handleDelete = async () => {
    await builder.deleteSelected();
    setShowDeleteConfirm(false);
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-border">
        <div className="flex items-center gap-3">
          <Icon className="w-5 h-5 text-primary" />
          <h2 className="text-lg font-semibold">{title}</h2>
        </div>
        <div className="flex items-center gap-2">
          {actions}
          <Button size="sm" onClick={builder.startCreate}>
            <Plus className="w-4 h-4" />
            New
          </Button>
        </div>
      </div>

      {/* Error display */}
      {builder.error && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
          {builder.error}
        </div>
      )}

      {/* Main content: split panel */}
      <div className="flex flex-1 min-h-0">
        {/* Left panel: item list */}
        <div className="w-72 flex flex-col border-r border-border">
          {/* Search input */}
          <div className="p-3 border-b border-border">
            <div className="relative">
              <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
              <input
                type="text"
                placeholder="Search..."
                value={builder.searchQuery}
                onChange={(e) => builder.setSearchQuery(e.target.value)}
                className="w-full pl-9 pr-3 py-1.5 text-sm bg-muted/50 border border-border rounded-md
                  placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary
                  text-foreground"
              />
            </div>
          </div>

          {/* Scrollable item list */}
          <ScrollArea className="flex-1">
            <div className="p-2 space-y-1">
              {builder.filteredItems.length === 0 ? (
                <div className="px-3 py-6 text-center text-sm text-muted-foreground">
                  {builder.searchQuery ? "No matching items" : "No items yet"}
                </div>
              ) : (
                builder.filteredItems.map((item) => {
                  const isSelected = builder.selectedItem?.id === item.id;
                  return (
                    <button
                      key={item.id}
                      onClick={() => builder.selectItem(item)}
                      className={cn(
                        "w-full text-left rounded-md px-3 py-2 text-sm transition-colors",
                        isSelected
                          ? "bg-primary/15 text-foreground border border-primary/30"
                          : "text-muted-foreground hover:bg-muted/50 hover:text-foreground border border-transparent",
                      )}
                    >
                      {renderListItem(item, isSelected)}
                    </button>
                  );
                })
              )}
            </div>
          </ScrollArea>
        </div>

        {/* Right panel: editor */}
        <div className="flex-1 flex flex-col min-h-0">
          {hasSelection ? (
            <>
              {/* Editor form area */}
              <ScrollArea className="flex-1">
                <div className="p-6">{renderEditor()}</div>
              </ScrollArea>

              {/* Footer actions */}
              <div className="flex items-center justify-between px-6 py-3 border-t border-border bg-muted/30">
                <div className="flex items-center gap-2">
                  {!builder.isCreating && builder.selectedItem && (
                    <Button
                      variant="danger"
                      size="sm"
                      onClick={() => setShowDeleteConfirm(true)}
                      disabled={builder.loading}
                    >
                      <Trash2 className="w-4 h-4" />
                      Delete
                    </Button>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  {!builder.isCreating && builder.selectedItem && (
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => builder.runSelected()}
                      disabled={builder.loading}
                    >
                      <Play className="w-4 h-4" />
                      Run
                    </Button>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={builder.cancelEdit}
                    disabled={builder.loading}
                  >
                    Cancel
                  </Button>
                  <Button
                    size="sm"
                    onClick={builder.save}
                    disabled={builder.loading || !builder.isDirty}
                  >
                    {builder.loading ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Save className="w-4 h-4" />
                    )}
                    {builder.isCreating ? "Create" : "Save"}
                  </Button>
                </div>
              </div>
            </>
          ) : (
            <div className="flex-1 flex items-center justify-center">
              {renderEmptyState ? (
                renderEmptyState()
              ) : (
                <EmptyState
                  icon={<Icon className="w-12 h-12" />}
                  title={`No ${title.toLowerCase()} selected`}
                  description={`Select an item from the list or click "New" to create one.`}
                />
              )}
            </div>
          )}
        </div>
      </div>

      {/* Delete confirmation dialog */}
      <ConfirmDialog
        open={showDeleteConfirm}
        title="Delete Item"
        message={`Are you sure you want to delete "${builder.selectedItem?.name ?? "this item"}"?`}
        description="This action cannot be undone."
        variant="danger"
        confirmText="Delete"
        isLoading={builder.loading}
        onClose={() => setShowDeleteConfirm(false)}
        onConfirm={handleDelete}
      />
    </div>
  );
}
