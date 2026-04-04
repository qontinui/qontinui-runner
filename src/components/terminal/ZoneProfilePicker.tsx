import { useState, useRef, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Save, FolderOpen, Trash2, ChevronDown } from "lucide-react";

export interface ZoneSessionInfo {
  zoneIndex: number;
  claudeSessionId: string;
  claudeConfigDir?: string;
}

interface ZoneProfile {
  layoutId: string;
  labels: Record<number, string>;
  notes: Record<number, string>;
  pins: number[];
  autoApprovePatterns: string[];
  /** Claude session IDs per zone (added in later version, may be absent in old profiles) */
  sessions?: ZoneSessionInfo[];
}

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
  onLoadProfile: (profile: ZoneProfile) => void;
}

const MAX_PROFILES = 10;

function profileSettingKey(pageId: string): string {
  return pageId === "default" ? "zone-profiles" : `zone-profiles:${pageId}`;
}

function activeProfileSettingKey(pageId: string): string {
  return pageId === "default" ? "zone-active-profile" : `zone-active-profile:${pageId}`;
}

async function loadProfilesFromDb(pageId: string): Promise<Record<string, ZoneProfile>> {
  try {
    const result = await invoke<Record<string, ZoneProfile> | null>("setting_get", {
      key: profileSettingKey(pageId),
    });
    return result ?? {};
  } catch {
    return {};
  }
}

async function saveProfilesToDb(profiles: Record<string, ZoneProfile>, pageId: string) {
  try {
    await invoke("setting_set", {
      key: profileSettingKey(pageId),
      value: profiles,
    });
  } catch {
    /* ignore save failures */
  }
}

async function loadActiveProfileFromDb(pageId: string): Promise<string | null> {
  try {
    const result = await invoke<string | null>("setting_get", {
      key: activeProfileSettingKey(pageId),
    });
    return result ?? null;
  } catch {
    return null;
  }
}

async function saveActiveProfileToDb(name: string | null, pageId: string) {
  try {
    await invoke("setting_set", {
      key: activeProfileSettingKey(pageId),
      value: name,
    });
  } catch {
    /* ignore save failures */
  }
}

export function ZoneProfilePicker({
  currentLayoutId,
  zoneLabels,
  zoneNotes,
  pinnedZones,
  autoApprovePatterns,
  zoneAssignments,
  tabs,
  pageId = "default",
  onLoadProfile,
}: ZoneProfilePickerProps) {
  const [open, setOpen] = useState(false);
  const [profiles, setProfiles] = useState<Record<string, ZoneProfile>>({});
  const [saveName, setSaveName] = useState("");
  const [showSaveInput, setShowSaveInput] = useState(false);
  const [activeProfileName, setActiveProfileName] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Load profiles and active profile from database on mount
  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [profs, active] = await Promise.all([
        loadProfilesFromDb(pageId),
        loadActiveProfileFromDb(pageId),
      ]);
      if (cancelled) return;
      setProfiles(profs);
      setActiveProfileName(active);
      setLoaded(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [pageId]);

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

  if (!loaded) return null;

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
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
                const labelCount = Object.keys(p.labels).filter((k) => p.labels[Number(k)]).length;
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
  );
}
