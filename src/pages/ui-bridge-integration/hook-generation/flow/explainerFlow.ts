/**
 * Project Explainer flow of the hook-generation step machine: build the
 * explainer queue (index → clusters → pages) once per-page generation is done,
 * then fire one AI prompt per queue item, rotating the AI session at the same
 * budget boundary the per-page flow uses.
 *
 * Moved verbatim from `handleSessionTransition`'s nested helpers; closure
 * variables became `ctx.*` and calls to sibling helpers became `ctx.flow.*`.
 */

import {
  buildExplainerIndexPrompt,
  buildExplainerClusterPrompt,
  buildExplainerPagePrompt,
} from "@/lib/page-analysis-prompt-builder";
import { type GeneratedFile, clusterSpecsByPrefix, gatherExplainerInputs } from "../parse";
import { readProjectFile } from "./readProjectFile";
import { SESSION_PAGE_BUDGET, type ExplainerQueueItem, type StepContext } from "../steps/types";

/** If generateProjectExplainer is on and we haven't started yet, kick off
 * the explainer phase (index → clusters → pages). Returns true when the
 * phase was started so the caller knows to NOT transition to preview.
 *
 * Async because when no specs were generated in this run (e.g. a
 * demo/tour + explainer re-run), we fall back to reading existing specs
 * + architecture diagrams from disk for every page in the run, so the
 * explainer can still compose a document. */
export async function maybeStartExplainerPhase(ctx: StepContext): Promise<boolean> {
  if (!ctx.pageOptionsRef.current.generateProjectExplainer) return false;
  if (ctx.explainerContextRef.current) return false; // already running

  let { specs, arch } = gatherExplainerInputs(ctx.allGeneratedFilesRef.current);

  if (specs.length === 0) {
    // No fresh specs in memory — fall back to reading existing specs +
    // architecture diagrams from disk for each page we were asked about.
    const diskFiles: GeneratedFile[] = [];
    for (const p of ctx.originalPagesRef.current) {
      if (ctx.controller.signal.aborted) return false;
      const slug = p.route.replace(/^\//, "").replace(/\//g, "-") || "root";
      const specBody = await readProjectFile(
        ctx.projectPath,
        `src/specs/${slug}.spec.uibridge.json`,
        ctx.controller.signal,
      );
      if (specBody) {
        diskFiles.push({
          filePath: `${slug}.spec.uibridge.json`,
          content: specBody,
        });
      }
      const archBody = await readProjectFile(
        ctx.projectPath,
        `src/specs/architecture/${slug}.arch.mmd`,
        ctx.controller.signal,
      );
      if (archBody) {
        diskFiles.push({
          filePath: `src/specs/architecture/${slug}.arch.mmd`,
          content: archBody,
        });
      }
    }
    if (ctx.controller.signal.aborted) return false;
    const fromDisk = gatherExplainerInputs(diskFiles);
    specs = fromDisk.specs;
    arch = fromDisk.arch;
  }

  if (specs.length === 0) return false; // still nothing — give up
  const clusters = clusterSpecsByPrefix(specs);
  const projectName = ctx.projectPath.split(/[\\/]/).filter(Boolean).slice(-1)[0] || "Project";
  ctx.explainerContextRef.current = { specs, arch, clusters, projectName };
  // Build the queue: one index, N clusters, then all pages grouped by cluster.
  const queue: ExplainerQueueItem[] = [{ kind: "index" }];
  for (const c of clusters) queue.push({ kind: "cluster", clusterId: c.id });
  for (const c of clusters) {
    for (const specId of c.specIds) {
      queue.push({ kind: "page", clusterId: c.id, specId });
    }
  }
  ctx.explainerQueueRef.current = queue;
  ctx.explainerCallsInSessionRef.current = 0;
  ctx.setStepStatuses((prev) => [
    ...prev,
    {
      state: "active",
      label: `Project Explainer (${queue.length} files: index + ${clusters.length} clusters + ${specs.length} pages)`,
    },
  ]);
  ctx.flow.advanceExplainerQueue();
  return true;
}

/** Pick the next explainer item and fire its prompt. Session-rotates at
 * the same SESSION_PAGE_BUDGET boundary used by per-page generation.
 *
 * (The original's local `ctx` — the explainer context — is named
 * `explainer` here, since `ctx` is the step context.) */
export function advanceExplainerQueue(ctx: StepContext) {
  const queue = ctx.explainerQueueRef.current;
  const explainer = ctx.explainerContextRef.current;
  if (!explainer || queue.length === 0) {
    ctx.explainerContextRef.current = null;
    ctx.explainerCurrentRef.current = null;
    ctx.setStepStatuses((prev) =>
      prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
    );
    ctx.setPhase("preview");
    return;
  }
  // Session rotation
  if (ctx.explainerCallsInSessionRef.current >= SESSION_PAGE_BUDGET) {
    ctx.explainerCallsInSessionRef.current = 0;
    ctx.setStepStatuses((prev) => [
      ...prev,
      { state: "active", label: `Rotating to fresh AI session (explainer)...` },
    ]);
    (async () => {
      if (ctx.controller.signal.aborted) return;
      await ctx.session.close();
      if (ctx.controller.signal.aborted) return;
      ctx.session.resetSession();
      const id = await ctx.session.createSession("Project Explainer (batch)");
      if (ctx.controller.signal.aborted) return;
      if (!id) {
        ctx.setError("Failed to rotate AI session during explainer phase");
        return;
      }
      ctx.setStepStatuses((prev) =>
        prev.map((s) => (s.state === "active" ? { ...s, state: "done" } : s)),
      );
      ctx.flow.advanceExplainerQueue();
    })();
    return;
  }
  ctx.explainerCallsInSessionRef.current++;
  const next = queue.shift()!;
  ctx.explainerCurrentRef.current = next;
  ctx.setPhase("generating-project-explainer");
  if (next.kind === "index") {
    ctx.pendingStepRef.current = "explainer-index";
    ctx.setStepStatuses((prev) => [...prev, { state: "active", label: "Explainer: index.md" }]);
    ctx.sendMessage(
      buildExplainerIndexPrompt(explainer.projectName, explainer.specs, explainer.clusters),
    );
  } else if (next.kind === "cluster") {
    const cluster = explainer.clusters.find((c) => c.id === next.clusterId);
    if (!cluster) return ctx.flow.advanceExplainerQueue();
    const specsInCluster = explainer.specs.filter((s) => cluster.specIds.includes(s.specId));
    const otherClusters = explainer.clusters
      .filter((c) => c.id !== cluster.id)
      .map((c) => ({ id: c.id, name: c.name, description: c.description }));
    ctx.pendingStepRef.current = "explainer-cluster";
    ctx.setStepStatuses((prev) => [
      ...prev,
      { state: "active", label: `Explainer: ${cluster.id}.md (cluster)` },
    ]);
    ctx.sendMessage(
      buildExplainerClusterPrompt(explainer.projectName, cluster, specsInCluster, otherClusters),
    );
  } else {
    // kind === "page"
    const cluster = explainer.clusters.find((c) => c.id === next.clusterId);
    const spec = explainer.specs.find((s) => s.specId === next.specId);
    if (!cluster || !spec) return ctx.flow.advanceExplainerQueue();
    const slug = spec.specId.replace(/[^a-z0-9-]/gi, "-");
    const archDiagram = explainer.arch.get(spec.specId) || null;
    const siblings = explainer.specs
      .filter((s) => cluster.specIds.includes(s.specId) && s.specId !== spec.specId)
      .map((s) => ({
        slug: s.specId.replace(/[^a-z0-9-]/gi, "-"),
        title: s.specId,
        tagline: (s.description || "").replace(/\s+/g, " ").slice(0, 80),
      }));
    ctx.pendingStepRef.current = "explainer-page";
    ctx.setStepStatuses((prev) => [
      ...prev,
      { state: "active", label: `Explainer: ${cluster.id}/${slug}.md` },
    ]);
    ctx.sendMessage(
      buildExplainerPagePrompt(
        explainer.projectName,
        cluster.id,
        slug,
        spec,
        archDiagram,
        siblings,
      ),
    );
  }
}
