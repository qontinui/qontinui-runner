import { useState, useRef, useEffect } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { Save, FolderOpen, Trash2, ChevronDown } from "lucide-react";

interface ZoneProfile {
  layoutId: string;
  labels: Record<number, string>;
  notes: Record<number, string>;
  pins: number[];
  autoApprovePatterns: string[];
}

interface ZoneProfilePickerProps {
  currentLayoutId: string;
  zoneLabels: Record<number, string>;
  zoneNotes: Record<number, string>;
  pinnedZones: Set<number>;
  autoApprovePatterns: string[];
  onLoadProfile: (profile: ZoneProfile) => void;
}

const STORAGE_KEY = "zone-profiles";
const MAX_PROFILES = 10;

function loadProfiles(): Record<string, ZoneProfile> {
  return instanceStorage.getJSON<Record<string, ZoneProfile>>(STORAGE_KEY, {});
}

function saveProfiles(profiles: Record<string, ZoneProfile>) {
  instanceStorage.setJSON(STORAGE_KEY, profiles);
}

export function ZoneProfilePicker({
  currentLayoutId,
  zoneLabels,
  zoneNotes,
  pinnedZones,
  autoApprovePatterns,
  onLoadProfile,
}: ZoneProfilePickerProps) {
  const [open, setOpen] = useState(false);
  const [profiles, setProfiles] = useState(loadProfiles);
  const [saveName, setSaveName] = useState("");
  const [showSaveInput, setShowSaveInput] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

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

  const handleSave = () => {
    const name = saveName.trim();
    if (!name) return;
    if (profileNames.length >= MAX_PROFILES && !profiles[name]) {
      // At limit, don't save
      return;
    }
    const profile: ZoneProfile = {
      layoutId: currentLayoutId,
      labels: { ...zoneLabels },
      notes: { ...zoneNotes },
      pins: [...pinnedZones],
      autoApprovePatterns: [...autoApprovePatterns],
    };
    const updated = { ...profiles, [name]: profile };
    setProfiles(updated);
    saveProfiles(updated);
    setShowSaveInput(false);
    setSaveName("");
  };

  const handleLoad = (name: string) => {
    const profile = profiles[name];
    if (profile) {
      onLoadProfile(profile);
      setOpen(false);
    }
  };

  const handleDelete = (name: string) => {
    const updated = { ...profiles };
    delete updated[name];
    setProfiles(updated);
    saveProfiles(updated);
  };

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`flex items-center gap-0.5 px-1.5 py-0.5 rounded text-[10px] transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title="Zone profiles — save/load configurations"
      >
        <FolderOpen className="w-3 h-3" />
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
                  onClick={handleSave}
                  disabled={!saveName.trim()}
                  className="p-0.5 rounded text-[#9ece6a] hover:bg-[#9ece6a]/10 disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                >
                  <Save className="w-3 h-3" />
                </button>
              </div>
            ) : (
              <button
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
                    className="flex items-center gap-1.5 px-2 py-1.5 hover:bg-[#2a2d3d]/50 transition-colors group"
                  >
                    <button onClick={() => handleLoad(name)} className="flex-1 min-w-0 text-left">
                      <div className="text-[11px] text-[#c0caf5] truncate">{name}</div>
                      <div className="text-[9px] text-[#565f89]">
                        {p.layoutId} · {labelCount} label{labelCount !== 1 ? "s" : ""} ·{" "}
                        {p.pins.length} pin{p.pins.length !== 1 ? "s" : ""}
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
