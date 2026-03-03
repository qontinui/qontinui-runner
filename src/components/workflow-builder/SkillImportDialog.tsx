/**
 * SkillImportDialog.tsx
 *
 * Modal for selecting import conflict resolution mode.
 */

interface SkillImportDialogProps {
  isOpen: boolean;
  onClose: () => void;
  skillCount: number;
  conflictMode: "skip" | "overwrite";
  onSetConflictMode: (mode: "skip" | "overwrite") => void;
  onConfirm: () => void;
}

export function SkillImportDialog({
  isOpen,
  onClose,
  skillCount,
  conflictMode,
  onSetConflictMode,
  onConfirm,
}: SkillImportDialogProps) {
  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-[380px]">
        <div className="px-4 py-3 border-b border-zinc-700">
          <h3 className="text-sm font-medium text-zinc-200">Import Skills</h3>
          <p className="text-xs text-zinc-500 mt-0.5">
            {skillCount} skill{skillCount !== 1 ? "s" : ""} found in file
          </p>
        </div>
        <div className="px-4 py-3 space-y-2">
          <p className="text-xs text-zinc-400">When a skill with the same name already exists:</p>
          <label className="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-zinc-800 cursor-pointer">
            <input
              type="radio"
              name="conflictMode"
              checked={conflictMode === "skip"}
              onChange={() => onSetConflictMode("skip")}
              className="text-blue-600"
            />
            <div>
              <span className="text-sm text-zinc-200">Skip</span>
              <p className="text-xs text-zinc-500">
                Keep the existing skill, ignore the imported one
              </p>
            </div>
          </label>
          <label className="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-zinc-800 cursor-pointer">
            <input
              type="radio"
              name="conflictMode"
              checked={conflictMode === "overwrite"}
              onChange={() => onSetConflictMode("overwrite")}
              className="text-blue-600"
            />
            <div>
              <span className="text-sm text-zinc-200">Overwrite</span>
              <p className="text-xs text-zinc-500">
                Replace existing skills with imported versions
              </p>
            </div>
          </label>
        </div>
        <div className="px-4 py-3 border-t border-zinc-700 flex justify-end gap-2">
          <button
            className="px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 transition-colors rounded"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="px-3 py-1.5 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded transition-colors"
            onClick={onConfirm}
          >
            Import
          </button>
        </div>
      </div>
    </div>
  );
}
