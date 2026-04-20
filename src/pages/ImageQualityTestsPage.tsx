/**
 * ImageQualityTestsPage.tsx
 *
 * Dev-mode page for viewing and managing image quality test images.
 * Displays test images grouped by category with metadata from the manifest.
 * Supports uploading new test images via drag-and-drop or file picker.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import {
  Upload,
  RefreshCw,
  Trash2,
  Image as ImageIcon,
  AlertCircle,
  ChevronDown,
  ChevronRight,
  X,
  Pencil,
  Check,
} from "lucide-react";

// ============================================================================
// Types
// ============================================================================

interface SourceDimensions {
  width: number;
  height: number;
}

interface TestImageEntry {
  file: string;
  expected_label: string;
  expected_text?: string;
  issue: string;
  description: string;
  element_type?: string;
  source_dimensions?: SourceDimensions;
  expected_score_range?: [number, number];
}

interface Manifest {
  version: string;
  description?: string;
  images: TestImageEntry[];
}

const CATEGORIES = [
  "good",
  "clipped",
  "invisible-text",
  "wrong-content",
  "blank",
  "shifted",
] as const;

type Category = (typeof CATEGORIES)[number];

const CATEGORY_COLORS: Record<
  Category,
  { variant: "success" | "danger" | "warning" | "info" | "purple" | "muted"; label: string }
> = {
  good: { variant: "success", label: "Good" },
  clipped: { variant: "warning", label: "Clipped" },
  "invisible-text": { variant: "danger", label: "Invisible Text" },
  "wrong-content": { variant: "danger", label: "Wrong Content" },
  blank: { variant: "muted", label: "Blank" },
  shifted: { variant: "purple", label: "Shifted" },
};

// ============================================================================
// Image Card Component
// ============================================================================

function ImageCard({
  entry,
  onDelete,
  onUpdate,
}: {
  entry: TestImageEntry;
  onDelete: (file: string) => void;
  onUpdate: (file: string, updates: Partial<TestImageEntry>) => Promise<void>;
}) {
  const [imageData, setImageData] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);

  // Edit state — initialized when entering edit mode
  const [editCategory, setEditCategory] = useState("");
  const [editLabel, setEditLabel] = useState("");
  const [editIssue, setEditIssue] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editElementType, setEditElementType] = useState("");
  const [editExpectedText, setEditExpectedText] = useState("");

  const category = entry.file.split("/")[0] as Category;
  const filename = entry.file.split("/").pop() || entry.file;
  const categoryInfo = CATEGORY_COLORS[category] || CATEGORY_COLORS.good;

  useEffect(() => {
    let cancelled = false;

    async function loadImage() {
      try {
        setLoading(true);
        setError(null);
        const resp = await tracedFetch(`${getApiBase()}/image-quality-tests/image/${entry.file}`);
        if (!resp.ok) {
          throw new Error(`HTTP ${resp.status}`);
        }
        const data = await resp.json();
        if (!cancelled && data.success && data.data?.base64) {
          setImageData(`data:image/png;base64,${data.data.base64}`);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "Failed to load");
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    loadImage();
    return () => {
      cancelled = true;
    };
  }, [entry.file]);

  const startEditing = useCallback(() => {
    setEditCategory(entry.file.split("/")[0]);
    setEditLabel(entry.expected_label);
    setEditIssue(entry.issue);
    setEditDescription(entry.description);
    setEditElementType(entry.element_type || "");
    setEditExpectedText(entry.expected_text || "");
    setEditing(true);
  }, [entry]);

  const handleSave = async () => {
    setSaving(true);
    try {
      const updates: Record<string, unknown> = {
        expected_label: editLabel,
        expected_text: editExpectedText || undefined,
        issue: editIssue,
        description: editDescription,
        element_type: editElementType || undefined,
      };
      // Include category if changed (backend moves the file)
      const currentCategory = entry.file.split("/")[0];
      if (editCategory !== currentCategory) {
        updates.category = editCategory;
      }
      await onUpdate(entry.file, updates as Partial<TestImageEntry>);
      setEditing(false);
    } catch {
      // Error already shown by handleUpdate alert
    } finally {
      setSaving(false);
    }
  };

  const handleCancel = () => {
    setEditCategory(entry.file.split("/")[0]);
    setEditLabel(entry.expected_label);
    setEditIssue(entry.issue);
    setEditDescription(entry.description);
    setEditElementType(entry.element_type || "");
    setEditExpectedText(entry.expected_text || "");
    setEditing(false);
  };

  const inputClass =
    "w-full px-1.5 py-1 text-[11px] bg-muted/30 border border-border rounded text-foreground placeholder:text-muted-foreground";

  return (
    <Card className="group relative">
      <CardContent className="p-3">
        {/* Image thumbnail */}
        <div className="relative w-full h-28 bg-black/20 rounded-md overflow-hidden mb-2 flex items-center justify-center">
          {loading && <div className="text-xs text-muted-foreground animate-pulse">Loading...</div>}
          {error && (
            <div className="text-xs text-red-400 flex items-center gap-1">
              <AlertCircle className="w-3 h-3" />
              {error}
            </div>
          )}
          {imageData && (
            <img
              src={imageData}
              alt={entry.description}
              className="max-w-full max-h-full object-contain"
            />
          )}
        </div>

        {editing ? (
          /* Edit mode */
          <div className="space-y-1.5">
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Category
              </label>
              <select
                className={inputClass}
                value={editCategory}
                onChange={(e) => setEditCategory(e.target.value)}
              >
                {CATEGORIES.map((cat) => (
                  <option key={cat} value={cat}>
                    {CATEGORY_COLORS[cat].label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Label
              </label>
              <input
                className={inputClass}
                value={editLabel}
                onChange={(e) => setEditLabel(e.target.value)}
                placeholder="Expected label"
              />
            </div>
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Expected Text
              </label>
              <input
                className={inputClass}
                value={editExpectedText}
                onChange={(e) => setEditExpectedText(e.target.value)}
                placeholder="Full expected text"
              />
            </div>
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Issue
              </label>
              <input
                className={inputClass}
                value={editIssue}
                onChange={(e) => setEditIssue(e.target.value)}
                placeholder="e.g. text_clipped_bottom"
              />
            </div>
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Element Type
              </label>
              <input
                className={inputClass}
                value={editElementType}
                onChange={(e) => setEditElementType(e.target.value)}
                placeholder="e.g. badge, button"
              />
            </div>
            <div>
              <label className="text-[9px] text-muted-foreground uppercase tracking-wider">
                Description
              </label>
              <textarea
                className={`${inputClass} resize-none`}
                rows={2}
                value={editDescription}
                onChange={(e) => setEditDescription(e.target.value)}
                placeholder="What's shown / what's wrong"
              />
            </div>
            <div className="flex gap-1.5 pt-1">
              <Button
                size="sm"
                variant="primary"
                onClick={handleSave}
                disabled={saving}
                className="h-6 text-[10px] px-2"
              >
                <Check className="w-3 h-3 mr-1" />
                {saving ? "Saving..." : "Save"}
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={handleCancel}
                disabled={saving}
                className="h-6 text-[10px] px-2"
              >
                Cancel
              </Button>
            </div>
          </div>
        ) : (
          /* View mode */
          <div className="space-y-1.5">
            <div className="flex items-center gap-1.5 flex-wrap">
              <Badge variant={categoryInfo.variant} size="sm">
                {categoryInfo.label}
              </Badge>
              {entry.element_type && (
                <Badge variant="outline" size="sm">
                  {entry.element_type}
                </Badge>
              )}
            </div>
            <div className="text-xs font-medium text-foreground truncate" title={filename}>
              {filename}
            </div>
            <div
              className="text-[11px] text-muted-foreground line-clamp-2"
              title={entry.description}
            >
              {entry.description}
            </div>
            <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
              <span>
                Label: <span className="text-foreground font-medium">{entry.expected_label}</span>
              </span>
              {entry.source_dimensions && (
                <span>
                  {entry.source_dimensions.width}x{entry.source_dimensions.height}
                </span>
              )}
            </div>
            <div className="text-[10px] text-muted-foreground">
              Issue: <span className="text-yellow-400">{entry.issue}</span>
            </div>
          </div>
        )}

        {/* Action buttons (hover) */}
        <div className="absolute top-2 right-2 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
          {!editing && (
            <button
              onClick={startEditing}
              className="p-1 rounded bg-blue-500/80 text-white"
              title="Edit metadata"
            >
              <Pencil className="w-3 h-3" />
            </button>
          )}
          <button
            onClick={() => onDelete(entry.file)}
            className="p-1 rounded bg-red-500/80 text-white"
            title="Delete image"
          >
            <Trash2 className="w-3 h-3" />
          </button>
        </div>
      </CardContent>
    </Card>
  );
}

// ============================================================================
// Upload Panel Component
// ============================================================================

function UploadPanel({ onUploadComplete }: { onUploadComplete: () => void }) {
  const [dragOver, setDragOver] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [category, setCategory] = useState<Category>("good");
  const [expectedLabel, setExpectedLabel] = useState("");
  const [description, setDescription] = useState("");
  const [issue, setIssue] = useState("");
  const [elementType, setElementType] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleFile = useCallback(
    async (file: File) => {
      if (!file.type.startsWith("image/")) {
        setUploadError("Only image files are supported");
        return;
      }

      if (!expectedLabel.trim()) {
        setUploadError("Expected label is required");
        return;
      }

      if (!description.trim()) {
        setUploadError("Description is required");
        return;
      }

      if (!issue.trim()) {
        setUploadError("Issue type is required");
        return;
      }

      setUploading(true);
      setUploadError(null);

      try {
        const arrayBuffer = await file.arrayBuffer();
        const base64 = btoa(
          new Uint8Array(arrayBuffer).reduce((data, byte) => data + String.fromCharCode(byte), ""),
        );

        const resp = await tracedFetch(`${getApiBase()}/image-quality-tests/upload`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            category,
            filename: file.name,
            base64_png: base64,
            expected_label: expectedLabel.trim(),
            description: description.trim(),
            issue: issue.trim(),
            element_type: elementType.trim() || undefined,
          }),
        });

        if (!resp.ok) {
          const data = await resp.json();
          throw new Error(data.error || `HTTP ${resp.status}`);
        }

        // Reset form on success
        setExpectedLabel("");
        setDescription("");
        setIssue("");
        setElementType("");
        onUploadComplete();
      } catch (err) {
        setUploadError(err instanceof Error ? err.message : "Upload failed");
      } finally {
        setUploading(false);
      }
    },
    [category, expectedLabel, description, issue, elementType, onUploadComplete],
  );

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      const file = e.dataTransfer.files[0];
      if (file) handleFile(file);
    },
    [handleFile],
  );

  const handleFileSelect = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (file) handleFile(file);
    },
    [handleFile],
  );

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <Upload className="w-4 h-4" />
          Upload Test Image
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="space-y-3">
          {/* Form fields row */}
          <div className="grid grid-cols-2 md:grid-cols-5 gap-2">
            <div>
              <label className="text-[10px] text-muted-foreground uppercase tracking-wider mb-0.5 block">
                Category
              </label>
              <select
                value={category}
                onChange={(e) => setCategory(e.target.value as Category)}
                className="w-full px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-md text-foreground"
              >
                {CATEGORIES.map((c) => (
                  <option key={c} value={c}>
                    {CATEGORY_COLORS[c].label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="text-[10px] text-muted-foreground uppercase tracking-wider mb-0.5 block">
                Expected Label
              </label>
              <input
                type="text"
                value={expectedLabel}
                onChange={(e) => setExpectedLabel(e.target.value)}
                placeholder="e.g. BETA"
                className="w-full px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-md text-foreground placeholder:text-muted-foreground"
              />
            </div>
            <div>
              <label className="text-[10px] text-muted-foreground uppercase tracking-wider mb-0.5 block">
                Issue Type
              </label>
              <input
                type="text"
                value={issue}
                onChange={(e) => setIssue(e.target.value)}
                placeholder="e.g. text_clipped_bottom"
                className="w-full px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-md text-foreground placeholder:text-muted-foreground"
              />
            </div>
            <div>
              <label className="text-[10px] text-muted-foreground uppercase tracking-wider mb-0.5 block">
                Element Type
              </label>
              <input
                type="text"
                value={elementType}
                onChange={(e) => setElementType(e.target.value)}
                placeholder="e.g. badge, button"
                className="w-full px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-md text-foreground placeholder:text-muted-foreground"
              />
            </div>
            <div>
              <label className="text-[10px] text-muted-foreground uppercase tracking-wider mb-0.5 block">
                Description
              </label>
              <input
                type="text"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="What's shown / what's wrong"
                className="w-full px-2 py-1.5 text-xs bg-muted/30 border border-border rounded-md text-foreground placeholder:text-muted-foreground"
              />
            </div>
          </div>

          {/* Drop zone */}
          <div
            onDragOver={(e) => {
              e.preventDefault();
              setDragOver(true);
            }}
            onDragLeave={() => setDragOver(false)}
            onDrop={handleDrop}
            onClick={() => fileInputRef.current?.click()}
            className={`
              flex items-center justify-center gap-2 px-4 py-4 rounded-md border-2 border-dashed cursor-pointer transition-colors
              ${
                dragOver
                  ? "border-primary bg-primary/10"
                  : "border-border/50 hover:border-border hover:bg-muted/20"
              }
              ${uploading ? "opacity-50 cursor-not-allowed" : ""}
            `}
          >
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              onChange={handleFileSelect}
              className="hidden"
            />
            {uploading ? (
              <RefreshCw className="w-4 h-4 animate-spin text-muted-foreground" />
            ) : (
              <Upload className="w-4 h-4 text-muted-foreground" />
            )}
            <span className="text-xs text-muted-foreground">
              {uploading ? "Uploading..." : "Drop an image here or click to browse"}
            </span>
          </div>

          {/* Error display */}
          {uploadError && (
            <div className="flex items-center gap-2 text-xs text-red-400 bg-red-500/10 px-3 py-2 rounded-md">
              <AlertCircle className="w-3 h-3 shrink-0" />
              {uploadError}
              <button onClick={() => setUploadError(null)} className="ml-auto">
                <X className="w-3 h-3" />
              </button>
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

// ============================================================================
// Category Group Component
// ============================================================================

function CategoryGroup({
  category,
  images,
  onDelete,
  onUpdate,
}: {
  category: Category;
  images: TestImageEntry[];
  onDelete: (file: string) => void;
  onUpdate: (file: string, updates: Partial<TestImageEntry>) => Promise<void>;
}) {
  const [expanded, setExpanded] = useState(true);
  const info = CATEGORY_COLORS[category];

  return (
    <div className="space-y-2">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 text-sm font-medium text-foreground hover:text-foreground/80 transition-colors"
      >
        {expanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        <Badge variant={info.variant} size="sm">
          {info.label}
        </Badge>
        <span className="text-muted-foreground text-xs">
          ({images.length} image{images.length !== 1 ? "s" : ""})
        </span>
      </button>
      {expanded && (
        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-3 pl-6">
          {images.map((entry) => (
            <ImageCard key={entry.file} entry={entry} onDelete={onDelete} onUpdate={onUpdate} />
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Main Page Component
// ============================================================================

export function ImageQualityTestsPage() {
  const [manifest, setManifest] = useState<Manifest | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadManifest = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const resp = await tracedFetch(`${getApiBase()}/image-quality-tests/manifest`);
      if (!resp.ok) {
        throw new Error(`HTTP ${resp.status}`);
      }
      const data = await resp.json();
      if (data.success && data.data) {
        setManifest(data.data);
      } else {
        throw new Error(data.error || "Unknown error");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load manifest");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    loadManifest();
  }, [loadManifest]);

  const handleDelete = useCallback(
    async (file: string) => {
      if (!confirm(`Delete test image "${file}"?`)) return;
      try {
        const resp = await tracedFetch(`${getApiBase()}/image-quality-tests/image/${file}`, {
          method: "DELETE",
        });
        if (!resp.ok) {
          const data = await resp.json();
          throw new Error(data.error || `HTTP ${resp.status}`);
        }
        loadManifest();
      } catch (err) {
        alert(`Delete failed: ${err instanceof Error ? err.message : "Unknown error"}`);
      }
    },
    [loadManifest],
  );

  const handleUpdate = useCallback(
    async (file: string, updates: Partial<TestImageEntry>) => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/image-quality-tests/image/${file}`, {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(updates),
        });
        if (!resp.ok) {
          let errorMsg = `HTTP ${resp.status}`;
          try {
            const data = await resp.json();
            errorMsg = data.error || errorMsg;
          } catch {
            // Response may not be JSON
          }
          throw new Error(errorMsg);
        }
        await loadManifest();
      } catch (err) {
        alert(`Update failed: ${err instanceof Error ? err.message : "Unknown error"}`);
        throw err; // Re-throw so ImageCard knows save failed
      }
    },
    [loadManifest],
  );

  // Group images by category
  const imagesByCategory = new Map<Category, TestImageEntry[]>();
  if (manifest) {
    for (const cat of CATEGORIES) {
      imagesByCategory.set(cat, []);
    }
    for (const img of manifest.images) {
      const cat = img.file.split("/")[0] as Category;
      const list = imagesByCategory.get(cat);
      if (list) {
        list.push(img);
      }
    }
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-3 border-b border-border shrink-0">
        <div className="flex items-center gap-2">
          <ImageIcon className="w-4 h-4 text-muted-foreground" />
          <h1 className="text-lg font-semibold">Image Quality Tests</h1>
          {manifest && (
            <span className="text-sm text-muted-foreground">
              {manifest.images.length} test image{manifest.images.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
        <Button variant="ghost" size="sm" onClick={loadManifest} disabled={loading}>
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </Button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6 space-y-6">
        {/* Upload panel */}
        <UploadPanel onUploadComplete={loadManifest} />

        {/* Error state */}
        {error && (
          <div className="flex items-center gap-2 text-sm text-red-400 bg-red-500/10 px-4 py-3 rounded-md">
            <AlertCircle className="w-4 h-4 shrink-0" />
            {error}
          </div>
        )}

        {/* Loading state */}
        {loading && !manifest && (
          <div className="text-center py-12 text-muted-foreground">
            <RefreshCw className="w-8 h-8 mx-auto mb-3 animate-spin opacity-50" />
            <p className="text-sm">Loading manifest...</p>
          </div>
        )}

        {/* Empty state */}
        {manifest && manifest.images.length === 0 && (
          <div className="text-center py-12 text-muted-foreground">
            <ImageIcon className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p className="text-lg font-medium mb-2">No Test Images</p>
            <p className="text-sm">
              Upload test images using the panel above to start building the test suite.
            </p>
          </div>
        )}

        {/* Image grid grouped by category */}
        {manifest &&
          manifest.images.length > 0 &&
          Array.from(imagesByCategory.entries())
            .filter(([_, images]) => images.length > 0)
            .map(([category, images]) => (
              <CategoryGroup
                key={category}
                category={category}
                images={images}
                onDelete={handleDelete}
                onUpdate={handleUpdate}
              />
            ))}
      </div>
    </div>
  );
}
