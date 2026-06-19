# Image for a generated FastAPI backend (Fork B runtime harness).
# Built from a materialized ./generated/ tree. Pinned slim base; deps are the runnable
# stack the generator targets: FastAPI + synchronous SQLAlchemy 2.0 over psycopg (v3).
FROM python:3.11-slim

WORKDIR /srv
COPY . /srv

RUN pip install --no-cache-dir \
    "fastapi" "uvicorn[standard]" "sqlalchemy>=2.0" "psycopg[binary]>=3" "pydantic>=2" \
    "alembic" "pytest" "pytest-cov" "httpx"

EXPOSE 8000
# Apply Alembic migrations on boot (the schema is owned by alembic, not create_all),
# then serve. The Phase-3 runtime test runs `pytest --cov` inside this container too.
CMD ["sh", "-c", "alembic upgrade head && uvicorn app.main:app --host 0.0.0.0 --port 8000"]
