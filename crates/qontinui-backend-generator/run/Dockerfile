# Image for a generated FastAPI backend (Fork B runtime harness) — DEFERRED in Phase 1.
# Built from a materialized ./generated/ tree. Pinned slim base; deps come from the
# generated pyproject.toml. See run/README.md for the deferral note.
FROM python:3.11-slim

WORKDIR /srv
COPY . /srv

RUN pip install --no-cache-dir \
    "fastapi" "uvicorn[standard]" "sqlalchemy>=2.0" "asyncpg" "alembic" "pydantic>=2"

EXPOSE 8000
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
