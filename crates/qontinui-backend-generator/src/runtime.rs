//! Phase 2 — the **runnable** backend (real persistence + a live endpoint).
//!
//! Phase 1 ([`crate::scaffold::generate_phase1`]) proves the deterministic spec→source
//! seam but emits a stub handler (`return {"deviceToken": ""}`), an async session, and
//! no table creation — so it cannot actually be brought up and observed. This module
//! emits a backend that **really persists and behaves**: a synchronous SQLAlchemy 2.0
//! stack (psycopg) whose `Base.metadata.create_all` runs on startup (so the `devices`
//! table truly exists), and a `pairConfirm` handler that **inserts a `Device` row** and
//! returns a token-shaped body with `201`. It reuses Phase 1's spec walk for the model
//! columns + the shared `endpoint_for` route derivation — route method/path are still
//! taken **only** from [`crate::route_map`], never re-derived.
//!
//! The point is falsifiable: a verifier brings this tree up under docker-compose, POSTs
//! a pairing, reads the real HTTP response, and introspects the live `devices` table —
//! coverage is observed from the running server, exactly the plan's premise. See
//! `tests/runtime_integration.rs`.

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{Entity, FunctionalSpec};
use qontinui_types::priorities_profile::Profile;

use crate::route_map::{openapi_document, route_for, RouteBinding};
use crate::scaffold::{generate_phase1, EmittedModel, EmittedRoute, GeneratedBackend};

/// Generate a **runnable** FastAPI backend (Phase 2): real persistence, table creation
/// on startup, and a `pairConfirm` handler that inserts a `Device` and returns a token.
///
/// Deterministic and side-effect-free (returns the file tree). The LLM-authored body
/// depth (Fork A) is intentionally NOT used here — a correct deterministic handler that
/// really persists is preferable to an unverified LLM call for this milestone.
///
/// `break_pairing` injects a deliberate fault so the verifier can prove a broken backend
/// surfaces a [`qontinui_types::completeness_eval::CoverageGap`]:
/// - [`BreakMode::None`] — the correct runnable backend.
/// - [`BreakMode::Handler500`] — the handler raises before persisting (effect broken).
/// - [`BreakMode::DropColumn`] — the named column is omitted from the model (schema gap).
pub fn generate_runnable(
    spec: &FunctionalSpec,
    profile: &Profile,
    break_mode: BreakMode,
) -> GeneratedBackend {
    // Start from the deterministic Phase-1 tree (skeleton + per-entity model/schema +
    // per-operation route + openapi aid), then overwrite the files that must become
    // real for a live bring-up.
    let mut backend = generate_phase1(spec, profile);
    let files: &mut BTreeMap<String, String> = &mut backend.files;

    // --- Declarative Base (re-emitted: Phase-1's used a string-continuation that
    //     stripped the `pass` indent — fine for its golden test, not for a real boot) ---
    files.insert(
        "app/db/base.py".to_string(),
        [
            "\"\"\"Declarative SQLAlchemy 2.0 Base (generated, runnable).\"\"\"",
            "from sqlalchemy.orm import DeclarativeBase",
            "",
            "",
            "class Base(DeclarativeBase):",
            "    pass",
            "",
        ]
        .join("\n"),
    );

    // --- Real synchronous session + table creation on startup ---
    files.insert("app/db/session.py".to_string(), emit_sync_session());

    // --- Models: re-emit so a DropColumn fault can remove a real column ---
    let mut new_models: Vec<EmittedModel> = Vec::new();
    for entity in &spec.entities {
        let dropped = match &break_mode {
            BreakMode::DropColumn { entity: e, column } if e == &entity.name => {
                Some(column.as_str())
            }
            _ => None,
        };
        let m = emit_runtime_model(entity, profile, dropped);
        files.insert(
            format!("app/models/{}.py", snake(&m.entity)),
            m.source.clone(),
        );
        // Re-emit the Pydantic schema with correct indentation (Phase-1's used a
        // string-continuation that stripped the first field's indent).
        files.insert(
            format!("app/schemas/{}.py", snake(&entity.name)),
            emit_runtime_schema(entity, dropped),
        );
        new_models.push(m);
    }
    backend.models = new_models;

    // --- Routes: re-emit pairConfirm as a REAL persisting handler ---
    let mut new_routes: Vec<EmittedRoute> = Vec::new();
    let mut bindings: Vec<RouteBinding> = Vec::new();
    for op in &spec.operations {
        let binding = route_for(op, profile);
        let r = emit_runtime_route(spec, &binding, &break_mode);
        files.insert(format!("app/api/{}.py", snake(&op.name)), r.source.clone());
        new_routes.push(r);
        bindings.push(binding);
    }
    backend.routes = new_routes;

    // --- Models package re-export so create_all discovers every model ---
    files.insert("app/models/__init__.py".to_string(), emit_models_init(spec));

    // --- App entrypoint: create tables on startup + a /healthz probe ---
    files.insert(
        "app/main.py".to_string(),
        emit_runtime_main(spec, &bindings),
    );

    // --- Refresh the OpenAPI aid (unchanged routes, kept consistent) ---
    let openapi = openapi_document(&bindings, &project_title(spec));
    files.insert(
        "openapi.json".to_string(),
        serde_json::to_string_pretty(&openapi).unwrap_or_else(|_| "{}".to_string()),
    );
    backend.openapi = openapi;

    backend
}

/// A deliberate fault the verifier injects to prove the evidence reflects the *running*
/// server, not the generator's own claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakMode {
    /// The correct, runnable backend.
    None,
    /// The `pairConfirm` handler raises a 500 before persisting (effect broken).
    Handler500,
    /// The named column is omitted from the named entity's model (schema gap).
    DropColumn { entity: String, column: String },
}

// ===========================================================================
// Synchronous session + declarative base (psycopg, SQLAlchemy 2.0)
// ===========================================================================

fn emit_sync_session() -> String {
    // Synchronous engine: simplest stack that lets `create_all` run at startup without
    // an async event loop. Driver = psycopg (v3). The DATABASE_URL the compose file
    // injects uses the `postgresql+psycopg://` scheme.
    [
        "\"\"\"Synchronous SQLAlchemy 2.0 session + engine (generated, runnable).\"\"\"",
        "import os",
        "",
        "from sqlalchemy import create_engine",
        "from sqlalchemy.orm import sessionmaker",
        "",
        "DATABASE_URL = os.environ.get(",
        "    \"DATABASE_URL\", \"postgresql+psycopg://postgres:postgres@localhost:5432/app\"",
        ")",
        "engine = create_engine(DATABASE_URL, future=True, pool_pre_ping=True)",
        "SessionLocal = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)",
        "",
        "",
        "def get_db():",
        "    db = SessionLocal()",
        "    try:",
        "        yield db",
        "    finally:",
        "        db.close()",
        "",
        "",
        "def init_db() -> None:",
        "    \"\"\"Create every mapped table; imports models so they register on Base.metadata.\"\"\"",
        "    from app.db.base import Base",
        "    import app.models  # noqa: F401  (registers all models on Base.metadata)",
        "",
        "    Base.metadata.create_all(bind=engine)",
        "",
    ]
    .join("\n")
}

/// Emit a runnable SQLAlchemy model. `dropped_column` (if any) is the snake_case column
/// to deliberately omit (the DropColumn fault).
fn emit_runtime_model(
    entity: &Entity,
    profile: &Profile,
    dropped_column: Option<&str>,
) -> EmittedModel {
    let table = table_name(entity, profile);
    let class = pascal(&entity.name);

    let mut columns: Vec<String> = vec!["id".to_string()];
    let mut lines: Vec<String> = Vec::new();
    lines.push(
        "    id: Mapped[int] = mapped_column(primary_key=True, autoincrement=True)".to_string(),
    );
    for f in &entity.fields {
        let col = snake(&f.name);
        if Some(col.as_str()) == dropped_column {
            // Deliberately omit this column — the live schema introspection will miss it.
            continue;
        }
        lines.push(format!(
            "    {col}: Mapped[str | None] = mapped_column(String, nullable=True)"
        ));
        columns.push(col);
    }

    let mut src_lines: Vec<String> = vec![
        format!(
            "\"\"\"SQLAlchemy model for the `{}` entity (generated, runnable).\"\"\"",
            entity.name
        ),
        "from sqlalchemy import String".to_string(),
        "from sqlalchemy.orm import Mapped, mapped_column".to_string(),
        String::new(),
        "from app.db.base import Base".to_string(),
        String::new(),
        String::new(),
        format!("class {class}(Base):"),
        format!("    __tablename__ = \"{table}\""),
        String::new(),
    ];
    src_lines.extend(lines);
    src_lines.push(String::new());
    let source = src_lines.join("\n");

    EmittedModel {
        entity: entity.name.clone(),
        table,
        columns,
        source,
    }
}

/// Emit the Pydantic read-schema for an entity (correct indentation).
fn emit_runtime_schema(entity: &Entity, dropped_column: Option<&str>) -> String {
    let class = pascal(&entity.name);
    let mut lines: Vec<String> = vec![
        format!(
            "\"\"\"Pydantic schema for `{}` (generated, runnable).\"\"\"",
            entity.name
        ),
        "from pydantic import BaseModel".to_string(),
        String::new(),
        String::new(),
        format!("class {class}Read(BaseModel):"),
        "    id: int".to_string(),
    ];
    for f in &entity.fields {
        let col = snake(&f.name);
        if Some(col.as_str()) == dropped_column {
            continue;
        }
        lines.push(format!("    {col}: str | None = None"));
    }
    lines.push(String::new());
    lines.join("\n")
}

// Re-export the models package so `import app.models` registers every model.
fn emit_models_init(spec: &FunctionalSpec) -> String {
    let mut lines = Vec::new();
    for e in &spec.entities {
        let module = snake(&e.name);
        let class = pascal(&e.name);
        lines.push(format!(
            "from app.models.{module} import {class}  # noqa: F401"
        ));
    }
    format!(
        "\"\"\"Model package — re-exports every model so create_all sees them.\"\"\"\n{}\n",
        lines.join("\n")
    )
}

// ===========================================================================
// Runnable route handler — real persistence
// ===========================================================================

fn emit_runtime_route(
    spec: &FunctionalSpec,
    binding: &RouteBinding,
    break_mode: &BreakMode,
) -> EmittedRoute {
    let op = &binding.operation;
    let method = binding.endpoint.method.clone();
    let path = binding.endpoint.path.clone();
    let decorator = method.to_ascii_lowercase();
    let func = snake(&op.name);

    let assumption = op
        .effect
        .as_ref()
        .and_then(|e| e.assumption.clone())
        .unwrap_or_else(|| "generated effect (assumed)".to_string());

    // Resolve the entity this op targets so the handler persists the right model.
    let entity_name = op.entity.clone().unwrap_or_default();
    let entity = spec.entities.iter().find(|e| e.name == entity_name);

    // For an entity-bound `create`/custom op we emit a real persisting handler.
    if let Some(entity) = entity {
        let class = pascal(&entity.name);
        let module = snake(&entity.name);
        // Request fields: the spec entity fields (all optional str in v0).
        let field_cols: Vec<String> = entity.fields.iter().map(|f| snake(&f.name)).collect();
        let mut req_field_lines: Vec<String> = if field_cols.is_empty() {
            vec!["    pass".to_string()]
        } else {
            field_cols
                .iter()
                .map(|c| format!("    {c}: str | None = None"))
                .collect()
        };

        // A device-id-shaped echo for the response body (if the entity has one).
        let echo_device_id = if field_cols.iter().any(|c| c == "device_id") {
            "payload.device_id".to_string()
        } else {
            "None".to_string()
        };

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "\"\"\"FastAPI route for the `{}` operation (generated, runnable).",
            op.name
        ));
        lines.push(String::new());
        lines.push(format!(
            "Route derived from the shared `endpoint_for`: {method} {path}"
        ));
        lines.push(format!("Assumed effect: {assumption}"));
        lines.push("\"\"\"".to_string());
        lines.push("import secrets".to_string());
        lines.push(String::new());
        lines.push("from fastapi import APIRouter, Depends, status".to_string());
        lines.push("from pydantic import BaseModel".to_string());
        lines.push("from sqlalchemy.orm import Session".to_string());
        lines.push(String::new());
        lines.push("from app.db.session import get_db".to_string());
        lines.push(format!("from app.models.{module} import {class}"));
        lines.push(String::new());
        lines.push("router = APIRouter()".to_string());
        lines.push(String::new());
        lines.push(String::new());
        lines.push(format!("class {class}PairRequest(BaseModel):"));
        lines.append(&mut req_field_lines);
        lines.push(String::new());
        lines.push(String::new());
        lines.push(format!(
            "@router.{decorator}(\"{path}\", status_code=status.HTTP_201_CREATED)"
        ));
        lines.push(format!(
            "def {func}(payload: {class}PairRequest, db: Session = Depends(get_db)) -> dict:"
        ));
        lines.push(format!("    # Effect (assumed): {assumption}"));
        if *break_mode == BreakMode::Handler500 {
            lines.push(
                "    raise RuntimeError(\"injected fault: handler broken before persist\")"
                    .to_string(),
            );
        }
        lines.push(format!("    row = {class}("));
        for c in &field_cols {
            lines.push(format!("        {c}=payload.{c},"));
        }
        lines.push("    )".to_string());
        lines.push("    db.add(row)".to_string());
        lines.push("    db.commit()".to_string());
        lines.push("    db.refresh(row)".to_string());
        lines.push("    token = secrets.token_urlsafe(24)".to_string());
        lines.push(format!(
            "    return {{\"deviceToken\": token, \"id\": row.id, \"deviceId\": {echo_device_id}}}"
        ));
        lines.push(String::new());

        let source = lines.join("\n");

        return EmittedRoute {
            operation: op.name.clone(),
            method,
            path,
            source,
        };
    }

    // No entity → keep a typed stub (parity with Phase 1 for non-entity ops).
    let source = [
        format!(
            "\"\"\"FastAPI route for the `{}` operation (generated, runnable stub).\"\"\"",
            op.name
        ),
        "from fastapi import APIRouter, status".to_string(),
        String::new(),
        "router = APIRouter()".to_string(),
        String::new(),
        String::new(),
        format!("@router.{decorator}(\"{path}\", status_code=status.HTTP_201_CREATED)"),
        format!("def {func}() -> dict:"),
        "    return {\"deviceToken\": \"\"}".to_string(),
        String::new(),
    ]
    .join("\n");
    EmittedRoute {
        operation: op.name.clone(),
        method,
        path,
        source,
    }
}

// ===========================================================================
// App entrypoint — create tables on startup + a health probe
// ===========================================================================

fn emit_runtime_main(spec: &FunctionalSpec, bindings: &[RouteBinding]) -> String {
    let mut imports = Vec::new();
    let mut includes = Vec::new();
    for b in bindings {
        let module = snake(&b.operation.name);
        imports.push(format!("from app.api import {module}"));
        includes.push(format!("app.include_router({module}.router)"));
    }
    let mut lines: Vec<String> = vec![
        "\"\"\"FastAPI application entrypoint (generated, runnable).\"\"\"".to_string(),
        "from contextlib import asynccontextmanager".to_string(),
        String::new(),
        "from fastapi import FastAPI".to_string(),
        String::new(),
        "from app.db.session import init_db".to_string(),
    ];
    lines.extend(imports.iter().cloned());
    lines.extend([
        String::new(),
        String::new(),
        "@asynccontextmanager".to_string(),
        "async def lifespan(app: FastAPI):".to_string(),
        "    # Create the schema on startup (create_all) so the tables really exist.".to_string(),
        "    init_db()".to_string(),
        "    yield".to_string(),
        String::new(),
        String::new(),
    ]);
    if let Some(a) = spec.auth.as_ref() {
        lines.push(format!("# auth model (spec): {}", a.model));
    }
    lines.extend([
        format!(
            "app = FastAPI(title=\"{}\", lifespan=lifespan)",
            project_title(spec)
        ),
        String::new(),
        String::new(),
        "@app.get(\"/healthz\")".to_string(),
        "def healthz() -> dict:".to_string(),
        "    return {\"status\": \"ok\"}".to_string(),
        String::new(),
        String::new(),
    ]);
    lines.extend(includes.iter().cloned());
    lines.push(String::new());
    lines.join("\n")
}

// ===========================================================================
// Naming helpers (local copies — the scaffold's are private)
// ===========================================================================

fn table_name(entity: &Entity, profile: &Profile) -> String {
    use qontinui_types::endpoint_for::endpoint_for;
    use qontinui_types::functional_spec::{Operation, SpecProvenance};
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
    endpoint_for(&probe, profile)
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&entity.name)
        .to_string()
}

fn project_title(spec: &FunctionalSpec) -> String {
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
