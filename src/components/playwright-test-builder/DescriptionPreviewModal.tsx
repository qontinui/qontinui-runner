import { X, Sparkles, CheckCircle, ChevronDown } from "lucide-react";
import { getAccentColors } from "@/design-system";

interface DescriptionPreviewModalProps {
  currentDescription: string;
  previewDescription: string;
  onAccept: () => void;
  onReject: () => void;
}

export function DescriptionPreviewModal({
  currentDescription,
  previewDescription,
  onAccept,
  onReject,
}: DescriptionPreviewModalProps) {
  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4">
      <div className="bg-card border border-border rounded-lg shadow-xl max-w-2xl w-full max-h-[80vh] overflow-hidden">
        <div className="px-4 py-3 border-b border-border flex items-center justify-between">
          <h3 className="text-lg font-semibold flex items-center gap-2">
            <Sparkles className={`w-5 h-5 ${getAccentColors("blue").text}`} />
            Generated Description
          </h3>
          <button onClick={onReject} className="p-1 hover:bg-muted rounded transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        <div className="p-4 space-y-4 overflow-y-auto max-h-[60vh]">
          <div>
            <label className="block text-sm font-medium text-muted-foreground mb-2">
              Current Description
            </label>
            <div className="p-3 bg-muted/30 border border-border rounded-lg text-sm">
              {currentDescription || (
                <span className="text-muted-foreground italic">No description</span>
              )}
            </div>
          </div>

          <div className="flex justify-center">
            <ChevronDown className="w-6 h-6 text-muted-foreground" />
          </div>

          <div>
            <label className={`block text-sm font-medium ${getAccentColors("blue").text} mb-2`}>
              New Description (from code)
            </label>
            <div
              className={`p-3 ${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg text-sm`}
            >
              {previewDescription}
            </div>
          </div>
        </div>

        <div className="px-4 py-3 border-t border-border flex justify-end gap-2">
          <button
            onClick={onReject}
            className="px-4 py-2 text-sm bg-muted hover:bg-muted/80 rounded-lg transition-colors"
          >
            Keep Original
          </button>
          <button
            onClick={onAccept}
            className={`px-4 py-2 text-sm ${getAccentColors("blue").bgSolid} text-white hover:opacity-90 rounded-lg transition-colors flex items-center gap-2`}
          >
            <CheckCircle className="w-4 h-4" />
            Use New Description
          </button>
        </div>
      </div>
    </div>
  );
}
