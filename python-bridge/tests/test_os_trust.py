"""os_trust routes the bridge's outbound TLS through the OS trust store.

Plan 2026-10-10-spec-front-end-phase-9-generic-boundary, Phase 5.
"""

import builtins
import ssl
import sys

import pytest


@pytest.fixture(autouse=True)
def _restore_ssl():  # type: ignore[no-untyped-def]
    """Importing os_trust injects process-wide; undo it after each test."""
    yield
    if "truststore" in sys.modules:
        sys.modules["truststore"].extract_from_ssl()


def test_injection_replaces_the_ssl_context_class() -> None:
    truststore = pytest.importorskip("truststore")
    import os_trust

    truststore.extract_from_ssl()
    assert ssl.SSLContext is not truststore.SSLContext
    assert os_trust.inject_os_trust() is True
    assert ssl.SSLContext is truststore.SSLContext, (
        "after injection every new ssl context must verify against the OS store"
    )


def test_every_spawned_entry_point_imports_it_first() -> None:
    from pathlib import Path

    bridge = Path(__file__).resolve().parent.parent
    for name in [
        "qontinui_executor.py",
        "embedding_server.py",
        "search_rag.py",
        "generate_embeddings.py",
        "extraction_executor.py",
        "find_rag.py",
    ]:
        lines = (bridge / name).read_text(encoding="utf-8").splitlines()
        imports = [ln for ln in lines if ln.startswith(("import ", "from "))]
        assert imports and imports[0].startswith("import os_trust"), (
            f"{name}: `import os_trust` must be the first import, before any library "
            f"builds an SSL context (first import is {imports[:1]})"
        )


def test_a_missing_truststore_is_reported_not_silent(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    import os_trust

    real_import = builtins.__import__

    def refuse(name: str, *args, **kwargs):  # type: ignore[no-untyped-def]
        if name == "truststore":
            raise ImportError("simulated")
        return real_import(name, *args, **kwargs)

    monkeypatch.delitem(sys.modules, "truststore", raising=False)
    monkeypatch.setattr(builtins, "__import__", refuse)
    with caplog.at_level("WARNING"):
        assert os_trust.inject_os_trust() is False
    assert "NOT trusted" in caplog.text
