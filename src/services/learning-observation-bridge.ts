/**
 * learning-observation-bridge.ts
 *
 * Bridges the learning service (session-scoped patterns/insights) to the
 * observation system (cross-session persistent memory). When significant
 * patterns or insights are identified, they are saved as typed observations
 * with topic keys for upsert semantics.
 *
 * This enables cross-session pattern learning: a SuccessStrategy pattern
 * discovered in session A can inform the AI context in session B.
 */

import { createLogger } from "@/lib/logger";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { learningService } from "./learning-service";
import type { Pattern, Insight, PatternType } from "../types/learning";

const logger = createLogger("LearningObservationBridge");

// ============================================================================
// Types
// ============================================================================

interface CreateObservationPayload {
  title: string;
  content: string;
  observationType: string;
  scope: string;
  topicKey?: string;
  projectId?: string;
}

// ============================================================================
// Mapping
// ============================================================================

/** Map pattern types to observation types */
const PATTERN_TYPE_TO_OBSERVATION: Record<PatternType, string> = {
  SuccessStrategy: "pattern",
  FailureMode: "bugfix",
  ToolUsage: "learning",
  Collaboration: "learning",
  IterationBehavior: "pattern",
  ErrorRecovery: "bugfix",
  DecisionMaking: "decision",
};

/** Minimum confidence to persist a pattern as an observation */
const MIN_PATTERN_CONFIDENCE = 0.6;

/** Minimum occurrences to persist a pattern */
const MIN_PATTERN_OCCURRENCES = 2;

/** Minimum confidence to persist an insight */
const MIN_INSIGHT_CONFIDENCE = 0.7;

// ============================================================================
// Service
// ============================================================================

class LearningObservationBridge {
  private projectId: string | null = null;
  private syncIntervalId: ReturnType<typeof setInterval> | null = null;
  private lastSyncedPatternIds = new Set<string>();
  private lastSyncedInsightIds = new Set<string>();

  /**
   * Start periodic sync of learning patterns to observations.
   */
  start(options: { projectId?: string; intervalMs?: number } = {}): void {
    this.projectId = options.projectId ?? null;
    const intervalMs = options.intervalMs ?? 300_000; // 5 minutes

    if (this.syncIntervalId) {
      return; // Already running
    }

    // Run initial sync
    this.syncPatterns().catch((e) =>
      logger.debug("Initial pattern sync skipped:", e),
    );

    this.syncIntervalId = setInterval(() => {
      this.syncPatterns().catch((e) =>
        logger.debug("Periodic pattern sync failed:", e),
      );
    }, intervalMs);

    logger.info(`Learning observation bridge started (interval: ${intervalMs}ms)`);
  }

  /**
   * Stop periodic sync.
   */
  stop(): void {
    if (this.syncIntervalId) {
      clearInterval(this.syncIntervalId);
      this.syncIntervalId = null;
    }
    this.lastSyncedPatternIds.clear();
    this.lastSyncedInsightIds.clear();
    logger.info("Learning observation bridge stopped");
  }

  /**
   * Update project context.
   */
  setProjectId(projectId: string | null): void {
    this.projectId = projectId;
  }

  /**
   * Manually trigger a sync of current patterns and insights to observations.
   */
  async syncPatterns(): Promise<{ patterns: number; insights: number }> {
    let patternsSynced = 0;
    let insightsSynced = 0;

    try {
      const patterns = await learningService.getLearningPatterns();
      for (const pattern of patterns) {
        if (this.shouldPersistPattern(pattern)) {
          const saved = await this.savePatternAsObservation(pattern);
          if (saved) {
            this.lastSyncedPatternIds.add(pattern.id);
            patternsSynced++;
          }
        }
      }
    } catch (err) {
      logger.debug("Could not sync patterns:", err);
    }

    try {
      const insights = await learningService.getLearningInsights();
      for (const insight of insights) {
        if (this.shouldPersistInsight(insight)) {
          const saved = await this.saveInsightAsObservation(insight);
          if (saved) {
            this.lastSyncedInsightIds.add(insight.id);
            insightsSynced++;
          }
        }
      }
    } catch (err) {
      logger.debug("Could not sync insights:", err);
    }

    if (patternsSynced > 0 || insightsSynced > 0) {
      logger.info(
        `Synced ${patternsSynced} patterns, ${insightsSynced} insights to observations`,
      );
    }

    return { patterns: patternsSynced, insights: insightsSynced };
  }

  // ============================================================================
  // Private
  // ============================================================================

  private shouldPersistPattern(pattern: Pattern): boolean {
    // Skip if already synced (topic-key upsert handles updates, but
    // we avoid unnecessary HTTP calls)
    if (this.lastSyncedPatternIds.has(pattern.id)) {
      return false;
    }

    return (
      pattern.confidence >= MIN_PATTERN_CONFIDENCE &&
      pattern.occurrences >= MIN_PATTERN_OCCURRENCES
    );
  }

  private shouldPersistInsight(insight: Insight): boolean {
    if (this.lastSyncedInsightIds.has(insight.id)) {
      return false;
    }

    return insight.confidence >= MIN_INSIGHT_CONFIDENCE;
  }

  private async savePatternAsObservation(pattern: Pattern): Promise<boolean> {
    const observationType =
      PATTERN_TYPE_TO_OBSERVATION[pattern.pattern_type] || "pattern";

    const content = [
      `## ${pattern.name}`,
      "",
      pattern.description,
      "",
      `- **Type:** ${pattern.pattern_type}`,
      `- **Confidence:** ${(pattern.confidence * 100).toFixed(0)}%`,
      `- **Success Rate:** ${(pattern.success_rate * 100).toFixed(0)}%`,
      `- **Occurrences:** ${pattern.occurrences}`,
      "",
      pattern.triggers.length > 0
        ? `**Triggers:** ${pattern.triggers.join(", ")}`
        : "",
      "",
      pattern.recommendations.length > 0
        ? `**Recommendations:**\n${pattern.recommendations.map((r) => `- ${r}`).join("\n")}`
        : "",
    ]
      .filter(Boolean)
      .join("\n");

    // Topic key enables upsert: same pattern updates the observation
    const topicKey = `pattern/${pattern.pattern_type.toLowerCase()}/${pattern.id}`;

    return this.postObservation({
      title: `Pattern: ${pattern.name}`,
      content,
      observationType,
      scope: "project",
      topicKey,
      projectId: this.projectId ?? undefined,
    });
  }

  private async saveInsightAsObservation(insight: Insight): Promise<boolean> {
    const content = [
      `## ${insight.category} Insight`,
      "",
      insight.content,
      "",
      `- **Confidence:** ${(insight.confidence * 100).toFixed(0)}%`,
      "",
      insight.evidence.length > 0
        ? `**Evidence:**\n${insight.evidence.map((e) => `- ${e}`).join("\n")}`
        : "",
      "",
      insight.recommendations.length > 0
        ? `**Recommendations:**\n${insight.recommendations.map((r) => `- ${r}`).join("\n")}`
        : "",
    ]
      .filter(Boolean)
      .join("\n");

    const topicKey = `insight/${insight.category.toLowerCase()}/${insight.id}`;

    return this.postObservation({
      title: `Insight: ${insight.category}`,
      content,
      observationType: "learning",
      scope: "project",
      topicKey,
      projectId: this.projectId ?? undefined,
    });
  }

  private async postObservation(
    payload: CreateObservationPayload,
  ): Promise<boolean> {
    try {
      const response = await tracedFetch(`${getApiBase()}/observations`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      if (response.status === 503) {
        // PG not available — silently skip
        return false;
      }

      return response.ok;
    } catch {
      // Network error — don't crash
      return false;
    }
  }
}

// ============================================================================
// Singleton export
// ============================================================================

export const learningObservationBridge = new LearningObservationBridge();
