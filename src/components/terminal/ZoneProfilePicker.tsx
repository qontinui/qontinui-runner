import { useState, useRef, useEffect, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Save, FolderOpen, Trash2, ChevronDown } from "lucide-react";
import { useUIComponent, UIBridgeComponentScope } from "@qontinui/ui-bridge";
// Profile shape + persistence live in `zoneProfileStorage` so the same
// profiles can be applied programmatically by name (`useZoneProfileRestore`)
// without duplicating this component's load/save logic.
import {
  loadProfilesFromDb,
  saveProfilesToDb,
  loadActiveProfileFromDb,
  saveActiveProfileToDb,
  profileSettingKey,
  activeProfileSettingKey,
  type ZoneProfile,
  type ZoneSessionInfo,
} from "./zoneProfileStorage";

interface ZoneProfilePickerProps {
  currentLayoutId: string;
  zoneLabels: Record<number, string>;
  zoneNotes: Record<number, string>;
  pinnedZones: Set<number>;
  autoApprovePatterns: string[];
  /** Current zone assignments (zoneIndex -> tabId) and tabs for session capture */
  zoneAssignments?: Record<number, string>;
  tabs?: Array<{ id: string; claudeSessionId?: string; claudeConfigDir?: string }>;
  /** Terminal page ID for storage namespacing */
  pageId?: string;
  /** Whether terminal initialization (reconnection) has completed */
  initialized?: boolean;
  onLoadProfile: (profile: ZoneProfile) => void;
}

const MAX_PROFILES = 10;

export function ZoneProfilePicker({
  currentLayoutId,
  zoneLabels,
  zoneNotes,
  pinnedZones,
  autoApprovePatterns,
  zoneAssignments,
  tabs,
  pageId = "default",
  initialized = false,
  onLoadProfile,
}: ZoneProfilePickerProps) {
  const [open, setOpen] = useState(false);
  const [profiles, setProfiles] = useState<Record<string, ZoneProfile>>({});
  const [saveName, setSaveName] = useState("");
  const [showSaveInput, setShowSaveInput] = useState(false);
  const [activeProfileName, setActiveProfileName] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const onLoadProfileRef = useRef(onLoadProfile);
  // Update ref in effect to avoid ref mutation during render
  useEffect(() => {
    onLoadProfileRef.current = onLoadProfile;
  });

  // Load profiles and active profile from database on mount, and refetch
  // when the backend emits `setting-changed` for keys we care about. This
  // lets external `setting_set` calls (automation, other windows) show up
  // without a runner restart.
  useEffect(() => {
    let cancelled = false;
    const profilesKey = profileSettingKey(pageId);
    const activeKey = activeProfileSettingKey(pageId);

    const refetch = async () => {
      const [profs, active] = await Promise.all([
        loadProfilesFromDb(pageId),
        loadActiveProfileFromDb(pageId),
      ]);
      if (cancelled) return;
      setProfiles(profs);
      setActiveProfileName(active);
      setLoaded(true);
    };

    refetch();

    let unlistenFn: UnlistenFn | null = null;
    listen<string>("setting-changed", (event) => {
      if (event.payload === profilesKey || event.payload === activeKey) {
        refetch();
      }
    })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else unlistenFn = unlisten;
      })
      .catch(() => {
        /* event bus not available — mount-only load remains the fallback */
      });

    return () => {
      cancelled = true;
      if (unlistenFn) unlistenFn();
    };
  }, [pageId]);

  // Auto-apply active profile only after terminal initialization completes
  // and only when no sessions were reconnected (tabs is empty).
  // When sessions were reconnected, useTerminalInitialization already restores
  // labels, notes, claude sessions from the saved layout — applying the profile
  // on top would create duplicate terminals and lose session state.
  const didAutoApply = useRef(false);
  useEffect(() => {
    if (!initialized || !loaded || didAutoApply.current) return;
    didAutoApply.current = true;
    if (activeProfileName && profiles[activeProfileName] && (!tabs || tabs.length === 0)) {
      onLoadProfileRef.current(profiles[activeProfileName]);
    }
  }, [initialized, loaded, activeProfileName, profiles, tabs]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
        setShowSaveInput(false);
        setSaveName("");
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const profileNames = Object.keys(profiles);

  const handleSave = useCallback(() => {
    const name = saveName.trim();
    if (!name) return;
    if (profileNames.length >= MAX_PROFILES && !profiles[name]) return;

    // Capture Claude session IDs from current zone assignments
    const sessions: ZoneSessionInfo[] = [];
    if (zoneAssignments && tabs) {
      const tabsById = new Map(tabs.map((t) => [t.id, t]));
      for (const [zoneIdxStr, tabId] of Object.entries(zoneAssignments)) {
        const tab = tabsById.get(tabId);
        if (tab?.claudeSessionId) {
          sessions.push({
            zoneIndex: Number(zoneIdxStr),
            claudeSessionId: tab.claudeSessionId,
            claudeConfigDir: tab.claudeConfigDir,
          });
        }
      }
    }

    const profile: ZoneProfile = {
      layoutId: currentLayoutId,
      labels: { ...zoneLabels },
      notes: { ...zoneNotes },
      pins: [...pinnedZones],
      autoApprovePatterns: [...autoApprovePatterns],
      sessions: sessions.length > 0 ? sessions : undefined,
    };
    const updated = { ...profiles, [name]: profile };
    setProfiles(updated);
    saveProfilesToDb(updated, pageId);
    setActiveProfileName(name);
    saveActiveProfileToDb(name, pageId);
    setShowSaveInput(false);
    setSaveName("");
  }, [
    saveName,
    profileNames.length,
    profiles,
    zoneAssignments,
    tabs,
    currentLayoutId,
    zoneLabels,
    zoneNotes,
    pinnedZones,
    autoApprovePatterns,
    pageId,
  ]);

  const handleLoad = useCallback(
    (name: string) => {
      const profile = profiles[name];
      if (profile) {
        onLoadProfile(profile);
        setActiveProfileName(name);
        saveActiveProfileToDb(name, pageId);
        setOpen(false);
      }
    },
    [profiles, onLoadProfile, pageId],
  );

  const handleDelete = useCallback(
    (name: string) => {
      const updated = { ...profiles };
      delete updated[name];
      setProfiles(updated);
      saveProfilesToDb(updated, pageId);
      if (activeProfileName === name) {
        setActiveProfileName(null);
        saveActiveProfileToDb(null, pageId);
      }
    },
    [profiles, activeProfileName, pageId],
  );

  // ── UI Bridge registration ──────────────────────────────────────────────
  // Registered here (always in the tree from TerminalPage) so agents can
  // invoke profile actions without interacting with DOM elements.
  useUIComponent({
    id: "zone-profile-picker",
    name: "Zone Profile Picker",
    description:
      "Saves and loads named zone-layout profiles (labels, notes, pins, sessions). " +
      "Use load-profile / save-profile / delete-profile to manage profiles programmatically.",
    // NO `open` / `close` actions — same reason as `zone-layout-picker`: this
    // component mounts inside a `display: none` container, so an action that
    // reports success for "opened the dropdown" is reporting on a dropdown
    // that does not render. The profile surface is fully reachable through
    // load-profile / save-profile / delete-profile.
    actions: [
      {
        id: "load-profile",
        label: "Load Profile",
        description: "Apply a saved zone profile (layout, labels, sessions) by name.",
        paramSchema: { name: "string" },
        // `destructive` — `useApplyZoneProfile` REPLACES the live layout, labels, notes,
        // pins and `autoApprovePatterns` (an agent auto-approval allow-list) with the
        // profile's, spawns terminals to fill its zones, and queues `claude --resume` into
        // them. Dim 1: the arrangement it overwrites was never saved anywhere. Dim 3: it
        // widens or narrows auto-approval silently, which is the worst quadrant.
        effect: "destructive",
        handler: (params?: unknown) => {
          const { name } = (params ?? {}) as { name?: string };
          if (!name) throw new Error("load-profile requires { name: string }");
          const profile = profiles[name];
          if (!profile)
            throw new Error(
              `Profile not found: "${name}". Available: ${Object.keys(profiles).join(", ") || "none"}`,
            );
          handleLoad(name);
        },
      },
      {
        id: "save-profile",
        label: "Save Profile",
        // The old text ("Capture current layout + zone assignments as a named
        // profile.") was INCOMPLETE rather than wrong: it disclosed neither the
        // full capture set nor the two side effects — an existing profile of the
        // same name is overwritten, and the profile becomes the page's ACTIVE
        // one (`setActiveProfileName` + `saveActiveProfileToDb` below), which a
        // later boot then auto-applies. Undisclosed side effects are exactly
        // what an effect classification has to be established from
        // [policy: the-ui-bridge-is-the-instrument].
        description:
          "Capture the current layout, zone assignments, labels, notes, pins, " +
          "auto-approve patterns and Claude session bindings as a named profile, and " +
          "make it the page's ACTIVE profile (a later boot re-applies it). " +
          "OVERWRITES an existing profile of the same name without confirmation.",
        paramSchema: { name: "string" },
        // `destructive` — writing to an EXISTING name overwrites that profile with no
        // confirmation and no copy of what it replaced (dim 1 + dim 3), and it also makes
        // the profile active — see the description above, which used to omit that.
        effect: "destructive",
        handler: (params?: unknown) => {
          const { name } = (params ?? {}) as { name?: string };
          if (!name) throw new Error("save-profile requires { name: string }");
          if (profileNames.length >= MAX_PROFILES && !profiles[name]) {
            throw new Error(`Cannot save: maximum of ${MAX_PROFILES} profiles reached`);
          }
          // Capture Claude session IDs from current zone assignments
          const sessions: ZoneSessionInfo[] = [];
          if (zoneAssignments && tabs) {
            const tabsById = new Map(tabs.map((t) => [t.id, t]));
            for (const [zoneIdxStr, tabId] of Object.entries(zoneAssignments)) {
              const tab = tabsById.get(tabId);
              if (tab?.claudeSessionId) {
                sessions.push({
                  zoneIndex: Number(zoneIdxStr),
                  claudeSessionId: tab.claudeSessionId,
                  claudeConfigDir: tab.claudeConfigDir,
                });
              }
            }
          }
          const profile: ZoneProfile = {
            layoutId: currentLayoutId,
            labels: { ...zoneLabels },
            notes: { ...zoneNotes },
            pins: [...pinnedZones],
            autoApprovePatterns: [...autoApprovePatterns],
            sessions: sessions.length > 0 ? sessions : undefined,
          };
          const updated = { ...profiles, [name]: profile };
          setProfiles(updated);
          saveProfilesToDb(updated, pageId);
          setActiveProfileName(name);
          saveActiveProfileToDb(name, pageId);
        },
      },
      {
        id: "delete-profile",
        label: "Delete Profile",
        description: "Remove a saved profile by name. Clears active-profile if it matches.",
        paramSchema: { name: "string" },
        // `destructive` — removes a saved profile from the page's persisted settings.
        // There is no undo and no recycle bin.
        effect: "destructive",
        handler: (params?: unknown) => {
          const { name } = (params ?? {}) as { name?: string };
          if (!name) throw new Error("delete-profile requires { name: string }");
          if (!profiles[name]) throw new Error(`Profile not found: "${name}"`);
          handleDelete(name);
        },
      },
      {
        id: "list-profiles",
        label: "List Profiles",
        // `read` — returns the profile names and the active one. A query.
        effect: "read",
        handler: () => ({
          profiles: profileNames,
          active: activeProfileName,
        }),
      },
    ],
  });

  if (!loaded) return null;

  return (
    <UIBridgeComponentScope componentId="zone-profile-picker">
      <div className="relative shrink-0" ref={ref}>
        <button
          onClick={() => setOpen(!open)}
          aria-label="Zone profiles"
          aria-haspopup="menu"
          aria-expanded={open}
          className={`flex items-center gap-0.5 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
            open
              ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
              : activeProfileName
                ? "text-[#7aa2f7] hover:text-[#7aa2f7] hover:bg-[#7aa2f7]/10"
                : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title={
            activeProfileName
              ? `Profile: ${activeProfileName}`
              : "Zone profiles — save/load configurations"
          }
        >
          <FolderOpen className="w-3 h-3" />
          {activeProfileName && <span className="max-w-[80px] truncate">{activeProfileName}</span>}
          <ChevronDown className="w-2.5 h-2.5" />
        </button>

        {open && (
          <div className="absolute left-0 top-full mt-1 w-56 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
            <div className="px-3 py-1.5 text-[9px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
              Zone Profiles ({profileNames.length}/{MAX_PROFILES})
            </div>

            {/* Save current */}
            <div className="px-2 py-1.5 border-b border-[#2a2d3d]">
              {showSaveInput ? (
                <div className="flex items-center gap-1">
                  <input
                    id="zone-profile-name-input"
                    autoFocus
                    value={saveName}
                    onChange={(e) => setSaveName(e.target.value)}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") handleSave();
                      if (e.key === "Escape") {
                        setShowSaveInput(false);
                        setSaveName("");
                      }
                    }}
                    placeholder="Profile name..."
                    className="flex-1 bg-[#13141f] border border-[#2a2d3d] rounded px-1.5 py-0.5 text-[10px] text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7]"
                    maxLength={30}
                  />
                  <button
                    id="zone-profile-save-btn"
                    onClick={handleSave}
                    disabled={!saveName.trim()}
                    className="p-0.5 rounded text-[#9ece6a] hover:bg-[#9ece6a]/10 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                    title="Save profile"
                  >
                    <Save className="w-3 h-3" />
                  </button>
                </div>
              ) : (
                <button
                  id="zone-profile-save-current"
                  onClick={() => setShowSaveInput(true)}
                  disabled={profileNames.length >= MAX_PROFILES}
                  className="flex items-center gap-1.5 w-full text-left px-1 py-0.5 text-[10px] text-[#9ece6a] hover:bg-[#9ece6a]/10 rounded transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
                >
                  <Save className="w-3 h-3" />
                  Save current configuration
                </button>
              )}
            </div>

            {/* Saved profiles */}
            <div className="max-h-48 overflow-y-auto scrollbar-dark">
              {profileNames.length === 0 ? (
                <div className="px-3 py-3 text-center text-[10px] text-[#565f89]">
                  No saved profiles
                </div>
              ) : (
                profileNames.map((name) => {
                  const p = profiles[name];
                  const labelCount = Object.keys(p.labels).filter(
                    (k) => p.labels[Number(k)],
                  ).length;
                  return (
                    <div
                      key={name}
                      className={`flex items-center gap-1.5 px-2 py-1.5 hover:bg-[#2a2d3d]/50 transition-colors group ${
                        name === activeProfileName ? "bg-[#7aa2f7]/5" : ""
                      }`}
                    >
                      <button onClick={() => handleLoad(name)} className="flex-1 min-w-0 text-left">
                        <div className="text-[11px] text-[#c0caf5] truncate">{name}</div>
                        <div className="text-[9px] text-[#565f89]">
                          {p.layoutId} · {labelCount} label{labelCount !== 1 ? "s" : ""}
                          {p.pins.length > 0 &&
                            ` · ${p.pins.length} pin${p.pins.length !== 1 ? "s" : ""}`}
                          {p.sessions && p.sessions.length > 0 && (
                            <span className="text-[#7aa2f7]">
                              {" "}
                              · {p.sessions.length} session{p.sessions.length !== 1 ? "s" : ""}
                            </span>
                          )}
                        </div>
                      </button>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleDelete(name);
                        }}
                        className="p-0.5 rounded text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10 opacity-0 group-hover:opacity-100 transition-all shrink-0"
                        title="Delete profile"
                      >
                        <Trash2 className="w-3 h-3" />
                      </button>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        )}
      </div>
    </UIBridgeComponentScope>
  );
}
