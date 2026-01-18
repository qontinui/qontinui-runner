/**
 * CompressionStatusSection Component
 *
 * Displays memory compression status including token counts,
 * threshold progress, and compression history.
 */

import { Archive, AlertTriangle, TrendingDown, Database, FileText } from "lucide-react";
import { Badge } from "../ui/Badge";
import { Progress } from "../ui/Progress";
import {
  type CompressionStatus,
  formatTokenCount,
  calculateCompressionSavings,
} from "../../types/executionStatus";
import { getStatusColors, getAccentColors } from "@/design-system";

interface CompressionStatusSectionProps {
  compression: CompressionStatus;
}

interface TokenBreakdownItemProps {
  label: string;
  tokens: number;
  total: number;
  colorClass: string;
}

function TokenBreakdownItem({ label, tokens, total, colorClass }: TokenBreakdownItemProps) {
  const percent = total > 0 ? (tokens / total) * 100 : 0;

  return (
    <div className="flex items-center justify-between text-xs">
      <span className="text-muted-foreground">{label}</span>
      <div className="flex items-center gap-2">
        <div className="w-16 bg-muted/50 rounded-full h-1">
          <div className={`h-full rounded-full ${colorClass}`} style={{ width: `${percent}%` }} />
        </div>
        <span className="font-mono w-12 text-right">{formatTokenCount(tokens)}</span>
      </div>
    </div>
  );
}

export function CompressionStatusSection({ compression }: CompressionStatusSectionProps) {
  const {
    enabled,
    thresholdTokens,
    targetTokens,
    currentTokenCount,
    lastCompression,
    thresholdPercentage,
    compressionImminent,
  } = compression;

  if (!enabled) {
    return (
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-muted-foreground">
          <Archive className="w-4 h-4" />
          <span className="text-sm">Memory compression is disabled</span>
        </div>
        <p className="text-xs text-muted-foreground">
          Enable memory compression in settings to automatically manage context size.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Token Usage Progress */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Database className="w-4 h-4 text-primary" />
            <span className="text-sm font-medium">Token Usage</span>
          </div>
          <Badge
            variant={compressionImminent ? "warning" : thresholdPercentage > 50 ? "info" : "muted"}
          >
            {formatTokenCount(currentTokenCount?.total || 0)} / {formatTokenCount(thresholdTokens)}
          </Badge>
        </div>

        {/* Progress Bar */}
        <div className="space-y-1">
          <Progress
            value={thresholdPercentage}
            className={`h-2 ${
              compressionImminent
                ? "[&>div]:bg-amber-500"
                : thresholdPercentage > 90
                  ? "[&>div]:bg-red-500"
                  : ""
            }`}
          />
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>{thresholdPercentage.toFixed(1)}% of threshold</span>
            {compressionImminent && (
              <span className={`flex items-center gap-1 ${getAccentColors("amber").text}`}>
                <AlertTriangle className="w-3 h-3" />
                Compression imminent
              </span>
            )}
          </div>
        </div>
      </div>

      {/* Thresholds */}
      <div className="grid grid-cols-2 gap-2 text-xs">
        <div className="bg-muted/30 rounded px-2 py-1.5">
          <span className="text-muted-foreground">Threshold</span>
          <p className="font-mono text-foreground">{formatTokenCount(thresholdTokens)}</p>
        </div>
        <div className="bg-muted/30 rounded px-2 py-1.5">
          <span className="text-muted-foreground">Target</span>
          <p className="font-mono text-foreground">{formatTokenCount(targetTokens)}</p>
        </div>
      </div>

      {/* Token Breakdown */}
      {currentTokenCount && currentTokenCount.total > 0 && (
        <div className="space-y-2">
          <span className="text-xs text-muted-foreground">Token Breakdown</span>
          <div className="space-y-1.5">
            <TokenBreakdownItem
              label="Findings"
              tokens={currentTokenCount.findings}
              total={currentTokenCount.total}
              colorClass="bg-red-500"
            />
            <TokenBreakdownItem
              label="Observations"
              tokens={currentTokenCount.observations}
              total={currentTokenCount.total}
              colorClass="bg-blue-500"
            />
            <TokenBreakdownItem
              label="Feedback"
              tokens={currentTokenCount.feedback}
              total={currentTokenCount.total}
              colorClass="bg-amber-500"
            />
            <TokenBreakdownItem
              label="Solutions"
              tokens={currentTokenCount.solutions}
              total={currentTokenCount.total}
              colorClass="bg-green-500"
            />
            <TokenBreakdownItem
              label="Other"
              tokens={currentTokenCount.other}
              total={currentTokenCount.total}
              colorClass="bg-purple-500"
            />
          </div>
          <div className="text-xs text-muted-foreground text-right">
            {currentTokenCount.entryCount} entries
          </div>
        </div>
      )}

      {/* Last Compression */}
      {lastCompression && (
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <TrendingDown className="w-4 h-4 text-green-500" />
            <span className="text-xs text-muted-foreground">Last Compression</span>
          </div>
          <div className="bg-muted/30 rounded p-2 space-y-2">
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground">Tokens saved</span>
              <span className={`font-mono ${getAccentColors("green").text}`}>
                {formatTokenCount(
                  lastCompression.originalTokens - lastCompression.compressedTokens,
                )}
              </span>
            </div>
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground">Reduction</span>
              <span className={`font-mono ${getAccentColors("green").text}`}>
                {calculateCompressionSavings(lastCompression).toFixed(1)}%
              </span>
            </div>
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground">Items summarized</span>
              <span className="font-mono">{lastCompression.itemsSummarized}</span>
            </div>
            {lastCompression.compressedCategories.length > 0 && (
              <div className="flex flex-wrap gap-1 mt-1">
                {lastCompression.compressedCategories.map((cat) => (
                  <Badge key={cat} variant="muted" size="sm">
                    {cat}
                  </Badge>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {/* No Data */}
      {!currentTokenCount && !lastCompression && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <FileText className="w-4 h-4" />
          <span>No token data available yet</span>
        </div>
      )}
    </div>
  );
}
