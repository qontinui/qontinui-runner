import { Database, Table2, KeyRound, ArrowRight } from "lucide-react";
import type { DataConfig, DataEntity, DataColumn, DataRelation } from "./types";
import { StatCard, SPEC_LOAD_SOURCE_TOOLTIPS } from "./spec-badges";

function ColumnRow({ column }: { column: DataColumn }) {
  return (
    <div className="flex items-center gap-2 py-0.5 px-2">
      <span className="text-xs font-mono text-foreground">{column.name}</span>
      <span className="text-[10px] font-mono text-cyan-400/80">{column.type}</span>
      {column.primaryKey && <KeyRound className="w-2.5 h-2.5 text-amber-400" />}
      {column.required && <span className="text-[10px] text-red-400/70">NOT NULL</span>}
      {column.unique && <span className="text-[10px] text-purple-400/70">UNIQUE</span>}
      {column.defaultValue && (
        <span className="text-[10px] text-muted-foreground/50">default: {column.defaultValue}</span>
      )}
      {column.description && (
        <span className="text-[10px] text-muted-foreground/60 truncate flex-1">
          — {column.description}
        </span>
      )}
    </div>
  );
}

function EntityCard({ entity }: { entity: DataEntity }) {
  const pkColumns = entity.columns.filter((c) => c.primaryKey);
  return (
    <div className="px-3 py-2.5 rounded border border-white/5 bg-white/[0.02] space-y-2">
      <div className="flex items-center gap-2">
        <Table2 className="w-3.5 h-3.5 text-green-400/60" />
        <span className="text-xs font-medium text-foreground">{entity.name}</span>
        <span className="text-[10px] text-muted-foreground">{entity.columns.length} columns</span>
        {pkColumns.length > 0 && (
          <span className="text-[10px] text-amber-400/60">
            PK: {pkColumns.map((c) => c.name).join(", ")}
          </span>
        )}
      </div>
      <p className="text-[10px] text-muted-foreground leading-relaxed">{entity.description}</p>

      <div className="border-l-2 border-white/5">
        {entity.columns.map((col) => (
          <ColumnRow key={`${entity.id}-${col.name}`} column={col} />
        ))}
      </div>

      {entity.indexes && entity.indexes.length > 0 && (
        <div className="flex items-center gap-2 flex-wrap pt-1 border-t border-white/5">
          <span className="text-[10px] font-medium text-muted-foreground">Indexes:</span>
          {entity.indexes.map((idx) => (
            <span
              key={idx.name}
              className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10"
            >
              {idx.unique ? "UNIQUE " : ""}
              {idx.name} ({idx.columns.join(", ")})
            </span>
          ))}
        </div>
      )}

      {entity.timestamps && (
        <div className="flex items-center gap-2 text-[10px] text-muted-foreground/50 pt-1 border-t border-white/5">
          {entity.timestamps.createdAt && <span>created: {entity.timestamps.createdAt}</span>}
          {entity.timestamps.updatedAt && <span>updated: {entity.timestamps.updatedAt}</span>}
          {entity.softDelete && <span>soft delete: {entity.softDelete}</span>}
        </div>
      )}
    </div>
  );
}

function RelationRow({ relation }: { relation: DataRelation }) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5">
      <span className="text-xs font-mono text-foreground">{relation.fromEntity}</span>
      <span className="text-[10px] text-muted-foreground/60">
        ({relation.fromColumns.join(", ")})
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
        {relation.type}
      </span>
      <ArrowRight className="w-3 h-3 text-muted-foreground/50" />
      <span className="text-xs font-mono text-foreground">{relation.toEntity}</span>
      <span className="text-[10px] text-muted-foreground/60">
        ({relation.toColumns.join(", ")})
      </span>
      {relation.onDelete && (
        <span className="text-[10px] text-red-400/50 ml-auto">ON DELETE {relation.onDelete}</span>
      )}
    </div>
  );
}

export function DataOverview({
  config,
  specId, // eslint-disable-line @typescript-eslint/no-unused-vars
  source,
  appName,
}: {
  config: DataConfig;
  specId: string;
  source: string;
  appName?: string;
}) {
  return (
    <div className="space-y-5">
      <div>
        <div className="flex items-center gap-2">
          <Database className="w-4 h-4 text-green-400" />
          <h2 className="text-sm font-semibold text-foreground">
            {config.description || "Data Schema"}
          </h2>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/10 text-green-400 border border-green-500/20 font-medium">
            data
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
          <span className="text-[10px] font-mono text-muted-foreground">{config.database}</span>
        </div>
      </div>

      <div className="grid grid-cols-4 gap-3">
        <StatCard label="Entities" value={config.entities.length} color="text-green-400" />
        <StatCard label="Relations" value={config.relations.length} color="text-cyan-400" />
        <StatCard label="Seeds" value={config.seeds?.length || 0} color="text-amber-400" />
        <StatCard
          label="Migrations"
          value={config.migrations?.length || 0}
          color="text-purple-400"
        />
      </div>

      {config.entities.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Entities ({config.entities.length})
          </h3>
          <div className="space-y-2">
            {config.entities.map((entity) => (
              <EntityCard key={entity.id} entity={entity} />
            ))}
          </div>
        </div>
      )}

      {config.relations.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Relations ({config.relations.length})
          </h3>
          <div className="space-y-1">
            {config.relations.map((rel) => (
              <RelationRow key={rel.id} relation={rel} />
            ))}
          </div>
        </div>
      )}

      {config.seeds && config.seeds.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Seed Data ({config.seeds.length})
          </h3>
          <div className="space-y-1">
            {config.seeds.map((seed) => (
              <div
                key={`${seed.entityId}-${seed.environment}`}
                className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
              >
                <span className="text-xs font-mono text-foreground">{seed.entityId}</span>
                <span className="text-[10px] text-muted-foreground">
                  {typeof seed.records === "number"
                    ? `${seed.records} records`
                    : `${seed.records.length} records`}
                </span>
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-white/5 text-muted-foreground border border-white/10">
                  {seed.environment}
                </span>
                {seed.description && (
                  <span className="text-[10px] text-muted-foreground/50 truncate flex-1 text-right">
                    {seed.description}
                  </span>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {config.migrations && config.migrations.length > 0 && (
        <div>
          <h3 className="text-xs font-medium text-muted-foreground mb-2">
            Planned Migrations ({config.migrations.length})
          </h3>
          <div className="space-y-1">
            {config.migrations
              .sort((a, b) => a.order - b.order)
              .map((mig) => (
                <div
                  key={mig.id}
                  className="flex items-center gap-2 px-3 py-1.5 rounded bg-white/[0.02] border border-white/5"
                >
                  <span className="text-[10px] font-mono text-muted-foreground/50 tabular-nums w-6">
                    #{mig.order}
                  </span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-purple-500/10 text-purple-400 border border-purple-500/20">
                    {mig.changeType}
                  </span>
                  <span className="text-xs text-foreground flex-1">{mig.description}</span>
                  <span className="text-[10px] text-muted-foreground/50">
                    {mig.entityIds.join(", ")}
                  </span>
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}
