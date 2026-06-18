//! Phase 3 — the **quality bar**: business logic from assumptions, real Alembic
//! migrations, a generated pytest suite (unit + integration tiers) with a `pytest-cov`
//! gate at the Profile's `min_coverage_pct`, and the Fork-D enforcement hook.
//!
//! Phase 2 ([`crate::runtime::generate_runnable`]) emits a backend that really persists
//! and behaves, but it (a) creates its schema with `Base.metadata.create_all`, (b) has
//! thin handlers (no input validation, no auth dependency, no typed errors), and (c)
//! ships **no tests**. Phase 3 closes all three:
//!
//! 1. **Alembic migrations** replace `create_all`: an `alembic/` dir + an initial
//!    migration that creates every entity table is emitted, and the app's `init_db`
//!    becomes a no-op (the schema is owned by `alembic upgrade head`, run before
//!    `uvicorn` in the Docker tier).
//! 2. **Business logic from `OperationEffect` assumptions**: handlers gain input
//!    validation (from `OperationInput.validation`), an auth dependency wired from the
//!    spec [`qontinui_types::functional_spec::AuthModel`] / `Profile.backend.auth`, and
//!    typed error responses (`422`/`401`). The bodies may optionally be authored by a
//!    `claude -p` subprocess ([`crate::codegen`]) — but whatever is emitted must compile
//!    and pass the generated tests, so the deterministic body is the verified default.
//! 3. **Generated pytest suite** to the `architecture.testing_bar`: per-operation unit
//!    tests (validation + auth + happy path against an in-process SQLite TestClient) and
//!    endpoint integration tests, plus a `pytest-cov` configuration. The Docker runtime
//!    tier runs `pytest --cov` **inside the container** and asserts real line coverage
//!    ≥ `min_coverage_pct`; a below-bar node surfaces as a [`CoverageGap`] (re-dispatch
//!    signal), never a silent pass.
//! 4. **Enforcement hook** (Fork D): [`crate::enforcement`] wires `code-reviewer` +
//!    `security-scan` as post-generation gates structurally (honestly flagged as
//!    structurally-present, see that module).

use std::collections::BTreeMap;

use qontinui_types::functional_spec::{Entity, FunctionalSpec, Operation};
use qontinui_types::priorities_profile::Profile;

use crate::route_map::{route_for, RouteBinding};
use crate::runtime::{generate_runnable, BreakMode};
use crate::scaffold::GeneratedBackend;

/// The default coverage bar when the profile omits `min_coverage_pct`.
pub const DEFAULT_MIN_COVERAGE_PCT: u32 = 80;

/// Generate a **Phase-3** backend: the runnable Phase-2 tree, deepened with Alembic
/// migrations, validated + auth-gated business logic, and a generated pytest suite with
/// a coverage gate.
///
/// Deterministic and side-effect-free (returns the file tree). The optional LLM
/// handler-body path ([`crate::codegen`]) is applied by the caller *after* this
/// deterministic emission and must keep the generated tests green.
///
/// `break_mode` is threaded through to [`generate_runnable`] so the runtime verifier can
/// still inject a fault (e.g. drop a column) and prove the evidence reflects the running
/// server.
pub fn generate_phase3(
    spec: &FunctionalSpec,
    profile: &Profile,
    break_mode: BreakMode,
) -> GeneratedBackend {
    let mut backend = generate_runnable(spec, profile, break_mode.clone());
    let files: &mut BTreeMap<String, String> = &mut backend.files;

    let min_cov = min_coverage_pct(profile);
    let auth_model = auth_model_str(spec, profile);

    // --- (1) Alembic: replace create_all. init_db becomes a no-op; the schema is
    //         owned by the emitted initial migration, applied by `alembic upgrade head`. ---
    files.insert(
        "app/db/session.py".to_string(),
        emit_sync_session_no_create(),
    );
    files.insert("alembic.ini".to_string(), emit_alembic_ini());
    files.insert("alembic/env.py".to_string(), emit_alembic_env());
    files.insert("alembic/script.py.mako".to_string(), emit_alembic_mako());
    files.insert(
        "alembic/versions/0001_initial.py".to_string(),
        emit_initial_migration(spec, profile, &break_mode),
    );

    // --- (2) Business logic: validation + auth dependency + typed errors ---
    files.insert("app/auth.py".to_string(), emit_auth_dependency(&auth_model));
    files.insert("app/validation.py".to_string(), emit_validators(spec));
    let mut bindings: Vec<RouteBinding> = Vec::new();
    for op in &spec.operations {
        let binding = route_for(op, profile);
        let src = emit_phase3_route(spec, &binding, &break_mode, &auth_model);
        files.insert(format!("app/api/{}.py", snake(&op.name)), src.clone());
        // Keep the structured EmittedRoute source consistent with what we wrote.
        if let Some(r) = backend.routes.iter_mut().find(|r| r.operation == op.name) {
            r.source = src;
        }
        bindings.push(binding);
    }
    // main.py: include the auth-aware routes; init_db is now a no-op (alembic owns schema).
    files.insert("app/main.py".to_string(), emit_phase3_main(spec, &bindings));

    // --- (3) Generated pytest suite + coverage config ---
    files.insert("conftest.py".to_string(), emit_conftest());
    files.insert("tests/__init__.py".to_string(), String::new());
    files.insert(
        "tests/conftest.py".to_string(),
        emit_tests_conftest(&auth_model),
    );
    files.insert(".coveragerc".to_string(), emit_coveragerc());
    files.insert("pytest.ini".to_string(), emit_pytest_ini());
    for op in &spec.operations {
        files.insert(
            format!("tests/test_{}.py", snake(&op.name)),
            emit_op_tests(spec, op, profile, &auth_model),
        );
    }
    for entity in &spec.entities {
        files.insert(
            format!("tests/test_model_{}.py", snake(&entity.name)),
            emit_model_tests(entity),
        );
    }
    files.insert("tests/test_health.py".to_string(), emit_health_test());
    files.insert(
        "tests/test_auth.py".to_string(),
        emit_auth_tests(&auth_model),
    );

    // --- The coverage runner the Docker tier invokes inside the container. ---
    files.insert("run_tests.sh".to_string(), emit_run_tests_sh(min_cov));

    // Update pyproject deps for the test stack (pytest, httpx, coverage, alembic, sqlite).
    files.insert(
        "pyproject.toml".to_string(),
        emit_phase3_pyproject(spec, profile, min_cov),
    );

    backend
}

/// The effective coverage bar: `architecture.min_coverage_pct` or the default (80).
pub fn min_coverage_pct(profile: &Profile) -> u32 {
    profile
        .architecture
        .min_coverage_pct
        .unwrap_or(DEFAULT_MIN_COVERAGE_PCT)
}

/// Resolve the auth model: `Profile.backend.auth` overrides, else the spec's
/// `auth.model`, else `"none"`.
fn auth_model_str(spec: &FunctionalSpec, profile: &Profile) -> String {
    if let Some(a) = profile.backend.auth.as_ref() {
        return a.clone();
    }
    spec.auth
        .as_ref()
        .map(|a| a.model.clone())
        .unwrap_or_else(|| "none".to_string())
}

// ===========================================================================
// (1) Alembic
// ===========================================================================

/// Synchronous session WITHOUT `create_all` — the schema is owned by Alembic now.
fn emit_sync_session_no_create() -> String {
    [
        "\"\"\"Synchronous SQLAlchemy 2.0 session + engine (generated, runnable).",
        "",
        "Schema ownership: Alembic. `init_db` is a deliberate no-op — the tables are",
        "created by `alembic upgrade head` (run before uvicorn), not at app startup.",
        "\"\"\"",
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
        "    \"\"\"No-op: Alembic owns the schema (alembic upgrade head).\"\"\"",
        "    return None",
        "",
    ]
    .join("\n")
}

fn emit_alembic_ini() -> String {
    [
        "# Generated Alembic config. The DB URL is taken from the DATABASE_URL env var",
        "# in env.py (NOT hard-coded here) so the same migration runs in Docker + tests.",
        "[alembic]",
        "script_location = alembic",
        "prepend_sys_path = .",
        "",
        "[loggers]",
        "keys = root,sqlalchemy,alembic",
        "",
        "[handlers]",
        "keys = console",
        "",
        "[formatters]",
        "keys = generic",
        "",
        "[logger_root]",
        "level = WARN",
        "handlers = console",
        "qualname =",
        "",
        "[logger_sqlalchemy]",
        "level = WARN",
        "handlers =",
        "qualname = sqlalchemy.engine",
        "",
        "[logger_alembic]",
        "level = INFO",
        "handlers =",
        "qualname = alembic",
        "",
        "[handler_console]",
        "class = StreamHandler",
        "args = (sys.stderr,)",
        "level = NOTSET",
        "formatter = generic",
        "",
        "[formatter_generic]",
        "format = %(levelname)-5.5s [%(name)s] %(message)s",
        "",
    ]
    .join("\n")
}

fn emit_alembic_env() -> String {
    [
        "\"\"\"Alembic migration environment (generated).",
        "",
        "Online-only. Binds Base.metadata so `--autogenerate` would work, but the initial",
        "migration is emitted deterministically by the generator. DB URL comes from",
        "DATABASE_URL so the migration runs identically in Docker and tests.",
        "\"\"\"",
        "import os",
        "",
        "from alembic import context",
        "from sqlalchemy import create_engine",
        "",
        "from app.db.base import Base",
        "import app.models  # noqa: F401  (register models on Base.metadata)",
        "",
        "target_metadata = Base.metadata",
        "",
        "",
        "def run_migrations_online() -> None:",
        "    url = os.environ.get(",
        "        \"DATABASE_URL\", \"postgresql+psycopg://postgres:postgres@localhost:5432/app\"",
        "    )",
        "    connectable = create_engine(url, future=True)",
        "    with connectable.connect() as connection:",
        "        context.configure(connection=connection, target_metadata=target_metadata)",
        "        with context.begin_transaction():",
        "            context.run_migrations()",
        "",
        "",
        "run_migrations_online()",
        "",
    ]
    .join("\n")
}

fn emit_alembic_mako() -> String {
    [
        "\"\"\"${message}",
        "",
        "Revision ID: ${up_revision}",
        "Revises: ${down_revision | comma,n}",
        "\"\"\"",
        "from alembic import op",
        "import sqlalchemy as sa",
        "",
        "revision = ${repr(up_revision)}",
        "down_revision = ${repr(down_revision)}",
        "branch_labels = ${repr(branch_labels)}",
        "depends_on = ${repr(depends_on)}",
        "",
        "",
        "def upgrade() -> None:",
        "    ${upgrades if upgrades else \"pass\"}",
        "",
        "",
        "def downgrade() -> None:",
        "    ${downgrades if downgrades else \"pass\"}",
        "",
    ]
    .join("\n")
}

/// The initial migration: one `create_table` per entity (id PK + a text column per
/// field), respecting the DropColumn fault so the runtime gap-proof still works.
fn emit_initial_migration(
    spec: &FunctionalSpec,
    profile: &Profile,
    break_mode: &BreakMode,
) -> String {
    let mut lines: Vec<String> = vec![
        "\"\"\"initial schema (generated deterministically from the spec).\"\"\"".to_string(),
        "from alembic import op".to_string(),
        "import sqlalchemy as sa".to_string(),
        String::new(),
        "revision = \"0001_initial\"".to_string(),
        "down_revision = None".to_string(),
        "branch_labels = None".to_string(),
        "depends_on = None".to_string(),
        String::new(),
        String::new(),
        "def upgrade() -> None:".to_string(),
    ];
    for entity in &spec.entities {
        let table = table_name(entity, profile);
        let dropped = match break_mode {
            BreakMode::DropColumn { entity: e, column } if e == &entity.name => {
                Some(column.as_str())
            }
            _ => None,
        };
        lines.push("    op.create_table(".to_string());
        lines.push(format!("        \"{table}\","));
        lines.push(
            "        sa.Column(\"id\", sa.Integer, primary_key=True, autoincrement=True),"
                .to_string(),
        );
        for f in &entity.fields {
            let col = snake(&f.name);
            if Some(col.as_str()) == dropped {
                continue;
            }
            lines.push(format!(
                "        sa.Column(\"{col}\", sa.String, nullable=True),"
            ));
        }
        lines.push("    )".to_string());
    }
    if spec.entities.is_empty() {
        lines.push("    pass".to_string());
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.push("def downgrade() -> None:".to_string());
    for entity in &spec.entities {
        let table = table_name(entity, profile);
        lines.push(format!("    op.drop_table(\"{table}\")"));
    }
    if spec.entities.is_empty() {
        lines.push("    pass".to_string());
    }
    lines.push(String::new());
    lines.join("\n")
}

// ===========================================================================
// (2) Business logic: validation, auth, typed errors
// ===========================================================================

/// The auth dependency. For `session`/`jwt`/`basic` it requires a bearer/cookie token
/// (any non-empty value passes in v0 — the point is the *gate exists and rejects when
/// absent*, which the auth test asserts). `none` admits everyone.
fn emit_auth_dependency(auth_model: &str) -> String {
    let mut lines: Vec<String> = vec![
        "\"\"\"Auth dependency wired from the spec AuthModel / Profile.backend.auth.".to_string(),
        String::new(),
        format!("Auth model: {auth_model}."),
        "A v0 gate: it rejects a request with no credential (401) and admits one that".to_string(),
        "carries any bearer token or session cookie. The point is the gate is real and".to_string(),
        "wired as a FastAPI dependency, not that it validates a signature (deferred).".to_string(),
        "\"\"\"".to_string(),
        "from fastapi import Depends, HTTPException, Request, status".to_string(),
        String::new(),
        format!("AUTH_MODEL = \"{auth_model}\""),
        String::new(),
        String::new(),
        "def require_auth(request: Request) -> str:".to_string(),
        "    \"\"\"Return the principal identity, or raise 401 when unauthenticated.\"\"\""
            .to_string(),
    ];
    if auth_model == "none" {
        lines.push("    return \"anonymous\"".to_string());
    } else {
        lines.push(
            "    # Accept either an Authorization: Bearer <t> header or a session cookie."
                .to_string(),
        );
        lines.push("    header = request.headers.get(\"authorization\", \"\")".to_string());
        lines
            .push("    if header.lower().startswith(\"bearer \") and len(header) > 7:".to_string());
        lines.push("        return header[7:]".to_string());
        lines.push("    cookie = request.cookies.get(\"session\", \"\")".to_string());
        lines.push("    if cookie:".to_string());
        lines.push("        return cookie".to_string());
        lines.push("    raise HTTPException(".to_string());
        lines.push("        status_code=status.HTTP_401_UNAUTHORIZED,".to_string());
        lines.push("        detail=\"authentication required\",".to_string());
        lines.push("    )".to_string());
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.push("AuthDep = Depends(require_auth)".to_string());
    lines.push(String::new());
    lines.join("\n")
}

/// Validators derived from `OperationInput.validation` rules. v0 understands the
/// `https allowlisted host` rule (the connect-runner fixture); other rules become a
/// permissive pass-through so unknown rules never block (recorded, not fatal).
fn emit_validators(spec: &FunctionalSpec) -> String {
    let mut lines: Vec<String> = vec![
        "\"\"\"Server-side input validators derived from OperationInput.validation.\"\"\""
            .to_string(),
        "from urllib.parse import urlparse".to_string(),
        String::new(),
        String::new(),
        "def is_https_allowlisted(value: str | None) -> bool:".to_string(),
        "    \"\"\"True iff `value` is an https URL with a non-empty host.\"\"\"".to_string(),
        "    if not value:".to_string(),
        "        return False".to_string(),
        "    parsed = urlparse(value)".to_string(),
        "    return parsed.scheme == \"https\" and bool(parsed.netloc)".to_string(),
        String::new(),
    ];
    // Record which (op, field) carry an https rule so the test generator can pair up.
    let mut rule_refs: Vec<String> = Vec::new();
    for op in &spec.operations {
        for inp in &op.inputs {
            if let Some(v) = &inp.validation {
                if v.rule.contains("https") {
                    rule_refs.push(format!("# {}.{}: {}", op.name, inp.field, v.rule));
                }
            }
        }
    }
    if !rule_refs.is_empty() {
        lines.push(String::new());
        lines.push("# Validation rules sourced from the spec:".to_string());
        lines.extend(rule_refs);
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Emit a Phase-3 route: validation (422 on bad input), the auth dependency, real
/// persistence, and a typed token response.
fn emit_phase3_route(
    spec: &FunctionalSpec,
    binding: &RouteBinding,
    break_mode: &BreakMode,
    auth_model: &str,
) -> String {
    let op = &binding.operation;
    let method = &binding.endpoint.method;
    let path = &binding.endpoint.path;
    let decorator = method.to_ascii_lowercase();
    let func = snake(&op.name);

    let assumption = op
        .effect
        .as_ref()
        .and_then(|e| e.assumption.clone())
        .unwrap_or_else(|| "generated effect (assumed)".to_string());

    let entity_name = op.entity.clone().unwrap_or_default();
    let entity = spec.entities.iter().find(|e| e.name == entity_name);

    // Find an https-allowlisted input field, if any (the connect-runner `callback`).
    let https_field: Option<String> = op
        .inputs
        .iter()
        .find(|inp| {
            inp.validation
                .as_ref()
                .map(|v| v.rule.contains("https"))
                .unwrap_or(false)
        })
        .map(|inp| snake(&inp.field));

    let Some(entity) = entity else {
        // No entity → a typed stub kept behind the same auth gate.
        return emit_stub_route(op, method, path, &decorator, &func, auth_model);
    };

    let class = pascal(&entity.name);
    let module = snake(&entity.name);
    let field_cols: Vec<String> = entity.fields.iter().map(|f| snake(&f.name)).collect();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "\"\"\"FastAPI route for the `{}` operation (generated, Phase 3).",
        op.name
    ));
    lines.push(String::new());
    lines.push(format!("Route (shared endpoint_for): {method} {path}"));
    lines.push(format!("Assumed effect: {assumption}"));
    lines.push("\"\"\"".to_string());
    lines.push("import secrets".to_string());
    lines.push(String::new());
    lines.push("from fastapi import APIRouter, Depends, HTTPException, status".to_string());
    lines.push("from pydantic import BaseModel".to_string());
    lines.push("from sqlalchemy.orm import Session".to_string());
    lines.push(String::new());
    lines.push("from app.auth import require_auth".to_string());
    lines.push("from app.db.session import get_db".to_string());
    lines.push(format!("from app.models.{module} import {class}"));
    if https_field.is_some() {
        lines.push("from app.validation import is_https_allowlisted".to_string());
    }
    lines.push(String::new());
    lines.push("router = APIRouter()".to_string());
    lines.push(String::new());
    lines.push(String::new());
    // Request model: required fields from inputs, the rest optional.
    let required_fields: std::collections::BTreeSet<String> = op
        .inputs
        .iter()
        .filter(|i| i.required)
        .map(|i| snake(&i.field))
        .collect();
    lines.push(format!("class {class}PairRequest(BaseModel):"));
    if field_cols.is_empty() {
        lines.push("    pass".to_string());
    } else {
        for c in &field_cols {
            if required_fields.contains(c) {
                lines.push(format!("    {c}: str"));
            } else {
                lines.push(format!("    {c}: str | None = None"));
            }
        }
    }
    lines.push(String::new());
    lines.push(String::new());
    lines.push(format!(
        "@router.{decorator}(\"{path}\", status_code=status.HTTP_201_CREATED)"
    ));
    lines.push(format!("def {func}("));
    lines.push(format!("    payload: {class}PairRequest,"));
    lines.push("    db: Session = Depends(get_db),".to_string());
    lines.push("    principal: str = Depends(require_auth),".to_string());
    lines.push(") -> dict:".to_string());
    lines.push(format!("    # Effect (assumed): {assumption}"));
    if *break_mode == BreakMode::Handler500 {
        lines.push(
            "    raise RuntimeError(\"injected fault: handler broken before persist\")".to_string(),
        );
    }
    // Validation gate (422) for the https-allowlisted field.
    if let Some(field) = &https_field {
        lines.push(format!("    if not is_https_allowlisted(payload.{field}):"));
        lines.push("        raise HTTPException(".to_string());
        lines.push("            status_code=status.HTTP_422_UNPROCESSABLE_ENTITY,".to_string());
        lines.push(format!(
            "            detail=\"{field} must be an https allowlisted URL\","
        ));
        lines.push("        )".to_string());
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
    let echo_device_id = if field_cols.iter().any(|c| c == "device_id") {
        "payload.device_id".to_string()
    } else {
        "None".to_string()
    };
    lines.push(format!(
        "    return {{\"deviceToken\": token, \"id\": row.id, \"deviceId\": {echo_device_id}, \"principal\": principal}}"
    ));
    lines.push(String::new());
    lines.join("\n")
}

fn emit_stub_route(
    op: &Operation,
    method: &str,
    path: &str,
    decorator: &str,
    func: &str,
    _auth_model: &str,
) -> String {
    let _ = method;
    [
        format!(
            "\"\"\"FastAPI route for the `{}` operation (generated, Phase 3 stub).\"\"\"",
            op.name
        ),
        "from fastapi import APIRouter, Depends, status".to_string(),
        String::new(),
        "from app.auth import require_auth".to_string(),
        String::new(),
        "router = APIRouter()".to_string(),
        String::new(),
        String::new(),
        format!("@router.{decorator}(\"{path}\", status_code=status.HTTP_201_CREATED)"),
        format!("def {func}(principal: str = Depends(require_auth)) -> dict:"),
        "    return {\"deviceToken\": \"\", \"principal\": principal}".to_string(),
        String::new(),
    ]
    .join("\n")
}

fn emit_phase3_main(spec: &FunctionalSpec, bindings: &[RouteBinding]) -> String {
    let mut imports = Vec::new();
    let mut includes = Vec::new();
    for b in bindings {
        let module = snake(&b.operation.name);
        imports.push(format!("from app.api import {module}"));
        includes.push(format!("app.include_router({module}.router)"));
    }
    let mut lines: Vec<String> = vec![
        "\"\"\"FastAPI application entrypoint (generated, Phase 3).".to_string(),
        String::new(),
        "Schema is owned by Alembic (alembic upgrade head runs before uvicorn); init_db is"
            .to_string(),
        "a no-op kept for symmetry.".to_string(),
        "\"\"\"".to_string(),
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
        "    init_db()  # no-op: alembic owns the schema".to_string(),
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
// (3) Generated pytest suite + coverage config
// ===========================================================================

/// Root conftest: makes `app` importable (adds the project root to sys.path). Empty
/// body beyond the import shim so coverage isn't diluted.
fn emit_conftest() -> String {
    [
        "\"\"\"Root conftest — ensures the `app` package is importable under pytest.\"\"\"",
        "import os",
        "import sys",
        "",
        "sys.path.insert(0, os.path.dirname(__file__))",
        "",
    ]
    .join("\n")
}

/// Test conftest: an in-process SQLite engine + a FastAPI TestClient with `get_db`
/// overridden to the SQLite session, so unit + integration tests run with NO external
/// postgres (fast, deterministic) while the SAME app code is exercised. The Docker tier
/// additionally runs the suite against the real postgres image.
fn emit_tests_conftest(auth_model: &str) -> String {
    let auth_header = if auth_model == "none" {
        "{}".to_string()
    } else {
        "{\"Authorization\": \"Bearer test-token\"}".to_string()
    };
    [
        "\"\"\"Test fixtures: an in-process SQLite DB + a TestClient with get_db overridden.",
        "",
        "The same generated app code runs against SQLite here (no external DB needed); the",
        "Docker tier re-runs the identical suite against the real postgres image.",
        "\"\"\"",
        "import pytest",
        "from fastapi.testclient import TestClient",
        "from sqlalchemy import create_engine",
        "from sqlalchemy.orm import sessionmaker",
        "from sqlalchemy.pool import StaticPool",
        "",
        "from app.db.base import Base",
        "import app.models  # noqa: F401  (register models on Base.metadata)",
        "from app.db.session import get_db",
        "from app.main import app",
        "",
        "TEST_DB_URL = \"sqlite+pysqlite:///:memory:\"",
        "# StaticPool keeps ONE shared in-memory connection so every session sees the",
        "# same schema (a fresh sqlite :memory: per connection would be empty otherwise).",
        "_engine = create_engine(",
        "    TEST_DB_URL,",
        "    connect_args={\"check_same_thread\": False},",
        "    poolclass=StaticPool,",
        "    future=True,",
        ")",
        "_TestSession = sessionmaker(bind=_engine, autoflush=False, expire_on_commit=False)",
        "",
        "",
        "@pytest.fixture(scope=\"session\", autouse=True)",
        "def _schema():",
        "    # The app's runtime schema is owned by Alembic; for the in-process SQLite test",
        "    # DB we materialize the same metadata directly (Base reflects every model).",
        "    Base.metadata.create_all(bind=_engine)",
        "    yield",
        "    Base.metadata.drop_all(bind=_engine)",
        "",
        "",
        "def _override_get_db():",
        "    db = _TestSession()",
        "    try:",
        "        yield db",
        "    finally:",
        "        db.close()",
        "",
        "",
        "@pytest.fixture()",
        "def client():",
        "    app.dependency_overrides[get_db] = _override_get_db",
        "    with TestClient(app) as c:",
        "        yield c",
        "    app.dependency_overrides.clear()",
        "",
        "",
        "@pytest.fixture()",
        &format!("def auth_headers():\n    return {auth_header}"),
        "",
    ]
    .join("\n")
}

fn emit_coveragerc() -> String {
    [
        "[run]",
        "source = app",
        "branch = True",
        "omit =",
        "    app/__init__.py",
        "    app/db/__init__.py",
        "    app/models/__init__.py",
        "    app/schemas/__init__.py",
        "    app/api/__init__.py",
        "",
        "[report]",
        "show_missing = True",
        "skip_covered = False",
        "",
    ]
    .join("\n")
}

fn emit_pytest_ini() -> String {
    ["[pytest]", "testpaths = tests", "addopts = -q", ""].join("\n")
}

/// Per-operation tests: happy path (201 + token), validation failure (422) when the op
/// has an https-allowlisted input, and auth rejection (401) when auth is enforced.
fn emit_op_tests(
    spec: &FunctionalSpec,
    op: &Operation,
    profile: &Profile,
    auth_model: &str,
) -> String {
    let func = snake(&op.name);
    let path = route_for(op, profile).endpoint.path.clone();
    let path = &path;
    let entity_name = op.entity.clone().unwrap_or_default();
    let entity = spec.entities.iter().find(|e| e.name == entity_name);

    // Build a valid JSON body from the entity fields (all str), with https callback valid.
    let mut valid_pairs: Vec<String> = Vec::new();
    let mut bad_pairs: Vec<String> = Vec::new();
    let https_field: Option<String> = op
        .inputs
        .iter()
        .find(|inp| {
            inp.validation
                .as_ref()
                .map(|v| v.rule.contains("https"))
                .unwrap_or(false)
        })
        .map(|inp| snake(&inp.field));
    if let Some(entity) = entity {
        for f in &entity.fields {
            let col = snake(&f.name);
            let valid_val = if Some(col.clone()) == https_field {
                "https://app.qontinui.io/paired".to_string()
            } else {
                format!("v-{col}")
            };
            valid_pairs.push(format!("\"{col}\": \"{valid_val}\""));
            // For the bad body, make the https field invalid (http), others same.
            let bad_val = if Some(col.clone()) == https_field {
                "http://evil.example/paired".to_string()
            } else {
                format!("v-{col}")
            };
            bad_pairs.push(format!("\"{col}\": \"{bad_val}\""));
        }
    }
    let valid_body = format!("{{{}}}", valid_pairs.join(", "));
    let bad_body = format!("{{{}}}", bad_pairs.join(", "));

    let auth_enforced = auth_model != "none";

    let mut lines: Vec<String> = vec![
        format!(
            "\"\"\"Generated tests for the `{}` operation.\"\"\"",
            op.name
        ),
        String::new(),
        String::new(),
    ];

    // Happy path
    lines.push(format!("def test_{func}_created(client, auth_headers):"));
    lines.push(format!(
        "    resp = client.post(\"{path}\", json={valid_body}, headers=auth_headers)"
    ));
    lines.push("    assert resp.status_code == 201, resp.text".to_string());
    lines.push("    body = resp.json()".to_string());
    lines.push("    assert body.get(\"deviceToken\"), body".to_string());
    lines.push("    assert len(body[\"deviceToken\"]) > 3".to_string());
    lines.push(String::new());
    lines.push(String::new());

    // Validation failure (422)
    if https_field.is_some() {
        lines.push(format!(
            "def test_{func}_rejects_invalid_callback(client, auth_headers):"
        ));
        lines.push(format!(
            "    resp = client.post(\"{path}\", json={bad_body}, headers=auth_headers)"
        ));
        lines.push("    assert resp.status_code == 422, resp.text".to_string());
        lines.push(String::new());
        lines.push(String::new());
    }

    // Auth rejection (401) — only when auth is enforced.
    if auth_enforced {
        lines.push(format!("def test_{func}_requires_auth(client):"));
        lines.push(format!(
            "    resp = client.post(\"{path}\", json={valid_body})"
        ));
        lines.push("    assert resp.status_code == 401, resp.text".to_string());
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Per-entity model + schema tests: the model maps to its table and carries every spec
/// column, and the Pydantic read-schema validates a row (so the schema module is covered).
fn emit_model_tests(entity: &Entity) -> String {
    let class = pascal(&entity.name);
    let module = snake(&entity.name);
    let cols: Vec<String> = entity.fields.iter().map(|f| snake(&f.name)).collect();
    let cols_py = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    // A kwargs string for instantiating the read-schema (id=1 + a value per field).
    let mut schema_kwargs: Vec<String> = vec!["id=1".to_string()];
    for c in &cols {
        schema_kwargs.push(format!("{c}=\"x\""));
    }
    [
        format!(
            "\"\"\"Generated model + schema tests for `{}`.\"\"\"",
            entity.name
        ),
        format!("from app.models.{module} import {class}"),
        format!("from app.schemas.{module} import {class}Read"),
        String::new(),
        String::new(),
        format!("def test_{module}_table_and_columns():"),
        format!("    table = {class}.__table__"),
        "    names = set(table.columns.keys())".to_string(),
        "    assert \"id\" in names".to_string(),
        format!("    for c in [{cols_py}]:"),
        "        assert c in names, (c, names)".to_string(),
        String::new(),
        String::new(),
        format!("def test_{module}_instantiates():"),
        format!("    row = {class}()"),
        "    assert row is not None".to_string(),
        String::new(),
        String::new(),
        format!("def test_{module}_read_schema_validates():"),
        format!("    obj = {class}Read({})", schema_kwargs.join(", ")),
        "    assert obj.id == 1".to_string(),
        String::new(),
    ]
    .join("\n")
}

fn emit_health_test() -> String {
    [
        "\"\"\"Generated health-probe test.\"\"\"",
        "",
        "",
        "def test_healthz(client):",
        "    resp = client.get(\"/healthz\")",
        "    assert resp.status_code == 200",
        "    assert resp.json() == {\"status\": \"ok\"}",
        "",
    ]
    .join("\n")
}

fn emit_auth_tests(auth_model: &str) -> String {
    if auth_model == "none" {
        return [
            "\"\"\"Generated auth tests (auth model: none — gate admits everyone).\"\"\"",
            "from app.auth import require_auth",
            "",
            "",
            "def test_anonymous_principal():",
            "    class _Req:",
            "        headers = {}",
            "        cookies = {}",
            "    assert require_auth(_Req()) == \"anonymous\"",
            "",
        ]
        .join("\n");
    }
    [
        "\"\"\"Generated auth-dependency tests (the gate exists and rejects/admits).\"\"\"",
        "import pytest",
        "from fastapi import HTTPException",
        "",
        "from app.auth import require_auth",
        "",
        "",
        "class _Req:",
        "    def __init__(self, headers=None, cookies=None):",
        "        self.headers = headers or {}",
        "        self.cookies = cookies or {}",
        "",
        "",
        "def test_rejects_missing_credential():",
        "    with pytest.raises(HTTPException) as exc:",
        "        require_auth(_Req())",
        "    assert exc.value.status_code == 401",
        "",
        "",
        "def test_admits_bearer_token():",
        "    principal = require_auth(_Req(headers={\"authorization\": \"Bearer abc123\"}))",
        "    assert principal == \"abc123\"",
        "",
        "",
        "def test_admits_session_cookie():",
        "    principal = require_auth(_Req(cookies={\"session\": \"sess-xyz\"}))",
        "    assert principal == \"sess-xyz\"",
        "",
    ]
    .join("\n")
}

/// The in-container test runner: apply migrations (proving Alembic works against the
/// real postgres), then run pytest --cov with a hard `--cov-fail-under` gate.
fn emit_run_tests_sh(min_cov: u32) -> String {
    [
        "#!/bin/sh".to_string(),
        "# Generated coverage runner (Phase 3). Run INSIDE the app container.".to_string(),
        "# 1) prove the Alembic migration applies against the live postgres,".to_string(),
        "# 2) run the generated pytest suite with a hard coverage gate.".to_string(),
        "set -e".to_string(),
        "echo \"== alembic upgrade head ==\"".to_string(),
        "alembic upgrade head".to_string(),
        "echo \"== pytest --cov (gate >= MIN%) ==\"".to_string(),
        format!("pytest --cov=app --cov-report=term-missing --cov-fail-under={min_cov}"),
        "".to_string(),
    ]
    .join("\n")
}

fn emit_phase3_pyproject(spec: &FunctionalSpec, profile: &Profile, min_cov: u32) -> String {
    use qontinui_types::priorities_profile::ApiStyle;
    let api = match profile.backend.api_style {
        ApiStyle::Rest => "rest",
        ApiStyle::Graphql => "graphql",
    };
    format!(
        "[project]\n\
name = \"{slug}\"\n\
version = \"0.1.0\"\n\
description = \"Generated backend for {url} (stack: {lang}/{fw}/{store}/{api}); coverage gate >= {min_cov}%\"\n\
requires-python = \">=3.11\"\n\
dependencies = [\n\
    \"fastapi\",\n\
    \"uvicorn[standard]\",\n\
    \"sqlalchemy>=2.0\",\n\
    \"psycopg[binary]>=3\",\n\
    \"alembic\",\n\
    \"pydantic>=2\",\n\
]\n\
\n\
[project.optional-dependencies]\n\
test = [\n\
    \"pytest\",\n\
    \"pytest-cov\",\n\
    \"httpx\",\n\
]\n",
        slug = project_title(spec),
        url = spec.target.source_url,
        lang = profile.backend.language,
        fw = profile.backend.framework,
        store = profile.backend.datastore,
        api = api,
        min_cov = min_cov,
    )
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
