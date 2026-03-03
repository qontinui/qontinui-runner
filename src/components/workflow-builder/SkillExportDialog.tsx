/**
 * SkillExportDialog.tsx
 *
 * Modal for selecting skills to export.
 */

interface SkillForExport {
  id: string;
  name: string;
  source: string;
}

interface SkillExportDialogProps {
  isOpen: boolean;
  onClose: () => void;
  availableSkills: SkillForExport[];
  selectedIds: Set<string>;
  onToggleId: (id: string) => void;
  onSelectAll: (ids: Set<string>) => void;
  onExport: () => void;
}

export function SkillExportDialog({
  isOpen,
  onClose,
  availableSkills,
  selectedIds,
  onToggleId,
  onSelectAll,
  onExport,
}: SkillExportDialogProps) {
  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-[400px] max-h-[500px] flex flex-col">
        <div className="px-4 py-3 border-b border-zinc-700">
          <h3 className="text-sm font-medium text-zinc-200">Export Skills</h3>
          <p className="text-xs text-zinc-500 mt-0.5">Select skills to export</p>
        </div>
        <div className="flex-1 overflow-y-auto px-4 py-2 space-y-1">
          <label className="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-zinc-800 cursor-pointer">
            <input
              type="checkbox"
              checked={selectedIds.size === availableSkills.length}
              onChange={(e) => {
                if (e.target.checked) {
                  onSelectAll(new Set(availableSkills.map((s) => s.id)));
                } else {
                  onSelectAll(new Set());
                }
              }}
              className="rounded border-zinc-600"
            />
            <span className="text-xs text-zinc-400">Select All</span>
          </label>
          {availableSkills.map((skill) => (
            <label
              key={skill.id}
              className="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-zinc-800 cursor-pointer"
            >
              <input
                type="checkbox"
                checked={selectedIds.has(skill.id)}
                onChange={() => onToggleId(skill.id)}
                className="rounded border-zinc-600"
              />
              <span className="text-sm text-zinc-200">{skill.name}</span>
              {skill.source === "community" && (
                <span className="px-1.5 py-0.5 text-[10px] rounded-full bg-purple-900/30 text-purple-400">
                  Community
                </span>
              )}
            </label>
          ))}
        </div>
        <div className="px-4 py-3 border-t border-zinc-700 flex justify-end gap-2">
          <button
            className="px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 transition-colors rounded"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            onClick={onExport}
            disabled={selectedIds.size === 0}
          >
            Export {selectedIds.size} Skill
            {selectedIds.size !== 1 ? "s" : ""}
          </button>
        </div>
      </div>
    </div>
  );
}
