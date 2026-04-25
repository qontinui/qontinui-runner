/**
 * WrapperCard — single card in the Installed grid.
 *
 * Shows the wrapper at-a-glance plus controls:
 *   - icon + name + version
 *   - description
 *   - transport badge + status badge + action count
 *   - three-dot menu: Start / Stop / Update / Uninstall
 *   - "Open" button → triggers `onOpen(wrapperId)`
 *
 * The card is intentionally stateless; lifecycle actions and refresh logic
 * live in the parent (`InstalledTab`) so multi-card concerns (optimistic UI,
 * cross-card highlight after install) stay in one place.
 */

import { useState, useRef, useEffect } from "react";
import { Package, MoreVertical, Play, Square, RefreshCw, Trash2, ArrowRight } from "lucide-react";
import { StatusBadge } from "./StatusBadge";
import { TransportBadge } from "./TransportBadge";
import type { InstalledWrapper, WrapperStatus } from "@/lib/wrappers/types";

export interface WrapperCardProps {
  wrapper: InstalledWrapper;
  status: WrapperStatus;
  busy?: boolean;
  highlighted?: boolean;
  onOpen: (id: string) => void;
  onStart: (id: string) => void;
  onStop: (id: string) => void;
  onUpdate: (id: string) => void;
  onUninstall: (id: string) => void;
}

export function WrapperCard({
  wrapper,
  status,
  busy = false,
  highlighted = false,
  onOpen,
  onStart,
  onStop,
  onUpdate,
  onUninstall,
}: WrapperCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [menuOpen]);

  const isRunning = status === "running" || status === "degraded";
  const actionCount = wrapper.actions?.length ?? 0;

  return (
    <div
      className={`flex flex-col rounded-xl border bg-card shadow-sm transition-all ${
        highlighted
          ? "border-cyan-500/60 ring-2 ring-cyan-500/30 shadow-md"
          : "border-border hover:border-border/80 hover:shadow-md"
      }`}
    >
      <div className="flex items-start gap-3 p-5">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-cyan-500/30 bg-cyan-500/10">
          <Package className="w-4 h-4 text-cyan-400" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0">
              <h3 className="text-sm font-semibold text-foreground truncate">
                {wrapper.manifest?.displayName ?? wrapper.id}
              </h3>
              <p className="text-xs text-muted-foreground font-mono truncate">
                {wrapper.packageName}
                <span className="ml-1 text-muted-foreground/70">v{wrapper.version}</span>
              </p>
            </div>
            <div className="relative" ref={menuRef}>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setMenuOpen((v) => !v);
                }}
                disabled={busy}
                aria-label="Wrapper actions"
                className="p-1.5 rounded-md text-muted-foreground hover:text-foreground hover:bg-muted/50 transition-colors disabled:opacity-50"
              >
                <MoreVertical className="w-4 h-4" />
              </button>
              {menuOpen && (
                <div
                  className="absolute right-0 top-full mt-1 z-20 min-w-[160px] rounded-md border border-border bg-card shadow-lg py-1"
                  role="menu"
                >
                  {isRunning ? (
                    <MenuItem
                      icon={Square}
                      label="Stop"
                      onClick={() => {
                        setMenuOpen(false);
                        onStop(wrapper.id);
                      }}
                    />
                  ) : (
                    <MenuItem
                      icon={Play}
                      label="Start"
                      onClick={() => {
                        setMenuOpen(false);
                        onStart(wrapper.id);
                      }}
                    />
                  )}
                  <MenuItem
                    icon={RefreshCw}
                    label="Update"
                    onClick={() => {
                      setMenuOpen(false);
                      onUpdate(wrapper.id);
                    }}
                  />
                  <MenuItem
                    icon={Trash2}
                    label="Uninstall"
                    danger
                    onClick={() => {
                      setMenuOpen(false);
                      onUninstall(wrapper.id);
                    }}
                  />
                </div>
              )}
            </div>
          </div>

          {wrapper.manifest?.description && (
            <p className="mt-2 text-xs text-muted-foreground line-clamp-2">
              {wrapper.manifest.description}
            </p>
          )}

          <div className="mt-3 flex items-center gap-2 flex-wrap">
            <TransportBadge transport={wrapper.manifest?.transport ?? "api"} />
            <StatusBadge status={status} />
            <span className="text-[11px] text-muted-foreground">
              {actionCount} action{actionCount === 1 ? "" : "s"}
            </span>
          </div>
        </div>
      </div>

      <div className="border-t border-border px-5 py-3 flex items-center justify-end">
        <button
          onClick={() => onOpen(wrapper.id)}
          className="inline-flex items-center gap-1.5 text-xs font-medium text-cyan-400 hover:text-cyan-300 transition-colors"
        >
          Open
          <ArrowRight className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

interface MenuItemProps {
  icon: typeof Play;
  label: string;
  onClick: () => void;
  danger?: boolean;
}
function MenuItem({ icon: Icon, label, onClick, danger }: MenuItemProps) {
  return (
    <button
      onClick={onClick}
      role="menuitem"
      className={`w-full px-3 py-1.5 text-left text-xs flex items-center gap-2 transition-colors ${
        danger ? "text-red-400 hover:bg-red-500/10" : "text-foreground hover:bg-muted/50"
      }`}
    >
      <Icon className="w-3.5 h-3.5" />
      {label}
    </button>
  );
}
