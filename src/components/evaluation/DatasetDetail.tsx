/**
 * DatasetDetail
 *
 * Shows items in a dataset as a table (input, expected_output columns).
 * Provides an "Add Items" button that opens a JSON textarea for bulk add.
 */

import { useState, useRef } from "react";
import { Plus, X, RefreshCw, FileText, Trash2, Download, Upload } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import type { EvaluationDataset } from "@/hooks/useEvaluationDatasets";
import type { EvaluationDatasetItem, AddItemsRequest } from "@/hooks/useEvaluationDatasets";

interface DatasetDetailProps {
  dataset: EvaluationDataset;
  items: EvaluationDatasetItem[];
  isLoading: boolean;
  onAddItems: (req: AddItemsRequest) => Promise<unknown>;
  isAdding: boolean;
  onDeleteItem?: (itemId: string) => void;
  isDeletingItem?: boolean;
}

const PLACEHOLDER_JSON = `[
  { "input": "What is 2+2?", "expected_output": "4" },
  { "input": "Capital of France?", "expected_output": "Paris" }
]`;

/** Trigger a browser download for the given content. */
function downloadFile(filename: string, content: string, mimeType: string) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

/** Escape a value for CSV (double-quote wrapping). */
function csvEscape(value: string): string {
  if (value.includes('"') || value.includes(",") || value.includes("\n")) {
    return `"${value.replace(/"/g, '""')}"`;
  }
  return value;
}

/** Parse a CSV string with header row into objects. Handles quoted fields. */
function parseCsv(text: string): Array<{ input: string; expected_output: string }> {
  const lines: string[] = [];
  let current = "";
  let inQuotes = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"') {
      if (inQuotes && text[i + 1] === '"') {
        current += '"';
        i++;
      } else {
        inQuotes = !inQuotes;
      }
    } else if (ch === "\n" && !inQuotes) {
      lines.push(current.replace(/\r$/, ""));
      current = "";
    } else {
      current += ch;
    }
  }
  if (current.trim()) lines.push(current.replace(/\r$/, ""));
  if (lines.length < 2) throw new Error("CSV must have a header row and at least one data row");

  const header = lines[0].split(",").map((h) => h.trim().toLowerCase());
  const inputIdx = header.indexOf("input");
  const outputIdx = header.indexOf("expected_output");
  if (inputIdx === -1 || outputIdx === -1) {
    throw new Error("CSV header must contain 'input' and 'expected_output' columns");
  }

  const results: Array<{ input: string; expected_output: string }> = [];
  for (let i = 1; i < lines.length; i++) {
    // Split fields respecting quotes
    const fields: string[] = [];
    let field = "";
    let fInQuotes = false;
    for (let j = 0; j < lines[i].length; j++) {
      const c = lines[i][j];
      if (c === '"') {
        if (fInQuotes && lines[i][j + 1] === '"') {
          field += '"';
          j++;
        } else {
          fInQuotes = !fInQuotes;
        }
      } else if (c === "," && !fInQuotes) {
        fields.push(field);
        field = "";
      } else {
        field += c;
      }
    }
    fields.push(field);
    results.push({ input: fields[inputIdx] ?? "", expected_output: fields[outputIdx] ?? "" });
  }
  return results;
}

export function DatasetDetail({
  dataset,
  items,
  isLoading,
  onAddItems,
  isAdding,
  onDeleteItem,
  isDeletingItem,
}: DatasetDetailProps) {
  const [showAddForm, setShowAddForm] = useState(false);
  const [jsonInput, setJsonInput] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleExportJson = () => {
    const data = items.map((item) => ({
      input: item.input,
      expected_output: item.expected_output,
      ...(item.metadata ? { metadata: item.metadata } : {}),
    }));
    downloadFile(`${dataset.name}.json`, JSON.stringify(data, null, 2), "application/json");
  };

  const handleExportCsv = () => {
    const header = "input,expected_output";
    const rows = items.map((item) => `${csvEscape(item.input)},${csvEscape(item.expected_output)}`);
    downloadFile(`${dataset.name}.csv`, [header, ...rows].join("\n"), "text/csv");
  };

  const handleFileImport = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setParseError(null);

    try {
      const text = await file.text();
      let parsed: Array<{
        input: string;
        expected_output: string;
        metadata?: Record<string, unknown>;
      }>;

      if (file.name.endsWith(".csv")) {
        parsed = parseCsv(text);
      } else {
        const json = JSON.parse(text);
        if (!Array.isArray(json)) {
          setParseError("JSON file must contain an array");
          return;
        }
        parsed = json;
      }

      for (const item of parsed) {
        if (typeof item.input !== "string" || typeof item.expected_output !== "string") {
          setParseError("Each item must have 'input' and 'expected_output' string fields");
          return;
        }
      }

      await onAddItems({
        items: parsed.map((item) => ({
          input: item.input,
          expected_output: item.expected_output,
          metadata: item.metadata,
        })),
      });
      setShowAddForm(false);
    } catch (err) {
      setParseError(err instanceof Error ? err.message : "Failed to parse file");
    } finally {
      // Reset file input so the same file can be re-imported
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  const handleSubmit = async () => {
    setParseError(null);
    try {
      const parsed = JSON.parse(jsonInput);
      if (!Array.isArray(parsed)) {
        setParseError("Input must be a JSON array");
        return;
      }
      for (const item of parsed) {
        if (typeof item.input !== "string" || typeof item.expected_output !== "string") {
          setParseError("Each item must have 'input' and 'expected_output' string fields");
          return;
        }
      }
      try {
        await onAddItems({
          items: parsed.map(
            (item: {
              input: string;
              expected_output: string;
              metadata?: Record<string, unknown>;
            }) => ({
              input: item.input,
              expected_output: item.expected_output,
              metadata: item.metadata,
            }),
          ),
        });
        setJsonInput("");
        setShowAddForm(false);
      } catch (err) {
        setParseError(err instanceof Error ? err.message : "Failed to add items");
      }
    } catch {
      setParseError("Invalid JSON");
    }
  };

  const handleDeleteItem = (itemId: string) => {
    if (!onDeleteItem) return;
    if (!window.confirm("Are you sure you want to delete this item?")) return;
    onDeleteItem(itemId);
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border/50">
        <div>
          <h3 className="text-sm font-semibold">{dataset.name}</h3>
          <div className="flex items-center gap-2 mt-0.5 text-xs text-muted-foreground">
            <span>{dataset.item_count} items</span>
            <Badge variant="muted" size="sm">
              v{dataset.version}
            </Badge>
          </div>
        </div>
        <div className="flex items-center gap-1.5">
          {items.length > 0 && (
            <>
              <Button
                size="sm"
                variant="ghost"
                onClick={handleExportJson}
                title="Export as JSON"
                data-tutorial-id="evaluation-export-json"
              >
                <Download className="w-3.5 h-3.5" />
                JSON
              </Button>
              <Button size="sm" variant="ghost" onClick={handleExportCsv} title="Export as CSV">
                <Download className="w-3.5 h-3.5" />
                CSV
              </Button>
            </>
          )}
          <Button
            size="sm"
            variant={showAddForm ? "ghost" : "outline"}
            onClick={() => setShowAddForm(!showAddForm)}
            data-tutorial-id="evaluation-add-items"
          >
            {showAddForm ? <X className="w-3.5 h-3.5" /> : <Plus className="w-3.5 h-3.5" />}
            {showAddForm ? "Cancel" : "Add Items"}
          </Button>
        </div>
      </div>

      {/* Add items form */}
      {showAddForm && (
        <div className="px-4 py-3 border-b border-border/50 bg-muted/20 space-y-2">
          <label className="text-xs font-medium text-muted-foreground">
            Paste JSON array of items
          </label>
          <textarea
            aria-label="Paste JSON array of items"
            className="w-full h-32 rounded-md bg-background border border-border px-3 py-2 text-xs font-mono resize-y focus:outline-hidden focus:ring-2 focus:ring-primary"
            placeholder={PLACEHOLDER_JSON}
            value={jsonInput}
            onChange={(e) => setJsonInput(e.target.value)}
          />
          {parseError && <p className="text-xs text-destructive">{parseError}</p>}
          <div className="flex items-center justify-between">
            <div>
              <input
                ref={fileInputRef}
                type="file"
                accept=".json,.csv"
                className="hidden"
                onChange={handleFileImport}
              />
              <Button
                size="sm"
                variant="outline"
                onClick={() => fileInputRef.current?.click()}
                disabled={isAdding}
                data-tutorial-id="evaluation-import-file"
              >
                <Upload className="w-3.5 h-3.5" />
                Import File
              </Button>
            </div>
            <Button size="sm" onClick={handleSubmit} disabled={!jsonInput.trim() || isAdding}>
              {isAdding ? "Adding..." : "Add Items"}
            </Button>
          </div>
        </div>
      )}

      {/* Items table */}
      <div className="flex-1 overflow-auto">
        {isLoading ? (
          <div className="flex items-center justify-center py-12 text-muted-foreground">
            <RefreshCw className="w-5 h-5 animate-spin" />
          </div>
        ) : items.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 text-muted-foreground gap-2">
            <FileText className="w-8 h-8 opacity-50" />
            <p className="text-sm">No items in this dataset</p>
            <p className="text-xs">Click "Add Items" to get started</p>
          </div>
        ) : (
          <table className="w-full text-xs" data-tutorial-id="evaluation-items-table">
            <thead className="sticky top-0 bg-card z-10">
              <tr className="border-b border-border/50 text-left text-muted-foreground">
                <th className="px-4 py-2 font-medium w-1/2">Input</th>
                <th className="px-4 py-2 font-medium w-1/2">Expected Output</th>
                {onDeleteItem && <th className="px-2 py-2 font-medium w-8" />}
              </tr>
            </thead>
            <tbody>
              {items.map((item) => (
                <tr
                  key={item.id}
                  className="border-b border-border/30 hover:bg-muted/30 transition-colors"
                >
                  <td className="px-4 py-2 align-top whitespace-pre-wrap break-words max-w-0">
                    {item.input}
                  </td>
                  <td className="px-4 py-2 align-top whitespace-pre-wrap break-words max-w-0">
                    {item.expected_output}
                  </td>
                  {onDeleteItem && (
                    <td className="px-2 py-2 align-top">
                      <button
                        className="p-1 rounded hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors disabled:opacity-50"
                        onClick={() => handleDeleteItem(item.id)}
                        disabled={isDeletingItem}
                        title="Delete item"
                      >
                        <Trash2 className="w-3.5 h-3.5" />
                      </button>
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
