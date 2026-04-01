/**
 * Knowledge Acquisition Service
 *
 * TypeScript types and API client for the /knowledge/* endpoints.
 * Provides web search, vulnerability lookup, dependency auditing,
 * and error research capabilities.
 */

import { getApiBase, tracedFetch } from "../lib/runner-api";

// ============================================================================
// Shared Types
// ============================================================================

/** Search provider identifiers */
export type SearchProvider =
  | "duckduckgo"
  | "tavily"
  | "ui_bridge"
  | "sploitus"
  | "osv_dev"
  | "perplexity"
  | "local_knowledge";

/** Knowledge domain classification */
export type KnowledgeDomain =
  | "error_resolution"
  | "api_documentation"
  | "vulnerability_intelligence"
  | "tooling_standards"
  | "general";

/** Metadata attached to a search result */
export interface KnowledgeMetadata {
  domain?: KnowledgeDomain;
  cvss_score?: number;
  cve_id?: string;
  ecosystem?: string;
  package_name?: string;
  package_version?: string;
}

/** A single knowledge result from any provider */
export interface KnowledgeResult {
  provider: SearchProvider;
  query: string;
  title: string;
  content: string;
  url?: string;
  relevance_score?: number;
  metadata: KnowledgeMetadata;
}

/** Provider availability info */
export interface ProviderInfo {
  name: string;
  available: boolean;
  requires_api_key: boolean;
}

/** Standard search response */
export interface SearchResponse {
  results: KnowledgeResult[];
  provider_used: string[];
}

/** Package vulnerability info */
export interface PackageVulns {
  package_name: string;
  vulns: KnowledgeResult[];
}

/** Vulnerability audit response */
export interface VulnAuditResponse {
  packages: PackageVulns[];
  total_vulns: number;
  skipped_ecosystems?: string[];
}

/** Per-provider statistics */
export interface ProviderStats {
  queries: number;
  successes: number;
  failures: number;
  timeouts: number;
  results_returned: number;
  bytes_fetched: number;
  success_rate: number;
}

/** Knowledge acquisition system stats snapshot */
export interface StatsSnapshot {
  uptime_secs: number;
  total_searches: number;
  cache_hits: number;
  cache_hit_rate: number;
  results_ingested: number;
  dedup_skipped: number;
  injection_detections: number;
  providers: {
    local_knowledge: ProviderStats;
    ui_bridge: ProviderStats;
    duckduckgo: ProviderStats;
    tavily: ProviderStats;
    perplexity: ProviderStats;
    sploitus: ProviderStats;
    osv: ProviderStats;
  };
}

/** Standard API response wrapper */
interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// ============================================================================
// Request Types
// ============================================================================

export interface SearchWebRequest {
  query: string;
  max_results?: number;
}

export interface ResearchErrorRequest {
  error_message: string;
  framework?: string;
  stack_trace?: string;
}

export interface FetchPageRequest {
  query?: string;
  snapshot_type?: "summary" | "analyze" | "full";
}

export interface LookupApiDocsRequest {
  query: string;
  max_results?: number;
}

export interface SearchVulnerabilitiesRequest {
  query: string;
}

export interface AuditDependenciesRequest {
  packages: Array<{
    name: string;
    ecosystem: string;
    version: string;
  }>;
}

export interface AuditManifestRequest {
  manifest_path: string;
}

// ============================================================================
// API Client
// ============================================================================

async function fetchApi<T>(path: string, body?: unknown): Promise<T> {
  const url = `${getApiBase()}${path}`;
  const response = await tracedFetch(url, {
    method: body ? "POST" : "GET",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });

  if (!response.ok) {
    throw new Error(`Knowledge API error: ${response.status} ${response.statusText}`);
  }

  const json: ApiResponse<T> = await response.json();
  if (!json.success || !json.data) {
    throw new Error(json.error || "Unknown error");
  }

  return json.data;
}

/** List available search providers and their status */
export async function listProviders(): Promise<ProviderInfo[]> {
  return fetchApi<ProviderInfo[]>("/knowledge/providers");
}

/** Get knowledge acquisition stats (hit rates, cache hits, provider success/failure) */
export async function getStats(): Promise<StatsSnapshot> {
  return fetchApi<StatsSnapshot>("/knowledge/stats");
}

/** Web search (DuckDuckGo / Tavily) */
export async function searchWeb(req: SearchWebRequest): Promise<SearchResponse> {
  return fetchApi<SearchResponse>("/knowledge/search-web", req);
}

/** Research an error message with automatic escalation */
export async function researchError(req: ResearchErrorRequest): Promise<SearchResponse> {
  return fetchApi<SearchResponse>("/knowledge/research-error", req);
}

/** Fetch content from UI Bridge-connected application */
export async function fetchPage(req: FetchPageRequest): Promise<SearchResponse> {
  return fetchApi<SearchResponse>("/knowledge/fetch-page", req);
}

/** Deep API documentation search */
export async function lookupApiDocs(req: LookupApiDocsRequest): Promise<SearchResponse> {
  return fetchApi<SearchResponse>("/knowledge/lookup-api-docs", req);
}

/** Search for vulnerabilities by CVE ID or software name */
export async function searchVulnerabilities(
  req: SearchVulnerabilitiesRequest,
): Promise<SearchResponse> {
  return fetchApi<SearchResponse>("/knowledge/search-vulnerabilities", req);
}

/** Batch dependency audit (pre-parsed packages) */
export async function auditDependencies(req: AuditDependenciesRequest): Promise<VulnAuditResponse> {
  return fetchApi<VulnAuditResponse>("/knowledge/audit-dependencies", req);
}

/** Audit dependencies from a manifest file */
export async function auditManifest(req: AuditManifestRequest): Promise<VulnAuditResponse> {
  return fetchApi<VulnAuditResponse>("/knowledge/audit-manifest", req);
}
