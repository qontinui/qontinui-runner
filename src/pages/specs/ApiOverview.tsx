import { Globe, Lock, Database } from "lucide-react";
import type { ApiConfig, ApiEndpoint, DataModel, SchemaField } from "./types";
import { StatCard, SPEC_LOAD_SOURCE_TOOLTIPS } from "./spec-badges";

const METHOD_COLORS: Record<string, string> = {
  GET: "bg-green-500/15 text-green-400 border-green-500/30",
  POST: "bg-blue-500/15 text-blue-400 border-blue-500/30",
  PUT: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  PATCH: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  DELETE: "bg-red-500/15 text-red-400 border-red-500/30",
};

function SchemaFieldRow({ field, depth = 0 }: { field: SchemaField; depth?: number }) {
  return (
    <>
      <div
        className="flex items-center gap-2 py-0.5"
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
      >
        <span className="text-xs font-mono text-foreground">{field.name}</span>
        <span className="text-[10px] font-mono text-cyan-400/80">{field.type}</span>
        {field.required && <span className="text-[10px] text-red-400/70">required</span>}
        {field.description && (
          <span className="text-[10px] text-muted-foreground/60 truncate flex-1">
            — {field.description}
          </span>
        )}
      </div>
      {field.fields?.map((nested) => (
        <SchemaFieldRow key={`${field.name}-${nested.name}`} field={nested} depth={depth + 1} />
      ))}
      {field.items && (
        <SchemaFieldRow
          key={`${field.name}-items`}
          field={{ ...field.items, name: `[${field.items.name || "item"}]` }}
          depth={depth + 1}
        />
      )}
    </>
  );
}

function EndpointRow({ endpoint }: { endpoint: ApiEndpoint }) {
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] hover:bg-white/[0.04] transition-colors space-y-2">
      <div className="flex items-center gap-2">
        <span
          className={`text-[10px] font-mono font-bold px-2 py-0.5 rounded border ${METHOD_COLORS[endpoint.method] || METHOD_COLORS.GET}`}
        >
          {endpoint.method}
        </span>
        <span className="text-xs font-mono text-foreground">{endpoint.path}</span>
        {endpoint.auth && <Lock className="w-3 h-3 text-amber-400/60" />}
        {endpoint.featureId && (
          <span className="text-[10px] text-muted-foreground/50 ml-auto">
            feature: {endpoint.featureId}
          </span>
        )}
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{endpoint.description}</p>

      {endpoint.pathParams && endpoint.pathParams.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Path params:</span>
          <div className="mt-0.5 border-l-2 border-white/5">
            {endpoint.pathParams.map((p) => (
              <SchemaFieldRow key={`path-${p.name}`} field={p} />
            ))}
          </div>
        </div>
      )}
      {endpoint.queryParams && endpoint.queryParams.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Query params:</span>
          <div className="mt-0.5 border-l-2 border-white/5">
            {endpoint.queryParams.map((p) => (
              <SchemaFieldRow key={`query-${p.name}`} field={p} />
            ))}
          </div>
        </div>
      )}
      {endpoint.requestBody && endpoint.requestBody.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Request body:</span>
          <div className="mt-0.5 border-l-2 border-cyan-500/20">
            {endpoint.requestBody.map((f) => (
              <SchemaFieldRow key={`req-${f.name}`} field={f} />
            ))}
          </div>
        </div>
      )}
      {endpoint.responseBody && endpoint.responseBody.length > 0 && (
        <div>
          <span className="text-[10px] font-medium text-muted-foreground">Response:</span>
          <div className="mt-0.5 border-l-2 border-green-500/20">
            {endpoint.responseBody.map((f) => (
              <SchemaFieldRow key={`res-${f.name}`} field={f} />
            ))}
          </div>
        </div>
      )}

      {endpoint.statusCodes && endpoint.statusCodes.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap">
          <span className="text-[10px] font-medium text-muted-foreground">Status:</span>
          {endpoint.statusCodes.map((sc) => (
            <span
              key={sc.code}
              className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
              title={sc.description}
            >
              {sc.code}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function ModelCard({ model }: { model: DataModel }) {
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] space-y-2">
      <div className="flex items-center gap-2">
        <Database className="w-3.5 h-3.5 text-green-400/60" />
        <span className="text-xs font-medium text-foreground">{model.name}</span>
        <span className="text-[10px] text-muted-foreground">{model.fields.length} fields</span>
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{model.description}</p>

      <div className="border-l-2 border-white/5">
        {model.fields.map((f) => (
          <SchemaFieldRow key={`${model.id}-${f.name}`} field={f} />
        ))}
      </div>

      {model.relations && model.relations.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap pt-1 border-t border-white/5">
          <span className="text-[10px] font-medium text-muted-foreground">Relations:</span>
          {model.relations.map((rel) => (
            <span
              key={`${rel.modelId}-${rel.type}`}
              className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            >
              {rel.type} → {rel.modelId}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

export function ApiOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: ApiConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  const methodCounts = new Map<string, number>();
  for (const ep of config.endpoints) {
    methodCounts.set(ep.method, (methodCounts.get(ep.method) || 0) + 1);
  }

  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Globe className="w-4 h-4 text-cyan-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || config.basePath}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20 font-medium">
            api
          </span>
          {appName && (
            <span
              className="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20"
              title={`Application: ${appName}`}
            >
              {appName}
            </span>
          )}
          <span
            className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            title={SPEC_LOAD_SOURCE_TOOLTIPS[source] || `Source: ${source}`}
          >
            {source}
          </span>
        </div>
        <div className="flex items-center gap-2 mt-1.5">
          <span className="text-xs font-mono text-muted-foreground">{config.basePath}</span>
          {config.authType && config.authType !== "none" && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20">
              <Lock className="w-2.5 h-2.5 inline mr-0.5" />
              {config.authType}
            </span>
          )}
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Endpoints" value={config.endpoints.length} color="text-cyan-400" />
        <StatCard label="Models" value={config.models.length} color="text-green-400" />
        <StatCard
          label="Auth Required"
          value={config.endpoints.filter((e) => e.auth).length}
          color="text-amber-400"
        />
        <StatCard
          label="Methods"
          value={Array.from(methodCounts.entries())
            .map(([m, c]) => `${m}:${c}`)
            .join(" ")}
        />
      </div>

      {config.endpoints.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Endpoints ({config.endpoints.length})
          </h3>
          <div className="space-y-2">
            {config.endpoints.map((endpoint) => (
              <EndpointRow key={endpoint.id} endpoint={endpoint} />
            ))}
          </div>
        </div>
      )}

      {config.models.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Data Models ({config.models.length})
          </h3>
          <div className="space-y-2">
            {config.models.map((model) => (
              <ModelCard key={model.id} model={model} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
