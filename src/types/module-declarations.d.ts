/**
 * Module declarations for packages whose dist has not been built yet,
 * or whose installed version mismatches the expected API.
 *
 * These declarations allow TypeScript compilation to succeed while the
 * upstream packages catch up with their build outputs.
 */

// =============================================================================
// @qontinui/ui-bridge/ctr (source exists, dist not yet built)
// =============================================================================

declare module "@qontinui/ui-bridge/ctr" {
  export class CentralTargetRegistry {
    resolve(name: string): CtrResolutionResult | null;
    register(entry: CtrEntry): void;
    unregister(name: string): void;
    getAll(): CtrEntry[];
    has(name: string): boolean;
    on(event: CtrEventType, listener: CtrListener): () => void;
  }

  export function getGlobalCtr(): CentralTargetRegistry;
  export function setGlobalCtr(ctr: CentralTargetRegistry): void;
  export function resetGlobalCtr(): void;
  export function createCtrEntry(
    name: string,
    selectors: CtrSelector[],
    metadata?: Partial<CtrEntryMetadata>,
  ): CtrEntry;

  export function promoteSelector(entry: CtrEntry, index: number): CtrEntry;
  export function demoteSelector(entry: CtrEntry, index: number): CtrEntry;
  export function getViableSelectors(entry: CtrEntry): CtrSelector[];
  export function hasViableSelectors(entry: CtrEntry): boolean;

  export function autoPopulateCtr(
    uiRegistry: unknown,
    ctr: CentralTargetRegistry,
    options?: AutoPopulateOptions,
  ): () => void;

  export const CTR_CONFIG_VERSION: string;
  export const CTR_FILE_EXTENSION: string;
  export const DEFAULT_SELECTOR_CONFIDENCE: number;
  export const CONFIDENCE_BOOST: number;
  export const CONFIDENCE_PENALTY: number;
  export const MIN_CONFIDENCE_THRESHOLD: number;

  export interface CtrSelectorStrategy {
    type: string;
    value: string;
  }

  export interface CtrSelector {
    strategy: CtrSelectorStrategy;
    confidence: number;
    lastUsed?: number;
  }

  export interface CtrEntryMetadata {
    description?: string;
    tags?: string[];
    createdAt: number;
    updatedAt: number;
  }

  export interface CtrEntry {
    name: string;
    selectors: CtrSelector[];
    metadata: CtrEntryMetadata;
  }

  export interface CtrConfig {
    version: string;
    entries: CtrEntry[];
  }

  export interface CtrResolutionResult {
    entry: CtrEntry;
    selector: CtrSelector;
    element?: unknown;
  }

  export type CtrEventType = string;
  export type CtrEvent = { type: CtrEventType; entry: CtrEntry };
  export type CtrListener = (event: CtrEvent) => void;

  export interface AutoPopulateOptions {
    filter?: (element: unknown) => boolean;
  }

  export interface MigrationResult {
    migrated: number;
    skipped: number;
    errors: string[];
  }
}

// =============================================================================
// @qontinui/ui-bridge/artifacts (source exists, dist not yet built)
// =============================================================================

declare module "@qontinui/ui-bridge/artifacts" {
  export interface ArtifactSource {
    specId?: string;
    contractId?: string;
    matrixId?: string;
    runId?: string;
    triggeredBy: "manual" | "ci" | "scheduled" | "workflow-step";
  }

  export interface ArtifactEnvironment {
    userAgent?: string;
    viewport?: { width: number; height: number };
    url?: string;
    timestamp: number;
    sdkVersion: string;
  }

  // ArtifactResult is a union type in the source (SpecGroupResult | ContractExecutionResult | ...).
  // We can't reference those types here, so we keep it as a loose base type.
  // eslint-disable-next-line @typescript-eslint/no-empty-object-type
  export type ArtifactResult = {};

  export interface ResultArtifact {
    artifactId: string;
    source: ArtifactSource;
    result: ArtifactResult;
    environment: ArtifactEnvironment;
    createdAt: string;
    readonly immutable: true;
  }

  export interface ArtifactQuery {
    specId?: string;
    contractId?: string;
    matrixId?: string;
    runId?: string;
    dateRange?: { from: string; to: string };
    passedOnly?: boolean;
    failedOnly?: boolean;
    limit?: number;
    offset?: number;
  }

  export type ArtifactEventType =
    | "artifact:saved"
    | "artifact:integrity-passed"
    | "artifact:integrity-failed";

  export interface ArtifactEvent {
    type: ArtifactEventType;
    artifactId: string;
    timestamp: number;
  }

  export type ArtifactListener = (event: ArtifactEvent) => void;

  export interface ArtifactStore {
    save(artifact: ResultArtifact): Promise<void>;
    get(artifactId: string): Promise<ResultArtifact | null>;
    query(query: ArtifactQuery): Promise<ResultArtifact[]>;
    verify(artifactId: string): Promise<boolean>;
    count(): Promise<number>;
  }

  export function computeHash(data: unknown): Promise<string>;

  export function createArtifact(
    result: ArtifactResult,
    source: ArtifactSource,
    environment?: Partial<ArtifactEnvironment>,
  ): Promise<ResultArtifact>;

  export function captureEnvironment(): Partial<ArtifactEnvironment>;

  export class MemoryArtifactStore implements ArtifactStore {
    save(artifact: ResultArtifact): Promise<void>;
    get(artifactId: string): Promise<ResultArtifact | null>;
    query(query: ArtifactQuery): Promise<ResultArtifact[]>;
    verify(artifactId: string): Promise<boolean>;
    count(): Promise<number>;
  }

  export function getGlobalArtifactStore(): ArtifactStore;
  export function setGlobalArtifactStore(store: ArtifactStore): void;
  export function resetGlobalArtifactStore(): void;

  export class IpcArtifactStore implements ArtifactStore {
    save(artifact: ResultArtifact): Promise<void>;
    get(artifactId: string): Promise<ResultArtifact | null>;
    query(query: ArtifactQuery): Promise<ResultArtifact[]>;
    verify(artifactId: string): Promise<boolean>;
    count(): Promise<number>;
  }
}

// =============================================================================
// @qontinui/ui-bridge/contracts (source exists, dist not yet built)
// =============================================================================

declare module "@qontinui/ui-bridge/contracts" {
  type SpecAssertion = import("ui-bridge").SpecAssertion;

  export type ContractCheck =
    | {
        type: "assertion";
        assertion: SpecAssertion;
      }
    | {
        type: "api";
        endpoint: string;
        method: "GET" | "POST" | "PUT" | "DELETE";
        expectedStatus: number;
        headers?: Record<string, string>;
        timeout?: number;
      };

  export interface ContractCondition {
    id: string;
    description: string;
    check: ContractCheck;
    onFailure: "skip" | "fail" | "warn";
  }

  export type ContractVerification =
    | {
        type: "specGroupRef";
        specId: string;
        groupId: string;
      }
    | {
        type: "inline";
        assertions: SpecAssertion[];
      };

  export interface ContractMetadata {
    author?: string;
    component?: string;
    pageUrl?: string;
    tags?: string[];
    createdAt?: string;
    updatedAt?: string;
  }

  export interface VerificationContract {
    id: string;
    name: string;
    description: string;
    version: string;
    preconditions: ContractCondition[];
    verification: ContractVerification;
    postconditions: ContractCondition[];
    metadata?: ContractMetadata;
  }

  export interface ContractConfig {
    version: string;
    contracts: VerificationContract[];
    metadata?: {
      author?: string;
      description?: string;
    };
  }

  export interface ConditionResult {
    conditionId: string;
    description: string;
    passed: boolean;
    assertionResult?: unknown;
    apiResult?: {
      status: number;
      ok: boolean;
      durationMs: number;
    };
    onFailure: "skip" | "fail" | "warn";
    skippedContract: boolean;
    durationMs: number;
  }

  export interface ContractExecutionResult {
    contractId: string;
    contractName: string;
    preconditionResults: ConditionResult[];
    preconditionsPassed: boolean;
    skipped: boolean;
    verificationResults: unknown[] | null;
    verificationPassed: boolean;
    postconditionResults: ConditionResult[] | null;
    postconditionsPassed: boolean;
    passed: boolean;
    warnings: string[];
    durationMs: number;
    timestamp: number;
  }

  export const CONTRACT_CONFIG_VERSION: string;
  export const CONTRACT_FILE_EXTENSION: string;

  export class ContractExecutor {
    execute(
      contract: VerificationContract,
      options?: ContractExecutionOptions,
    ): Promise<ContractExecutionResult>;
  }

  export interface ContractExecutionOptions {
    timeout?: number;
    [key: string]: unknown;
  }

  export function validateContractConfig(config: unknown): ContractValidationResult;

  export interface ContractValidationError {
    path: string;
    message: string;
  }

  export interface ContractValidationResult {
    valid: boolean;
    errors: ContractValidationError[];
  }
}

// =============================================================================
// @qontinui/ui-bridge/core — getGlobalRegistry (exported in source, missing in dist)
// Note: Augmented via ui-bridge-augments.d.ts (module file)
// =============================================================================

// =============================================================================
// react-window v2 API (package.json wants ^2.2.7, v1 installed with v1 types)
// =============================================================================

declare module "react-window" {
  import type { Component, Ref, CSSProperties, ComponentType } from "react";

  /**
   * react-window v2 RowComponentProps: the component receives `index`, `style`,
   * plus all properties from the generic type T (spread via rowProps).
   */
  export type RowComponentProps<T = Record<string, never>> = {
    index: number;
    style: CSSProperties;
  } & T;

  export interface ListProps<RP = Record<string, never>> {
    listRef?: Ref<unknown>;
    rowCount: number;
    rowHeight: number | ((index: number) => number);
    rowComponent: ComponentType<RowComponentProps<RP>>;
    rowProps?: RP;
    overscanCount?: number;
    style?: CSSProperties;
    className?: string;
    width?: number | string;
    height?: number | string;
    [key: string]: unknown;
  }

  export class List<RP = Record<string, never>> extends Component<ListProps<RP>> {}

  export function useListRef(initial: unknown): Ref<unknown>;

  // v1 exports (still available at runtime)
  export class FixedSizeList<_T = unknown> extends Component<Record<string, unknown>> {}
  export class VariableSizeList<_T = unknown> extends Component<Record<string, unknown>> {}
  export class FixedSizeGrid<_T = unknown> extends Component<Record<string, unknown>> {}
  export class VariableSizeGrid<_T = unknown> extends Component<Record<string, unknown>> {}
  export function areEqual(
    prevProps: Record<string, unknown>,
    nextProps: Record<string, unknown>,
  ): boolean;
  export function shouldComponentUpdate(
    this: Component,
    nextProps: Record<string, unknown>,
    nextState: Record<string, unknown>,
  ): boolean;
}

// =============================================================================
// html2canvas (missing type declarations)
// =============================================================================

// =============================================================================
// @testing-library/react — used only via dynamic import in vdom-renderer.ts
// (test-only dep, not installed in production)
// =============================================================================

declare module "@testing-library/react" {
  import type { ReactElement } from "react";

  interface RenderResult {
    container: HTMLElement;
    unmount: () => void;
    [key: string]: unknown;
  }

  export function render(element: ReactElement): RenderResult;
  export const screen: Record<string, unknown>;
}

// =============================================================================
// html2canvas (missing type declarations)
// =============================================================================

declare module "html2canvas" {
  interface Html2CanvasOptions {
    allowTaint?: boolean;
    backgroundColor?: string | null;
    canvas?: HTMLCanvasElement;
    foreignObjectRendering?: boolean;
    imageTimeout?: number;
    ignoreElements?: (element: Element) => boolean;
    logging?: boolean;
    onclone?: (document: Document, element: HTMLElement) => void;
    proxy?: string | null;
    removeContainer?: boolean;
    scale?: number;
    useCORS?: boolean;
    width?: number;
    height?: number;
    x?: number;
    y?: number;
    scrollX?: number;
    scrollY?: number;
    windowWidth?: number;
    windowHeight?: number;
    [key: string]: unknown;
  }

  function html2canvas(
    element: HTMLElement,
    options?: Partial<Html2CanvasOptions>,
  ): Promise<HTMLCanvasElement>;

  export default html2canvas;
}
