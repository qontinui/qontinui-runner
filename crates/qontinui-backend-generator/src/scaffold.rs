//! Deterministic scaffold (Phase 1, no LLM) — emits the layered FastAPI skeleton plus
//! one SQLAlchemy model + Pydantic schema per entity and one FastAPI route per
//! operation. Mirrors the reference backend style (`qontinui-web/backend`:
//! `Mapped[...]`, `mapped_column`, a declarative `Base`, `__tablename__`,
//! `__table_args__`) and honors the `Profile` conventions (`pluralize` table names via
//! the shared `endpoint_for` table rule, `snake_case` columns).
//!
//! Output is a [`GeneratedBackend`]: a map of relative path → file content, plus a
//! structured view ([`EmittedModel`] / [`EmittedRoute`]) the golden test asserts
//! against. Route method/path is taken **only** from [`crate::route_map`] (the shared
//! `endpoint_for`), never re-derived here.

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{Entity, EntityField, FunctionalSpec};
use qontinui_types::priorities_profile::Profile;

use crate::route_map::{openapi_document, route_for, RouteBinding};

/// A generated backend: its files and a structured view of what was emitted.
#[derive(Debug, Clone)]
pub struct GeneratedBackend {
    /// Relative-path → file-content for the whole emitted tree (deterministic order).
    pub files: BTreeMap<String, String>,
    /// One per spec entity, in spec order.
    pub models: Vec<EmittedModel>,
    /// One per spec operation, in spec order.
    pub routes: Vec<EmittedRoute>,
    /// The derived OpenAPI aid (Fork C) — a verification aid, not a #1 contract.
    pub openapi: serde_json::Value,
}

impl GeneratedBackend {
    /// Look up an emitted route by the spec operation name.
    pub fn route(&self, operation_name: &str) -> Option<&EmittedRoute> {
        self.routes.iter().find(|r| r.operation == operation_name)
    }

    /// Look up an emitted model by the spec entity name.
    pub fn model(&self, entity_name: &str) -> Option<&EmittedModel> {
        self.models.iter().find(|m| m.entity == entity_name)
    }

    /// Write the emitted file tree under `root` (creating parent dirs). Used by the
    /// deferred runtime-integration tier to materialize the backend before bringing it
    /// up under docker-compose. The deterministic golden tests do NOT touch disk.
    pub fn materialize_to(&self, root: &std::path::Path) -> std::io::Result<()> {
        for (rel, contents) in &self.files {
            let dest = root.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, contents)?;
        }
        Ok(())
    }
}

/// A structured view of one emitted SQLAlchemy model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedModel {
    /// Spec entity name (`Device`).
    pub entity: String,
    /// Table name under the profile conventions (`devices`).
    pub table: String,
    /// One column name per spec [`EntityField`], in field order (plus the synthetic
    /// `id` primary key first).
    pub columns: Vec<String>,
    /// The emitted python source for the model file.
    pub source: String,
}

/// A structured view of one emitted FastAPI route. `method`/`path` are exactly the
/// shared `endpoint_for` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmittedRoute {
    /// Spec operation name (`pairConfirm`).
    pub operation: String,
    /// HTTP method from `endpoint_for` (`POST`).
    pub method: String,
    /// Path from `endpoint_for` (`/api/v1/devices/pair-confirm`).
    pub path: String,
    /// The emitted python source for the route file.
    pub source: String,
}

/// Generate a FastAPI backend scaffold for the given spec + profile (Phase 1 core).
///
/// Deterministic and side-effect-free: returns the file tree + structured view; it
/// does NOT write to disk, run migrations, or start a server.
pub fn generate_phase1(spec: &FunctionalSpec, profile: &Profile) -> GeneratedBackend {
    let mut files: BTreeMap<String, String> = BTreeMap::new();

    // --- Project skeleton (fixed, profile-aware) ---
    files.insert("pyproject.toml".to_string(), emit_pyproject(spec, profile));
    files.insert("app/__init__.py".to_string(), String::new());
    files.insert("app/db/__init__.py".to_string(), String::new());
    files.insert("app/db/base.py".to_string(), emit_db_base());
    files.insert("app/db/session.py".to_string(), emit_db_session());
    files.insert("app/models/__init__.py".to_string(), String::new());
    files.insert("app/schemas/__init__.py".to_string(), String::new());
    files.insert("app/api/__init__.py".to_string(), String::new());

    // --- Models + schemas (one per entity) ---
    let mut models = Vec::new();
    for entity in &spec.entities {
        let m = emit_model(entity, profile);
        files.insert(
            format!("app/models/{}.py", snake(&m.entity)),
            m.source.clone(),
        );
        files.insert(
            format!("app/schemas/{}.py", snake(&m.entity)),
            emit_schema(entity),
        );
        models.push(m);
    }

    // --- Routes (one per operation), method/path from the shared endpoint_for ---
    let mut routes = Vec::new();
    let mut bindings: Vec<RouteBinding> = Vec::new();
    for op in &spec.operations {
        let binding = route_for(op, profile);
        let r = emit_route(&binding);
        files.insert(format!("app/api/{}.py", snake(&op.name)), r.source.clone());
        routes.push(r);
        bindings.push(binding);
    }

    // --- App entrypoint wiring every router ---
    files.insert("app/main.py".to_string(), emit_main(spec, &bindings));

    // --- OpenAPI aid (Fork C) ---
    let openapi = openapi_document(&bindings, &project_title(spec));
    files.insert(
        "openapi.json".to_string(),
        serde_json::to_string_pretty(&openapi).unwrap_or_else(|_| "{}".to_string()),
    );

    GeneratedBackend {
        files,
        models,
        routes,
        openapi,
    }
}

// ===========================================================================
// Model emission (SQLAlchemy 2.0, reference-backend style)
// ===========================================================================

fn emit_model(entity: &Entity, profile: &Profile) -> EmittedModel {
    let table = table_name(entity, profile);
    let class = pascal(&entity.name);

    let mut columns: Vec<String> = vec!["id".to_string()];
    let mut lines: Vec<String> = Vec::new();
    // Synthetic primary key — every generated table gets one (REST resource identity).
    lines.push(
        "    id: Mapped[int] = mapped_column(primary_key=True, autoincrement=True)".to_string(),
    );
    for f in &entity.fields {
        let col = snake(&f.name);
        let (py_ty, sa_ty) = sqlalchemy_type(f);
        lines.push(format!(
            "    {col}: Mapped[{py_ty}] = mapped_column({sa_ty}, nullable=False)"
        ));
        columns.push(col);
    }

    let source = format!(
        "\"\"\"SQLAlchemy model for the `{name}` entity (generated, deterministic scaffold).\"\"\"\n\
from sqlalchemy import String, Integer\n\
from sqlalchemy.orm import Mapped, mapped_column\n\
\n\
from app.db.base import Base\n\
\n\
\n\
class {class}(Base):\n\
    __tablename__ = \"{table}\"\n\
\n\
{cols}\n",
        name = entity.name,
        class = class,
        table = table,
        cols = lines.join("\n"),
    );

    EmittedModel {
        entity: entity.name.clone(),
        table,
        columns,
        source,
    }
}

/// Map a spec field type → (python type, SQLAlchemy column type). v0 conservative
/// mapping; unknown coarse types fall back to `String`. Phase 2 widens this.
fn sqlalchemy_type(f: &EntityField) -> (&'static str, &'static str) {
    match f.field_type.as_str() {
        "number" => ("int", "Integer"),
        "bool" => ("bool", "Boolean"),
        // string / enum / date / money / reference / unknown -> textual column in v0.
        _ => ("str", "String"),
    }
}

fn emit_schema(entity: &Entity) -> String {
    let class = pascal(&entity.name);
    let mut fields: Vec<String> = Vec::new();
    for f in &entity.fields {
        let (py_ty, _) = sqlalchemy_type(f);
        fields.push(format!("    {}: {}", snake(&f.name), py_ty));
    }
    if fields.is_empty() {
        fields.push("    pass".to_string());
    }
    format!(
        "\"\"\"Pydantic schema for `{name}` (generated, deterministic scaffold).\"\"\"\n\
from pydantic import BaseModel\n\
\n\
\n\
class {class}Read(BaseModel):\n\
    id: int\n\
{fields}\n",
        name = entity.name,
        class = class,
        fields = fields.join("\n"),
    )
}

// ===========================================================================
// Route emission (FastAPI) — method/path from the shared endpoint_for
// ===========================================================================

fn emit_route(binding: &RouteBinding) -> EmittedRoute {
    let op = &binding.operation;
    let method = binding.endpoint.method.clone();
    let path = binding.endpoint.path.clone();
    let decorator = method.to_ascii_lowercase();
    let func = snake(&op.name);

    // The handler body is a deterministic typed stub; per Fork A the real effect
    // ("persists the pairing and returns a device token") is LLM-filled later. The
    // stub returns the assumed-shape 201 so the route signature is real and testable.
    let assumption = op
        .effect
        .as_ref()
        .and_then(|e| e.assumption.clone())
        .unwrap_or_else(|| "generated effect (assumed)".to_string());

    let source = format!(
        "\"\"\"FastAPI route for the `{op}` operation (generated scaffold).\n\
\n\
Route derived from the shared `endpoint_for`: {method} {path}\n\
Assumed effect: {assumption}\n\
\"\"\"\n\
from fastapi import APIRouter, status\n\
\n\
router = APIRouter()\n\
\n\
\n\
@router.{decorator}(\"{path}\", status_code=status.HTTP_201_CREATED)\n\
async def {func}() -> dict:\n\
    # TODO(codegen): LLM-filled body — {assumption}\n\
    return {{\"deviceToken\": \"\"}}\n",
        op = op.name,
        method = method,
        path = path,
        assumption = assumption,
        decorator = decorator,
        func = func,
    );

    EmittedRoute {
        operation: op.name.clone(),
        method,
        path,
        source,
    }
}

// ===========================================================================
// Project skeleton files
// ===========================================================================

fn emit_main(spec: &FunctionalSpec, bindings: &[RouteBinding]) -> String {
    let mut imports = Vec::new();
    let mut includes = Vec::new();
    for b in bindings {
        let module = snake(&b.operation.name);
        imports.push(format!("from app.api import {module}"));
        includes.push(format!("app.include_router({module}.router)"));
    }
    let auth_note = spec
        .auth
        .as_ref()
        .map(|a| format!("# auth model (spec): {}\n", a.model))
        .unwrap_or_default();
    format!(
        "\"\"\"FastAPI application entrypoint (generated scaffold).\"\"\"\n\
from fastapi import FastAPI\n\
\n\
{imports}\n\
\n\
{auth_note}app = FastAPI(title=\"{title}\")\n\
\n\
{includes}\n",
        imports = imports.join("\n"),
        auth_note = auth_note,
        title = project_title(spec),
        includes = includes.join("\n"),
    )
}

fn emit_db_base() -> String {
    "\"\"\"Declarative SQLAlchemy 2.0 Base (generated scaffold).\"\"\"\n\
from sqlalchemy.orm import DeclarativeBase\n\
\n\
\n\
class Base(DeclarativeBase):\n\
    pass\n"
        .to_string()
}

fn emit_db_session() -> String {
    "\"\"\"Async SQLAlchemy session factory (generated scaffold).\"\"\"\n\
import os\n\
\n\
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine\n\
\n\
DATABASE_URL = os.environ.get(\n\
    \"DATABASE_URL\", \"postgresql+asyncpg://postgres:postgres@localhost:5432/app\"\n\
)\n\
engine = create_async_engine(DATABASE_URL, future=True)\n\
SessionLocal = async_sessionmaker(engine, expire_on_commit=False)\n"
        .to_string()
}

fn emit_pyproject(spec: &FunctionalSpec, profile: &Profile) -> String {
    format!(
        "[project]\n\
name = \"{slug}\"\n\
version = \"0.1.0\"\n\
description = \"Generated backend for {url} (stack: {lang}/{fw}/{store}/{api})\"\n\
requires-python = \">=3.11\"\n\
dependencies = [\n\
    \"fastapi\",\n\
    \"uvicorn[standard]\",\n\
    \"sqlalchemy>=2.0\",\n\
    \"asyncpg\",\n\
    \"alembic\",\n\
    \"pydantic>=2\",\n\
]\n",
        slug = slug(spec),
        url = spec.target.source_url,
        lang = profile.backend.language,
        fw = profile.backend.framework,
        store = profile.backend.datastore,
        api = api_style_str(profile),
    )
}

fn api_style_str(profile: &Profile) -> &'static str {
    use qontinui_types::priorities_profile::ApiStyle;
    match profile.backend.api_style {
        ApiStyle::Rest => "rest",
        ApiStyle::Graphql => "graphql",
    }
}

// ===========================================================================
// Naming helpers
// ===========================================================================

/// Table name for an entity under the profile conventions. Derived by reusing the
/// shared `endpoint_for` path rule (so the table name and the route path cannot drift):
/// `endpoint_for(create<Entity>, profile).path` == `/api/v1/{table}`, last segment = table.
fn table_name(entity: &Entity, profile: &Profile) -> String {
    use qontinui_types::endpoint_for::endpoint_for;
    use qontinui_types::functional_spec::{Operation, SpecProvenance};
    // A synthetic bare-CRUD op `create<Entity>` addresses the collection, so its
    // derived path's final segment is exactly the profile-conventioned table name.
    let probe = Operation {
        name: format!("create{}", entity.name),
        verb: "create".to_string(),
        entity: Some(entity.name.clone()),
        inputs: vec![],
        effect: None,
        confidence: SpecProvenance::Observed,
        provenance: None,
        credibility: None,
    };
    let ep = endpoint_for(&probe, profile);
    ep.path
        .rsplit('/')
        .next()
        .unwrap_or(&entity.name)
        .to_string()
}

fn project_title(spec: &FunctionalSpec) -> String {
    slug(spec)
}

/// A filesystem-safe slug from the spec target URL (the artifacts dir name in Fork B).
fn slug(spec: &FunctionalSpec) -> String {
    let url = &spec.target.source_url;
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let mut out = String::with_capacity(stripped.len());
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if (ch == '.' || ch == '/' || ch == '-' || ch == '_')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `PascalCase`/`camelCase`/`kebab`/`space` → `snake_case`.
fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch == '-' || ch == '_' || ch == ' ' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
        } else if ch.is_ascii_uppercase() {
            if i != 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `camelCase`/`snake`/`kebab` → `PascalCase`.
fn pascal(s: &str) -> String {
    let snaked = snake(s);
    let mut out = String::with_capacity(snaked.len());
    let mut upper_next = true;
    for ch in snaked.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}
